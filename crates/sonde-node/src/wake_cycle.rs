// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Wake cycle state machine.
//!
//! Implements the core node lifecycle per protocol.md §6.1:
//! `boot → WAKE → COMMAND → dispatch → (transfer/execute) → sleep`

use sonde_protocol::{
    BoardLayout, CommandPayload, DecodeError, FrameHeader, GatewayMessage, NodeMessage,
    Sha256Provider, MSG_APP_DATA, MSG_APP_DATA_REPLY, MSG_CHUNK, MSG_COMMAND, MSG_GET_CHUNK,
    MSG_PROGRAM_ACK, MSG_WAKE,
};

use crate::async_queue::AsyncQueue;
use crate::bpf_helpers::{ProgramClass, SondeContext};
use crate::bpf_runtime::BpfInterpreter;
use crate::error::{NodeError, NodeResult};
use crate::hal::Hal;
use crate::key_store::NodeIdentity;
use crate::map_storage::MapStorage;
use crate::peer_request::peer_request_exchange;
use crate::program_store::{LoadedProgram, ProgramStore};
use crate::sleep::{SleepManager, WakeReason};
use crate::traits::{Clock, PlatformStorage, Rng, Transport};
use crate::FIRMWARE_ABI_VERSION;

/// Retry and timing constants shared by WAKE/COMMAND and GET_CHUNK exchanges
/// (protocol.md §9, node-requirements.md ND-0700/ND-0701/ND-0702).
const MAX_RETRIES: u32 = 3;
const RETRY_DELAY_MS: u32 = 400;
const RESPONSE_TIMEOUT_MS: u32 = 200;

/// Default instruction budget for BPF execution.
const DEFAULT_INSTRUCTION_BUDGET: u64 = 100_000;
const BATTERY_FALLBACK_MV: u32 = 3300;
const SENSOR_SETTLE_MS: u32 = 10;
const ADC_FULL_SCALE_MV: u32 = 2500;
const BATTERY_DIVIDER_RATIO: u32 = 2;

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

// The ESP32-C3 ADC1 path used by this firmware exposes GPIO0-4 only.
fn is_supported_battery_adc_gpio(pin: u8) -> bool {
    matches!(pin, 0..=4)
}

fn gpio_to_adc_channel(pin: u8) -> Option<u32> {
    if is_supported_battery_adc_gpio(pin) {
        Some(pin as u32)
    } else {
        None
    }
}

fn capture_current_cycle_battery(
    hal: &mut dyn Hal,
    board_layout: &BoardLayout,
    clock: &dyn Clock,
) -> u32 {
    if let Some(sensor_enable) = board_layout.sensor_enable {
        if hal.gpio_write(sensor_enable as u32, 0) < 0 {
            log::warn!("failed to assert sensor_enable GPIO {}", sensor_enable);
        } else {
            clock.delay_ms(SENSOR_SETTLE_MS);
        }
    }

    let Some(battery_pin) = board_layout.battery_adc else {
        return BATTERY_FALLBACK_MV;
    };

    let Some(channel) = gpio_to_adc_channel(battery_pin) else {
        log::warn!(
            "battery_adc GPIO {} is not ADC-capable on ESP32-C3; using fallback {} mV",
            battery_pin,
            BATTERY_FALLBACK_MV
        );
        return BATTERY_FALLBACK_MV;
    };

    let raw = hal.adc_read(channel);
    if raw < 0 {
        log::warn!(
            "battery ADC sample failed on GPIO {} (channel {}); using fallback {} mV",
            battery_pin,
            channel,
            BATTERY_FALLBACK_MV
        );
        return BATTERY_FALLBACK_MV;
    }

    let sensed_mv = (raw as u32).saturating_mul(ADC_FULL_SCALE_MV) / 4095;
    sensed_mv.saturating_mul(BATTERY_DIVIDER_RATIO)
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

/// Extract `starting_seq` and `timestamp_ms` from a COMMAND payload
/// with an unknown `command_type`, treating it as NOP.
///
/// Per ND-0202, unknown command types are treated as NOP. The node
/// still needs `starting_seq` and `timestamp_ms` from the CBOR map
/// to maintain session sequencing and time reference.
fn decode_command_as_nop(
    payload: &[u8],
) -> NodeResult<(u64, u64, CommandPayload, Option<Vec<u8>>)> {
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

    Ok((starting_seq, timestamp_ms, CommandPayload::Nop, None))
}

use sonde_protocol::{decode_frame, encode_frame, open_frame, AeadProvider};

/// Decode and authenticate a raw frame using AES-256-GCM.
///
/// Returns `(header, plaintext_payload)` on success.
fn decode_verify_frame<A: AeadProvider + ?Sized, S: Sha256Provider + ?Sized>(
    raw: &[u8],
    psk: &[u8; 32],
    aead: &A,
    sha: &S,
) -> NodeResult<(FrameHeader, Vec<u8>)> {
    let decoded =
        decode_frame(raw).map_err(|_| NodeError::MalformedPayload("frame decode failed"))?;
    let header = decoded.header.clone();
    let payload = open_frame(&decoded, psk, aead, sha).map_err(|_| NodeError::AuthFailure)?;
    Ok((header, payload))
}

/// Authenticate and decode WAKE → COMMAND exchange.
///
/// Encodes the WAKE frame with AES-256-GCM and decodes the COMMAND
/// response using AEAD authentication instead of HMAC-SHA256.
///
/// If `wake_blob` is `Some`, the blob is piggybacked on the WAKE message.
/// The returned `Option<Vec<u8>>` is the downlink blob from the COMMAND.
#[allow(clippy::too_many_arguments)]
pub fn wake_command_exchange<T: Transport, A: AeadProvider, S: Sha256Provider>(
    transport: &mut T,
    identity: &NodeIdentity,
    wake_nonce: u64,
    program_hash: &[u8],
    battery_mv: u32,
    clock: &dyn Clock,
    aead: &A,
    sha: &S,
    wake_blob: Option<Vec<u8>>,
) -> NodeResult<(u64, u64, CommandPayload, Option<Vec<u8>>)> {
    let wake_msg = NodeMessage::Wake {
        firmware_abi_version: FIRMWARE_ABI_VERSION,
        program_hash: program_hash.to_vec(),
        battery_mv,
        firmware_version: env!("CARGO_PKG_VERSION").into(),
        blob: wake_blob,
    };
    let payload_cbor = wake_msg
        .encode()
        .map_err(|_| NodeError::MalformedPayload("WAKE message encode failed"))?;

    let header = FrameHeader {
        key_hint: identity.key_hint,
        msg_type: MSG_WAKE,
        nonce: wake_nonce,
    };

    let frame = encode_frame(&header, &payload_cbor, &identity.psk, aead, sha)
        .map_err(|_| NodeError::MalformedPayload("frame encode failed"))?;

    for attempt in 0..=MAX_RETRIES {
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

        match transport.recv(RESPONSE_TIMEOUT_MS)? {
            Some(raw_response) => {
                match verify_and_decode_command(&raw_response, identity, wake_nonce, aead, sha) {
                    Ok(result) => return Ok(result),
                    Err(e) => {
                        log::warn!("COMMAND verification failed: {} (ND-1009)", e);
                        continue;
                    }
                }
            }
            None => continue,
        }
    }

    Err(NodeError::WakeRetriesExhausted)
}

/// Verify and decode a COMMAND frame.
fn verify_and_decode_command<A: AeadProvider, S: Sha256Provider>(
    raw: &[u8],
    identity: &NodeIdentity,
    expected_nonce: u64,
    aead: &A,
    sha: &S,
) -> NodeResult<(u64, u64, CommandPayload, Option<Vec<u8>>)> {
    let (header, payload) = decode_verify_frame(raw, &identity.psk, aead, sha)?;

    if header.msg_type != MSG_COMMAND {
        return Err(NodeError::UnexpectedMsgType(header.msg_type));
    }

    if header.nonce != expected_nonce {
        return Err(NodeError::ResponseBindingMismatch);
    }

    let gateway_msg = match GatewayMessage::decode(header.msg_type, &payload) {
        Ok(msg) => msg,
        Err(DecodeError::InvalidCommandType(_)) => {
            return decode_command_as_nop(&payload);
        }
        Err(_) => return Err(NodeError::MalformedPayload("COMMAND payload decode failed")),
    };

    match gateway_msg {
        GatewayMessage::Command {
            starting_seq,
            timestamp_ms,
            payload,
            blob,
        } => Ok((starting_seq, timestamp_ms, payload, blob)),
        _ => Err(NodeError::UnexpectedMsgType(header.msg_type)),
    }
}

/// Send APP_DATA frame.
pub fn send_app_data<
    T: Transport + ?Sized,
    A: AeadProvider + ?Sized,
    S: Sha256Provider + ?Sized,
>(
    transport: &mut T,
    identity: &NodeIdentity,
    current_seq: &mut u64,
    blob: &[u8],
    aead: &A,
    sha: &S,
) -> NodeResult<()> {
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

    let frame = encode_frame(&header, &payload_cbor, &identity.psk, aead, sha)
        .map_err(|_| NodeError::MalformedPayload("frame encode failed"))?;

    transport.send(&frame)?;
    *current_seq += 1;
    Ok(())
}

/// Send APP_DATA and wait for reply.
#[allow(clippy::too_many_arguments)]
pub fn send_recv_app_data<
    T: Transport + ?Sized,
    C: Clock + ?Sized,
    A: AeadProvider + ?Sized,
    S: Sha256Provider + ?Sized,
>(
    transport: &mut T,
    identity: &NodeIdentity,
    current_seq: &mut u64,
    blob: &[u8],
    timeout_ms: u32,
    clock: &C,
    aead: &A,
    sha: &S,
) -> NodeResult<Vec<u8>> {
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

    let frame = encode_frame(&header, &payload_cbor, &identity.psk, aead, sha)
        .map_err(|_| NodeError::MalformedPayload("frame encode failed"))?;

    transport.send(&frame)?;
    *current_seq += 1;

    let deadline = clock.elapsed_ms().saturating_add(timeout_ms as u64);
    loop {
        let now = clock.elapsed_ms();
        if now >= deadline {
            return Err(NodeError::Timeout);
        }
        let remaining = (deadline - now) as u32;
        match transport.recv(remaining)? {
            Some(raw_response) => {
                let (hdr, payload) =
                    match decode_verify_frame(&raw_response, &identity.psk, aead, sha) {
                        Ok(result) => result,
                        Err(_) => continue,
                    };

                if hdr.msg_type != MSG_APP_DATA_REPLY {
                    continue;
                }

                if hdr.nonce != seq {
                    continue;
                }

                let gateway_msg = match GatewayMessage::decode(hdr.msg_type, &payload) {
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

/// Send PROGRAM_ACK frame.
fn send_program_ack<T: Transport, A: AeadProvider, S: Sha256Provider>(
    transport: &mut T,
    identity: &NodeIdentity,
    current_seq: &mut u64,
    program_hash: &[u8],
    aead: &A,
    sha: &S,
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

    let frame = encode_frame(&header, &payload_cbor, &identity.psk, aead, sha)
        .map_err(|_| NodeError::MalformedPayload("frame encode failed"))?;

    transport.send(&frame)?;
    *current_seq += 1;
    Ok(())
}

fn verify_and_decode_chunk<A: AeadProvider, S: Sha256Provider>(
    raw: &[u8],
    identity: &NodeIdentity,
    expected_seq: u64,
    expected_index: u32,
    aead: &A,
    sha: &S,
) -> NodeResult<Vec<u8>> {
    let (header, payload) = decode_verify_frame(raw, &identity.psk, aead, sha)?;

    if header.msg_type != MSG_CHUNK {
        return Err(NodeError::UnexpectedMsgType(header.msg_type));
    }

    if header.nonce != expected_seq {
        return Err(NodeError::ResponseBindingMismatch);
    }

    let gateway_msg = GatewayMessage::decode(header.msg_type, &payload)
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
        _ => Err(NodeError::UnexpectedMsgType(header.msg_type)),
    }
}

#[allow(clippy::too_many_arguments)]
fn get_chunk_with_retry<T: Transport, A: AeadProvider, S: Sha256Provider>(
    transport: &mut T,
    identity: &NodeIdentity,
    current_seq: &mut u64,
    chunk_index: u32,
    clock: &dyn Clock,
    aead: &A,
    sha: &S,
) -> NodeResult<Vec<u8>> {
    let get_msg = NodeMessage::GetChunk { chunk_index };
    let payload_cbor = get_msg
        .encode()
        .map_err(|_| NodeError::MalformedPayload("GET_CHUNK message encode failed"))?;

    for attempt in 0..=MAX_RETRIES {
        if attempt > 0 {
            clock.delay_ms(RETRY_DELAY_MS);
        }

        let attempt_seq = *current_seq;

        let header = FrameHeader {
            key_hint: identity.key_hint,
            msg_type: MSG_GET_CHUNK,
            nonce: attempt_seq,
        };

        let frame = encode_frame(&header, &payload_cbor, &identity.psk, aead, sha)
            .map_err(|_| NodeError::MalformedPayload("frame encode failed"))?;

        transport.send(&frame)?;
        log::debug!(
            "GET_CHUNK sent chunk_index={} attempt={} (ND-1011)",
            chunk_index,
            attempt
        );
        *current_seq += 1;

        // Inner loop: drain stale wrong-type frames without consuming a retry
        // attempt (F-003), but keep each attempt bounded by the original
        // RESPONSE_TIMEOUT_MS budget so a flood of stale frames cannot keep the
        // node awake indefinitely.
        let deadline = clock
            .elapsed_ms()
            .saturating_add(RESPONSE_TIMEOUT_MS as u64);
        loop {
            let now = clock.elapsed_ms();
            if now >= deadline {
                break;
            }
            let remaining = (deadline - now) as u32;
            match transport.recv(remaining)? {
                None => break, // timeout — count as a retry attempt
                Some(raw_response) => {
                    match verify_and_decode_chunk(
                        &raw_response,
                        identity,
                        attempt_seq,
                        chunk_index,
                        aead,
                        sha,
                    ) {
                        Ok(data) => {
                            log::debug!(
                                "CHUNK received chunk_index={} len={} (ND-1011)",
                                chunk_index,
                                data.len()
                            );
                            return Ok(data);
                        }
                        Err(NodeError::UnexpectedMsgType(_)) => {
                            // Stale frame from a different exchange — discard
                            // and keep waiting without burning a retry attempt.
                            log::debug!(
                                "GET_CHUNK: discarding stale frame (wrong msg_type) chunk_index={}",
                                chunk_index
                            );
                        }
                        Err(_) => break, // auth failure, seq mismatch, etc.
                    }
                }
            }
        }
    }

    Err(NodeError::ChunkTransferFailed { chunk_index })
}

/// Chunked program transfer.
#[allow(clippy::too_many_arguments)]
pub fn chunked_transfer<T: Transport, A: AeadProvider, S: Sha256Provider>(
    transport: &mut T,
    identity: &NodeIdentity,
    current_seq: &mut u64,
    program_size: u32,
    chunk_size: u32,
    chunk_count: u32,
    max_image_size: usize,
    clock: &dyn Clock,
    aead: &A,
    sha: &S,
) -> NodeResult<Vec<u8>> {
    let program_size_usize = program_size as usize;
    let chunk_size_usize = chunk_size as usize;

    if program_size_usize > max_image_size {
        return Err(NodeError::MalformedPayload(
            "program_size exceeds maximum image size",
        ));
    }

    if chunk_size == 0 {
        return Err(NodeError::MalformedPayload("chunk_size is zero"));
    }

    let expected_chunk_count = sonde_protocol::chunk_count(program_size_usize, chunk_size_usize);
    if expected_chunk_count != Some(chunk_count) {
        return Err(NodeError::MalformedPayload(
            "chunk_count does not match program_size / chunk_size",
        ));
    }

    let mut image_data: Vec<u8> = Vec::with_capacity(program_size_usize);

    for ci in 0..chunk_count {
        let chunk_data =
            get_chunk_with_retry(transport, identity, current_seq, ci, clock, aead, sha)?;

        if chunk_data.len() > chunk_size_usize {
            return Err(NodeError::MalformedPayload(
                "received chunk larger than declared chunk_size",
            ));
        }

        if image_data.len() + chunk_data.len() > program_size_usize {
            return Err(NodeError::MalformedPayload(
                "received data exceeds declared program_size",
            ));
        }

        image_data.extend_from_slice(&chunk_data);
    }

    if image_data.len() != program_size_usize {
        return Err(NodeError::MalformedPayload(
            "assembled program size does not match declared program_size",
        ));
    }

    Ok(image_data)
}

/// Execute a complete wake cycle using AES-256-GCM frame encoding.
///
/// Encodes/decodes all radio frames using AES-256-GCM (AEAD).  The
/// AEAD providers are installed into the BPF dispatch context so that
/// `send()` / `send_recv()` helpers also produce AEAD-authenticated
/// APP_DATA frames.
#[allow(clippy::too_many_arguments)]
pub fn run_wake_cycle<T, S, I, A, H>(
    transport: &mut T,
    storage: &mut S,
    hal: &mut (dyn Hal + 'static),
    rng: &mut dyn Rng,
    clock: &(dyn Clock + 'static),
    board_layout: &BoardLayout,
    interpreter: &mut I,
    map_storage: &mut MapStorage,
    sha: &H,
    aead: &A,
    async_queue: &mut AsyncQueue,
) -> WakeCycleOutcome
where
    T: Transport + 'static,
    S: PlatformStorage,
    I: BpfInterpreter,
    A: AeadProvider + 'static,
    H: Sha256Provider + 'static,
{
    // 1. Load identity
    let identity = match storage.read_key() {
        Some((key_hint, psk)) => NodeIdentity { key_hint, psk },
        None => return WakeCycleOutcome::Unpaired,
    };

    // 1b. RNG health check (ND-0304 AC3).
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

    // 3a. PEER_REQUEST/PEER_ACK exchange via AEAD (ND-0909–ND-0913).
    // The encrypted_payload is a complete ESP-NOW frame built by the
    // phone — the node transmits it verbatim.
    if !storage.read_reg_complete() {
        if let Some(encrypted_payload) = storage.read_peer_payload() {
            match peer_request_exchange(
                transport,
                storage,
                &identity,
                &encrypted_payload,
                clock,
                aead,
                sha,
            ) {
                Ok(true) => {
                    // Registration complete — fall through to normal WAKE cycle.
                }
                Ok(false) => {
                    // Timeout — sleep and retry next wake cycle (ND-0910/ND-0911).
                    return log_and_sleep(&sleep_mgr);
                }
                Err(e) => {
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
    let (program_hash, mut resident_image_bytes) = {
        let program_store = ProgramStore::new(storage);
        program_store.load_active_raw(sha)
    };

    // 5. Generate WAKE nonce
    let wake_nonce = rng.random_u64();
    let wake_battery_mv = storage.read_last_battery_mv().unwrap_or(0);

    // 5a. Check async queue for WAKE piggybacking.
    // The queue persists across wake cycles in RTC slow SRAM (ESP) or
    // on the heap (tests), so blobs queued by BPF in cycle N are
    // available here in cycle N+1. Survives deep sleep; lost on reboot.
    let wake_blob = {
        let candidate = async_queue.single_for_piggyback(sonde_protocol::MAX_PAYLOAD_SIZE);
        if let Some(blob) = candidate {
            let trial = NodeMessage::Wake {
                firmware_abi_version: FIRMWARE_ABI_VERSION,
                program_hash: program_hash.clone(),
                battery_mv: wake_battery_mv,
                firmware_version: env!("CARGO_PKG_VERSION").into(),
                blob: Some(blob.to_vec()),
            };
            match trial.encode() {
                Ok(encoded) if encoded.len() <= sonde_protocol::MAX_PAYLOAD_SIZE => {
                    // Clone the blob for WAKE; queue is cleared after successful send.
                    Some(blob.to_vec())
                }
                _ => None,
            }
        } else {
            None
        }
    };
    let piggybacked = wake_blob.is_some();

    // 6. Send WAKE, await COMMAND (with retries) via AEAD
    let command_result = wake_command_exchange(
        transport,
        &identity,
        wake_nonce,
        &program_hash,
        wake_battery_mv,
        clock,
        aead,
        sha,
        wake_blob,
    );

    let (starting_seq, timestamp_ms, command_payload, command_blob) = match command_result {
        Ok(cmd) => {
            // WAKE/COMMAND succeeded — erase peer_payload if still present (ND-0914).
            if storage.has_peer_payload() {
                if let Err(e) = storage.erase_peer_payload() {
                    log::warn!("failed to erase peer_payload after WAKE success: {}", e);
                }
            }
            // Consume the piggybacked async blob so it is not resent.
            // single_for_piggyback() only reads without removing.
            if piggybacked {
                async_queue.clear();
            }
            cmd
        }
        Err(e) => {
            // WAKE retries exhausted or transport error.
            log::warn!("WAKE/COMMAND failed: {} — sleeping (ND-1009)", e);
            // Self-healing (ND-0915).
            if storage.read_reg_complete() && storage.has_peer_payload() {
                if let Err(e) = storage.write_reg_complete(false) {
                    log::warn!("failed to clear reg_complete after WAKE failure: {}", e);
                }
            }
            return log_and_sleep(&sleep_mgr);
        }
    };

    let mut current_seq = starting_seq;
    let current_battery_mv = capture_current_cycle_battery(hal, board_layout, clock);
    if let Err(e) = storage.write_last_battery_mv(current_battery_mv) {
        log::warn!("failed to persist battery reading for next wake: {}", e);
    }

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
    let mut loaded_program: Option<LoadedProgram> = None;

    let is_ephemeral = matches!(&command_payload, CommandPayload::RunEphemeral { .. });

    match command_payload {
        CommandPayload::Nop => {
            // 8a. Drain remaining async queue blobs as APP_DATA.
            // Piggybacking (step 5a) handles at most one blob; send the
            // rest now, before BPF execution, so that blobs queued by the
            // current cycle's BPF stay in the queue for piggybacking on the
            // next WAKE.  Only drain on NOP cycles — non-NOP commands have
            // their own radio work and the queue can wait.
            if !async_queue.is_empty() {
                let pending = async_queue.drain();
                for queued_blob in &pending {
                    if let Err(e) = send_app_data(
                        transport,
                        &identity,
                        &mut current_seq,
                        queued_blob,
                        aead,
                        sha,
                    ) {
                        log::warn!("async queue APP_DATA send failed: {}", e);
                        break;
                    }
                }
            }
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
            resident_image_bytes = None;
            // New program load invalidates any blobs queued by the old program.
            async_queue.clear();

            let max_image_size = if is_ephemeral {
                MAX_EPHEMERAL_IMAGE_SIZE
            } else {
                MAX_RESIDENT_IMAGE_SIZE
            };

            // Chunked transfer via AEAD
            let transfer_result = chunked_transfer(
                transport,
                &identity,
                &mut current_seq,
                program_size,
                chunk_size,
                chunk_count,
                max_image_size,
                clock,
                aead,
                sha,
            );

            match transfer_result {
                Ok(image_bytes) => {
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
                            // PROGRAM_ACK via AEAD
                            if send_program_ack(
                                transport,
                                &identity,
                                &mut current_seq,
                                &program.hash,
                                aead,
                                sha,
                            )
                            .is_err()
                            {
                                return log_and_sleep(&sleep_mgr);
                            }

                            if !is_ephemeral {
                                sleep_mgr.set_wake_reason(WakeReason::ProgramUpdate);
                            }
                            loaded_program = Some(program);
                        }
                        Err(e) => {
                            log::warn!("program install failed: {}", e);
                            return log_and_sleep(&sleep_mgr);
                        }
                    }
                }
                Err(e) => {
                    log::warn!("chunk transfer failed: {}", e);
                    return log_and_sleep(&sleep_mgr);
                }
            }
        }
    }

    // 9. BPF execution
    let resident_installed_this_cycle = loaded_program.as_ref().is_some_and(|p| !p.is_ephemeral);

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
            if !program.map_defs.is_empty() {
                return log_and_sleep(&sleep_mgr);
            }
        } else if resident_installed_this_cycle || !map_storage.layout_matches(&program.map_defs) {
            if map_storage.allocate(&program.map_defs).is_err() {
                return log_and_sleep(&sleep_mgr);
            }
            map_storage.apply_initial_data(&program.map_initial_data);
        }

        let map_ptrs = map_storage.map_pointers().to_vec();

        let elapsed_since_command = clock.elapsed_ms().saturating_sub(command_received_at);
        let battery_mv_clamped = if current_battery_mv > u16::MAX as u32 {
            u16::MAX
        } else {
            current_battery_mv as u16
        };
        let ctx = SondeContext {
            timestamp: timestamp_ms.saturating_add(elapsed_since_command),
            battery_mv: battery_mv_clamped,
            firmware_abi_version: u16::try_from(FIRMWARE_ABI_VERSION)
                .expect("FIRMWARE_ABI_VERSION must fit in u16"),
            wake_reason: sleep_mgr.wake_reason() as u8,
            _padding: [0; 3],
            data_start: command_blob.as_ref().map_or(0, |b| b.as_ptr() as u64),
            data_end: command_blob
                .as_ref()
                .map_or(0, |b| unsafe { b.as_ptr().add(b.len()) } as u64),
        };

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
                &identity as *const NodeIdentity,
                &mut current_seq as *mut u64,
                program_class,
                &mut trace_log as *mut Vec<String>,
                async_queue as *mut AsyncQueue,
                timestamp_ms,
                command_received_at,
                current_battery_mv,
                aead as *const dyn AeadProvider,
                sha as *const dyn Sha256Provider,
            );
        }
        let _guard = crate::bpf_dispatch::DispatchGuard;

        let exec_result = match crate::bpf_dispatch::register_all(interpreter) {
            Ok(()) => {
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

        match &exec_result {
            Ok(rc) => log::info!("BPF execution completed rc={}", rc),
            Err(err) => log::info!("BPF execution failed: {}", err),
        }
        let _ = exec_result;

        flush_trace_log(&trace_log);
    }

    // 10. Determine sleep duration
    if sleep_mgr.will_wake_early() {
        if let Err(err) = storage.set_early_wake_flag() {
            log::warn!("Failed to set early-wake flag: {:?}", err);
        }
    }

    log_and_sleep(&sleep_mgr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bpf_runtime::BpfError;
    use crate::error::NodeResult;
    use crate::traits::PlatformStorage;
    use std::collections::VecDeque;

    // --- Mock transport ---

    struct MockTransport {
        /// Responses queued for recv()
        inbound: VecDeque<Option<Vec<u8>>>,
        /// Frames captured from send()
        outbound: Vec<Vec<u8>>,
        /// Timeout values passed to recv(), recorded for assertions.
        recv_timeouts: Vec<u32>,
    }

    impl MockTransport {
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

    impl Transport for MockTransport {
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
        pub last_battery_mv: Option<u32>,
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
                last_battery_mv: None,
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
        fn read_last_battery_mv(&self) -> Option<u32> {
            self.last_battery_mv
        }
        fn write_last_battery_mv(&mut self, battery_mv: u32) -> NodeResult<()> {
            self.last_battery_mv = Some(battery_mv);
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
        fn spi_transfer(&mut self, _h: u32, _buf: &mut [u8]) -> i32 {
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

    #[test]
    fn gpio_to_adc_channel_only_maps_esp32c3_adc1_pins() {
        assert!(is_supported_battery_adc_gpio(0));
        assert!(is_supported_battery_adc_gpio(4));
        assert!(!is_supported_battery_adc_gpio(5));
        assert_eq!(gpio_to_adc_channel(0), Some(0));
        assert_eq!(gpio_to_adc_channel(4), Some(4));
        assert_eq!(gpio_to_adc_channel(5), None);
        assert_eq!(gpio_to_adc_channel(21), None);
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

    // ------------------------------------------------------------------
    // AES-256-GCM frame processing tests (feature-gated)
    // ------------------------------------------------------------------

    mod aead_tests {
        use super::*;
        use crate::node_aead::NodeAead;
        use sonde_protocol::{decode_frame, encode_frame, open_frame};

        /// Encode a COMMAND response using AES-GCM for test fixtures.
        fn make_command(psk: &[u8; 32], nonce: u64, payload: &CommandPayload) -> Vec<u8> {
            make_command_with_seq(psk, nonce, 1, 1000, payload)
        }

        fn make_command_with_seq(
            psk: &[u8; 32],
            nonce: u64,
            starting_seq: u64,
            timestamp_ms: u64,
            payload: &CommandPayload,
        ) -> Vec<u8> {
            let aead = NodeAead;
            let sha = crate::crypto::SoftwareSha256;
            let msg = GatewayMessage::Command {
                starting_seq,
                timestamp_ms,
                payload: payload.clone(),
                blob: None,
            };
            let payload_cbor = msg.encode().unwrap();
            let header = FrameHeader {
                key_hint: sonde_protocol::key_hint_from_psk(psk, &sha),
                msg_type: MSG_COMMAND,
                nonce,
            };
            encode_frame(&header, &payload_cbor, psk, &aead, &sha).unwrap()
        }

        #[test]
        fn wake_command_exchange_round_trip() {
            let psk = [0x42u8; 32];
            let sha = crate::crypto::SoftwareSha256;
            let aead = NodeAead;
            let key_hint = sonde_protocol::key_hint_from_psk(&psk, &sha);
            let identity = NodeIdentity { key_hint, psk };
            let clock = MockClock;
            let mut transport = MockTransport::new();

            let command_frame = make_command(&psk, 42, &CommandPayload::Nop);
            transport.queue_response(Some(command_frame));

            let result = wake_command_exchange(
                &mut transport,
                &identity,
                42,
                &[0u8; 32],
                3300,
                &clock,
                &aead,
                &sha,
                None,
            );

            assert!(result.is_ok(), "AEAD wake/command exchange should succeed");
            let (starting_seq, timestamp_ms, cmd, _blob) = result.unwrap();
            assert_eq!(starting_seq, 1);
            assert_eq!(timestamp_ms, 1000);
            assert_eq!(cmd, CommandPayload::Nop);

            // Verify recv was called with RESPONSE_TIMEOUT_MS (ND-0702)
            assert_eq!(transport.recv_timeouts.len(), 1);
            assert_eq!(transport.recv_timeouts[0], RESPONSE_TIMEOUT_MS);

            // Verify outbound WAKE frame is AEAD-encoded
            assert_eq!(transport.outbound.len(), 1);
            let decoded = decode_frame(&transport.outbound[0]).unwrap();
            assert_eq!(decoded.header.msg_type, MSG_WAKE);
            let wake_payload = open_frame(&decoded, &psk, &aead, &sha).unwrap();
            assert!(!wake_payload.is_empty());
        }

        /// T-N702: Response timeout — 200ms boundary.
        ///
        /// First recv returns None (simulating response delayed beyond
        /// RESPONSE_TIMEOUT_MS). Node retries; second recv succeeds.
        /// Validates that recv is called with the 200ms timeout and
        /// that the retry mechanism recovers.
        #[test]
        fn t_n702_response_timeout_boundary() {
            let psk = [0x42u8; 32];
            let sha = crate::crypto::SoftwareSha256;
            let aead = NodeAead;
            let key_hint = sonde_protocol::key_hint_from_psk(&psk, &sha);
            let identity = NodeIdentity { key_hint, psk };
            let clock = MockClock;
            let mut transport = MockTransport::new();

            // First attempt: timeout (None).
            transport.queue_response(None);
            // Retry: succeed with a valid COMMAND.
            let command_frame = make_command(&psk, 42, &CommandPayload::Nop);
            transport.queue_response(Some(command_frame));

            let result = wake_command_exchange(
                &mut transport,
                &identity,
                42,
                &[0u8; 32],
                3300,
                &clock,
                &aead,
                &sha,
                None,
            );

            assert!(result.is_ok(), "retry after timeout should succeed");

            // Verify both recv calls used RESPONSE_TIMEOUT_MS = 200 (ND-0702).
            assert_eq!(transport.recv_timeouts.len(), 2, "initial + 1 retry");
            assert!(
                transport
                    .recv_timeouts
                    .iter()
                    .all(|&t| t == RESPONSE_TIMEOUT_MS),
                "all recv calls must use RESPONSE_TIMEOUT_MS (200 ms)"
            );
        }

        #[test]
        fn send_app_data_round_trip() {
            let psk = [0x42u8; 32];
            let sha = crate::crypto::SoftwareSha256;
            let aead = NodeAead;
            let key_hint = sonde_protocol::key_hint_from_psk(&psk, &sha);
            let identity = NodeIdentity { key_hint, psk };
            let mut transport = MockTransport::new();
            let mut seq = 0u64;

            let blob = b"hello";
            let result = send_app_data(&mut transport, &identity, &mut seq, blob, &aead, &sha);

            assert!(result.is_ok());
            assert_eq!(seq, 1);
            assert_eq!(transport.outbound.len(), 1);

            // Verify the outbound frame decrypts correctly
            let decoded = decode_frame(&transport.outbound[0]).unwrap();
            assert_eq!(decoded.header.msg_type, MSG_APP_DATA);
            let payload = open_frame(&decoded, &psk, &aead, &sha).unwrap();
            assert!(!payload.is_empty());
        }

        #[test]
        fn send_recv_app_data_round_trip() {
            let psk = [0x42u8; 32];
            let sha = crate::crypto::SoftwareSha256;
            let aead = NodeAead;
            let key_hint = sonde_protocol::key_hint_from_psk(&psk, &sha);
            let identity = NodeIdentity { key_hint, psk };
            let clock = MockClock;
            let mut transport = MockTransport::new();
            let mut seq = 0u64;

            // Build an APP_DATA_REPLY response
            let reply_msg = GatewayMessage::AppDataReply {
                blob: b"reply".to_vec(),
            };
            let reply_cbor = reply_msg.encode().unwrap();
            let reply_header = FrameHeader {
                key_hint,
                msg_type: MSG_APP_DATA_REPLY,
                nonce: 0, // echoes the seq we'll send
            };
            let reply_frame = encode_frame(&reply_header, &reply_cbor, &psk, &aead, &sha).unwrap();
            transport.queue_response(Some(reply_frame));

            let result = send_recv_app_data(
                &mut transport,
                &identity,
                &mut seq,
                b"request",
                5000,
                &clock,
                &aead,
                &sha,
            );

            assert!(result.is_ok());
            assert_eq!(result.unwrap(), b"reply");
            assert_eq!(seq, 1);
        }

        #[test]
        fn wrong_key_fails() {
            let psk = [0x42u8; 32];
            let wrong_psk = [0x99u8; 32];
            let sha = crate::crypto::SoftwareSha256;
            let aead = NodeAead;
            let key_hint = sonde_protocol::key_hint_from_psk(&psk, &sha);
            let identity = NodeIdentity {
                key_hint,
                psk: wrong_psk,
            };
            let clock = MockClock;
            let mut transport = MockTransport::new();

            // Encode with correct PSK, but identity uses wrong PSK
            let command_frame = make_command(&psk, 42, &CommandPayload::Nop);
            transport.queue_response(Some(command_frame.clone()));
            transport.queue_response(Some(command_frame.clone()));
            transport.queue_response(Some(command_frame.clone()));
            transport.queue_response(Some(command_frame));

            let result = wake_command_exchange(
                &mut transport,
                &identity,
                42,
                &[0u8; 32],
                3300,
                &clock,
                &aead,
                &sha,
                None,
            );

            assert!(
                result.is_err(),
                "wrong key must cause authentication failure"
            );
        }

        #[test]
        fn send_program_ack_succeeds() {
            let psk = [0x42u8; 32];
            let sha = crate::crypto::SoftwareSha256;
            let aead = NodeAead;
            let key_hint = sonde_protocol::key_hint_from_psk(&psk, &sha);
            let identity = NodeIdentity { key_hint, psk };
            let mut transport = MockTransport::new();
            let mut seq = 5u64;

            let result = send_program_ack(
                &mut transport,
                &identity,
                &mut seq,
                &[0xABu8; 32],
                &aead,
                &sha,
            );

            assert!(result.is_ok());
            assert_eq!(seq, 6);
            assert_eq!(transport.outbound.len(), 1);

            let decoded = decode_frame(&transport.outbound[0]).unwrap();
            assert_eq!(decoded.header.msg_type, MSG_PROGRAM_ACK);
            assert_eq!(decoded.header.nonce, 5);
        }

        // --- AEAD chunked transfer tests ---

        /// Build a CHUNK response frame using AEAD encryption.
        fn build_chunk_response(
            psk: &[u8; 32],
            echo_seq: u64,
            chunk_index: u32,
            chunk_data: &[u8],
        ) -> Vec<u8> {
            let aead = NodeAead;
            let sha = crate::crypto::SoftwareSha256;
            let msg = GatewayMessage::Chunk {
                chunk_index,
                chunk_data: chunk_data.to_vec(),
            };
            let payload_cbor = msg.encode().unwrap();
            let header = FrameHeader {
                key_hint: sonde_protocol::key_hint_from_psk(psk, &sha),
                msg_type: MSG_CHUNK,
                nonce: echo_seq,
            };
            encode_frame(&header, &payload_cbor, psk, &aead, &sha).unwrap()
        }

        #[test]
        fn chunked_transfer_success() {
            let psk = [0x22u8; 32];
            let sha = crate::crypto::SoftwareSha256;
            let aead = NodeAead;
            let key_hint = sonde_protocol::key_hint_from_psk(&psk, &sha);
            let identity = NodeIdentity { key_hint, psk };
            let clock = MockClock;
            let mut transport = MockTransport::new();

            let image = sonde_protocol::ProgramImage {
                bytecode: vec![0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
                maps: vec![],
                map_initial_data: vec![],
            };
            let image_cbor = image.encode_deterministic().unwrap();

            let chunk_size = 10u32;
            let chunk_count =
                sonde_protocol::chunk_count(image_cbor.len(), chunk_size as usize).unwrap();

            let starting_seq = 5000u64;
            let mut current_seq = starting_seq;

            for i in 0..chunk_count {
                let chunk_data = sonde_protocol::get_chunk(&image_cbor, i, chunk_size)
                    .unwrap()
                    .to_vec();
                let seq = starting_seq + i as u64;
                let chunk_frame = build_chunk_response(&psk, seq, i, &chunk_data);
                transport.queue_response(Some(chunk_frame));
            }

            let result = chunked_transfer(
                &mut transport,
                &identity,
                &mut current_seq,
                image_cbor.len() as u32,
                chunk_size,
                chunk_count,
                MAX_RESIDENT_IMAGE_SIZE,
                &clock,
                &aead,
                &sha,
            );

            assert!(result.is_ok(), "AEAD chunked transfer should succeed");
            assert_eq!(result.unwrap(), image_cbor);
        }

        #[test]
        fn chunked_transfer_retry_exhausted() {
            let psk = [0x33u8; 32];
            let sha = crate::crypto::SoftwareSha256;
            let aead = NodeAead;
            let key_hint = sonde_protocol::key_hint_from_psk(&psk, &sha);
            let identity = NodeIdentity { key_hint, psk };
            let clock = MockClock;
            let mut transport = MockTransport::new();
            let mut current_seq = 100u64;

            // No chunk responses — all 4 attempts (1 + 3 retries) timeout
            for _ in 0..4 {
                transport.queue_response(None);
            }

            let result = chunked_transfer(
                &mut transport,
                &identity,
                &mut current_seq,
                20,
                10,
                2,
                MAX_RESIDENT_IMAGE_SIZE,
                &clock,
                &aead,
                &sha,
            );

            assert!(result.is_err(), "should fail after retry exhaustion");

            // Verify all recv calls used RESPONSE_TIMEOUT_MS (ND-0702)
            assert_eq!(transport.recv_timeouts.len(), 4); // 1 initial + 3 retries
            assert!(
                transport
                    .recv_timeouts
                    .iter()
                    .all(|&t| t == RESPONSE_TIMEOUT_MS),
                "all recv calls should use RESPONSE_TIMEOUT_MS"
            );
        }

        #[test]
        fn chunked_transfer_wrong_chunk_index() {
            let psk = [0x44u8; 32];
            let sha = crate::crypto::SoftwareSha256;
            let aead = NodeAead;
            let key_hint = sonde_protocol::key_hint_from_psk(&psk, &sha);
            let identity = NodeIdentity { key_hint, psk };
            let clock = MockClock;
            let mut transport = MockTransport::new();
            let mut current_seq = 200u64;

            // Respond with wrong chunk index (5 instead of 0), then timeouts
            let bad_chunk = build_chunk_response(&psk, current_seq, 5, &[0u8; 10]);
            transport.queue_response(Some(bad_chunk));
            transport.queue_response(None);
            transport.queue_response(None);
            transport.queue_response(None);

            let result = chunked_transfer(
                &mut transport,
                &identity,
                &mut current_seq,
                10,
                10,
                1,
                MAX_RESIDENT_IMAGE_SIZE,
                &clock,
                &aead,
                &sha,
            );

            assert!(
                result.is_err(),
                "wrong chunk index should cause transfer failure"
            );
        }

        /// F-003 regression: a stale frame with the wrong msg_type received
        /// before the correct CHUNK does NOT consume a retry attempt or
        /// advance `current_seq` a second time.
        ///
        /// Before the fix, the `UnexpectedMsgType` error path hit `continue`
        /// in the outer retry loop, burning one retry for a stale frame.
        #[test]
        fn chunked_transfer_stale_wrong_type_frame_does_not_consume_retry() {
            let psk = [0x55u8; 32];
            let sha = crate::crypto::SoftwareSha256;
            let aead = NodeAead;
            let key_hint = sonde_protocol::key_hint_from_psk(&psk, &sha);
            let identity = NodeIdentity { key_hint, psk };
            let clock = MockClock;
            let mut transport = MockTransport::new();

            let image = sonde_protocol::ProgramImage {
                bytecode: vec![0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
                maps: vec![],
                map_initial_data: vec![],
            };
            let image_cbor = image.encode_deterministic().unwrap();
            let chunk_size = image_cbor.len() as u32; // single chunk
            let chunk_count = 1u32;
            let starting_seq = 300u64;
            let mut current_seq = starting_seq;

            // Queue a stale COMMAND frame (wrong msg_type) then the correct CHUNK.
            // The stale frame is encrypted with the same PSK so AEAD succeeds, but
            // msg_type != MSG_CHUNK, triggering UnexpectedMsgType.
            let stale_frame = make_command(&psk, starting_seq, &CommandPayload::Nop);
            let correct_chunk = build_chunk_response(&psk, starting_seq, 0, &image_cbor);
            transport.queue_response(Some(stale_frame));
            transport.queue_response(Some(correct_chunk));

            let result = chunked_transfer(
                &mut transport,
                &identity,
                &mut current_seq,
                image_cbor.len() as u32,
                chunk_size,
                chunk_count,
                MAX_RESIDENT_IMAGE_SIZE,
                &clock,
                &aead,
                &sha,
            );

            assert!(
                result.is_ok(),
                "transfer must succeed after discarding stale frame"
            );
            assert_eq!(result.unwrap(), image_cbor);
            // Only one GET_CHUNK was sent (no retry triggered by the stale frame).
            assert_eq!(
                transport.outbound.len(),
                1,
                "only one GET_CHUNK should be sent"
            );
            // Two recv calls: one for the stale frame, one for the correct chunk.
            assert_eq!(transport.recv_timeouts.len(), 2, "two recv calls expected");
            // current_seq advanced by exactly 1 (one GET_CHUNK sent).
            assert_eq!(
                current_seq,
                starting_seq + 1,
                "seq must advance exactly once"
            );
        }

        struct AdvancingClock(std::cell::Cell<u64>);
        impl Clock for AdvancingClock {
            fn elapsed_ms(&self) -> u64 {
                let now = self.0.get();
                self.0.set(now + 1);
                now
            }

            fn delay_ms(&self, _ms: u32) {}
        }

        struct InfiniteStaleTransport {
            outbound: Vec<Vec<u8>>,
            stale_frame: Vec<u8>,
            recv_calls: usize,
        }

        impl Transport for InfiniteStaleTransport {
            fn send(&mut self, frame: &[u8]) -> NodeResult<()> {
                self.outbound.push(frame.to_vec());
                Ok(())
            }

            fn recv(&mut self, timeout_ms: u32) -> NodeResult<Option<Vec<u8>>> {
                self.recv_calls += 1;
                if timeout_ms == 0 {
                    return Ok(None);
                }
                Ok(Some(self.stale_frame.clone()))
            }
        }

        #[test]
        fn chunked_transfer_stale_wrong_type_frames_are_bounded_per_attempt() {
            let psk = [0x56u8; 32];
            let sha = crate::crypto::SoftwareSha256;
            let aead = NodeAead;
            let key_hint = sonde_protocol::key_hint_from_psk(&psk, &sha);
            let identity = NodeIdentity { key_hint, psk };
            let clock = AdvancingClock(std::cell::Cell::new(0));
            let starting_seq = 700u64;
            let stale_frame = make_command(&psk, starting_seq, &CommandPayload::Nop);
            let mut transport = InfiniteStaleTransport {
                outbound: Vec::new(),
                stale_frame,
                recv_calls: 0,
            };
            let mut current_seq = starting_seq;

            let err = get_chunk_with_retry(
                &mut transport,
                &identity,
                &mut current_seq,
                0,
                &clock,
                &aead,
                &sha,
            )
            .expect_err("stale-frame flood should eventually time out");

            assert!(
                matches!(err, NodeError::ChunkTransferFailed { chunk_index: 0 }),
                "expected bounded retry exhaustion, got {err:?}"
            );
            assert_eq!(
                transport.outbound.len(),
                (MAX_RETRIES + 1) as usize,
                "each attempt should still send at most one GET_CHUNK"
            );
            assert_eq!(
                current_seq,
                starting_seq + (MAX_RETRIES + 1) as u64,
                "sequence should advance once per retry attempt"
            );
            assert!(
                transport.recv_calls < 1000,
                "per-attempt stale-frame draining must remain bounded"
            );
        }

        // --- end-to-end run_wake_cycle tests ---

        #[test]
        fn run_wake_cycle_nop() {
            let psk = [0x42u8; 32];
            let sha = crate::crypto::SoftwareSha256;
            let aead = NodeAead;
            let key_hint = sonde_protocol::key_hint_from_psk(&psk, &sha);

            let command_frame = make_command(&psk, 1, &CommandPayload::Nop);
            let mut transport = MockTransport::new();
            transport.queue_response(Some(command_frame));

            let mut storage = MockStorage::new().with_key(key_hint, psk);
            let mut hal = MockHal;
            let mut rng = MockRng(0);
            let clock = MockClock;
            let mut interp = MockBpfInterpreter::new();
            let mut map_storage = MapStorage::new(DEFAULT_MAP_BUDGET);
            let mut async_queue = AsyncQueue::new();

            let outcome = run_wake_cycle(
                &mut transport,
                &mut storage,
                &mut hal,
                &mut rng,
                &clock,
                &BoardLayout::LEGACY_COMPAT,
                &mut interp,
                &mut map_storage,
                &sha,
                &aead,
                &mut async_queue,
            );

            assert_eq!(outcome, WakeCycleOutcome::Sleep { seconds: 60 });
            // WAKE frame should have been sent
            assert!(!transport.outbound.is_empty());
        }

        #[test]
        fn run_wake_cycle_reports_previous_battery_on_next_wake() {
            let psk = [0x42u8; 32];
            let sha = crate::crypto::SoftwareSha256;
            let aead = NodeAead;
            let key_hint = sonde_protocol::key_hint_from_psk(&psk, &sha);
            let clock = MockClock;
            let mut storage = MockStorage::new().with_key(key_hint, psk);
            let mut hal = MockHal;
            let mut rng = MockRng(0);
            let mut interp = MockBpfInterpreter::new();
            let mut map_storage = MapStorage::new(DEFAULT_MAP_BUDGET);
            let mut async_queue = AsyncQueue::new();

            let mut transport_first = MockTransport::new();
            transport_first.queue_response(Some(make_command(&psk, 1, &CommandPayload::Nop)));
            let first_outcome = run_wake_cycle(
                &mut transport_first,
                &mut storage,
                &mut hal,
                &mut rng,
                &clock,
                &BoardLayout::LEGACY_COMPAT,
                &mut interp,
                &mut map_storage,
                &sha,
                &aead,
                &mut async_queue,
            );
            assert_eq!(first_outcome, WakeCycleOutcome::Sleep { seconds: 60 });

            let first_wake = decode_frame(&transport_first.outbound[0]).unwrap();
            let first_payload = open_frame(&first_wake, &psk, &aead, &sha).unwrap();
            let first_battery_mv =
                match sonde_protocol::NodeMessage::decode(MSG_WAKE, &first_payload).unwrap() {
                    sonde_protocol::NodeMessage::Wake { battery_mv, .. } => battery_mv,
                    _ => panic!("expected Wake message"),
                };
            assert_eq!(first_battery_mv, 0);
            assert_eq!(storage.last_battery_mv, Some(BATTERY_FALLBACK_MV));

            let mut transport_second = MockTransport::new();
            transport_second.queue_response(Some(make_command(&psk, 2, &CommandPayload::Nop)));
            let second_outcome = run_wake_cycle(
                &mut transport_second,
                &mut storage,
                &mut hal,
                &mut rng,
                &clock,
                &BoardLayout::LEGACY_COMPAT,
                &mut interp,
                &mut map_storage,
                &sha,
                &aead,
                &mut async_queue,
            );
            assert_eq!(second_outcome, WakeCycleOutcome::Sleep { seconds: 60 });

            let second_wake = decode_frame(&transport_second.outbound[0]).unwrap();
            let second_payload = open_frame(&second_wake, &psk, &aead, &sha).unwrap();
            let second_battery_mv =
                match sonde_protocol::NodeMessage::decode(MSG_WAKE, &second_payload).unwrap() {
                    sonde_protocol::NodeMessage::Wake { battery_mv, .. } => battery_mv,
                    _ => panic!("expected Wake message"),
                };
            assert_eq!(second_battery_mv, BATTERY_FALLBACK_MV);
            assert_eq!(storage.last_battery_mv, Some(BATTERY_FALLBACK_MV));
        }

        #[test]
        fn run_wake_cycle_update_program() {
            let psk = [0x22u8; 32];
            let sha = crate::crypto::SoftwareSha256;
            let aead = NodeAead;
            let key_hint = sonde_protocol::key_hint_from_psk(&psk, &sha);

            let image = sonde_protocol::ProgramImage {
                bytecode: vec![0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
                maps: vec![],
                map_initial_data: vec![],
            };
            let image_cbor = image.encode_deterministic().unwrap();
            let image_hash = sha.hash(&image_cbor);

            let chunk_size = 10u32;
            let chunk_count =
                sonde_protocol::chunk_count(image_cbor.len(), chunk_size as usize).unwrap();

            let starting_seq = 5000u64;

            let command_frame = make_command_with_seq(
                &psk,
                1, // echoes WAKE nonce (MockRng returns 1)
                starting_seq,
                1710000000000,
                &CommandPayload::UpdateProgram {
                    program_hash: image_hash.to_vec(),
                    program_size: image_cbor.len() as u32,
                    chunk_size,
                    chunk_count,
                },
            );
            let mut transport = MockTransport::new();
            transport.queue_response(Some(command_frame));

            for i in 0..chunk_count {
                let chunk_data = sonde_protocol::get_chunk(&image_cbor, i, chunk_size)
                    .unwrap()
                    .to_vec();
                let seq = starting_seq + i as u64;
                let chunk_frame = build_chunk_response(&psk, seq, i, &chunk_data);
                transport.queue_response(Some(chunk_frame));
            }

            let mut storage = MockStorage::new().with_key(key_hint, psk);
            let mut hal = MockHal;
            let mut rng = MockRng(0);
            let clock = MockClock;
            let mut interp = MockBpfInterpreter::new();
            let mut map_storage = MapStorage::new(DEFAULT_MAP_BUDGET);
            let mut async_queue = AsyncQueue::new();

            let outcome = run_wake_cycle(
                &mut transport,
                &mut storage,
                &mut hal,
                &mut rng,
                &clock,
                &BoardLayout::LEGACY_COMPAT,
                &mut interp,
                &mut map_storage,
                &sha,
                &aead,
                &mut async_queue,
            );

            assert_eq!(outcome, WakeCycleOutcome::Sleep { seconds: 60 });
            // WAKE + chunk GET_CHUNKs + PROGRAM_ACK
            assert_eq!(transport.outbound.len(), 1 + chunk_count as usize + 1);
            // Program installed on inactive partition
            assert!(storage.read_program(1).is_some());
            assert_eq!(storage.active_partition, 1);
            assert!(interp.loaded);
        }

        #[test]
        fn run_wake_cycle_wrong_msg_type_discarded() {
            let psk = [0x42u8; 32];
            let sha = crate::crypto::SoftwareSha256;
            let aead = NodeAead;
            let key_hint = sonde_protocol::key_hint_from_psk(&psk, &sha);

            // Encode a CHUNK frame where a COMMAND is expected — must be discarded.
            let wrong_msg = GatewayMessage::Chunk {
                chunk_index: 0,
                chunk_data: vec![0u8; 10],
            };
            let wrong_cbor = wrong_msg.encode().unwrap();
            let header = FrameHeader {
                key_hint,
                msg_type: MSG_CHUNK, // wrong type for COMMAND phase
                nonce: 1,
            };
            let wrong_frame = encode_frame(&header, &wrong_cbor, &psk, &aead, &sha).unwrap();

            let mut transport = MockTransport::new();
            // All responses are wrong → retries exhausted → sleep
            transport.queue_response(Some(wrong_frame.clone()));
            transport.queue_response(Some(wrong_frame.clone()));
            transport.queue_response(Some(wrong_frame.clone()));
            transport.queue_response(Some(wrong_frame));

            let mut storage = MockStorage::new().with_key(key_hint, psk);
            let mut hal = MockHal;
            let mut rng = MockRng(0);
            let clock = MockClock;
            let mut interp = MockBpfInterpreter::new();
            let mut map_storage = MapStorage::new(DEFAULT_MAP_BUDGET);
            let mut async_queue = AsyncQueue::new();

            let outcome = run_wake_cycle(
                &mut transport,
                &mut storage,
                &mut hal,
                &mut rng,
                &clock,
                &BoardLayout::LEGACY_COMPAT,
                &mut interp,
                &mut map_storage,
                &sha,
                &aead,
                &mut async_queue,
            );

            assert_eq!(outcome, WakeCycleOutcome::Sleep { seconds: 60 });
            assert!(!interp.loaded, "BPF must not run on invalid COMMAND");
        }

        /// T-N622: When one message is queued and fits, it is piggybacked on WAKE.
        ///
        /// Pre-loads the async queue with a single small blob, runs a NOP wake
        /// cycle, and verifies the outbound WAKE frame contains the blob.
        #[test]
        fn t_n622_piggyback_verification() {
            let psk = [0x42u8; 32];
            let sha = crate::crypto::SoftwareSha256;
            let aead = NodeAead;
            let key_hint = sonde_protocol::key_hint_from_psk(&psk, &sha);

            let command_frame = make_command(&psk, 1, &CommandPayload::Nop);
            let mut transport = MockTransport::new();
            transport.queue_response(Some(command_frame));

            let mut storage = MockStorage::new().with_key(key_hint, psk);
            let mut hal = MockHal;
            let mut rng = MockRng(0);
            let clock = MockClock;
            let mut interp = MockBpfInterpreter::new();
            let mut map_storage = MapStorage::new(DEFAULT_MAP_BUDGET);
            let mut async_queue = AsyncQueue::new();

            // Pre-load the queue with a small blob that fits in WAKE.
            let queued_blob = vec![0xAB; 10];
            assert_eq!(async_queue.push(queued_blob.clone()), 0);

            let outcome = run_wake_cycle(
                &mut transport,
                &mut storage,
                &mut hal,
                &mut rng,
                &clock,
                &BoardLayout::LEGACY_COMPAT,
                &mut interp,
                &mut map_storage,
                &sha,
                &aead,
                &mut async_queue,
            );

            assert_eq!(outcome, WakeCycleOutcome::Sleep { seconds: 60 });

            // The first outbound frame is the WAKE. Decode it and verify blob.
            assert!(!transport.outbound.is_empty(), "WAKE frame must be sent");
            let decoded = decode_frame(&transport.outbound[0]).unwrap();
            assert_eq!(decoded.header.msg_type, MSG_WAKE);
            let payload = open_frame(&decoded, &psk, &aead, &sha).unwrap();
            let wake_msg = sonde_protocol::NodeMessage::decode(MSG_WAKE, &payload).unwrap();
            match wake_msg {
                sonde_protocol::NodeMessage::Wake { blob, .. } => {
                    assert_eq!(
                        blob,
                        Some(queued_blob),
                        "piggybacked blob must appear in WAKE frame"
                    );
                }
                _ => panic!("expected Wake message"),
            }

            // Queue must be empty after the cycle because the piggybacked
            // WAKE path consumes and clears the queued blob before step 9b.
            assert!(
                async_queue.is_empty(),
                "queue must be empty after piggyback clear"
            );

            // Piggybacked blob must NOT be resent as APP_DATA.
            // Only 1 WAKE frame should be sent (no APP_DATA frames).
            assert_eq!(
                transport.outbound.len(),
                1,
                "only WAKE frame expected — piggybacked blob must not be resent as APP_DATA"
            );
        }

        /// T-N626: Async queue is cleared after send (drain empties queue).
        ///
        /// Pre-loads the queue with multiple messages so piggybacking is
        /// skipped, then verifies all messages are sent as APP_DATA and the
        /// queue is empty afterward.
        #[test]
        fn t_n626_queue_cleared_after_send() {
            let psk = [0x42u8; 32];
            let sha = crate::crypto::SoftwareSha256;
            let aead = NodeAead;
            let key_hint = sonde_protocol::key_hint_from_psk(&psk, &sha);

            let command_frame = make_command(&psk, 1, &CommandPayload::Nop);
            let mut transport = MockTransport::new();
            transport.queue_response(Some(command_frame));

            let mut storage = MockStorage::new().with_key(key_hint, psk);
            let mut hal = MockHal;
            let mut rng = MockRng(0);
            let clock = MockClock;
            let mut interp = MockBpfInterpreter::new();
            let mut map_storage = MapStorage::new(DEFAULT_MAP_BUDGET);
            let mut async_queue = AsyncQueue::new();

            // Push 3 blobs — more than 1 means no piggybacking.
            for i in 0u8..3 {
                assert_eq!(async_queue.push(vec![i; 5]), 0);
            }

            let outcome = run_wake_cycle(
                &mut transport,
                &mut storage,
                &mut hal,
                &mut rng,
                &clock,
                &BoardLayout::LEGACY_COMPAT,
                &mut interp,
                &mut map_storage,
                &sha,
                &aead,
                &mut async_queue,
            );

            assert_eq!(outcome, WakeCycleOutcome::Sleep { seconds: 60 });

            // Outbound: 1 WAKE + 3 APP_DATA frames from the drain.
            assert_eq!(
                transport.outbound.len(),
                4,
                "expected 1 WAKE + 3 APP_DATA frames"
            );

            // Verify the WAKE frame has no piggybacked blob.
            let decoded = decode_frame(&transport.outbound[0]).unwrap();
            assert_eq!(decoded.header.msg_type, MSG_WAKE);
            let payload = open_frame(&decoded, &psk, &aead, &sha).unwrap();
            let wake_msg = sonde_protocol::NodeMessage::decode(MSG_WAKE, &payload).unwrap();
            match wake_msg {
                sonde_protocol::NodeMessage::Wake { blob, .. } => {
                    assert!(blob.is_none(), "multiple queued blobs must not piggyback");
                }
                _ => panic!("expected Wake message"),
            }

            // Verify the 3 APP_DATA frames.
            for idx in 1..=3 {
                let decoded = decode_frame(&transport.outbound[idx]).unwrap();
                assert_eq!(
                    decoded.header.msg_type, MSG_APP_DATA,
                    "frame {} must be APP_DATA",
                    idx
                );
            }

            // Queue must be empty after the cycle.
            assert!(async_queue.is_empty(), "queue must be empty after drain");
        }
    }
}
