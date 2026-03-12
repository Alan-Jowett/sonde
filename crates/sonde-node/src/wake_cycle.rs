// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Wake cycle state machine.
//!
//! Implements the core node lifecycle per protocol.md §6.1:
//! `boot → WAKE → COMMAND → dispatch → (transfer/execute) → sleep`

use sonde_protocol::{
    decode_frame, encode_frame, verify_frame, CommandPayload, DecodeError, FrameHeader,
    GatewayMessage, HmacProvider, NodeMessage, Sha256Provider, MSG_APP_DATA, MSG_APP_DATA_REPLY,
    MSG_CHUNK, MSG_COMMAND, MSG_GET_CHUNK, MSG_PROGRAM_ACK, MSG_WAKE,
};

use crate::bpf_helpers::{ProgramClass, SondeContext};
use crate::bpf_runtime::BpfInterpreter;
use crate::error::{NodeError, NodeResult};
use crate::hal::{BatteryReader, Hal};
use crate::key_store::NodeIdentity;
use crate::map_storage::MapStorage;
use crate::program_store::{resolve_map_references, LoadedProgram, ProgramStore};
use crate::sleep::{SleepManager, WakeReason};
use crate::traits::{Clock, PlatformStorage, Rng, Transport};
use crate::FIRMWARE_ABI_VERSION;

/// Retry and timing constants (protocol.md §9).
const WAKE_MAX_RETRIES: u32 = 3;
const RETRY_DELAY_MS: u32 = 100;
const RESPONSE_TIMEOUT_MS: u32 = 50;

/// Default instruction budget for BPF execution.
const DEFAULT_INSTRUCTION_BUDGET: u64 = 100_000;

/// Default map budget in bytes (~4 KB for ESP32-C3 after firmware overhead).
/// Used by tests; production code receives the budget via `MapStorage`.
#[cfg(test)]
const DEFAULT_MAP_BUDGET: usize = 4096;

/// Maximum resident program image size (4 KB, matches flash partition).
const MAX_RESIDENT_IMAGE_SIZE: usize = 4096;

/// Maximum ephemeral program image size (2 KB, stored in RAM).
const MAX_EPHEMERAL_IMAGE_SIZE: usize = 2048;

/// Outcome of a wake cycle.
#[derive(Debug, PartialEq)]
pub enum WakeCycleOutcome {
    /// Normal completion — node should sleep for the specified seconds.
    Sleep { seconds: u32 },
    /// Reboot was requested by the gateway.
    Reboot,
    /// Node is unpaired — sleep indefinitely.
    Unpaired,
}

/// Run one complete wake cycle.
///
/// This is the top-level function that orchestrates the entire wake cycle.
/// It is generic over all platform traits so it can be tested on the host.
///
/// `map_storage` is caller-owned and backed by sleep-persistent memory
/// (RTC slow SRAM on ESP32). The caller preserves it across deep sleep
/// so that map contents survive between wake cycles. This function only
/// re-allocates maps when a new program is installed.
#[allow(clippy::too_many_arguments)]
pub fn run_wake_cycle<T, S, H, R, C, B, I, M>(
    transport: &mut T,
    storage: &mut S,
    hal: &mut H,
    rng: &mut R,
    clock: &C,
    battery: &B,
    interpreter: &mut I,
    map_storage: &mut MapStorage,
    hmac: &M,
    sha: &impl Sha256Provider,
) -> WakeCycleOutcome
where
    T: Transport + 'static,
    S: PlatformStorage,
    H: Hal + 'static,
    R: Rng,
    C: Clock + 'static,
    B: BatteryReader + 'static,
    I: BpfInterpreter,
    M: HmacProvider + 'static,
{
    // 1. Load identity
    let identity = match storage.read_key() {
        Some((key_hint, psk)) => NodeIdentity { key_hint, psk },
        None => return WakeCycleOutcome::Unpaired,
    };

    // 2. Determine wake reason
    let wake_reason = determine_wake_reason(storage);

    // 3. Load schedule
    let (base_interval_s, _active_partition) = storage.read_schedule();
    let mut sleep_mgr = SleepManager::new(base_interval_s, wake_reason);

    // 4. Get current program hash
    let program_hash = {
        let program_store = ProgramStore::new(storage);
        program_store.active_program_hash(sha)
    };

    // 5. Generate WAKE nonce
    let wake_nonce = rng.random_u64();
    let battery_mv = battery.battery_mv();

    // 6. Send WAKE, await COMMAND (with retries)
    let command_result = wake_command_exchange(
        transport,
        &identity,
        wake_nonce,
        &program_hash,
        battery_mv,
        clock,
        hmac,
    );

    let (starting_seq, timestamp_ms, command_payload) = match command_result {
        Ok(cmd) => cmd,
        Err(_) => {
            // WAKE retries exhausted or transport error — sleep
            return WakeCycleOutcome::Sleep {
                seconds: sleep_mgr.effective_sleep_s(),
            };
        }
    };

    // 7. Record gateway timestamp for BPF context
    let command_received_at = clock.elapsed_ms();

    // 8. Dispatch command
    let mut current_seq = starting_seq;
    let mut loaded_program: Option<LoadedProgram> = None;

    let is_ephemeral = matches!(&command_payload, CommandPayload::RunEphemeral { .. });

    match command_payload {
        CommandPayload::Nop => {
            // Proceed to BPF execution
        }
        CommandPayload::Reboot => {
            return WakeCycleOutcome::Reboot;
        }
        CommandPayload::UpdateSchedule { interval_s } => {
            if storage.write_schedule_interval(interval_s).is_ok() {
                sleep_mgr.set_base_interval(interval_s);
            }
        }
        CommandPayload::UpdateProgram {
            program_hash: expected_hash,
            program_size,
            chunk_size,
            chunk_count,
            ..
        }
        | CommandPayload::RunEphemeral {
            program_hash: expected_hash,
            program_size,
            chunk_size,
            chunk_count,
            ..
        } => {
            let max_image_size = if is_ephemeral {
                MAX_EPHEMERAL_IMAGE_SIZE
            } else {
                MAX_RESIDENT_IMAGE_SIZE
            };

            // Chunked transfer
            let transfer_result = chunked_transfer(
                transport,
                &identity,
                &mut current_seq,
                program_size,
                chunk_size,
                chunk_count,
                max_image_size,
                clock,
                hmac,
            );

            match transfer_result {
                Ok(image_bytes) => {
                    // Verify hash and install/load
                    let install_result = {
                        let mut program_store = ProgramStore::new(storage);
                        if is_ephemeral {
                            program_store.load_ephemeral(&image_bytes, &expected_hash, sha)
                        } else {
                            program_store.install_resident(
                                &image_bytes,
                                &expected_hash,
                                sha,
                                map_storage.budget_bytes(),
                            )
                        }
                    };

                    match install_result {
                        Ok(program) => {
                            // Send PROGRAM_ACK; abort cycle on failure to
                            // keep session sequencing consistent.
                            if send_program_ack(
                                transport,
                                &identity,
                                &mut current_seq,
                                &program.hash,
                                hmac,
                            )
                            .is_err()
                            {
                                return WakeCycleOutcome::Sleep {
                                    seconds: sleep_mgr.effective_sleep_s(),
                                };
                            }

                            if !is_ephemeral {
                                // Set the in-cycle wake reason so the BPF
                                // program executing immediately below sees
                                // ProgramUpdate. We do NOT persist a flag
                                // for the next boot — the program already
                                // runs in this cycle (ND-0506), so the next
                                // boot should report Scheduled, not
                                // ProgramUpdate again.
                                sleep_mgr.set_wake_reason(WakeReason::ProgramUpdate);
                            }
                            loaded_program = Some(program);
                        }
                        Err(_) => {
                            // Hash mismatch or decode failure — discard, sleep
                            return WakeCycleOutcome::Sleep {
                                seconds: sleep_mgr.effective_sleep_s(),
                            };
                        }
                    }
                }
                Err(_) => {
                    // Chunk transfer failed — sleep
                    return WakeCycleOutcome::Sleep {
                        seconds: sleep_mgr.effective_sleep_s(),
                    };
                }
            }
        }
    }

    // 9. BPF execution
    // Track whether a new resident program was installed this cycle.
    // Used to force map re-initialization even when layout matches.
    let resident_installed_this_cycle = loaded_program.as_ref().is_some_and(|p| !p.is_ephemeral);

    // Load program if not already loaded from transfer
    if loaded_program.is_none() {
        let program_store = ProgramStore::new(storage);
        loaded_program = program_store.load_active(sha);
    }

    if let Some(mut program) = loaded_program {
        let program_class = if program.is_ephemeral {
            ProgramClass::Ephemeral
        } else {
            ProgramClass::Resident
        };

        if program.is_ephemeral {
            // Ephemeral programs use the existing resident map layout
            // (read-only access only, per ND-0503 / bpf-environment §2.2).
            // They must not declare their own maps — reject if they do,
            // as re-allocating would destroy the resident program's
            // sleep-persistent map state.
            if !program.map_defs.is_empty() {
                return WakeCycleOutcome::Sleep {
                    seconds: sleep_mgr.effective_sleep_s(),
                };
            }
        } else {
            // For resident programs, re-allocate maps when the layout
            // doesn't match OR when a new program was installed this
            // cycle (even with identical layout, map data must be
            // zero-initialized per node-design.md §9.2).
            if (resident_installed_this_cycle || !map_storage.layout_matches(&program.map_defs))
                && map_storage.allocate(&program.map_defs).is_err()
            {
                // Map budget exceeded. The newly installed resident
                // program is already active (install_resident swapped
                // partitions), so we do not roll back here.
                return WakeCycleOutcome::Sleep {
                    seconds: sleep_mgr.effective_sleep_s(),
                };
            }
        }

        // Resolve LDDW map references.
        let map_ptrs = map_storage.map_pointers().to_vec();
        if resolve_map_references(&mut program.bytecode, &map_ptrs).is_err() {
            return WakeCycleOutcome::Sleep {
                seconds: sleep_mgr.effective_sleep_s(),
            };
        }

        // Build execution context
        let elapsed_since_command = clock.elapsed_ms().saturating_sub(command_received_at);
        let battery_mv_clamped = if battery_mv > u16::MAX as u32 {
            u16::MAX
        } else {
            battery_mv as u16
        };
        let ctx = SondeContext {
            timestamp: timestamp_ms.saturating_add(elapsed_since_command),
            battery_mv: battery_mv_clamped,
            firmware_abi_version: u16::try_from(FIRMWARE_ABI_VERSION)
                .expect("FIRMWARE_ABI_VERSION must fit in u16"),
            wake_reason: sleep_mgr.wake_reason() as u8,
            _padding: [0; 3],
        };

        // Load and execute with helper dispatch context installed.
        // Trace log for bpf_trace_printk. Capped at MAX entries to bound
        // heap growth; on embedded builds this could be feature-gated.
        let mut trace_log = Vec::new();
        // SAFETY: all referenced objects are alive on this stack frame
        // and will not be moved until `_guard` is dropped below.
        unsafe {
            crate::bpf_dispatch::install(
                hal as *mut H as *mut dyn crate::hal::Hal,
                transport as *mut T as *mut dyn crate::traits::Transport,
                map_storage as *mut MapStorage,
                &mut sleep_mgr as *mut SleepManager,
                clock as *const C as *const dyn crate::traits::Clock,
                hmac as *const M as *const dyn HmacProvider,
                &identity as *const NodeIdentity,
                &mut current_seq as *mut u64,
                program_class,
                &mut trace_log as *mut Vec<String>,
                timestamp_ms,
                command_received_at,
                battery_mv,
            );
        }
        let _guard = crate::bpf_dispatch::DispatchGuard;

        let exec_result = match crate::bpf_dispatch::register_all(interpreter) {
            Ok(()) => match interpreter.load(&program.bytecode, &map_ptrs) {
                Ok(()) => {
                    let ctx_ptr = &ctx as *const SondeContext as u64;
                    interpreter.execute(ctx_ptr, DEFAULT_INSTRUCTION_BUDGET)
                }
                Err(err) => {
                    log::error!("BPF program load failed: {}", err);
                    Err(err)
                }
            },
            Err(err) => {
                log::error!("BPF helper registration failed: {}", err);
                Err(err)
            }
        };

        // Swallow BPF errors — node sleeps normally regardless (ND-0504).
        let _ = exec_result;

        // Flush accumulated trace output (ND-0604 / T-N613).
        for entry in &trace_log {
            log::debug!("bpf_trace_printk: {}", entry);
        }
    }

    // 10. Determine sleep duration
    if sleep_mgr.will_wake_early() {
        // Best-effort: retry once if the first write fails.
        if storage.set_early_wake_flag().is_err() {
            let _ = storage.set_early_wake_flag();
        }
    }

    WakeCycleOutcome::Sleep {
        seconds: sleep_mgr.effective_sleep_s(),
    }
}

/// Determine the wake reason from RTC flags.
fn determine_wake_reason<S: PlatformStorage>(storage: &mut S) -> WakeReason {
    if storage.take_early_wake_flag() {
        WakeReason::Early
    } else {
        WakeReason::Scheduled
    }
}

/// Execute the WAKE/COMMAND exchange with retry logic.
///
/// Returns `(starting_seq, timestamp_ms, CommandPayload)` on success.
fn wake_command_exchange<T: Transport, C: Clock>(
    transport: &mut T,
    identity: &NodeIdentity,
    wake_nonce: u64,
    program_hash: &[u8],
    battery_mv: u32,
    clock: &C,
    hmac: &impl HmacProvider,
) -> NodeResult<(u64, u64, CommandPayload)> {
    let wake_msg = NodeMessage::Wake {
        firmware_abi_version: FIRMWARE_ABI_VERSION,
        program_hash: program_hash.to_vec(),
        battery_mv,
    };
    let payload_cbor = wake_msg
        .encode()
        .map_err(|e| NodeError::MalformedPayload(format!("{}", e)))?;

    let header = FrameHeader {
        key_hint: identity.key_hint,
        msg_type: MSG_WAKE,
        nonce: wake_nonce,
    };

    let frame = encode_frame(&header, &payload_cbor, &identity.psk, hmac)
        .map_err(|e| NodeError::MalformedPayload(format!("{}", e)))?;

    // Try sending WAKE up to (1 + WAKE_MAX_RETRIES) times
    for attempt in 0..=WAKE_MAX_RETRIES {
        if attempt > 0 {
            clock.delay_ms(RETRY_DELAY_MS);
        }

        transport.send(&frame)?;

        // Await COMMAND response
        match transport.recv(RESPONSE_TIMEOUT_MS)? {
            Some(raw_response) => {
                // Try to verify and decode
                match verify_and_decode_command(&raw_response, identity, wake_nonce, hmac) {
                    Ok(result) => return Ok(result),
                    Err(_) => {
                        // Invalid response — discard and retry
                        continue;
                    }
                }
            }
            None => {
                // Timeout — retry
                continue;
            }
        }
    }

    Err(NodeError::WakeRetriesExhausted)
}

/// Verify and decode a COMMAND response.
fn verify_and_decode_command(
    raw: &[u8],
    identity: &NodeIdentity,
    expected_nonce: u64,
    hmac: &impl HmacProvider,
) -> NodeResult<(u64, u64, CommandPayload)> {
    let decoded = decode_frame(raw).map_err(|e| NodeError::MalformedPayload(format!("{}", e)))?;

    // Verify HMAC
    if !verify_frame(&decoded, &identity.psk, hmac) {
        return Err(NodeError::AuthFailure);
    }

    // Verify msg_type
    if decoded.header.msg_type != MSG_COMMAND {
        return Err(NodeError::UnexpectedMsgType(decoded.header.msg_type));
    }

    // Verify echoed nonce
    if decoded.header.nonce != expected_nonce {
        return Err(NodeError::ResponseBindingMismatch);
    }

    // Decode CBOR. If the command_type is unknown, treat it as NOP per
    // ND-0202: "Unknown command types are ignored (the node proceeds to
    // BPF execution as if NOP)." We still need starting_seq and
    // timestamp_ms from the payload, so we attempt a NOP-style decode.
    let gateway_msg = match GatewayMessage::decode(decoded.header.msg_type, &decoded.payload) {
        Ok(msg) => msg,
        Err(DecodeError::InvalidCommandType(_)) => {
            // Unknown command_type in an otherwise valid, authenticated
            // COMMAND frame. Extract starting_seq and timestamp_ms by
            // re-decoding with a NOP command_type substituted. The
            // simplest approach: parse the top-level CBOR map directly
            // for the two required fields.
            return decode_command_as_nop(&decoded.payload);
        }
        Err(e) => return Err(NodeError::MalformedPayload(format!("{}", e))),
    };

    match gateway_msg {
        GatewayMessage::Command {
            starting_seq,
            timestamp_ms,
            payload,
        } => Ok((starting_seq, timestamp_ms, payload)),
        _ => Err(NodeError::UnexpectedMsgType(decoded.header.msg_type)),
    }
}

/// Extract `starting_seq` and `timestamp_ms` from a COMMAND payload
/// with an unknown `command_type`, treating it as NOP.
///
/// Per ND-0202, unknown command types are treated as NOP. The node
/// still needs `starting_seq` and `timestamp_ms` from the CBOR map
/// to maintain session sequencing and time reference.
fn decode_command_as_nop(payload: &[u8]) -> NodeResult<(u64, u64, CommandPayload)> {
    // Parse the CBOR map to extract keys 13 (starting_seq) and 14 (timestamp_ms).
    // We use ciborium directly since GatewayMessage::decode rejected the command_type.
    let value: ciborium::Value = ciborium::from_reader(payload)
        .map_err(|e| NodeError::MalformedPayload(format!("{}", e)))?;

    let fields = match &value {
        ciborium::Value::Map(pairs) => pairs,
        _ => return Err(NodeError::MalformedPayload("expected CBOR map".into())),
    };

    let mut starting_seq: Option<u64> = None;
    let mut timestamp_ms: Option<u64> = None;

    for (k, v) in fields {
        let key = k.as_integer().and_then(|i| u64::try_from(i).ok());
        match key {
            Some(sonde_protocol::KEY_STARTING_SEQ) => {
                starting_seq = v.as_integer().and_then(|i| u64::try_from(i).ok());
            }
            Some(sonde_protocol::KEY_TIMESTAMP_MS) => {
                timestamp_ms = v.as_integer().and_then(|i| u64::try_from(i).ok());
            }
            _ => {}
        }
    }

    let starting_seq = starting_seq.ok_or_else(|| {
        NodeError::MalformedPayload("missing starting_seq in unknown command".into())
    })?;
    let timestamp_ms = timestamp_ms.ok_or_else(|| {
        NodeError::MalformedPayload("missing timestamp_ms in unknown command".into())
    })?;

    Ok((starting_seq, timestamp_ms, CommandPayload::Nop))
}

/// Execute the chunked program transfer protocol.
///
/// Returns the reassembled program image bytes on success.
#[allow(clippy::too_many_arguments)]
fn chunked_transfer<T: Transport, C: Clock>(
    transport: &mut T,
    identity: &NodeIdentity,
    current_seq: &mut u64,
    program_size: u32,
    chunk_size: u32,
    chunk_count: u32,
    max_image_size: usize,
    clock: &C,
    hmac: &impl HmacProvider,
) -> NodeResult<Vec<u8>> {
    let program_size_usize = program_size as usize;
    let chunk_size_usize = chunk_size as usize;

    // Reject transfers that exceed the maximum program image size
    if program_size_usize > max_image_size {
        return Err(NodeError::MalformedPayload(format!(
            "program_size {} exceeds maximum {}",
            program_size, max_image_size
        )));
    }

    // Validate chunk_size is non-zero
    if chunk_size == 0 {
        return Err(NodeError::MalformedPayload("chunk_size is zero".into()));
    }

    // Validate chunk_count matches expected value from program_size/chunk_size
    let expected_chunk_count = sonde_protocol::chunk_count(program_size_usize, chunk_size_usize);
    if expected_chunk_count != Some(chunk_count) {
        return Err(NodeError::MalformedPayload(format!(
            "chunk_count {} does not match expected {:?} for program_size={} chunk_size={}",
            chunk_count, expected_chunk_count, program_size, chunk_size
        )));
    }

    let mut image_data: Vec<u8> = Vec::with_capacity(program_size_usize);

    for chunk_index in 0..chunk_count {
        let chunk_data =
            get_chunk_with_retry(transport, identity, current_seq, chunk_index, clock, hmac)?;

        // Enforce per-chunk size limit
        if chunk_data.len() > chunk_size_usize {
            return Err(NodeError::MalformedPayload(
                "received chunk larger than declared chunk_size".into(),
            ));
        }

        // Enforce overall program size limit
        if image_data.len() + chunk_data.len() > program_size_usize {
            return Err(NodeError::MalformedPayload(
                "received data exceeds declared program_size".into(),
            ));
        }

        image_data.extend_from_slice(&chunk_data);
    }

    // Final validation: assembled size must match declared program_size
    if image_data.len() != program_size_usize {
        return Err(NodeError::MalformedPayload(
            "assembled program size does not match declared program_size".into(),
        ));
    }

    Ok(image_data)
}

/// Request a single chunk with retry logic.
///
/// Each retry attempt uses a fresh sequence number, since the gateway
/// may have received (and advanced past) the prior attempt's seq even
/// though the response was lost.
fn get_chunk_with_retry<T: Transport, C: Clock>(
    transport: &mut T,
    identity: &NodeIdentity,
    current_seq: &mut u64,
    chunk_index: u32,
    clock: &C,
    hmac: &impl HmacProvider,
) -> NodeResult<Vec<u8>> {
    let get_chunk_msg = NodeMessage::GetChunk { chunk_index };
    let payload_cbor = get_chunk_msg
        .encode()
        .map_err(|e| NodeError::MalformedPayload(format!("{}", e)))?;

    for attempt in 0..=WAKE_MAX_RETRIES {
        if attempt > 0 {
            clock.delay_ms(RETRY_DELAY_MS);
        }

        let attempt_seq = *current_seq;

        let header = FrameHeader {
            key_hint: identity.key_hint,
            msg_type: MSG_GET_CHUNK,
            nonce: attempt_seq,
        };

        let frame = encode_frame(&header, &payload_cbor, &identity.psk, hmac)
            .map_err(|e| NodeError::MalformedPayload(format!("{}", e)))?;

        transport.send(&frame)?;
        *current_seq += 1;

        match transport.recv(RESPONSE_TIMEOUT_MS)? {
            Some(raw_response) => {
                match verify_and_decode_chunk(
                    &raw_response,
                    identity,
                    attempt_seq,
                    chunk_index,
                    hmac,
                ) {
                    Ok(data) => return Ok(data),
                    Err(_) => continue,
                }
            }
            None => continue,
        }
    }

    Err(NodeError::ChunkTransferFailed { chunk_index })
}

/// Verify and decode a CHUNK response.
fn verify_and_decode_chunk(
    raw: &[u8],
    identity: &NodeIdentity,
    expected_seq: u64,
    expected_index: u32,
    hmac: &impl HmacProvider,
) -> NodeResult<Vec<u8>> {
    let decoded = decode_frame(raw).map_err(|e| NodeError::MalformedPayload(format!("{}", e)))?;

    if !verify_frame(&decoded, &identity.psk, hmac) {
        return Err(NodeError::AuthFailure);
    }

    if decoded.header.msg_type != MSG_CHUNK {
        return Err(NodeError::UnexpectedMsgType(decoded.header.msg_type));
    }

    if decoded.header.nonce != expected_seq {
        return Err(NodeError::ResponseBindingMismatch);
    }

    let gateway_msg = GatewayMessage::decode(decoded.header.msg_type, &decoded.payload)
        .map_err(|e| NodeError::MalformedPayload(format!("{}", e)))?;

    match gateway_msg {
        GatewayMessage::Chunk {
            chunk_index,
            chunk_data,
        } => {
            if chunk_index != expected_index {
                return Err(NodeError::ChunkIndexMismatch {
                    expected: expected_index,
                    received: chunk_index,
                });
            }
            Ok(chunk_data)
        }
        _ => Err(NodeError::UnexpectedMsgType(decoded.header.msg_type)),
    }
}

/// Send a PROGRAM_ACK message.
///
/// The sequence number is consumed only after a successful send.
fn send_program_ack<T: Transport>(
    transport: &mut T,
    identity: &NodeIdentity,
    current_seq: &mut u64,
    program_hash: &[u8],
    hmac: &impl HmacProvider,
) -> NodeResult<()> {
    let seq = *current_seq;

    let ack_msg = NodeMessage::ProgramAck {
        program_hash: program_hash.to_vec(),
    };
    let payload_cbor = ack_msg
        .encode()
        .map_err(|e| NodeError::MalformedPayload(format!("{}", e)))?;

    let header = FrameHeader {
        key_hint: identity.key_hint,
        msg_type: MSG_PROGRAM_ACK,
        nonce: seq,
    };

    let frame = encode_frame(&header, &payload_cbor, &identity.psk, hmac)
        .map_err(|e| NodeError::MalformedPayload(format!("{}", e)))?;

    transport.send(&frame)?;
    *current_seq += 1;
    Ok(())
}

/// Send an APP_DATA message (fire-and-forget).
///
/// The sequence number is consumed only after a successful send. If
/// `transport.send()` fails, `current_seq` is not advanced so the
/// gateway's expected sequence stays in sync.
///
/// Rejects payloads that exceed the frame payload budget. A pre-check
/// on blob length avoids allocating for obviously oversized inputs;
/// the exact check is done after CBOR encoding.
pub fn send_app_data<T: Transport + ?Sized, H: HmacProvider + ?Sized>(
    transport: &mut T,
    identity: &NodeIdentity,
    current_seq: &mut u64,
    blob: &[u8],
    hmac: &H,
) -> NodeResult<()> {
    // Fast pre-check: blob alone exceeds payload budget (CBOR overhead
    // only makes it larger), so reject before allocating.
    if blob.len() > sonde_protocol::MAX_PAYLOAD_SIZE {
        return Err(NodeError::MalformedPayload(
            "APP_DATA blob exceeds frame payload budget".into(),
        ));
    }

    let seq = *current_seq;

    let msg = NodeMessage::AppData {
        blob: blob.to_vec(),
    };
    let payload_cbor = msg
        .encode()
        .map_err(|e| NodeError::MalformedPayload(format!("{}", e)))?;

    // Reject after encoding so the CBOR overhead is accounted for.
    if payload_cbor.len() > sonde_protocol::MAX_PAYLOAD_SIZE {
        return Err(NodeError::MalformedPayload(
            "APP_DATA payload exceeds frame payload budget".into(),
        ));
    }

    let header = FrameHeader {
        key_hint: identity.key_hint,
        msg_type: MSG_APP_DATA,
        nonce: seq,
    };

    let frame = encode_frame(&header, &payload_cbor, &identity.psk, hmac)
        .map_err(|e| NodeError::MalformedPayload(format!("{}", e)))?;

    transport.send(&frame)?;
    *current_seq += 1;
    Ok(())
}

/// Send an APP_DATA message and wait for APP_DATA_REPLY.
///
/// The sequence number is consumed after a successful send. On send
/// failure `current_seq` is not advanced. On recv timeout `current_seq`
/// remains advanced because the gateway likely received the request.
///
/// An overall deadline is enforced via `clock` so that a stream of
/// invalid frames cannot keep the node awake indefinitely.
pub fn send_recv_app_data<T: Transport + ?Sized, C: Clock + ?Sized, H: HmacProvider + ?Sized>(
    transport: &mut T,
    identity: &NodeIdentity,
    current_seq: &mut u64,
    blob: &[u8],
    timeout_ms: u32,
    clock: &C,
    hmac: &H,
) -> NodeResult<Vec<u8>> {
    // Fast pre-check: blob alone exceeds payload budget, reject before
    // allocating.
    if blob.len() > sonde_protocol::MAX_PAYLOAD_SIZE {
        return Err(NodeError::MalformedPayload(
            "APP_DATA blob exceeds frame payload budget".into(),
        ));
    }

    let seq = *current_seq;

    let msg = NodeMessage::AppData {
        blob: blob.to_vec(),
    };
    let payload_cbor = msg
        .encode()
        .map_err(|e| NodeError::MalformedPayload(format!("{}", e)))?;

    // Reject after encoding so the CBOR overhead is accounted for.
    if payload_cbor.len() > sonde_protocol::MAX_PAYLOAD_SIZE {
        return Err(NodeError::MalformedPayload(
            "APP_DATA payload exceeds frame payload budget".into(),
        ));
    }

    let header = FrameHeader {
        key_hint: identity.key_hint,
        msg_type: MSG_APP_DATA,
        nonce: seq,
    };

    let frame = encode_frame(&header, &payload_cbor, &identity.psk, hmac)
        .map_err(|e| NodeError::MalformedPayload(format!("{}", e)))?;

    transport.send(&frame)?;
    // Advance seq after successful send — the gateway saw this seq even
    // if the reply is lost or times out.
    *current_seq += 1;

    // Wait for reply: silently discard malformed/unexpected frames until
    // a valid APP_DATA_REPLY arrives or the overall deadline expires
    // (ND-0800/ND-0801). The deadline prevents a stream of junk frames
    // from keeping the node awake indefinitely.
    let deadline = clock.elapsed_ms().saturating_add(timeout_ms as u64);
    loop {
        let now = clock.elapsed_ms();
        if now >= deadline {
            return Err(NodeError::Timeout);
        }
        let remaining = (deadline - now) as u32;
        match transport.recv(remaining)? {
            Some(raw_response) => {
                let decoded = match decode_frame(&raw_response) {
                    Ok(frame) => frame,
                    Err(_) => continue,
                };

                if !verify_frame(&decoded, &identity.psk, hmac) {
                    continue;
                }

                if decoded.header.msg_type != MSG_APP_DATA_REPLY {
                    continue;
                }

                if decoded.header.nonce != seq {
                    continue;
                }

                let gateway_msg =
                    match GatewayMessage::decode(decoded.header.msg_type, &decoded.payload) {
                        Ok(msg) => msg,
                        Err(_) => continue,
                    };

                match gateway_msg {
                    GatewayMessage::AppDataReply { blob } => return Ok(blob),
                    _ => continue,
                }
            }
            None => return Err(NodeError::Timeout),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bpf_runtime::BpfError;
    use crate::error::NodeResult;
    use crate::traits::PlatformStorage;
    use sonde_protocol::{HmacProvider, Sha256Provider};
    use std::collections::VecDeque;

    // --- Test crypto providers ---

    struct TestHmac;
    impl HmacProvider for TestHmac {
        fn compute(&self, key: &[u8], data: &[u8]) -> [u8; 32] {
            use hmac::Mac;
            let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(key).expect("HMAC key");
            mac.update(data);
            mac.finalize().into_bytes().into()
        }
        fn verify(&self, key: &[u8], data: &[u8], expected: &[u8; 32]) -> bool {
            let computed = self.compute(key, data);
            computed == *expected
        }
    }

    struct TestSha256;
    impl Sha256Provider for TestSha256 {
        fn hash(&self, data: &[u8]) -> [u8; 32] {
            use sha2::Digest;
            sha2::Sha256::digest(data).into()
        }
    }

    // --- Mock transport ---

    struct MockTransport {
        /// Responses queued for recv()
        inbound: VecDeque<Option<Vec<u8>>>,
        /// Frames captured from send()
        outbound: Vec<Vec<u8>>,
    }

    impl MockTransport {
        fn new() -> Self {
            Self {
                inbound: VecDeque::new(),
                outbound: Vec::new(),
            }
        }

        fn queue_response(&mut self, frame: Option<Vec<u8>>) {
            self.inbound.push_back(frame);
        }
    }

    impl Transport for MockTransport {
        fn send(&mut self, frame: &[u8]) -> NodeResult<()> {
            self.outbound.push(frame.to_vec());
            Ok(())
        }

        fn recv(&mut self, _timeout_ms: u32) -> NodeResult<Option<Vec<u8>>> {
            Ok(self.inbound.pop_front().flatten())
        }
    }

    // --- Mock storage ---

    pub struct MockStorage {
        pub key: Option<(u16, [u8; 32])>,
        pub schedule_interval: u32,
        pub active_partition: u8,
        pub programs: [Option<Vec<u8>>; 2],
        pub early_wake_flag: bool,
    }

    impl MockStorage {
        pub fn new() -> Self {
            Self {
                key: None,
                schedule_interval: 60,
                active_partition: 0,
                programs: [None, None],
                early_wake_flag: false,
            }
        }

        pub fn with_key(mut self, key_hint: u16, psk: [u8; 32]) -> Self {
            self.key = Some((key_hint, psk));
            self
        }
    }

    impl PlatformStorage for MockStorage {
        fn read_key(&self) -> Option<(u16, [u8; 32])> {
            self.key
        }
        fn write_key(&mut self, key_hint: u16, psk: &[u8; 32]) -> NodeResult<()> {
            self.key = Some((key_hint, *psk));
            Ok(())
        }
        fn erase_key(&mut self) -> NodeResult<()> {
            self.key = None;
            Ok(())
        }
        fn read_schedule(&self) -> (u32, u8) {
            (self.schedule_interval, self.active_partition)
        }
        fn write_schedule_interval(&mut self, interval_s: u32) -> NodeResult<()> {
            self.schedule_interval = interval_s;
            Ok(())
        }
        fn write_active_partition(&mut self, partition: u8) -> NodeResult<()> {
            self.active_partition = partition;
            Ok(())
        }
        fn reset_schedule(&mut self) -> NodeResult<()> {
            self.schedule_interval = 60;
            self.active_partition = 0;
            Ok(())
        }
        fn read_program(&self, partition: u8) -> Option<Vec<u8>> {
            self.programs[partition as usize].clone()
        }
        fn write_program(&mut self, partition: u8, image: &[u8]) -> NodeResult<()> {
            self.programs[partition as usize] = Some(image.to_vec());
            Ok(())
        }
        fn erase_program(&mut self, partition: u8) -> NodeResult<()> {
            self.programs[partition as usize] = None;
            Ok(())
        }
        fn take_early_wake_flag(&mut self) -> bool {
            let v = self.early_wake_flag;
            self.early_wake_flag = false;
            v
        }
        fn set_early_wake_flag(&mut self) -> NodeResult<()> {
            self.early_wake_flag = true;
            Ok(())
        }
    }

    // --- Mock HAL ---

    struct MockHal;
    impl Hal for MockHal {
        fn i2c_read(&mut self, _h: u32, _buf: &mut [u8]) -> i32 {
            0
        }
        fn i2c_write(&mut self, _h: u32, _data: &[u8]) -> i32 {
            0
        }
        fn i2c_write_read(&mut self, _h: u32, _w: &[u8], _r: &mut [u8]) -> i32 {
            0
        }
        fn spi_transfer(
            &mut self,
            _h: u32,
            _tx: Option<&[u8]>,
            _rx: Option<&mut [u8]>,
            _l: usize,
        ) -> i32 {
            0
        }
        fn gpio_read(&self, _pin: u32) -> i32 {
            0
        }
        fn gpio_write(&mut self, _pin: u32, _val: u32) -> i32 {
            0
        }
        fn adc_read(&self, _ch: u32) -> i32 {
            0
        }
    }

    struct MockBattery;
    impl BatteryReader for MockBattery {
        fn battery_mv(&self) -> u32 {
            3300
        }
    }

    // --- Mock Rng ---
    struct MockRng(u64);
    impl Rng for MockRng {
        fn random_u64(&mut self) -> u64 {
            self.0 += 1;
            self.0
        }
    }

    // --- Mock Clock ---
    struct MockClock;
    impl Clock for MockClock {
        fn elapsed_ms(&self) -> u64 {
            100
        }
        fn delay_ms(&self, _ms: u32) {
            // No-op in tests
        }
    }

    // --- Mock BPF interpreter ---
    struct MockBpfInterpreter {
        loaded: bool,
        executed: bool,
        execute_result: Result<u64, BpfError>,
        /// Captured BPF execution context (copied during execute()).
        captured_ctx: Option<SondeContext>,
    }

    impl MockBpfInterpreter {
        fn new() -> Self {
            Self {
                loaded: false,
                executed: false,
                execute_result: Ok(0),
                captured_ctx: None,
            }
        }
    }

    impl BpfInterpreter for MockBpfInterpreter {
        fn register_helper(
            &mut self,
            _id: u32,
            _func: crate::bpf_runtime::HelperFn,
        ) -> Result<(), BpfError> {
            Ok(())
        }
        fn load(&mut self, _bytecode: &[u8], _map_ptrs: &[u64]) -> Result<(), BpfError> {
            self.loaded = true;
            Ok(())
        }
        fn execute(&mut self, ctx_ptr: u64, _budget: u64) -> Result<u64, BpfError> {
            self.executed = true;
            if ctx_ptr != 0 {
                // Safety: ctx_ptr points to a SondeContext on the caller's
                // stack, which is alive for the duration of this call.
                let ctx = unsafe { &*(ctx_ptr as *const SondeContext) };
                self.captured_ctx = Some(*ctx);
            }
            self.execute_result.clone()
        }
    }

    // --- Helper to build a valid COMMAND response frame ---

    fn build_command_response(
        psk: &[u8; 32],
        key_hint: u16,
        echo_nonce: u64,
        starting_seq: u64,
        timestamp_ms: u64,
        payload: CommandPayload,
    ) -> Vec<u8> {
        let cmd = GatewayMessage::Command {
            starting_seq,
            timestamp_ms,
            payload,
        };
        let payload_cbor = cmd.encode().unwrap();
        let header = FrameHeader {
            key_hint,
            msg_type: MSG_COMMAND,
            nonce: echo_nonce,
        };
        encode_frame(&header, &payload_cbor, psk, &TestHmac).unwrap()
    }

    fn build_chunk_response(
        psk: &[u8; 32],
        key_hint: u16,
        echo_seq: u64,
        chunk_index: u32,
        chunk_data: &[u8],
    ) -> Vec<u8> {
        let msg = GatewayMessage::Chunk {
            chunk_index,
            chunk_data: chunk_data.to_vec(),
        };
        let payload_cbor = msg.encode().unwrap();
        let header = FrameHeader {
            key_hint,
            msg_type: MSG_CHUNK,
            nonce: echo_seq,
        };
        encode_frame(&header, &payload_cbor, psk, &TestHmac).unwrap()
    }

    fn build_app_data_reply(psk: &[u8; 32], key_hint: u16, echo_seq: u64, blob: &[u8]) -> Vec<u8> {
        let msg = GatewayMessage::AppDataReply {
            blob: blob.to_vec(),
        };
        let payload_cbor = msg.encode().unwrap();
        let header = FrameHeader {
            key_hint,
            msg_type: MSG_APP_DATA_REPLY,
            nonce: echo_seq,
        };
        encode_frame(&header, &payload_cbor, psk, &TestHmac).unwrap()
    }

    // --- Tests ---

    #[test]
    fn test_unpaired_node_returns_unpaired() {
        let mut transport = MockTransport::new();
        let mut storage = MockStorage::new(); // no key
        let mut hal = MockHal;
        let mut rng = MockRng(0);
        let clock = MockClock;
        let mut interp = MockBpfInterpreter::new();
        let mut map_storage = MapStorage::new(DEFAULT_MAP_BUDGET);

        let outcome = run_wake_cycle(
            &mut transport,
            &mut storage,
            &mut hal,
            &mut rng,
            &clock,
            &MockBattery,
            &mut interp,
            &mut map_storage,
            &TestHmac,
            &TestSha256,
        );
        assert_eq!(outcome, WakeCycleOutcome::Unpaired);
        // No frames should have been sent
        assert!(transport.outbound.is_empty());
    }

    #[test]
    fn test_wake_retries_exhausted() {
        let psk = [0xAA; 32];
        let mut transport = MockTransport::new();
        // Queue 4 timeouts (1 initial + 3 retries)
        for _ in 0..4 {
            transport.queue_response(None);
        }
        let mut storage = MockStorage::new().with_key(42, psk);
        let mut hal = MockHal;
        let mut rng = MockRng(0);
        let clock = MockClock;
        let mut interp = MockBpfInterpreter::new();
        let mut map_storage = MapStorage::new(DEFAULT_MAP_BUDGET);

        let outcome = run_wake_cycle(
            &mut transport,
            &mut storage,
            &mut hal,
            &mut rng,
            &clock,
            &MockBattery,
            &mut interp,
            &mut map_storage,
            &TestHmac,
            &TestSha256,
        );

        assert_eq!(outcome, WakeCycleOutcome::Sleep { seconds: 60 });
        // Should have sent 4 WAKE frames
        assert_eq!(transport.outbound.len(), 4);
    }

    #[test]
    fn test_normal_nop_wake_cycle() {
        let psk = [0xBB; 32];
        let key_hint = 7u16;
        let mut transport = MockTransport::new();

        // The WAKE nonce will be rng.random_u64() = 1 (MockRng starts at 0, +1)
        let command_frame = build_command_response(
            &psk,
            key_hint,
            1, // echo nonce
            1000,
            1710000000000,
            CommandPayload::Nop,
        );
        transport.queue_response(Some(command_frame));

        let mut storage = MockStorage::new().with_key(key_hint, psk);
        let mut hal = MockHal;
        let mut rng = MockRng(0);
        let clock = MockClock;
        let mut interp = MockBpfInterpreter::new();
        let mut map_storage = MapStorage::new(DEFAULT_MAP_BUDGET);

        let outcome = run_wake_cycle(
            &mut transport,
            &mut storage,
            &mut hal,
            &mut rng,
            &clock,
            &MockBattery,
            &mut interp,
            &mut map_storage,
            &TestHmac,
            &TestSha256,
        );

        assert_eq!(outcome, WakeCycleOutcome::Sleep { seconds: 60 });
        // Should have sent exactly 1 WAKE
        assert_eq!(transport.outbound.len(), 1);
    }

    #[test]
    fn test_reboot_command() {
        let psk = [0xCC; 32];
        let key_hint = 1u16;
        let mut transport = MockTransport::new();

        let command_frame = build_command_response(
            &psk,
            key_hint,
            1,
            1000,
            1710000000000,
            CommandPayload::Reboot,
        );
        transport.queue_response(Some(command_frame));

        let mut storage = MockStorage::new().with_key(key_hint, psk);
        let mut hal = MockHal;
        let mut rng = MockRng(0);
        let clock = MockClock;
        let mut interp = MockBpfInterpreter::new();
        let mut map_storage = MapStorage::new(DEFAULT_MAP_BUDGET);

        let outcome = run_wake_cycle(
            &mut transport,
            &mut storage,
            &mut hal,
            &mut rng,
            &clock,
            &MockBattery,
            &mut interp,
            &mut map_storage,
            &TestHmac,
            &TestSha256,
        );

        assert_eq!(outcome, WakeCycleOutcome::Reboot);
    }

    #[test]
    fn test_update_schedule() {
        let psk = [0xDD; 32];
        let key_hint = 1u16;
        let mut transport = MockTransport::new();

        let command_frame = build_command_response(
            &psk,
            key_hint,
            1,
            1000,
            1710000000000,
            CommandPayload::UpdateSchedule { interval_s: 120 },
        );
        transport.queue_response(Some(command_frame));

        let mut storage = MockStorage::new().with_key(key_hint, psk);
        let mut hal = MockHal;
        let mut rng = MockRng(0);
        let clock = MockClock;
        let mut interp = MockBpfInterpreter::new();
        let mut map_storage = MapStorage::new(DEFAULT_MAP_BUDGET);

        let outcome = run_wake_cycle(
            &mut transport,
            &mut storage,
            &mut hal,
            &mut rng,
            &clock,
            &MockBattery,
            &mut interp,
            &mut map_storage,
            &TestHmac,
            &TestSha256,
        );

        assert_eq!(outcome, WakeCycleOutcome::Sleep { seconds: 120 });
        assert_eq!(storage.schedule_interval, 120);
    }

    #[test]
    fn test_invalid_hmac_discarded() {
        let psk = [0xEE; 32];
        let wrong_psk = [0xFF; 32];
        let key_hint = 1u16;
        let mut transport = MockTransport::new();

        // Build a response signed with the wrong key
        let bad_frame = build_command_response(
            &wrong_psk,
            key_hint,
            1,
            1000,
            1710000000000,
            CommandPayload::Nop,
        );
        // Queue: bad response, then 3 more timeouts
        transport.queue_response(Some(bad_frame));
        transport.queue_response(None);
        transport.queue_response(None);
        transport.queue_response(None);

        let mut storage = MockStorage::new().with_key(key_hint, psk);
        let mut hal = MockHal;
        let mut rng = MockRng(0);
        let clock = MockClock;
        let mut interp = MockBpfInterpreter::new();
        let mut map_storage = MapStorage::new(DEFAULT_MAP_BUDGET);

        let outcome = run_wake_cycle(
            &mut transport,
            &mut storage,
            &mut hal,
            &mut rng,
            &clock,
            &MockBattery,
            &mut interp,
            &mut map_storage,
            &TestHmac,
            &TestSha256,
        );

        // Should exhaust retries after discarding the bad frame
        assert_eq!(outcome, WakeCycleOutcome::Sleep { seconds: 60 });
    }

    #[test]
    fn test_wrong_nonce_discarded() {
        let psk = [0x11; 32];
        let key_hint = 1u16;
        let mut transport = MockTransport::new();

        // Response echoes wrong nonce (99 instead of 1)
        let bad_frame =
            build_command_response(&psk, key_hint, 99, 1000, 1710000000000, CommandPayload::Nop);
        transport.queue_response(Some(bad_frame));
        transport.queue_response(None);
        transport.queue_response(None);
        transport.queue_response(None);

        let mut storage = MockStorage::new().with_key(key_hint, psk);
        let mut hal = MockHal;
        let mut rng = MockRng(0);
        let clock = MockClock;
        let mut interp = MockBpfInterpreter::new();
        let mut map_storage = MapStorage::new(DEFAULT_MAP_BUDGET);

        let outcome = run_wake_cycle(
            &mut transport,
            &mut storage,
            &mut hal,
            &mut rng,
            &clock,
            &MockBattery,
            &mut interp,
            &mut map_storage,
            &TestHmac,
            &TestSha256,
        );

        assert_eq!(outcome, WakeCycleOutcome::Sleep { seconds: 60 });
    }

    // --- Chunked transfer tests ---

    #[test]
    fn test_chunked_transfer_success() {
        let psk = [0x22; 32];
        let key_hint = 1u16;
        let mut transport = MockTransport::new();

        // Build a small program image
        let image = sonde_protocol::ProgramImage {
            bytecode: vec![0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
            maps: vec![],
        };
        let image_cbor = image.encode_deterministic().unwrap();
        let image_hash = TestSha256.hash(&image_cbor);

        let chunk_size = 10u32;
        let chunk_count =
            sonde_protocol::chunk_count(image_cbor.len(), chunk_size as usize).unwrap();

        // WAKE nonce will be 1
        let starting_seq = 5000u64;

        // Queue COMMAND response with UPDATE_PROGRAM
        let command_frame = build_command_response(
            &psk,
            key_hint,
            1,
            starting_seq,
            1710000000000,
            CommandPayload::UpdateProgram {
                program_hash: image_hash.to_vec(),
                program_size: image_cbor.len() as u32,
                chunk_size,
                chunk_count,
            },
        );
        transport.queue_response(Some(command_frame));

        // Queue CHUNK responses for each chunk
        for i in 0..chunk_count {
            let chunk_data = sonde_protocol::get_chunk(&image_cbor, i, chunk_size)
                .unwrap()
                .to_vec();
            let seq = starting_seq + i as u64;
            let chunk_frame = build_chunk_response(&psk, key_hint, seq, i, &chunk_data);
            transport.queue_response(Some(chunk_frame));
        }

        let mut storage = MockStorage::new().with_key(key_hint, psk);
        let mut hal = MockHal;
        let mut rng = MockRng(0);
        let clock = MockClock;
        let mut interp = MockBpfInterpreter::new();
        let mut map_storage = MapStorage::new(DEFAULT_MAP_BUDGET);

        let outcome = run_wake_cycle(
            &mut transport,
            &mut storage,
            &mut hal,
            &mut rng,
            &clock,
            &MockBattery,
            &mut interp,
            &mut map_storage,
            &TestHmac,
            &TestSha256,
        );

        assert_eq!(outcome, WakeCycleOutcome::Sleep { seconds: 60 });
        // Should have sent: 1 WAKE + chunk_count GET_CHUNKs + 1 PROGRAM_ACK
        assert_eq!(transport.outbound.len(), 1 + chunk_count as usize + 1);
        // Program should be installed on the inactive partition (1)
        assert!(storage.read_program(1).is_some());
        assert_eq!(storage.active_partition, 1);
        // BPF interpreter should have been loaded and executed
        assert!(interp.loaded);
    }

    #[test]
    fn test_chunked_transfer_chunk_retry_exhausted() {
        let psk = [0x33; 32];
        let key_hint = 1u16;
        let mut transport = MockTransport::new();

        let starting_seq = 100u64;
        let command_frame = build_command_response(
            &psk,
            key_hint,
            1,
            starting_seq,
            1710000000000,
            CommandPayload::UpdateProgram {
                program_hash: vec![0xAA; 32],
                program_size: 20,
                chunk_size: 10,
                chunk_count: 2,
            },
        );
        transport.queue_response(Some(command_frame));
        // No chunk responses — all 4 attempts (1 + 3 retries) will timeout
        for _ in 0..4 {
            transport.queue_response(None);
        }

        let mut storage = MockStorage::new().with_key(key_hint, psk);
        let mut hal = MockHal;
        let mut rng = MockRng(0);
        let clock = MockClock;
        let mut interp = MockBpfInterpreter::new();
        let mut map_storage = MapStorage::new(DEFAULT_MAP_BUDGET);

        let outcome = run_wake_cycle(
            &mut transport,
            &mut storage,
            &mut hal,
            &mut rng,
            &clock,
            &MockBattery,
            &mut interp,
            &mut map_storage,
            &TestHmac,
            &TestSha256,
        );

        // Should sleep after exhausting chunk retries
        assert_eq!(outcome, WakeCycleOutcome::Sleep { seconds: 60 });
        // BPF should NOT have executed
        assert!(!interp.loaded);
    }

    #[test]
    fn test_chunked_transfer_wrong_chunk_index() {
        let psk = [0x44; 32];
        let key_hint = 1u16;
        let mut transport = MockTransport::new();

        let starting_seq = 200u64;
        let command_frame = build_command_response(
            &psk,
            key_hint,
            1,
            starting_seq,
            1710000000000,
            CommandPayload::UpdateProgram {
                program_hash: vec![0xBB; 32],
                program_size: 10,
                chunk_size: 10,
                chunk_count: 1,
            },
        );
        transport.queue_response(Some(command_frame));

        // Respond with wrong chunk index (5 instead of 0), then timeout the rest
        let bad_chunk = build_chunk_response(&psk, key_hint, starting_seq, 5, &[0u8; 10]);
        transport.queue_response(Some(bad_chunk));
        transport.queue_response(None);
        transport.queue_response(None);
        transport.queue_response(None);

        let mut storage = MockStorage::new().with_key(key_hint, psk);
        let mut hal = MockHal;
        let mut rng = MockRng(0);
        let clock = MockClock;
        let mut interp = MockBpfInterpreter::new();
        let mut map_storage = MapStorage::new(DEFAULT_MAP_BUDGET);

        let outcome = run_wake_cycle(
            &mut transport,
            &mut storage,
            &mut hal,
            &mut rng,
            &clock,
            &MockBattery,
            &mut interp,
            &mut map_storage,
            &TestHmac,
            &TestSha256,
        );

        assert_eq!(outcome, WakeCycleOutcome::Sleep { seconds: 60 });
    }

    // --- send_recv_app_data tests ---

    #[test]
    fn test_send_recv_app_data_success() {
        let psk = [0x55; 32];
        let key_hint = 1u16;
        let mut transport = MockTransport::new();

        // Queue reply
        let reply = build_app_data_reply(&psk, key_hint, 42, &[0xCC, 0xDD]);
        transport.queue_response(Some(reply));

        let identity = NodeIdentity { key_hint, psk };
        let mut seq = 42u64;
        let result = send_recv_app_data(
            &mut transport,
            &identity,
            &mut seq,
            &[0xAA, 0xBB],
            RESPONSE_TIMEOUT_MS,
            &MockClock,
            &TestHmac,
        );

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), vec![0xCC, 0xDD]);
        assert_eq!(seq, 43); // seq incremented
    }

    #[test]
    fn test_send_recv_app_data_timeout() {
        let psk = [0x66; 32];
        let key_hint = 1u16;
        let mut transport = MockTransport::new();
        transport.queue_response(None); // timeout

        let identity = NodeIdentity { key_hint, psk };
        let mut seq = 10u64;
        let result = send_recv_app_data(
            &mut transport,
            &identity,
            &mut seq,
            &[0x01],
            RESPONSE_TIMEOUT_MS,
            &MockClock,
            &TestHmac,
        );

        assert!(matches!(result, Err(NodeError::Timeout)));
        assert_eq!(seq, 11); // seq still incremented
    }

    #[test]
    fn test_send_recv_app_data_wrong_nonce() {
        let psk = [0x77; 32];
        let key_hint = 1u16;
        let mut transport = MockTransport::new();

        // Reply echoes wrong seq (99 instead of 20) — silently discarded
        let reply = build_app_data_reply(&psk, key_hint, 99, &[0x01]);
        transport.queue_response(Some(reply));
        // Next recv returns None → timeout
        transport.queue_response(None);

        let identity = NodeIdentity { key_hint, psk };
        let mut seq = 20u64;
        let result = send_recv_app_data(
            &mut transport,
            &identity,
            &mut seq,
            &[0x01],
            RESPONSE_TIMEOUT_MS,
            &MockClock,
            &TestHmac,
        );

        // Wrong-nonce frame is discarded per ND-0800/ND-0801; falls through to timeout
        assert!(matches!(result, Err(NodeError::Timeout)));
    }

    // ===================================================================
    // Protocol conformance tests (T-N101, T-N102, T-N300, T-N103, T-N104)
    // ===================================================================

    #[test]
    fn test_wake_cbor_integer_keys() {
        // T-N101: WAKE CBOR payload uses integer keys per protocol spec.
        let psk = [0xA1; 32];
        let key_hint = 1u16;
        let mut transport = MockTransport::new();
        let command_frame =
            build_command_response(&psk, key_hint, 1, 1000, 1710000000000, CommandPayload::Nop);
        transport.queue_response(Some(command_frame));

        let mut storage = MockStorage::new().with_key(key_hint, psk);
        let mut hal = MockHal;
        let mut rng = MockRng(0);
        let clock = MockClock;
        let mut interp = MockBpfInterpreter::new();
        let mut map_storage = MapStorage::new(DEFAULT_MAP_BUDGET);

        run_wake_cycle(
            &mut transport,
            &mut storage,
            &mut hal,
            &mut rng,
            &clock,
            &MockBattery,
            &mut interp,
            &mut map_storage,
            &TestHmac,
            &TestSha256,
        );

        // Decode the WAKE frame's CBOR payload
        assert!(!transport.outbound.is_empty());
        let decoded = decode_frame(&transport.outbound[0]).unwrap();
        assert_eq!(decoded.header.msg_type, MSG_WAKE);

        let cbor_value: ciborium::Value =
            ciborium::from_reader(decoded.payload.as_slice()).unwrap();
        match cbor_value {
            ciborium::Value::Map(pairs) => {
                assert!(!pairs.is_empty());
                for (key, _) in &pairs {
                    assert!(
                        key.as_integer().is_some(),
                        "CBOR key must be an integer, got: {:?}",
                        key
                    );
                }
            }
            _ => panic!("WAKE payload must be a CBOR map"),
        }
    }

    #[test]
    fn test_outbound_frame_format() {
        // T-N102 + T-N300: Outbound frame structure: 11B header + CBOR + 32B HMAC,
        // total ≤ 250B, with valid HMAC over header+payload.
        let psk = [0xA2; 32];
        let key_hint = 42u16;
        let mut transport = MockTransport::new();
        let command_frame =
            build_command_response(&psk, key_hint, 1, 1000, 1710000000000, CommandPayload::Nop);
        transport.queue_response(Some(command_frame));

        let mut storage = MockStorage::new().with_key(key_hint, psk);
        let mut hal = MockHal;
        let mut rng = MockRng(0);
        let clock = MockClock;
        let mut interp = MockBpfInterpreter::new();
        let mut map_storage = MapStorage::new(DEFAULT_MAP_BUDGET);

        run_wake_cycle(
            &mut transport,
            &mut storage,
            &mut hal,
            &mut rng,
            &clock,
            &MockBattery,
            &mut interp,
            &mut map_storage,
            &TestHmac,
            &TestSha256,
        );

        let wake_frame = &transport.outbound[0];

        // Total ≤ MAX_FRAME_SIZE (250)
        assert!(wake_frame.len() <= sonde_protocol::MAX_FRAME_SIZE);
        // At least MIN_FRAME_SIZE (43 = 11 header + 32 HMAC)
        assert!(wake_frame.len() >= sonde_protocol::MIN_FRAME_SIZE);

        // bytes 0-1: key_hint (big-endian)
        let frame_key_hint = u16::from_be_bytes([wake_frame[0], wake_frame[1]]);
        assert_eq!(frame_key_hint, key_hint);

        // byte 2: msg_type = MSG_WAKE (0x01)
        assert_eq!(wake_frame[2], MSG_WAKE);

        // bytes 3-10: nonce (8 bytes, big-endian) — must be non-zero
        let nonce = u64::from_be_bytes(wake_frame[3..11].try_into().unwrap());
        assert_ne!(nonce, 0);

        // last 32 bytes: valid HMAC-SHA256 over header+payload
        let hmac_start = wake_frame.len() - sonde_protocol::HMAC_SIZE;
        let data_portion = &wake_frame[..hmac_start];
        let expected_hmac = TestHmac.compute(&psk, data_portion);
        assert_eq!(&wake_frame[hmac_start..], &expected_hmac);
    }

    #[test]
    fn test_send_app_data_max_blob() {
        // T-N103: Maximum blob that fits within frame payload budget.
        // CBOR overhead for AppData{blob} with blob in [24,255]:
        //   1 (map) + 1 (key) + 2 (bytes header) + N = 4 + N
        // MAX_PAYLOAD_SIZE = 207, so max blob = 203 bytes.
        let psk = [0xA3; 32];
        let key_hint = 1u16;
        let mut transport = MockTransport::new();
        let identity = NodeIdentity { key_hint, psk };
        let mut seq = 100u64;

        let blob = vec![0xAB; 203];
        let result = send_app_data(&mut transport, &identity, &mut seq, &blob, &TestHmac);
        assert!(result.is_ok());
        assert_eq!(seq, 101);
        assert!(!transport.outbound.is_empty());
        assert!(transport.outbound[0].len() <= sonde_protocol::MAX_FRAME_SIZE);
    }

    #[test]
    fn test_send_app_data_oversized_blob() {
        // T-N104: Blob exceeding frame payload budget → rejected.
        let psk = [0xA4; 32];
        let key_hint = 1u16;
        let mut transport = MockTransport::new();
        let identity = NodeIdentity { key_hint, psk };
        let mut seq = 100u64;

        let blob = vec![0xCD; sonde_protocol::MAX_PAYLOAD_SIZE + 1];
        let result = send_app_data(&mut transport, &identity, &mut seq, &blob, &TestHmac);
        assert!(result.is_err());
        assert_eq!(seq, 100); // not advanced
        assert!(transport.outbound.is_empty()); // no frame sent
    }

    // ===================================================================
    // Wake cycle integration tests (T-N202, T-N203, T-N207, T-N507)
    // ===================================================================

    #[test]
    fn test_wake_message_fields() {
        // T-N202: WAKE fields match: firmware_abi_version, program_hash, battery_mv.
        let psk = [0xB2; 32];
        let key_hint = 5u16;
        let mut transport = MockTransport::new();
        let command_frame =
            build_command_response(&psk, key_hint, 1, 1000, 1710000000000, CommandPayload::Nop);
        transport.queue_response(Some(command_frame));

        let mut storage = MockStorage::new().with_key(key_hint, psk);
        let mut hal = MockHal;
        let mut rng = MockRng(0);
        let clock = MockClock;
        let mut interp = MockBpfInterpreter::new();
        let mut map_storage = MapStorage::new(DEFAULT_MAP_BUDGET);

        run_wake_cycle(
            &mut transport,
            &mut storage,
            &mut hal,
            &mut rng,
            &clock,
            &MockBattery,
            &mut interp,
            &mut map_storage,
            &TestHmac,
            &TestSha256,
        );

        let decoded = decode_frame(&transport.outbound[0]).unwrap();
        let wake_msg = NodeMessage::decode(decoded.header.msg_type, &decoded.payload).unwrap();
        match wake_msg {
            NodeMessage::Wake {
                firmware_abi_version,
                program_hash,
                battery_mv,
            } => {
                assert_eq!(firmware_abi_version, FIRMWARE_ABI_VERSION);
                assert_eq!(battery_mv, 3300); // MockBattery
                                              // No program installed → empty hash
                assert!(program_hash.is_empty());
            }
            _ => panic!("expected Wake message"),
        }
    }

    #[test]
    fn test_no_program_empty_hash() {
        // T-N203: No resident program → program_hash is empty in WAKE.
        let psk = [0xB3; 32];
        let key_hint = 1u16;
        let mut transport = MockTransport::new();
        // Exhaust retries — we only need the outbound WAKE frame
        for _ in 0..4 {
            transport.queue_response(None);
        }

        let mut storage = MockStorage::new().with_key(key_hint, psk);
        let mut hal = MockHal;
        let mut rng = MockRng(0);
        let clock = MockClock;
        let mut interp = MockBpfInterpreter::new();
        let mut map_storage = MapStorage::new(DEFAULT_MAP_BUDGET);

        run_wake_cycle(
            &mut transport,
            &mut storage,
            &mut hal,
            &mut rng,
            &clock,
            &MockBattery,
            &mut interp,
            &mut map_storage,
            &TestHmac,
            &TestSha256,
        );

        let decoded = decode_frame(&transport.outbound[0]).unwrap();
        let wake_msg = NodeMessage::decode(decoded.header.msg_type, &decoded.payload).unwrap();
        match wake_msg {
            NodeMessage::Wake { program_hash, .. } => {
                assert!(
                    program_hash.is_empty(),
                    "no program should yield empty hash"
                );
            }
            _ => panic!("expected Wake message"),
        }
    }

    #[test]
    fn test_unknown_command_treated_as_nop() {
        // T-N207: Unknown command_type in COMMAND → treated as NOP.
        let psk = [0xC7; 32];
        let key_hint = 1u16;
        let mut transport = MockTransport::new();

        // Build COMMAND frame with unknown command_type (0xFE)
        let cbor_map = ciborium::Value::Map(vec![
            (
                ciborium::Value::Integer(4.into()),
                ciborium::Value::Integer(0xFE.into()),
            ),
            (
                ciborium::Value::Integer(5.into()),
                ciborium::Value::Map(vec![]),
            ),
            (
                ciborium::Value::Integer(13.into()),
                ciborium::Value::Integer(1000.into()),
            ),
            (
                ciborium::Value::Integer(14.into()),
                ciborium::Value::Integer(1710000000000u64.try_into().unwrap()),
            ),
        ]);
        let mut payload_cbor = Vec::new();
        ciborium::into_writer(&cbor_map, &mut payload_cbor).unwrap();
        let header = FrameHeader {
            key_hint,
            msg_type: MSG_COMMAND,
            nonce: 1,
        };
        let command_frame = encode_frame(&header, &payload_cbor, &psk, &TestHmac).unwrap();
        transport.queue_response(Some(command_frame));

        let mut storage = MockStorage::new().with_key(key_hint, psk);
        let mut hal = MockHal;
        let mut rng = MockRng(0);
        let clock = MockClock;
        let mut interp = MockBpfInterpreter::new();
        let mut map_storage = MapStorage::new(DEFAULT_MAP_BUDGET);

        let outcome = run_wake_cycle(
            &mut transport,
            &mut storage,
            &mut hal,
            &mut rng,
            &clock,
            &MockBattery,
            &mut interp,
            &mut map_storage,
            &TestHmac,
            &TestSha256,
        );

        // Treated as NOP → normal sleep (no crash, no reboot)
        assert_eq!(outcome, WakeCycleOutcome::Sleep { seconds: 60 });
    }

    #[test]
    fn test_execution_context_fields() {
        // T-N507: BPF context: timestamp, battery_mv, firmware_abi_version, wake_reason.
        let psk = [0xF7; 32];
        let key_hint = 1u16;
        let mut transport = MockTransport::new();
        let timestamp_ms = 1710000000000u64;
        let command_frame =
            build_command_response(&psk, key_hint, 1, 1000, timestamp_ms, CommandPayload::Nop);
        transport.queue_response(Some(command_frame));

        // Install a resident program so BPF execution happens
        let image = sonde_protocol::ProgramImage {
            bytecode: vec![0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
            maps: vec![],
        };
        let image_cbor = image.encode_deterministic().unwrap();

        let mut storage = MockStorage::new().with_key(key_hint, psk);
        storage.programs[0] = Some(image_cbor);

        let mut hal = MockHal;
        let mut rng = MockRng(0);
        let clock = MockClock; // elapsed_ms always returns 100
        let mut interp = MockBpfInterpreter::new();
        let mut map_storage = MapStorage::new(DEFAULT_MAP_BUDGET);

        run_wake_cycle(
            &mut transport,
            &mut storage,
            &mut hal,
            &mut rng,
            &clock,
            &MockBattery,
            &mut interp,
            &mut map_storage,
            &TestHmac,
            &TestSha256,
        );

        assert!(interp.executed);
        let ctx = interp.captured_ctx.expect("BPF context should be captured");

        // timestamp = gateway_timestamp_ms + elapsed_since_command
        // MockClock always returns 100, so elapsed_since_command = 0
        assert_eq!(ctx.timestamp, timestamp_ms);
        assert_eq!(ctx.battery_mv, 3300); // MockBattery
        assert_eq!(ctx.firmware_abi_version, FIRMWARE_ABI_VERSION as u16);
        assert_eq!(ctx.wake_reason, WakeReason::Scheduled as u8);
    }

    // ===================================================================
    // Auth & replay protection tests (T-N304, T-N305, T-N306)
    // ===================================================================

    #[test]
    fn test_wrong_seq_on_chunk_discarded() {
        // T-N304: CHUNK with wrong echoed seq → discarded, retry exhausted.
        let psk = [0xD4; 32];
        let key_hint = 1u16;
        let mut transport = MockTransport::new();

        let starting_seq = 500u64;
        let command_frame = build_command_response(
            &psk,
            key_hint,
            1,
            starting_seq,
            1710000000000,
            CommandPayload::UpdateProgram {
                program_hash: vec![0xAA; 32],
                program_size: 10,
                chunk_size: 10,
                chunk_count: 1,
            },
        );
        transport.queue_response(Some(command_frame));

        // CHUNK with wrong seq (999 instead of starting_seq)
        let bad_chunk = build_chunk_response(&psk, key_hint, 999, 0, &[0u8; 10]);
        transport.queue_response(Some(bad_chunk));
        // Remaining retries timeout
        transport.queue_response(None);
        transport.queue_response(None);
        transport.queue_response(None);

        let mut storage = MockStorage::new().with_key(key_hint, psk);
        let mut hal = MockHal;
        let mut rng = MockRng(0);
        let clock = MockClock;
        let mut interp = MockBpfInterpreter::new();
        let mut map_storage = MapStorage::new(DEFAULT_MAP_BUDGET);

        let outcome = run_wake_cycle(
            &mut transport,
            &mut storage,
            &mut hal,
            &mut rng,
            &clock,
            &MockBattery,
            &mut interp,
            &mut map_storage,
            &TestHmac,
            &TestSha256,
        );

        assert_eq!(outcome, WakeCycleOutcome::Sleep { seconds: 60 });
        assert!(!interp.loaded);
    }

    #[test]
    fn test_sequence_increment_correctness() {
        // T-N305: Sequence numbers increment correctly across GET_CHUNK + PROGRAM_ACK.
        let psk = [0xD5; 32];
        let key_hint = 1u16;
        let mut transport = MockTransport::new();

        let image = sonde_protocol::ProgramImage {
            bytecode: vec![0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
            maps: vec![],
        };
        let image_cbor = image.encode_deterministic().unwrap();
        let image_hash = TestSha256.hash(&image_cbor);
        let chunk_size = 10u32;
        let chunk_count =
            sonde_protocol::chunk_count(image_cbor.len(), chunk_size as usize).unwrap();
        assert!(chunk_count >= 1);

        let starting_seq = 1000u64;
        let command_frame = build_command_response(
            &psk,
            key_hint,
            1,
            starting_seq,
            1710000000000,
            CommandPayload::UpdateProgram {
                program_hash: image_hash.to_vec(),
                program_size: image_cbor.len() as u32,
                chunk_size,
                chunk_count,
            },
        );
        transport.queue_response(Some(command_frame));

        for i in 0..chunk_count {
            let chunk_data = sonde_protocol::get_chunk(&image_cbor, i, chunk_size)
                .unwrap()
                .to_vec();
            let seq = starting_seq + i as u64;
            let chunk_frame = build_chunk_response(&psk, key_hint, seq, i, &chunk_data);
            transport.queue_response(Some(chunk_frame));
        }

        let mut storage = MockStorage::new().with_key(key_hint, psk);
        let mut hal = MockHal;
        let mut rng = MockRng(0);
        let clock = MockClock;
        let mut interp = MockBpfInterpreter::new();
        let mut map_storage = MapStorage::new(DEFAULT_MAP_BUDGET);

        let outcome = run_wake_cycle(
            &mut transport,
            &mut storage,
            &mut hal,
            &mut rng,
            &clock,
            &MockBattery,
            &mut interp,
            &mut map_storage,
            &TestHmac,
            &TestSha256,
        );

        assert_eq!(outcome, WakeCycleOutcome::Sleep { seconds: 60 });

        // Verify seq on each outbound frame (skip WAKE at index 0)
        for i in 0..chunk_count {
            let frame = &transport.outbound[1 + i as usize];
            let decoded = decode_frame(frame).unwrap();
            assert_eq!(decoded.header.msg_type, MSG_GET_CHUNK);
            assert_eq!(
                decoded.header.nonce,
                starting_seq + i as u64,
                "GET_CHUNK {} should use seq {}",
                i,
                starting_seq + i as u64
            );
        }

        // PROGRAM_ACK should use seq = starting_seq + chunk_count
        let ack_frame = &transport.outbound[1 + chunk_count as usize];
        let decoded_ack = decode_frame(ack_frame).unwrap();
        assert_eq!(decoded_ack.header.msg_type, MSG_PROGRAM_ACK);
        assert_eq!(
            decoded_ack.header.nonce,
            starting_seq + chunk_count as u64,
            "PROGRAM_ACK should use seq {}",
            starting_seq + chunk_count as u64
        );
    }

    #[test]
    fn test_nonce_uniqueness_across_cycles() {
        // T-N306: 1000 wake cycles produce unique WAKE nonces.
        let psk = [0xD6; 32];
        let key_hint = 1u16;
        let mut nonces = std::collections::HashSet::new();

        for cycle in 0..1000u64 {
            let mut transport = MockTransport::new();
            for _ in 0..4 {
                transport.queue_response(None);
            }

            let mut storage = MockStorage::new().with_key(key_hint, psk);
            let mut hal = MockHal;
            let mut rng = MockRng(cycle);
            let clock = MockClock;
            let mut interp = MockBpfInterpreter::new();
            let mut map_storage = MapStorage::new(DEFAULT_MAP_BUDGET);

            run_wake_cycle(
                &mut transport,
                &mut storage,
                &mut hal,
                &mut rng,
                &clock,
                &MockBattery,
                &mut interp,
                &mut map_storage,
                &TestHmac,
                &TestSha256,
            );

            let decoded = decode_frame(&transport.outbound[0]).unwrap();
            assert!(
                nonces.insert(decoded.header.nonce),
                "duplicate nonce at cycle {}",
                cycle
            );
        }
        assert_eq!(nonces.len(), 1000);
    }

    // ===================================================================
    // Program transfer & execution tests (T-N502, T-N505, T-N508–T-N510)
    // ===================================================================

    #[test]
    fn test_program_transfer_hash_mismatch() {
        // T-N502: Hash mismatch after transfer → discard, sleep; no PROGRAM_ACK.
        let psk = [0xE2; 32];
        let key_hint = 1u16;
        let mut transport = MockTransport::new();

        let image = sonde_protocol::ProgramImage {
            bytecode: vec![0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
            maps: vec![],
        };
        let image_cbor = image.encode_deterministic().unwrap();
        let wrong_hash = [0xFF; 32];
        let chunk_size = image_cbor.len() as u32;
        let chunk_count = 1u32;
        let starting_seq = 100u64;

        let command_frame = build_command_response(
            &psk,
            key_hint,
            1,
            starting_seq,
            1710000000000,
            CommandPayload::UpdateProgram {
                program_hash: wrong_hash.to_vec(),
                program_size: image_cbor.len() as u32,
                chunk_size,
                chunk_count,
            },
        );
        transport.queue_response(Some(command_frame));

        let chunk_frame = build_chunk_response(&psk, key_hint, starting_seq, 0, &image_cbor);
        transport.queue_response(Some(chunk_frame));

        let mut storage = MockStorage::new().with_key(key_hint, psk);
        let mut hal = MockHal;
        let mut rng = MockRng(0);
        let clock = MockClock;
        let mut interp = MockBpfInterpreter::new();
        let mut map_storage = MapStorage::new(DEFAULT_MAP_BUDGET);

        let outcome = run_wake_cycle(
            &mut transport,
            &mut storage,
            &mut hal,
            &mut rng,
            &clock,
            &MockBattery,
            &mut interp,
            &mut map_storage,
            &TestHmac,
            &TestSha256,
        );

        assert_eq!(outcome, WakeCycleOutcome::Sleep { seconds: 60 });
        // 1 WAKE + 1 GET_CHUNK — no PROGRAM_ACK
        assert_eq!(transport.outbound.len(), 2);
        let last_decoded = decode_frame(&transport.outbound[1]).unwrap();
        assert_eq!(last_decoded.header.msg_type, MSG_GET_CHUNK);
        assert!(!interp.loaded);
    }

    #[test]
    fn test_ephemeral_program_integration() {
        // T-N505: RUN_EPHEMERAL through wake cycle: executes in RAM,
        // resident program unaffected.
        let psk = [0xE5; 32];
        let key_hint = 1u16;
        let mut transport = MockTransport::new();

        let image = sonde_protocol::ProgramImage {
            bytecode: vec![0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
            maps: vec![],
        };
        let image_cbor = image.encode_deterministic().unwrap();
        let image_hash = TestSha256.hash(&image_cbor);
        let chunk_size = image_cbor.len() as u32;
        let chunk_count = 1u32;
        let starting_seq = 200u64;

        let command_frame = build_command_response(
            &psk,
            key_hint,
            1,
            starting_seq,
            1710000000000,
            CommandPayload::RunEphemeral {
                program_hash: image_hash.to_vec(),
                program_size: image_cbor.len() as u32,
                chunk_size,
                chunk_count,
            },
        );
        transport.queue_response(Some(command_frame));
        let chunk_frame = build_chunk_response(&psk, key_hint, starting_seq, 0, &image_cbor);
        transport.queue_response(Some(chunk_frame));

        let mut storage = MockStorage::new().with_key(key_hint, psk);
        let mut hal = MockHal;
        let mut rng = MockRng(0);
        let clock = MockClock;
        let mut interp = MockBpfInterpreter::new();
        let mut map_storage = MapStorage::new(DEFAULT_MAP_BUDGET);

        let outcome = run_wake_cycle(
            &mut transport,
            &mut storage,
            &mut hal,
            &mut rng,
            &clock,
            &MockBattery,
            &mut interp,
            &mut map_storage,
            &TestHmac,
            &TestSha256,
        );

        assert_eq!(outcome, WakeCycleOutcome::Sleep { seconds: 60 });
        assert!(interp.loaded);
        assert!(interp.executed);
        // Resident program partitions NOT modified
        assert!(storage.programs[0].is_none());
        assert!(storage.programs[1].is_none());
        assert_eq!(storage.active_partition, 0);
    }

    #[test]
    fn test_wake_reason_program_update() {
        // T-N508: After resident program install, BPF context wake_reason = ProgramUpdate.
        let psk = [0xF8; 32];
        let key_hint = 1u16;
        let mut transport = MockTransport::new();

        let image = sonde_protocol::ProgramImage {
            bytecode: vec![0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
            maps: vec![],
        };
        let image_cbor = image.encode_deterministic().unwrap();
        let image_hash = TestSha256.hash(&image_cbor);
        let chunk_size = image_cbor.len() as u32;
        let chunk_count = 1u32;
        let starting_seq = 300u64;

        let command_frame = build_command_response(
            &psk,
            key_hint,
            1,
            starting_seq,
            1710000000000,
            CommandPayload::UpdateProgram {
                program_hash: image_hash.to_vec(),
                program_size: image_cbor.len() as u32,
                chunk_size,
                chunk_count,
            },
        );
        transport.queue_response(Some(command_frame));
        let chunk_frame = build_chunk_response(&psk, key_hint, starting_seq, 0, &image_cbor);
        transport.queue_response(Some(chunk_frame));

        let mut storage = MockStorage::new().with_key(key_hint, psk);
        let mut hal = MockHal;
        let mut rng = MockRng(0);
        let clock = MockClock;
        let mut interp = MockBpfInterpreter::new();
        let mut map_storage = MapStorage::new(DEFAULT_MAP_BUDGET);

        let outcome = run_wake_cycle(
            &mut transport,
            &mut storage,
            &mut hal,
            &mut rng,
            &clock,
            &MockBattery,
            &mut interp,
            &mut map_storage,
            &TestHmac,
            &TestSha256,
        );

        assert_eq!(outcome, WakeCycleOutcome::Sleep { seconds: 60 });
        assert!(interp.executed);
        let ctx = interp.captured_ctx.expect("BPF context should be captured");
        assert_eq!(ctx.wake_reason, WakeReason::ProgramUpdate as u8);
    }

    #[test]
    fn test_wake_reason_early() {
        // T-N509: When early_wake_flag is set, BPF context wake_reason = Early.
        let psk = [0xF9; 32];
        let key_hint = 1u16;
        let mut transport = MockTransport::new();

        let command_frame =
            build_command_response(&psk, key_hint, 1, 1000, 1710000000000, CommandPayload::Nop);
        transport.queue_response(Some(command_frame));

        let mut storage = MockStorage::new().with_key(key_hint, psk);
        storage.early_wake_flag = true;

        // Install a resident program so BPF execution happens
        let image = sonde_protocol::ProgramImage {
            bytecode: vec![0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
            maps: vec![],
        };
        let image_cbor = image.encode_deterministic().unwrap();
        storage.programs[0] = Some(image_cbor);

        let mut hal = MockHal;
        let mut rng = MockRng(0);
        let clock = MockClock;
        let mut interp = MockBpfInterpreter::new();
        let mut map_storage = MapStorage::new(DEFAULT_MAP_BUDGET);

        let outcome = run_wake_cycle(
            &mut transport,
            &mut storage,
            &mut hal,
            &mut rng,
            &clock,
            &MockBattery,
            &mut interp,
            &mut map_storage,
            &TestHmac,
            &TestSha256,
        );

        assert_eq!(outcome, WakeCycleOutcome::Sleep { seconds: 60 });
        assert!(interp.executed);
        let ctx = interp.captured_ctx.expect("BPF context should be captured");
        assert_eq!(ctx.wake_reason, WakeReason::Early as u8);
    }

    #[test]
    fn test_post_update_immediate_execution() {
        // T-N510: New program executes in same wake cycle after install.
        let psk = [0xFA; 32];
        let key_hint = 1u16;
        let mut transport = MockTransport::new();

        let image = sonde_protocol::ProgramImage {
            bytecode: vec![0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
            maps: vec![],
        };
        let image_cbor = image.encode_deterministic().unwrap();
        let image_hash = TestSha256.hash(&image_cbor);
        let chunk_size = image_cbor.len() as u32;
        let chunk_count = 1u32;
        let starting_seq = 400u64;

        let command_frame = build_command_response(
            &psk,
            key_hint,
            1,
            starting_seq,
            1710000000000,
            CommandPayload::UpdateProgram {
                program_hash: image_hash.to_vec(),
                program_size: image_cbor.len() as u32,
                chunk_size,
                chunk_count,
            },
        );
        transport.queue_response(Some(command_frame));
        let chunk_frame = build_chunk_response(&psk, key_hint, starting_seq, 0, &image_cbor);
        transport.queue_response(Some(chunk_frame));

        let mut storage = MockStorage::new().with_key(key_hint, psk);
        let mut hal = MockHal;
        let mut rng = MockRng(0);
        let clock = MockClock;
        let mut interp = MockBpfInterpreter::new();
        let mut map_storage = MapStorage::new(DEFAULT_MAP_BUDGET);

        let outcome = run_wake_cycle(
            &mut transport,
            &mut storage,
            &mut hal,
            &mut rng,
            &clock,
            &MockBattery,
            &mut interp,
            &mut map_storage,
            &TestHmac,
            &TestSha256,
        );

        assert_eq!(outcome, WakeCycleOutcome::Sleep { seconds: 60 });
        assert!(interp.loaded, "program should be loaded");
        assert!(
            interp.executed,
            "program should execute immediately after install"
        );
        assert!(storage.programs[1].is_some());
        assert_eq!(storage.active_partition, 1);
    }

    // ===================================================================
    // Error handling tests (T-N800, T-N801)
    // ===================================================================

    #[test]
    fn test_malformed_cbor_discarded() {
        // T-N800: Valid HMAC frame with garbage CBOR → discarded, no crash.
        let psk = [0xF0; 32];
        let key_hint = 1u16;
        let mut transport = MockTransport::new();

        // Build frame with valid HMAC but invalid CBOR payload
        let garbage_payload = vec![0xFF, 0xFE, 0xFD, 0xFC];
        let header = FrameHeader {
            key_hint,
            msg_type: MSG_COMMAND,
            nonce: 1,
        };
        let bad_frame = encode_frame(&header, &garbage_payload, &psk, &TestHmac).unwrap();
        transport.queue_response(Some(bad_frame));
        // Remaining retries timeout
        transport.queue_response(None);
        transport.queue_response(None);
        transport.queue_response(None);

        let mut storage = MockStorage::new().with_key(key_hint, psk);
        let mut hal = MockHal;
        let mut rng = MockRng(0);
        let clock = MockClock;
        let mut interp = MockBpfInterpreter::new();
        let mut map_storage = MapStorage::new(DEFAULT_MAP_BUDGET);

        let outcome = run_wake_cycle(
            &mut transport,
            &mut storage,
            &mut hal,
            &mut rng,
            &clock,
            &MockBattery,
            &mut interp,
            &mut map_storage,
            &TestHmac,
            &TestSha256,
        );

        assert_eq!(outcome, WakeCycleOutcome::Sleep { seconds: 60 });
        assert!(!interp.loaded);
    }

    #[test]
    fn test_unexpected_msg_type_discarded() {
        // T-N801: Frame with wrong msg_type (CHUNK when expecting COMMAND) → discarded.
        let psk = [0xF1; 32];
        let key_hint = 1u16;
        let mut transport = MockTransport::new();

        let msg = GatewayMessage::Chunk {
            chunk_index: 0,
            chunk_data: vec![0x01, 0x02],
        };
        let payload_cbor = msg.encode().unwrap();
        let header = FrameHeader {
            key_hint,
            msg_type: MSG_CHUNK,
            nonce: 1,
        };
        let bad_frame = encode_frame(&header, &payload_cbor, &psk, &TestHmac).unwrap();
        transport.queue_response(Some(bad_frame));
        transport.queue_response(None);
        transport.queue_response(None);
        transport.queue_response(None);

        let mut storage = MockStorage::new().with_key(key_hint, psk);
        let mut hal = MockHal;
        let mut rng = MockRng(0);
        let clock = MockClock;
        let mut interp = MockBpfInterpreter::new();
        let mut map_storage = MapStorage::new(DEFAULT_MAP_BUDGET);

        let outcome = run_wake_cycle(
            &mut transport,
            &mut storage,
            &mut hal,
            &mut rng,
            &clock,
            &MockBattery,
            &mut interp,
            &mut map_storage,
            &TestHmac,
            &TestSha256,
        );

        assert_eq!(outcome, WakeCycleOutcome::Sleep { seconds: 60 });
        assert!(!interp.loaded);
    }
}
