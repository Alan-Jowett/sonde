// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Thread-local BPF helper dispatch.
//!
//! BPF helpers are registered as bare `fn` pointers
//! ([`crate::bpf_runtime::HelperFn`]) with the interpreter, so they
//! cannot capture state. This module bridges that gap by stashing
//! mutable references into a thread-local [`DispatchContext`] that is
//! installed at the start of BPF execution and cleared at the end.
//!
//! **Lifetime contract:** the context is valid only while
//! [`crate::wake_cycle::run_wake_cycle`] is executing BPF. No helper
//! may be invoked outside that window. The owning function holds all
//! referenced objects on its stack, guaranteeing pointer validity.
//!
//! See node-design.md §8.2 which explicitly endorses thread-local
//! dispatch for wiring helpers to platform state.

use std::cell::RefCell;

use sonde_protocol::{AeadProvider, Sha256Provider};

use crate::async_queue::AsyncQueue;
use crate::bpf_helpers::ProgramClass;
use crate::hal::Hal;
use crate::key_store::NodeIdentity;
use crate::map_storage::MapStorage;
use crate::sleep::SleepManager;
use crate::traits::{Clock, Transport};

/// Default response timeout for `send_recv` helper (ms). Matches the protocol
/// spec (node-requirements.md ND-0702) and accounts for USB-CDC modem bridge
/// round-trip latency.
const SEND_RECV_TIMEOUT_MS: u32 = 200;

/// Maximum buffer length for bus helper operations (I2C/SPI).
/// Defence-in-depth cap to prevent oversized stack/heap access.
const MAX_BUS_TRANSFER_LEN: usize = 4096;

/// Maximum allowed timeout for `send_recv` helper (ms).
const MAX_SEND_RECV_TIMEOUT_MS: u32 = 5000;

/// Maximum delay allowed by `delay_us` helper (1 second).
const MAX_DELAY_US: u32 = 1_000_000;

/// Upper bound on the number of BPF maps supported per program.
/// Typical usage is 1–4 maps (bounded by RTC SRAM budget).
pub const MAX_MAPS: usize = 16;

// ---------------------------------------------------------------------------
// Map pointer index
// ---------------------------------------------------------------------------

/// Error returned by [`MapPtrIndex::insert`].
#[derive(Debug, PartialEq)]
enum MapPtrInsertError {
    /// The index is already at capacity (`MAX_MAPS`).
    Overflow,
    /// The pointer already exists in the index.
    Duplicate,
}

/// Fixed-size flat array mapping relocated map pointers to map indices.
///
/// Replaces `HashMap<u64, usize>` for zero heap allocation and faster
/// lookup over the small (1–4 entry) typical map counts.
///
/// Map pointers originate from `Vec::as_ptr()` in [`MapStorage`], which
/// guarantees non-null for non-zero-capacity vectors. The sentinel value `0`
/// therefore never collides with a valid map pointer.
struct MapPtrIndex {
    entries: [(u64, usize); MAX_MAPS],
    len: usize,
}

impl MapPtrIndex {
    fn new() -> Self {
        Self {
            entries: [(0, 0); MAX_MAPS],
            len: 0,
        }
    }

    /// Insert a map pointer → index mapping. Returns an error if the
    /// index is full or if the pointer is a duplicate (which would cause
    /// `get()` to resolve the wrong map).
    fn insert(&mut self, ptr: u64, idx: usize) -> Result<(), MapPtrInsertError> {
        if self.len >= MAX_MAPS {
            return Err(MapPtrInsertError::Overflow);
        }
        // Reject duplicates in all builds — not just debug. Duplicate
        // pointers can arise from zero-sized maps (empty Vec returns a
        // dangling non-null pointer that may collide).
        if self.entries[..self.len].iter().any(|(p, _)| *p == ptr) {
            return Err(MapPtrInsertError::Duplicate);
        }
        self.entries[self.len] = (ptr, idx);
        self.len += 1;
        Ok(())
    }

    fn get(&self, ptr: u64) -> Option<usize> {
        self.entries[..self.len]
            .iter()
            .find(|(p, _)| *p == ptr)
            .map(|(_, idx)| *idx)
    }
}

// ---------------------------------------------------------------------------
// Dispatch context
// ---------------------------------------------------------------------------

/// Raw-pointer bundle installed in the thread-local before BPF runs.
///
/// Every pointer is valid for the duration of a single
/// `interpreter.execute()` call. The owning `run_wake_cycle` holds
/// all objects on its stack.
struct DispatchContext {
    hal: *mut dyn Hal,
    transport: *mut dyn Transport,
    map_storage: *mut MapStorage,
    sleep_mgr: *mut SleepManager,
    clock: *const dyn Clock,
    identity: *const NodeIdentity,
    current_seq: *mut u64,
    program_class: ProgramClass,
    trace_log: *mut Vec<String>,
    async_queue: *mut AsyncQueue,
    gateway_timestamp_ms: u64,
    command_received_at_ms: u64,
    battery_mv: u32,
    /// Relocated map pointer → index mapping (linear scan, bounded by MAX_MAPS).
    map_ptr_index: MapPtrIndex,
    /// AEAD + SHA providers for AES-256-GCM frame encoding.
    aead: (*const dyn AeadProvider, *const dyn Sha256Provider),
}

thread_local! {
    static CTX: RefCell<Option<DispatchContext>> = const { RefCell::new(None) };
}

/// Maximum number of trace entries kept per BPF execution.
const MAX_TRACE_ENTRIES: usize = 64;

/// Borrow the context mutably and run `f` inside the borrow.
/// Returns `None` if no context is installed (helper called outside BPF
/// execution). Callers map `None` to the appropriate error sentinel.
fn with_ctx<R>(f: impl FnOnce(&mut DispatchContext) -> R) -> Option<R> {
    CTX.with(|cell| {
        let mut borrow = cell.borrow_mut();
        borrow.as_mut().map(f)
    })
}

// ---------------------------------------------------------------------------
// Lifecycle (called by run_wake_cycle)
// ---------------------------------------------------------------------------

/// Install with AEAD providers for AES-256-GCM frame encoding.
///
/// # Safety
///
/// All pointers must remain valid until [`clear`] is called.
/// The caller (`run_wake_cycle`) guarantees this by holding
/// ownership of every referenced object on its stack.
/// `aead` and `sha` must also remain valid.
#[allow(clippy::too_many_arguments)]
pub unsafe fn install(
    hal: *mut dyn Hal,
    transport: *mut dyn Transport,
    map_storage: *mut MapStorage,
    sleep_mgr: *mut SleepManager,
    clock: *const dyn Clock,
    identity: *const NodeIdentity,
    current_seq: *mut u64,
    program_class: ProgramClass,
    trace_log: *mut Vec<String>,
    async_queue: *mut AsyncQueue,
    gateway_timestamp_ms: u64,
    command_received_at_ms: u64,
    battery_mv: u32,
    aead: *const dyn AeadProvider,
    sha: *const dyn Sha256Provider,
) {
    install_core(
        hal,
        transport,
        map_storage,
        sleep_mgr,
        clock,
        identity,
        current_seq,
        program_class,
        trace_log,
        async_queue,
        gateway_timestamp_ms,
        command_received_at_ms,
        battery_mv,
        aead,
        sha,
    );
}

#[allow(clippy::too_many_arguments)]
unsafe fn install_core(
    hal: *mut dyn Hal,
    transport: *mut dyn Transport,
    map_storage: *mut MapStorage,
    sleep_mgr: *mut SleepManager,
    clock: *const dyn Clock,
    identity: *const NodeIdentity,
    current_seq: *mut u64,
    program_class: ProgramClass,
    trace_log: *mut Vec<String>,
    async_queue: *mut AsyncQueue,
    gateway_timestamp_ms: u64,
    command_received_at_ms: u64,
    battery_mv: u32,
    aead: *const dyn AeadProvider,
    sha: *const dyn Sha256Provider,
) {
    CTX.with(|cell| {
        let mut borrow = cell.borrow_mut();
        assert!(borrow.is_none(), "BPF dispatch context already installed");
        // Build pointer→index map for fast lookup in map helpers.
        let map_ptr_index = {
            // SAFETY: caller guarantees map_storage is valid until clear().
            let ms = unsafe { &*map_storage };
            let mut index = MapPtrIndex::new();
            let mut ok = true;
            for (i, &p) in ms.map_pointers().iter().enumerate() {
                match index.insert(p, i) {
                    Ok(()) => {}
                    Err(MapPtrInsertError::Overflow) => {
                        log::error!(
                            "map pointer index overflow at map {i} \
                             (capacity {MAX_MAPS}) — \
                             all map helpers will return errors this cycle"
                        );
                        ok = false;
                        break;
                    }
                    Err(MapPtrInsertError::Duplicate) => {
                        log::error!(
                            "duplicate map pointer at map {i} — \
                             all map helpers will return errors this cycle"
                        );
                        ok = false;
                        break;
                    }
                }
            }
            // If any insert failed, use an empty index so all map
            // operations fail consistently rather than having a partial
            // index where some maps work and others silently don't.
            if !ok {
                MapPtrIndex::new()
            } else {
                index
            }
        };
        *borrow = Some(DispatchContext {
            hal,
            transport,
            map_storage,
            sleep_mgr,
            clock,
            identity,
            current_seq,
            program_class,
            trace_log,
            async_queue,
            gateway_timestamp_ms,
            command_received_at_ms,
            battery_mv,
            map_ptr_index,
            aead: (aead, sha),
        });
    });
}

/// Clear the dispatch context after BPF execution completes.
pub fn clear() {
    CTX.with(|cell| {
        cell.borrow_mut().take();
    });
}

/// RAII guard that clears the dispatch context on drop.
pub struct DispatchGuard;

impl Drop for DispatchGuard {
    fn drop(&mut self) {
        clear();
    }
}

// ---------------------------------------------------------------------------
// Helper implementations (bare fn pointers for BpfInterpreter)
// ---------------------------------------------------------------------------
//
// # Safety contract for all helpers
//
// Each helper dereferences raw pointers from two sources:
//
// 1. **Dispatch context pointers** (`ctx.hal`, `ctx.transport`, etc.) —
//    guaranteed valid by `run_wake_cycle`, which holds all objects on
//    its stack for the duration of BPF execution.
//
// 2. **BPF register values** (`r1`–`r5` used as buffer pointers) —
//    guaranteed valid by the Prevail static verifier + interpreter
//    sandboxing. The verifier ensures all memory accesses fall within
//    the program's stack, context, or map regions. Null checks and
//    length caps are defence-in-depth against verifier bypass.

/// Helper 1: I2C read.
/// Args: r1=handle, r2=buf_ptr, r3=buf_len.
/// Returns: 0 on success, negative on error.
pub fn helper_i2c_read(r1: u64, r2: u64, r3: u64, _r4: u64, _r5: u64) -> u64 {
    let result = with_ctx(|ctx| {
        let handle = r1 as u32;
        let buf_ptr = r2 as *mut u8;
        let buf_len = r3 as usize;
        if buf_ptr.is_null() || buf_len == 0 || buf_len > MAX_BUS_TRANSFER_LEN {
            return (-1i64) as u64;
        }
        // SAFETY: buf_ptr and buf_len are verified by the BPF verifier
        // to point within the program's accessible memory regions.
        unsafe {
            let buf = core::slice::from_raw_parts_mut(buf_ptr, buf_len);
            (*ctx.hal).i2c_read(handle, buf) as i64 as u64
        }
    })
    .unwrap_or((-1i64) as u64);
    log::debug!("bpf helper i2c_read: result={}", result as i64);
    result
}

/// Helper 2: I2C write.
/// Args: r1=handle, r2=data_ptr, r3=data_len.
pub fn helper_i2c_write(r1: u64, r2: u64, r3: u64, _r4: u64, _r5: u64) -> u64 {
    let result = with_ctx(|ctx| {
        let handle = r1 as u32;
        let data_ptr = r2 as *const u8;
        let data_len = r3 as usize;
        if data_ptr.is_null() || data_len == 0 || data_len > MAX_BUS_TRANSFER_LEN {
            return (-1i64) as u64;
        }
        unsafe {
            let data = core::slice::from_raw_parts(data_ptr, data_len);
            (*ctx.hal).i2c_write(handle, data) as i64 as u64
        }
    })
    .unwrap_or((-1i64) as u64);
    log::debug!("bpf helper i2c_write: result={}", result as i64);
    result
}

/// Helper 3: I2C write-then-read.
/// Args: r1=handle, r2=write_ptr, r3=write_len, r4=read_ptr, r5=read_len.
pub fn helper_i2c_write_read(r1: u64, r2: u64, r3: u64, r4: u64, r5: u64) -> u64 {
    let result = with_ctx(|ctx| {
        let handle = r1 as u32;
        let write_ptr = r2 as *const u8;
        let write_len = r3 as usize;
        let read_ptr = r4 as *mut u8;
        let read_len = r5 as usize;
        if write_ptr.is_null()
            || read_ptr.is_null()
            || write_len == 0
            || read_len == 0
            || write_len > MAX_BUS_TRANSFER_LEN
            || read_len > MAX_BUS_TRANSFER_LEN
        {
            return (-1i64) as u64;
        }
        unsafe {
            let write_data = core::slice::from_raw_parts(write_ptr, write_len);
            let read_buf = core::slice::from_raw_parts_mut(read_ptr, read_len);
            (*ctx.hal).i2c_write_read(handle, write_data, read_buf) as i64 as u64
        }
    })
    .unwrap_or((-1i64) as u64);
    log::debug!("bpf helper i2c_write_read: result={}", result as i64);
    result
}

/// Helper 4: In-place SPI full-duplex transfer.
/// Args: r1=handle, r2=buf_ptr, r3=len.
/// The buffer is read for TX, then overwritten with RX.
pub fn helper_spi_transfer(r1: u64, r2: u64, r3: u64, _r4: u64, _r5: u64) -> u64 {
    let result = with_ctx(|ctx| {
        let handle = r1 as u32;
        let buf_ptr = r2 as *mut u8;
        let len = r3 as usize;
        if len == 0 || len > MAX_BUS_TRANSFER_LEN {
            return (-1i64) as u64;
        }
        if buf_ptr.is_null() {
            return (-1i64) as u64;
        }
        // SAFETY: buf_ptr is non-null (checked above), len is capped to
        // MAX_BUS_TRANSFER_LEN, and the BPF verifier guarantees the
        // argument points to a writable region of at least len bytes
        // via its PtrToWritableMem + ConstSize checks.
        unsafe {
            let buf = core::slice::from_raw_parts_mut(buf_ptr, len);
            (*ctx.hal).spi_transfer(handle, buf) as i64 as u64
        }
    })
    .unwrap_or((-1i64) as u64);
    log::debug!("bpf helper spi_transfer: result={}", result as i64);
    result
}

/// Helper 5: GPIO read.
/// Args: r1=pin.
pub fn helper_gpio_read(r1: u64, _r2: u64, _r3: u64, _r4: u64, _r5: u64) -> u64 {
    let result = with_ctx(|ctx| {
        let pin = r1 as u32;
        unsafe { (*ctx.hal).gpio_read(pin) as i64 as u64 }
    })
    .unwrap_or((-1i64) as u64);
    log::debug!("bpf helper gpio_read: result={}", result as i64);
    result
}

/// Helper 6: GPIO write.
/// Args: r1=pin, r2=value.
pub fn helper_gpio_write(r1: u64, r2: u64, _r3: u64, _r4: u64, _r5: u64) -> u64 {
    let result = with_ctx(|ctx| {
        let pin = r1 as u32;
        let value = r2 as u32;
        unsafe { (*ctx.hal).gpio_write(pin, value) as i64 as u64 }
    })
    .unwrap_or((-1i64) as u64);
    log::debug!("bpf helper gpio_write: result={}", result as i64);
    result
}

/// Helper 7: ADC read.
/// Args: r1=channel.
/// Returns: raw ADC reading on success, negative on error (invalid channel).
pub fn helper_adc_read(r1: u64, _r2: u64, _r3: u64, _r4: u64, _r5: u64) -> u64 {
    let result = with_ctx(|ctx| {
        let channel = r1 as u32;
        unsafe { (*ctx.hal).adc_read(channel) as i64 as u64 }
    })
    .unwrap_or((-1i64) as u64);
    log::debug!("bpf helper adc_read: result={}", result as i64);
    result
}

/// Helper 8: send (fire-and-forget APP_DATA).
/// Args: r1=blob_ptr, r2=blob_len.
/// Returns: 0 on success, negative on error.
pub fn helper_send(r1: u64, r2: u64, _r3: u64, _r4: u64, _r5: u64) -> u64 {
    let result = with_ctx(|ctx| {
        let blob_ptr = r1 as *const u8;
        let blob_len = r2 as usize;
        if blob_ptr.is_null() || blob_len > sonde_protocol::MAX_APP_DATA_BLOB_SIZE {
            return (-1i64) as u64;
        }

        unsafe {
            let blob = core::slice::from_raw_parts(blob_ptr, blob_len);
            let identity = &*ctx.identity;
            let transport = &mut *ctx.transport;
            let seq = &mut *ctx.current_seq;

            let (aead_ptr, sha_ptr) = ctx.aead;
            let aead = &*aead_ptr;
            let sha = &*sha_ptr;
            match crate::wake_cycle::send_app_data(transport, identity, seq, blob, aead, sha) {
                Ok(()) => 0,
                Err(_) => (-1i64) as u64,
            }
        }
    })
    .unwrap_or((-1i64) as u64);
    log::debug!("bpf helper send: result={}", result as i64);
    result
}

/// Helper 9: send_recv (APP_DATA + wait for APP_DATA_REPLY).
/// Args: r1=blob_ptr, r2=blob_len, r3=reply_ptr, r4=reply_cap, r5=timeout_ms (0=default).
/// Returns: reply length on success, negative on error.
pub fn helper_send_recv(r1: u64, r2: u64, r3: u64, r4: u64, r5: u64) -> u64 {
    let result = with_ctx(|ctx| {
        let blob_ptr = r1 as *const u8;
        let blob_len = r2 as usize;
        let reply_ptr = r3 as *mut u8;
        let reply_cap = r4 as usize;
        if blob_ptr.is_null()
            || blob_len > sonde_protocol::MAX_APP_DATA_BLOB_SIZE
            || reply_ptr.is_null()
            || reply_cap == 0
        {
            return (-1i64) as u64;
        }

        let timeout_ms = if r5 == 0 {
            SEND_RECV_TIMEOUT_MS
        } else {
            (r5 as u32).min(MAX_SEND_RECV_TIMEOUT_MS)
        };

        unsafe {
            let blob = core::slice::from_raw_parts(blob_ptr, blob_len);
            let identity = &*ctx.identity;
            let transport = &mut *ctx.transport;
            let clock = &*ctx.clock;
            let seq = &mut *ctx.current_seq;

            let (aead_ptr, sha_ptr) = ctx.aead;
            let aead = &*aead_ptr;
            let sha = &*sha_ptr;
            let send_result = crate::wake_cycle::send_recv_app_data(
                transport, identity, seq, blob, timeout_ms, clock, aead, sha,
            );

            match send_result {
                Ok(reply_blob) => {
                    if reply_blob.len() > reply_cap {
                        return (-1i64) as u64;
                    }
                    let copy_len = reply_blob.len();
                    let reply_buf = core::slice::from_raw_parts_mut(reply_ptr, copy_len);
                    reply_buf.copy_from_slice(&reply_blob);
                    copy_len as u64
                }
                Err(_) => (-1i64) as u64,
            }
        }
    })
    .unwrap_or((-1i64) as u64);
    log::debug!("bpf helper send_recv: result={}", result as i64);
    result
}

/// Helper 10: map_lookup_elem.
/// Args: r1=relocated map pointer, r2=key_ptr.
/// Returns: pointer to value, or 0 (NULL) on error/not-found.
pub fn helper_map_lookup_elem(r1: u64, r2: u64, _r3: u64, _r4: u64, _r5: u64) -> u64 {
    with_ctx(|ctx| {
        let key_ptr = r2 as *const u32;
        if key_ptr.is_null() {
            return 0;
        }
        unsafe {
            let key = core::ptr::read_unaligned(key_ptr);
            let map_idx = match ctx.map_ptr_index.get(r1) {
                Some(idx) => idx,
                None => return 0,
            };
            let maps = &*ctx.map_storage;
            match maps.get(map_idx) {
                Some(map) => match map.lookup(key) {
                    Some(value) => value.as_ptr() as u64,
                    None => 0,
                },
                None => 0,
            }
        }
    })
    .unwrap_or(0)
}

/// Helper 11: map_update_elem (blocked for ephemeral programs).
/// Args: r1=relocated map pointer, r2=key_ptr, r3=value_ptr.
/// Returns: 0 on success, negative on error.
pub fn helper_map_update_elem(r1: u64, r2: u64, r3: u64, _r4: u64, _r5: u64) -> u64 {
    with_ctx(|ctx| {
        if ctx.program_class == ProgramClass::Ephemeral {
            return (-1i64) as u64;
        }

        let key_ptr = r2 as *const u32;
        let value_ptr = r3 as *const u8;
        if key_ptr.is_null() || value_ptr.is_null() {
            return (-1i64) as u64;
        }
        unsafe {
            let key = core::ptr::read_unaligned(key_ptr);
            let map_idx = match ctx.map_ptr_index.get(r1) {
                Some(idx) => idx,
                None => return (-1i64) as u64,
            };
            let maps = &mut *ctx.map_storage;
            match maps.get_mut(map_idx) {
                Some(map) => {
                    let value_size = map.def.value_size as usize;
                    let value = core::slice::from_raw_parts(value_ptr, value_size);
                    match map.update(key, value) {
                        Ok(()) => 0,
                        Err(_) => (-1i64) as u64,
                    }
                }
                None => (-1i64) as u64,
            }
        }
    })
    .unwrap_or((-1i64) as u64)
}

/// Helper 12: get_time.
/// Returns: estimated epoch time in milliseconds.
pub fn helper_get_time(_r1: u64, _r2: u64, _r3: u64, _r4: u64, _r5: u64) -> u64 {
    with_ctx(|ctx| unsafe {
        let elapsed = (*ctx.clock)
            .elapsed_ms()
            .saturating_sub(ctx.command_received_at_ms);
        ctx.gateway_timestamp_ms.saturating_add(elapsed)
    })
    .unwrap_or(0)
}

/// Helper 13: get_battery_mv.
/// Returns: battery voltage in millivolts, clamped to u16 to match
/// the BPF execution context `ctx->battery_mv` field.
pub fn helper_get_battery_mv(_r1: u64, _r2: u64, _r3: u64, _r4: u64, _r5: u64) -> u64 {
    with_ctx(|ctx| {
        if ctx.battery_mv > u16::MAX as u32 {
            u16::MAX as u64
        } else {
            ctx.battery_mv as u64
        }
    })
    .unwrap_or(0)
}

/// Helper 14: delay_us.
/// Args: r1=microseconds (max 1 second).
/// Returns: 0 on success, negative if delay exceeds maximum.
pub fn helper_delay_us(r1: u64, _r2: u64, _r3: u64, _r4: u64, _r5: u64) -> u64 {
    if r1 > MAX_DELAY_US as u64 {
        return (-1i64) as u64;
    }
    let us = r1 as u32;
    with_ctx(|ctx| {
        if us > 0 {
            unsafe {
                (*ctx.clock).delay_us(us);
            }
        }
        0
    })
    .unwrap_or((-1i64) as u64)
}

/// Helper 15: set_next_wake (blocked for ephemeral programs).
/// Args: r1=seconds.
pub fn helper_set_next_wake(r1: u64, _r2: u64, _r3: u64, _r4: u64, _r5: u64) -> u64 {
    with_ctx(|ctx| {
        if ctx.program_class == ProgramClass::Ephemeral {
            return (-1i64) as u64;
        }
        let seconds = match u32::try_from(r1) {
            Ok(s) => s,
            Err(_) => return (-1i64) as u64,
        };
        unsafe {
            (*ctx.sleep_mgr).set_next_wake(seconds);
        }
        0
    })
    .unwrap_or((-1i64) as u64)
}

/// Helper 16: bpf_trace_printk.
/// Args: r1=fmt_ptr, r2=fmt_len.
pub fn helper_bpf_trace_printk(r1: u64, r2: u64, _r3: u64, _r4: u64, _r5: u64) -> u64 {
    with_ctx(|ctx| {
        let fmt_ptr = r1 as *const u8;
        let fmt_len = r2 as usize;
        if fmt_ptr.is_null() || fmt_len == 0 || fmt_len > 256 {
            return (-1i64) as u64;
        }
        unsafe {
            let bytes = core::slice::from_raw_parts(fmt_ptr, fmt_len);
            match core::str::from_utf8(bytes) {
                Ok(s) => {
                    let log = &mut *ctx.trace_log;
                    if log.len() < MAX_TRACE_ENTRIES {
                        log.push(s.to_string());
                    }
                    0
                }
                Err(_) => (-1i64) as u64,
            }
        }
    })
    .unwrap_or((-1i64) as u64)
}

/// Helper 17: send_async (queue blob for deferred transmission).
/// Args: r1=blob_ptr, r2=blob_len.
/// Returns: 0 on success, -1 if queue is full, -2 if blob is oversized.
pub fn helper_send_async(r1: u64, r2: u64, _r3: u64, _r4: u64, _r5: u64) -> u64 {
    let result = with_ctx(|ctx| {
        let blob_ptr = r1 as *const u8;
        if r2 > usize::MAX as u64 {
            return (-2i64) as u64;
        }
        let blob_len = r2 as usize;
        if blob_ptr.is_null() || blob_len > sonde_protocol::MAX_APP_DATA_BLOB_SIZE {
            return (-2i64) as u64;
        }

        unsafe {
            let blob = core::slice::from_raw_parts(blob_ptr, blob_len);
            let queue = &mut *ctx.async_queue;
            queue.push(blob.to_vec()) as u64
        }
    })
    .unwrap_or((-1i64) as u64);
    log::debug!("bpf helper send_async: result={}", result as i64);
    result
}

/// Register all 17 helpers with the interpreter.
pub fn register_all(
    interpreter: &mut impl crate::bpf_runtime::BpfInterpreter,
) -> Result<(), crate::bpf_runtime::BpfError> {
    use crate::bpf_helpers::helper_ids::*;
    interpreter.register_helper(I2C_READ, helper_i2c_read)?;
    interpreter.register_helper(I2C_WRITE, helper_i2c_write)?;
    interpreter.register_helper(I2C_WRITE_READ, helper_i2c_write_read)?;
    interpreter.register_helper(SPI_TRANSFER, helper_spi_transfer)?;
    interpreter.register_helper(GPIO_READ, helper_gpio_read)?;
    interpreter.register_helper(GPIO_WRITE, helper_gpio_write)?;
    interpreter.register_helper(ADC_READ, helper_adc_read)?;
    interpreter.register_helper(SEND, helper_send)?;
    interpreter.register_helper(SEND_RECV, helper_send_recv)?;
    interpreter.register_helper(MAP_LOOKUP_ELEM, helper_map_lookup_elem)?;
    interpreter.register_helper(MAP_UPDATE_ELEM, helper_map_update_elem)?;
    interpreter.register_helper(GET_TIME, helper_get_time)?;
    interpreter.register_helper(GET_BATTERY_MV, helper_get_battery_mv)?;
    interpreter.register_helper(DELAY_US, helper_delay_us)?;
    interpreter.register_helper(SET_NEXT_WAKE, helper_set_next_wake)?;
    interpreter.register_helper(BPF_TRACE_PRINTK, helper_bpf_trace_printk)?;
    interpreter.register_helper(SEND_ASYNC, helper_send_async)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::async_queue::AsyncQueue;
    use crate::bpf_runtime::BpfInterpreter;
    use crate::error::NodeResult;
    use crate::hal::Hal;
    use crate::map_storage::MapStorage;
    use crate::sleep::{SleepManager, WakeReason};
    use crate::traits::{Clock, Transport};
    use object::{Object, ObjectSection, ObjectSymbol, RelocationTarget};
    use sonde_protocol::{MapDef, NodeMessage, MSG_APP_DATA};
    use std::cell::RefCell;
    use std::collections::VecDeque;
    // -- Test mocks ---------------------------------------------------------

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum I2cEvent {
        Write { handle: u32, data: Vec<u8> },
        WriteRead { handle: u32, write: Vec<u8> },
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum I2cExpectation {
        Write {
            handle: u32,
            data: Vec<u8>,
            rc: i32,
        },
        WriteRead {
            handle: u32,
            write: Vec<u8>,
            read: Vec<u8>,
            rc: i32,
        },
    }

    struct TestHal {
        /// Data returned by i2c_read.
        i2c_read_data: Vec<u8>,
        /// Return code for i2c operations (-1 = NACK).
        i2c_return: i32,
        strict_i2c: bool,
        i2c_events: Vec<I2cEvent>,
        i2c_expectations: VecDeque<I2cExpectation>,
        gpio_states: [i32; 32],
        adc_values: [i32; 8],
    }

    impl TestHal {
        fn new() -> Self {
            Self {
                i2c_read_data: vec![0x1A, 0x2B],
                i2c_return: 0,
                strict_i2c: false,
                i2c_events: Vec::new(),
                i2c_expectations: VecDeque::new(),
                gpio_states: [0; 32],
                adc_values: [0; 8],
            }
        }

        fn expect_i2c_write(&mut self, handle: u32, data: &[u8], rc: i32) {
            self.i2c_expectations.push_back(I2cExpectation::Write {
                handle,
                data: data.to_vec(),
                rc,
            });
        }

        fn expect_i2c_write_read(&mut self, handle: u32, write: &[u8], read: &[u8], rc: i32) {
            self.i2c_expectations.push_back(I2cExpectation::WriteRead {
                handle,
                write: write.to_vec(),
                read: read.to_vec(),
                rc,
            });
        }
    }

    impl Hal for TestHal {
        fn i2c_read(&mut self, _handle: u32, buf: &mut [u8]) -> i32 {
            if self.i2c_return != 0 {
                return self.i2c_return;
            }
            let copy_len = buf.len().min(self.i2c_read_data.len());
            buf[..copy_len].copy_from_slice(&self.i2c_read_data[..copy_len]);
            0
        }
        fn i2c_write(&mut self, handle: u32, data: &[u8]) -> i32 {
            self.i2c_events.push(I2cEvent::Write {
                handle,
                data: data.to_vec(),
            });
            if let Some(expectation) = self.i2c_expectations.pop_front() {
                match expectation {
                    I2cExpectation::Write {
                        handle: expected_handle,
                        data: expected_data,
                        rc,
                    } if expected_handle == handle && expected_data == data => {
                        return rc;
                    }
                    other => {
                        self.i2c_expectations.push_front(other);
                        return -1;
                    }
                }
            }
            if self.strict_i2c {
                return -1;
            }
            self.i2c_return
        }
        fn i2c_write_read(&mut self, handle: u32, w: &[u8], buf: &mut [u8]) -> i32 {
            self.i2c_events.push(I2cEvent::WriteRead {
                handle,
                write: w.to_vec(),
            });
            if let Some(expectation) = self.i2c_expectations.pop_front() {
                match expectation {
                    I2cExpectation::WriteRead {
                        handle: expected_handle,
                        write: expected_write,
                        read,
                        rc,
                    } if expected_handle == handle && expected_write == w => {
                        if rc == 0 {
                            if self.strict_i2c {
                                assert_eq!(
                                    read.len(),
                                    buf.len(),
                                    "strict i2c expectation length mismatch: expected {} bytes, program requested {}",
                                    read.len(),
                                    buf.len()
                                );
                            }
                            let copy_len = buf.len().min(read.len());
                            buf[..copy_len].copy_from_slice(&read[..copy_len]);
                        }
                        return rc;
                    }
                    other => {
                        self.i2c_expectations.push_front(other);
                        return -1;
                    }
                }
            }
            if self.strict_i2c {
                return -1;
            }
            if self.i2c_return != 0 {
                return self.i2c_return;
            }
            let copy_len = buf.len().min(self.i2c_read_data.len());
            buf[..copy_len].copy_from_slice(&self.i2c_read_data[..copy_len]);
            0
        }
        fn spi_transfer(&mut self, _handle: u32, buf: &mut [u8]) -> i32 {
            // Simulate SPI RX by flipping all bits.  This proves the helper
            // passes the correct slice to the HAL and that the caller
            // observes the overwritten contents.
            for b in buf.iter_mut() {
                *b ^= 0xFF;
            }
            0
        }
        fn gpio_read(&self, pin: u32) -> i32 {
            self.gpio_states.get(pin as usize).copied().unwrap_or(-1)
        }
        fn gpio_write(&mut self, pin: u32, value: u32) -> i32 {
            if let Some(slot) = self.gpio_states.get_mut(pin as usize) {
                *slot = value as i32;
                0
            } else {
                -1
            }
        }
        fn adc_read(&mut self, channel: u32) -> i32 {
            self.adc_values.get(channel as usize).copied().unwrap_or(-1)
        }
    }

    struct TestTransport {
        outbound: Vec<Vec<u8>>,
        inbound: std::collections::VecDeque<Option<Vec<u8>>>,
        /// Timeout values passed to recv(), recorded for assertions.
        recv_timeouts: Vec<u32>,
    }
    impl TestTransport {
        fn new() -> Self {
            Self {
                outbound: Vec::new(),
                inbound: std::collections::VecDeque::new(),
                recv_timeouts: Vec::new(),
            }
        }
    }
    impl Transport for TestTransport {
        fn send(&mut self, frame: &[u8]) -> NodeResult<()> {
            self.outbound.push(frame.to_vec());
            Ok(())
        }
        fn recv(&mut self, timeout_ms: u32) -> NodeResult<Option<Vec<u8>>> {
            self.recv_timeouts.push(timeout_ms);
            Ok(self.inbound.pop_front().flatten())
        }
    }

    struct TestClock(u64);
    impl Clock for TestClock {
        fn elapsed_ms(&self) -> u64 {
            self.0
        }
        fn delay_ms(&self, _ms: u32) {}
    }

    struct RecordingClock {
        elapsed_ms: u64,
        delays_ms: RefCell<Vec<u32>>,
        delays_us: RefCell<Vec<u32>>,
    }

    impl RecordingClock {
        fn new(elapsed_ms: u64) -> Self {
            Self {
                elapsed_ms,
                delays_ms: RefCell::new(Vec::new()),
                delays_us: RefCell::new(Vec::new()),
            }
        }
    }

    impl Clock for RecordingClock {
        fn elapsed_ms(&self) -> u64 {
            self.elapsed_ms
        }

        fn delay_ms(&self, ms: u32) {
            self.delays_ms.borrow_mut().push(ms);
        }

        fn delay_us(&self, us: u32) {
            self.delays_us.borrow_mut().push(us);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn with_recording_clock_test_context_and_queue<F, R>(
        hal: &mut TestHal,
        transport: &mut TestTransport,
        map_storage: &mut MapStorage,
        sleep_mgr: &mut SleepManager,
        clock: &RecordingClock,
        identity: &NodeIdentity,
        seq: &mut u64,
        program_class: ProgramClass,
        trace_log: &mut Vec<String>,
        async_queue: &mut AsyncQueue,
        f: F,
    ) -> R
    where
        F: FnOnce() -> R,
    {
        let aead = crate::node_aead::NodeAead;
        let sha = crate::crypto::SoftwareSha256;
        unsafe {
            install(
                hal as *mut TestHal as *mut dyn Hal,
                transport as *mut TestTransport as *mut dyn Transport,
                map_storage as *mut MapStorage,
                sleep_mgr as *mut SleepManager,
                clock as *const RecordingClock as *const dyn Clock,
                identity as *const NodeIdentity,
                seq as *mut u64,
                program_class,
                trace_log as *mut Vec<String>,
                async_queue as *mut AsyncQueue,
                1_710_000_000_000,
                100,
                3300,
                &aead,
                &sha,
            );
        }
        let _guard = DispatchGuard;
        f()
    }

    /// Install the dispatch context for the test, run `f`, then clear.
    #[allow(clippy::too_many_arguments)]
    fn with_test_context<F, R>(
        hal: &mut TestHal,
        transport: &mut TestTransport,
        map_storage: &mut MapStorage,
        sleep_mgr: &mut SleepManager,
        clock: &TestClock,
        identity: &NodeIdentity,
        seq: &mut u64,
        program_class: ProgramClass,
        trace_log: &mut Vec<String>,
        f: F,
    ) -> R
    where
        F: FnOnce() -> R,
    {
        let mut queue = AsyncQueue::new();
        with_test_clock_context_and_queue(
            hal,
            transport,
            map_storage,
            sleep_mgr,
            clock,
            identity,
            seq,
            program_class,
            trace_log,
            &mut queue,
            f,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn with_test_clock_context_and_queue<F, R>(
        hal: &mut TestHal,
        transport: &mut TestTransport,
        map_storage: &mut MapStorage,
        sleep_mgr: &mut SleepManager,
        clock: &TestClock,
        identity: &NodeIdentity,
        seq: &mut u64,
        program_class: ProgramClass,
        trace_log: &mut Vec<String>,
        async_queue: &mut AsyncQueue,
        f: F,
    ) -> R
    where
        F: FnOnce() -> R,
    {
        let aead = crate::node_aead::NodeAead;
        let sha = crate::crypto::SoftwareSha256;
        unsafe {
            install(
                hal as *mut TestHal as *mut dyn Hal,
                transport as *mut TestTransport as *mut dyn Transport,
                map_storage as *mut MapStorage,
                sleep_mgr as *mut SleepManager,
                clock as *const TestClock as *const dyn Clock,
                identity as *const NodeIdentity,
                seq as *mut u64,
                program_class,
                trace_log as *mut Vec<String>,
                async_queue as *mut AsyncQueue,
                1_710_000_000_000,
                100,
                3300,
                &aead,
                &sha,
            );
        }
        let _guard = DispatchGuard;
        f()
    }

    // Pre-compiled BPF ELF for `test-programs/veml7700_sensor.c`
    // (`clang -target bpf -O2 -Wall -Wextra -DSONDE_DISABLE_DECODER=1`).
    // Embedding avoids a runtime clang dependency in host/CI tests.
    const VEML7700_SENSOR_ELF_HEX: &str = include_str!("veml7700_sensor_elf.hex");

    fn decode_veml7700_sensor_elf() -> Vec<u8> {
        let hex = VEML7700_SENSOR_ELF_HEX.trim();
        assert!(
            hex.len().is_multiple_of(2),
            "embedded VEML7700 ELF hex should contain an even number of digits"
        );

        let mut bytes = Vec::with_capacity(hex.len() / 2);
        for i in (0..hex.len()).step_by(2) {
            let byte = u8::from_str_radix(&hex[i..i + 2], 16)
                .expect("embedded VEML7700 ELF hex should be valid");
            bytes.push(byte);
        }
        bytes
    }

    fn compile_veml7700_program_image() -> sonde_protocol::ProgramImage {
        const VEML7700_STATE_SIZE: u32 = 16;
        const STATE_MAP_TYPE: u32 = 1;
        const STATE_MAP_DEF_SIZE: u64 = 32;

        let elf = decode_veml7700_sensor_elf();
        let file = object::File::parse(&*elf).expect("compiled ELF should parse");
        let maps_section = file
            .section_by_name(".maps")
            .expect("compiled ELF should contain a .maps section");
        assert_eq!(
            maps_section
                .data()
                .expect(".maps section bytes should be readable")
                .len() as u64,
            STATE_MAP_DEF_SIZE,
            "embedded ELF .maps section size should match the single-entry state_map definition"
        );
        let state_map_symbol = file
            .symbols()
            .find(|symbol| symbol.name().ok() == Some("state_map"))
            .expect("compiled ELF should define the state_map symbol");
        assert_eq!(
            state_map_symbol.size(),
            STATE_MAP_DEF_SIZE,
            "embedded ELF state_map symbol size should match the expected map-definition layout"
        );
        let section = file
            .section_by_name("sonde")
            .expect("compiled ELF should contain a sonde section");
        let mut bytecode = section
            .data()
            .expect("sonde section bytes should be readable")
            .to_vec();
        let relocations: Vec<usize> = section
            .relocations()
            .filter_map(|(offset, relocation)| {
                let RelocationTarget::Symbol(symbol_index) = relocation.target() else {
                    return None;
                };
                let symbol = file
                    .symbol_by_index(symbol_index)
                    .expect("relocation symbol should be readable");
                let symbol_name = symbol.name().expect("relocation symbol should be named");
                if symbol_name != "state_map" {
                    return None;
                }
                Some(relocation_lldw_start(
                    &bytecode,
                    offset as usize,
                    symbol_name,
                ))
            })
            .collect();
        assert!(
            !relocations.is_empty(),
            "compiled ELF should contain at least one state_map relocation"
        );

        let mut patched_offsets = Vec::new();
        for offset in relocations {
            if patched_offsets.contains(&offset) {
                continue;
            }
            patched_offsets.push(offset);
            bytecode[offset + 1] = (bytecode[offset + 1] & 0x0f) | 0x10;
            bytecode[offset + 4..offset + 8].copy_from_slice(&0i32.to_le_bytes());
            bytecode[offset + 12..offset + 16].copy_from_slice(&0i32.to_le_bytes());
        }

        sonde_protocol::ProgramImage {
            bytecode,
            maps: vec![MapDef {
                // Must match `state_map` in `test-programs/veml7700_sensor.c`
                // and `BPF_MAP_TYPE_ARRAY` in `include/sonde_helpers.h`.
                map_type: STATE_MAP_TYPE,
                key_size: 4,
                // Kept in sync with the C source by the compile-time assertion
                // `veml7700_state_size_must_be_16`.
                value_size: VEML7700_STATE_SIZE,
                max_entries: 1,
            }],
            map_initial_data: vec![Vec::new()],
            map_readonly: vec![false],
        }
    }

    fn relocation_lldw_start(
        bytecode: &[u8],
        relocation_offset: usize,
        symbol_name: &str,
    ) -> usize {
        for candidate in [relocation_offset, relocation_offset.saturating_sub(4)] {
            let Some(opcode) = bytecode.get(candidate).copied() else {
                continue;
            };
            if opcode == 0x18 && relocation_offset >= candidate && relocation_offset < candidate + 8
            {
                return candidate;
            }
        }

        panic!(
            "relocation for {symbol_name} at byte offset {relocation_offset} does not point to an LDDW instruction"
        );
    }

    fn default_identity() -> NodeIdentity {
        NodeIdentity {
            key_hint: 1,
            psk: [0xAA; 32],
        }
    }

    fn encode_veml7700_state(
        recent_lux_ml: [u32; 3],
        current_conf: u16,
        recent_count: u8,
        recent_index: u8,
    ) -> [u8; 16] {
        let mut bytes = [0u8; 16];
        bytes[0..4].copy_from_slice(&recent_lux_ml[0].to_le_bytes());
        bytes[4..8].copy_from_slice(&recent_lux_ml[1].to_le_bytes());
        bytes[8..12].copy_from_slice(&recent_lux_ml[2].to_le_bytes());
        bytes[12..14].copy_from_slice(&current_conf.to_le_bytes());
        bytes[14] = recent_count;
        bytes[15] = recent_index;
        bytes
    }

    // -- Tests --------------------------------------------------------------

    #[test]
    fn test_context_lifecycle() {
        // Verify install sets context and clear removes it.
        let mut hal = TestHal::new();
        let mut transport = TestTransport::new();
        let mut maps = MapStorage::new(4096);
        let mut sleep = SleepManager::new(60, WakeReason::Scheduled);
        let clock = TestClock(1000);
        let identity = default_identity();
        let mut seq = 0u64;
        let mut trace = Vec::new();

        with_test_context(
            &mut hal,
            &mut transport,
            &mut maps,
            &mut sleep,
            &clock,
            &identity,
            &mut seq,
            ProgramClass::Resident,
            &mut trace,
            || {
                // Should not panic — context is installed
                // get_time: 1_710_000_000_000 + (1000 - 100) = 1_710_000_000_900
                let result = helper_get_time(0, 0, 0, 0, 0);
                assert_eq!(result, 1_710_000_000_900);
            },
        );
    }

    #[test]
    fn test_helper_i2c_read() {
        // T-N600: I2C read returns data from mock device.
        let mut hal = TestHal::new();
        hal.i2c_read_data = vec![0x1A, 0x2B];
        let mut transport = TestTransport::new();
        let mut maps = MapStorage::new(4096);
        let mut sleep = SleepManager::new(60, WakeReason::Scheduled);
        let clock = TestClock(0);
        let identity = default_identity();
        let mut seq = 0u64;
        let mut trace = Vec::new();
        let mut buf = [0u8; 2];

        with_test_context(
            &mut hal,
            &mut transport,
            &mut maps,
            &mut sleep,
            &clock,
            &identity,
            &mut seq,
            ProgramClass::Resident,
            &mut trace,
            || {
                let handle = crate::hal::i2c_handle(0, 0x48);
                let result = helper_i2c_read(
                    handle as u64,
                    buf.as_mut_ptr() as u64,
                    buf.len() as u64,
                    0,
                    0,
                );
                assert_eq!(result, 0);
            },
        );
        assert_eq!(buf, [0x1A, 0x2B]);
    }

    #[test]
    fn test_helper_i2c_error() {
        // T-N601: I2C NACK → helper returns negative.
        let mut hal = TestHal::new();
        hal.i2c_return = -1;
        let mut transport = TestTransport::new();
        let mut maps = MapStorage::new(4096);
        let mut sleep = SleepManager::new(60, WakeReason::Scheduled);
        let clock = TestClock(0);
        let identity = default_identity();
        let mut seq = 0u64;
        let mut trace = Vec::new();
        let mut buf = [0u8; 2];

        with_test_context(
            &mut hal,
            &mut transport,
            &mut maps,
            &mut sleep,
            &clock,
            &identity,
            &mut seq,
            ProgramClass::Resident,
            &mut trace,
            || {
                let result =
                    helper_i2c_read(0x0048, buf.as_mut_ptr() as u64, buf.len() as u64, 0, 0);
                assert_eq!(result as i64, -1);
            },
        );
    }

    #[test]
    fn test_helper_i2c_write_read_rejects_zero_length() {
        // Zero-length write_len or read_len must return -1.
        let mut hal = TestHal::new();
        let mut transport = TestTransport::new();
        let mut maps = MapStorage::new(4096);
        let mut sleep = SleepManager::new(60, WakeReason::Scheduled);
        let clock = TestClock(0);
        let identity = default_identity();
        let mut seq = 0u64;
        let mut trace = Vec::new();
        let write_buf = [0x42u8; 2];
        let mut read_buf = [0u8; 2];

        with_test_context(
            &mut hal,
            &mut transport,
            &mut maps,
            &mut sleep,
            &clock,
            &identity,
            &mut seq,
            ProgramClass::Resident,
            &mut trace,
            || {
                let handle = crate::hal::i2c_handle(0, 0x48) as u64;

                // Zero write_len → -1
                let result = helper_i2c_write_read(
                    handle,
                    write_buf.as_ptr() as u64,
                    0, // write_len = 0
                    read_buf.as_mut_ptr() as u64,
                    read_buf.len() as u64,
                );
                assert_eq!(result as i64, -1, "zero write_len should be rejected");

                // Zero read_len → -1
                let result = helper_i2c_write_read(
                    handle,
                    write_buf.as_ptr() as u64,
                    write_buf.len() as u64,
                    read_buf.as_mut_ptr() as u64,
                    0, // read_len = 0
                );
                assert_eq!(result as i64, -1, "zero read_len should be rejected");
            },
        );
    }

    #[test]
    fn test_helper_spi_transfer() {
        // T-N602: SPI in-place transfer — mock XORs buffer with 0xFF.
        let mut hal = TestHal::new();
        let mut transport = TestTransport::new();
        let mut maps = MapStorage::new(4096);
        let mut sleep = SleepManager::new(60, WakeReason::Scheduled);
        let clock = TestClock(0);
        let identity = default_identity();
        let mut seq = 0u64;
        let mut trace = Vec::new();
        let mut buf = [0xDE, 0xAD, 0xBE, 0xEF];
        let expected_rx: [u8; 4] = [0xDE ^ 0xFF, 0xAD ^ 0xFF, 0xBE ^ 0xFF, 0xEF ^ 0xFF];

        with_test_context(
            &mut hal,
            &mut transport,
            &mut maps,
            &mut sleep,
            &clock,
            &identity,
            &mut seq,
            ProgramClass::Resident,
            &mut trace,
            || {
                let handle = crate::hal::spi_handle(0);
                let result = helper_spi_transfer(
                    handle as u64,
                    buf.as_mut_ptr() as u64,
                    buf.len() as u64,
                    0,
                    0,
                );
                assert_eq!(result, 0);
            },
        );
        // Mock XORs each byte with 0xFF — proves HAL received the buffer.
        assert_eq!(buf, expected_rx);
    }

    #[test]
    fn test_helper_gpio_and_adc() {
        // T-N603: GPIO pin 5=HIGH, ADC channel 0=2048.
        let mut hal = TestHal::new();
        hal.gpio_states[5] = 1;
        hal.adc_values[0] = 2048;
        let mut transport = TestTransport::new();
        let mut maps = MapStorage::new(4096);
        let mut sleep = SleepManager::new(60, WakeReason::Scheduled);
        let clock = TestClock(0);
        let identity = default_identity();
        let mut seq = 0u64;
        let mut trace = Vec::new();

        with_test_context(
            &mut hal,
            &mut transport,
            &mut maps,
            &mut sleep,
            &clock,
            &identity,
            &mut seq,
            ProgramClass::Resident,
            &mut trace,
            || {
                assert_eq!(helper_gpio_read(5, 0, 0, 0, 0), 1);
                assert_eq!(helper_adc_read(0, 0, 0, 0, 0), 2048);
            },
        );
    }

    #[test]
    fn test_helper_map_lookup_update() {
        // T-N607: Write 42 to key 0, read it back.
        let mut hal = TestHal::new();
        let mut transport = TestTransport::new();
        let mut maps = MapStorage::new(4096);
        maps.allocate(&[MapDef {
            map_type: 1,
            key_size: 4,
            value_size: 4,
            max_entries: 4,
        }])
        .unwrap();
        let map_ptr = maps.map_pointers()[0];
        let mut sleep = SleepManager::new(60, WakeReason::Scheduled);
        let clock = TestClock(0);
        let identity = default_identity();
        let mut seq = 0u64;
        let mut trace = Vec::new();

        with_test_context(
            &mut hal,
            &mut transport,
            &mut maps,
            &mut sleep,
            &clock,
            &identity,
            &mut seq,
            ProgramClass::Resident,
            &mut trace,
            || {
                let key: u32 = 0;
                let value: [u8; 4] = 42u32.to_ne_bytes();

                // Update
                let result = helper_map_update_elem(
                    map_ptr,
                    &key as *const u32 as u64,
                    value.as_ptr() as u64,
                    0,
                    0,
                );
                assert_eq!(result, 0);

                // Lookup
                let ptr = helper_map_lookup_elem(map_ptr, &key as *const u32 as u64, 0, 0, 0);
                assert_ne!(ptr, 0, "lookup should return non-null pointer");

                let read_value = unsafe { core::ptr::read_unaligned(ptr as *const u32) };
                assert_eq!(read_value, 42);
            },
        );
    }

    #[test]
    fn test_helper_map_update_ephemeral_rejected() {
        // T-N609: Ephemeral program cannot write maps.
        let mut hal = TestHal::new();
        let mut transport = TestTransport::new();
        let mut maps = MapStorage::new(4096);
        maps.allocate(&[MapDef {
            map_type: 1,
            key_size: 4,
            value_size: 4,
            max_entries: 4,
        }])
        .unwrap();
        let map_ptr = maps.map_pointers()[0];
        let mut sleep = SleepManager::new(60, WakeReason::Scheduled);
        let clock = TestClock(0);
        let identity = default_identity();
        let mut seq = 0u64;
        let mut trace = Vec::new();

        with_test_context(
            &mut hal,
            &mut transport,
            &mut maps,
            &mut sleep,
            &clock,
            &identity,
            &mut seq,
            ProgramClass::Ephemeral, // ← ephemeral
            &mut trace,
            || {
                let key: u32 = 0;
                let value: [u8; 4] = 99u32.to_ne_bytes();
                let result = helper_map_update_elem(
                    map_ptr,
                    &key as *const u32 as u64,
                    value.as_ptr() as u64,
                    0,
                    0,
                );
                assert_eq!(result as i64, -1, "ephemeral should be rejected");
            },
        );

        // Verify map unchanged (still zero)
        let val = maps.get(0).unwrap().lookup(0).unwrap();
        assert!(val.iter().all(|&b| b == 0));
    }

    #[test]
    fn test_helper_get_time_and_battery() {
        // T-N610: get_time returns epoch estimate, get_battery_mv returns captured value.
        let mut hal = TestHal::new();
        let mut transport = TestTransport::new();
        let mut maps = MapStorage::new(4096);
        let mut sleep = SleepManager::new(60, WakeReason::Scheduled);
        let clock = TestClock(200);
        let identity = default_identity();
        let mut seq = 0u64;
        let mut trace = Vec::new();

        // Override defaults: gateway_timestamp_ms=1_710_000_000_000,
        // command_received_at_ms=100, battery_mv=3300
        // Expected get_time: 1_710_000_000_000 + (200 - 100) = 1_710_000_000_100
        let aead = crate::node_aead::NodeAead;
        let sha = crate::crypto::SoftwareSha256;
        let mut queue = AsyncQueue::new();
        unsafe {
            install(
                &mut hal as *mut TestHal as *mut dyn Hal,
                &mut transport as *mut TestTransport as *mut dyn Transport,
                &mut maps as *mut MapStorage,
                &mut sleep as *mut SleepManager,
                &clock as *const TestClock as *const dyn Clock,
                &identity as *const NodeIdentity,
                &mut seq as *mut u64,
                ProgramClass::Resident,
                &mut trace as *mut Vec<String>,
                &mut queue as *mut AsyncQueue,
                1_710_000_000_000,
                100,
                3300,
                &aead,
                &sha,
            );
        }
        let _guard = DispatchGuard;
        assert_eq!(helper_get_time(0, 0, 0, 0, 0), 1_710_000_000_100);
        assert_eq!(helper_get_battery_mv(0, 0, 0, 0, 0), 3300);
    }

    #[test]
    fn test_helper_delay_us() {
        // T-N611: delay_us does not crash and returns 0.
        let mut hal = TestHal::new();
        let mut transport = TestTransport::new();
        let mut maps = MapStorage::new(4096);
        let mut sleep = SleepManager::new(60, WakeReason::Scheduled);
        let clock = TestClock(0);
        let identity = default_identity();
        let mut seq = 0u64;
        let mut trace = Vec::new();

        with_test_context(
            &mut hal,
            &mut transport,
            &mut maps,
            &mut sleep,
            &clock,
            &identity,
            &mut seq,
            ProgramClass::Resident,
            &mut trace,
            || {
                assert_eq!(helper_delay_us(1000, 0, 0, 0, 0), 0);
                assert_eq!(helper_delay_us(0, 0, 0, 0, 0), 0);
            },
        );
    }

    #[test]
    fn test_helper_delay_us_max_enforcement() {
        // ND-0604 AC3: delay_us with value exceeding MAX_DELAY_US (1 s)
        // must return an error (-1) and must NOT busy-wait. A tracking
        // clock verifies that delay_ms is only called for accepted values.
        use std::cell::Cell;

        struct TrackingClock {
            delay_calls: Cell<u32>,
        }
        impl Clock for TrackingClock {
            fn elapsed_ms(&self) -> u64 {
                0
            }
            fn delay_ms(&self, _ms: u32) {
                self.delay_calls.set(self.delay_calls.get() + 1);
            }
        }

        let mut hal = TestHal::new();
        let mut transport = TestTransport::new();
        let mut maps = MapStorage::new(4096);
        let mut sleep = SleepManager::new(60, WakeReason::Scheduled);
        let clock = TrackingClock {
            delay_calls: Cell::new(0),
        };
        let identity = default_identity();
        let mut seq = 0u64;
        let mut trace = Vec::new();

        let aead = crate::node_aead::NodeAead;
        let sha_prov = crate::crypto::SoftwareSha256;
        let mut queue = AsyncQueue::new();
        unsafe {
            install(
                &mut hal as *mut TestHal as *mut dyn Hal,
                &mut transport as *mut TestTransport as *mut dyn Transport,
                &mut maps as *mut MapStorage,
                &mut sleep as *mut SleepManager,
                &clock as *const TrackingClock as *const dyn Clock,
                &identity as *const NodeIdentity,
                &mut seq as *mut u64,
                ProgramClass::Resident,
                &mut trace as *mut Vec<String>,
                &mut queue as *mut AsyncQueue,
                1_710_000_000_000,
                100,
                3300,
                &aead,
                &sha_prov,
            );
        }
        let _guard = DispatchGuard;

        // Exactly at the limit — must succeed and invoke delay
        assert_eq!(helper_delay_us(1_000_000, 0, 0, 0, 0), 0);
        assert!(
            clock.delay_calls.get() > 0,
            "accepted delay must invoke clock"
        );

        clock.delay_calls.set(0);

        // One over the limit — must reject WITHOUT calling delay
        assert_eq!(
            helper_delay_us(1_000_001, 0, 0, 0, 0),
            (-1i64) as u64,
            "delay exceeding MAX_DELAY_US must return error"
        );
        assert_eq!(
            clock.delay_calls.get(),
            0,
            "rejected delay must not invoke clock"
        );

        // Far above the limit
        assert_eq!(
            helper_delay_us(u64::MAX, 0, 0, 0, 0),
            (-1i64) as u64,
            "extremely large delay must return error"
        );
        assert_eq!(
            clock.delay_calls.get(),
            0,
            "rejected delay must not invoke clock"
        );
    }

    #[test]
    fn test_helper_set_next_wake() {
        // set_next_wake for resident program succeeds.
        let mut hal = TestHal::new();
        let mut transport = TestTransport::new();
        let mut maps = MapStorage::new(4096);
        let mut sleep = SleepManager::new(300, WakeReason::Scheduled);
        let clock = TestClock(0);
        let identity = default_identity();
        let mut seq = 0u64;
        let mut trace = Vec::new();

        with_test_context(
            &mut hal,
            &mut transport,
            &mut maps,
            &mut sleep,
            &clock,
            &identity,
            &mut seq,
            ProgramClass::Resident,
            &mut trace,
            || {
                let result = helper_set_next_wake(10, 0, 0, 0, 0);
                assert_eq!(result, 0);
            },
        );
        assert_eq!(sleep.effective_sleep_s(), 10);
    }

    #[test]
    fn test_helper_set_next_wake_ephemeral_rejected() {
        // T-N612: Ephemeral cannot call set_next_wake.
        let mut hal = TestHal::new();
        let mut transport = TestTransport::new();
        let mut maps = MapStorage::new(4096);
        let mut sleep = SleepManager::new(300, WakeReason::Scheduled);
        let clock = TestClock(0);
        let identity = default_identity();
        let mut seq = 0u64;
        let mut trace = Vec::new();

        with_test_context(
            &mut hal,
            &mut transport,
            &mut maps,
            &mut sleep,
            &clock,
            &identity,
            &mut seq,
            ProgramClass::Ephemeral,
            &mut trace,
            || {
                let result = helper_set_next_wake(10, 0, 0, 0, 0);
                assert_eq!(result as i64, -1);
            },
        );
        assert_eq!(sleep.effective_sleep_s(), 300);
    }

    #[test]
    fn test_helper_bpf_trace_printk() {
        // T-N613: trace_printk captures string.
        let mut hal = TestHal::new();
        let mut transport = TestTransport::new();
        let mut maps = MapStorage::new(4096);
        let mut sleep = SleepManager::new(60, WakeReason::Scheduled);
        let clock = TestClock(0);
        let identity = default_identity();
        let mut seq = 0u64;
        let mut trace = Vec::new();

        with_test_context(
            &mut hal,
            &mut transport,
            &mut maps,
            &mut sleep,
            &clock,
            &identity,
            &mut seq,
            ProgramClass::Resident,
            &mut trace,
            || {
                let msg = b"hello";
                let result =
                    helper_bpf_trace_printk(msg.as_ptr() as u64, msg.len() as u64, 0, 0, 0);
                assert_eq!(result, 0);
            },
        );
        assert_eq!(trace, vec!["hello".to_string()]);
    }

    #[test]
    fn test_helper_send() {
        // T-N604: send() produces an APP_DATA frame on the transport.
        use sonde_protocol::{decode_frame, open_frame};

        let mut hal = TestHal::new();
        let mut transport = TestTransport::new();
        let mut maps = MapStorage::new(4096);
        let mut sleep = SleepManager::new(60, WakeReason::Scheduled);
        let clock = TestClock(0);
        let identity = default_identity();
        let mut seq = 100u64;
        let mut trace = Vec::new();
        let blob: Vec<u8> = vec![0xAA, 0xBB];

        let aead = crate::node_aead::NodeAead;
        let sha = crate::crypto::SoftwareSha256;

        with_test_context(
            &mut hal,
            &mut transport,
            &mut maps,
            &mut sleep,
            &clock,
            &identity,
            &mut seq,
            ProgramClass::Resident,
            &mut trace,
            || {
                let result = helper_send(blob.as_ptr() as u64, blob.len() as u64, 0, 0, 0);
                assert_eq!(result, 0);
            },
        );

        assert_eq!(seq, 101);
        assert_eq!(transport.outbound.len(), 1);

        // Decode and verify it's a valid AEAD APP_DATA frame
        let decoded = decode_frame(&transport.outbound[0]).unwrap();
        assert_eq!(decoded.header.msg_type, MSG_APP_DATA);
        let plaintext =
            open_frame(&decoded, &identity.psk, &aead, &sha).expect("AEAD decrypt should succeed");
        let msg = NodeMessage::decode(MSG_APP_DATA, &plaintext).unwrap();
        match msg {
            NodeMessage::AppData { blob: received } => assert_eq!(received, vec![0xAA, 0xBB]),
            _ => panic!("expected AppData"),
        }
    }

    #[test]
    fn test_helper_send_recv() {
        // T-N605: send_recv sends APP_DATA and receives APP_DATA_REPLY.
        use sonde_protocol::{encode_frame, FrameHeader, GatewayMessage, MSG_APP_DATA_REPLY};

        let mut hal = TestHal::new();
        let mut transport = TestTransport::new();

        let aead = crate::node_aead::NodeAead;
        let sha = crate::crypto::SoftwareSha256;

        // Pre-queue a valid AEAD reply
        let identity = default_identity();
        let reply_msg = GatewayMessage::AppDataReply {
            blob: vec![0xCC, 0xDD],
        };
        let reply_cbor = reply_msg.encode().unwrap();
        let reply_header = FrameHeader {
            key_hint: identity.key_hint,
            msg_type: MSG_APP_DATA_REPLY,
            nonce: 100, // must match the seq we'll send with
        };
        let reply_frame =
            encode_frame(&reply_header, &reply_cbor, &identity.psk, &aead, &sha).unwrap();
        transport.inbound.push_back(Some(reply_frame));

        let mut maps = MapStorage::new(4096);
        let mut sleep = SleepManager::new(60, WakeReason::Scheduled);
        let clock = TestClock(0);
        let mut seq = 100u64;
        let mut trace = Vec::new();
        let blob = [0x01, 0x02];
        let mut reply_buf = [0u8; 16];

        with_test_context(
            &mut hal,
            &mut transport,
            &mut maps,
            &mut sleep,
            &clock,
            &identity,
            &mut seq,
            ProgramClass::Resident,
            &mut trace,
            || {
                let result = helper_send_recv(
                    blob.as_ptr() as u64,
                    blob.len() as u64,
                    reply_buf.as_mut_ptr() as u64,
                    reply_buf.len() as u64,
                    0,
                );
                assert_eq!(result, 2); // 2 bytes received
            },
        );

        assert_eq!(&reply_buf[..2], &[0xCC, 0xDD]);
        assert_eq!(seq, 101);
    }

    #[test]
    fn test_helper_send_recv_timeout() {
        // T-N606: send_recv with no reply → negative return.
        let mut hal = TestHal::new();
        let mut transport = TestTransport::new();
        transport.inbound.push_back(None); // timeout

        let mut maps = MapStorage::new(4096);
        let mut sleep = SleepManager::new(60, WakeReason::Scheduled);
        let clock = TestClock(0);
        let identity = default_identity();
        let mut seq = 50u64;
        let mut trace = Vec::new();
        let blob = [0x01];
        let mut reply_buf = [0u8; 16];

        with_test_context(
            &mut hal,
            &mut transport,
            &mut maps,
            &mut sleep,
            &clock,
            &identity,
            &mut seq,
            ProgramClass::Resident,
            &mut trace,
            || {
                let result = helper_send_recv(
                    blob.as_ptr() as u64,
                    blob.len() as u64,
                    reply_buf.as_mut_ptr() as u64,
                    reply_buf.len() as u64,
                    0,
                );
                assert_eq!(result as i64, -1);
            },
        );

        // Verify recv was called with the default SEND_RECV_TIMEOUT_MS (ND-0702)
        assert_eq!(transport.recv_timeouts.len(), 1);
        assert_eq!(transport.recv_timeouts[0], SEND_RECV_TIMEOUT_MS);
    }

    // -- MapPtrIndex unit tests ---------------------------------------------

    #[test]
    fn test_map_ptr_index_basic_insert_and_get() {
        let mut idx = MapPtrIndex::new();
        assert!(idx.insert(0x1000, 0).is_ok());
        assert!(idx.insert(0x2000, 1).is_ok());
        assert_eq!(idx.get(0x1000), Some(0));
        assert_eq!(idx.get(0x2000), Some(1));
        assert_eq!(idx.get(0x3000), None);
    }

    #[test]
    fn test_map_ptr_index_overflow_returns_error() {
        let mut idx = MapPtrIndex::new();
        for i in 0..MAX_MAPS {
            assert!(
                idx.insert(0x1000 + i as u64, i).is_ok(),
                "insert {i} should succeed"
            );
        }
        // MAX_MAPS+1 should fail with overflow
        assert_eq!(
            idx.insert(0xFFFF, MAX_MAPS),
            Err(MapPtrInsertError::Overflow)
        );
    }

    #[test]
    fn test_map_ptr_index_duplicate_returns_error() {
        let mut idx = MapPtrIndex::new();
        assert!(idx.insert(0x1000, 0).is_ok());
        assert_eq!(idx.insert(0x1000, 1), Err(MapPtrInsertError::Duplicate),);
        // Original mapping should be unchanged
        assert_eq!(idx.get(0x1000), Some(0));
    }

    #[test]
    fn test_map_ptr_index_get_returns_first_match() {
        let mut idx = MapPtrIndex::new();
        assert!(idx.insert(0x1000, 0).is_ok());
        assert!(idx.insert(0x2000, 1).is_ok());
        assert!(idx.insert(0x3000, 2).is_ok());
        assert_eq!(idx.get(0x2000), Some(1));
    }

    // ===================================================================
    // Gap 8 (ND-0601): Bus helpers available to ephemeral programs
    // ===================================================================

    #[test]
    fn test_bus_helpers_available_to_ephemeral() {
        // ND-0601 AC3: Helpers are available to both resident and ephemeral
        // programs. All existing bus tests use resident programs only.
        let mut hal = TestHal::new();
        hal.i2c_read_data = vec![0xAA, 0xBB];
        hal.gpio_states[3] = 1;
        hal.adc_values[1] = 1024;
        let mut transport = TestTransport::new();
        let mut maps = MapStorage::new(4096);
        let mut sleep = SleepManager::new(60, WakeReason::Scheduled);
        let clock = TestClock(0);
        let identity = default_identity();
        let mut seq = 0u64;
        let mut trace = Vec::new();
        let mut buf = [0u8; 2];

        with_test_context(
            &mut hal,
            &mut transport,
            &mut maps,
            &mut sleep,
            &clock,
            &identity,
            &mut seq,
            ProgramClass::Ephemeral, // ← ephemeral
            &mut trace,
            || {
                // I2C read
                let handle = crate::hal::i2c_handle(0, 0x48);
                let result = helper_i2c_read(
                    handle as u64,
                    buf.as_mut_ptr() as u64,
                    buf.len() as u64,
                    0,
                    0,
                );
                assert_eq!(result, 0, "i2c_read must work for ephemeral programs");

                // GPIO read
                assert_eq!(
                    helper_gpio_read(3, 0, 0, 0, 0),
                    1,
                    "gpio_read must work for ephemeral programs"
                );

                // ADC read
                assert_eq!(
                    helper_adc_read(1, 0, 0, 0, 0),
                    1024,
                    "adc_read must work for ephemeral programs"
                );
            },
        );
        assert_eq!(buf, [0xAA, 0xBB]);
    }

    #[test]
    fn test_spi_transfer_available_to_ephemeral() {
        // ND-0601 AC3: SPI helper also available to ephemeral programs.
        let mut hal = TestHal::new();
        let mut transport = TestTransport::new();
        let mut maps = MapStorage::new(4096);
        let mut sleep = SleepManager::new(60, WakeReason::Scheduled);
        let clock = TestClock(0);
        let identity = default_identity();
        let mut seq = 0u64;
        let mut trace = Vec::new();
        let mut buf = [0xCA, 0xFE];
        let expected: [u8; 2] = [0xCA ^ 0xFF, 0xFE ^ 0xFF];

        with_test_context(
            &mut hal,
            &mut transport,
            &mut maps,
            &mut sleep,
            &clock,
            &identity,
            &mut seq,
            ProgramClass::Ephemeral,
            &mut trace,
            || {
                let handle = crate::hal::spi_handle(0);
                let result = helper_spi_transfer(
                    handle as u64,
                    buf.as_mut_ptr() as u64,
                    buf.len() as u64,
                    0,
                    0,
                );
                assert_eq!(result, 0, "spi_transfer must work for ephemeral programs");
            },
        );
        assert_eq!(buf, expected, "mock XORs buffer, proving in-place write");
    }

    // ===================================================================
    // Gap 9 (ND-0604): delay_us max value enforcement
    // ===================================================================

    #[test]
    fn test_delay_us_max_value_rejected() {
        // ND-0604 AC3: The firmware enforces a maximum delay value.
        // No existing test calls delay_us with an excessive value.
        let mut hal = TestHal::new();
        let mut transport = TestTransport::new();
        let mut maps = MapStorage::new(4096);
        let mut sleep = SleepManager::new(60, WakeReason::Scheduled);
        let clock = TestClock(0);
        let identity = default_identity();
        let mut seq = 0u64;
        let mut trace = Vec::new();

        with_test_context(
            &mut hal,
            &mut transport,
            &mut maps,
            &mut sleep,
            &clock,
            &identity,
            &mut seq,
            ProgramClass::Resident,
            &mut trace,
            || {
                // At the limit (1 second) — should succeed
                assert_eq!(
                    helper_delay_us(MAX_DELAY_US as u64, 0, 0, 0, 0),
                    0,
                    "delay_us at max value must succeed"
                );

                // Exceeds max (1 second + 1 microsecond) — must return error
                assert_eq!(
                    helper_delay_us(MAX_DELAY_US as u64 + 1, 0, 0, 0, 0) as i64,
                    -1,
                    "delay_us exceeding max must return -1"
                );

                // Way over max — must return error
                assert_eq!(
                    helper_delay_us(10_000_000, 0, 0, 0, 0) as i64,
                    -1,
                    "delay_us(10s) must return -1"
                );
            },
        );
    }

    // -- Gap 1: ND-0604 — delay_us maximum enforcement ----------------------

    #[test]
    fn test_helper_delay_us_exceeds_max_rejected() {
        // ND-0604: delay_us() values above the 1,000,000 µs cap must
        // return -1. Without this, an uncapped delay hangs the node.
        let mut hal = TestHal::new();
        let mut transport = TestTransport::new();
        let mut maps = MapStorage::new(4096);
        let mut sleep = SleepManager::new(60, WakeReason::Scheduled);
        let clock = TestClock(0);
        let identity = default_identity();
        let mut seq = 0u64;
        let mut trace = Vec::new();

        with_test_context(
            &mut hal,
            &mut transport,
            &mut maps,
            &mut sleep,
            &clock,
            &identity,
            &mut seq,
            ProgramClass::Resident,
            &mut trace,
            || {
                // Just above the 1-second cap → must fail.
                assert_eq!(helper_delay_us(1_000_001, 0, 0, 0, 0), (-1i64) as u64);
                // Way above → must fail.
                assert_eq!(helper_delay_us(u64::MAX, 0, 0, 0, 0), (-1i64) as u64);
            },
        );
    }

    #[test]
    fn test_helper_delay_us_exact_max_succeeds() {
        // ND-0604 boundary: exactly 1,000,000 µs must succeed.
        let mut hal = TestHal::new();
        let mut transport = TestTransport::new();
        let mut maps = MapStorage::new(4096);
        let mut sleep = SleepManager::new(60, WakeReason::Scheduled);
        let clock = TestClock(0);
        let identity = default_identity();
        let mut seq = 0u64;
        let mut trace = Vec::new();

        with_test_context(
            &mut hal,
            &mut transport,
            &mut maps,
            &mut sleep,
            &clock,
            &identity,
            &mut seq,
            ProgramClass::Resident,
            &mut trace,
            || {
                assert_eq!(helper_delay_us(1_000_000, 0, 0, 0, 0), 0);
            },
        );
    }

    // -- Gap 2: ND-0601 — Bus helpers in ephemeral programs ------------------

    #[test]
    fn test_helper_bus_helpers_ephemeral_succeed() {
        // T-N931: All bus helper calls (I2C, SPI, GPIO, ADC) must succeed
        // from an ephemeral program — behaviour identical to resident.
        let mut hal = TestHal::new();
        hal.i2c_read_data = vec![0x1A, 0x2B];
        hal.gpio_states[5] = 1;
        hal.adc_values[0] = 2048;
        let mut transport = TestTransport::new();
        let mut maps = MapStorage::new(4096);
        let mut sleep = SleepManager::new(60, WakeReason::Scheduled);
        let clock = TestClock(0);
        let identity = default_identity();
        let mut seq = 0u64;
        let mut trace = Vec::new();

        let mut read_buf = [0u8; 2];
        let write_data = [0x55u8; 2];
        let mut wr_buf = [0u8; 2];
        let mut spi_buf = [0xAA, 0xBB];
        let spi_expected: [u8; 2] = [0xAA ^ 0xFF, 0xBB ^ 0xFF];

        with_test_context(
            &mut hal,
            &mut transport,
            &mut maps,
            &mut sleep,
            &clock,
            &identity,
            &mut seq,
            ProgramClass::Ephemeral, // ← ephemeral context
            &mut trace,
            || {
                let handle = crate::hal::i2c_handle(0, 0x48);

                // I2C read — verify data populated
                let r = helper_i2c_read(
                    handle as u64,
                    read_buf.as_mut_ptr() as u64,
                    read_buf.len() as u64,
                    0,
                    0,
                );
                assert_eq!(r, 0, "i2c_read should succeed for ephemeral");
                assert_eq!(read_buf, [0x1A, 0x2B], "i2c_read must populate buffer");

                // I2C write
                let r = helper_i2c_write(
                    handle as u64,
                    write_data.as_ptr() as u64,
                    write_data.len() as u64,
                    0,
                    0,
                );
                assert_eq!(r, 0, "i2c_write should succeed for ephemeral");

                // I2C write-read — verify read buffer populated
                let w = [0x01u8];
                let r = helper_i2c_write_read(
                    handle as u64,
                    w.as_ptr() as u64,
                    w.len() as u64,
                    wr_buf.as_mut_ptr() as u64,
                    wr_buf.len() as u64,
                );
                assert_eq!(r, 0, "i2c_write_read should succeed for ephemeral");
                assert_eq!(wr_buf, [0x1A, 0x2B], "i2c_write_read must fill read buf");

                // SPI transfer — verify in-place mutation
                let spi_h = crate::hal::spi_handle(0) as u64;
                let r = helper_spi_transfer(
                    spi_h,
                    spi_buf.as_mut_ptr() as u64,
                    spi_buf.len() as u64,
                    0,
                    0,
                );
                assert_eq!(r, 0, "spi_transfer should succeed for ephemeral");
                assert_eq!(
                    spi_buf, spi_expected,
                    "spi_transfer: mock XORs buffer, proving HAL received correct slice"
                );

                // GPIO read — verify pin state returned
                let r = helper_gpio_read(5, 0, 0, 0, 0);
                assert_eq!(
                    r as i64, 1,
                    "gpio_read should return pin state for ephemeral"
                );

                // GPIO write — verify pin state changed
                let r = helper_gpio_write(5, 0, 0, 0, 0);
                assert_eq!(r, 0, "gpio_write should succeed for ephemeral");
            },
        );

        // Verify GPIO side effect persisted
        assert_eq!(hal.gpio_states[5], 0, "gpio_write(5, 0) must clear pin");

        // ADC read in a separate context to confirm independence
        let mut trace2 = Vec::new();
        with_test_context(
            &mut hal,
            &mut transport,
            &mut maps,
            &mut sleep,
            &clock,
            &identity,
            &mut seq,
            ProgramClass::Ephemeral,
            &mut trace2,
            || {
                let r = helper_adc_read(0, 0, 0, 0, 0);
                assert_eq!(r as i64, 2048, "adc_read should return value for ephemeral");
            },
        );
    }

    // -- Gap 3: ND-0603 — map_lookup_elem returns NULL on out-of-range key ---

    #[test]
    fn test_helper_map_lookup_out_of_range_key_returns_null() {
        // T-N932: map_lookup_elem on an out-of-range key (>= max_entries)
        // must return 0 (NULL). BPF programs rely on this for NULL checks.
        // Uses key = 4, which is the first out-of-range index for max_entries = 4.
        let mut hal = TestHal::new();
        let mut transport = TestTransport::new();
        let mut maps = MapStorage::new(4096);
        maps.allocate(&[MapDef {
            map_type: 1,
            key_size: 4,
            value_size: 4,
            max_entries: 4,
        }])
        .unwrap();
        let map_ptr = maps.map_pointers()[0];
        let mut sleep = SleepManager::new(60, WakeReason::Scheduled);
        let clock = TestClock(0);
        let identity = default_identity();
        let mut seq = 0u64;
        let mut trace = Vec::new();

        with_test_context(
            &mut hal,
            &mut transport,
            &mut maps,
            &mut sleep,
            &clock,
            &identity,
            &mut seq,
            ProgramClass::Resident,
            &mut trace,
            || {
                // In-range key that was never explicitly written: for
                // BPF_MAP_TYPE_ARRAY, entries are zero-initialized and
                // always present, so lookup must return a non-NULL pointer.
                let in_range_key: u32 = 0;
                let in_range_ptr =
                    helper_map_lookup_elem(map_ptr, &in_range_key as *const u32 as u64, 0, 0, 0);
                assert_ne!(
                    in_range_ptr, 0,
                    "lookup of in-range never-updated key must return non-NULL (zero-initialized)"
                );

                // Key 4 is the first out-of-range index (max_entries = 4,
                // valid indices are 0..3) — lookup must return NULL (0).
                let key: u32 = 4;
                let ptr = helper_map_lookup_elem(map_ptr, &key as *const u32 as u64, 0, 0, 0);
                assert_eq!(ptr, 0, "lookup of out-of-range key must return NULL");
            },
        );
    }

    // -- Gap 6: set_next_wake min() semantics via helper dispatch -------------

    #[test]
    fn test_helper_set_next_wake_clamped_to_base_interval() {
        // bpf-env §6.4: set_next_wake cannot extend beyond the
        // gateway-configured base interval. min(600, 300) == 300.
        let mut hal = TestHal::new();
        let mut transport = TestTransport::new();
        let mut maps = MapStorage::new(4096);
        let mut sleep = SleepManager::new(300, WakeReason::Scheduled);
        let clock = TestClock(0);
        let identity = default_identity();
        let mut seq = 0u64;
        let mut trace = Vec::new();

        with_test_context(
            &mut hal,
            &mut transport,
            &mut maps,
            &mut sleep,
            &clock,
            &identity,
            &mut seq,
            ProgramClass::Resident,
            &mut trace,
            || {
                // Request 600 s when base is 300 s — helper should succeed
                // but effective sleep must be clamped to base interval.
                let result = helper_set_next_wake(600, 0, 0, 0, 0);
                assert_eq!(result, 0, "set_next_wake should return success");
            },
        );
        assert_eq!(
            sleep.effective_sleep_s(),
            300,
            "effective sleep must be min(600, 300) = 300"
        );
        assert!(
            !sleep.will_wake_early(),
            "requesting longer than base should not count as early wake"
        );
    }

    // -- Log-level tests (ND-1010 / T-N1015) ----------------------------------

    #[cfg(debug_assertions)]
    use crate::test_log_capture;

    // Skipped in release builds — `debug!()` is stripped at compile time by
    // `release_max_level_warn`.
    #[cfg(debug_assertions)]
    #[test]
    fn test_helper_i2c_read_emits_debug_log() {
        // ND-1010 / T-N1015: I/O helpers emit DEBUG-level logs.
        test_log_capture::init();
        test_log_capture::drain_log_records(); // discard prior records

        let mut hal = TestHal::new();
        let mut transport = TestTransport::new();
        let mut maps = MapStorage::new(4096);
        let mut sleep = SleepManager::new(60, WakeReason::Scheduled);
        let clock = TestClock(0);
        let identity = default_identity();
        let mut seq = 0u64;
        let mut trace = Vec::new();
        let mut buf = [0u8; 2];

        with_test_context(
            &mut hal,
            &mut transport,
            &mut maps,
            &mut sleep,
            &clock,
            &identity,
            &mut seq,
            ProgramClass::Resident,
            &mut trace,
            || {
                let handle = crate::hal::i2c_handle(0, 0x48);
                helper_i2c_read(
                    handle as u64,
                    buf.as_mut_ptr() as u64,
                    buf.len() as u64,
                    0,
                    0,
                );
            },
        );

        let records = test_log_capture::drain_log_records();
        assert!(
            records
                .iter()
                .any(|(level, msg)| *level == log::Level::Debug
                    && msg.contains("bpf helper i2c_read")
                    && msg.contains("result=")),
            "expected DEBUG log for i2c_read helper, got: {:?}",
            records
        );
    }

    #[cfg(debug_assertions)]
    #[test]
    fn test_helper_gpio_read_emits_debug_log() {
        // ND-1010 / T-N1015: gpio_read emits a DEBUG log with result.
        test_log_capture::init();
        test_log_capture::drain_log_records();

        let mut hal = TestHal::new();
        hal.gpio_states[3] = 1;
        let mut transport = TestTransport::new();
        let mut maps = MapStorage::new(4096);
        let mut sleep = SleepManager::new(60, WakeReason::Scheduled);
        let clock = TestClock(0);
        let identity = default_identity();
        let mut seq = 0u64;
        let mut trace = Vec::new();

        with_test_context(
            &mut hal,
            &mut transport,
            &mut maps,
            &mut sleep,
            &clock,
            &identity,
            &mut seq,
            ProgramClass::Resident,
            &mut trace,
            || {
                helper_gpio_read(3, 0, 0, 0, 0);
            },
        );

        let records = test_log_capture::drain_log_records();
        assert!(
            records
                .iter()
                .any(|(level, msg)| *level == log::Level::Debug
                    && msg.contains("bpf helper gpio_read")
                    && msg.contains("result=")),
            "expected DEBUG log for gpio_read helper, got: {:?}",
            records
        );
    }

    #[cfg(debug_assertions)]
    #[test]
    fn test_non_io_helper_does_not_emit_debug_log() {
        // ND-1010 AC #2: non-I/O helpers must NOT emit DEBUG logs.
        test_log_capture::init();
        test_log_capture::drain_log_records();

        let mut hal = TestHal::new();
        let mut transport = TestTransport::new();
        let mut maps = MapStorage::new(4096);
        let mut sleep = SleepManager::new(60, WakeReason::Scheduled);
        let clock = TestClock(1_000);
        let identity = default_identity();
        let mut seq = 0u64;
        let mut trace = Vec::new();

        with_test_context(
            &mut hal,
            &mut transport,
            &mut maps,
            &mut sleep,
            &clock,
            &identity,
            &mut seq,
            ProgramClass::Resident,
            &mut trace,
            || {
                // Call several non-I/O helpers.
                helper_get_time(0, 0, 0, 0, 0);
                helper_get_battery_mv(0, 0, 0, 0, 0);
                helper_delay_us(10, 0, 0, 0, 0);
            },
        );

        let records = test_log_capture::drain_log_records();
        let has_debug_helper_log = records
            .iter()
            .any(|(level, msg)| *level == log::Level::Debug && msg.contains("bpf helper"));
        assert!(
            !has_debug_helper_log,
            "non-I/O helpers must not emit DEBUG 'bpf helper' logs, got: {:?}",
            records
        );
    }

    #[test]
    fn test_helper_send_async_success() {
        let mut hal = TestHal::new();
        let mut transport = TestTransport::new();
        let mut maps = MapStorage::new(4096);
        let mut sleep = SleepManager::new(60, WakeReason::Scheduled);
        let clock = TestClock(0);
        let identity = default_identity();
        let mut seq = 0u64;
        let mut trace = Vec::new();
        let data = [0x42u8; 10];

        with_test_context(
            &mut hal,
            &mut transport,
            &mut maps,
            &mut sleep,
            &clock,
            &identity,
            &mut seq,
            ProgramClass::Resident,
            &mut trace,
            || {
                let result = helper_send_async(data.as_ptr() as u64, data.len() as u64, 0, 0, 0);
                assert_eq!(result, 0, "send_async should succeed");
            },
        );
        // No outbound frames — data is queued, not sent immediately.
        assert!(transport.outbound.is_empty());
    }

    /// T-N632: send_async from ephemeral program (ND-0602 AC6).
    #[test]
    fn test_helper_send_async_ephemeral() {
        let mut hal = TestHal::new();
        let mut transport = TestTransport::new();
        let mut maps = MapStorage::new(4096);
        let mut sleep = SleepManager::new(60, WakeReason::Scheduled);
        let clock = TestClock(0);
        let identity = default_identity();
        let mut seq = 0u64;
        let mut trace = Vec::new();
        let data = [0x42u8; 10];

        with_test_context(
            &mut hal,
            &mut transport,
            &mut maps,
            &mut sleep,
            &clock,
            &identity,
            &mut seq,
            ProgramClass::Ephemeral,
            &mut trace,
            || {
                let result = helper_send_async(data.as_ptr() as u64, data.len() as u64, 0, 0, 0);
                assert_eq!(
                    result, 0,
                    "send_async should succeed from ephemeral program"
                );
            },
        );
        // No outbound frames — data is queued, not sent immediately.
        assert!(transport.outbound.is_empty());
    }

    #[test]
    fn test_helper_send_async_queue_full() {
        let mut hal = TestHal::new();
        let mut transport = TestTransport::new();
        let mut maps = MapStorage::new(4096);
        let mut sleep = SleepManager::new(60, WakeReason::Scheduled);
        let clock = TestClock(0);
        let identity = default_identity();
        let mut seq = 0u64;
        let mut trace = Vec::new();

        let mut queue = AsyncQueue::new();
        // Pre-fill the queue to capacity.
        for _ in 0..10 {
            let _ = queue.push(vec![0x42]);
        }

        let aead = crate::node_aead::NodeAead;
        let sha = crate::crypto::SoftwareSha256;
        unsafe {
            install(
                &mut hal as *mut TestHal as *mut dyn Hal,
                &mut transport as *mut TestTransport as *mut dyn Transport,
                &mut maps as *mut MapStorage,
                &mut sleep as *mut SleepManager,
                &clock as *const TestClock as *const dyn Clock,
                &identity as *const NodeIdentity,
                &mut seq as *mut u64,
                ProgramClass::Resident,
                &mut trace as *mut Vec<String>,
                &mut queue as *mut AsyncQueue,
                1_710_000_000_000,
                100,
                3300,
                &aead as *const _ as *const dyn sonde_protocol::AeadProvider,
                &sha as *const _ as *const dyn sonde_protocol::Sha256Provider,
            );
        }

        let _guard = DispatchGuard;

        let data = [0x42u8; 4];
        let result = helper_send_async(data.as_ptr() as u64, data.len() as u64, 0, 0, 0);
        assert_eq!(result as i64, -1, "queue full should return -1");
    }

    #[test]
    fn test_veml7700_sensor_program_queues_payload_with_documented_registers() {
        let image = compile_veml7700_program_image();
        let handle = crate::hal::i2c_handle(0, 0x10);
        let als_counts = 0x1234u16;
        let white_counts = 0x5678u16;
        let expected_lux_ml = u32::from(als_counts) * 576u32 / 10u32;

        let mut hal = TestHal::new();
        hal.strict_i2c = true;
        hal.expect_i2c_write(handle, &[0x00, 0x00, 0x00], 0);
        hal.expect_i2c_write_read(handle, &[0x04], &als_counts.to_le_bytes(), 0);
        hal.expect_i2c_write_read(handle, &[0x05], &white_counts.to_le_bytes(), 0);
        hal.expect_i2c_write(handle, &[0x00, 0x01, 0x00], 0);

        let mut transport = TestTransport::new();
        let mut maps = MapStorage::new(4096);
        maps.allocate(&image.maps).unwrap();
        maps.apply_initial_data(&image.map_initial_data);

        let clock = RecordingClock::new(0);
        let identity = default_identity();
        let mut seq = 0u64;
        let mut trace = Vec::new();
        let mut queue = AsyncQueue::new();
        let mut sleep = SleepManager::new(60, WakeReason::Scheduled);
        let mut interpreter = crate::sonde_bpf_adapter::SondeBpfInterpreter::new();
        register_all(&mut interpreter).unwrap();
        interpreter
            .load(&image.bytecode, maps.map_pointers(), &image.maps)
            .unwrap();

        with_recording_clock_test_context_and_queue(
            &mut hal,
            &mut transport,
            &mut maps,
            &mut sleep,
            &clock,
            &identity,
            &mut seq,
            ProgramClass::Resident,
            &mut trace,
            &mut queue,
            || {
                let ctx = crate::bpf_helpers::SondeContext {
                    timestamp: 0x0102_0304_0506_0708,
                    battery_mv: 3300,
                    firmware_abi_version: crate::FIRMWARE_ABI_VERSION as u16,
                    wake_reason: 0,
                    _padding: [0; 3],
                    data_start: 0,
                    data_end: 0,
                };
                let result = interpreter
                    .execute(
                        (&ctx as *const crate::bpf_helpers::SondeContext) as u64,
                        100_000,
                    )
                    .unwrap();
                assert_eq!(result, 0);
            },
        );

        assert!(
            hal.i2c_expectations.is_empty(),
            "all expected VEML7700 transactions should be consumed"
        );
        assert_eq!(
            hal.i2c_events,
            vec![
                I2cEvent::Write {
                    handle,
                    data: vec![0x00, 0x00, 0x00],
                },
                I2cEvent::WriteRead {
                    handle,
                    write: vec![0x04],
                },
                I2cEvent::WriteRead {
                    handle,
                    write: vec![0x05],
                },
                I2cEvent::Write {
                    handle,
                    data: vec![0x00, 0x01, 0x00],
                },
            ]
        );
        assert_eq!(*clock.delays_us.borrow(), vec![120_000]);
        assert!(transport.outbound.is_empty());
        assert_eq!(queue.len(), 1);

        let queued = queue.drain();
        assert_eq!(queued.len(), 1);
        let payload = &queued[0];
        assert_eq!(payload.len(), 18);
        assert_eq!(&payload[0..8], &0x0102_0304_0506_0708u64.to_le_bytes());
        assert_eq!(u16::from_le_bytes([payload[8], payload[9]]), 0x0000);
        assert_eq!(u16::from_le_bytes([payload[10], payload[11]]), als_counts);
        assert_eq!(u16::from_le_bytes([payload[12], payload[13]]), white_counts);
        assert_eq!(
            u32::from_le_bytes([payload[14], payload[15], payload[16], payload[17]]),
            expected_lux_ml
        );
    }

    #[test]
    fn test_veml7700_sensor_program_suppresses_payload_on_i2c_error() {
        let image = compile_veml7700_program_image();
        let handle = crate::hal::i2c_handle(0, 0x10);

        let mut hal = TestHal::new();
        hal.strict_i2c = true;
        hal.expect_i2c_write(handle, &[0x00, 0x00, 0x00], 0);
        hal.expect_i2c_write_read(handle, &[0x04], &[], -1);

        let mut transport = TestTransport::new();
        let mut maps = MapStorage::new(4096);
        maps.allocate(&image.maps).unwrap();
        maps.apply_initial_data(&image.map_initial_data);

        let clock = RecordingClock::new(0);
        let identity = default_identity();
        let mut seq = 0u64;
        let mut trace = Vec::new();
        let mut queue = AsyncQueue::new();
        let mut sleep = SleepManager::new(60, WakeReason::Scheduled);
        let mut interpreter = crate::sonde_bpf_adapter::SondeBpfInterpreter::new();
        register_all(&mut interpreter).unwrap();
        interpreter
            .load(&image.bytecode, maps.map_pointers(), &image.maps)
            .unwrap();

        with_recording_clock_test_context_and_queue(
            &mut hal,
            &mut transport,
            &mut maps,
            &mut sleep,
            &clock,
            &identity,
            &mut seq,
            ProgramClass::Resident,
            &mut trace,
            &mut queue,
            || {
                let ctx = crate::bpf_helpers::SondeContext {
                    timestamp: 1234,
                    battery_mv: 3300,
                    firmware_abi_version: crate::FIRMWARE_ABI_VERSION as u16,
                    wake_reason: 0,
                    _padding: [0; 3],
                    data_start: 0,
                    data_end: 0,
                };
                let result = interpreter
                    .execute(
                        (&ctx as *const crate::bpf_helpers::SondeContext) as u64,
                        100_000,
                    )
                    .unwrap();
                assert_eq!(result, 0);
            },
        );

        assert!(
            hal.i2c_expectations.is_empty(),
            "configured failure path should be exercised"
        );
        assert_eq!(
            hal.i2c_events,
            vec![
                I2cEvent::Write {
                    handle,
                    data: vec![0x00, 0x00, 0x00],
                },
                I2cEvent::WriteRead {
                    handle,
                    write: vec![0x04],
                },
            ]
        );
        assert_eq!(*clock.delays_us.borrow(), vec![120_000]);
        assert!(transport.outbound.is_empty());
        assert!(queue.is_empty());
    }

    #[test]
    fn test_veml7700_sensor_program_suppresses_payload_on_i2c_write_error() {
        let image = compile_veml7700_program_image();
        let handle = crate::hal::i2c_handle(0, 0x10);

        let mut hal = TestHal::new();
        hal.strict_i2c = true;
        hal.expect_i2c_write(handle, &[0x00, 0x00, 0x00], -1);

        let mut transport = TestTransport::new();
        let mut maps = MapStorage::new(4096);
        maps.allocate(&image.maps).unwrap();
        maps.apply_initial_data(&image.map_initial_data);

        let clock = RecordingClock::new(0);
        let identity = default_identity();
        let mut seq = 0u64;
        let mut trace = Vec::new();
        let mut queue = AsyncQueue::new();
        let mut sleep = SleepManager::new(60, WakeReason::Scheduled);
        let mut interpreter = crate::sonde_bpf_adapter::SondeBpfInterpreter::new();
        register_all(&mut interpreter).unwrap();
        interpreter
            .load(&image.bytecode, maps.map_pointers(), &image.maps)
            .unwrap();

        with_recording_clock_test_context_and_queue(
            &mut hal,
            &mut transport,
            &mut maps,
            &mut sleep,
            &clock,
            &identity,
            &mut seq,
            ProgramClass::Resident,
            &mut trace,
            &mut queue,
            || {
                let ctx = crate::bpf_helpers::SondeContext {
                    timestamp: 1234,
                    battery_mv: 3300,
                    firmware_abi_version: crate::FIRMWARE_ABI_VERSION as u16,
                    wake_reason: 0,
                    _padding: [0; 3],
                    data_start: 0,
                    data_end: 0,
                };
                let result = interpreter
                    .execute(
                        (&ctx as *const crate::bpf_helpers::SondeContext) as u64,
                        100_000,
                    )
                    .unwrap();
                assert_eq!(result, 0);
            },
        );

        assert!(hal.i2c_expectations.is_empty());
        assert_eq!(
            hal.i2c_events,
            vec![I2cEvent::Write {
                handle,
                data: vec![0x00, 0x00, 0x00],
            }]
        );
        assert!(clock.delays_us.borrow().is_empty());
        assert!(transport.outbound.is_empty());
        assert!(queue.is_empty());
    }

    #[test]
    fn test_veml7700_sensor_program_does_not_persist_conf_on_i2c_error() {
        let image = compile_veml7700_program_image();
        let handle = crate::hal::i2c_handle(0, 0x10);

        let mut hal = TestHal::new();
        hal.strict_i2c = true;
        hal.expect_i2c_write(handle, &[0x00, 0xC0, 0x08], 0);
        hal.expect_i2c_write_read(handle, &[0x04], &[], -1);

        let mut transport = TestTransport::new();
        let mut maps = MapStorage::new(4096);
        maps.allocate(&image.maps).unwrap();
        maps.apply_initial_data(&image.map_initial_data);
        maps.get_mut(0)
            .unwrap()
            .update(0, &encode_veml7700_state([150, 180, 190], 0x0000, 3, 0))
            .unwrap();

        let clock = RecordingClock::new(0);
        let identity = default_identity();
        let mut seq = 0u64;
        let mut trace = Vec::new();
        let mut queue = AsyncQueue::new();
        let mut sleep = SleepManager::new(60, WakeReason::Scheduled);
        let mut interpreter = crate::sonde_bpf_adapter::SondeBpfInterpreter::new();
        register_all(&mut interpreter).unwrap();
        interpreter
            .load(&image.bytecode, maps.map_pointers(), &image.maps)
            .unwrap();

        with_recording_clock_test_context_and_queue(
            &mut hal,
            &mut transport,
            &mut maps,
            &mut sleep,
            &clock,
            &identity,
            &mut seq,
            ProgramClass::Resident,
            &mut trace,
            &mut queue,
            || {
                let ctx = crate::bpf_helpers::SondeContext {
                    timestamp: 1234,
                    battery_mv: 3300,
                    firmware_abi_version: crate::FIRMWARE_ABI_VERSION as u16,
                    wake_reason: 0,
                    _padding: [0; 3],
                    data_start: 0,
                    data_end: 0,
                };
                let result = interpreter
                    .execute(
                        (&ctx as *const crate::bpf_helpers::SondeContext) as u64,
                        100_000,
                    )
                    .unwrap();
                assert_eq!(result, 0);
            },
        );

        assert!(hal.i2c_expectations.is_empty());
        assert_eq!(
            hal.i2c_events,
            vec![
                I2cEvent::Write {
                    handle,
                    data: vec![0x00, 0xC0, 0x08],
                },
                I2cEvent::WriteRead {
                    handle,
                    write: vec![0x04],
                },
            ]
        );
        assert_eq!(*clock.delays_us.borrow(), vec![900_000]);
        assert!(transport.outbound.is_empty());
        assert!(queue.is_empty());

        let state = maps.get(0).unwrap().lookup(0).unwrap();
        assert_eq!(state.len(), 16);
        assert_eq!(u16::from_le_bytes([state[12], state[13]]), 0x0000);
        assert_eq!(state[14], 3);
        assert_eq!(state[15], 0);
    }

    #[test]
    fn test_veml7700_sensor_program_switches_to_lowlight_after_low_reading() {
        let image = compile_veml7700_program_image();
        let handle = crate::hal::i2c_handle(0, 0x10);
        let first_als_counts = 1u16;
        let first_white_counts = 2u16;
        let second_als_counts = 20u16;
        let second_white_counts = 40u16;

        let mut hal = TestHal::new();
        hal.strict_i2c = true;
        hal.expect_i2c_write(handle, &[0x00, 0x00, 0x00], 0);
        hal.expect_i2c_write_read(handle, &[0x04], &first_als_counts.to_le_bytes(), 0);
        hal.expect_i2c_write_read(handle, &[0x05], &first_white_counts.to_le_bytes(), 0);
        hal.expect_i2c_write(handle, &[0x00, 0x01, 0x00], 0);
        hal.expect_i2c_write(handle, &[0x00, 0xC0, 0x08], 0);
        hal.expect_i2c_write_read(handle, &[0x04], &second_als_counts.to_le_bytes(), 0);
        hal.expect_i2c_write_read(handle, &[0x05], &second_white_counts.to_le_bytes(), 0);
        hal.expect_i2c_write(handle, &[0x00, 0x01, 0x00], 0);

        let mut transport = TestTransport::new();
        let mut maps = MapStorage::new(4096);
        maps.allocate(&image.maps).unwrap();
        maps.apply_initial_data(&image.map_initial_data);

        let clock = RecordingClock::new(0);
        let identity = default_identity();
        let mut seq = 0u64;
        let mut trace = Vec::new();
        let mut queue = AsyncQueue::new();
        let mut sleep = SleepManager::new(60, WakeReason::Scheduled);
        let mut interpreter = crate::sonde_bpf_adapter::SondeBpfInterpreter::new();
        register_all(&mut interpreter).unwrap();
        interpreter
            .load(&image.bytecode, maps.map_pointers(), &image.maps)
            .unwrap();

        for timestamp in [111u64, 222u64] {
            with_recording_clock_test_context_and_queue(
                &mut hal,
                &mut transport,
                &mut maps,
                &mut sleep,
                &clock,
                &identity,
                &mut seq,
                ProgramClass::Resident,
                &mut trace,
                &mut queue,
                || {
                    let ctx = crate::bpf_helpers::SondeContext {
                        timestamp,
                        battery_mv: 3300,
                        firmware_abi_version: crate::FIRMWARE_ABI_VERSION as u16,
                        wake_reason: 0,
                        _padding: [0; 3],
                        data_start: 0,
                        data_end: 0,
                    };
                    let result = interpreter
                        .execute(
                            (&ctx as *const crate::bpf_helpers::SondeContext) as u64,
                            100_000,
                        )
                        .unwrap();
                    assert_eq!(result, 0);
                },
            );
        }

        assert!(hal.i2c_expectations.is_empty());
        assert_eq!(*clock.delays_us.borrow(), vec![120_000, 900_000]);
        assert!(transport.outbound.is_empty());

        let queued = queue.drain();
        assert_eq!(queued.len(), 2);
        assert_eq!(u16::from_le_bytes([queued[0][8], queued[0][9]]), 0x0000);
        assert_eq!(
            u32::from_le_bytes([queued[0][14], queued[0][15], queued[0][16], queued[0][17]]),
            57
        );
        assert_eq!(u16::from_le_bytes([queued[1][8], queued[1][9]]), 0x08C0);
        assert_eq!(
            u32::from_le_bytes([queued[1][14], queued[1][15], queued[1][16], queued[1][17]]),
            72
        );
    }

    #[test]
    fn test_veml7700_sensor_program_switches_back_to_default_after_bright_history() {
        let image = compile_veml7700_program_image();
        let handle = crate::hal::i2c_handle(0, 0x10);
        let als_counts = 50u16;
        let white_counts = 60u16;
        let expected_lux_ml = u32::from(als_counts) * 576u32 / 10u32;

        let mut hal = TestHal::new();
        hal.strict_i2c = true;
        hal.expect_i2c_write(handle, &[0x00, 0x00, 0x00], 0);
        hal.expect_i2c_write_read(handle, &[0x04], &als_counts.to_le_bytes(), 0);
        hal.expect_i2c_write_read(handle, &[0x05], &white_counts.to_le_bytes(), 0);
        hal.expect_i2c_write(handle, &[0x00, 0x01, 0x00], 0);

        let mut transport = TestTransport::new();
        let mut maps = MapStorage::new(4096);
        maps.allocate(&image.maps).unwrap();
        maps.apply_initial_data(&image.map_initial_data);
        maps.get_mut(0)
            .unwrap()
            .update(0, &encode_veml7700_state([1200, 1300, 1400], 0x08C0, 3, 0))
            .unwrap();

        let clock = RecordingClock::new(0);
        let identity = default_identity();
        let mut seq = 0u64;
        let mut trace = Vec::new();
        let mut queue = AsyncQueue::new();
        let mut sleep = SleepManager::new(60, WakeReason::Scheduled);
        let mut interpreter = crate::sonde_bpf_adapter::SondeBpfInterpreter::new();
        register_all(&mut interpreter).unwrap();
        interpreter
            .load(&image.bytecode, maps.map_pointers(), &image.maps)
            .unwrap();

        with_recording_clock_test_context_and_queue(
            &mut hal,
            &mut transport,
            &mut maps,
            &mut sleep,
            &clock,
            &identity,
            &mut seq,
            ProgramClass::Resident,
            &mut trace,
            &mut queue,
            || {
                let ctx = crate::bpf_helpers::SondeContext {
                    timestamp: 333,
                    battery_mv: 3300,
                    firmware_abi_version: crate::FIRMWARE_ABI_VERSION as u16,
                    wake_reason: 0,
                    _padding: [0; 3],
                    data_start: 0,
                    data_end: 0,
                };
                let result = interpreter
                    .execute(
                        (&ctx as *const crate::bpf_helpers::SondeContext) as u64,
                        100_000,
                    )
                    .unwrap();
                assert_eq!(result, 0);
            },
        );

        assert!(hal.i2c_expectations.is_empty());
        assert_eq!(
            hal.i2c_events,
            vec![
                I2cEvent::Write {
                    handle,
                    data: vec![0x00, 0x00, 0x00],
                },
                I2cEvent::WriteRead {
                    handle,
                    write: vec![0x04],
                },
                I2cEvent::WriteRead {
                    handle,
                    write: vec![0x05],
                },
                I2cEvent::Write {
                    handle,
                    data: vec![0x00, 0x01, 0x00],
                },
            ]
        );
        assert_eq!(*clock.delays_us.borrow(), vec![120_000]);
        assert!(transport.outbound.is_empty());

        let queued = queue.drain();
        assert_eq!(queued.len(), 1);
        assert_eq!(u16::from_le_bytes([queued[0][8], queued[0][9]]), 0x0000);
        assert_eq!(
            u32::from_le_bytes([queued[0][14], queued[0][15], queued[0][16], queued[0][17]]),
            expected_lux_ml
        );
    }

    #[test]
    fn test_helper_send_async_oversized() {
        let mut hal = TestHal::new();
        let mut transport = TestTransport::new();
        let mut maps = MapStorage::new(4096);
        let mut sleep = SleepManager::new(60, WakeReason::Scheduled);
        let clock = TestClock(0);
        let identity = default_identity();
        let mut seq = 0u64;
        let mut trace = Vec::new();

        let big = vec![0x42u8; sonde_protocol::MAX_APP_DATA_BLOB_SIZE + 1];

        with_test_context(
            &mut hal,
            &mut transport,
            &mut maps,
            &mut sleep,
            &clock,
            &identity,
            &mut seq,
            ProgramClass::Resident,
            &mut trace,
            || {
                let result = helper_send_async(big.as_ptr() as u64, big.len() as u64, 0, 0, 0);
                assert_eq!(result as i64, -2, "oversized blob should return -2");
            },
        );
    }

    #[test]
    fn test_helper_send_async_null_ptr() {
        let mut hal = TestHal::new();
        let mut transport = TestTransport::new();
        let mut maps = MapStorage::new(4096);
        let mut sleep = SleepManager::new(60, WakeReason::Scheduled);
        let clock = TestClock(0);
        let identity = default_identity();
        let mut seq = 0u64;
        let mut trace = Vec::new();

        with_test_context(
            &mut hal,
            &mut transport,
            &mut maps,
            &mut sleep,
            &clock,
            &identity,
            &mut seq,
            ProgramClass::Resident,
            &mut trace,
            || {
                let result = helper_send_async(0, 10, 0, 0, 0);
                assert_eq!(result as i64, -2, "null ptr should return -2");
            },
        );
    }
}
