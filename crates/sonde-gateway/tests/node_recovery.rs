// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Integration tests for the declarative node recovery engine (GW-2009).
//!
//! Test IDs: T-2006, T-2006a, T-2006b.
//!
//! T-2010 (pending recovery purge on rotation) is in `rotation.rs`.

use std::sync::Arc;
use std::time::Duration;

use zeroize::Zeroizing;

use sonde_gateway::connector::{ConnectorEventHub, GatewayDesiredState, RecoveredPskRecord};
use sonde_gateway::crypto::RustCryptoSha256;
use sonde_gateway::engine::{Gateway, MissingKeyHintTracker};
use sonde_gateway::gateway_identity::GatewayIdentity;
use sonde_gateway::rotation_engine::RotationEngine;
use sonde_gateway::sqlite_storage::{encrypt_psk, SqliteStorage};
use sonde_gateway::storage::Storage;
use sonde_gateway::transport::PeerAddress;
use sonde_gateway::GatewayAead;

use sonde_protocol::{
    decode_frame, encode_frame, open_frame, FrameHeader, GatewayMessage, NodeMessage, MSG_WAKE,
};

// ── Helpers ─────────────────────────────────────────────────────────

const TEST_MASTER_KEY: [u8; 32] = [0x42u8; 32];
const TEST_PSK: [u8; 32] = [0xBBu8; 32];
const TEST_KEY_HINT: u16 = 0x1234;

async fn make_gateway_with_sqlite(store: Arc<SqliteStorage>) -> Gateway {
    let mut gw = Gateway::new(store.clone() as Arc<dyn Storage>, Duration::from_secs(30));
    gw.set_sqlite_storage(store).await;
    gw
}

fn build_wake_frame(key_hint: u16, psk: &[u8; 32], nonce: u64) -> Vec<u8> {
    let header = FrameHeader {
        key_hint,
        msg_type: MSG_WAKE,
        nonce,
    };
    let msg = NodeMessage::Wake {
        firmware_abi_version: 1,
        program_hash: vec![0u8; 32],
        battery_mv: 3300,
        firmware_version: "0.7.0".into(),
        blob: None,
    };
    let cbor = msg.encode().unwrap();
    encode_frame(&header, &cbor, psk, &GatewayAead, &RustCryptoSha256).unwrap()
}

fn peer_addr() -> PeerAddress {
    b"test-node".to_vec()
}

// ── T-2006: Declarative node recovery ───────────────────────────────

/// T-2006: Full recovery cycle — unknown key_hint reported, rate-limited,
/// recovered PSK ingested, trial authentication succeeds, node promoted.
#[tokio::test]
async fn test_t2006_declarative_node_recovery() {
    let master_key = Zeroizing::new(TEST_MASTER_KEY);
    let store = Arc::new(SqliteStorage::in_memory(master_key.clone()).unwrap());

    // Initialize master_key_id so recovered_psks validation works.
    let (current_key_id, _current_epoch) = store.init_master_key_id().await.unwrap();

    let gw = make_gateway_with_sqlite(store.clone()).await;

    // Step 2: Send a valid encrypted WAKE frame with unknown key_hint.
    let frame = build_wake_frame(TEST_KEY_HINT, &TEST_PSK, 42);
    let resp = gw.process_frame(&frame, peer_addr()).await;
    assert!(
        resp.is_none(),
        "frame with unknown key_hint should be discarded"
    );

    // Step 3: Verify missing_key_hints includes the key_hint.
    let hints = gw.drain_missing_hints().await;
    assert!(
        hints.contains(&TEST_KEY_HINT),
        "missing_key_hints should contain the unknown key_hint"
    );

    // Step 4: Subsequent drain should be empty (cleared after reporting).
    let hints2 = gw.drain_missing_hints().await;
    assert!(
        hints2.is_empty(),
        "missing_key_hints should be cleared after drain"
    );

    // Step 5: Same key_hint within 60s should NOT be re-reported (rate limit).
    let resp2 = gw.process_frame(&frame, peer_addr()).await;
    assert!(resp2.is_none());
    let hints3 = gw.drain_missing_hints().await;
    assert!(
        hints3.is_empty(),
        "same key_hint should be rate-limited within 60s"
    );

    // Step 6: Deliver recovered_psks via DESIRED_STATE.
    // Encrypt the PSK with the master key for the pending_recovery table.
    let encrypted_psk = encrypt_psk(&master_key, "recovered-node", &TEST_PSK).unwrap();

    // Process recovered_psks through the rotation engine.
    let (_, grpc_rx) = tokio::sync::mpsc::unbounded_channel();
    let (ds_tx, ds_rx) = tokio::sync::mpsc::unbounded_channel();
    let event_hub = Arc::new(ConnectorEventHub::default());

    // Load identity for rotation engine.
    let identity = store
        .load_gateway_identity()
        .await
        .unwrap()
        .unwrap_or_else(|| {
            // Generate one if none exists.
            let id = GatewayIdentity::generate().unwrap();
            // We won't persist it since we only need it for the engine constructor.
            id
        });

    let entity_id: String = identity
        .gateway_id()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();

    let engine = RotationEngine::new(store.clone(), identity, event_hub, grpc_rx, ds_rx);

    // Send the DESIRED_STATE with recovered_psks through the channel.
    let desired_state = GatewayDesiredState {
        entity_id,
        channel: None,
        salt: None,
        kdf_params: None,
        rotation_payload: None,
        recovered_psks: Some(vec![RecoveredPskRecord {
            node_id: "recovered-node".to_string(),
            key_hint: TEST_KEY_HINT,
            encrypted_psk: encrypted_psk.clone(),
            master_key_id: current_key_id.to_vec(),
        }]),
    };
    ds_tx.send(desired_state).unwrap();

    // Run the engine for one tick to process the DESIRED_STATE.
    // We need to drop ds_tx so the engine loop terminates after processing.
    drop(ds_tx);
    engine.run().await;

    // Step 7: Verify PSK stored in pending_recovery table.
    let pending = store.lookup_pending_recovery(TEST_KEY_HINT).await.unwrap();
    assert_eq!(pending.len(), 1, "should have 1 pending_recovery record");
    assert_eq!(pending[0].node_id, "recovered-node");
    assert_eq!(pending[0].key_hint, TEST_KEY_HINT);

    // Step 8: Send another frame with same key_hint — verify frame is processed
    // using the recovered PSK.
    // Build a new gateway (the MissingKeyHintTracker rate limit would block
    // reporting on the old instance, but we need to test trial auth, not
    // rate limiting).
    let gw2 = make_gateway_with_sqlite(store.clone()).await;
    let frame2 = build_wake_frame(TEST_KEY_HINT, &TEST_PSK, 43);
    let resp3 = gw2.process_frame(&frame2, peer_addr()).await;
    assert!(
        resp3.is_some(),
        "trial authentication should succeed and produce a COMMAND response"
    );

    // Verify the response is a valid AEAD frame decodable with the node's PSK.
    let resp3_bytes = resp3.unwrap();
    let decoded = decode_frame(&resp3_bytes).expect("response must decode");
    let pt = open_frame(&decoded, &TEST_PSK, &GatewayAead, &RustCryptoSha256)
        .expect("open must succeed with correct PSK");
    let msg = GatewayMessage::decode(decoded.header.msg_type, &pt);
    assert!(msg.is_ok(), "response payload must be valid CBOR");

    // Step 9: Verify node promoted from pending_recovery to nodes table.
    let promoted = store.get_nodes_by_key_hint(TEST_KEY_HINT).await.unwrap();
    assert_eq!(promoted.len(), 1, "node should be promoted to nodes table");
    assert_eq!(promoted[0].node_id, "recovered-node");

    // Verify pending_recovery is empty after promotion.
    let pending_after = store.lookup_pending_recovery(TEST_KEY_HINT).await.unwrap();
    assert!(
        pending_after.is_empty(),
        "pending_recovery should be empty after promotion"
    );
}

// ── T-2006a: Provisional recovery — wrong PSK ──────────────────────

/// T-2006a: A bogus PSK in pending_recovery fails trial decryption and
/// remains in the table (not promoted to nodes).
#[tokio::test]
async fn test_t2006a_wrong_psk_not_promoted() {
    let master_key = Zeroizing::new(TEST_MASTER_KEY);
    let store = Arc::new(SqliteStorage::in_memory(master_key.clone()).unwrap());
    let (current_key_id, current_epoch) = store.init_master_key_id().await.unwrap();

    // Step 1: Insert a bogus PSK into pending_recovery.
    let bogus_psk = [0xDDu8; 32];
    let encrypted_bogus = encrypt_psk(&master_key, "bogus-node", &bogus_psk).unwrap();
    store
        .insert_pending_recovery(
            TEST_KEY_HINT,
            "bogus-node",
            &encrypted_bogus,
            &current_key_id,
            current_epoch,
        )
        .await
        .unwrap();

    let gw = make_gateway_with_sqlite(store.clone()).await;

    // Step 2: Send a frame with matching key_hint but different PSK.
    let real_psk = [0xEEu8; 32];
    let frame = build_wake_frame(TEST_KEY_HINT, &real_psk, 50);

    // Step 3: Verify trial-decryption fails (no response).
    let resp = gw.process_frame(&frame, peer_addr()).await;
    assert!(
        resp.is_none(),
        "trial decryption with wrong PSK should fail"
    );

    // Step 4: Verify bogus record remains in pending_recovery (not promoted).
    let pending = store.lookup_pending_recovery(TEST_KEY_HINT).await.unwrap();
    assert_eq!(
        pending.len(),
        1,
        "bogus record should remain in pending_recovery"
    );
    assert_eq!(pending[0].node_id, "bogus-node");

    // Verify nodes table is empty.
    let nodes = store.get_nodes_by_key_hint(TEST_KEY_HINT).await.unwrap();
    assert!(
        nodes.is_empty(),
        "bogus PSK should not be promoted to nodes"
    );

    // Step 5: Verify the bogus record is purged after 24 hours.
    // We can't wait 24h in a unit test. The expire_pending_recovery function
    // uses `WHERE received_at < now - max_age_secs`. Since the record was
    // just inserted (received_at ≈ now), calling with max_age_secs=0 is a
    // boundary race. Instead, verify the existing sqlite_storage expiry tests
    // cover this path, and verify the record is still present.
    //
    // The sqlite_storage::test_pending_recovery_expire test manipulates
    // received_at directly to verify expiry semantics.
    let pending_after = store.lookup_pending_recovery(TEST_KEY_HINT).await.unwrap();
    assert_eq!(
        pending_after.len(),
        1,
        "bogus record should still be in pending_recovery (not expired yet)"
    );
}

// ── T-2006b: Provisional recovery — mismatched master_key_id ────────

/// T-2006b: Recovered PSKs with a master_key_id that doesn't match the
/// gateway's current key are skipped (not inserted into pending_recovery).
#[tokio::test]
async fn test_t2006b_mismatched_master_key_id_skipped() {
    let master_key = Zeroizing::new(TEST_MASTER_KEY);
    let store = Arc::new(SqliteStorage::in_memory(master_key.clone()).unwrap());
    let (_current_key_id, _current_epoch) = store.init_master_key_id().await.unwrap();

    let identity = store
        .load_gateway_identity()
        .await
        .unwrap()
        .unwrap_or_else(|| GatewayIdentity::generate().unwrap());

    let (_, grpc_rx) = tokio::sync::mpsc::unbounded_channel();
    let (ds_tx, ds_rx) = tokio::sync::mpsc::unbounded_channel();
    let event_hub = Arc::new(ConnectorEventHub::default());

    let engine = RotationEngine::new(store.clone(), identity, event_hub, grpc_rx, ds_rx);

    // Step 1: Deliver recovered_psks with wrong master_key_id.
    let wrong_key_id = [0xFFu8; 16];
    let encrypted_psk = encrypt_psk(&master_key, "orphan-node", &TEST_PSK).unwrap();

    let desired_state = GatewayDesiredState {
        entity_id: "gateway-1".to_string(),
        channel: None,
        salt: None,
        kdf_params: None,
        rotation_payload: None,
        recovered_psks: Some(vec![RecoveredPskRecord {
            node_id: "orphan-node".to_string(),
            key_hint: TEST_KEY_HINT,
            encrypted_psk,
            master_key_id: wrong_key_id.to_vec(),
        }]),
    };
    ds_tx.send(desired_state).unwrap();
    drop(ds_tx);
    engine.run().await;

    // Step 2: Verify the record is skipped (not inserted into pending_recovery).
    let pending = store.lookup_pending_recovery(TEST_KEY_HINT).await.unwrap();
    assert!(
        pending.is_empty(),
        "recovered_psk with wrong master_key_id should be skipped"
    );
}

// ── MissingKeyHintTracker unit tests ────────────────────────────────

#[test]
fn missing_hint_tracker_basic() {
    let mut tracker = MissingKeyHintTracker::new();

    // First report should be accepted.
    assert!(tracker.report(100));
    assert_eq!(tracker.len(), 1);

    // Same key_hint immediately should be rate-limited.
    assert!(!tracker.report(100));

    // Different key_hint should be accepted.
    assert!(tracker.report(200));
    assert_eq!(tracker.len(), 2);

    // Drain should return both.
    let drained = tracker.drain();
    assert_eq!(drained.len(), 2);
    assert!(drained.contains(&100));
    assert!(drained.contains(&200));

    // Second drain should be empty.
    assert!(tracker.drain().is_empty());
}

#[test]
fn missing_hint_tracker_lru_eviction() {
    let mut tracker = MissingKeyHintTracker::new();

    // Fill to capacity.
    for i in 0..256u16 {
        assert!(tracker.report(i));
    }
    assert_eq!(tracker.len(), 256);

    // Adding one more should evict the oldest (0).
    assert!(tracker.report(999));
    assert_eq!(tracker.len(), 256);

    // The evicted key_hint (0) should now be accepted again.
    assert!(tracker.report(0));
}

/// Rate-limited hints should refresh their LRU position so they are not
/// evicted while still actively appearing.
#[test]
fn missing_hint_tracker_rate_limited_refreshes_lru() {
    let mut tracker = MissingKeyHintTracker::new();

    // Insert hint 0, then fill 255 more slots (1..=255).
    assert!(tracker.report(0));
    for i in 1..256u16 {
        assert!(tracker.report(i));
    }
    assert_eq!(tracker.len(), 256);

    // Hint 0 is rate-limited (within 60s), but should refresh LRU position.
    assert!(!tracker.report(0), "should be rate-limited");

    // Now insert a new hint — it should evict hint 1 (the oldest that was
    // NOT refreshed), not hint 0.
    assert!(tracker.report(999));
    assert_eq!(tracker.len(), 256);

    // Hint 1 was evicted, so reporting it should succeed as new.
    assert!(tracker.report(1), "hint 1 should have been evicted");

    // Hint 0 is still tracked (not evicted), so it should still be rate-limited.
    assert!(!tracker.report(0), "hint 0 should NOT have been evicted");
}
