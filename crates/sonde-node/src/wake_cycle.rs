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
use crate::peer_request::peer_request_exchange;
use crate::program_store::{LoadedProgram, ProgramStore};
use crate::sleep::{SleepManager, WakeReason};
use crate::traits::{Clock, PlatformStorage, Rng, Transport};
use crate::FIRMWARE_ABI_VERSION;

/// Retry and timing constants (protocol.md §9).
const WAKE_MAX_RETRIES: u32 = 3;
const RETRY_DELAY_MS: u32 = 400;
const RESPONSE_TIMEOUT_MS: u32 = 200;

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

/// Lightweight wrapper that formats the first 4 bytes of a hash as a hex prefix (8 hex chars).
/// Intended for use in logging without performing any heap allocation.
struct HashHexPrefix<'a>(&'a [u8]);

impl<'a> core::fmt::Display for HashHexPrefix<'a> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for &b in self.0.iter().take(4) {
            write!(f, "{:02x}", b)?;
        }
        Ok(())
    }
}

/// Return a `Display`able view of the first 4 bytes of a hash as a hex prefix (8 hex chars).
/// This avoids heap allocation; use with formatting macros like `log::info!`.
fn hash_hex_prefix(hash: &[u8]) -> HashHexPrefix<'_> {
    HashHexPrefix(hash)
}

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

/// Log the ND-1007 deep-sleep entry and return `WakeCycleOutcome::Sleep`.
///
/// Centralises the sleep-entry log so every path that returns `Sleep`
/// emits the required INFO line with `duration_seconds` and `reason`.
fn log_and_sleep(sleep_mgr: &SleepManager) -> WakeCycleOutcome {
    let seconds = sleep_mgr.effective_sleep_s();
    let reason = match sleep_mgr.wake_reason() {
        WakeReason::Scheduled => "scheduled",
        WakeReason::Early => "early_wake",
        WakeReason::ProgramUpdate => "program_update",
    };
    log::info!(
        "entering deep sleep duration_seconds={} reason={} (ND-1007)",
        seconds,
        reason,
    );
    WakeCycleOutcome::Sleep { seconds }
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
pub fn run_wake_cycle<T, S, I>(
    transport: &mut T,
    storage: &mut S,
    hal: &mut (dyn Hal + 'static),
    rng: &mut dyn Rng,
    clock: &(dyn Clock + 'static),
    battery: &dyn BatteryReader,
    interpreter: &mut I,
    map_storage: &mut MapStorage,
    hmac: &(dyn HmacProvider + 'static),
    sha: &dyn Sha256Provider,
) -> WakeCycleOutcome
where
    T: Transport + 'static,
    S: PlatformStorage,
    I: BpfInterpreter,
{
    // 1. Load identity
    let identity = match storage.read_key() {
        Some((key_hint, psk)) => NodeIdentity { key_hint, psk },
        None => return WakeCycleOutcome::Unpaired,
    };

    // 1b. RNG health check (ND-0304 AC3).
    //     Abort early if the hardware RNG fails its self-test.
    //     Do not call determine_wake_reason() here — it consumes the
    //     early-wake flag via take_early_wake_flag().  On RNG failure we
    //     must preserve that flag so the next wake cycle can honour it.
    if !rng.health_check() {
        log::warn!("RNG health check failed — aborting wake cycle (ND-1009)");
        let (base_interval_s, _) = storage.read_schedule();
        let effective_sleep_s =
            SleepManager::new(base_interval_s, WakeReason::Scheduled).effective_sleep_s();
        log::info!(
            "entering deep sleep duration_seconds={} reason=scheduled (ND-1007)",
            effective_sleep_s,
        );
        return WakeCycleOutcome::Sleep {
            seconds: effective_sleep_s,
        };
    }

    // 2. Determine wake reason
    let wake_reason = determine_wake_reason(storage);

    log::info!(
        "wake cycle started key_hint=0x{:04X} wake_reason={:?} (ND-1001)",
        identity.key_hint,
        wake_reason,
    );

    // 3. Load schedule
    let (base_interval_s, _active_partition) = storage.read_schedule();
    let mut sleep_mgr = SleepManager::new(base_interval_s, wake_reason);

    // 3a. PEER_REQUEST/PEER_ACK exchange (ND-0909–ND-0913).
    //     If PSK is stored but reg_complete is not set, the node has been
    //     BLE-provisioned but not yet acknowledged by the gateway.  Run the
    //     PEER_REQUEST exchange before proceeding to the normal WAKE cycle.
    //
    //     If reg_complete is false but peer_payload is absent, the node was
    //     either BLE-provisioned (no payload) or the payload was erased after a
    //     permanent error.  In both cases we fall through to the normal WAKE
    //     cycle — the gateway will accept the WAKE regardless.
    if !storage.read_reg_complete() {
        if let Some(encrypted_payload) = storage.read_peer_payload() {
            match peer_request_exchange(
                transport,
                storage,
                &identity,
                &encrypted_payload,
                rng,
                clock,
                hmac,
            ) {
                Ok(true) => {
                    // Registration complete — fall through to normal WAKE cycle.
                }
                Ok(false) => {
                    // Timeout — sleep and retry next wake cycle (ND-0910/ND-0911).
                    return log_and_sleep(&sleep_mgr);
                }
                Err(e) => {
                    // Distinguish permanent errors (malformed payload, oversize)
                    // from transient ones (transport timeout, storage I/O).
                    // MalformedPayload errors are non-recoverable — the stored
                    // payload will never succeed, so erase it to break the loop.
                    if matches!(e, NodeError::MalformedPayload(_)) {
                        log::warn!("PEER_REQUEST permanent error: {} — erasing peer_payload", e);
                        let _ = storage.erase_peer_payload();
                    }
                    return log_and_sleep(&sleep_mgr);
                }
            }
        }
    }

    // 4. Load active resident program hash and raw bytes from NVS.
    // Only the hash is needed for the WAKE message; CBOR decode is
    // deferred to step 9 so that cycles which return early (Reboot,
    // transport/transfer failures) skip the decode entirely.
    let (program_hash, mut resident_image_bytes) = {
        let program_store = ProgramStore::new(storage);
        program_store.load_active_raw(sha)
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
        Ok(cmd) => {
            // WAKE/COMMAND succeeded — erase peer_payload if still present (ND-0914).
            if storage.has_peer_payload() {
                if let Err(e) = storage.erase_peer_payload() {
                    log::warn!("failed to erase peer_payload after WAKE success: {}", e);
                }
            }
            cmd
        }
        Err(e) => {
            // WAKE retries exhausted or transport error.
            log::warn!("WAKE/COMMAND failed: {} — sleeping (ND-1009)", e);
            // Self-healing (ND-0915): if reg_complete is set and peer_payload
            // is still present (meaning deferred erasure hasn't happened yet),
            // clear reg_complete so the next boot reverts to PEER_REQUEST.
            // Once peer_payload has been erased (after a prior successful
            // WAKE/COMMAND), transient WAKE failures should NOT revert to
            // PEER_REQUEST since there is no payload to re-send.
            if storage.read_reg_complete() && storage.has_peer_payload() {
                if let Err(e) = storage.write_reg_complete(false) {
                    log::warn!("failed to clear reg_complete after WAKE failure: {}", e);
                }
            }
            return log_and_sleep(&sleep_mgr);
        }
    };

    // Log the received COMMAND (ND-1003).
    match &command_payload {
        CommandPayload::Nop => log::info!("COMMAND received command_type=Nop"),
        CommandPayload::Reboot => log::info!("COMMAND received command_type=Reboot"),
        CommandPayload::UpdateSchedule { interval_s } => {
            log::info!(
                "COMMAND received command_type=UpdateSchedule interval_s={}",
                interval_s
            );
        }
        CommandPayload::UpdateProgram { program_hash, .. } => {
            log::info!(
                "COMMAND received command_type=UpdateProgram program_hash={}",
                hash_hex_prefix(program_hash)
            );
        }
        CommandPayload::RunEphemeral { program_hash, .. } => {
            log::info!(
                "COMMAND received command_type=RunEphemeral program_hash={}",
                hash_hex_prefix(program_hash)
            );
        }
    }

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
            // Drop the cached resident image bytes before starting the
            // chunked transfer.  During UpdateProgram/RunEphemeral the
            // node will allocate a reassembly buffer (up to
            // MAX_RESIDENT_IMAGE_SIZE); freeing the ~4 KB resident cache
            // here avoids holding both buffers simultaneously, reducing
            // peak heap usage on memory-constrained targets (ESP32-C3).
            resident_image_bytes = None;

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
                                return log_and_sleep(&sleep_mgr);
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
                        Err(e) => {
                            // Hash mismatch or decode failure — discard, sleep
                            log::warn!("program install failed: {}", e);
                            return log_and_sleep(&sleep_mgr);
                        }
                    }
                }
                Err(e) => {
                    // Chunk transfer failed — sleep
                    log::warn!("chunk transfer failed: {}", e);
                    return log_and_sleep(&sleep_mgr);
                }
            }
        }
    }

    // 9. BPF execution
    // Track whether a new resident program was installed this cycle.
    // Used to force map re-initialization even when layout matches.
    let resident_installed_this_cycle = loaded_program.as_ref().is_some_and(|p| !p.is_ephemeral);

    // Use the resident program loaded at step 4 if no new program was
    // transferred this cycle (avoids a second NVS read). Decode is
    // deferred to here so cycles that exit early skip CBOR parsing.
    if loaded_program.is_none() {
        if let Some(raw) = resident_image_bytes {
            loaded_program = ProgramStore::<S>::decode_image(&raw, program_hash);
        }
    }

    if let Some(program) = loaded_program {
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
                return log_and_sleep(&sleep_mgr);
            }
        } else {
            // For resident programs, re-allocate maps when the layout
            // doesn't match OR when a new program was installed this
            // cycle (even with identical layout, map data must be
            // zero-initialized per node-design.md §9.2).
            if resident_installed_this_cycle || !map_storage.layout_matches(&program.map_defs) {
                if map_storage.allocate(&program.map_defs).is_err() {
                    // Map budget exceeded. The newly installed resident
                    // program is already active (install_resident swapped
                    // partitions), so we do not roll back here.
                    return log_and_sleep(&sleep_mgr);
                }
                // Pre-populate maps with initial data from the program
                // image (e.g. .rodata / .data section content).
                map_storage.apply_initial_data(&program.map_initial_data);
            }
        }

        // Clone map pointers — the borrow on map_storage must be released
        // before bpf_dispatch::install() takes a mutable raw pointer to it.
        let map_ptrs = map_storage.map_pointers().to_vec();

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
                hal as *mut dyn crate::hal::Hal,
                transport as *mut T as *mut dyn crate::traits::Transport,
                map_storage as *mut MapStorage,
                &mut sleep_mgr as *mut SleepManager,
                clock as *const dyn crate::traits::Clock,
                hmac as *const dyn HmacProvider,
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
            Ok(()) => {
                // Ephemeral programs don't declare their own maps but can
                // access the resident program's maps (read-only).  Derive
                // map metadata from the current MapStorage allocation only
                // when needed to avoid per-cycle allocation for resident programs.
                let load_defs_owned;
                let (load_ptrs, load_defs): (&[u64], &[sonde_protocol::MapDef]) =
                    if program.map_defs.is_empty() && map_storage.map_count() > 0 {
                        load_defs_owned = (0..map_storage.map_count())
                            .filter_map(|i| map_storage.get(i).map(|m| m.def))
                            .collect::<Vec<_>>();
                        (&map_ptrs, &load_defs_owned)
                    } else {
                        (&map_ptrs, &program.map_defs)
                    };
                match interpreter.load(&program.bytecode, load_ptrs, load_defs) {
                    Ok(()) => {
                        // Log BPF execution start (ND-1006) — after all
                        // preconditions and load succeed.
                        log::info!(
                            "BPF execute program_hash={}",
                            hash_hex_prefix(&program.hash)
                        );
                        let ctx_ptr = &ctx as *const SondeContext as u64;
                        interpreter.execute(ctx_ptr, DEFAULT_INSTRUCTION_BUDGET)
                    }
                    Err(err) => {
                        log::error!("BPF program load failed: {}", err);
                        Err(err)
                    }
                }
            }
            Err(err) => {
                log::error!("BPF helper registration failed: {}", err);
                Err(err)
            }
        };

        // Swallow BPF errors — node sleeps normally regardless (ND-0504).
        match &exec_result {
            Ok(rc) => log::info!("BPF execution completed rc={}", rc),
            Err(err) => log::info!("BPF execution failed: {}", err),
        }
        let _ = exec_result;

        // Flush accumulated trace output (ND-1006 / T-N613).
        flush_trace_log(&trace_log);
    }

    // 10. Determine sleep duration
    if sleep_mgr.will_wake_early() {
        // Best-effort: retry once if the first write fails.
        if storage.set_early_wake_flag().is_err() {
            let _ = storage.set_early_wake_flag();
        }
    }

    log_and_sleep(&sleep_mgr)
}

/// Emit each accumulated `bpf_trace_printk` entry at INFO level (ND-1006).
fn flush_trace_log(trace_log: &[String]) {
    for entry in trace_log {
        log::info!("bpf_trace_printk: {}", entry);
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
fn wake_command_exchange<T: Transport>(
    transport: &mut T,
    identity: &NodeIdentity,
    wake_nonce: u64,
    program_hash: &[u8],
    battery_mv: u32,
    clock: &dyn Clock,
    hmac: &dyn HmacProvider,
) -> NodeResult<(u64, u64, CommandPayload)> {
    let wake_msg = NodeMessage::Wake {
        firmware_abi_version: FIRMWARE_ABI_VERSION,
        program_hash: program_hash.to_vec(),
        battery_mv,
    };
    let payload_cbor = wake_msg
        .encode()
        .map_err(|_| NodeError::MalformedPayload("WAKE message encode failed"))?;

    let header = FrameHeader {
        key_hint: identity.key_hint,
        msg_type: MSG_WAKE,
        nonce: wake_nonce,
    };

    let frame = encode_frame(&header, &payload_cbor, &identity.psk, hmac)
        .map_err(|_| NodeError::MalformedPayload("frame encode failed"))?;

    // Try sending WAKE up to (1 + WAKE_MAX_RETRIES) times
    for attempt in 0..=WAKE_MAX_RETRIES {
        if attempt > 0 {
            clock.delay_ms(RETRY_DELAY_MS);
        }

        transport.send(&frame)?;
        log::info!(
            "WAKE sent key_hint=0x{:04X} nonce=0x{:016X} attempt={} (ND-1002)",
            identity.key_hint,
            wake_nonce,
            attempt,
        );

        // Await COMMAND response
        match transport.recv(RESPONSE_TIMEOUT_MS)? {
            Some(raw_response) => {
                // Try to verify and decode
                match verify_and_decode_command(&raw_response, identity, wake_nonce, hmac) {
                    Ok(result) => return Ok(result),
                    Err(e) => {
                        log::warn!("COMMAND verification failed: {} (ND-1009)", e);
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
    hmac: &dyn HmacProvider,
) -> NodeResult<(u64, u64, CommandPayload)> {
    let decoded =
        decode_frame(raw).map_err(|_| NodeError::MalformedPayload("frame decode failed"))?;

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
        Err(_) => return Err(NodeError::MalformedPayload("COMMAND payload decode failed")),
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
        .map_err(|_| NodeError::MalformedPayload("CBOR decode failed"))?;

    let fields = match &value {
        ciborium::Value::Map(pairs) => pairs,
        _ => return Err(NodeError::MalformedPayload("expected CBOR map")),
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

    let starting_seq = starting_seq.ok_or(NodeError::MalformedPayload(
        "missing starting_seq in unknown command",
    ))?;
    let timestamp_ms = timestamp_ms.ok_or(NodeError::MalformedPayload(
        "missing timestamp_ms in unknown command",
    ))?;

    Ok((starting_seq, timestamp_ms, CommandPayload::Nop))
}

/// Execute the chunked program transfer protocol.
///
/// Returns the reassembled program image bytes on success.
#[allow(clippy::too_many_arguments)]
fn chunked_transfer<T: Transport>(
    transport: &mut T,
    identity: &NodeIdentity,
    current_seq: &mut u64,
    program_size: u32,
    chunk_size: u32,
    chunk_count: u32,
    max_image_size: usize,
    clock: &dyn Clock,
    hmac: &dyn HmacProvider,
) -> NodeResult<Vec<u8>> {
    let program_size_usize = program_size as usize;
    let chunk_size_usize = chunk_size as usize;

    // Reject transfers that exceed the maximum program image size
    if program_size_usize > max_image_size {
        return Err(NodeError::MalformedPayload(
            "program_size exceeds maximum image size",
        ));
    }

    // Validate chunk_size is non-zero
    if chunk_size == 0 {
        return Err(NodeError::MalformedPayload("chunk_size is zero"));
    }

    // Validate chunk_count matches expected value from program_size/chunk_size
    let expected_chunk_count = sonde_protocol::chunk_count(program_size_usize, chunk_size_usize);
    if expected_chunk_count != Some(chunk_count) {
        return Err(NodeError::MalformedPayload(
            "chunk_count does not match program_size / chunk_size",
        ));
    }

    let mut image_data: Vec<u8> = Vec::with_capacity(program_size_usize);

    for chunk_index in 0..chunk_count {
        let chunk_data =
            get_chunk_with_retry(transport, identity, current_seq, chunk_index, clock, hmac)?;

        // Enforce per-chunk size limit
        if chunk_data.len() > chunk_size_usize {
            return Err(NodeError::MalformedPayload(
                "received chunk larger than declared chunk_size",
            ));
        }

        // Enforce overall program size limit
        if image_data.len() + chunk_data.len() > program_size_usize {
            return Err(NodeError::MalformedPayload(
                "received data exceeds declared program_size",
            ));
        }

        image_data.extend_from_slice(&chunk_data);
    }

    // Final validation: assembled size must match declared program_size
    if image_data.len() != program_size_usize {
        return Err(NodeError::MalformedPayload(
            "assembled program size does not match declared program_size",
        ));
    }

    Ok(image_data)
}

/// Request a single chunk with retry logic.
///
/// Each retry attempt uses a fresh sequence number, since the gateway
/// may have received (and advanced past) the prior attempt's seq even
/// though the response was lost.
fn get_chunk_with_retry<T: Transport>(
    transport: &mut T,
    identity: &NodeIdentity,
    current_seq: &mut u64,
    chunk_index: u32,
    clock: &dyn Clock,
    hmac: &dyn HmacProvider,
) -> NodeResult<Vec<u8>> {
    let get_chunk_msg = NodeMessage::GetChunk { chunk_index };
    let payload_cbor = get_chunk_msg
        .encode()
        .map_err(|_| NodeError::MalformedPayload("GET_CHUNK message encode failed"))?;

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
            .map_err(|_| NodeError::MalformedPayload("frame encode failed"))?;

        transport.send(&frame)?;
        log::debug!(
            "GET_CHUNK sent chunk_index={} attempt={} (ND-1011)",
            chunk_index,
            attempt
        );
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
                    Ok(data) => {
                        log::debug!(
                            "CHUNK received chunk_index={} len={} (ND-1011)",
                            chunk_index,
                            data.len()
                        );
                        return Ok(data);
                    }
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
    hmac: &dyn HmacProvider,
) -> NodeResult<Vec<u8>> {
    let decoded =
        decode_frame(raw).map_err(|_| NodeError::MalformedPayload("frame decode failed"))?;

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
        .map_err(|_| NodeError::MalformedPayload("CHUNK payload decode failed"))?;

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
    hmac: &dyn HmacProvider,
) -> NodeResult<()> {
    let seq = *current_seq;

    let ack_msg = NodeMessage::ProgramAck {
        program_hash: program_hash.to_vec(),
    };
    let payload_cbor = ack_msg
        .encode()
        .map_err(|_| NodeError::MalformedPayload("PROGRAM_ACK message encode failed"))?;

    let header = FrameHeader {
        key_hint: identity.key_hint,
        msg_type: MSG_PROGRAM_ACK,
        nonce: seq,
    };

    let frame = encode_frame(&header, &payload_cbor, &identity.psk, hmac)
        .map_err(|_| NodeError::MalformedPayload("frame encode failed"))?;

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
            "APP_DATA blob exceeds frame payload budget",
        ));
    }

    let seq = *current_seq;

    let msg = NodeMessage::AppData {
        blob: blob.to_vec(),
    };
    let payload_cbor = msg
        .encode()
        .map_err(|_| NodeError::MalformedPayload("APP_DATA message encode failed"))?;

    // Reject after encoding so the CBOR overhead is accounted for.
    if payload_cbor.len() > sonde_protocol::MAX_PAYLOAD_SIZE {
        return Err(NodeError::MalformedPayload(
            "APP_DATA payload exceeds frame payload budget",
        ));
    }

    let header = FrameHeader {
        key_hint: identity.key_hint,
        msg_type: MSG_APP_DATA,
        nonce: seq,
    };

    let frame = encode_frame(&header, &payload_cbor, &identity.psk, hmac)
        .map_err(|_| NodeError::MalformedPayload("frame encode failed"))?;

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
            "APP_DATA blob exceeds frame payload budget",
        ));
    }

    let seq = *current_seq;

    let msg = NodeMessage::AppData {
        blob: blob.to_vec(),
    };
    let payload_cbor = msg
        .encode()
        .map_err(|_| NodeError::MalformedPayload("APP_DATA message encode failed"))?;

    // Reject after encoding so the CBOR overhead is accounted for.
    if payload_cbor.len() > sonde_protocol::MAX_PAYLOAD_SIZE {
        return Err(NodeError::MalformedPayload(
            "APP_DATA payload exceeds frame payload budget",
        ));
    }

    let header = FrameHeader {
        key_hint: identity.key_hint,
        msg_type: MSG_APP_DATA,
        nonce: seq,
    };

    let frame = encode_frame(&header, &payload_cbor, &identity.psk, hmac)
        .map_err(|_| NodeError::MalformedPayload("frame encode failed"))?;

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
            // `recv(remaining)` blocks for up to `remaining` ms (the
            // time left until the overall deadline). A `None` return
            // means that full interval elapsed with no data, so the
            // deadline has been reached — return `Timeout` immediately.
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

    /// Array map type identifier used in `MapDef` entries.
    const MAP_TYPE_ARRAY: u32 = 1;

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

    /// Transport mock that also records `timeout_ms` values passed to `recv()`.
    struct RecordingTransport {
        inbound: VecDeque<Option<Vec<u8>>>,
        outbound: Vec<Vec<u8>>,
        recv_timeouts: Vec<u32>,
    }

    impl RecordingTransport {
        fn new() -> Self {
            Self {
                inbound: VecDeque::new(),
                outbound: Vec::new(),
                recv_timeouts: Vec::new(),
            }
        }

        fn queue_response(&mut self, frame: Option<Vec<u8>>) {
            self.inbound.push_back(frame);
        }
    }

    impl Transport for RecordingTransport {
        fn send(&mut self, frame: &[u8]) -> NodeResult<()> {
            self.outbound.push(frame.to_vec());
            Ok(())
        }

        fn recv(&mut self, timeout_ms: u32) -> NodeResult<Option<Vec<u8>>> {
            self.recv_timeouts.push(timeout_ms);
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
        /// Counts how many times `read_program()` is called.
        pub read_program_count: std::cell::Cell<u32>,
        pub channel: Option<u8>,
        pub peer_payload: Option<Vec<u8>>,
        pub reg_complete: bool,
    }

    impl MockStorage {
        pub fn new() -> Self {
            Self {
                key: None,
                schedule_interval: 60,
                active_partition: 0,
                programs: [None, None],
                early_wake_flag: false,
                read_program_count: std::cell::Cell::new(0),
                channel: None,
                peer_payload: None,
                reg_complete: false,
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
            self.read_program_count
                .set(self.read_program_count.get() + 1);
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
        fn read_channel(&self) -> Option<u8> {
            self.channel
        }
        fn write_channel(&mut self, ch: u8) -> NodeResult<()> {
            self.channel = Some(ch);
            Ok(())
        }
        fn read_peer_payload(&self) -> Option<Vec<u8>> {
            self.peer_payload.clone()
        }
        fn write_peer_payload(&mut self, p: &[u8]) -> NodeResult<()> {
            self.peer_payload = Some(p.to_vec());
            Ok(())
        }
        fn erase_peer_payload(&mut self) -> NodeResult<()> {
            self.peer_payload = None;
            Ok(())
        }
        fn read_reg_complete(&self) -> bool {
            self.reg_complete
        }
        fn write_reg_complete(&mut self, c: bool) -> NodeResult<()> {
            self.reg_complete = c;
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
        fn adc_read(&mut self, _ch: u32) -> i32 {
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

    /// Clock that records every `delay_ms` call for timing assertions.
    struct TimingClock {
        delays: std::cell::RefCell<Vec<u32>>,
    }
    impl TimingClock {
        fn new() -> Self {
            Self {
                delays: std::cell::RefCell::new(Vec::new()),
            }
        }
        fn recorded_delays(&self) -> Vec<u32> {
            self.delays.borrow().clone()
        }
    }
    impl Clock for TimingClock {
        fn elapsed_ms(&self) -> u64 {
            100
        }
        fn delay_ms(&self, ms: u32) {
            self.delays.borrow_mut().push(ms);
        }
    }

    // --- Mock BPF interpreter ---
    struct MockBpfInterpreter {
        loaded: bool,
        executed: bool,
        execute_result: Result<u64, BpfError>,
        /// Captured BPF execution context (copied during execute()).
        captured_ctx: Option<SondeContext>,
        /// Bytecode passed to `load()`.
        captured_bytecode: Option<Vec<u8>>,
        /// Map pointers passed to `load()`.
        captured_map_ptrs: Option<Vec<u64>>,
        /// Map definitions passed to `load()`.
        captured_map_defs: Option<Vec<sonde_protocol::MapDef>>,
    }

    impl MockBpfInterpreter {
        fn new() -> Self {
            Self {
                loaded: false,
                executed: false,
                execute_result: Ok(0),
                captured_ctx: None,
                captured_bytecode: None,
                captured_map_ptrs: None,
                captured_map_defs: None,
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
        fn load(
            &mut self,
            bytecode: &[u8],
            map_ptrs: &[u64],
            map_defs: &[sonde_protocol::MapDef],
        ) -> Result<(), BpfError> {
            self.loaded = true;
            self.captured_bytecode = Some(bytecode.to_vec());
            self.captured_map_ptrs = Some(map_ptrs.to_vec());
            self.captured_map_defs = Some(map_defs.to_vec());
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
        // T-N100, T-N401: Unpaired node sends no frames and returns Unpaired.
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
        // T-N201, T-N700: No gateway response → exhaust retries → sleep.
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
        // T-N200, T-N204: Normal NOP wake cycle — WAKE → COMMAND → NOP → BPF → sleep.
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
        // T-N206: COMMAND REBOOT → outcome is Reboot.
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
        // T-N205: COMMAND UPDATE_SCHEDULE → node stores new interval and sleeps for it.
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
        // T-N301: Frame with wrong HMAC → discarded; retries exhausted → sleep.
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
        // T-N303: COMMAND with wrong echoed nonce → discarded; retries exhausted → sleep.
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
        // T-N500, T-N501, T-N504, T-N506, T-N508: Complete chunked transfer,
        // hash verification, A/B partition swap, BPF execution, ProgramUpdate wake reason.
        let psk = [0x22; 32];
        let key_hint = 1u16;
        let mut transport = MockTransport::new();

        // Build a small program image
        let image = sonde_protocol::ProgramImage {
            bytecode: vec![0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
            maps: vec![],
            map_initial_data: vec![],
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
        // T-N701: Chunk transfer timeout → exhaust retries → sleep (no BPF execution).
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
        // T-N802: CHUNK with wrong chunk_index → discarded; retries exhausted → sleep.
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
        // T-N604, T-N605: send() and send_recv() helpers — successful APP_DATA exchange.
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
        // T-N606: send_recv() timeout → returns Timeout error.
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
        // T-N303: APP_DATA_REPLY with wrong nonce → silently discarded → timeout.
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
                ciborium::Value::Integer(1710000000000u64.into()),
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
            map_initial_data: vec![],
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
            map_initial_data: vec![],
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
            map_initial_data: vec![],
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
            map_initial_data: vec![],
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
            map_initial_data: vec![],
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
            map_initial_data: vec![],
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
            map_initial_data: vec![],
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

    // ===================================================================
    // T-N608: Map persistence across wake cycles
    // ===================================================================

    #[test]
    fn test_map_persistence_across_cycles() {
        // T-N608: Map data survives across wake cycles when MapStorage
        // is preserved (simulating RTC SRAM persistence).
        let psk = [0x60; 32];
        let key_hint = 1u16;

        // Build a program with one map (4 entries × 4B value)
        let image = sonde_protocol::ProgramImage {
            bytecode: vec![0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
            maps: vec![sonde_protocol::MapDef {
                map_type: MAP_TYPE_ARRAY,
                key_size: 4,
                value_size: 4,
                max_entries: 4,
            }],
            map_initial_data: vec![vec![]],
        };
        let image_cbor = image.encode_deterministic().unwrap();
        let image_hash = TestSha256.hash(&image_cbor);
        let chunk_size = image_cbor.len() as u32;
        let starting_seq = 100u64;

        // Shared map storage — survives across cycles (like RTC SRAM)
        let mut map_storage = MapStorage::new(DEFAULT_MAP_BUDGET);

        // --- Cycle 1: Install program, maps get allocated ---
        let mut transport = MockTransport::new();
        let cmd = build_command_response(
            &psk,
            key_hint,
            1,
            starting_seq,
            1710000000000,
            CommandPayload::UpdateProgram {
                program_hash: image_hash.to_vec(),
                program_size: image_cbor.len() as u32,
                chunk_size,
                chunk_count: 1,
            },
        );
        transport.queue_response(Some(cmd));
        transport.queue_response(Some(build_chunk_response(
            &psk,
            key_hint,
            starting_seq,
            0,
            &image_cbor,
        )));

        let mut storage = MockStorage::new().with_key(key_hint, psk);
        let mut hal = MockHal;
        let mut rng = MockRng(0);
        let clock = MockClock;
        let mut interp = MockBpfInterpreter::new();

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

        // Maps should be allocated
        assert_eq!(map_storage.map_count(), 1);

        // Write a value into the map (simulating what a BPF program would do)
        let value = 42u32.to_le_bytes();
        map_storage.get_mut(0).unwrap().update(0, &value).unwrap();

        // --- Cycle 2: NOP command, same program, maps preserved ---
        let mut transport2 = MockTransport::new();
        let cmd2 = build_command_response(
            &psk,
            key_hint,
            2, // new nonce
            200,
            1710000000000,
            CommandPayload::Nop,
        );
        transport2.queue_response(Some(cmd2));
        let mut rng2 = MockRng(1);
        let mut interp2 = MockBpfInterpreter::new();

        run_wake_cycle(
            &mut transport2,
            &mut storage,
            &mut hal,
            &mut rng2,
            &clock,
            &MockBattery,
            &mut interp2,
            &mut map_storage,
            &TestHmac,
            &TestSha256,
        );

        // Map data should still be 42 (persisted across cycles)
        let read_back = map_storage.get(0).unwrap().lookup(0).unwrap();
        assert_eq!(read_back, &42u32.to_le_bytes());
    }

    // ===================================================================
    // T-N614 / T-N615: BPF execution constraint errors
    // ===================================================================

    #[test]
    fn test_instruction_budget_exceeded_graceful() {
        // T-N614: When the interpreter reports InstructionBudgetExceeded,
        // the node sleeps normally (no crash, no panic).
        let psk = [0x61; 32];
        let key_hint = 1u16;
        let mut transport = MockTransport::new();
        let command_frame =
            build_command_response(&psk, key_hint, 1, 1000, 1710000000000, CommandPayload::Nop);
        transport.queue_response(Some(command_frame));

        let image = sonde_protocol::ProgramImage {
            bytecode: vec![0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
            maps: vec![],
            map_initial_data: vec![],
        };
        let image_cbor = image.encode_deterministic().unwrap();

        let mut storage = MockStorage::new().with_key(key_hint, psk);
        storage.programs[0] = Some(image_cbor);
        let mut hal = MockHal;
        let mut rng = MockRng(0);
        let clock = MockClock;
        let mut interp = MockBpfInterpreter {
            loaded: false,
            executed: false,
            execute_result: Err(BpfError::InstructionBudgetExceeded),
            captured_ctx: None,
            captured_bytecode: None,
            captured_map_ptrs: None,
            captured_map_defs: None,
        };
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

        // Node should sleep normally despite BPF budget error
        assert_eq!(outcome, WakeCycleOutcome::Sleep { seconds: 60 });
        assert!(interp.executed, "interpreter must have executed");
    }

    #[test]
    fn test_call_depth_exceeded_graceful() {
        // T-N615: When the interpreter reports CallDepthExceeded,
        // the node sleeps normally.
        let psk = [0x62; 32];
        let key_hint = 1u16;
        let mut transport = MockTransport::new();
        let command_frame =
            build_command_response(&psk, key_hint, 1, 1000, 1710000000000, CommandPayload::Nop);
        transport.queue_response(Some(command_frame));

        let image = sonde_protocol::ProgramImage {
            bytecode: vec![0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
            maps: vec![],
            map_initial_data: vec![],
        };
        let image_cbor = image.encode_deterministic().unwrap();

        let mut storage = MockStorage::new().with_key(key_hint, psk);
        storage.programs[0] = Some(image_cbor);
        let mut hal = MockHal;
        let mut rng = MockRng(0);
        let clock = MockClock;
        let mut interp = MockBpfInterpreter {
            loaded: false,
            executed: false,
            execute_result: Err(BpfError::CallDepthExceeded),
            captured_ctx: None,
            captured_bytecode: None,
            captured_map_ptrs: None,
            captured_map_defs: None,
        };
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
        assert!(interp.executed, "interpreter must have executed");
    }

    #[test]
    fn test_nop_cycle_reads_program_exactly_once() {
        // Verify the single-read optimization: a no-update (Nop) wake
        // cycle must call read_program() exactly once — step 4 reads the
        // raw bytes and step 9 decodes them in-memory without a second
        // NVS read.
        let psk = [0x42; 32];
        let key_hint = 1u16;

        let image = sonde_protocol::ProgramImage {
            bytecode: vec![0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
            maps: vec![],
            map_initial_data: vec![],
        };
        let image_cbor = image.encode_deterministic().unwrap();

        let mut transport = MockTransport::new();
        let command_frame =
            build_command_response(&psk, key_hint, 1, 1000, 1710000000000, CommandPayload::Nop);
        transport.queue_response(Some(command_frame));

        let mut storage = MockStorage::new().with_key(key_hint, psk);
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
        assert!(interp.executed, "BPF must have executed");
        assert_eq!(
            storage.read_program_count.get(),
            1,
            "read_program() must be called exactly once (no double NVS read)"
        );
    }

    // ===================================================================
    // PEER_REQUEST integration tests (ND-0909–ND-0915)
    // ===================================================================

    /// Build a valid PEER_ACK frame for testing.
    fn build_peer_ack_response(
        psk: &[u8; 32],
        key_hint: u16,
        echo_nonce: u64,
        encrypted_payload: &[u8],
    ) -> Vec<u8> {
        use sonde_protocol::{MSG_PEER_ACK, PEER_ACK_KEY_PROOF, PEER_ACK_KEY_STATUS};

        // registration_proof = HMAC-SHA256(psk, "sonde-peer-ack-v1" || payload)
        let mut proof_input = Vec::new();
        proof_input.extend_from_slice(b"sonde-peer-ack-v1");
        proof_input.extend_from_slice(encrypted_payload);
        let proof = TestHmac.compute(psk, &proof_input);

        let cbor_map = ciborium::Value::Map(vec![
            (
                ciborium::Value::Integer(PEER_ACK_KEY_STATUS.into()),
                ciborium::Value::Integer(0.into()),
            ),
            (
                ciborium::Value::Integer(PEER_ACK_KEY_PROOF.into()),
                ciborium::Value::Bytes(proof.to_vec()),
            ),
        ]);
        let mut cbor_buf = Vec::new();
        ciborium::into_writer(&cbor_map, &mut cbor_buf).unwrap();

        let header = FrameHeader {
            key_hint,
            msg_type: MSG_PEER_ACK,
            nonce: echo_nonce,
        };
        encode_frame(&header, &cbor_buf, psk, &TestHmac).unwrap()
    }

    /// PEER_REQUEST is attempted when reg_complete=false and peer_payload present.
    /// After valid PEER_ACK, reg_complete is set and WAKE proceeds normally.
    #[test]
    fn test_peer_request_then_wake() {
        let psk = [0x42u8; 32];
        let key_hint = 0x1234u16;
        let payload = vec![0xDE, 0xAD, 0xBE, 0xEF];

        let mut transport = MockTransport::new();

        // MockRng starts at 0: first random_u64() returns 1 (PEER_REQUEST nonce),
        // second returns 2 (WAKE nonce).
        let peer_ack = build_peer_ack_response(&psk, key_hint, 1, &payload);
        transport.queue_response(Some(peer_ack));

        let command =
            build_command_response(&psk, key_hint, 2, 1000, 1710000000000, CommandPayload::Nop);
        transport.queue_response(Some(command));

        let mut storage = MockStorage::new().with_key(key_hint, psk);
        storage.peer_payload = Some(payload);
        storage.reg_complete = false;

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
        assert!(storage.reg_complete);
        assert!(storage.peer_payload.is_none());
        assert_eq!(transport.outbound.len(), 2);
    }

    /// When reg_complete=true, PEER_REQUEST is skipped and WAKE proceeds directly.
    #[test]
    fn test_skip_peer_request_when_reg_complete() {
        let psk = [0x42u8; 32];
        let key_hint = 0x1234u16;

        let mut transport = MockTransport::new();
        let command =
            build_command_response(&psk, key_hint, 1, 1000, 1710000000000, CommandPayload::Nop);
        transport.queue_response(Some(command));

        let mut storage = MockStorage::new().with_key(key_hint, psk);
        storage.reg_complete = true;

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
        assert_eq!(transport.outbound.len(), 1);
    }

    /// T-N917: WAKE failure after reg_complete clears the flag (self-healing)
    /// when peer_payload is still present.
    #[test]
    fn test_wake_failure_clears_reg_complete() {
        let psk = [0x42u8; 32];
        let key_hint = 0x1234u16;

        let mut transport = MockTransport::new();
        for _ in 0..4 {
            transport.queue_response(None);
        }

        let mut storage = MockStorage::new().with_key(key_hint, psk);
        storage.reg_complete = true;
        // peer_payload still present → self-healing can revert to PEER_REQUEST
        storage.peer_payload = Some(vec![0xDE, 0xAD]);

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
        assert!(!storage.reg_complete);
    }

    // ===================================================================
    // ND-0101: Unknown CBOR keys — forward compatibility (AC2)
    // ===================================================================

    #[test]
    fn test_unknown_cbor_keys_ignored() {
        // ND-0101 AC2: Node MUST ignore unknown CBOR keys in inbound
        // messages. Gateway sends a COMMAND with extra future-extension
        // keys (99, 100) alongside the standard fields. The node must
        // decode the message, treat it as NOP, and sleep normally.
        let psk = [0xF1; 32];
        let key_hint = 1u16;
        let mut transport = MockTransport::new();

        // Build a COMMAND NOP with extra unknown keys 99 and 100.
        let cbor_map = ciborium::Value::Map(vec![
            (
                ciborium::Value::Integer(4.into()),    // command_type
                ciborium::Value::Integer(0x00.into()), // NOP
            ),
            (
                ciborium::Value::Integer(13.into()), // starting_seq
                ciborium::Value::Integer(1000.into()),
            ),
            (
                ciborium::Value::Integer(14.into()), // timestamp_ms
                ciborium::Value::Integer(1710000000000u64.into()),
            ),
            (
                ciborium::Value::Integer(99.into()), // unknown future key
                ciborium::Value::Text("future_field".into()),
            ),
            (
                ciborium::Value::Integer(100.into()), // another unknown key
                ciborium::Value::Integer(42.into()),
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

        // Node must decode successfully and sleep normally.
        assert_eq!(outcome, WakeCycleOutcome::Sleep { seconds: 60 });
        assert_eq!(transport.outbound.len(), 1, "exactly one WAKE sent");
    }

    // ===================================================================
    // ND-0200: No second COMMAND exchange before sleep
    // ===================================================================

    #[test]
    fn test_second_command_not_consumed() {
        // ND-0200 AC2: Node processes at most one COMMAND per cycle and
        // MUST NOT initiate a second COMMAND exchange before sleep.
        //
        // A resident program is installed so BPF execution runs after the
        // first COMMAND. The second COMMAND (REBOOT) is queued in the
        // transport but must remain unconsumed: the wake cycle's recv()
        // calls are strictly limited to the WAKE→COMMAND exchange (and
        // optionally to APP_DATA_REPLY via send_recv during BPF helpers).
        // No code path calls recv() for a second COMMAND-type message.
        let psk = [0xF2; 32];
        let key_hint = 1u16;
        let mut transport = MockTransport::new();

        // First COMMAND — NOP (consumed)
        let nop_frame = build_command_response(
            &psk,
            key_hint,
            1, // echo nonce = rng(0)+1
            1000,
            1710000000000,
            CommandPayload::Nop,
        );
        transport.queue_response(Some(nop_frame));

        // Second COMMAND — REBOOT (must NOT be consumed)
        let reboot_frame = build_command_response(
            &psk,
            key_hint,
            1,
            1001,
            1710000000000,
            CommandPayload::Reboot,
        );
        transport.queue_response(Some(reboot_frame));

        // Install a resident program so BPF execution actually runs.
        let image = sonde_protocol::ProgramImage {
            bytecode: vec![0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
            maps: vec![],
            map_initial_data: vec![],
        };
        let image_cbor = image.encode_deterministic().unwrap();

        let mut storage = MockStorage::new().with_key(key_hint, psk);
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

        // Outcome must be Sleep (NOP), NOT Reboot.
        assert_eq!(outcome, WakeCycleOutcome::Sleep { seconds: 60 });
        // BPF must have executed (proving the program was loaded and run).
        assert!(interp.executed, "BPF must execute after first COMMAND");
        // The second frame must remain unconsumed in the transport queue.
        assert_eq!(
            transport.inbound.len(),
            1,
            "second COMMAND must not be consumed"
        );
    }

    // ===================================================================
    // ND-0605: Per-frame stack overflow — graceful termination
    // ===================================================================

    #[test]
    fn test_ac3_stack_overflow_graceful() {
        // ND-0605 AC3: A BPF stack overflow must terminate the program
        // and the node must sleep normally (no crash). The mock
        // interpreter returns RuntimeError("stack overflow") to simulate
        // the overflow that the real sonde-bpf interpreter would produce
        // when a program accesses memory beyond the 512-byte per-frame stack.
        let psk = [0xF3; 32];
        let key_hint = 1u16;
        let mut transport = MockTransport::new();
        let command_frame =
            build_command_response(&psk, key_hint, 1, 1000, 1710000000000, CommandPayload::Nop);
        transport.queue_response(Some(command_frame));

        let image = sonde_protocol::ProgramImage {
            bytecode: vec![0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
            maps: vec![],
            map_initial_data: vec![],
        };
        let image_cbor = image.encode_deterministic().unwrap();

        let mut storage = MockStorage::new().with_key(key_hint, psk);
        storage.programs[0] = Some(image_cbor);
        let mut hal = MockHal;
        let mut rng = MockRng(0);
        let clock = MockClock;
        let mut interp = MockBpfInterpreter {
            loaded: false,
            executed: false,
            execute_result: Err(BpfError::RuntimeError("stack overflow")),
            captured_ctx: None,
            captured_bytecode: None,
            captured_map_ptrs: None,
            captured_map_defs: None,
        };
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

        // Node sleeps normally despite stack overflow
        assert_eq!(outcome, WakeCycleOutcome::Sleep { seconds: 60 });
        assert!(interp.executed, "interpreter must have executed");
    }

    /// WAKE failure after peer_payload erased does NOT clear reg_complete.
    /// Once peer_payload is gone, transient WAKE failures should not revert
    /// to PEER_REQUEST since there is no payload to re-send.
    #[test]
    fn test_wake_failure_keeps_reg_complete_when_payload_erased() {
        let psk = [0x42u8; 32];
        let key_hint = 0x1234u16;

        let mut transport = MockTransport::new();
        for _ in 0..4 {
            transport.queue_response(None);
        }

        let mut storage = MockStorage::new().with_key(key_hint, psk);
        storage.reg_complete = true;
        // peer_payload already erased (prior WAKE/COMMAND succeeded)
        storage.peer_payload = None;

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
        // reg_complete must remain set — no payload to revert with
        assert!(storage.reg_complete);
    }

    // ===================================================================
    // Gap 1 (ND-0701): Chunk retry delay timing — 400 ms between retries
    // ===================================================================

    #[test]
    fn test_chunk_retry_delay_timing() {
        // T-N701 gap: Verify 400 ms delay between chunk retries.
        // Existing T-N701 checks retry count but never asserts the delay.
        let psk = [0x71; 32];
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
        // All 4 chunk attempts (1 initial + 3 retries) timeout
        for _ in 0..4 {
            transport.queue_response(None);
        }

        let mut storage = MockStorage::new().with_key(key_hint, psk);
        let mut hal = MockHal;
        let mut rng = MockRng(0);
        let clock = TimingClock::new();
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

        // Chunk retries should have 400 ms delay between each attempt.
        // The first attempt has no delay; retries 2, 3, 4 each delay 400 ms.
        let delays = clock.recorded_delays();
        let retry_delays: Vec<_> = delays
            .iter()
            .copied()
            .filter(|&d| d == RETRY_DELAY_MS)
            .collect();
        assert_eq!(
            retry_delays.len(),
            3,
            "expected 3 retry delays of {} ms, got delays: {:?}",
            RETRY_DELAY_MS,
            delays
        );
        for &d in &retry_delays {
            assert_eq!(
                d, RETRY_DELAY_MS,
                "retry delay must be {} ms",
                RETRY_DELAY_MS
            );
        }
    }

    // ===================================================================
    // Gap 2 (ND-0702): Under-timeout response accepted
    // ===================================================================

    #[test]
    fn test_response_accepted_under_timeout() {
        // T-N702 gap: A valid response arriving within the 200 ms timeout
        // must be accepted. Existing test only proves >200 ms triggers timeout.
        // Uses RecordingTransport to verify the production code passes the
        // correct RESPONSE_TIMEOUT_MS to recv().
        let psk = [0x72; 32];
        let key_hint = 1u16;
        let mut transport = RecordingTransport::new();

        // Queue a valid COMMAND response (arrives immediately = ~0 ms < 200 ms)
        let command_frame =
            build_command_response(&psk, key_hint, 1, 1000, 1710000000000, CommandPayload::Nop);
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

        // Response arrived under timeout — node must accept it and sleep normally
        assert_eq!(outcome, WakeCycleOutcome::Sleep { seconds: 60 });
        // Exactly 1 WAKE sent, no retries needed
        assert_eq!(transport.outbound.len(), 1);
        let decoded = decode_frame(&transport.outbound[0]).unwrap();
        assert_eq!(decoded.header.msg_type, MSG_WAKE);

        // Verify the production code used the correct timeout constant.
        // The first recv() call (WAKE/COMMAND exchange) must use
        // RESPONSE_TIMEOUT_MS (200 ms).
        assert!(!transport.recv_timeouts.is_empty());
        assert_eq!(
            transport.recv_timeouts[0], RESPONSE_TIMEOUT_MS,
            "recv() must be called with RESPONSE_TIMEOUT_MS ({} ms)",
            RESPONSE_TIMEOUT_MS
        );
    }

    // ===================================================================
    // Gap 3 (ND-0200): Second COMMAND during BPF execution discarded
    // ===================================================================

    #[test]
    fn test_second_command_during_bpf_discarded() {
        // ND-0200 AC2: The node processes at most one COMMAND response per
        // wake cycle and MUST NOT initiate a second COMMAND exchange.
        // Install a resident BPF program so execution actually enters the
        // BPF phase, then queue a second unsolicited COMMAND frame.
        // The node must accept the first, run BPF, ignore the second,
        // and sleep.
        let psk = [0x20; 32];
        let key_hint = 1u16;
        let mut transport = MockTransport::new();

        let command_frame =
            build_command_response(&psk, key_hint, 1, 1000, 1710000000000, CommandPayload::Nop);
        transport.queue_response(Some(command_frame));
        // Second COMMAND — should be ignored since the node has already
        // received its one COMMAND and moved to BPF execution.
        let second_command = build_command_response(
            &psk,
            key_hint,
            1,
            2000,
            1720000000000,
            CommandPayload::Reboot,
        );
        transport.queue_response(Some(second_command));

        // Install a minimal resident program (EXIT_0) so BPF execution
        // actually occurs, matching ND-0200's "during BPF" intent.
        let image = sonde_protocol::ProgramImage {
            bytecode: vec![0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
            maps: vec![],
            map_initial_data: vec![],
        };
        let image_cbor = image.encode_deterministic().unwrap();

        let mut storage = MockStorage::new().with_key(key_hint, psk);
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

        // Must sleep (NOP processed), NOT reboot (second COMMAND ignored)
        assert_eq!(outcome, WakeCycleOutcome::Sleep { seconds: 60 });
        // Exactly 1 WAKE sent — no second COMMAND exchange initiated
        assert_eq!(transport.outbound.len(), 1);
        // BPF must have executed — confirms the scenario truly tested
        // "during BPF execution" per ND-0200.
        assert!(
            interp.executed,
            "BPF must have executed with resident program"
        );
    }

    // ===================================================================
    // Gap 4 (ND-0301): Invalid HMAC — no other frames transmitted
    // ===================================================================

    #[test]
    fn test_invalid_hmac_no_other_frames_transmitted() {
        // ND-0301 AC2: No error response or diagnostic frame is transmitted
        // in response to an invalid HMAC. Existing T-N301 only asserts
        // "retries WAKE" but never asserts no other frames transmitted.
        let psk = [0x31; 32];
        let wrong_psk = [0x32; 32];
        let key_hint = 1u16;
        let mut transport = MockTransport::new();

        // Bad HMAC response, then 3 more timeouts
        let bad_frame = build_command_response(
            &wrong_psk,
            key_hint,
            1,
            1000,
            1710000000000,
            CommandPayload::Nop,
        );
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

        // ALL outbound frames must be WAKE messages — no error/diagnostic
        // frames are permitted in response to invalid HMAC (silent discard).
        assert!(!transport.outbound.is_empty());
        for (i, frame) in transport.outbound.iter().enumerate() {
            let decoded = decode_frame(frame).unwrap();
            assert_eq!(
                decoded.header.msg_type, MSG_WAKE,
                "outbound frame {} must be WAKE, got msg_type=0x{:02x}",
                i, decoded.header.msg_type
            );
        }
    }

    // ===================================================================
    // Gap 5 (ND-0801): Known msg_type in wrong context (COMMAND when
    // expecting CHUNK) is discarded
    // ===================================================================

    #[test]
    fn test_known_msg_type_wrong_context_discarded() {
        // ND-0801 AC2: A frame with a msg_type that does not match the
        // expected response (e.g., COMMAND when waiting for CHUNK) is
        // discarded. Existing T-N801 only tests unknown type (0x99).
        let psk = [0x81; 32];
        let key_hint = 1u16;
        let mut transport = MockTransport::new();

        let starting_seq = 300u64;
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

        // Send a valid COMMAND frame when the node expects CHUNK — wrong context.
        // Use the WAKE nonce (1 from MockRng(0)) as echo_nonce so the frame
        // is otherwise fully valid/authenticated and only wrong by msg_type.
        // This catches regressions where the node incorrectly tries to treat
        // the unexpected COMMAND as a real COMMAND during chunk transfer.
        let wrong_context_frame = build_command_response(
            &psk,
            key_hint,
            1, // WAKE nonce from MockRng(0) — frame is valid except for context
            2000,
            1720000000000,
            CommandPayload::Nop,
        );
        transport.queue_response(Some(wrong_context_frame));
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

        // Wrong-context frame is silently discarded, chunk retries exhausted
        assert_eq!(outcome, WakeCycleOutcome::Sleep { seconds: 60 });
        assert!(
            !interp.loaded,
            "BPF should not have loaded after chunk failure"
        );
    }

    // ===================================================================
    // Gap 6 (ND-0202): timestamp_ms stored and used in BPF context
    // ===================================================================

    #[test]
    fn test_timestamp_ms_stored_in_bpf_context() {
        // ND-0202 AC4: The `timestamp_ms` value is stored and used as
        // the basis for all time-related operations. Existing T-N204
        // provides it but never asserts the node stored or used it.
        let psk = [0x22; 32];
        let key_hint = 1u16;
        let mut transport = MockTransport::new();
        let timestamp_ms = 1_720_000_000_000u64;

        let command_frame =
            build_command_response(&psk, key_hint, 1, 1000, timestamp_ms, CommandPayload::Nop);
        transport.queue_response(Some(command_frame));

        // Install a resident program so BPF execution happens
        let image = sonde_protocol::ProgramImage {
            bytecode: vec![0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
            maps: vec![],
            map_initial_data: vec![],
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

        assert!(interp.executed, "BPF must have executed");
        let ctx = interp.captured_ctx.expect("BPF context should be captured");

        // The context timestamp must reflect the gateway-provided
        // timestamp_ms plus elapsed time since COMMAND was received.
        // MockClock always returns 100, so elapsed_since_command = 0.
        assert_eq!(
            ctx.timestamp, timestamp_ms,
            "`timestamp_ms` from COMMAND must be stored in BPF context"
        );
    }

    // ===================================================================
    // Gap 7 (ND-0303): Cross-sleep sequence isolation
    // ===================================================================

    #[test]
    fn test_sequence_number_isolation_across_cycles() {
        // ND-0303 AC3/AC4: No sequence state is persisted across deep sleep.
        // Each cycle starts fresh from the gateway-provided starting_seq.
        let psk = [0x33; 32];
        let key_hint = 1u16;

        let image = sonde_protocol::ProgramImage {
            bytecode: vec![0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
            maps: vec![],
            map_initial_data: vec![],
        };
        let image_cbor = image.encode_deterministic().unwrap();
        let image_hash = TestSha256.hash(&image_cbor);
        let chunk_size = image_cbor.len() as u32;

        // --- Cycle 1: starting_seq = 1000 ---
        let mut transport1 = MockTransport::new();
        let starting_seq_1 = 1000u64;
        transport1.queue_response(Some(build_command_response(
            &psk,
            key_hint,
            1,
            starting_seq_1,
            1710000000000,
            CommandPayload::UpdateProgram {
                program_hash: image_hash.to_vec(),
                program_size: image_cbor.len() as u32,
                chunk_size,
                chunk_count: 1,
            },
        )));
        transport1.queue_response(Some(build_chunk_response(
            &psk,
            key_hint,
            starting_seq_1,
            0,
            &image_cbor,
        )));

        let mut storage = MockStorage::new().with_key(key_hint, psk);
        let mut hal = MockHal;
        let mut rng = MockRng(0);
        let clock = MockClock;
        let mut interp1 = MockBpfInterpreter::new();
        let mut map_storage = MapStorage::new(DEFAULT_MAP_BUDGET);

        run_wake_cycle(
            &mut transport1,
            &mut storage,
            &mut hal,
            &mut rng,
            &clock,
            &MockBattery,
            &mut interp1,
            &mut map_storage,
            &TestHmac,
            &TestSha256,
        );

        // Verify cycle 1 used starting_seq_1
        let get_chunk_1 = &transport1.outbound[1];
        let decoded_1 = decode_frame(get_chunk_1).unwrap();
        assert_eq!(decoded_1.header.nonce, starting_seq_1);

        // --- Cycle 2: starting_seq = 5000 (NOT a continuation of 1000) ---
        // Use UpdateProgram so the node sends GET_CHUNK with starting_seq_2,
        // which lets us verify the sequence is NOT a continuation of cycle 1.
        let mut transport2 = MockTransport::new();
        let starting_seq_2 = 5000u64;
        // Use a fresh MockRng to simulate a new boot.
        let mut rng2 = MockRng(10);
        transport2.queue_response(Some(build_command_response(
            &psk,
            key_hint,
            11, // MockRng(10) → first random is 11
            starting_seq_2,
            1720000000000,
            CommandPayload::UpdateProgram {
                program_hash: image_hash.to_vec(),
                program_size: image_cbor.len() as u32,
                chunk_size,
                chunk_count: 1,
            },
        )));
        transport2.queue_response(Some(build_chunk_response(
            &psk,
            key_hint,
            starting_seq_2,
            0,
            &image_cbor,
        )));

        let mut interp2 = MockBpfInterpreter::new();

        run_wake_cycle(
            &mut transport2,
            &mut storage,
            &mut hal,
            &mut rng2,
            &clock,
            &MockBattery,
            &mut interp2,
            &mut map_storage,
            &TestHmac,
            &TestSha256,
        );

        // Cycle 2: first GET_CHUNK must use starting_seq_2 (5000),
        // NOT a continuation from cycle 1 (1000 + ...).
        let get_chunk_2 = &transport2.outbound[1]; // [0]=WAKE, [1]=GET_CHUNK
        let decoded_2 = decode_frame(get_chunk_2).unwrap();
        assert_eq!(decoded_2.header.msg_type, MSG_GET_CHUNK);
        assert_eq!(
            decoded_2.header.nonce, starting_seq_2,
            "cycle 2 GET_CHUNK must use starting_seq={}, not a continuation from cycle 1",
            starting_seq_2
        );
    }

    // -----------------------------------------------------------------------
    // T-N503: Program image decoding with maps
    // -----------------------------------------------------------------------

    /// T-N503: Transfer a program image with 2 map definitions and verify:
    /// - bytecode extraction,
    /// - map allocation and sizes, and
    /// - that the map pointer array is forwarded to `interpreter.load()`.
    ///
    /// Note: this test uses a simple `exit` bytecode and does not exercise
    /// LDDW pseudo-map-reference relocation. It validates map decoding,
    /// allocation, and pointer forwarding only.
    #[test]
    fn test_program_image_decoding_with_maps() {
        let psk = [0x42u8; 32];
        let key_hint = 1u16;

        let mut transport = MockTransport::new();

        // BPF bytecode: `exit` instruction (0x95)
        let bytecode = vec![0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];

        // Two map definitions:
        // Map 0: 16 entries × (4 key + 8 value) = 192 bytes
        // Map 1:  4 entries × (4 key + 32 value) = 144 bytes
        // Total: 336 bytes (well within 4096 budget)
        let map_defs = vec![
            sonde_protocol::MapDef {
                map_type: MAP_TYPE_ARRAY,
                key_size: 4,
                value_size: 8,
                max_entries: 16,
            },
            sonde_protocol::MapDef {
                map_type: MAP_TYPE_ARRAY,
                key_size: 4,
                value_size: 32,
                max_entries: 4,
            },
        ];

        let image = sonde_protocol::ProgramImage {
            bytecode: bytecode.clone(),
            maps: map_defs.clone(),
            map_initial_data: vec![vec![], vec![]],
        };
        let image_cbor = image.encode_deterministic().unwrap();
        let image_hash = TestSha256.hash(&image_cbor);

        let chunk_size = 64u32;
        let chunk_count =
            sonde_protocol::chunk_count(image_cbor.len(), chunk_size as usize).unwrap();
        const STARTING_SEQ: u64 = 100;
        const TIMESTAMP_MS: u64 = 1_710_000_000_000;

        // Queue COMMAND with UpdateProgram
        let command_frame = build_command_response(
            &psk,
            key_hint,
            1,
            STARTING_SEQ,
            TIMESTAMP_MS,
            CommandPayload::UpdateProgram {
                program_hash: image_hash.to_vec(),
                program_size: image_cbor.len() as u32,
                chunk_size,
                chunk_count,
            },
        );
        transport.queue_response(Some(command_frame));

        // Queue CHUNK responses
        for i in 0..chunk_count {
            let chunk_data = sonde_protocol::get_chunk(&image_cbor, i, chunk_size)
                .unwrap()
                .to_vec();
            let seq = STARTING_SEQ + i as u64;
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

        // Program installed and executed
        assert!(interp.loaded, "interpreter should be loaded");
        assert!(interp.executed, "interpreter should have executed");

        // A/B partition swapped (partition 1 now active)
        assert_eq!(storage.active_partition, 1);
        assert!(storage.read_program(1).is_some());

        // Maps allocated with correct count and sizes
        assert_eq!(map_storage.map_count(), 2, "should have 2 maps allocated");
        assert_eq!(
            map_storage.get(0).unwrap().storage_bytes(),
            192,
            "map 0: 16 × (4+8) = 192 bytes"
        );
        assert_eq!(
            map_storage.get(1).unwrap().storage_bytes(),
            144,
            "map 1: 4 × (4+32) = 144 bytes"
        );

        // Map definitions match what was requested
        assert_eq!(map_storage.get(0).unwrap().def, map_defs[0]);
        assert_eq!(map_storage.get(1).unwrap().def, map_defs[1]);

        // Map pointer forwarding: 2 non-zero, distinct map pointers
        // were passed to interpreter.load()
        let captured_ptrs = interp.captured_map_ptrs.as_ref().unwrap();
        assert_eq!(captured_ptrs.len(), 2, "should have 2 map pointers");
        assert_ne!(captured_ptrs[0], 0, "map pointer 0 must be non-zero");
        assert_ne!(captured_ptrs[1], 0, "map pointer 1 must be non-zero");
        assert_ne!(
            captured_ptrs[0], captured_ptrs[1],
            "map pointers must be distinct"
        );

        // Map definitions were forwarded to the interpreter
        let captured_defs = interp.captured_map_defs.as_ref().unwrap();
        assert_eq!(captured_defs.len(), 2);
        assert_eq!(captured_defs[0], map_defs[0]);
        assert_eq!(captured_defs[1], map_defs[1]);

        // Bytecode was forwarded unchanged
        let captured_bc = interp.captured_bytecode.as_ref().unwrap();
        assert_eq!(captured_bc, &bytecode);

        // Map pointers match MapStorage's cached pointers
        assert_eq!(captured_ptrs.as_slice(), map_storage.map_pointers());
    }

    // -----------------------------------------------------------------------
    // T-N616: Map memory budget enforcement
    // -----------------------------------------------------------------------

    /// T-N616: Transfer a program whose map definitions exceed the RTC SRAM
    /// budget. Verify installation fails, active partition is unchanged,
    /// and no PROGRAM_ACK is sent.
    #[test]
    fn test_map_budget_exceeded_rejects_program() {
        let psk = [0x42u8; 32];
        let key_hint = 1u16;
        let mut transport = MockTransport::new();

        // Oversized maps: 2 maps × 5 entries × (key_size + value_size) per entry
        const MAP_KEY_SIZE: u32 = 4;
        const MAP_VALUE_SIZE: u32 = 1024;
        const MAP_MAX_ENTRIES: u32 = 5;
        // Total: 2 × 5 × (4 + 1024) = 10_280 bytes
        // Budget is 4096 bytes → must be rejected.
        let image = sonde_protocol::ProgramImage {
            bytecode: vec![0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
            maps: vec![
                sonde_protocol::MapDef {
                    map_type: MAP_TYPE_ARRAY,
                    key_size: MAP_KEY_SIZE,
                    value_size: MAP_VALUE_SIZE,
                    max_entries: MAP_MAX_ENTRIES,
                },
                sonde_protocol::MapDef {
                    map_type: MAP_TYPE_ARRAY,
                    key_size: MAP_KEY_SIZE,
                    value_size: MAP_VALUE_SIZE,
                    max_entries: MAP_MAX_ENTRIES,
                },
            ],
            map_initial_data: vec![vec![], vec![]],
        };
        let image_cbor = image.encode_deterministic().unwrap();
        let image_hash = TestSha256.hash(&image_cbor);

        let chunk_size = 64u32;
        let chunk_count =
            sonde_protocol::chunk_count(image_cbor.len(), chunk_size as usize).unwrap();
        const STARTING_SEQ: u64 = 200;
        const TIMESTAMP_MS: u64 = 1_710_000_000_000;

        // Queue COMMAND with UpdateProgram
        let command_frame = build_command_response(
            &psk,
            key_hint,
            1,
            STARTING_SEQ,
            TIMESTAMP_MS,
            CommandPayload::UpdateProgram {
                program_hash: image_hash.to_vec(),
                program_size: image_cbor.len() as u32,
                chunk_size,
                chunk_count,
            },
        );
        transport.queue_response(Some(command_frame));

        // Queue CHUNK responses
        for i in 0..chunk_count {
            let chunk_data = sonde_protocol::get_chunk(&image_cbor, i, chunk_size)
                .unwrap()
                .to_vec();
            let seq = STARTING_SEQ + i as u64;
            let chunk_frame = build_chunk_response(&psk, key_hint, seq, i, &chunk_data);
            transport.queue_response(Some(chunk_frame));
        }

        // Pre-install a small program on partition 0 to verify it survives
        let existing_image = sonde_protocol::ProgramImage {
            bytecode: vec![0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
            maps: vec![],
            map_initial_data: vec![],
        };
        let existing_cbor = existing_image.encode_deterministic().unwrap();

        let mut storage = MockStorage::new().with_key(key_hint, psk);
        storage.programs[0] = Some(existing_cbor.clone());

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

        // Budget check happens in install_resident() BEFORE the A/B swap,
        // so the active partition must remain 0 and the existing program
        // must be untouched.
        assert_eq!(
            storage.active_partition, 0,
            "active partition must not change on budget rejection"
        );
        assert_eq!(
            storage.programs[0].as_deref(),
            Some(existing_cbor.as_slice()),
            "existing program must be preserved"
        );
        // The key invariant is that the active partition remains 0 and the
        // existing program is preserved. An implementation may legitimately
        // write the candidate image to the inactive partition as long as
        // activation (A/B swap) is rejected.

        // No PROGRAM_ACK sent — verify by inspecting all outbound frames.
        assert!(
            !transport.outbound.is_empty(),
            "at least one outbound frame expected"
        );
        for frame in &transport.outbound {
            let decoded =
                decode_frame(frame).expect("all outbound frames must decode successfully in test");
            assert_ne!(
                decoded.header.msg_type, MSG_PROGRAM_ACK,
                "no PROGRAM_ACK should be sent when budget is exceeded"
            );
        }

        // Interpreter should not have been loaded with the oversized program
        assert!(
            !interp.loaded,
            "interpreter must not load a budget-rejected program"
        );

        // Map storage should remain empty (no allocation occurred)
        assert_eq!(
            map_storage.map_count(),
            0,
            "no maps should be allocated on budget rejection"
        );
    }

    // ===================================================================
    // T-N927: RNG health-test failure aborts wake cycle (ND-0304 AC3)
    // ===================================================================

    /// MockRng that fails its health check.
    struct FailingRng;
    impl Rng for FailingRng {
        fn random_u64(&mut self) -> u64 {
            panic!("random_u64 must not be called when health check fails");
        }
        fn health_check(&mut self) -> bool {
            false
        }
    }

    #[test]
    fn t_n927_rng_health_check_failure_aborts() {
        // T-N927: If the hardware RNG health check fails, the firmware
        // must abort the wake cycle. No WAKE frame is transmitted.
        let psk = [0x42u8; 32];
        let key_hint = 1u16;

        let mut transport = MockTransport::new();
        let mut storage = MockStorage::new().with_key(key_hint, psk);
        // Set the early-wake flag before the cycle to verify it is preserved
        // on RNG failure (the code must not call determine_wake_reason which
        // would consume it via take_early_wake_flag).
        storage.early_wake_flag = true;
        let mut hal = MockHal;
        let mut rng = FailingRng;
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

        // Must sleep — no WAKE frame transmitted.
        assert!(matches!(outcome, WakeCycleOutcome::Sleep { .. }));
        assert!(
            transport.outbound.is_empty(),
            "no frames must be transmitted when RNG health check fails"
        );
        // Early-wake flag must be preserved (not consumed by determine_wake_reason).
        // TODO: Once the wake-cycle logic is updated to consume/clear the flag
        // on early abort (to prevent the next cycle from misclassifying the
        // wake as Early), update this assertion to expect `false`.
        assert!(
            storage.early_wake_flag,
            "early_wake_flag must remain set after RNG failure"
        );
    }

    // --- Gap 1: ND-0101 AC2 — Forward compatibility (unknown CBOR keys) ---

    /// Build a COMMAND frame with extra unknown CBOR keys. The node must
    /// process the command normally.
    fn build_command_with_extra_keys(
        psk: &[u8; 32],
        key_hint: u16,
        echo_nonce: u64,
        starting_seq: u64,
        timestamp_ms: u64,
    ) -> Vec<u8> {
        // Build a NOP COMMAND map with two extra unknown keys.
        // Keys in ascending order per RFC 8949 §4.2:
        //   4 (command_type), 13 (starting_seq), 14 (timestamp_ms),
        //   99 (unknown), 100 (unknown)
        let map = ciborium::Value::Map(vec![
            (
                ciborium::Value::Integer(sonde_protocol::KEY_COMMAND_TYPE.into()),
                ciborium::Value::Integer(sonde_protocol::CMD_NOP.into()),
            ),
            (
                ciborium::Value::Integer(sonde_protocol::KEY_STARTING_SEQ.into()),
                ciborium::Value::Integer(starting_seq.into()),
            ),
            (
                ciborium::Value::Integer(sonde_protocol::KEY_TIMESTAMP_MS.into()),
                ciborium::Value::Integer(timestamp_ms.into()),
            ),
            (
                ciborium::Value::Integer(99.into()),
                ciborium::Value::Text("future_gateway_field".to_string()),
            ),
            (
                ciborium::Value::Integer(100.into()),
                ciborium::Value::Array(vec![
                    ciborium::Value::Integer(1.into()),
                    ciborium::Value::Integer(2.into()),
                ]),
            ),
        ]);
        let mut payload_cbor = Vec::new();
        ciborium::into_writer(&map, &mut payload_cbor).unwrap();

        let header = FrameHeader {
            key_hint,
            msg_type: MSG_COMMAND,
            nonce: echo_nonce,
        };
        encode_frame(&header, &payload_cbor, psk, &TestHmac).unwrap()
    }

    #[test]
    fn test_cbor_forward_compat_unknown_keys() {
        // ND-0101 AC2: Node ignores unknown CBOR keys in inbound messages.
        // A COMMAND with extra keys (99, 100) must be processed as NOP.
        let psk = [0xC1; 32];
        let key_hint = 1u16;
        let mut transport = MockTransport::new();

        let command_frame = build_command_with_extra_keys(&psk, key_hint, 1, 1000, 1710000000000);
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

        // Node must treat the command as NOP and sleep normally.
        assert_eq!(outcome, WakeCycleOutcome::Sleep { seconds: 60 });
        // Exactly 1 WAKE frame sent (no retries needed).
        assert_eq!(transport.outbound.len(), 1);

        // Decode the outbound frame and ensure it is a WAKE message.
        let decoded = decode_frame(&transport.outbound[0]).unwrap();
        assert_eq!(decoded.header.msg_type, MSG_WAKE);
    }

    // --- Gap 2: ND-0103 — send_recv() frame size enforcement ---

    /// Derive the maximum AppData blob size that fits within MAX_PAYLOAD_SIZE
    /// after CBOR encoding, to avoid coupling tests to encoding overhead.
    fn max_app_data_blob_len() -> usize {
        let mut size = sonde_protocol::MAX_PAYLOAD_SIZE;
        while size > 0 {
            let probe = NodeMessage::AppData {
                blob: vec![0x00; size],
            };
            if probe.encode().unwrap().len() <= sonde_protocol::MAX_PAYLOAD_SIZE {
                return size;
            }
            size -= 1;
        }
        0
    }

    #[test]
    fn test_send_recv_max_blob() {
        // ND-0103 / T-N103: send_recv() with maximum blob that fits
        // within the frame budget succeeds.
        let max_blob_size = max_app_data_blob_len();

        let psk = [0xC2; 32];
        let key_hint = 1u16;
        let mut transport = MockTransport::new();

        // Queue a valid reply echoing the seq
        let reply = build_app_data_reply(&psk, key_hint, 100, &[0x01]);
        transport.queue_response(Some(reply));

        let identity = NodeIdentity { key_hint, psk };
        let mut seq = 100u64;

        let blob = vec![0xAB; max_blob_size];
        let result = send_recv_app_data(
            &mut transport,
            &identity,
            &mut seq,
            &blob,
            RESPONSE_TIMEOUT_MS,
            &MockClock,
            &TestHmac,
        );

        assert!(result.is_ok());
        assert_eq!(seq, 101);
        assert!(!transport.outbound.is_empty());
        assert!(transport.outbound[0].len() <= sonde_protocol::MAX_FRAME_SIZE);
    }

    #[test]
    fn test_send_recv_oversized_blob_rejected() {
        // ND-0103 / T-N104: send_recv() with oversized blob is rejected.
        // Seq is not advanced and no frame is sent.
        let max_blob_size = max_app_data_blob_len();

        let psk = [0xC3; 32];
        let key_hint = 1u16;
        let mut transport = MockTransport::new();

        let identity = NodeIdentity { key_hint, psk };
        let mut seq = 50u64;

        let blob = vec![0xCD; max_blob_size + 1];
        let result = send_recv_app_data(
            &mut transport,
            &identity,
            &mut seq,
            &blob,
            RESPONSE_TIMEOUT_MS,
            &MockClock,
            &TestHmac,
        );

        assert!(result.is_err());
        assert_eq!(seq, 50, "seq must not advance on rejection");
        assert!(
            transport.outbound.is_empty(),
            "no frame should be sent for oversized blob"
        );
    }

    // --- Gap 3: ND-0303 — Sequence number reset across sleep ---

    #[test]
    fn test_sequence_reset_across_wake_cycles() {
        // ND-0303 AC3/AC4: Sequence numbers do not persist across deep sleep.
        // Each wake cycle starts fresh from the gateway-provided starting_seq.
        let psk = [0xC4; 32];
        let key_hint = 1u16;

        let image = sonde_protocol::ProgramImage {
            bytecode: vec![0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
            maps: vec![],
            map_initial_data: vec![],
        };
        let image_cbor = image.encode_deterministic().unwrap();
        let image_hash = TestSha256.hash(&image_cbor);
        let chunk_size = image_cbor.len() as u32;
        let chunk_count =
            sonde_protocol::chunk_count(image_cbor.len(), chunk_size as usize).unwrap();

        // --- Cycle 1: starting_seq = 1000 ---
        let starting_seq_1 = 1000u64;
        let mut transport1 = MockTransport::new();
        let cmd1 = build_command_response(
            &psk,
            key_hint,
            1,
            starting_seq_1,
            1710000000000,
            CommandPayload::UpdateProgram {
                program_hash: image_hash.to_vec(),
                program_size: image_cbor.len() as u32,
                chunk_size,
                chunk_count,
            },
        );
        transport1.queue_response(Some(cmd1));

        for i in 0..chunk_count {
            let data = sonde_protocol::get_chunk(&image_cbor, i, chunk_size)
                .unwrap()
                .to_vec();
            let chunk = build_chunk_response(&psk, key_hint, starting_seq_1 + i as u64, i, &data);
            transport1.queue_response(Some(chunk));
        }

        let mut storage = MockStorage::new().with_key(key_hint, psk);
        let mut hal = MockHal;
        let mut rng1 = MockRng(0);
        let clock = MockClock;
        let mut interp1 = MockBpfInterpreter::new();
        let mut map_storage = MapStorage::new(DEFAULT_MAP_BUDGET);

        let outcome1 = run_wake_cycle(
            &mut transport1,
            &mut storage,
            &mut hal,
            &mut rng1,
            &clock,
            &MockBattery,
            &mut interp1,
            &mut map_storage,
            &TestHmac,
            &TestSha256,
        );
        assert_eq!(outcome1, WakeCycleOutcome::Sleep { seconds: 60 });

        // Verify cycle 1 used starting_seq_1 for the first GET_CHUNK it sent
        let gc1 = transport1
            .outbound
            .iter()
            .filter_map(|frame| decode_frame(frame).ok())
            .find(|msg| msg.header.msg_type == MSG_GET_CHUNK)
            .expect("no MSG_GET_CHUNK frame sent in cycle 1");
        assert_eq!(gc1.header.nonce, starting_seq_1);

        // --- Cycle 2: starting_seq = 5000 (fresh, no carryover) ---
        let starting_seq_2 = 5000u64;
        let mut transport2 = MockTransport::new();
        let cmd2 = build_command_response(
            &psk,
            key_hint,
            2, // nonce = 2 because MockRng(1) starts at 1 and increments on first use
            starting_seq_2,
            1710000000000,
            CommandPayload::UpdateProgram {
                program_hash: image_hash.to_vec(),
                program_size: image_cbor.len() as u32,
                chunk_size,
                chunk_count,
            },
        );
        transport2.queue_response(Some(cmd2));

        for i in 0..chunk_count {
            let data = sonde_protocol::get_chunk(&image_cbor, i, chunk_size)
                .unwrap()
                .to_vec();
            let chunk = build_chunk_response(&psk, key_hint, starting_seq_2 + i as u64, i, &data);
            transport2.queue_response(Some(chunk));
        }

        let mut rng2 = MockRng(1);
        let mut interp2 = MockBpfInterpreter::new();
        // Create fresh MapStorage for cycle 2 to model a rebooted wake
        // cycle where RAM-backed map state does not survive deep sleep.
        let mut map_storage2 = MapStorage::new(DEFAULT_MAP_BUDGET);

        let outcome2 = run_wake_cycle(
            &mut transport2,
            &mut storage,
            &mut hal,
            &mut rng2,
            &clock,
            &MockBattery,
            &mut interp2,
            &mut map_storage2,
            &TestHmac,
            &TestSha256,
        );
        assert_eq!(outcome2, WakeCycleOutcome::Sleep { seconds: 60 });

        // Verify cycle 2 uses starting_seq_2, NOT a continuation of cycle 1.
        let gc2 = transport2
            .outbound
            .iter()
            .filter_map(|frame| decode_frame(frame).ok())
            .find(|msg| msg.header.msg_type == MSG_GET_CHUNK)
            .expect("no MSG_GET_CHUNK frame sent in cycle 2");
        assert_eq!(
            gc2.header.nonce, starting_seq_2,
            "cycle 2 must start fresh from gateway-provided starting_seq"
        );
    }

    // --- Gap 4: ND-0203 — Base interval restore after set_next_wake() ---

    /// BPF interpreter mock that calls `set_next_wake` during execute.
    struct SetNextWakeInterpreter {
        next_wake_s: Option<u32>,
        loaded: bool,
        executed: bool,
    }

    impl SetNextWakeInterpreter {
        fn new(next_wake_s: Option<u32>) -> Self {
            Self {
                next_wake_s,
                loaded: false,
                executed: false,
            }
        }
    }

    impl BpfInterpreter for SetNextWakeInterpreter {
        fn register_helper(
            &mut self,
            _id: u32,
            _func: crate::bpf_runtime::HelperFn,
        ) -> Result<(), BpfError> {
            Ok(())
        }
        fn load(
            &mut self,
            _bytecode: &[u8],
            _map_ptrs: &[u64],
            _map_defs: &[sonde_protocol::MapDef],
        ) -> Result<(), BpfError> {
            self.loaded = true;
            Ok(())
        }
        fn execute(&mut self, _ctx_ptr: u64, _budget: u64) -> Result<u64, BpfError> {
            self.executed = true;
            // Call set_next_wake via the bpf_dispatch helper if requested.
            // The dispatch context is installed by run_wake_cycle before
            // execute() is called, so the thread-local is valid.
            if let Some(seconds) = self.next_wake_s {
                crate::bpf_dispatch::helper_set_next_wake(seconds as u64, 0, 0, 0, 0);
            }
            Ok(0)
        }
    }

    #[test]
    fn test_set_next_wake_e2e_base_interval_restore() {
        // ND-0203 / T-N208: Full e2e set_next_wake → base-interval-restore.
        // Cycle 1: base=300s, BPF calls set_next_wake(10) → sleeps 10s.
        // Cycle 2: base=300s, no set_next_wake → sleeps 300s (restored).
        let psk = [0xC5; 32];
        let key_hint = 1u16;

        let image = sonde_protocol::ProgramImage {
            bytecode: vec![0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
            maps: vec![],
            map_initial_data: vec![],
        };
        let image_cbor = image.encode_deterministic().unwrap();

        // --- Cycle 1: set_next_wake(10) with base=300 ---
        let mut transport1 = MockTransport::new();
        let cmd1 =
            build_command_response(&psk, key_hint, 1, 1000, 1710000000000, CommandPayload::Nop);
        transport1.queue_response(Some(cmd1));

        let mut storage = MockStorage::new().with_key(key_hint, psk);
        storage.schedule_interval = 300;
        storage.programs[0] = Some(image_cbor.clone());

        let mut hal = MockHal;
        let mut rng1 = MockRng(0);
        let clock = MockClock;
        // This interpreter calls set_next_wake(10) during execute.
        let mut interp1 = SetNextWakeInterpreter::new(Some(10));
        let mut map_storage = MapStorage::new(DEFAULT_MAP_BUDGET);

        let outcome1 = run_wake_cycle(
            &mut transport1,
            &mut storage,
            &mut hal,
            &mut rng1,
            &clock,
            &MockBattery,
            &mut interp1,
            &mut map_storage,
            &TestHmac,
            &TestSha256,
        );

        assert!(interp1.executed, "BPF must have executed");
        assert_eq!(
            outcome1,
            WakeCycleOutcome::Sleep { seconds: 10 },
            "cycle 1 must sleep for min(10, 300) = 10"
        );
        // Early wake flag should be set for cycle 2
        assert!(
            storage.early_wake_flag,
            "early_wake_flag must be persisted for next cycle"
        );

        // --- Cycle 2: no set_next_wake, base interval restored ---
        let mut transport2 = MockTransport::new();
        let cmd2 =
            build_command_response(&psk, key_hint, 2, 2000, 1710000000000, CommandPayload::Nop);
        transport2.queue_response(Some(cmd2));

        let mut rng2 = MockRng(1);
        // No set_next_wake this cycle.
        let mut interp2 = SetNextWakeInterpreter::new(None);

        let outcome2 = run_wake_cycle(
            &mut transport2,
            &mut storage,
            &mut hal,
            &mut rng2,
            &clock,
            &MockBattery,
            &mut interp2,
            &mut map_storage,
            &TestHmac,
            &TestSha256,
        );

        assert!(interp2.executed, "BPF must have executed in cycle 2");
        assert_eq!(
            outcome2,
            WakeCycleOutcome::Sleep { seconds: 300 },
            "cycle 2 must restore base interval (300s)"
        );
    }

    // --- Gap 5: ND-0801 AC2 — Wrong msg_type for expected context ---

    #[test]
    fn test_wrong_msg_type_command_when_chunk_expected() {
        // ND-0801 AC2: During chunked transfer, a COMMAND frame (valid
        // msg_type, valid HMAC) is sent when CHUNK is expected.
        // Must be silently discarded; retries exhaust → sleep.
        let psk = [0xC6; 32];
        let key_hint = 1u16;
        let starting_seq = 100u64;

        let image = sonde_protocol::ProgramImage {
            bytecode: vec![0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
            maps: vec![],
            map_initial_data: vec![],
        };
        let image_cbor = image.encode_deterministic().unwrap();
        let image_hash = TestSha256.hash(&image_cbor);

        let mut transport = MockTransport::new();
        // Valid COMMAND that initiates the chunked transfer
        let cmd = build_command_response(
            &psk,
            key_hint,
            1,
            starting_seq,
            1710000000000,
            CommandPayload::UpdateProgram {
                program_hash: image_hash.to_vec(),
                program_size: image_cbor.len() as u32,
                chunk_size: image_cbor.len() as u32,
                chunk_count: 1,
            },
        );
        transport.queue_response(Some(cmd));

        // Instead of CHUNK, send a COMMAND frame — valid but wrong msg_type
        let wrong_frame = build_command_response(
            &psk,
            key_hint,
            starting_seq,
            2000,
            1710000000000,
            CommandPayload::Nop,
        );
        transport.queue_response(Some(wrong_frame));
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
        // Verify the node entered the chunk-transfer path by sending
        // at least one MSG_GET_CHUNK before the wrong msg_type was
        // received and discarded.
        let sent_get_chunk = transport.outbound.iter().any(|f| {
            decode_frame(f)
                .map(|d| d.header.msg_type == MSG_GET_CHUNK)
                .unwrap_or(false)
        });
        assert!(
            sent_get_chunk,
            "node must have sent MSG_GET_CHUNK before discarding wrong msg_type"
        );
        assert!(
            !interp.loaded,
            "program must not load — wrong msg_type was discarded"
        );
    }

    #[test]
    fn test_wrong_msg_type_chunk_when_app_data_reply_expected() {
        // ND-0801 AC2: During send_recv, a CHUNK frame is sent when
        // APP_DATA_REPLY is expected. Must be silently discarded → timeout.
        let psk = [0xC7; 32];
        let key_hint = 1u16;
        let mut transport = MockTransport::new();

        // Queue a CHUNK frame when APP_DATA_REPLY is expected
        let wrong_frame = build_chunk_response(&psk, key_hint, 42, 0, &[0x01, 0x02]);
        transport.queue_response(Some(wrong_frame));
        // Next recv returns None → timeout
        transport.queue_response(None);

        let identity = NodeIdentity { key_hint, psk };
        let mut seq = 42u64;
        let result = send_recv_app_data(
            &mut transport,
            &identity,
            &mut seq,
            &[0xAA],
            RESPONSE_TIMEOUT_MS,
            &MockClock,
            &TestHmac,
        );

        assert!(
            matches!(result, Err(NodeError::Timeout)),
            "CHUNK when APP_DATA_REPLY expected must be discarded → timeout"
        );
        assert_eq!(seq, 43, "seq must still advance after send");
    }

    // --- Gap 6: ND-0702 — Response timeout (200 ms) ---

    /// Clock whose elapsed_ms advances by a fixed step each call.
    struct AdvancingClock {
        step_ms: u64,
        counter: std::cell::Cell<u64>,
    }

    impl AdvancingClock {
        fn new(start_ms: u64, step_ms: u64) -> Self {
            Self {
                step_ms,
                counter: std::cell::Cell::new(start_ms),
            }
        }
    }

    impl Clock for AdvancingClock {
        fn elapsed_ms(&self) -> u64 {
            let v = self.counter.get();
            self.counter.set(v + self.step_ms);
            v
        }
        fn delay_ms(&self, _ms: u32) {}
    }

    #[test]
    fn test_response_timeout_constant_is_200ms() {
        // ND-0702: On ESP-NOW with USB-CDC modem bridge, the response timeout
        // MUST be 200 ms to account for serial round-trip latency.
        assert_eq!(RESPONSE_TIMEOUT_MS, 200);
    }

    #[test]
    fn test_response_timeout_send_recv_deadline() {
        // ND-0702 / T-N702: send_recv uses the 200 ms timeout as a
        // deadline. With a clock that advances, once the deadline
        // expires the node returns Timeout even if recv would produce
        // a frame later.
        let psk = [0xC8; 32];
        let key_hint = 1u16;
        let mut transport = MockTransport::new();

        // Queue an invalid frame (wrong nonce) so the loop continues,
        // then the advancing clock pushes past the deadline.
        let wrong_nonce_reply = build_app_data_reply(&psk, key_hint, 999, &[0x01]);
        transport.queue_response(Some(wrong_nonce_reply));

        let identity = NodeIdentity { key_hint, psk };
        let mut seq = 42u64;
        // Clock starts at 0, advances 30ms per call.
        // Call 1 (deadline calc): elapsed=0, deadline=200
        // Call 2 (loop check): elapsed=30, 30<200 → recv
        // recv returns wrong-nonce frame → continue
        // Call 3 (loop check): elapsed=60, ...
        // Eventually elapsed >= 200 → Timeout
        let clock = AdvancingClock::new(0, 30);

        let result = send_recv_app_data(
            &mut transport,
            &identity,
            &mut seq,
            &[0xAA],
            RESPONSE_TIMEOUT_MS,
            &clock,
            &TestHmac,
        );

        assert!(
            matches!(result, Err(NodeError::Timeout)),
            "must timeout when clock exceeds 200 ms deadline"
        );
    }

    #[test]
    fn test_wake_command_timeout_retries() {
        // ND-0702 / T-N702: WAKE/COMMAND exchange uses 200 ms timeout.
        // First response times out (None), second succeeds.
        let psk = [0xC9; 32];
        let key_hint = 1u16;
        let mut transport = MockTransport::new();

        // Attempt 0: timeout
        transport.queue_response(None);
        // Attempt 1: valid COMMAND
        let cmd =
            build_command_response(&psk, key_hint, 1, 1000, 1710000000000, CommandPayload::Nop);
        transport.queue_response(Some(cmd));

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
        // 2 WAKE frames: initial attempt + 1 retry
        assert_eq!(transport.outbound.len(), 2, "node must retry after timeout");
    }

    // -----------------------------------------------------------------------
    // T-N925: APP_DATA_REPLY with mismatched nonce — discarded (ND-0302)
    // -----------------------------------------------------------------------

    /// T-N925: BPF `send_recv()` receives an APP_DATA_REPLY whose nonce
    /// does not match the request → silently discarded → call times out.
    #[test]
    fn t_n925_app_data_reply_mismatched_nonce_discarded() {
        let psk = [0xA5; 32];
        let key_hint = 1u16;
        let mut transport = MockTransport::new();

        // Reply echoes wrong seq (999 instead of 42) — silently discarded.
        let reply = build_app_data_reply(&psk, key_hint, 999, &[0xBB]);
        transport.queue_response(Some(reply));
        // Subsequent recv returns None → timeout.
        transport.queue_response(None);

        let identity = NodeIdentity { key_hint, psk };
        let mut seq = 42u64;
        let result = send_recv_app_data(
            &mut transport,
            &identity,
            &mut seq,
            &[0xAA],
            RESPONSE_TIMEOUT_MS,
            &MockClock,
            &TestHmac,
        );

        // The mismatched-nonce reply is silently discarded; call times out.
        assert!(
            matches!(result, Err(NodeError::Timeout)),
            "mismatched-nonce APP_DATA_REPLY must be discarded"
        );
        // seq is still incremented (send succeeded).
        assert_eq!(seq, 43);
    }

    // -- Log-level tests (ND-1006 / T-N1014) ----------------------------------

    #[cfg(debug_assertions)]
    use crate::test_log_capture;

    // Skipped in release builds — `info!()` is stripped at compile time by
    // `release_max_level_warn` (ND-1012).
    #[cfg(debug_assertions)]
    #[test]
    fn test_flush_trace_log_emits_info() {
        // ND-1006 / T-N1014: bpf_trace_printk output is flushed at INFO level.
        test_log_capture::init();
        test_log_capture::drain_log_records();

        let entries = vec!["hello".to_string(), "world".to_string()];
        flush_trace_log(&entries);

        let records = test_log_capture::drain_log_records();
        assert!(
            records.iter().any(|(level, msg)| *level == log::Level::Info
                && msg.contains("bpf_trace_printk: hello")),
            "expected INFO log for 'hello', got: {:?}",
            records
        );
        assert!(
            records.iter().any(|(level, msg)| *level == log::Level::Info
                && msg.contains("bpf_trace_printk: world")),
            "expected INFO log for 'world', got: {:?}",
            records
        );
    }
}
