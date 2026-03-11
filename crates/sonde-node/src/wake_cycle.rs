// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Wake cycle state machine.
//!
//! Implements the core node lifecycle per protocol.md §6.1:
//! `boot → WAKE → COMMAND → dispatch → (transfer/execute) → sleep`

use sonde_protocol::{
    decode_frame, encode_frame, verify_frame, CommandPayload, FrameHeader, GatewayMessage,
    HmacProvider, NodeMessage, Sha256Provider, MSG_APP_DATA, MSG_APP_DATA_REPLY, MSG_CHUNK,
    MSG_COMMAND, MSG_GET_CHUNK, MSG_PROGRAM_ACK, MSG_WAKE,
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

/// Maximum allowed program image size in bytes. Protects against
/// excessive allocation from a large (but authenticated) `program_size`
/// in the COMMAND payload. Derived from the flash partition size (4 KB).
const MAX_PROGRAM_IMAGE_SIZE: usize = 4096;

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
pub fn run_wake_cycle<T, S, H, R, C, B, I>(
    transport: &mut T,
    storage: &mut S,
    _hal: &mut H,
    rng: &mut R,
    clock: &C,
    battery: &B,
    interpreter: &mut I,
    map_storage: &mut MapStorage,
    hmac: &impl HmacProvider,
    sha: &impl Sha256Provider,
) -> WakeCycleOutcome
where
    T: Transport,
    S: PlatformStorage,
    H: Hal,
    R: Rng,
    C: Clock,
    B: BatteryReader,
    I: BpfInterpreter,
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
            // Chunked transfer
            let transfer_result = chunked_transfer(
                transport,
                &identity,
                &mut current_seq,
                program_size,
                chunk_size,
                chunk_count,
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
                            // Send PROGRAM_ACK
                            let _ = send_program_ack(
                                transport,
                                &identity,
                                &mut current_seq,
                                &program.hash,
                                hmac,
                            );

                            if !is_ephemeral && storage.set_program_updated_flag().is_err() {
                                return WakeCycleOutcome::Sleep {
                                    seconds: sleep_mgr.effective_sleep_s(),
                                };
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
    // Load program if not already loaded from transfer
    let program_is_new = loaded_program.is_some();
    if loaded_program.is_none() {
        let program_store = ProgramStore::new(storage);
        loaded_program = program_store.load_active(sha);
    }

    if let Some(mut program) = loaded_program {
        let _program_class = if program.is_ephemeral {
            ProgramClass::Ephemeral
        } else {
            ProgramClass::Resident
        };

        // Re-allocate maps only when a new program was installed this
        // cycle. On a normal NOP wake the caller-owned map_storage
        // already contains the correct layout with data surviving from
        // the previous cycle (backed by RTC slow SRAM on real hardware).
        if (program_is_new || map_storage.map_count() == 0)
            && map_storage.allocate(&program.map_defs).is_err()
        {
            // Map budget exceeded. The newly installed resident program
            // is already active (install_resident swapped partitions),
            // so we do not roll back here. This is a firmware-level
            // configuration issue.
            return WakeCycleOutcome::Sleep {
                seconds: sleep_mgr.effective_sleep_s(),
            };
        }

        // Resolve LDDW map references
        let map_ptrs = map_storage.map_pointers();
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
            timestamp: timestamp_ms + elapsed_since_command,
            battery_mv: battery_mv_clamped,
            firmware_abi_version: u16::try_from(FIRMWARE_ABI_VERSION)
                .expect("FIRMWARE_ABI_VERSION must fit in u16"),
            wake_reason: wake_reason as u8,
            _padding: [0; 3],
        };

        // Load and execute
        if let Ok(()) = interpreter.load(&program.bytecode, &map_ptrs) {
            let ctx_ptr = &ctx as *const SondeContext as u64;
            let _ = interpreter.execute(ctx_ptr, DEFAULT_INSTRUCTION_BUDGET);
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
    if storage.take_program_updated_flag() {
        WakeReason::ProgramUpdate
    } else if storage.take_early_wake_flag() {
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

    // Decode CBOR
    let gateway_msg = GatewayMessage::decode(decoded.header.msg_type, &decoded.payload)
        .map_err(|e| NodeError::MalformedPayload(format!("{}", e)))?;

    match gateway_msg {
        GatewayMessage::Command {
            starting_seq,
            timestamp_ms,
            payload,
        } => Ok((starting_seq, timestamp_ms, payload)),
        _ => Err(NodeError::UnexpectedMsgType(decoded.header.msg_type)),
    }
}

/// Execute the chunked transfer sub-protocol.
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
    clock: &C,
    hmac: &impl HmacProvider,
) -> NodeResult<Vec<u8>> {
    let program_size_usize = program_size as usize;
    let chunk_size_usize = chunk_size as usize;

    // Reject transfers that exceed the maximum program image size
    if program_size_usize > MAX_PROGRAM_IMAGE_SIZE {
        return Err(NodeError::MalformedPayload(format!(
            "program_size {} exceeds maximum {}",
            program_size, MAX_PROGRAM_IMAGE_SIZE
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
pub fn send_app_data<T: Transport>(
    transport: &mut T,
    identity: &NodeIdentity,
    current_seq: &mut u64,
    blob: &[u8],
    hmac: &impl HmacProvider,
) -> NodeResult<()> {
    let seq = *current_seq;

    let msg = NodeMessage::AppData {
        blob: blob.to_vec(),
    };
    let payload_cbor = msg
        .encode()
        .map_err(|e| NodeError::MalformedPayload(format!("{}", e)))?;

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
pub fn send_recv_app_data<T: Transport>(
    transport: &mut T,
    identity: &NodeIdentity,
    current_seq: &mut u64,
    blob: &[u8],
    timeout_ms: u32,
    hmac: &impl HmacProvider,
) -> NodeResult<Vec<u8>> {
    let seq = *current_seq;

    let msg = NodeMessage::AppData {
        blob: blob.to_vec(),
    };
    let payload_cbor = msg
        .encode()
        .map_err(|e| NodeError::MalformedPayload(format!("{}", e)))?;

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
    // a valid APP_DATA_REPLY arrives or the timeout expires (ND-0800/ND-0801).
    loop {
        match transport.recv(timeout_ms)? {
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
        pub program_updated_flag: bool,
    }

    impl MockStorage {
        pub fn new() -> Self {
            Self {
                key: None,
                schedule_interval: 60,
                active_partition: 0,
                programs: [None, None],
                early_wake_flag: false,
                program_updated_flag: false,
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
        fn take_program_updated_flag(&mut self) -> bool {
            let v = self.program_updated_flag;
            self.program_updated_flag = false;
            v
        }
        fn set_program_updated_flag(&mut self) -> NodeResult<()> {
            self.program_updated_flag = true;
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
        execute_result: Result<u64, BpfError>,
    }

    impl MockBpfInterpreter {
        fn new() -> Self {
            Self {
                loaded: false,
                execute_result: Ok(0),
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
        fn execute(&mut self, _ctx_ptr: u64, _budget: u64) -> Result<u64, BpfError> {
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
            &TestHmac,
        );

        // Wrong-nonce frame is discarded per ND-0800/ND-0801; falls through to timeout
        assert!(matches!(result, Err(NodeError::Timeout)));
    }
}
