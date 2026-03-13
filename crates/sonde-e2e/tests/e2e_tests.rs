// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! End-to-end integration tests exercising the full gateway ↔ node protocol.

use sonde_e2e::harness::{E2eTestEnv, NodeProxy, TestSha256};
use sonde_gateway::engine::PendingCommand;
use sonde_gateway::{ProgramRecord, VerificationProfile};
use sonde_node::traits::PlatformStorage;
use sonde_node::wake_cycle::WakeCycleOutcome;
use sonde_protocol::{ProgramImage, Sha256Provider};

use sonde_gateway::storage::Storage;

/// Create a minimal valid BPF program image (mov r0, 0; exit) with no maps.
fn make_test_program() -> (ProgramRecord, Vec<u8>) {
    let image = ProgramImage {
        bytecode: vec![
            0xb7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r0, 0
            0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // exit
        ],
        maps: vec![],
    };
    let cbor = image.encode_deterministic().unwrap();
    let sha = TestSha256;
    let hash = sha.hash(&cbor).to_vec();
    let size = cbor.len() as u32;
    let record = ProgramRecord {
        hash: hash.clone(),
        image: cbor,
        size,
        verification_profile: VerificationProfile::Resident,
    };
    (record, hash)
}

/// T-E2E-001 — NOP wake cycle.
///
/// A paired node with no pending commands completes a normal WAKE/COMMAND
/// exchange and returns to sleep at its configured interval.
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_001_nop_wake_cycle() {
    let env = E2eTestEnv::new();
    let psk = [0xAA; 32];
    env.register_node("test-node", 1, psk).await;

    let mut node = NodeProxy::new(1, psk);
    let stats = node.run_wake_cycle(&env);

    assert_eq!(stats.outcome, WakeCycleOutcome::Sleep { seconds: 60 });

    // Verify gateway updated node telemetry
    let record = env.storage.get_node("test-node").await.unwrap().unwrap();
    assert_eq!(record.last_battery_mv, Some(3300));
    assert!(record.last_seen.is_some());
    assert_eq!(
        record.firmware_abi_version,
        Some(sonde_node::FIRMWARE_ABI_VERSION)
    );
}

/// T-E2E-002 — HMAC authentication round-trip.
///
/// Validates that the gateway (RustCryptoHmac) and node (TestHmac) produce
/// compatible HMAC-SHA256 tags. A successful NOP cycle proves both sides
/// authenticate each other's frames.
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_002_hmac_round_trip() {
    let env = E2eTestEnv::new();
    let psk = [0x42; 32];
    env.register_node("hmac-node", 0x1234, psk).await;

    let mut node = NodeProxy::new(0x1234, psk);
    let stats = node.run_wake_cycle(&env);

    // Success means: node's WAKE was authenticated by gateway,
    // and gateway's COMMAND was authenticated by node.
    assert_eq!(stats.outcome, WakeCycleOutcome::Sleep { seconds: 60 });
}

/// T-E2E-002b — Consecutive wake cycles (state persistence, nonce uniqueness).
///
/// Runs two wake cycles on the same `NodeProxy`. Verifies that both cycles
/// complete successfully with persistent storage and monotonic RNG state.
/// Explicitly asserts that the WAKE nonces differ across the two cycles.
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_002b_consecutive_wake_cycles() {
    let env = E2eTestEnv::new();
    let psk = [0x55; 32];
    env.register_node("multi-node", 1, psk).await;

    let mut node = NodeProxy::new(1, psk);

    let stats1 = node.run_wake_cycle(&env);
    assert_eq!(stats1.outcome, WakeCycleOutcome::Sleep { seconds: 60 });
    assert!(
        stats1.response_count > 0,
        "first cycle should receive gateway responses"
    );

    let stats2 = node.run_wake_cycle(&env);
    assert_eq!(stats2.outcome, WakeCycleOutcome::Sleep { seconds: 60 });
    assert!(
        stats2.response_count > 0,
        "second cycle should receive gateway responses"
    );

    // Verify nonce uniqueness across cycles.
    assert!(
        !stats1.wake_nonces.is_empty() && !stats2.wake_nonces.is_empty(),
        "both cycles should send at least one WAKE frame"
    );
    assert_ne!(
        stats1.wake_nonces[0], stats2.wake_nonces[0],
        "consecutive wake cycles must use different nonces"
    );

    // Both cycles should update last_seen
    let record = env.storage.get_node("multi-node").await.unwrap().unwrap();
    assert!(record.last_seen.is_some());
}

/// T-E2E-003 — Wrong PSK rejected (silent discard).
///
/// When the node's PSK does not match the gateway's record the gateway
/// silently discards the WAKE frame. The node exhausts its retries and
/// sleeps for its configured schedule interval.
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_003_wrong_psk_rejected() {
    let env = E2eTestEnv::new();
    env.register_node("test-node", 1, [0xAA; 32]).await;

    // Node has a different PSK
    let mut node = NodeProxy::new(1, [0xBB; 32]);
    let stats = node.run_wake_cycle(&env);

    // Should exhaust retries and sleep
    assert_eq!(stats.outcome, WakeCycleOutcome::Sleep { seconds: 60 });

    // Gateway must produce zero response frames (silent discard).
    assert_eq!(
        stats.response_count, 0,
        "gateway should send zero responses on HMAC failure"
    );

    // Verify the gateway did NOT update telemetry for this node
    // (the WAKE was silently discarded due to HMAC failure).
    let record = env.storage.get_node("test-node").await.unwrap().unwrap();
    assert!(
        record.last_seen.is_none(),
        "last_seen should be None — gateway should not have processed the WAKE"
    );
    assert_eq!(
        record.last_battery_mv, None,
        "battery should not be updated on auth failure"
    );
}

/// T-E2E-020 — UPDATE_SCHEDULE command.
///
/// The gateway queues an UpdateSchedule command. After the wake cycle the
/// node adopts the new interval.
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_020_update_schedule() {
    let env = E2eTestEnv::new();
    let psk = [0xCC; 32];
    env.register_node("sched-node", 1, psk).await;

    // Queue schedule update
    env.gateway
        .queue_command(
            "sched-node",
            PendingCommand::UpdateSchedule { interval_s: 120 },
        )
        .await;

    let mut node = NodeProxy::new(1, psk);
    let stats = node.run_wake_cycle(&env);

    assert_eq!(stats.outcome, WakeCycleOutcome::Sleep { seconds: 120 });

    // Node persisted the new interval
    assert_eq!(node.storage.read_schedule().0, 120);

    // Pending command consumed
    let cmds = env.pending_commands.read().await;
    assert!(cmds.get("sched-node").is_none_or(|v| v.is_empty()));
}

/// T-E2E-021 — REBOOT command.
///
/// The gateway queues a Reboot command. The wake cycle returns `Reboot`.
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_021_reboot() {
    let env = E2eTestEnv::new();
    let psk = [0xDD; 32];
    env.register_node("reboot-node", 1, psk).await;

    env.gateway
        .queue_command("reboot-node", PendingCommand::Reboot)
        .await;

    let mut node = NodeProxy::new(1, psk);
    let stats = node.run_wake_cycle(&env);

    assert_eq!(stats.outcome, WakeCycleOutcome::Reboot);
}

/// T-E2E-040 — Unknown node (silent discard).
///
/// A node whose key_hint is not registered on the gateway gets no
/// response. The node exhausts retries and sleeps at the default interval.
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_040_unknown_node() {
    let env = E2eTestEnv::new();
    // Do NOT register node

    let mut node = NodeProxy::new(99, [0xFF; 32]);
    let stats = node.run_wake_cycle(&env);

    assert_eq!(stats.outcome, WakeCycleOutcome::Sleep { seconds: 60 });

    // Gateway must produce zero response frames for an unknown node (silent discard).
    assert_eq!(
        stats.response_count, 0,
        "gateway should send zero responses for unknown key_hint"
    );

    // Verify the gateway did not create any node record for the unknown key_hint.
    let nodes = env.storage.get_nodes_by_key_hint(99).await.unwrap();
    assert!(
        nodes.is_empty(),
        "no node record should exist for unknown key_hint 99"
    );
}

// ===========================================================================
// Program distribution tests
// ===========================================================================

/// T-E2E-010 — Full program update cycle.
///
/// A node with no program receives UPDATE_PROGRAM from the gateway,
/// downloads the image via chunked transfer, verifies its hash, persists
/// it, and sends PROGRAM_ACK. The gateway confirms the update.
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_010_full_program_update() {
    let env = E2eTestEnv::new();
    let psk = [0x10; 32];
    env.register_node("prog-node", 1, psk).await;

    // Create and store a minimal test program in the gateway.
    let (program, hash) = make_test_program();
    env.storage.store_program(&program).await.unwrap();

    // Assign the program to the node (current_program_hash remains None).
    let mut node_rec = env.storage.get_node("prog-node").await.unwrap().unwrap();
    node_rec.assigned_program_hash = Some(hash.clone());
    env.storage.upsert_node(&node_rec).await.unwrap();

    let mut node = NodeProxy::new(1, psk);
    let stats = node.run_wake_cycle(&env);

    assert_eq!(stats.outcome, WakeCycleOutcome::Sleep { seconds: 60 });

    // Node should have sent: WAKE, GET_CHUNK(s), PROGRAM_ACK.
    let get_chunk_count = stats
        .sent_frames
        .iter()
        .filter(|(t, _)| *t == sonde_protocol::MSG_GET_CHUNK)
        .count();
    assert!(
        get_chunk_count > 0,
        "node should have sent GET_CHUNK frames"
    );

    let ack_count = stats
        .sent_frames
        .iter()
        .filter(|(t, _)| *t == sonde_protocol::MSG_PROGRAM_ACK)
        .count();
    assert_eq!(
        ack_count, 1,
        "node should have sent exactly one PROGRAM_ACK"
    );

    // Node persisted the program to a storage partition.
    let (_, active_partition) = node.storage.read_schedule();
    let stored = node.storage.read_program(active_partition);
    assert!(stored.is_some(), "program should be persisted in storage");

    // Gateway should have updated current_program_hash after PROGRAM_ACK.
    let updated = env.storage.get_node("prog-node").await.unwrap().unwrap();
    assert_eq!(
        updated.current_program_hash,
        Some(hash),
        "gateway should confirm the program via PROGRAM_ACK"
    );
}

/// T-E2E-011 — Program already current → NOP.
///
/// When the node already has the assigned program (reports matching hash
/// in WAKE), the gateway responds with NOP (no chunked transfer occurs).
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_011_program_already_current() {
    let env = E2eTestEnv::new();
    let psk = [0x11; 32];
    env.register_node("current-node", 1, psk).await;

    // Create and store a program, then mark it as assigned on the gateway.
    let (program, hash) = make_test_program();
    env.storage.store_program(&program).await.unwrap();

    let mut node_rec = env.storage.get_node("current-node").await.unwrap().unwrap();
    node_rec.assigned_program_hash = Some(hash.clone());
    node_rec.current_program_hash = Some(hash);
    env.storage.upsert_node(&node_rec).await.unwrap();

    // Pre-load the program into the node's storage so it reports the
    // matching hash in its WAKE message.
    let mut node = NodeProxy::new(1, psk);
    node.storage
        .write_program(0, &program.image)
        .expect("write program to node storage");

    let stats = node.run_wake_cycle(&env);

    assert_eq!(stats.outcome, WakeCycleOutcome::Sleep { seconds: 60 });

    // No chunked transfer should have occurred.
    let get_chunk_count = stats
        .sent_frames
        .iter()
        .filter(|(t, _)| *t == sonde_protocol::MSG_GET_CHUNK)
        .count();
    assert_eq!(
        get_chunk_count, 0,
        "no GET_CHUNK frames when program is already current"
    );

    // Only WAKE should have been sent (no PROGRAM_ACK either).
    let ack_count = stats
        .sent_frames
        .iter()
        .filter(|(t, _)| *t == sonde_protocol::MSG_PROGRAM_ACK)
        .count();
    assert_eq!(
        ack_count, 0,
        "no PROGRAM_ACK when program is already current"
    );
}

/// T-E2E-022 — RUN_EPHEMERAL command.
///
/// The gateway queues a RunEphemeral command. The node downloads the
/// ephemeral program via chunked transfer, executes it, but does NOT
/// persist it to storage partitions.
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_022_run_ephemeral() {
    let env = E2eTestEnv::new();
    let psk = [0x22; 32];
    env.register_node("ephemeral-node", 1, psk).await;

    // Create an ephemeral program (no maps required).
    let image = ProgramImage {
        bytecode: vec![
            0xb7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r0, 0
            0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // exit
        ],
        maps: vec![],
    };
    let cbor = image.encode_deterministic().unwrap();
    let sha = TestSha256;
    let hash = sha.hash(&cbor).to_vec();
    let size = cbor.len() as u32;
    let record = ProgramRecord {
        hash: hash.clone(),
        image: cbor,
        size,
        verification_profile: VerificationProfile::Ephemeral,
    };
    env.storage.store_program(&record).await.unwrap();

    // Queue the ephemeral run command.
    env.gateway
        .queue_command(
            "ephemeral-node",
            PendingCommand::RunEphemeral { program_hash: hash },
        )
        .await;

    let mut node = NodeProxy::new(1, psk);
    let stats = node.run_wake_cycle(&env);

    assert_eq!(stats.outcome, WakeCycleOutcome::Sleep { seconds: 60 });

    // Chunked transfer should have occurred.
    let get_chunk_count = stats
        .sent_frames
        .iter()
        .filter(|(t, _)| *t == sonde_protocol::MSG_GET_CHUNK)
        .count();
    assert!(
        get_chunk_count > 0,
        "ephemeral program should be downloaded via chunked transfer"
    );

    // Ephemeral program must NOT be persisted to storage partitions.
    assert!(
        node.storage.read_program(0).is_none(),
        "ephemeral program should not be in partition 0"
    );
    assert!(
        node.storage.read_program(1).is_none(),
        "ephemeral program should not be in partition 1"
    );
}

/// T-E2E-041 — Sequence number enforcement during chunked transfer.
///
/// During a program update, verifies that GET_CHUNK frames use monotonically
/// increasing sequence numbers (nonces) assigned by the gateway's COMMAND
/// `starting_seq`.
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_041_sequence_numbers() {
    let env = E2eTestEnv::new();
    let psk = [0x41; 32];
    env.register_node("seq-node", 1, psk).await;

    let (program, hash) = make_test_program();
    env.storage.store_program(&program).await.unwrap();

    let mut node_rec = env.storage.get_node("seq-node").await.unwrap().unwrap();
    node_rec.assigned_program_hash = Some(hash);
    env.storage.upsert_node(&node_rec).await.unwrap();

    let mut node = NodeProxy::new(1, psk);
    let stats = node.run_wake_cycle(&env);

    assert_eq!(stats.outcome, WakeCycleOutcome::Sleep { seconds: 60 });

    // Extract GET_CHUNK nonces (sequence numbers).
    let get_chunk_seqs: Vec<u64> = stats
        .sent_frames
        .iter()
        .filter(|(t, _)| *t == sonde_protocol::MSG_GET_CHUNK)
        .map(|(_, nonce)| *nonce)
        .collect();

    assert!(
        !get_chunk_seqs.is_empty(),
        "should have at least one GET_CHUNK"
    );

    // Sequence numbers must be monotonically increasing.
    for window in get_chunk_seqs.windows(2) {
        assert!(
            window[1] > window[0],
            "GET_CHUNK sequence numbers must be monotonically increasing: {} -> {}",
            window[0],
            window[1]
        );
    }

    // PROGRAM_ACK should also use a sequence number after the last GET_CHUNK.
    let ack_nonces: Vec<u64> = stats
        .sent_frames
        .iter()
        .filter(|(t, _)| *t == sonde_protocol::MSG_PROGRAM_ACK)
        .map(|(_, nonce)| *nonce)
        .collect();
    assert_eq!(ack_nonces.len(), 1);
    assert!(
        ack_nonces[0] > *get_chunk_seqs.last().unwrap(),
        "PROGRAM_ACK seq should follow the last GET_CHUNK seq"
    );
}
