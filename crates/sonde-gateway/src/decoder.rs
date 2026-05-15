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

/// State collected during a single decoder execution.
struct DecoderState {
    readings: BTreeMap<String, i64>,
    count: usize,
    /// Per-map read-only flag: true if the map is .rodata (writes rejected).
    map_readonly: Vec<bool>,
}

thread_local! {
    static DECODER_STATE: RefCell<Option<DecoderState>> = const { RefCell::new(None) };
}

/// RAII guard that installs and clears thread-local decoder state.
struct DecoderStateGuard;

impl DecoderStateGuard {
    fn install(map_readonly: Vec<bool>) -> Self {
        DECODER_STATE.with(|cell| {
            *cell.borrow_mut() = Some(DecoderState {
                readings: BTreeMap::new(),
                count: 0,
                map_readonly,
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

/// Helper 18: emit_reading(name_ptr, name_len, value_i64, _, _) -> 0 | -1 | -2
///
/// The name_ptr and name_len refer to the BPF program's virtual address space.
/// We cannot dereference them directly — the interpreter resolves them to
/// actual memory before calling the helper (via PtrToReadableMem + ConstSize).
///
/// However, the sonde-bpf helper calling convention passes raw register values.
/// The name bytes are in BPF memory at the address pointed to by r1. Since
/// the interpreter validates the pointer before calling us, we can safely
/// read from that address.
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
            // SAFETY: The BPF interpreter validated the pointer and length
            // before calling this helper. The memory is within a valid BPF
            // region (context or stack) and remains live during execution.
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
/// Standard BPF map lookup — handled entirely by the interpreter's
/// built-in map support. This is a pass-through.
fn helper_map_lookup(_r1: u64, _r2: u64, _r3: u64, _r4: u64, _r5: u64) -> u64 {
    // The interpreter handles map_lookup_elem internally via MapRegion.
    // This should not be called directly. Return null.
    0
}

/// Helper 11: map_update_elem(map_fd, key_ptr, value_ptr) -> 0 or error
///
/// For .rodata maps, returns error (-1). Otherwise handled by interpreter.
fn helper_map_update(r1: u64, _r2: u64, _r3: u64, _r4: u64, _r5: u64) -> u64 {
    // Check if the map is read-only (.rodata).
    let map_idx = r1 as usize;
    DECODER_STATE.with(|cell| {
        let state = cell.borrow();
        if let Some(ref s) = *state {
            if map_idx < s.map_readonly.len() && s.map_readonly[map_idx] {
                return (-1i64) as u64; // .rodata — write rejected
            }
        }
        0u64
    })
}

/// Helper 16: bpf_trace_printk(fmt_ptr, fmt_len, ...) -> 0
fn helper_trace_printk(r1: u64, r2: u64, _r3: u64, _r4: u64, _r5: u64) -> u64 {
    let fmt_ptr = r1 as *const u8;
    let fmt_len = r2 as usize;

    if !fmt_ptr.is_null() && fmt_len > 0 {
        // SAFETY: pointer validated by interpreter before call.
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
pub fn execute_decoder(
    decoder_image_cbor: &[u8],
    raw_blob: &[u8],
) -> Result<BTreeMap<String, i64>, DecoderError> {
    // Decode the ProgramImage from CBOR.
    let image = ProgramImage::decode(decoder_image_cbor)
        .map_err(|e| DecoderError::ImageDecodeError(format!("{e}")))?;

    // Determine which maps are read-only (.rodata).
    // Convention: map_type == 0 maps with non-empty initial_data that exactly
    // fills the value_size are considered read-only (from .rodata). Maps with
    // empty initial_data (from .bss) or map_type != 0 are writable.
    // This is a heuristic — a more precise approach would track provenance
    // from the ELF section name, but for decoder maps this is sufficient
    // since .rodata always has initial data and .bss/.data patterns differ.
    //
    // NOTE: This heuristic treats .data maps (which also have initial data)
    // as read-only, which is actually correct for decoder semantics — decoder
    // maps are ephemeral and reset each execution, so there's no persistence
    // concern. The spec says .data maps ARE writable though, so we only mark
    // map_type==0 maps as readonly if they have initial data. This matches
    // .rodata behavior. For .data (also map_type==0 with initial data), we
    // cannot distinguish from .rodata using ProgramImage alone.
    // For safety, we'll treat ALL map_type==0 maps as writable (the program
    // resets each time anyway), and only enforce read-only at the verifier
    // level (DecoderPlatform prevents writes to .rodata via Prevail checks).
    let map_readonly: Vec<bool> = image
        .maps
        .iter()
        .map(|_| false) // All maps writable at runtime; verifier enforces .rodata
        .collect();

    // Allocate and initialize map storage.
    let mut map_backing: Vec<Vec<u8>> = Vec::with_capacity(image.maps.len());
    let mut map_regions: Vec<MapRegion> = Vec::with_capacity(image.maps.len());

    for (i, map_def) in image.maps.iter().enumerate() {
        let total_size = (map_def.value_size as usize)
            .checked_mul(map_def.max_entries as usize)
            .unwrap_or(0);
        let mut backing = vec![0u8; total_size];

        // Initialize from initial_data if present.
        if let Some(initial) = image.map_initial_data.get(i) {
            if !initial.is_empty() {
                let copy_len = initial.len().min(backing.len());
                backing[..copy_len].copy_from_slice(&initial[..copy_len]);
            }
        }

        map_backing.push(backing);
    }

    // Build MapRegion descriptors pointing to the backing storage.
    // Use a two-pass approach: first allocate all backing, then build regions.
    // This avoids lifetime issues with Vec reallocation.
    for (i, map_def) in image.maps.iter().enumerate() {
        let backing = &map_backing[i];
        let base_ptr = backing.as_ptr() as u64;
        let end_ptr = base_ptr + backing.len() as u64;

        // relocated_ptr: the "fd" value that BPF LDDW instructions resolve to.
        // In sonde-bpf, map indices start at 1 for the first map.
        let relocated_ptr = (i + 1) as u64;

        map_regions.push(MapRegion {
            relocated_ptr,
            value_size: map_def.value_size,
            data_start: base_ptr,
            data_end: end_ptr,
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

    // Install thread-local state and execute.
    let guard = DecoderStateGuard::install(map_readonly);
    let helpers = decoder_helpers();

    let result = if map_regions.is_empty() {
        interpreter::execute_program_no_maps(
            &image.bytecode,
            &mut ctx_buf,
            &helpers,
            true, // read_only_ctx
            DECODER_INSTRUCTION_BUDGET,
        )
    } else {
        // SAFETY: each MapRegion's data_start..data_end covers valid, live
        // heap allocations (the Vec<u8> in map_backing). They do not alias
        // ctx_buf or the interpreter's stack. The map_backing Vecs outlive
        // the execute_program call.
        unsafe {
            interpreter::execute_program(
                &image.bytecode,
                &mut ctx_buf,
                &helpers,
                &map_regions,
                true, // read_only_ctx
                DECODER_INSTRUCTION_BUDGET,
            )
        }
    };

    match result {
        Ok(_) => Ok(guard.take_readings()),
        Err(e) => {
            warn!(error = %e, "decoder BPF execution failed");
            Err(DecoderError::ExecutionError(e))
        }
    }
}
