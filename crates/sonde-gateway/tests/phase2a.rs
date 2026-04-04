// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Phase 2A validation tests for sonde-gateway foundation modules.
//!
//! Covers: T-0100 (transport), T-0700/T-0702/T-0703 (node registry),
//! T-0604–T-0607/T-1004 (session manager), T-0400–T-0407 (program library),
//! and crypto unit tests.

use std::time::Duration;

use sonde_gateway::{
    InMemoryStorage, MockTransport, NodeRecord, ProgramLibrary, RustCryptoSha256, SessionManager,
    SessionState, Storage, Transport, VerificationProfile,
};
use sonde_protocol::Sha256Provider;

// ── Transport (T-0100) ──────────────────────────────────────────────

/// T-0100: No unsolicited transmission.
/// Start with an empty inbound queue, verify no outbound frames appear.
#[tokio::test]
async fn t0100_no_unsolicited_transmission() {
    let transport = MockTransport::new();

    // No inbound frames queued — recv should fail immediately.
    let recv_result = transport.recv().await;
    assert!(recv_result.is_err(), "recv must fail on empty queue");

    // Gateway should never transmit without an inbound trigger.
    assert_eq!(
        transport.outbound_count().await,
        0,
        "no outbound frames expected when inbound queue is empty"
    );
}

// ── Node Registry (T-0700, T-0702, T-0703) ─────────────────────────

/// T-0700: Node registry persistence — store and retrieve via InMemoryStorage.
#[tokio::test]
async fn t0700_node_registry_persistence() {
    let storage = InMemoryStorage::new();
    let psk = [0xABu8; 32];

    let node = NodeRecord::new("node-alpha".into(), 42, psk);
    storage.upsert_node(&node).await.unwrap();

    // Retrieve by node_id.
    let fetched = storage.get_node("node-alpha").await.unwrap();
    assert!(fetched.is_some(), "node must be retrievable after upsert");
    let fetched = fetched.unwrap();
    assert_eq!(fetched.node_id, "node-alpha");
    assert_eq!(fetched.key_hint, 42);
    assert_eq!(fetched.psk, psk);

    // Retrieve by key_hint.
    let by_hint = storage.get_nodes_by_key_hint(42).await.unwrap();
    assert_eq!(by_hint.len(), 1);
    assert_eq!(by_hint[0].node_id, "node-alpha");

    // List all nodes.
    let all = storage.list_nodes().await.unwrap();
    assert_eq!(all.len(), 1);

    // Delete and verify gone.
    storage.delete_node("node-alpha").await.unwrap();
    let gone = storage.get_node("node-alpha").await.unwrap();
    assert!(gone.is_none(), "node must not exist after deletion");
}

/// T-0702: Battery level tracking — update via WAKE telemetry data.
#[tokio::test]
async fn t0702_battery_level_tracking() {
    let storage = InMemoryStorage::new();
    let mut node = NodeRecord::new("node-bat".into(), 10, [0x01; 32]);

    assert!(
        node.last_battery_mv.is_none(),
        "initial battery must be None"
    );

    // First WAKE: battery at 3300 mV.
    node.update_telemetry(3300, 1);
    storage.upsert_node(&node).await.unwrap();

    let fetched = storage.get_node("node-bat").await.unwrap().unwrap();
    assert_eq!(fetched.last_battery_mv, Some(3300));

    // Second WAKE: battery drops to 2900 mV.
    node.update_telemetry(2900, 1);
    storage.upsert_node(&node).await.unwrap();

    let fetched = storage.get_node("node-bat").await.unwrap().unwrap();
    assert_eq!(fetched.last_battery_mv, Some(2900));
}

/// T-0702b: Battery historical data retention and cap at 100 readings.
/// Validates GW-0702 AC2: historical battery data is available for trend
/// analysis, capped at 100 readings per node.
#[tokio::test]
async fn t0702b_battery_history_retention_and_cap() {
    let storage = InMemoryStorage::new();
    let mut node = NodeRecord::new("node-bat-hist".into(), 11, [0x42; 32]);

    // Initially empty.
    assert!(
        node.battery_history.is_empty(),
        "initial battery history must be empty"
    );

    // Send 105 readings (exceeds the 100-reading cap).
    for i in 0u32..105 {
        node.update_telemetry(3000 + i, 1);
    }
    storage.upsert_node(&node).await.unwrap();

    let fetched = storage.get_node("node-bat-hist").await.unwrap().unwrap();

    // History must be capped at 100 readings.
    assert_eq!(
        fetched.battery_history.len(),
        100,
        "battery history must be capped at 100 readings"
    );

    // Oldest retained reading should be the 6th (index 5) since first 5 were evicted.
    assert_eq!(
        fetched.battery_history[0].battery_mv, 3005,
        "oldest retained reading must be 3005 mV (first 5 evicted)"
    );

    // Most recent reading must be the last one sent.
    assert_eq!(
        fetched.battery_history[99].battery_mv, 3104,
        "most recent reading must be 3104 mV"
    );

    // last_battery_mv must reflect the most recent reading.
    assert_eq!(fetched.last_battery_mv, Some(3104));
}

/// T-0703: Firmware ABI version tracking.
#[tokio::test]
async fn t0703_firmware_abi_version_tracking() {
    let storage = InMemoryStorage::new();
    let mut node = NodeRecord::new("node-abi".into(), 20, [0x02; 32]);

    assert!(
        node.firmware_abi_version.is_none(),
        "initial ABI version must be None"
    );

    node.update_telemetry(3300, 2);
    storage.upsert_node(&node).await.unwrap();

    let fetched = storage.get_node("node-abi").await.unwrap().unwrap();
    assert_eq!(fetched.firmware_abi_version, Some(2));

    // ABI version updated on subsequent WAKE.
    node.update_telemetry(3300, 3);
    storage.upsert_node(&node).await.unwrap();

    let fetched = storage.get_node("node-abi").await.unwrap().unwrap();
    assert_eq!(fetched.firmware_abi_version, Some(3));
}

// ── Session Manager (T-0604–T-0607, T-1004) ────────────────────────

/// T-0604: Replay protection — sequence number enforced.
#[tokio::test]
async fn t0604_replay_protection_seq_enforced() {
    let mgr = SessionManager::new(Duration::from_secs(30));
    let peer = vec![0x01, 0x02, 0x03, 0x04, 0x05, 0x06];
    let starting_seq = 100;

    mgr.create_session("node-1".into(), peer, 0xDEAD, starting_seq)
        .await;

    // First message at starting_seq: accepted.
    let result = mgr.verify_and_advance_seq("node-1", starting_seq).await;
    assert!(result.is_ok(), "first seq must be accepted");

    // Replay same seq: rejected.
    let replay = mgr.verify_and_advance_seq("node-1", starting_seq).await;
    assert!(replay.is_err(), "replayed seq must be rejected");
}

/// T-0605: New WAKE creates a new session, replacing the old one.
#[tokio::test]
async fn t0605_wake_creates_new_session() {
    let mgr = SessionManager::new(Duration::from_secs(30));
    let peer = vec![0xAA; 6];

    // Session 1.
    let s1_seq = 10;
    mgr.create_session("node-2".into(), peer.clone(), 0x1111, s1_seq)
        .await;

    // Session 2 replaces session 1.
    let s2_seq = 50;
    mgr.create_session("node-2".into(), peer.clone(), 0x2222, s2_seq)
        .await;

    // Old session seq rejected.
    let old = mgr.verify_and_advance_seq("node-2", s1_seq).await;
    assert!(
        old.is_err(),
        "old session seq must be rejected after replacement"
    );

    // New session seq accepted.
    let new = mgr.verify_and_advance_seq("node-2", s2_seq).await;
    assert!(new.is_ok(), "new session seq must be accepted");

    // Verify the session nonce is from session 2.
    let session = mgr.get_session("node-2").await.unwrap();
    assert_eq!(session.wake_nonce, 0x2222);
}

/// T-0606: Wrong sequence number rejected.
#[tokio::test]
async fn t0606_wrong_seq_rejected() {
    let mgr = SessionManager::new(Duration::from_secs(30));
    let peer = vec![0xBB; 6];
    let starting_seq = 0;

    mgr.create_session("node-3".into(), peer, 0xAAAA, starting_seq)
        .await;

    // Skip ahead — send seq 5 when 0 is expected.
    let result = mgr.verify_and_advance_seq("node-3", 5).await;
    assert!(result.is_err(), "skipped seq must be rejected");
}

/// T-0607: No active session — post-WAKE message rejected.
#[tokio::test]
async fn t0607_no_active_session_rejected() {
    let mgr = SessionManager::new(Duration::from_secs(30));

    // No session created for "ghost-node".
    let result = mgr.verify_and_advance_seq("ghost-node", 0).await;
    assert!(result.is_err(), "must reject when no session exists");

    let session = mgr.get_session("ghost-node").await;
    assert!(session.is_none(), "no session must exist for unknown node");
}

/// T-0607a: WAKE retry preserves ChunkedTransfer session.
/// GW-0602 AC5: When a WAKE arrives with a matching nonce during an active
/// ChunkedTransfer, the session MUST be reused and the starting_seq preserved.
#[tokio::test]
async fn t0607a_wake_retry_preserves_chunked_transfer_session() {
    let mgr = SessionManager::new(Duration::from_secs(30));
    let peer = vec![0xDD; 6];
    let wake_nonce = 0xCAFE;
    let starting_seq = 42;

    // 1. Create session simulating an initial WAKE.
    mgr.create_session("node-chunk".into(), peer.clone(), wake_nonce, starting_seq)
        .await;

    // 2. Transition to ChunkedTransfer state (gateway sets this after selecting
    //    a chunked command).
    mgr.set_state(
        "node-chunk",
        SessionState::ChunkedTransfer {
            program_hash: vec![0x42; 32],
            program_size: 1024,
            chunk_size: 200,
            chunk_count: 6,
            is_ephemeral: true,
        },
    )
    .await
    .unwrap();

    // 3. Simulate a WAKE retry (same nonce). The caller (engine) should detect
    //    reuse_chunked and NOT call create_session. Verify the session state is
    //    preserved by checking get_session.
    let session = mgr.get_session("node-chunk").await.unwrap();
    assert_eq!(
        session.wake_nonce, wake_nonce,
        "session nonce must still match the original WAKE"
    );
    assert!(
        matches!(session.state, SessionState::ChunkedTransfer { .. }),
        "session must remain in ChunkedTransfer state"
    );
    assert_eq!(
        session.next_expected_seq, starting_seq,
        "next_expected_seq must equal the original starting_seq"
    );

    // 4. Verify sequence tracking still works (simulating GET_CHUNK).
    let result = mgr.verify_and_advance_seq("node-chunk", starting_seq).await;
    assert!(
        result.is_ok(),
        "GET_CHUNK with starting_seq must succeed on reused session"
    );

    // 5. Verify next seq also works (second GET_CHUNK).
    let result = mgr
        .verify_and_advance_seq("node-chunk", starting_seq + 1)
        .await;
    assert!(
        result.is_ok(),
        "second GET_CHUNK must succeed with incremented seq"
    );
}

/// T-1004: Session timeout and cleanup.
#[tokio::test]
async fn t1004_session_timeout_cleanup() {
    // Use a very short timeout for the test.
    let mgr = SessionManager::new(Duration::from_millis(50));
    let peer = vec![0xCC; 6];

    mgr.create_session("node-expire".into(), peer, 0xBEEF, 0)
        .await;
    assert_eq!(mgr.active_count().await, 1);

    // Verify seq works before timeout.
    let result = mgr.verify_and_advance_seq("node-expire", 0).await;
    assert!(result.is_ok(), "seq should work before timeout");

    // Wait for timeout to pass.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Post-timeout message rejected by verify_and_advance_seq (not by manual reap).
    let result = mgr.verify_and_advance_seq("node-expire", 1).await;
    assert!(result.is_err(), "expired session must reject messages");

    // Reap should also clean up.
    let expired = mgr.reap_expired().await;
    assert_eq!(mgr.active_count().await, 0, "no sessions after reap");
    // Session was already removed by verify_and_advance_seq, so reap may find nothing.
    let _ = expired;
}

// ── Program Library (T-0400–T-0407) ─────────────────────────────────

/// T-0400: Valid program ingestion — store CBOR image, verify hash.
#[tokio::test]
async fn t0400_valid_program_ingestion() {
    let lib = ProgramLibrary::new();
    let sha = RustCryptoSha256;

    // Simulate a valid CBOR program image (non-empty bytes).
    let image = vec![0xA2, 0x01, 0x42, 0x00, 0x00, 0x02, 0x80]; // small CBOR-ish blob
    let record = lib
        .ingest_unverified(image.clone(), VerificationProfile::Resident)
        .unwrap();

    assert_eq!(record.size, image.len() as u32);
    assert_eq!(record.image, image);
    assert_eq!(record.verification_profile, VerificationProfile::Resident);

    // Hash must match SHA-256 of the image bytes.
    let expected_hash = sha.hash(&image);
    assert_eq!(record.hash, expected_hash.to_vec());
}

/// T-0401: Invalid program rejection (empty bytes).
#[tokio::test]
async fn t0401_invalid_program_rejection() {
    let lib = ProgramLibrary::new();

    // Empty image should be rejected.
    let result = lib.ingest_unverified(vec![], VerificationProfile::Resident);
    assert!(result.is_err(), "empty image must be rejected");

    // Random bytes — accepted at Phase 2A (no ELF/CBOR validation yet).
    // True ELF validation comes with prevail-rust integration.
    let result = lib.ingest_unverified(vec![0xFF, 0xFE, 0xFD], VerificationProfile::Resident);
    assert!(
        result.is_ok(),
        "non-empty bytes accepted in Phase 2A (no ELF validation yet)"
    );
}

/// T-0405: Content hash identity — same image twice produces same hash.
#[tokio::test]
async fn t0405_content_hash_identity() {
    let lib = ProgramLibrary::new();
    let storage = InMemoryStorage::new();

    let image = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x02, 0x03];

    let r1 = lib
        .ingest_unverified(image.clone(), VerificationProfile::Resident)
        .unwrap();
    let r2 = lib
        .ingest_unverified(image.clone(), VerificationProfile::Resident)
        .unwrap();

    // Same image ⇒ same hash.
    assert_eq!(
        r1.hash, r2.hash,
        "identical images must produce identical hashes"
    );

    // Store both — second insert overwrites first (same key).
    storage.store_program(&r1).await.unwrap();
    storage.store_program(&r2).await.unwrap();

    let all = storage.list_programs().await.unwrap();
    assert_eq!(all.len(), 1, "only one record for identical images");
}

/// T-0406: Hash covers maps — different map definitions produce different hashes.
#[tokio::test]
async fn t0406_hash_covers_maps() {
    let lib = ProgramLibrary::new();

    // Two images with identical prefix but different trailing bytes (simulating
    // different map definitions appended to the same bytecode).
    let image_a = vec![0x01, 0x02, 0x03, 0x04, 0xAA];
    let image_b = vec![0x01, 0x02, 0x03, 0x04, 0xBB];

    let ra = lib
        .ingest_unverified(image_a, VerificationProfile::Resident)
        .unwrap();
    let rb = lib
        .ingest_unverified(image_b, VerificationProfile::Resident)
        .unwrap();

    assert_ne!(
        ra.hash, rb.hash,
        "different map defs must yield different hashes"
    );
}

/// T-0407: Program size enforcement — oversized images rejected.
#[tokio::test]
async fn t0407_program_size_enforcement() {
    let lib = ProgramLibrary::new();

    // Resident limit is 4096 bytes.
    let oversized_resident = vec![0xFF; 4097];
    let res = lib.ingest_unverified(oversized_resident, VerificationProfile::Resident);
    assert!(
        res.is_err(),
        "resident program >4096 bytes must be rejected"
    );

    // Ephemeral limit is 2048 bytes.
    let oversized_ephemeral = vec![0xFF; 2049];
    let res = lib.ingest_unverified(oversized_ephemeral, VerificationProfile::Ephemeral);
    assert!(
        res.is_err(),
        "ephemeral program >2048 bytes must be rejected"
    );

    // At-limit images should be accepted.
    let at_limit_resident = vec![0xAA; 4096];
    let res = lib.ingest_unverified(at_limit_resident, VerificationProfile::Resident);
    assert!(
        res.is_ok(),
        "resident program at exactly 4096 bytes must be accepted"
    );

    let at_limit_ephemeral = vec![0xBB; 2048];
    let res = lib.ingest_unverified(at_limit_ephemeral, VerificationProfile::Ephemeral);
    assert!(
        res.is_ok(),
        "ephemeral program at exactly 2048 bytes must be accepted"
    );
}

// ── Crypto unit tests ──────────────────────────────────────────────

/// SHA-256 hash is deterministic.
#[tokio::test]
async fn crypto_sha256_deterministic() {
    let sha = RustCryptoSha256;
    let data = b"deterministic input";

    let h1 = sha.hash(data);
    let h2 = sha.hash(data);

    assert_eq!(
        h1, h2,
        "SHA-256 must produce identical output for identical input"
    );
    assert_eq!(h1.len(), 32);

    // Different input produces different hash.
    let h3 = sha.hash(b"different input");
    assert_ne!(h1, h3, "different inputs must produce different hashes");
}
