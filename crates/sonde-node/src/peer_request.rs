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
//! │ key_hint(2) ‖ msg_type(1) ‖ nonce(8)│ CBOR {1: blob} │ GCM    │
//! └─────────────────────────────────────┴────────────────┴────────┘
//! ```
//!
//! **PEER_ACK (0x84, gateway → node):**
//! ```text
//! ┌─────────────────────────────────────┬────────────────────┬──────────┐
//! │ key_hint(2) ‖ msg_type(1) ‖ nonce(8)│ CBOR {1: status}   │ GCM      │
//! └─────────────────────────────────────┴────────────────────┴──────────┘
//! ```

use alloc::vec::Vec;

use sonde_protocol::{MSG_PEER_ACK, MSG_PEER_REQUEST, PEER_ACK_KEY_STATUS};

use crate::error::{NodeError, NodeResult};
use crate::key_store::NodeIdentity;
use crate::traits::{PlatformStorage, Transport};

extern crate alloc;

/// PEER_ACK listen timeout in milliseconds (ND-0911: ≥10 seconds).
const PEER_ACK_TIMEOUT_MS: u32 = 10_000;

/// PEER_ACK status code: registered successfully.
const PEER_ACK_STATUS_OK: u64 = 0;

// ---------------------------------------------------------------------------
// AES-256-GCM peer request processing
// ---------------------------------------------------------------------------

/// Build and validate a PEER_REQUEST AEAD frame for transmission.
///
/// The phone builds the complete ESP-NOW PEER_REQUEST frame during BLE
/// provisioning.  This function validates the stored blob is a
/// decodable AEAD frame with `msg_type == MSG_PEER_REQUEST` and returns
/// it as-is for transmission.
pub fn build_peer_request_frame_aead(complete_frame: &[u8]) -> NodeResult<Vec<u8>> {
    let decoded = sonde_protocol::decode_frame_aead(complete_frame)
        .map_err(|_| NodeError::MalformedPayload("PEER_REQUEST frame is not a valid AEAD frame"))?;

    if decoded.header.msg_type != MSG_PEER_REQUEST {
        return Err(NodeError::UnexpectedMsgType(decoded.header.msg_type));
    }

    Ok(complete_frame.to_vec())
}

/// Verify a PEER_ACK frame using AES-256-GCM.
///
/// Decrypts and authenticates the PEER_ACK frame using AES-256-GCM,
/// then verifies `msg_type`, nonce echo, and status.  Per
/// `ble-pairing-protocol.md` §7.2, the `registration_proof` field is
/// retired under AES-256-GCM: successful AEAD open with `node_psk`
/// constitutes proof that the gateway holds the node's PSK.
pub fn verify_peer_ack_aead<A: sonde_protocol::AeadProvider, S: sonde_protocol::Sha256Provider>(
    raw: &[u8],
    identity: &NodeIdentity,
    expected_nonce: u64,
    aead: &A,
    sha: &S,
) -> NodeResult<()> {
    let decoded = sonde_protocol::decode_frame_aead(raw)
        .map_err(|_| NodeError::MalformedPayload("PEER_ACK decode failed"))?;

    let header = decoded.header.clone();
    // Successful AEAD open proves the gateway holds node_psk (§7.2).
    let payload = sonde_protocol::open_frame(&decoded, &identity.psk, aead, sha)
        .map_err(|_| NodeError::AuthFailure)?;

    if header.msg_type != MSG_PEER_ACK {
        return Err(NodeError::UnexpectedMsgType(header.msg_type));
    }

    if header.nonce != expected_nonce {
        return Err(NodeError::ResponseBindingMismatch);
    }

    let cbor: ciborium::Value = ciborium::from_reader(&payload[..])
        .map_err(|_| NodeError::MalformedPayload("PEER_ACK CBOR decode failed"))?;

    let map = cbor
        .as_map()
        .ok_or(NodeError::MalformedPayload("PEER_ACK payload is not a map"))?;

    let mut status: Option<u64> = None;

    for (k, v) in map {
        let key = k
            .as_integer()
            .and_then(|i| u64::try_from(i).ok())
            .ok_or(NodeError::MalformedPayload("PEER_ACK non-integer key"))?;
        if key == PEER_ACK_KEY_STATUS {
            status = v.as_integer().and_then(|i| u64::try_from(i).ok());
        }
    }

    let status = status.ok_or(NodeError::MalformedPayload("PEER_ACK missing status field"))?;

    if status != PEER_ACK_STATUS_OK {
        return Err(NodeError::MalformedPayload(
            "PEER_ACK status is not registered",
        ));
    }

    Ok(())
}

/// Perform a PEER_REQUEST/PEER_ACK exchange using AES-256-GCM.
///
/// The `complete_frame` is a pre-built ESP-NOW PEER_REQUEST frame
/// (built by the phone during BLE provisioning).  The node transmits
/// it verbatim and waits for a PEER_ACK encrypted with `node_psk`.
#[allow(clippy::too_many_arguments)]
pub fn peer_request_exchange_aead<
    T: Transport,
    S: PlatformStorage,
    A: sonde_protocol::AeadProvider,
    H: sonde_protocol::Sha256Provider,
>(
    transport: &mut T,
    storage: &mut S,
    identity: &NodeIdentity,
    complete_frame: &[u8],
    clock: &dyn crate::traits::Clock,
    aead: &A,
    sha: &H,
) -> NodeResult<bool> {
    let frame = build_peer_request_frame_aead(complete_frame)?;

    // Extract nonce via decode (already validated by build_peer_request_frame_aead).
    let decoded = sonde_protocol::decode_frame_aead(&frame)
        .map_err(|_| NodeError::MalformedPayload("cannot decode validated frame"))?;
    let nonce = decoded.header.nonce;

    transport.send(&frame)?;
    log::info!("PEER_REQUEST sent (relayed phone-built frame, ND-1004)");

    let start_ms = clock.elapsed_ms();
    loop {
        let elapsed = clock.elapsed_ms().saturating_sub(start_ms);
        if elapsed >= PEER_ACK_TIMEOUT_MS as u64 {
            return Ok(false);
        }

        let remaining = (PEER_ACK_TIMEOUT_MS as u64 - elapsed) as u32;
        let recv_timeout = remaining.min(500);

        if let Some(raw) = transport.recv(recv_timeout)? {
            if verify_peer_ack_aead(&raw, identity, nonce, aead, sha).is_ok() {
                storage.write_reg_complete(true)?;
                log::info!("PEER_ACK received — registration complete (ND-1005)");
                return Ok(true);
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
    use crate::traits::{Clock, PlatformStorage};
    use alloc::vec;
    use sonde_protocol::FrameHeader;

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

    // ------------------------------------------------------------------
    // AES-256-GCM peer request tests
    // ------------------------------------------------------------------

    mod aead_tests {
        use super::*;
        use crate::node_aead::NodeAead;
        use sonde_protocol::{decode_frame_aead, Sha256Provider};

        struct TestSha256;
        impl Sha256Provider for TestSha256 {
            fn hash(&self, data: &[u8]) -> [u8; 32] {
                use sha2::Digest;
                sha2::Sha256::digest(data).into()
            }
        }

        /// Build a complete PEER_REQUEST AEAD frame (simulating what the
        /// phone builds) for use in node-side tests.
        fn build_phone_peer_request_frame(
            phone_psk: &[u8; 32],
            inner_payload: &[u8],
            nonce: u64,
        ) -> Vec<u8> {
            let sha = TestSha256;
            let aead = NodeAead;
            let phone_key_hint = sonde_protocol::key_hint_from_psk(phone_psk, &sha);

            // Wrap inner payload in CBOR: { 1: inner_payload }
            let cbor_map = ciborium::Value::Map(alloc::vec![(
                ciborium::Value::Integer(sonde_protocol::PEER_REQ_KEY_PAYLOAD.into()),
                ciborium::Value::Bytes(inner_payload.to_vec()),
            )]);
            let mut cbor_buf = Vec::new();
            ciborium::into_writer(&cbor_map, &mut cbor_buf).unwrap();

            let header = FrameHeader {
                key_hint: phone_key_hint,
                msg_type: MSG_PEER_REQUEST,
                nonce,
            };

            sonde_protocol::encode_frame_aead(&header, &cbor_buf, phone_psk, &aead, &sha).unwrap()
        }

        #[test]
        fn build_peer_request_frame_aead_validates_and_returns() {
            let phone_psk = [0x42u8; 32];
            let inner_payload = vec![0xAAu8; 100];

            let frame = build_phone_peer_request_frame(&phone_psk, &inner_payload, 12345);
            let result = build_peer_request_frame_aead(&frame)
                .expect("AEAD PEER_REQUEST frame validation should succeed");

            // Must return the frame unchanged
            assert_eq!(result, frame);

            // Verify it's a valid AEAD frame
            let decoded = decode_frame_aead(&result).unwrap();
            assert_eq!(decoded.header.msg_type, MSG_PEER_REQUEST);
            assert_eq!(decoded.header.nonce, 12345);
        }

        #[test]
        fn build_peer_request_frame_aead_rejects_too_short() {
            let too_short = vec![0u8; sonde_protocol::MIN_FRAME_SIZE_AEAD - 1];
            let result = build_peer_request_frame_aead(&too_short);
            assert!(result.is_err());
        }

        #[test]
        fn build_peer_request_frame_aead_rejects_oversized() {
            let oversized = vec![0xBBu8; sonde_protocol::MAX_FRAME_SIZE + 1];
            let result = build_peer_request_frame_aead(&oversized);
            assert!(result.is_err());
        }

        #[test]
        fn build_peer_request_frame_aead_accepts_max_size() {
            // Build a frame of exactly MAX_FRAME_SIZE with a valid header
            // (msg_type = MSG_PEER_REQUEST).
            let mut exact = vec![0xCCu8; sonde_protocol::MAX_FRAME_SIZE];
            let header = FrameHeader {
                key_hint: 0x0001,
                msg_type: MSG_PEER_REQUEST,
                nonce: 1,
            };
            let header_bytes = header.to_bytes();
            exact[..header_bytes.len()].copy_from_slice(&header_bytes);
            let result = build_peer_request_frame_aead(&exact);
            assert!(result.is_ok(), "MAX_FRAME_SIZE must succeed");
        }

        #[test]
        fn build_peer_request_frame_aead_rejects_wrong_msg_type() {
            let phone_psk = [0x42u8; 32];
            let sha = TestSha256;
            let aead = NodeAead;
            let phone_key_hint = sonde_protocol::key_hint_from_psk(&phone_psk, &sha);

            let header = FrameHeader {
                key_hint: phone_key_hint,
                msg_type: sonde_protocol::MSG_WAKE, // wrong type
                nonce: 1,
            };
            let cbor_map = ciborium::Value::Map(alloc::vec![(
                ciborium::Value::Integer(sonde_protocol::PEER_REQ_KEY_PAYLOAD.into()),
                ciborium::Value::Bytes(vec![0xAA; 10]),
            )]);
            let mut cbor_buf = Vec::new();
            ciborium::into_writer(&cbor_map, &mut cbor_buf).unwrap();

            let frame =
                sonde_protocol::encode_frame_aead(&header, &cbor_buf, &phone_psk, &aead, &sha)
                    .unwrap();
            let result = build_peer_request_frame_aead(&frame);
            assert!(result.is_err(), "wrong msg_type must be rejected");
        }

        // --- AEAD peer registration exchange tests ---

        /// Build a valid PEER_ACK frame using AEAD encryption.
        ///
        /// Per `ble-pairing-protocol.md` §7.2, PEER_ACK payload is
        /// `{ 1: status }` — the `registration_proof` field is retired
        /// under AES-256-GCM.
        fn build_peer_ack_aead(
            identity: &NodeIdentity,
            nonce: u64,
            aead: &NodeAead,
            sha: &TestSha256,
        ) -> Vec<u8> {
            let cbor_map = ciborium::Value::Map(vec![(
                ciborium::Value::Integer(PEER_ACK_KEY_STATUS.into()),
                ciborium::Value::Integer(0.into()),
            )]);
            let mut cbor_buf = Vec::new();
            ciborium::into_writer(&cbor_map, &mut cbor_buf).unwrap();

            let header = FrameHeader {
                key_hint: identity.key_hint,
                msg_type: MSG_PEER_ACK,
                nonce,
            };

            sonde_protocol::encode_frame_aead(&header, &cbor_buf, &identity.psk, aead, sha).unwrap()
        }

        fn test_identity_aead() -> NodeIdentity {
            let psk = [0x42u8; 32];
            let sha = TestSha256;
            NodeIdentity {
                key_hint: sonde_protocol::key_hint_from_psk(&psk, &sha),
                psk,
            }
        }

        #[test]
        fn verify_peer_ack_aead_valid() {
            let sha = TestSha256;
            let aead = NodeAead;
            let identity = test_identity_aead();
            let nonce: u64 = 0xAABBCCDDEEFF0011;

            let ack_frame = build_peer_ack_aead(&identity, nonce, &aead, &sha);
            let result = verify_peer_ack_aead(&ack_frame, &identity, nonce, &aead, &sha);
            assert!(result.is_ok());
        }

        #[test]
        fn verify_peer_ack_aead_wrong_nonce() {
            let sha = TestSha256;
            let aead = NodeAead;
            let identity = test_identity_aead();
            let request_nonce: u64 = 0xAABBCCDDEEFF0011;
            let wrong_nonce: u64 = 0x1111111111111111;

            let ack_frame = build_peer_ack_aead(&identity, wrong_nonce, &aead, &sha);
            let result = verify_peer_ack_aead(&ack_frame, &identity, request_nonce, &aead, &sha);
            assert!(result.is_err());
        }

        /// Registration proof is retired under AEAD (§7.2) — successful
        /// AEAD decryption with `node_psk` is sufficient.  This test
        /// verifies that decryption with a wrong key is rejected.
        #[test]
        fn verify_peer_ack_aead_wrong_key() {
            let sha = TestSha256;
            let aead = NodeAead;
            let identity = test_identity_aead();
            let nonce: u64 = 0x42;

            let ack_frame = build_peer_ack_aead(&identity, nonce, &aead, &sha);

            // Verify with a different PSK — decryption must fail
            let wrong_identity = NodeIdentity {
                key_hint: identity.key_hint,
                psk: [0x99u8; 32],
            };
            let result = verify_peer_ack_aead(&ack_frame, &wrong_identity, nonce, &aead, &sha);
            assert!(result.is_err());
        }

        #[test]
        fn peer_request_exchange_aead_sets_reg_complete() {
            let sha = TestSha256;
            let aead = NodeAead;
            let identity = test_identity_aead();
            let phone_psk = [0x55u8; 32];
            let nonce: u64 = 0x1122334455667788;

            // Build a complete PEER_REQUEST frame as the phone would.
            let inner_payload = vec![0xDE, 0xAD, 0xBE, 0xEF];
            let complete_frame = build_phone_peer_request_frame(&phone_psk, &inner_payload, nonce);

            // Build PEER_ACK with the nonce from the frame.
            let ack = build_peer_ack_aead(&identity, nonce, &aead, &sha);
            let mut transport = MockTransport::with_responses(vec![Some(ack)]);
            let mut storage =
                MockStorage::with_identity(identity.key_hint, identity.psk, complete_frame.clone());

            assert!(!storage.reg_complete);

            let result = peer_request_exchange_aead(
                &mut transport,
                &mut storage,
                &identity,
                &complete_frame,
                &MockClock::new(500),
                &aead,
                &sha,
            )
            .unwrap();

            assert!(result, "AEAD exchange should succeed");
            assert!(storage.reg_complete, "reg_complete must be set");
            assert!(storage.peer_payload.is_some());
        }

        #[test]
        fn peer_request_exchange_aead_timeout() {
            let sha = TestSha256;
            let aead = NodeAead;
            let identity = test_identity_aead();
            let phone_psk = [0x55u8; 32];

            let complete_frame = build_phone_peer_request_frame(&phone_psk, &[0xDE, 0xAD], 0x42);

            let mut transport = MockTransport::new();
            let mut storage =
                MockStorage::with_identity(identity.key_hint, identity.psk, complete_frame.clone());

            let result = peer_request_exchange_aead(
                &mut transport,
                &mut storage,
                &identity,
                &complete_frame,
                &MockClock::new(5000),
                &aead,
                &sha,
            )
            .unwrap();

            assert!(!result, "should timeout");
            assert!(!storage.reg_complete, "reg_complete must NOT be set");
        }
    }
}
