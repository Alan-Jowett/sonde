// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Decoder BPF execution engine (GW-1903, GW-1904).
//!
//! Executes decoder BPF programs on the gateway to enrich APP_DATA messages
//! with named sensor readings before forwarding to handlers and connectors.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;

use sonde_bpf::interpreter::{self, BpfError, HelperDescriptor, HelperReturn, MapRegion};
use sonde_protocol::ProgramImage;
use tracing::warn;

/// Maximum number of readings per decoder execution (GW-1904).
const MAX_READINGS: usize = 32;

/// Maximum name length in bytes for a single reading (GW-1904).
const MAX_NAME_LEN: usize = 64;

/// Instruction budget for decoder programs — same as ephemeral.
const DECODER_INSTRUCTION_BUDGET: u64 = 100_000;

/// Maximum total map backing memory per decoder execution (64 KiB).
const MAX_DECODER_MAP_MEMORY: usize = 64 * 1024;

/// Maximum number of maps a decoder program may declare.
const MAX_DECODER_MAPS: usize = 16;

/// Errors from decoder execution.
#[derive(Debug)]
pub enum DecoderError {
    /// Failed to decode the decoder image CBOR.
    ImageDecodeError(String),
    /// BPF interpreter error.
    ExecutionError(BpfError),
}

impl fmt::Display for DecoderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecoderError::ImageDecodeError(msg) => write!(f, "decoder image decode error: {msg}"),
            DecoderError::ExecutionError(e) => write!(f, "decoder execution error: {e}"),
        }
    }
}

impl std::error::Error for DecoderError {}

// ── Thread-local readings collection ────────────────────────────────────

/// Metadata for a single map during decoder execution.
struct MapInfo {
    /// The `relocated_ptr` value that identifies this map in BPF registers.
    relocated_ptr: u64,
    /// Start address of the map backing storage.
    data_start: u64,
    /// Size of each value in bytes.
    value_size: u32,
    /// Number of entries (array length).
    max_entries: u32,
    /// True if this map is `.rodata`-backed (writes rejected at runtime).
    read_only: bool,
}

/// State collected during a single decoder execution.
struct DecoderState {
    readings: BTreeMap<String, i64>,
    count: usize,
    /// Map metadata for helper implementations.
    map_infos: Vec<MapInfo>,
}

thread_local! {
    static DECODER_STATE: RefCell<Option<DecoderState>> = const { RefCell::new(None) };
}

/// RAII guard that installs and clears thread-local decoder state.
struct DecoderStateGuard;

impl DecoderStateGuard {
    fn install(map_infos: Vec<MapInfo>) -> Self {
        DECODER_STATE.with(|cell| {
            *cell.borrow_mut() = Some(DecoderState {
                readings: BTreeMap::new(),
                count: 0,
                map_infos,
            });
        });
        DecoderStateGuard
    }

    fn take_readings(&self) -> BTreeMap<String, i64> {
        DECODER_STATE.with(|cell| {
            cell.borrow()
                .as_ref()
                .map(|s| s.readings.clone())
                .unwrap_or_default()
        })
    }
}

impl Drop for DecoderStateGuard {
    fn drop(&mut self) {
        DECODER_STATE.with(|cell| {
            *cell.borrow_mut() = None;
        });
    }
}

// ── BPF helper implementations ─────────────────────────────────────────

/// Find a map by its `relocated_ptr` value.
fn find_map(state: &DecoderState, relocated_ptr: u64) -> Option<&MapInfo> {
    state
        .map_infos
        .iter()
        .find(|m| m.relocated_ptr == relocated_ptr)
}

/// Helper 18: emit_reading(name_ptr, name_len, value_i64, _, _) -> 0 | -1 | -2
///
/// The name_ptr and name_len refer to the BPF program's virtual address space.
///
/// The sonde-bpf helper calling convention passes raw register values.
/// The name bytes are in BPF memory at the address pointed to by r1.
///
/// # Safety invariant
///
/// Pointer arguments are valid only when the decoder image was verified by
/// Prevail at ingestion time (see `execute_decoder` doc). The Prevail
/// verifier ensures pointer arguments satisfy type constraints
/// (PtrToReadableMem + ConstSize). The interpreter does NOT validate
/// helper pointer arguments at dispatch — it only validates returned
/// map pointers (MapValueOrNull). Runtime helper-argument validation
/// in sonde-bpf is a future hardening item (see issue backlog).
fn helper_emit_reading(r1: u64, r2: u64, r3: u64, _r4: u64, _r5: u64) -> u64 {
    let name_ptr = r1 as *const u8;
    let name_len = r2 as usize;
    let value = r3 as i64;

    if name_len > MAX_NAME_LEN {
        return (-1i64) as u64;
    }

    DECODER_STATE.with(|cell| {
        let mut state = cell.borrow_mut();
        let state = match state.as_mut() {
            Some(s) => s,
            None => return (-1i64) as u64,
        };

        // Check if adding a new unique name would exceed the limit.
        // Last-write-wins: updating an existing name doesn't count as new.
        let name = if name_len > 0 && !name_ptr.is_null() {
            // SAFETY: Relies on Prevail verification at ingestion time — the
            // verifier guarantees r1 is PtrToReadableMem of length r2 within
            // a valid BPF region (context or stack). See `execute_decoder` doc.
            let bytes = unsafe { std::slice::from_raw_parts(name_ptr, name_len) };
            match std::str::from_utf8(bytes) {
                Ok(s) => s.to_owned(),
                Err(_) => return (-1i64) as u64,
            }
        } else {
            return (-1i64) as u64;
        };

        // If this is a new name (not overwrite), check the count limit.
        if !state.readings.contains_key(&name) {
            if state.count >= MAX_READINGS {
                warn!("decoder emit_reading overflow: limit of {MAX_READINGS} readings exceeded");
                return (-2i64) as u64;
            }
            state.count += 1;
        }

        state.readings.insert(name, value);
        0u64
    })
}

/// Helper 10: map_lookup_elem(map_fd, key_ptr) -> value_ptr or null
///
/// For array maps: key is a u32 index, returns pointer to the value at
/// `data_start + index * value_size`. Returns 0 (null) if out of bounds.
fn helper_map_lookup(r1: u64, r2: u64, _r3: u64, _r4: u64, _r5: u64) -> u64 {
    let map_fd = r1;
    let key_ptr = r2 as *const u8;

    if key_ptr.is_null() {
        return 0;
    }

    DECODER_STATE.with(|cell| {
        let state = cell.borrow();
        let state = match state.as_ref() {
            Some(s) => s,
            None => return 0,
        };
        let map = match find_map(state, map_fd) {
            Some(m) => m,
            None => return 0,
        };

        // Read key as u32 index (may be unaligned).
        // SAFETY: key_ptr points into valid BPF memory (Prevail verifier
        // ensures PtrToMapKey constraints, interpreter enforces bounds).
        let key_index = unsafe { std::ptr::read_unaligned(key_ptr as *const u32) };

        if key_index >= map.max_entries {
            return 0; // out of bounds → null
        }

        // Return pointer to value at data_start + index * value_size.
        let offset = key_index as u64 * map.value_size as u64;
        map.data_start + offset
    })
}

/// Helper 11: map_update_elem(map_fd, key_ptr, value_ptr) -> 0 or error
///
/// For array maps: copies `value_size` bytes from `value_ptr` into the map
/// at the index given by `*key_ptr`.
fn helper_map_update(r1: u64, r2: u64, r3: u64, _r4: u64, _r5: u64) -> u64 {
    let map_fd = r1;
    let key_ptr = r2 as *const u8;
    let value_ptr = r3 as *const u8;

    if key_ptr.is_null() || value_ptr.is_null() {
        return (-1i64) as u64;
    }

    DECODER_STATE.with(|cell| {
        let state = cell.borrow();
        let state = match state.as_ref() {
            Some(s) => s,
            None => return (-1i64) as u64,
        };
        let map = match find_map(state, map_fd) {
            Some(m) => m,
            None => return (-1i64) as u64,
        };

        // GW-1904 AC-10: .rodata maps are read-only at runtime.
        if map.read_only {
            return (-1i64) as u64;
        }

        // SAFETY: key_ptr points into valid BPF memory (may be unaligned).
        let key_index = unsafe { std::ptr::read_unaligned(key_ptr as *const u32) };

        if key_index >= map.max_entries {
            return (-1i64) as u64;
        }

        let offset = key_index as u64 * map.value_size as u64;
        let dst = (map.data_start + offset) as *mut u8;

        // SAFETY: dst is within the map backing storage (heap-allocated Vec<u8>
        // that outlives this call), and value_ptr is within a valid BPF region.
        // Use `copy` (not `copy_nonoverlapping`) because value_ptr may alias
        // dst when a program updates a map entry from a lookup on the same key.
        unsafe {
            std::ptr::copy(value_ptr, dst, map.value_size as usize);
        }

        0u64
    })
}

/// Helper 16: bpf_trace_printk(fmt_ptr, fmt_len, ...) -> 0
fn helper_trace_printk(r1: u64, r2: u64, _r3: u64, _r4: u64, _r5: u64) -> u64 {
    let fmt_ptr = r1 as *const u8;
    let fmt_len = r2 as usize;

    if !fmt_ptr.is_null() && fmt_len > 0 {
        // SAFETY: Relies on Prevail verification at ingestion time — see
        // `execute_decoder` doc for the safety precondition.
        let bytes = unsafe { std::slice::from_raw_parts(fmt_ptr, fmt_len) };
        if let Ok(msg) = std::str::from_utf8(bytes) {
            tracing::debug!(target: "decoder_bpf", "{}", msg.trim_end());
        }
    }
    0
}

/// Decoder helper descriptors for the BPF interpreter.
fn decoder_helpers() -> Vec<HelperDescriptor> {
    vec![
        HelperDescriptor {
            id: 10,
            func: helper_map_lookup,
            ret: HelperReturn::MapValueOrNull { map_arg: 1 },
        },
        HelperDescriptor {
            id: 11,
            func: helper_map_update,
            ret: HelperReturn::Scalar,
        },
        HelperDescriptor {
            id: 16,
            func: helper_trace_printk,
            ret: HelperReturn::Scalar,
        },
        HelperDescriptor {
            id: 18,
            func: helper_emit_reading,
            ret: HelperReturn::Scalar,
        },
    ]
}

// ── Public API ──────────────────────────────────────────────────────────

/// Execute a decoder BPF program on raw APP_DATA bytes.
///
/// Returns the collected readings on success, or an error if execution fails.
/// This function is synchronous — call it from `spawn_blocking` in async
/// contexts to avoid blocking the tokio event loop.
///
/// # Safety precondition
///
/// `decoder_image_cbor` **must** be a CBOR image that was previously verified
/// by Prevail via `DecoderPlatform` during ELF ingestion (`extract_decoder`).
/// Executing unverified bytecode can cause undefined behavior because BPF
/// helper functions dereference raw register values as host pointers, relying
/// on the verifier to guarantee that pointer arguments are within valid
/// memory regions.
///
/// # Context ABI
///
/// The decoder context provides `{ input_data, input_end }` pointers at
/// offsets 0 and 8.  [`ContextPointerField`](interpreter::ContextPointerField)
/// descriptors tell the interpreter to tag these loaded values as pointers
/// into the blob region, enabling C-compiled decoders that use
/// `ctx->input_data` / `ctx->input_end` to work correctly.
///
/// # Safety
///
/// Caller must ensure `decoder_image_cbor` was produced by a Prevail-verified
/// ingestion path (i.e., `extract_decoder`). Passing unverified bytecode
/// causes undefined behavior.
pub unsafe fn execute_decoder(
    decoder_image_cbor: &[u8],
    raw_blob: &[u8],
) -> Result<BTreeMap<String, i64>, DecoderError> {
    // Decode the ProgramImage from CBOR.
    let image = ProgramImage::decode(decoder_image_cbor)
        .map_err(|e| DecoderError::ImageDecodeError(format!("{e}")))?;

    // Validate map count.
    if image.maps.len() > MAX_DECODER_MAPS {
        return Err(DecoderError::ImageDecodeError(format!(
            "decoder declares {} maps, max is {MAX_DECODER_MAPS}",
            image.maps.len()
        )));
    }

    // Allocate and initialize map storage.
    let mut map_backing: Vec<Vec<u8>> = Vec::with_capacity(image.maps.len());
    let mut map_regions: Vec<MapRegion> = Vec::with_capacity(image.maps.len());
    let mut map_infos: Vec<MapInfo> = Vec::with_capacity(image.maps.len());
    let mut total_map_memory: usize = 0;

    for (i, map_def) in image.maps.iter().enumerate() {
        // Decoder maps must be array-style with u32 keys.
        // Supported types: 0 (global variable / .rodata / .data / .bss), 1 (array).
        if map_def.map_type > 1 {
            return Err(DecoderError::ImageDecodeError(format!(
                "map {i}: unsupported map_type {} (expected 0 or 1)",
                map_def.map_type
            )));
        }
        if map_def.key_size != 4 {
            return Err(DecoderError::ImageDecodeError(format!(
                "map {i}: unsupported key_size {} (expected 4)",
                map_def.key_size
            )));
        }
        let total_size = (map_def.value_size as usize)
            .checked_mul(map_def.max_entries as usize)
            .ok_or_else(|| {
                DecoderError::ImageDecodeError(format!(
                    "map {i}: value_size * max_entries overflow ({} * {})",
                    map_def.value_size, map_def.max_entries
                ))
            })?;
        if total_size == 0 {
            return Err(DecoderError::ImageDecodeError(format!(
                "map {i}: zero-size map (value_size={}, max_entries={})",
                map_def.value_size, map_def.max_entries
            )));
        }
        total_map_memory = total_map_memory.saturating_add(total_size);
        if total_map_memory > MAX_DECODER_MAP_MEMORY {
            return Err(DecoderError::ImageDecodeError(format!(
                "decoder map memory ({total_map_memory} bytes) exceeds limit ({MAX_DECODER_MAP_MEMORY})"
            )));
        }
        let mut backing = vec![0u8; total_size];

        // Initialize from initial_data if present.
        if let Some(initial) = image.map_initial_data.get(i) {
            if !initial.is_empty() {
                if initial.len() > backing.len() {
                    return Err(DecoderError::ImageDecodeError(format!(
                        "map {i}: initial_data size ({}) exceeds backing size ({})",
                        initial.len(),
                        backing.len()
                    )));
                }
                backing[..initial.len()].copy_from_slice(initial);
            }
        }

        map_backing.push(backing);
    }

    // Build MapRegion descriptors and MapInfo entries pointing to backing storage.
    for (i, map_def) in image.maps.iter().enumerate() {
        let backing = &map_backing[i];
        let base_ptr = backing.as_ptr() as u64;
        let end_ptr = base_ptr + backing.len() as u64;

        map_regions.push(MapRegion {
            relocated_ptr: base_ptr,
            // Decoder backing stores value bytes densely without per-entry key
            // prefixes, so direct map-value relocations must treat data_start
            // as the start of entry 0's value region.
            key_size: 0,
            value_size: map_def.value_size,
            data_start: base_ptr,
            data_end: end_ptr,
        });

        map_infos.push(MapInfo {
            relocated_ptr: base_ptr,
            data_start: base_ptr,
            value_size: map_def.value_size,
            max_entries: map_def.max_entries,
            read_only: image.map_readonly.get(i).copied().unwrap_or(false),
        });
    }

    // Build the context buffer: [16-byte decoder_context header] + [raw blob].
    // The decoder_context has input_data (pointer to blob) and input_end
    // (pointer past blob end), both as u64 values.
    let ctx_header_size = 16usize;
    let mut ctx_buf = vec![0u8; ctx_header_size + raw_blob.len()];

    // Copy raw blob into the context buffer after the header.
    ctx_buf[ctx_header_size..].copy_from_slice(raw_blob);

    // Set input_data (offset 0): pointer to blob start within ctx_buf.
    let blob_addr = ctx_buf.as_ptr() as u64 + ctx_header_size as u64;
    ctx_buf[0..8].copy_from_slice(&blob_addr.to_le_bytes());

    // Set input_end (offset 8): pointer past blob end.
    let blob_end_addr = blob_addr + raw_blob.len() as u64;
    ctx_buf[8..16].copy_from_slice(&blob_end_addr.to_le_bytes());

    // Context pointer fields: tell the interpreter that the u64 values at
    // offsets 0 and 8 are pointers into the blob region so that BPF
    // programs can dereference ctx->input_data after loading it.
    let data_region = interpreter::Region {
        tag: interpreter::RegionTag::Context,
        base: blob_addr,
        end: blob_end_addr,
    };
    let ctx_ptrs = [
        interpreter::ContextPointerField {
            offset: 0,
            region: data_region,
        },
        interpreter::ContextPointerField {
            offset: 8,
            region: data_region,
        },
    ];

    // Install thread-local state and execute.
    let guard = DecoderStateGuard::install(map_infos);
    let helpers = decoder_helpers();

    // SAFETY:
    // - Each MapRegion's data_start..data_end covers valid, live heap
    //   allocations (the Vec<u8> in map_backing). They do not alias ctx_buf
    //   or the interpreter's stack. The map_backing Vecs outlive this call.
    // - Each ContextPointerField region covers blob_addr..blob_end_addr
    //   which is a sub-range of ctx_buf (offset 16..). ctx_buf is live for
    //   the duration of this call and does not alias the BPF stack.
    // - When map_regions is empty the MapRegion invariants are trivially met.
    let result = unsafe {
        interpreter::execute_program(
            &image.bytecode,
            &mut ctx_buf,
            &helpers,
            &map_regions,
            true, // read_only_ctx
            DECODER_INSTRUCTION_BUDGET,
            &ctx_ptrs,
        )
    };

    match result {
        Ok(_) => Ok(guard.take_readings()),
        Err(e) => {
            warn!(error = %e, "decoder BPF execution failed");
            Err(DecoderError::ExecutionError(e))
        }
    }
}
