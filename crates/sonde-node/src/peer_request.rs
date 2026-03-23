// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! PEER_REQUEST/PEER_ACK exchange for BLE pairing registration.
//!
//! After BLE provisioning stores a PSK and encrypted payload, the node
//! must relay the payload to the gateway (PEER_REQUEST) and verify the
//! gateway's acknowledgment (PEER_ACK) before entering normal WAKE cycles.
//!
//! Wire format per ble-pairing-protocol.md §7:
//!
//! **PEER_REQUEST (0x05, node → gateway):**
//! ```text
//! ┌─────────────────────────────────────┬────────────────┬────────┐
//! │ key_hint(2) ‖ msg_type(1) ‖ nonce(8)│ CBOR {1: blob} │ HMAC   │
//! └─────────────────────────────────────┴────────────────┴────────┘
//! ```
//!
//! **PEER_ACK (0x84, gateway → node):**
//! ```text
//! ┌─────────────────────────────────────┬─────────────────────────┬────────┐
//! │ key_hint(2) ‖ msg_type(1) ‖ nonce(8)│ CBOR {1: status, 2: proof}│ HMAC │
//! └─────────────────────────────────────┴─────────────────────────┴────────┘
//! ```

use alloc::vec::Vec;

use sonde_protocol::{
    decode_frame, encode_frame, verify_frame, FrameHeader, HmacProvider, MSG_PEER_ACK,
    MSG_PEER_REQUEST, PEER_ACK_KEY_PROOF, PEER_ACK_KEY_STATUS, PEER_REQ_KEY_PAYLOAD,
};

use crate::error::{NodeError, NodeResult};
use crate::key_store::NodeIdentity;
use crate::traits::{Clock, PlatformStorage, Rng, Transport};

extern crate alloc;

/// Domain separator for registration proof (ble-pairing-protocol.md §7.2).
const PROOF_DOMAIN: &[u8] = b"sonde-peer-ack-v1";

use crate::ble_pairing::PEER_PAYLOAD_MAX_LEN;

/// PEER_ACK listen timeout in milliseconds (ND-0911: ≥10 seconds).
const PEER_ACK_TIMEOUT_MS: u32 = 10_000;

/// PEER_ACK status code: registered successfully.
const PEER_ACK_STATUS_OK: u64 = 0;

/// Build a PEER_REQUEST frame.
///
/// Encodes `{ 1: encrypted_payload }` as CBOR, wraps in a protocol frame
/// with `msg_type = 0x05`, the provided 8-byte nonce, and HMAC-SHA256.
///
/// The caller generates the nonce (via `Rng`) and retains it to verify
/// the echoed nonce in PEER_ACK.
///
/// Returns `Err` if `encrypted_payload` is too large for the ESP-NOW frame
/// budget (max 207 bytes of CBOR payload after header + HMAC).
pub fn build_peer_request_frame(
    identity: &NodeIdentity,
    encrypted_payload: &[u8],
    nonce: u64,
    hmac: &dyn HmacProvider,
) -> NodeResult<Vec<u8>> {
    // The ESP-NOW frame budget is 250 bytes total. After the 11-byte header
    // and 32-byte HMAC, 207 bytes remain for CBOR payload. The CBOR framing
    // for { 1: bstr(N) } uses ~5 bytes, so encrypted_payload must be at most
    // 202 bytes. See ble-pairing-protocol.md §11.1.
    if encrypted_payload.len() > PEER_PAYLOAD_MAX_LEN {
        return Err(NodeError::MalformedPayload(
            "encrypted_payload exceeds ESP-NOW frame budget (max 202 bytes)",
        ));
    }

    // Encode CBOR: { 1: bstr(encrypted_payload) }
    let cbor_map = ciborium::Value::Map(alloc::vec![(
        ciborium::Value::Integer(PEER_REQ_KEY_PAYLOAD.into()),
        ciborium::Value::Bytes(encrypted_payload.to_vec()),
    )]);
    let mut cbor_buf = Vec::new();
    ciborium::into_writer(&cbor_map, &mut cbor_buf)
        .map_err(|_| NodeError::MalformedPayload("PEER_REQUEST CBOR encode failed"))?;

    let header = FrameHeader {
        key_hint: identity.key_hint,
        msg_type: MSG_PEER_REQUEST,
        nonce,
    };

    encode_frame(&header, &cbor_buf, &identity.psk, hmac)
        .map_err(|_| NodeError::MalformedPayload("PEER_REQUEST frame encode failed"))
}

/// Verify a PEER_ACK frame.
///
/// Checks:
/// 1. Frame HMAC is valid (keyed with node PSK).
/// 2. `msg_type` is `MSG_PEER_ACK` (0x84).
/// 3. Nonce echoes the PEER_REQUEST nonce.
/// 4. CBOR payload contains `{ 1: 0, 2: registration_proof }`.
/// 5. `registration_proof == HMAC-SHA256(psk, "sonde-peer-ack-v1" ‖ encrypted_payload)`.
///
/// Returns `Ok(())` on success, `Err` on any verification failure.
pub fn verify_peer_ack(
    raw: &[u8],
    identity: &NodeIdentity,
    expected_nonce: u64,
    encrypted_payload: &[u8],
    hmac: &dyn HmacProvider,
) -> NodeResult<()> {
    let decoded =
        decode_frame(raw).map_err(|_| NodeError::MalformedPayload("PEER_ACK decode failed"))?;

    // 1. Verify HMAC
    if !verify_frame(&decoded, &identity.psk, hmac) {
        return Err(NodeError::AuthFailure);
    }

    // 2. Verify msg_type
    if decoded.header.msg_type != MSG_PEER_ACK {
        return Err(NodeError::UnexpectedMsgType(decoded.header.msg_type));
    }

    // 3. Verify echoed nonce
    if decoded.header.nonce != expected_nonce {
        return Err(NodeError::ResponseBindingMismatch);
    }

    // 4. Decode CBOR payload: { 1: status, 2: proof }
    let cbor: ciborium::Value = ciborium::from_reader(&decoded.payload[..])
        .map_err(|_| NodeError::MalformedPayload("PEER_ACK CBOR decode failed"))?;

    let map = cbor
        .as_map()
        .ok_or(NodeError::MalformedPayload("PEER_ACK payload is not a map"))?;

    let mut status: Option<u64> = None;
    let mut proof: Option<&[u8]> = None;

    for (k, v) in map {
        let key = k
            .as_integer()
            .and_then(|i| u64::try_from(i).ok())
            .ok_or(NodeError::MalformedPayload("PEER_ACK non-integer key"))?;
        match key {
            PEER_ACK_KEY_STATUS => {
                status = v.as_integer().and_then(|i| u64::try_from(i).ok());
            }
            PEER_ACK_KEY_PROOF => {
                proof = v.as_bytes().map(|v| &**v);
            }
            _ => {} // ignore unknown keys
        }
    }

    let status = status.ok_or(NodeError::MalformedPayload("PEER_ACK missing status field"))?;
    let proof = proof.ok_or(NodeError::MalformedPayload("PEER_ACK missing proof field"))?;

    if status != PEER_ACK_STATUS_OK {
        return Err(NodeError::MalformedPayload(
            "PEER_ACK status is not registered",
        ));
    }

    // 5. Verify registration_proof
    if proof.len() != 32 {
        return Err(NodeError::MalformedPayload(
            "PEER_ACK proof is not 32 bytes",
        ));
    }
    let proof_array: &[u8; 32] = proof
        .try_into()
        .map_err(|_| NodeError::MalformedPayload("PEER_ACK proof is not 32 bytes"))?;

    let mut proof_input = Vec::with_capacity(PROOF_DOMAIN.len() + encrypted_payload.len());
    proof_input.extend_from_slice(PROOF_DOMAIN);
    proof_input.extend_from_slice(encrypted_payload);

    if !hmac.verify(&identity.psk, &proof_input, proof_array) {
        return Err(NodeError::MalformedPayload(
            "PEER_ACK registration_proof mismatch",
        ));
    }

    Ok(())
}

/// Execute the PEER_REQUEST/PEER_ACK exchange.
///
/// 1. Build and send PEER_REQUEST with a random nonce.
/// 2. Listen for PEER_ACK for ≥10 seconds (ND-0911).
/// 3. Verify PEER_ACK (HMAC, nonce echo, registration_proof).
/// 4. On success: set `reg_complete` flag (ND-0913).
///
/// Returns `Ok(true)` if registration completed, `Ok(false)` on timeout,
/// or `Err` on transport/storage failure.
pub fn peer_request_exchange<T: Transport, S: PlatformStorage>(
    transport: &mut T,
    storage: &mut S,
    identity: &NodeIdentity,
    encrypted_payload: &[u8],
    rng: &mut dyn Rng,
    clock: &dyn Clock,
    hmac: &dyn HmacProvider,
) -> NodeResult<bool> {
    let nonce = rng.random_u64();

    // Build and send PEER_REQUEST
    let frame = build_peer_request_frame(identity, encrypted_payload, nonce, hmac)?;
    transport.send(&frame)?;

    // Listen for PEER_ACK with 10-second timeout (ND-0911).
    // Use the clock to track elapsed time so we keep listening even if
    // individual recv() calls return early.
    let start_ms = clock.elapsed_ms();
    loop {
        let elapsed = clock.elapsed_ms().saturating_sub(start_ms);
        if elapsed >= PEER_ACK_TIMEOUT_MS as u64 {
            return Ok(false); // Timeout — retry next wake cycle
        }

        let remaining = (PEER_ACK_TIMEOUT_MS as u64 - elapsed) as u32;
        // Use shorter recv windows so we can re-check elapsed time
        let recv_timeout = remaining.min(500);

        match transport.recv(recv_timeout)? {
            Some(raw) => {
                if verify_peer_ack(&raw, identity, nonce, encrypted_payload, hmac).is_ok() {
                    // Valid PEER_ACK — set reg_complete (ND-0913)
                    storage.write_reg_complete(true)?;
                    return Ok(true);
                }
                // Invalid response — keep listening
            }
            None => {
                // recv timeout — loop and check wall-clock timeout
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::NodeResult;
    use crate::traits::PlatformStorage;
    use alloc::vec;
    use sonde_protocol::HmacProvider;

    // --- Test HMAC provider (real HMAC-SHA256) ---

    struct TestHmac;

    impl HmacProvider for TestHmac {
        fn compute(&self, key: &[u8], data: &[u8]) -> [u8; 32] {
            use hmac::{Hmac, Mac};
            use sha2::Sha256;
            type HmacSha256 = Hmac<Sha256>;
            let mut mac = HmacSha256::new_from_slice(key).expect("HMAC key length should be valid");
            mac.update(data);
            mac.finalize().into_bytes().into()
        }

        fn verify(&self, key: &[u8], data: &[u8], expected: &[u8; 32]) -> bool {
            let computed = self.compute(key, data);
            computed == *expected
        }
    }

    // --- Minimal mock storage ---

    struct MockStorage {
        key: Option<(u16, [u8; 32])>,
        reg_complete: bool,
        peer_payload: Option<Vec<u8>>,
    }

    impl MockStorage {
        fn with_identity(key_hint: u16, psk: [u8; 32], payload: Vec<u8>) -> Self {
            Self {
                key: Some((key_hint, psk)),
                reg_complete: false,
                peer_payload: Some(payload),
            }
        }
    }

    impl PlatformStorage for MockStorage {
        fn read_key(&self) -> Option<(u16, [u8; 32])> {
            self.key
        }
        fn write_key(&mut self, kh: u16, psk: &[u8; 32]) -> NodeResult<()> {
            self.key = Some((kh, *psk));
            Ok(())
        }
        fn erase_key(&mut self) -> NodeResult<()> {
            self.key = None;
            Ok(())
        }
        fn read_schedule(&self) -> (u32, u8) {
            (60, 0)
        }
        fn write_schedule_interval(&mut self, _: u32) -> NodeResult<()> {
            Ok(())
        }
        fn write_active_partition(&mut self, _: u8) -> NodeResult<()> {
            Ok(())
        }
        fn reset_schedule(&mut self) -> NodeResult<()> {
            Ok(())
        }
        fn read_program(&self, _: u8) -> Option<Vec<u8>> {
            None
        }
        fn write_program(&mut self, _: u8, _: &[u8]) -> NodeResult<()> {
            Ok(())
        }
        fn erase_program(&mut self, _: u8) -> NodeResult<()> {
            Ok(())
        }
        fn take_early_wake_flag(&mut self) -> bool {
            false
        }
        fn set_early_wake_flag(&mut self) -> NodeResult<()> {
            Ok(())
        }
        fn read_channel(&self) -> Option<u8> {
            None
        }
        fn write_channel(&mut self, _: u8) -> NodeResult<()> {
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

    // --- Mock transport that records sent frames and replays responses ---

    struct MockTransport {
        sent: Vec<Vec<u8>>,
        responses: Vec<Option<Vec<u8>>>,
        recv_index: usize,
    }

    impl MockTransport {
        fn new() -> Self {
            Self {
                sent: Vec::new(),
                responses: Vec::new(),
                recv_index: 0,
            }
        }

        fn with_responses(responses: Vec<Option<Vec<u8>>>) -> Self {
            Self {
                sent: Vec::new(),
                responses,
                recv_index: 0,
            }
        }
    }

    impl Transport for MockTransport {
        fn send(&mut self, data: &[u8]) -> NodeResult<()> {
            self.sent.push(data.to_vec());
            Ok(())
        }

        fn recv(&mut self, _timeout_ms: u32) -> NodeResult<Option<Vec<u8>>> {
            if self.recv_index < self.responses.len() {
                let r = self.responses[self.recv_index].clone();
                self.recv_index += 1;
                Ok(r)
            } else {
                Ok(None)
            }
        }
    }

    // --- Mock clock ---

    struct MockClock {
        ticks: core::cell::Cell<u64>,
        step_ms: u64,
    }

    impl MockClock {
        fn new(step_ms: u64) -> Self {
            Self {
                ticks: core::cell::Cell::new(0),
                step_ms,
            }
        }
    }

    impl Clock for MockClock {
        fn elapsed_ms(&self) -> u64 {
            let t = self.ticks.get();
            self.ticks.set(t + self.step_ms);
            t
        }

        fn delay_ms(&self, _ms: u32) {}
    }

    // --- Mock RNG ---

    struct MockRng {
        value: u64,
    }

    impl MockRng {
        fn new(value: u64) -> Self {
            Self { value }
        }
    }

    impl Rng for MockRng {
        fn random_u64(&mut self) -> u64 {
            self.value
        }
    }

    // --- Helper to build a valid PEER_ACK frame ---

    fn build_peer_ack(
        identity: &NodeIdentity,
        nonce: u64,
        encrypted_payload: &[u8],
        hmac: &dyn HmacProvider,
    ) -> Vec<u8> {
        // Compute registration_proof
        let mut proof_input = Vec::with_capacity(PROOF_DOMAIN.len() + encrypted_payload.len());
        proof_input.extend_from_slice(PROOF_DOMAIN);
        proof_input.extend_from_slice(encrypted_payload);
        let proof = hmac.compute(&identity.psk, &proof_input);

        // Encode CBOR: { 1: 0, 2: proof }
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
            key_hint: identity.key_hint,
            msg_type: MSG_PEER_ACK,
            nonce,
        };

        encode_frame(&header, &cbor_buf, &identity.psk, hmac).unwrap()
    }

    fn test_identity() -> NodeIdentity {
        NodeIdentity {
            key_hint: 0x1234,
            psk: [0x42u8; 32],
        }
    }

    // -----------------------------------------------------------------------
    // T-N909: PEER_REQUEST frame construction
    // -----------------------------------------------------------------------

    #[test]
    fn t_n909_peer_request_frame_construction() {
        let hmac = TestHmac;
        let identity = test_identity();
        let payload = vec![0xDE, 0xAD, 0xBE, 0xEF];
        let nonce: u64 = 0x1122334455667788;

        let frame = build_peer_request_frame(&identity, &payload, nonce, &hmac).unwrap();

        // Decode and verify the frame
        let decoded = decode_frame(&frame).unwrap();
        assert_eq!(decoded.header.key_hint, 0x1234);
        assert_eq!(decoded.header.msg_type, MSG_PEER_REQUEST);
        assert_eq!(decoded.header.nonce, nonce);

        // Verify HMAC
        assert!(verify_frame(&decoded, &identity.psk, &hmac));

        // Decode CBOR payload: { 1: encrypted_payload }
        let cbor: ciborium::Value = ciborium::from_reader(&decoded.payload[..]).unwrap();
        let map = cbor.as_map().unwrap();
        assert_eq!(map.len(), 1);
        let (k, v) = &map[0];
        assert_eq!(u64::try_from(k.as_integer().unwrap()).unwrap(), 1);
        assert_eq!(v.as_bytes().unwrap(), &payload);
    }

    // -----------------------------------------------------------------------
    // T-N912: PEER_ACK happy path
    // -----------------------------------------------------------------------

    #[test]
    fn t_n912_peer_ack_valid() {
        let hmac = TestHmac;
        let identity = test_identity();
        let payload = vec![0xDE, 0xAD, 0xBE, 0xEF];
        let nonce: u64 = 0xAABBCCDDEEFF0011;

        let ack_frame = build_peer_ack(&identity, nonce, &payload, &hmac);
        let result = verify_peer_ack(&ack_frame, &identity, nonce, &payload, &hmac);
        assert!(result.is_ok());
    }

    // -----------------------------------------------------------------------
    // T-N913: PEER_ACK with wrong nonce — discarded
    // -----------------------------------------------------------------------

    #[test]
    fn t_n913_peer_ack_wrong_nonce() {
        let hmac = TestHmac;
        let identity = test_identity();
        let payload = vec![0xDE, 0xAD, 0xBE, 0xEF];
        let request_nonce: u64 = 0xAABBCCDDEEFF0011;
        let wrong_nonce: u64 = 0x1111111111111111;

        // Build ACK with the wrong nonce
        let ack_frame = build_peer_ack(&identity, wrong_nonce, &payload, &hmac);
        let result = verify_peer_ack(&ack_frame, &identity, request_nonce, &payload, &hmac);
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // T-N914: PEER_ACK with wrong registration_proof — discarded
    // -----------------------------------------------------------------------

    #[test]
    fn t_n914_peer_ack_wrong_proof() {
        let hmac = TestHmac;
        let identity = test_identity();
        let payload = vec![0xDE, 0xAD, 0xBE, 0xEF];
        let nonce: u64 = 0xAABBCCDDEEFF0011;

        // Build ACK with a different payload (produces wrong proof)
        let wrong_payload = vec![0xFF, 0xFF, 0xFF, 0xFF];
        let ack_frame = build_peer_ack(&identity, nonce, &wrong_payload, &hmac);

        // Verify with the correct payload — proof should mismatch
        let result = verify_peer_ack(&ack_frame, &identity, nonce, &payload, &hmac);
        assert!(result.is_err());
    }

    /// PEER_ACK with tampered HMAC — discarded.
    #[test]
    fn peer_ack_tampered_hmac() {
        let hmac = TestHmac;
        let identity = test_identity();
        let payload = vec![0xDE, 0xAD];
        let nonce: u64 = 0x42;

        let mut ack_frame = build_peer_ack(&identity, nonce, &payload, &hmac);
        // Tamper with the last byte (HMAC)
        let last = ack_frame
            .last_mut()
            .expect("build_peer_ack() must not return an empty PEER_ACK frame");
        *last ^= 0xFF;

        let result = verify_peer_ack(&ack_frame, &identity, nonce, &payload, &hmac);
        assert!(result.is_err());
    }

    /// PEER_ACK with wrong msg_type — discarded.
    #[test]
    fn peer_ack_wrong_msg_type() {
        let hmac = TestHmac;
        let identity = test_identity();
        let payload = vec![0xDE, 0xAD];
        let nonce: u64 = 0x42;

        // Build a valid ACK but replace msg_type with MSG_COMMAND
        let mut proof_input = Vec::with_capacity(PROOF_DOMAIN.len() + payload.len());
        proof_input.extend_from_slice(PROOF_DOMAIN);
        proof_input.extend_from_slice(&payload);
        let proof = hmac.compute(&identity.psk, &proof_input);

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
            key_hint: identity.key_hint,
            msg_type: sonde_protocol::MSG_COMMAND, // wrong type
            nonce,
        };

        let frame = encode_frame(&header, &cbor_buf, &identity.psk, &hmac).unwrap();
        let result = verify_peer_ack(&frame, &identity, nonce, &payload, &hmac);
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // T-N915: peer_request_exchange sets reg_complete on valid PEER_ACK
    // -----------------------------------------------------------------------

    #[test]
    fn t_n915_exchange_sets_reg_complete() {
        let hmac = TestHmac;
        let identity = test_identity();
        let payload = vec![0xDE, 0xAD, 0xBE, 0xEF];
        let nonce: u64 = 0x1122334455667788;
        let mut rng = MockRng::new(nonce);
        let clock = MockClock::new(500); // 500ms per elapsed_ms() call

        // Build the expected PEER_ACK
        let ack = build_peer_ack(&identity, nonce, &payload, &hmac);
        let mut transport = MockTransport::with_responses(vec![Some(ack)]);
        let mut storage = MockStorage::with_identity(0x1234, [0x42u8; 32], payload.clone());

        assert!(!storage.reg_complete);

        let result = peer_request_exchange(
            &mut transport,
            &mut storage,
            &identity,
            &payload,
            &mut rng,
            &clock,
            &hmac,
        )
        .unwrap();

        assert!(result, "exchange should succeed");
        assert!(storage.reg_complete, "reg_complete must be set");
        // peer_payload is retained per ND-0913
        assert!(storage.peer_payload.is_some());
    }

    // -----------------------------------------------------------------------
    // T-N911: Timeout after 10 seconds — returns Ok(false)
    // -----------------------------------------------------------------------

    #[test]
    fn t_n911_exchange_timeout() {
        let hmac = TestHmac;
        let identity = test_identity();
        let payload = vec![0xDE, 0xAD];
        let mut rng = MockRng::new(0x42);
        // Each elapsed_ms() call advances by 5000ms → 3 calls = 0, 5000, 10000 → timeout
        let clock = MockClock::new(5000);
        let mut transport = MockTransport::new(); // no responses
        let mut storage = MockStorage::with_identity(0x1234, [0x42u8; 32], payload.clone());

        let result = peer_request_exchange(
            &mut transport,
            &mut storage,
            &identity,
            &payload,
            &mut rng,
            &clock,
            &hmac,
        )
        .unwrap();

        assert!(!result, "should timeout");
        assert!(!storage.reg_complete, "reg_complete must NOT be set");
    }

    // -----------------------------------------------------------------------
    // Exchange ignores invalid responses and keeps listening
    // -----------------------------------------------------------------------

    #[test]
    fn exchange_ignores_garbage_then_accepts_valid() {
        let hmac = TestHmac;
        let identity = test_identity();
        let payload = vec![0xDE, 0xAD, 0xBE, 0xEF];
        let nonce: u64 = 0x42;
        let mut rng = MockRng::new(nonce);
        let clock = MockClock::new(500);

        let valid_ack = build_peer_ack(&identity, nonce, &payload, &hmac);
        let mut transport = MockTransport::with_responses(vec![
            Some(vec![0xFF; 50]), // garbage
            None,                 // timeout
            Some(valid_ack),      // valid
        ]);
        let mut storage = MockStorage::with_identity(0x1234, [0x42u8; 32], payload.clone());

        let result = peer_request_exchange(
            &mut transport,
            &mut storage,
            &identity,
            &payload,
            &mut rng,
            &clock,
            &hmac,
        )
        .unwrap();

        assert!(result);
        assert!(storage.reg_complete);
    }

    // -----------------------------------------------------------------------
    // T-N941: PEER_ACK with corrupted HMAC — silently discarded
    // -----------------------------------------------------------------------

    #[test]
    fn t_n941_exchange_peer_ack_corrupted_hmac_discarded() {
        // T-N941: Send a PEER_ACK with a valid nonce and registration proof
        // but a corrupted HMAC.  The node must silently discard the frame:
        // no error response transmitted, reg_complete not set.
        let hmac = TestHmac;
        let identity = test_identity();
        let payload = vec![0xDE, 0xAD, 0xBE, 0xEF];
        let nonce: u64 = 0x42;
        let mut rng = MockRng::new(nonce);
        // Each elapsed_ms() call advances 500ms; after ~21 calls → 10 s timeout
        let clock = MockClock::new(500);

        // Build a valid PEER_ACK then corrupt the HMAC by flipping the last byte.
        let mut ack_frame = build_peer_ack(&identity, nonce, &payload, &hmac);
        let last = ack_frame
            .last_mut()
            .expect("build_peer_ack() must not return an empty PEER_ACK frame");
        *last ^= 0xFF;

        let mut transport = MockTransport::with_responses(vec![
            Some(ack_frame), // corrupted HMAC
            None,            // timeout fills remaining listen window
        ]);
        let mut storage = MockStorage::with_identity(0x1234, [0x42u8; 32], payload.clone());

        let result = peer_request_exchange(
            &mut transport,
            &mut storage,
            &identity,
            &payload,
            &mut rng,
            &clock,
            &hmac,
        )
        .unwrap();

        // Exchange must time out — corrupted HMAC is silently discarded.
        assert!(!result, "exchange must timeout, not succeed");
        assert!(
            !storage.reg_complete,
            "reg_complete must NOT be set on HMAC failure"
        );
        // Only the PEER_REQUEST was sent; no error response.
        assert_eq!(transport.sent.len(), 1);
    }
}
