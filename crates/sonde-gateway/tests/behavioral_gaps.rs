// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Behavioral verification tests for gateway structural gaps (Issue #357).
//!
//! These tests go beyond structural (output-exists) checks to verify behavioral
//! correctness: randomness, encoding, state invariants, and API coverage depth.
//!
//! Gaps addressed:
//!   1.  GW-0402 — deterministic CBOR byte-level verification
//!   2.  GW-0600 — HMAC failure state isolation
//!   3.  GW-0705 — factory reset gateway-side behaviour
//!   4.  GW-0103 — `starting_seq` randomness
//!   5.  GW-0104 — COMMAND / APP_DATA_REPLY frame size constraint
//!   6.  GW-0202 — RUN_EPHEMERAL dispatch-time program availability
//!   7.  GW-0802 — RemoveProgram end-to-end verification
//!   8.  GW-0400 — verification-at-ingestion immediate availability
//!   9.  GW-0504 — many-to-one handler routing
//!  10.  GW-0700 — registry entry field completeness
//!  11.  GW-0801 — GetNode RPC field coverage
//!  12.  GW-1201 — `gateway_id` probabilistic uniqueness

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;
use tonic::Request;

use sonde_gateway::admin::pb::gateway_admin_server::GatewayAdmin;
use sonde_gateway::admin::pb::VerificationProfile as PbVerificationProfile;
use sonde_gateway::admin::pb::{
    AssignProgramRequest, Empty, GetNodeRequest, IngestProgramRequest, RegisterNodeRequest,
    RemoveNodeRequest, RemoveProgramRequest, SetScheduleRequest,
};
use sonde_gateway::admin::AdminService;
use sonde_gateway::crypto::{RustCryptoHmac, RustCryptoSha256};
use sonde_gateway::engine::{Gateway, PendingCommand};
use sonde_gateway::gateway_identity::GatewayIdentity;
use sonde_gateway::program::{ProgramLibrary, VerificationProfile};
use sonde_gateway::registry::NodeRecord;
use sonde_gateway::session::SessionManager;
use sonde_gateway::storage::{InMemoryStorage, Storage};
use sonde_gateway::transport::PeerAddress;

use sonde_protocol::{
    decode_frame, encode_frame, verify_frame, CommandPayload, FrameHeader, GatewayMessage, MapDef,
    NodeMessage, ProgramImage, MAX_FRAME_SIZE, MSG_COMMAND, MSG_WAKE,
};

// ─── Test helpers ──────────────────────────────────────────────────────

struct TestNode {
    node_id: String,
    key_hint: u16,
    psk: [u8; 32],
}

impl TestNode {
    fn new(node_id: &str, key_hint: u16, psk: [u8; 32]) -> Self {
        Self {
            node_id: node_id.to_string(),
            key_hint,
            psk,
        }
    }

    fn to_record(&self) -> NodeRecord {
        NodeRecord::new(self.node_id.clone(), self.key_hint, self.psk)
    }

    fn peer_address(&self) -> PeerAddress {
        self.node_id.as_bytes().to_vec()
    }

    fn build_wake(
        &self,
        nonce: u64,
        firmware_abi_version: u32,
        program_hash: &[u8],
        battery_mv: u32,
    ) -> Vec<u8> {
        let header = FrameHeader {
            key_hint: self.key_hint,
            msg_type: MSG_WAKE,
            nonce,
        };
        let msg = NodeMessage::Wake {
            firmware_abi_version,
            program_hash: program_hash.to_vec(),
            battery_mv,
        };
        let cbor = msg.encode().unwrap();
        encode_frame(&header, &cbor, &self.psk, &RustCryptoHmac).unwrap()
    }
}

struct TestHarness {
    storage: Arc<InMemoryStorage>,
    pending_commands: Arc<RwLock<HashMap<String, Vec<PendingCommand>>>>,
    session_manager: Arc<SessionManager>,
    admin: AdminService,
}

impl TestHarness {
    fn new() -> Self {
        let storage = Arc::new(InMemoryStorage::new());
        let pending_commands: Arc<RwLock<HashMap<String, Vec<PendingCommand>>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let session_manager = Arc::new(SessionManager::new(Duration::from_secs(30)));
        let admin = AdminService::new(
            storage.clone(),
            pending_commands.clone(),
            session_manager.clone(),
        );
        Self {
            storage,
            pending_commands,
            session_manager,
            admin,
        }
    }

    fn make_gateway(&self) -> Gateway {
        Gateway::new_with_pending(
            self.storage.clone(),
            self.pending_commands.clone(),
            self.session_manager.clone(),
        )
    }
}

fn make_gateway(storage: Arc<InMemoryStorage>) -> Gateway {
    Gateway::new(storage, Duration::from_secs(30))
}

fn make_cbor_image(bytecode: &[u8]) -> Vec<u8> {
    let image = ProgramImage {
        bytecode: bytecode.to_vec(),
        maps: vec![],
    };
    image.encode_deterministic().unwrap()
}

fn decode_response(raw: &[u8], psk: &[u8; 32]) -> (FrameHeader, GatewayMessage) {
    let decoded = decode_frame(raw).unwrap();
    assert!(verify_frame(&decoded, psk, &RustCryptoHmac));
    let msg = GatewayMessage::decode(decoded.header.msg_type, &decoded.payload).unwrap();
    (decoded.header, msg)
}

async fn do_wake(
    gw: &Gateway,
    node: &TestNode,
    nonce: u64,
    program_hash: &[u8],
) -> (u64, u64, CommandPayload) {
    let frame = node.build_wake(nonce, 1, program_hash, 3300);
    let resp = gw
        .process_frame(&frame, node.peer_address())
        .await
        .expect("expected COMMAND response");
    let (_hdr, msg) = decode_response(&resp, &node.psk);
    match msg {
        GatewayMessage::Command {
            starting_seq,
            timestamp_ms,
            payload,
        } => (starting_seq, timestamp_ms, payload),
        other => panic!("expected Command, got {:?}", other),
    }
}

async fn store_test_program(storage: &InMemoryStorage, bytecode: &[u8]) -> Vec<u8> {
    let lib = ProgramLibrary::new();
    let image = ProgramImage {
        bytecode: bytecode.to_vec(),
        maps: vec![],
    };
    let cbor = image.encode_deterministic().unwrap();
    let record = lib
        .ingest_unverified(cbor, VerificationProfile::Resident)
        .unwrap();
    let hash = record.hash.clone();
    storage.store_program(&record).await.unwrap();
    hash
}

async fn store_test_program_with_profile(
    storage: &InMemoryStorage,
    bytecode: &[u8],
    profile: VerificationProfile,
) -> Vec<u8> {
    let lib = ProgramLibrary::new();
    let image = ProgramImage {
        bytecode: bytecode.to_vec(),
        maps: vec![],
    };
    let cbor = image.encode_deterministic().unwrap();
    let record = lib.ingest_unverified(cbor, profile).unwrap();
    let hash = record.hash.clone();
    storage.store_program(&record).await.unwrap();
    hash
}

/// Minimal `mov r0, 0; exit` BPF program — two valid instructions.
const MINIMAL_BPF: &[u8] = &[
    0xb7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r0, 0
    0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // exit
];

// ═══════════════════════════════════════════════════════════════════════
//  Gap 1 — GW-0402: Deterministic CBOR byte-level verification
// ═══════════════════════════════════════════════════════════════════════

/// Verify that `ProgramImage::encode_deterministic()` produces CBOR that
/// satisfies RFC 8949 §4.2: map keys in ascending order and integer keys
/// using the shortest-form encoding.
#[test]
fn t0402_deterministic_cbor_sorted_keys_and_shortest_form() {
    let image = ProgramImage {
        bytecode: vec![0xAA, 0xBB, 0xCC],
        maps: vec![
            MapDef {
                map_type: 1,
                key_size: 4,
                value_size: 8,
                max_entries: 16,
            },
            MapDef {
                map_type: 2,
                key_size: 32,
                value_size: 64,
                max_entries: 128,
            },
        ],
    };

    let cbor = image.encode_deterministic().unwrap();

    // Parse back as ciborium::Value to inspect the raw structure.
    let value: ciborium::Value = ciborium::from_reader(&cbor[..]).unwrap();
    let outer_map = value.as_map().expect("outer must be a map");

    // Outer map: keys must be [1, 2] (ascending).
    let outer_keys: Vec<u64> = outer_map
        .iter()
        .map(|(k, _)| {
            k.as_integer()
                .and_then(|i| u64::try_from(i).ok())
                .expect("outer key must be a positive integer")
        })
        .collect();
    assert_eq!(outer_keys, vec![1, 2], "outer keys must be [1, 2]");

    // Inner map arrays: each MapDef map must have keys [1, 2, 3, 4].
    let maps_array = outer_map
        .iter()
        .find(|(k, _)| {
            k.as_integer()
                .and_then(|i| u64::try_from(i).ok())
                .map(|n| n == 2)
                .unwrap_or(false)
        })
        .map(|(_, v)| v)
        .expect("outer map must contain key 2")
        .as_array()
        .expect("key 2 must hold an array");
    for (i, map_val) in maps_array.iter().enumerate() {
        let inner_map = map_val.as_map().expect("MapDef must be a map");
        let inner_keys: Vec<u64> = inner_map
            .iter()
            .map(|(k, _)| {
                k.as_integer()
                    .and_then(|i| u64::try_from(i).ok())
                    .expect("MapDef key must be a positive integer")
            })
            .collect();
        assert_eq!(
            inner_keys,
            vec![1, 2, 3, 4],
            "MapDef[{}] keys must be [1, 2, 3, 4]",
            i
        );
    }

    // Byte-level key ordering verification: walk the raw CBOR bytes to
    // confirm map keys appear in ascending order, independent of how
    // ciborium iterates decoded map entries.
    fn read_cbor_uint(data: &[u8], pos: &mut usize) -> (u8, u64) {
        assert!(
            *pos < data.len(),
            "unexpected end of CBOR at offset {}",
            *pos
        );
        let byte = data[*pos];
        *pos += 1;
        let major = byte >> 5;
        let info = byte & 0x1f;
        let val = match info {
            0..=23 => info as u64,
            24 => {
                assert!(
                    *pos < data.len(),
                    "truncated CBOR: expected 1 extra byte at offset {}",
                    *pos
                );
                let v = data[*pos] as u64;
                *pos += 1;
                v
            }
            25 => {
                assert!(
                    *pos + 1 < data.len(),
                    "truncated CBOR: expected 2 extra bytes at offset {}",
                    *pos
                );
                let v = u16::from_be_bytes([data[*pos], data[*pos + 1]]) as u64;
                *pos += 2;
                v
            }
            _ => panic!(
                "unexpected CBOR additional info {} at offset {}",
                info,
                *pos - 1
            ),
        };
        (major, val)
    }

    fn skip_cbor_item(data: &[u8], pos: &mut usize) {
        let (major, val) = read_cbor_uint(data, pos);
        match major {
            0 | 1 => {}                    // unsigned/negative int — already consumed
            2 | 3 => *pos += val as usize, // byte/text string
            4 => {
                for _ in 0..val {
                    skip_cbor_item(data, pos);
                }
            }
            5 => {
                for _ in 0..val {
                    skip_cbor_item(data, pos); // key
                    skip_cbor_item(data, pos); // value
                }
            }
            _ => panic!("unhandled CBOR major type {} at offset {}", major, *pos),
        }
    }

    fn assert_map_keys_ascending(data: &[u8], pos: &mut usize, label: &str) {
        let (major, count) = read_cbor_uint(data, pos);
        assert_eq!(major, 5, "{} must be a CBOR map", label);
        let mut prev_key: Option<u64> = None;
        for _ in 0..count {
            let (km, kv) = read_cbor_uint(data, pos);
            assert_eq!(km, 0, "{} map keys must be unsigned integers", label);
            if let Some(p) = prev_key {
                assert!(
                    kv > p,
                    "{}: keys not in ascending order ({} after {})",
                    label,
                    kv,
                    p
                );
            }
            prev_key = Some(kv);
            skip_cbor_item(data, pos); // skip value
        }
    }

    {
        let mut pos = 0usize;
        // Outer map
        let (major, count) = read_cbor_uint(&cbor, &mut pos);
        assert_eq!(major, 5, "outer CBOR must be a map");
        let mut prev_key: Option<u64> = None;
        for _pair_idx in 0..count {
            let (km, kv) = read_cbor_uint(&cbor, &mut pos);
            assert_eq!(km, 0, "outer map keys must be unsigned integers");
            if let Some(p) = prev_key {
                assert!(
                    kv > p,
                    "outer map keys not in ascending order ({} after {})",
                    kv,
                    p
                );
            }
            prev_key = Some(kv);
            if kv == 2 {
                // key 2 → array of MapDef maps
                let (am, alen) = read_cbor_uint(&cbor, &mut pos);
                assert_eq!(am, 4, "value of outer key 2 must be a CBOR array");
                for mi in 0..alen {
                    let label = format!("MapDef[{}]", mi);
                    assert_map_keys_ascending(&cbor, &mut pos, &label);
                }
            } else {
                // skip value for other keys
                skip_cbor_item(&cbor, &mut pos);
            }
        }
    }

    // Shortest-form encoding verification: inspect the raw CBOR bytes
    // directly. Re-encoding a decoded `ciborium::Value` always produces
    // minimal encoding, which makes the check a tautology. Instead, walk
    // the raw bytes and assert that no integer uses the 2-byte form
    // `[major|24, val]` when `val` fits in the single-byte direct form
    // (values 0–23).
    fn assert_no_non_minimal_ints(data: &[u8]) {
        let mut pos = 0;
        while pos < data.len() {
            let byte = data[pos];
            let major = byte >> 5;
            let info = byte & 0x1f;
            pos += 1;

            let val = match info {
                0..=23 => info as usize,
                24 => {
                    assert!(
                        pos < data.len(),
                        "truncated CBOR: expected 1-byte value at offset {}",
                        pos
                    );
                    let v = data[pos] as usize;
                    assert!(
                        v > 23,
                        "non-minimal CBOR at byte {}: value {v} \
                         encoded as 2 bytes but fits in single-byte form",
                        pos - 1
                    );
                    pos += 1;
                    v
                }
                25 => {
                    assert!(
                        pos + 1 < data.len(),
                        "truncated CBOR: expected 2-byte value at offset {}",
                        pos
                    );
                    let v = u16::from_be_bytes([data[pos], data[pos + 1]]) as usize;
                    assert!(
                        v > 0xff,
                        "non-minimal CBOR at byte {}: value {v} \
                         encoded as 2-byte uint but fits in 1 byte",
                        pos - 1
                    );
                    pos += 2;
                    v
                }
                26 => {
                    assert!(
                        pos + 3 < data.len(),
                        "truncated CBOR: expected 4-byte value at offset {}",
                        pos
                    );
                    let v = u32::from_be_bytes([
                        data[pos],
                        data[pos + 1],
                        data[pos + 2],
                        data[pos + 3],
                    ]) as usize;
                    assert!(
                        v > 0xffff,
                        "non-minimal CBOR at byte {}: value {v} \
                         encoded as 4-byte uint but fits in 2 bytes",
                        pos - 1
                    );
                    pos += 4;
                    v
                }
                27 => {
                    assert!(
                        pos + 7 < data.len(),
                        "truncated CBOR: expected 8-byte value at offset {}",
                        pos
                    );
                    let v = u64::from_be_bytes([
                        data[pos],
                        data[pos + 1],
                        data[pos + 2],
                        data[pos + 3],
                        data[pos + 4],
                        data[pos + 5],
                        data[pos + 6],
                        data[pos + 7],
                    ]) as usize;
                    assert!(
                        v > 0xffff_ffff,
                        "non-minimal CBOR at byte {}: value {v} \
                         encoded as 8-byte uint but fits in 4 bytes",
                        pos - 1
                    );
                    pos += 8;
                    v
                }
                _ => {
                    // Indefinite-length encodings (info == 31) for byte/text strings,
                    // arrays, and maps are not allowed in deterministic CBOR.
                    if info == 31 && (major == 2 || major == 3 || major == 4 || major == 5) {
                        panic!(
                            "indefinite-length CBOR item (major type {} with info 31) \
                             at byte {} is not allowed in deterministic CBOR",
                            major,
                            pos - 1
                        );
                    }
                    continue;
                }
            };

            // Skip over content bytes for byte strings and text strings.
            if major == 2 || major == 3 {
                assert!(
                    pos + val <= data.len(),
                    "truncated CBOR: string length {} at offset {} exceeds buffer",
                    val,
                    pos - 1
                );
                pos += val;
            }
        }
    }

    assert_no_non_minimal_ints(&cbor);

    // Verify round-trip: same input always produces the same bytes.
    let cbor2 = image.encode_deterministic().unwrap();
    assert_eq!(cbor, cbor2, "deterministic encoding must be stable");
}

/// Verify that two `ProgramImage` values with identical content produce the
/// same SHA-256 hash (the determinism guarantee that GW-0402 relies on).
#[test]
fn t0402_deterministic_hash_identity() {
    let image_a = ProgramImage {
        bytecode: vec![0x01, 0x02, 0x03],
        maps: vec![MapDef {
            map_type: 1,
            key_size: 4,
            value_size: 8,
            max_entries: 256,
        }],
    };
    let image_b = image_a.clone();

    let cbor_a = image_a.encode_deterministic().unwrap();
    let cbor_b = image_b.encode_deterministic().unwrap();
    assert_eq!(cbor_a, cbor_b);

    let sha = RustCryptoSha256;
    let hash_a = sonde_protocol::program_hash(&cbor_a, &sha);
    let hash_b = sonde_protocol::program_hash(&cbor_b, &sha);
    assert_eq!(
        hash_a, hash_b,
        "identical images must produce identical hashes"
    );
}

// ═══════════════════════════════════════════════════════════════════════
//  Gap 2 — GW-0600: HMAC failure state isolation
// ═══════════════════════════════════════════════════════════════════════

/// After HMAC verification failure from a known `key_hint`, the gateway's
/// internal state (sessions, registry) must remain unchanged.
#[tokio::test]
async fn t0600_hmac_failure_state_unchanged() {
    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway(storage.clone());

    let node = TestNode::new("node-iso", 0x0600, [0x66; 32]);
    storage.upsert_node(&node.to_record()).await.unwrap();

    // Establish a valid session.
    let frame_ok = node.build_wake(1, 1, &[0u8; 32], 3300);
    let resp = gw
        .process_frame(&frame_ok, node.peer_address())
        .await
        .expect("valid WAKE must produce a response");
    let (_, msg) = decode_response(&resp, &node.psk);
    let _starting_seq = match msg {
        GatewayMessage::Command { starting_seq, .. } => starting_seq,
        other => panic!("expected Command, got {:?}", other),
    };

    // Snapshot state after valid WAKE.
    let session_before = gw
        .session_manager()
        .get_session("node-iso")
        .await
        .expect("session must exist");
    let node_before = storage.get_node("node-iso").await.unwrap().unwrap();

    // Send a frame with the correct key_hint but corrupted HMAC.
    let mut bad_frame = node.build_wake(2, 1, &[0u8; 32], 2900);
    let last = bad_frame.len() - 1;
    bad_frame[last] ^= 0xFF;

    let resp = gw.process_frame(&bad_frame, node.peer_address()).await;
    assert!(resp.is_none(), "corrupted HMAC must be silently discarded");

    // Verify: session state unchanged (same starting_seq, same state variant).
    let session_after = gw
        .session_manager()
        .get_session("node-iso")
        .await
        .expect("session must still exist");
    assert_eq!(
        session_before.next_expected_seq, session_after.next_expected_seq,
        "next_expected_seq must not change after HMAC failure"
    );
    assert_eq!(
        std::mem::discriminant(&session_before.state),
        std::mem::discriminant(&session_after.state),
        "session state variant must not change after HMAC failure"
    );

    // Verify: registry unchanged (battery_mv, firmware_abi_version not overwritten
    // by the bad frame's payload values).
    let node_after = storage.get_node("node-iso").await.unwrap().unwrap();
    assert_eq!(
        node_before.last_battery_mv, node_after.last_battery_mv,
        "battery_mv must not change after HMAC failure"
    );
    assert_eq!(
        node_before.firmware_abi_version, node_after.firmware_abi_version,
        "firmware_abi_version must not change after HMAC failure"
    );

    // Verify: no new session was created (session count unchanged).
    assert_eq!(gw.session_manager().active_count().await, 1);
}

// ═══════════════════════════════════════════════════════════════════════
//  Gap 3 — GW-0705: Factory reset gateway-side behaviour
// ═══════════════════════════════════════════════════════════════════════

/// After RemoveNode (gateway-side factory reset), a WAKE from the removed
/// node must be silently discarded — the node is unknown.
#[tokio::test]
async fn t0705_factory_reset_wake_discarded() {
    let h = TestHarness::new();
    let psk = [0x77; 32];

    // Register the node.
    h.admin
        .register_node(Request::new(RegisterNodeRequest {
            node_id: "reset-node".into(),
            key_hint: 0x0705,
            psk: psk.to_vec(),
        }))
        .await
        .unwrap();

    // Verify the node is reachable: send WAKE and expect a response.
    let gw = h.make_gateway();
    let node = TestNode::new("reset-node", 0x0705, psk);
    let (_, _, payload) = do_wake(&gw, &node, 1, &[0u8; 32]).await;
    assert!(
        matches!(payload, CommandPayload::Nop),
        "node must be reachable before removal"
    );

    // Remove the node (gateway-side factory reset).
    h.admin
        .remove_node(Request::new(RemoveNodeRequest {
            node_id: "reset-node".into(),
        }))
        .await
        .unwrap();

    // Verify: GetNode returns NotFound.
    let err = h
        .admin
        .get_node(Request::new(GetNodeRequest {
            node_id: "reset-node".into(),
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::NotFound);

    // Snapshot session state after removal, before the second WAKE.
    let session_before = gw.session_manager().get_session("reset-node").await;

    // Verify: WAKE from the removed node is silently discarded.
    let frame = node.build_wake(2, 1, &[0u8; 32], 3300);
    let resp = gw.process_frame(&frame, node.peer_address()).await;
    assert!(
        resp.is_none(),
        "WAKE from removed node must be silently discarded"
    );

    // Verify: the post-removal WAKE did not create or replace a session.
    // Checking `active_count() <= 1` is insufficient because sessions are
    // keyed by `node_id` — an overwrite keeps the count at 1. Instead,
    // snapshot session state before the second WAKE and assert it's unchanged.
    let session_after = gw.session_manager().get_session("reset-node").await;
    match (&session_before, &session_after) {
        (Some(before), Some(after)) => {
            assert_eq!(
                before.next_expected_seq, after.next_expected_seq,
                "session must not be replaced by post-removal WAKE"
            );
        }
        (None, None) => {
            // Session was cleaned up during removal — no new session either.
        }
        (None, Some(_)) => {
            panic!("post-removal WAKE must not create a new session");
        }
        (Some(_), None) => {
            // Session was cleaned up during removal — acceptable.
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════
//  Gap 4 — GW-0103: `starting_seq` randomness
// ═══════════════════════════════════════════════════════════════════════

/// `starting_seq` must be random. Consecutive sessions (from distinct
/// nodes) must not return zero or a constant value.
#[tokio::test]
async fn t0103_starting_seq_not_zero_or_constant() {
    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway(storage.clone());

    let mut seq_values = Vec::new();
    for i in 1..=10u64 {
        // Use a distinct node per iteration to ensure a fresh session each time.
        let node_id = format!("node-rand-{i}");
        let node = TestNode::new(&node_id, 0x0103 + i as u16, [0x13; 32]);
        storage.upsert_node(&node.to_record()).await.unwrap();

        let frame = node.build_wake(i, 1, &[0u8; 32], 3300);
        let resp = gw
            .process_frame(&frame, node.peer_address())
            .await
            .expect("expected COMMAND response");
        let (_, msg) = decode_response(&resp, &node.psk);
        match msg {
            GatewayMessage::Command { starting_seq, .. } => {
                seq_values.push(starting_seq);
            }
            other => panic!("expected Command, got {:?}", other),
        }
    }

    // Not all zero.
    assert!(
        seq_values.iter().any(|&s| s != 0),
        "starting_seq must not always be zero; got {:?}",
        seq_values
    );

    // Not all identical.
    let unique: HashSet<u64> = seq_values.iter().copied().collect();
    assert!(
        unique.len() > 1,
        "starting_seq values must vary across sessions; all were {}",
        seq_values[0]
    );
}

// ═══════════════════════════════════════════════════════════════════════
//  Gap 5 — GW-0104: COMMAND frame size constraint
// ═══════════════════════════════════════════════════════════════════════

/// COMMAND response frames (not just CHUNK) must be ≤ 250 bytes.
#[tokio::test]
async fn t0104_command_frame_size_within_limit() {
    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway(storage.clone());

    let node = TestNode::new("node-cmdsz", 0x0104, [0x14; 32]);
    let program_hash = store_test_program(&storage, b"a-resident-program").await;

    let mut record = node.to_record();
    record.assigned_program_hash = Some(program_hash.clone());
    storage.upsert_node(&record).await.unwrap();

    // WAKE with a different hash → triggers UPDATE_PROGRAM (larger COMMAND payload).
    let frame = node.build_wake(1, 1, &[0u8; 32], 3300);
    let resp = gw
        .process_frame(&frame, node.peer_address())
        .await
        .expect("expected COMMAND response");

    assert!(
        resp.len() <= MAX_FRAME_SIZE,
        "COMMAND response {} bytes exceeds {} byte limit",
        resp.len(),
        MAX_FRAME_SIZE,
    );

    // Also verify it's a valid COMMAND.
    let (hdr, _msg) = decode_response(&resp, &node.psk);
    assert_eq!(hdr.msg_type, MSG_COMMAND);
}

/// Verify that NOP, UpdateSchedule, and Reboot COMMAND frames also
/// respect the 250-byte limit.
#[tokio::test]
async fn t0104_all_command_types_within_limit() {
    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway(storage.clone());

    let node = TestNode::new("node-allcmd", 0x1040, [0x15; 32]);
    storage.upsert_node(&node.to_record()).await.unwrap();

    // NOP
    let frame = node.build_wake(1, 1, &[0u8; 32], 3300);
    let resp = gw.process_frame(&frame, node.peer_address()).await.unwrap();
    assert!(resp.len() <= MAX_FRAME_SIZE, "NOP frame too large");

    // UpdateSchedule
    gw.queue_command(
        "node-allcmd",
        PendingCommand::UpdateSchedule { interval_s: 300 },
    )
    .await;
    let frame = node.build_wake(2, 1, &[0u8; 32], 3300);
    let resp = gw.process_frame(&frame, node.peer_address()).await.unwrap();
    assert!(
        resp.len() <= MAX_FRAME_SIZE,
        "UpdateSchedule frame too large"
    );

    // Reboot
    gw.queue_command("node-allcmd", PendingCommand::Reboot)
        .await;
    let frame = node.build_wake(3, 1, &[0u8; 32], 3300);
    let resp = gw.process_frame(&frame, node.peer_address()).await.unwrap();
    assert!(resp.len() <= MAX_FRAME_SIZE, "Reboot frame too large");
}

// ═══════════════════════════════════════════════════════════════════════
//  Gap 6 — GW-0202: RUN_EPHEMERAL dispatch-time program availability
// ═══════════════════════════════════════════════════════════════════════

/// If an ephemeral program is deleted from storage after being queued but
/// before the node wakes, the gateway must fall through to the next
/// priority command (NOP) without crashing.
#[tokio::test]
async fn t0202_run_ephemeral_dispatch_deleted_program() {
    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway(storage.clone());

    let node = TestNode::new("node-eph-del", 0x0202, [0x22; 32]);
    storage.upsert_node(&node.to_record()).await.unwrap();

    // Ingest an ephemeral program and queue it.
    let eph_hash = store_test_program_with_profile(
        &storage,
        b"ephemeral-to-delete",
        VerificationProfile::Ephemeral,
    )
    .await;

    gw.queue_command(
        "node-eph-del",
        PendingCommand::RunEphemeral {
            program_hash: eph_hash.clone(),
        },
    )
    .await;

    // Delete the program from storage before the node wakes.
    storage.delete_program(&eph_hash).await.unwrap();

    // Send WAKE — gateway should gracefully fall through to NOP.
    let (_, _, payload) = do_wake(&gw, &node, 1, &[0u8; 32]).await;
    assert_eq!(
        payload.command_type(),
        sonde_protocol::CMD_NOP,
        "gateway must fall through to NOP when ephemeral program is missing at dispatch"
    );
}

/// If an ephemeral program exists at dispatch time, it must be delivered
/// correctly via RUN_EPHEMERAL — verifying dispatch-time availability.
#[tokio::test]
async fn t0202_run_ephemeral_dispatch_program_present() {
    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway(storage.clone());

    let node = TestNode::new("node-eph-ok", 0x2020, [0x23; 32]);
    storage.upsert_node(&node.to_record()).await.unwrap();

    let eph_hash = store_test_program_with_profile(
        &storage,
        b"ephemeral-available",
        VerificationProfile::Ephemeral,
    )
    .await;

    gw.queue_command(
        "node-eph-ok",
        PendingCommand::RunEphemeral {
            program_hash: eph_hash.clone(),
        },
    )
    .await;

    let (_, _, payload) = do_wake(&gw, &node, 1, &[0u8; 32]).await;
    assert_eq!(
        payload.command_type(),
        sonde_protocol::CMD_RUN_EPHEMERAL,
        "ephemeral program must be delivered at dispatch time"
    );
    match &payload {
        CommandPayload::RunEphemeral { program_hash, .. } => {
            assert_eq!(program_hash, &eph_hash);
        }
        other => panic!("expected RunEphemeral, got {:?}", other),
    }
}

// ═══════════════════════════════════════════════════════════════════════
//  Gap 7 — GW-0802: RemoveProgram end-to-end verification
// ═══════════════════════════════════════════════════════════════════════

/// After RemoveProgram, the program must be completely gone:
/// - not in ListPrograms
/// - cannot be assigned to a node
/// - re-ingesting returns the same hash (not cached stale)
#[tokio::test]
async fn t0802_remove_program_verified_gone() {
    let h = TestHarness::new();

    // Register a node for assignment testing.
    h.admin
        .register_node(Request::new(RegisterNodeRequest {
            node_id: "prog-node".into(),
            key_hint: 0x0802,
            psk: vec![0x82; 32],
        }))
        .await
        .unwrap();

    // Ingest a program.
    let cbor = make_cbor_image(&[0xDE, 0xAD, 0xBE, 0xEF]);
    let ingest = h
        .admin
        .ingest_program(Request::new(IngestProgramRequest {
            image_data: cbor.clone(),
            verification_profile: PbVerificationProfile::Ephemeral.into(),
            abi_version: None,
        }))
        .await
        .unwrap()
        .into_inner();
    let hash = ingest.program_hash.clone();

    // Remove the program.
    h.admin
        .remove_program(Request::new(RemoveProgramRequest {
            program_hash: hash.clone(),
        }))
        .await
        .unwrap();

    // Verify: not in ListPrograms.
    let list = h
        .admin
        .list_programs(Request::new(Empty {}))
        .await
        .unwrap()
        .into_inner();
    assert!(
        list.programs.is_empty(),
        "program must not appear in ListPrograms after removal"
    );

    // Verify: assignment fails (program not found in storage).
    let err = h
        .admin
        .assign_program(Request::new(AssignProgramRequest {
            node_id: "prog-node".into(),
            program_hash: hash.clone(),
        }))
        .await
        .unwrap_err();
    assert_eq!(
        err.code(),
        tonic::Code::NotFound,
        "assigning removed program must fail with NotFound"
    );

    // Verify: re-ingesting produces the same hash (storage truly cleared).
    let re_ingest = h
        .admin
        .ingest_program(Request::new(IngestProgramRequest {
            image_data: cbor,
            verification_profile: PbVerificationProfile::Ephemeral.into(),
            abi_version: None,
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        re_ingest.program_hash, hash,
        "re-ingesting same content must produce identical hash"
    );
}

// ═══════════════════════════════════════════════════════════════════════
//  Gap 8 — GW-0400: Verification at ingestion — immediate availability
// ═══════════════════════════════════════════════════════════════════════

/// A program must be available for assignment and delivery immediately
/// after IngestProgram returns — verification is not deferred.
#[tokio::test]
async fn t0400_program_available_immediately_after_ingestion() {
    let h = TestHarness::new();
    let psk = [0x40; 32];

    h.admin
        .register_node(Request::new(RegisterNodeRequest {
            node_id: "immed-node".into(),
            key_hint: 0x0400,
            psk: psk.to_vec(),
        }))
        .await
        .unwrap();

    // Ingest program.
    let cbor = make_cbor_image(MINIMAL_BPF);
    let ingest = h
        .admin
        .ingest_program(Request::new(IngestProgramRequest {
            image_data: cbor,
            verification_profile: PbVerificationProfile::Resident.into(),
            abi_version: None,
        }))
        .await
        .unwrap()
        .into_inner();

    // Assign immediately (no delay).
    h.admin
        .assign_program(Request::new(AssignProgramRequest {
            node_id: "immed-node".into(),
            program_hash: ingest.program_hash.clone(),
        }))
        .await
        .unwrap();

    // Send WAKE with a different hash → must get UPDATE_PROGRAM right away.
    let gw = h.make_gateway();
    let node = TestNode::new("immed-node", 0x0400, psk);
    let (_, _, payload) = do_wake(&gw, &node, 1, &[0u8; 32]).await;

    match payload {
        CommandPayload::UpdateProgram { program_hash, .. } => {
            assert_eq!(
                program_hash, ingest.program_hash,
                "program must be available immediately — no deferred verification"
            );
        }
        other => panic!(
            "expected UpdateProgram (immediate availability), got {:?}",
            other
        ),
    }
}

// ═══════════════════════════════════════════════════════════════════════
//  Gap 9 — GW-0504: Many-to-one handler routing (config-level)
// ═══════════════════════════════════════════════════════════════════════

/// Multiple programs routed to a single handler via `ProgramMatcher::Any`.
///
/// Verifies that a gateway configured with one catch-all handler
/// configuration correctly routes APP_DATA from two nodes with different
/// `current_program_hash` values to a handler matched by that
/// configuration. Requires Python 3 for the handler process. Skipped if
/// unavailable or if the handler is too slow (CI).
///
/// Note: this test does not assert that both requests are handled by the
/// same handler process instance; it validates the routing configuration.
/// `find_handler` routing logic is also covered by unit tests in
/// `handler.rs`; this integration test verifies the full end-to-end path.
#[tokio::test]
#[ignore] // Requires Python 3; opt-in via `cargo test -- --ignored`
async fn t0504_many_to_one_handler_routing() {
    if !python_available() {
        eprintln!("SKIPPED: Python 3 not available");
        return;
    }

    use sonde_gateway::handler::{HandlerConfig, HandlerRouter, ProgramMatcher};
    use sonde_protocol::MSG_APP_DATA;
    use std::io::Write;

    let tmp = tempfile::tempdir().unwrap();
    let script_path = tmp.path().join("multi_echo.py");
    let mut f = std::fs::File::create(&script_path).unwrap();
    f.write_all(MULTI_ECHO_HANDLER_PY.as_bytes()).unwrap();
    f.flush().unwrap();

    let mut args: Vec<String> = python_args().iter().map(|s| s.to_string()).collect();
    args.push(script_path.to_str().unwrap().to_string());
    let router = Arc::new(HandlerRouter::new(vec![HandlerConfig {
        matchers: vec![ProgramMatcher::Any],
        command: python_cmd().to_string(),
        args,
        reply_timeout: None,
    }]));

    let storage = Arc::new(InMemoryStorage::new());
    let gw = Gateway::new_with_handler(storage.clone(), Duration::from_secs(30), router);

    // Two nodes with different program hashes — both must route to the
    // same catch-all handler.
    let hash_a = vec![0xA0; 32];
    let hash_b = vec![0xB0; 32];

    let node_a = TestNode::new("node-504a", 0x504A, [0x5A; 32]);
    let node_b = TestNode::new("node-504b", 0x504B, [0x5B; 32]);

    let mut rec_a = node_a.to_record();
    rec_a.assigned_program_hash = Some(hash_a.clone());
    rec_a.current_program_hash = Some(hash_a.clone());
    storage.upsert_node(&rec_a).await.unwrap();

    let mut rec_b = node_b.to_record();
    rec_b.assigned_program_hash = Some(hash_b.clone());
    rec_b.current_program_hash = Some(hash_b.clone());
    storage.upsert_node(&rec_b).await.unwrap();

    // WAKE both nodes.
    let (seq_a, _, _) = do_wake(&gw, &node_a, 100, &hash_a).await;
    let (seq_b, _, _) = do_wake(&gw, &node_b, 200, &hash_b).await;

    // Send APP_DATA from node A.
    let header_a = FrameHeader {
        key_hint: node_a.key_hint,
        msg_type: MSG_APP_DATA,
        nonce: seq_a,
    };
    let msg_a = NodeMessage::AppData {
        blob: vec![0x01, 0x02],
    };
    let cbor_a = msg_a.encode().unwrap();
    let frame_a = encode_frame(&header_a, &cbor_a, &node_a.psk, &RustCryptoHmac).unwrap();
    let resp_a = tokio::time::timeout(
        Duration::from_secs(5),
        gw.process_frame(&frame_a, node_a.peer_address()),
    )
    .await
    .expect("handler did not respond for node A within timeout; routing must be enforced");
    assert!(
        resp_a.is_some(),
        "handler did not respond for node A; routing must be enforced"
    );
    let (_, msg_a_reply) = decode_response(&resp_a.unwrap(), &node_a.psk);
    assert!(
        matches!(msg_a_reply, GatewayMessage::AppDataReply { .. }),
        "node A must receive AppDataReply"
    );

    // Send APP_DATA from node B — same handler process must serve it.
    let header_b = FrameHeader {
        key_hint: node_b.key_hint,
        msg_type: MSG_APP_DATA,
        nonce: seq_b,
    };
    let msg_b = NodeMessage::AppData {
        blob: vec![0x03, 0x04],
    };
    let cbor_b = msg_b.encode().unwrap();
    let frame_b = encode_frame(&header_b, &cbor_b, &node_b.psk, &RustCryptoHmac).unwrap();
    let resp_b = tokio::time::timeout(
        Duration::from_secs(5),
        gw.process_frame(&frame_b, node_b.peer_address()),
    )
    .await
    .expect("handler did not respond for node B within timeout; routing must be enforced");
    assert!(
        resp_b.is_some(),
        "handler did not respond for node B; routing must be enforced"
    );
    let (_, msg_b_reply) = decode_response(&resp_b.unwrap(), &node_b.psk);
    assert!(
        matches!(msg_b_reply, GatewayMessage::AppDataReply { .. }),
        "node B must also receive AppDataReply from same handler"
    );
}

// ═══════════════════════════════════════════════════════════════════════
//  Gap 10 — GW-0700: Registry entry field completeness
// ═══════════════════════════════════════════════════════════════════════

/// After registration + WAKE, the node record must contain all specified
/// fields with correct values: node_id, key_hint, psk,
/// assigned_program_hash, schedule_interval_s, firmware_abi_version,
/// last_battery_mv, last_seen, and current_program_hash (None, since
/// it is only set via PROGRAM_ACK).
#[tokio::test]
async fn t0700_registry_entry_all_fields_present() {
    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway(storage.clone());

    let node = TestNode::new("node-fields", 0x0700, [0x70; 32]);
    let program_hash = store_test_program(&storage, b"completeness-bytecode").await;

    let mut record = node.to_record();
    record.assigned_program_hash = Some(program_hash.clone());
    record.schedule_interval_s = 120;
    storage.upsert_node(&record).await.unwrap();

    // WAKE to populate telemetry fields.
    let frame = node.build_wake(42, 5, &program_hash, 3700);
    let resp = gw
        .process_frame(&frame, node.peer_address())
        .await
        .expect("expected response");

    // Should be NOP since program_hash matches.
    let (_, msg) = decode_response(&resp, &node.psk);
    assert!(matches!(
        msg,
        GatewayMessage::Command {
            payload: CommandPayload::Nop,
            ..
        }
    ));

    // Inspect the stored record.
    let stored = storage.get_node("node-fields").await.unwrap().unwrap();

    assert_eq!(stored.node_id, "node-fields");
    assert_eq!(stored.key_hint, 0x0700);
    assert_eq!(stored.psk, [0x70; 32]);
    assert_eq!(
        stored.assigned_program_hash.as_deref(),
        Some(program_hash.as_slice())
    );
    assert_eq!(stored.schedule_interval_s, 120);
    assert_eq!(
        stored.firmware_abi_version,
        Some(5),
        "firmware_abi_version must be updated by WAKE"
    );
    assert_eq!(
        stored.last_battery_mv,
        Some(3700),
        "last_battery_mv must be updated by WAKE"
    );
    assert!(
        stored.last_seen.is_some(),
        "last_seen must be set after WAKE"
    );
    // `current_program_hash` is only set via PROGRAM_ACK, not WAKE.
    assert_eq!(
        stored.current_program_hash, None,
        "current_program_hash must not be set by WAKE alone"
    );
}

// ═══════════════════════════════════════════════════════════════════════
//  Gap 11 — GW-0801: GetNode RPC field coverage
// ═══════════════════════════════════════════════════════════════════════

/// GetNode must return all documented fields: node_id, key_hint,
/// assigned_program_hash, last_battery_mv, last_firmware_abi_version,
/// last_seen_ms, schedule_interval_s.
#[tokio::test]
async fn t0801_get_node_returns_all_fields() {
    let h = TestHarness::new();
    let psk = [0x81; 32];

    h.admin
        .register_node(Request::new(RegisterNodeRequest {
            node_id: "full-node".into(),
            key_hint: 0x0801,
            psk: psk.to_vec(),
        }))
        .await
        .unwrap();

    // Set schedule.
    h.admin
        .set_schedule(Request::new(SetScheduleRequest {
            node_id: "full-node".into(),
            interval_s: 180,
        }))
        .await
        .unwrap();

    // Ingest and assign a program.
    let cbor = make_cbor_image(MINIMAL_BPF);
    let ingest = h
        .admin
        .ingest_program(Request::new(IngestProgramRequest {
            image_data: cbor,
            verification_profile: PbVerificationProfile::Resident.into(),
            abi_version: None,
        }))
        .await
        .unwrap()
        .into_inner();

    h.admin
        .assign_program(Request::new(AssignProgramRequest {
            node_id: "full-node".into(),
            program_hash: ingest.program_hash.clone(),
        }))
        .await
        .unwrap();

    // Send WAKE to populate telemetry (battery, ABI, last_seen).
    // First consume the schedule command via a WAKE.
    let gw = h.make_gateway();
    let node = TestNode::new("full-node", 0x0801, psk);
    let _ = do_wake(&gw, &node, 1, &[0u8; 32]).await;

    // GetNode must return all fields.
    let info = h
        .admin
        .get_node(Request::new(GetNodeRequest {
            node_id: "full-node".into(),
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(info.node_id, "full-node");
    assert_eq!(info.key_hint, 0x0801);
    assert_eq!(info.assigned_program_hash, ingest.program_hash);
    assert_eq!(
        info.last_battery_mv,
        Some(3300),
        "battery_mv must be populated from WAKE"
    );
    assert_eq!(
        info.last_firmware_abi_version,
        Some(1),
        "firmware_abi_version must be populated from WAKE"
    );
    assert!(
        info.last_seen_ms.is_some(),
        "last_seen_ms must be set after WAKE"
    );
    assert_eq!(
        info.schedule_interval_s,
        Some(180),
        "schedule_interval_s must reflect SetSchedule"
    );
}

// ═══════════════════════════════════════════════════════════════════════
//  Gap 12 — GW-1201: gateway_id probabilistic uniqueness
// ═══════════════════════════════════════════════════════════════════════

/// Generate multiple gateway identities and verify all `gateway_id` values
/// are unique. With 16 random bytes, collision in 100 samples is
/// astronomically unlikely — a failure indicates a CSPRNG bug.
#[test]
fn t1201_gateway_id_uniqueness() {
    let mut ids = HashSet::new();
    for _ in 0..100 {
        let identity = GatewayIdentity::generate().unwrap();
        let id = *identity.gateway_id();
        assert_ne!(id, [0u8; 16], "gateway_id must not be all-zero");
        assert!(
            ids.insert(id),
            "duplicate gateway_id detected — CSPRNG failure"
        );
    }
    assert_eq!(ids.len(), 100);
}

// ─── Python handler helpers (Gap 9) ───────────────────────────────────

fn python_cmd() -> &'static str {
    if cfg!(windows) {
        "py"
    } else {
        "python3"
    }
}

fn python_args() -> Vec<&'static str> {
    if cfg!(windows) {
        vec!["-3"]
    } else {
        vec![]
    }
}

fn python_available() -> bool {
    let mut cmd = std::process::Command::new(python_cmd());
    for arg in python_args() {
        cmd.arg(arg);
    }
    match cmd
        .arg("--version")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
    {
        Ok(output) => {
            if !output.status.success() {
                return false;
            }
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            stdout.starts_with("Python 3") || stderr.starts_with("Python 3")
        }
        Err(_) => false,
    }
}

/// Multi-echo handler: reads multiple DATA messages, replies to each with
/// the same payload. Ignores EVENT messages.
const MULTI_ECHO_HANDLER_PY: &str = r#"
import sys, struct

def read_exact(n):
    buf = bytearray()
    while len(buf) < n:
        chunk = sys.stdin.buffer.read(n - len(buf))
        if not chunk:
            sys.exit(0)
        buf.extend(chunk)
    return bytes(buf)

def read_msg():
    raw = read_exact(4)
    length = struct.unpack('>I', raw)[0]
    data = read_exact(length)
    return data

def write_msg(payload):
    sys.stdout.buffer.write(struct.pack('>I', len(payload)))
    sys.stdout.buffer.write(payload)
    sys.stdout.buffer.flush()

def decode_cbor_map(data):
    result = {}
    idx = 0
    if data[idx] & 0xe0 != 0xa0 and data[idx] != 0xbf:
        raise ValueError(f"expected map, got {data[idx]:#x}")
    if data[idx] == 0xbf:
        idx += 1
        while data[idx] != 0xff:
            k, idx = decode_item(data, idx)
            v, idx = decode_item(data, idx)
            result[k] = v
        idx += 1
    else:
        info = data[idx] & 0x1f
        idx += 1
        if info < 24:
            count = info
        elif info == 24:
            count = data[idx]; idx += 1
        elif info == 25:
            count = struct.unpack('>H', data[idx:idx+2])[0]; idx += 2
        elif info == 26:
            count = struct.unpack('>I', data[idx:idx+4])[0]; idx += 4
        elif info == 27:
            count = struct.unpack('>Q', data[idx:idx+8])[0]; idx += 8
        else:
            raise ValueError(f"unsupported map additional info {info}")
        for _ in range(count):
            k, idx = decode_item(data, idx)
            v, idx = decode_item(data, idx)
            result[k] = v
    return result

def decode_item(data, idx):
    major = data[idx] >> 5
    info = data[idx] & 0x1f
    idx += 1
    if info < 24:
        val = info
    elif info == 24:
        val = data[idx]; idx += 1
    elif info == 25:
        val = struct.unpack('>H', data[idx:idx+2])[0]; idx += 2
    elif info == 26:
        val = struct.unpack('>I', data[idx:idx+4])[0]; idx += 4
    elif info == 27:
        val = struct.unpack('>Q', data[idx:idx+8])[0]; idx += 8
    else:
        raise ValueError(f"unsupported additional info {info}")
    if major == 0:
        return val, idx
    elif major == 2:
        return data[idx:idx+val], idx+val
    elif major == 3:
        return data[idx:idx+val].decode('utf-8'), idx+val
    elif major == 5:
        result = {}
        for _ in range(val):
            k, idx = decode_item(data, idx)
            v, idx = decode_item(data, idx)
            result[k] = v
        return result, idx
    else:
        raise ValueError(f"unsupported major type {major}")

def encode_uint(major, val):
    major_bits = major << 5
    if val < 24:
        return bytes([major_bits | val])
    elif val < 256:
        return bytes([major_bits | 24, val])
    elif val < 65536:
        return bytes([major_bits | 25]) + struct.pack('>H', val)
    elif val < 2**32:
        return bytes([major_bits | 26]) + struct.pack('>I', val)
    else:
        return bytes([major_bits | 27]) + struct.pack('>Q', val)

def encode_cbor_map(pairs):
    out = encode_uint(5, len(pairs))
    for k, v in pairs:
        out += encode_item(k)
        out += encode_item(v)
    return out

def encode_item(val):
    if isinstance(val, int):
        return encode_uint(0, val)
    elif isinstance(val, bytes):
        return encode_uint(2, len(val)) + val
    elif isinstance(val, str):
        encoded = val.encode('utf-8')
        return encode_uint(3, len(encoded)) + encoded
    else:
        raise ValueError(f"unsupported type {type(val)}")

while True:
    cbor_data = read_msg()
    msg = decode_cbor_map(cbor_data)
    if msg[1] == 2:  # EVENT — no reply expected
        continue
    request_id = msg[2]
    payload_data = msg[5]
    reply = encode_cbor_map([
        (1, 0x81),
        (2, request_id),
        (3, payload_data),
    ])
    write_msg(reply)
"#;
