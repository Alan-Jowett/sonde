// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! AES-256-GCM end-to-end integration tests.
//!
//! These tests exercise the AEAD frame path: the node uses
//! `run_wake_cycle` (with `NodeAead`) and the gateway processes
//! frames via `process_frame` (with `GatewayAead`).

use sonde_e2e::harness::{E2eTestEnv, NodeProxy, TestSha256};
use sonde_gateway::storage::Storage;
use sonde_gateway::{ProgramRecord, VerificationProfile};
use sonde_node::wake_cycle::WakeCycleOutcome;
use sonde_protocol::{ProgramImage, Sha256Provider};

// ---------------------------------------------------------------------------
// Helper: program construction (mirrors e2e_tests.rs helpers)
// ---------------------------------------------------------------------------

/// Create a BPF program that calls `send()` (helper 8) with a 2-byte
/// blob `[0xAA, 0xBB]` on the stack — fire-and-forget APP_DATA.
fn make_send_program() -> (ProgramRecord, Vec<u8>) {
    let bytecode = [
        // sth [r10-8], 0xBBAA  — store 2-byte immediate on stack
        0x6a, 0x0a, 0xf8, 0xff, 0xAA, 0xBB, 0x00, 0x00,
        // mov r1, r10          — r1 = frame pointer
        0xbf, 0xa1, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        // add r1, -8           — r1 = fp - 8 (pointer to data)
        0x07, 0x01, 0x00, 0x00, 0xf8, 0xff, 0xff, 0xff,
        // mov r2, 2            — r2 = blob length
        0xb7, 0x02, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00,
        // call 8               — helper_send(r1=ptr, r2=len)
        0x85, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00,
        // mov r0, 0            — return 0
        0xb7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // exit
        0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    make_program_from_bytecode(&bytecode)
}

fn make_program_from_bytecode(bytecode: &[u8]) -> (ProgramRecord, Vec<u8>) {
    let image = ProgramImage {
        bytecode: bytecode.to_vec(),
        maps: vec![],
        map_initial_data: vec![],
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
        abi_version: None,
        source_filename: None,
    };
    (record, hash)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// T-E2E-001 — AEAD NOP wake cycle.
///
/// A paired node with no pending commands completes a normal WAKE/COMMAND
/// exchange over AES-256-GCM and returns to sleep at its configured
/// interval.
///
/// Validates that:
/// - The node's `NodeAead` and the gateway's `GatewayAead` produce
///   compatible AES-256-GCM frames.
/// - Gateway updates node telemetry after a successful AEAD exchange.
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_001_nop_wake_cycle() {
    let env = E2eTestEnv::new();
    let psk = [0x50; 32];
    env.register_node("aead-nop", 1, psk).await;

    let mut node = NodeProxy::new(1, psk);
    let stats = node.run_wake_cycle(&env);

    assert_eq!(stats.outcome, WakeCycleOutcome::Sleep { seconds: 60 });

    // Gateway should have produced a COMMAND response.
    assert!(
        stats.response_count > 0,
        "AEAD wake cycle must receive at least one response (COMMAND)"
    );

    // Verify gateway updated node telemetry.
    let record = env.storage.get_node("aead-nop").await.unwrap().unwrap();
    assert_eq!(record.last_battery_mv, Some(3300));
    assert!(env
        .gateway
        .session_manager()
        .get_last_seen("aead-nop")
        .await
        .is_some());
    assert_eq!(
        record.firmware_abi_version,
        Some(sonde_node::FIRMWARE_ABI_VERSION)
    );
}

/// T-E2E-002b — Consecutive AEAD wake cycles (state persistence).
///
/// Runs two AEAD wake cycles on the same `NodeProxy` to verify that
/// persistent storage and monotonic RNG state work correctly across
/// multiple AES-256-GCM exchanges.
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_002b_consecutive_wake_cycles() {
    let env = E2eTestEnv::new();
    let psk = [0x55; 32];
    env.register_node("aead-multi", 1, psk).await;

    let mut node = NodeProxy::new(1, psk);

    let stats1 = node.run_wake_cycle(&env);
    assert_eq!(stats1.outcome, WakeCycleOutcome::Sleep { seconds: 60 });
    assert!(stats1.response_count > 0);

    let stats2 = node.run_wake_cycle(&env);
    assert_eq!(stats2.outcome, WakeCycleOutcome::Sleep { seconds: 60 });
    assert!(stats2.response_count > 0);

    // Verify nonce uniqueness across cycles.
    assert!(
        !stats1.wake_nonces.is_empty() && !stats2.wake_nonces.is_empty(),
        "both cycles should send at least one WAKE frame"
    );
    assert_ne!(
        stats1.wake_nonces[0], stats2.wake_nonces[0],
        "consecutive AEAD wake cycles must use different nonces"
    );
}

/// T-E2E-002c — AEAD wake cycle with BPF APP_DATA.
///
/// Exercises the full AEAD wake cycle with a BPF program that calls the
/// `send()` helper. Both the WAKE/COMMAND exchange and the resulting
/// `APP_DATA` dispatch use the AES-256-GCM AEAD frame path.
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_002c_app_data_fire_and_forget() {
    use sonde_node::sonde_bpf_adapter::SondeBpfInterpreter;

    let env = E2eTestEnv::new();
    let psk = [0x51; 32];
    env.register_node("aead-appdata", 1, psk).await;

    let (program, hash) = make_send_program();
    env.storage.store_program(&program).await.unwrap();

    let mut node_rec = env.storage.get_node("aead-appdata").await.unwrap().unwrap();
    node_rec.assigned_program_hash = Some(hash);
    env.storage.upsert_node(&node_rec).await.unwrap();

    let mut node = NodeProxy::new(1, psk);
    let mut interpreter = SondeBpfInterpreter::new();
    let stats = node.run_wake_cycle_with(&env, &mut interpreter);

    assert_eq!(stats.outcome, WakeCycleOutcome::Sleep { seconds: 60 });

    let app_data_count = stats
        .sent_frames
        .iter()
        .filter(|(t, _)| *t == sonde_protocol::MSG_APP_DATA)
        .count();
    assert_eq!(
        app_data_count, 1,
        "node should send exactly one APP_DATA frame"
    );
}

/// T-E2E-003 — AEAD wrong PSK (silent discard).
///
/// When the node's PSK does not match the gateway's record, the gateway
/// cannot decrypt the AEAD frame and silently discards it. The node
/// exhausts its retries and sleeps.
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_003_wrong_psk_rejected() {
    let env = E2eTestEnv::new();
    env.register_node("aead-wrong", 1, [0xAA; 32]).await;

    // Node has a different PSK.
    let mut node = NodeProxy::new(1, [0xBB; 32]);
    let stats = node.run_wake_cycle(&env);

    assert_eq!(stats.outcome, WakeCycleOutcome::Sleep { seconds: 60 });
    assert_eq!(
        stats.response_count, 0,
        "gateway should send zero responses on AEAD authentication failure"
    );

    // Gateway must not have updated telemetry.
    let record = env.storage.get_node("aead-wrong").await.unwrap().unwrap();
    assert!(
        env.gateway
            .session_manager()
            .get_last_seen("aead-wrong")
            .await
            .is_none(),
        "runtime `last_seen` should be None — gateway silently discarded the WAKE"
    );
    assert_eq!(
        record.last_battery_mv, None,
        "battery should not be updated on auth failure"
    );
}

/// T-E2E-004 — AEAD tampered frame (silent discard).
///
/// A bit-flip in the ciphertext region causes the GCM tag check to fail.
/// The gateway silently discards the corrupted frame and the node receives
/// no response, eventually exhausting retries.
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_004_tampered_frame_discarded() {
    let env = E2eTestEnv::new();
    let psk = [0x53; 32];
    env.register_node("aead-tamper", 1, psk).await;

    let mut node = NodeProxy::new(1, psk);
    let stats = node.run_wake_cycle_tampered(&env);

    assert_eq!(stats.outcome, WakeCycleOutcome::Sleep { seconds: 60 });
    assert_eq!(
        stats.response_count, 0,
        "gateway should send zero responses on tampered AEAD frame"
    );

    // Gateway must not have updated telemetry.
    let record = env.storage.get_node("aead-tamper").await.unwrap().unwrap();
    assert!(
        env.gateway
            .session_manager()
            .get_last_seen("aead-tamper")
            .await
            .is_none(),
        "runtime `last_seen` should be None — gateway silently discarded the tampered WAKE"
    );
    assert_eq!(
        record.last_battery_mv, None,
        "battery should not be updated on tampered frame"
    );
}

// ---------------------------------------------------------------------------
// Helper: send_recv program (helper 9)
// ---------------------------------------------------------------------------

/// Create a BPF program that calls `send_recv()` (helper 9) with a 2-byte
/// blob `[0xAA, 0xBB]` and a 16-byte reply buffer on the stack.
fn make_send_recv_program() -> (ProgramRecord, Vec<u8>) {
    let bytecode = [
        // sth [r10-8], 0xBBAA  — store 2-byte blob on stack
        0x6a, 0x0a, 0xf8, 0xff, 0xAA, 0xBB, 0x00, 0x00,
        // mov r1, r10          — r1 = frame pointer
        0xbf, 0xa1, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        // add r1, -8           — r1 = fp - 8 (pointer to data)
        0x07, 0x01, 0x00, 0x00, 0xf8, 0xff, 0xff, 0xff,
        // mov r2, 2            — r2 = blob length
        0xb7, 0x02, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00,
        // mov r3, r10          — r3 = frame pointer
        0xbf, 0xa3, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        // add r3, -24          — r3 = fp - 24 (reply buffer)
        0x07, 0x03, 0x00, 0x00, 0xe8, 0xff, 0xff, 0xff,
        // mov r4, 16           — r4 = reply buffer capacity
        0xb7, 0x04, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00,
        // mov r5, 0            — r5 = timeout (0 = default)
        0xb7, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        // call 9               — helper_send_recv(r1..r5)
        0x85, 0x00, 0x00, 0x00, 0x09, 0x00, 0x00, 0x00,
        // mov r0, 0            — return 0
        0xb7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // exit
        0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    make_program_from_bytecode(&bytecode)
}

/// Create a BPF program that calls `send()` (helper 8) with a 2-byte
/// blob `[0xDE, 0xAD]` on the stack — fire-and-forget APP_DATA.
fn make_send_program_dead() -> (ProgramRecord, Vec<u8>) {
    let bytecode = [
        // sth [r10-8], 0xADDE  — store 2-byte immediate on stack
        0x6a, 0x0a, 0xf8, 0xff, 0xDE, 0xAD, 0x00, 0x00,
        // mov r1, r10          — r1 = frame pointer
        0xbf, 0xa1, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        // add r1, -8           — r1 = fp - 8 (pointer to data)
        0x07, 0x01, 0x00, 0x00, 0xf8, 0xff, 0xff, 0xff,
        // mov r2, 2            — r2 = blob length
        0xb7, 0x02, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00,
        // call 8               — helper_send(r1=ptr, r2=len)
        0x85, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00,
        // mov r0, 0            — return 0
        0xb7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // exit
        0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    make_program_from_bytecode(&bytecode)
}

/// Create a BPF program that calls `send()` (helper 8) with a 2-byte
/// blob `[0x01, 0x02]` on the stack — fire-and-forget APP_DATA (per
/// T-E2E-031 spec).
fn make_send_program_0102() -> (ProgramRecord, Vec<u8>) {
    let bytecode = [
        // sth [r10-8], 0x0201  — store 2-byte immediate on stack
        0x6a, 0x0a, 0xf8, 0xff, 0x01, 0x02, 0x00, 0x00,
        // mov r1, r10          — r1 = frame pointer
        0xbf, 0xa1, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        // add r1, -8           — r1 = fp - 8 (pointer to data)
        0x07, 0x01, 0x00, 0x00, 0xf8, 0xff, 0xff, 0xff,
        // mov r2, 2            — r2 = blob length
        0xb7, 0x02, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00,
        // call 8               — helper_send(r1=ptr, r2=len)
        0x85, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00,
        // mov r0, 0            — return 0
        0xb7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // exit
        0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    make_program_from_bytecode(&bytecode)
}

// ---------------------------------------------------------------------------
// Handler routing tests (T-E2E-030 through T-E2E-034)
// ---------------------------------------------------------------------------

/// Helper: register a node with an assigned BPF program in the gateway.
async fn setup_node_with_program(
    env: &E2eTestEnv,
    node_id: &str,
    key_hint: u16,
    psk: [u8; 32],
    program: &ProgramRecord,
    hash: &[u8],
) {
    env.register_node(node_id, key_hint, psk).await;
    env.storage.store_program(program).await.unwrap();
    let mut rec = env.storage.get_node(node_id).await.unwrap().unwrap();
    rec.assigned_program_hash = Some(hash.to_vec());
    env.storage.upsert_node(&rec).await.unwrap();
}

/// T-E2E-030 — APP_DATA round-trip with handler.
///
/// A BPF program calls `send_recv()` (helper 9) with blob `[0xAA, 0xBB]`.
/// The gateway routes the APP_DATA to the stub handler, which replies with
/// `[0xCC, 0xDD]`. The gateway forwards the reply as APP_DATA_REPLY.
///
/// Validates: GW-0500, GW-0501, ND-0602.
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_030_app_data_round_trip_with_handler() {
    use sonde_node::sonde_bpf_adapter::SondeBpfInterpreter;

    let receipt_dir = tempfile::tempdir().unwrap();
    let stub = env!("CARGO_BIN_EXE_stub_handler");
    let receipt_path = receipt_dir.path().to_str().unwrap();
    let env = E2eTestEnv::new_with_handler(stub, &["--receipt-dir", receipt_path]);

    let psk = [0x30; 32];
    let (program, hash) = make_send_recv_program();
    setup_node_with_program(&env, "handler-rt", 1, psk, &program, &hash).await;

    let mut node = NodeProxy::new(1, psk);
    let mut interpreter = SondeBpfInterpreter::new();
    let stats = node.run_wake_cycle_with(&env, &mut interpreter);

    assert_eq!(stats.outcome, WakeCycleOutcome::Sleep { seconds: 60 });

    // The node should have sent exactly one APP_DATA frame.
    let app_data_count = stats
        .sent_frames
        .iter()
        .filter(|(t, _)| *t == sonde_protocol::MSG_APP_DATA)
        .count();
    assert_eq!(
        app_data_count, 1,
        "node should send exactly one APP_DATA frame"
    );

    // Gateway must have replied with an APP_DATA_REPLY.
    let reply_count = stats
        .received_msg_types
        .iter()
        .filter(|&&t| t == sonde_protocol::MSG_APP_DATA_REPLY)
        .count();
    assert_eq!(
        reply_count, 1,
        "gateway must produce exactly one APP_DATA_REPLY"
    );

    // Verify the handler received the correct blob via receipt file.
    let receipt = std::fs::read(receipt_dir.path().join("receipt.bin")).unwrap();
    assert_eq!(
        receipt,
        [0xAA, 0xBB],
        "handler must receive the correct APP_DATA blob"
    );
}

/// T-E2E-031 — APP_DATA fire-and-forget (send helper).
///
/// A BPF program calls `send()` (helper 8) with blob `[0x01, 0x02]`.
/// The gateway routes the APP_DATA to a handler that returns an empty
/// reply. No APP_DATA_REPLY is sent back to the node.
///
/// Validates: ND-0602 (send, no reply expected).
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_031_app_data_fire_and_forget_with_handler() {
    use sonde_node::sonde_bpf_adapter::SondeBpfInterpreter;

    let receipt_dir = tempfile::tempdir().unwrap();
    let stub = env!("CARGO_BIN_EXE_stub_handler");
    let receipt_path = receipt_dir.path().to_str().unwrap();
    let env = E2eTestEnv::new_with_handler(stub, &["--no-reply", "--receipt-dir", receipt_path]);

    let psk = [0x31; 32];
    let (program, hash) = make_send_program_0102();
    setup_node_with_program(&env, "handler-ff", 1, psk, &program, &hash).await;

    let mut node = NodeProxy::new(1, psk);
    let mut interpreter = SondeBpfInterpreter::new();
    let stats = node.run_wake_cycle_with(&env, &mut interpreter);

    assert_eq!(stats.outcome, WakeCycleOutcome::Sleep { seconds: 60 });

    // The node should have sent exactly one APP_DATA frame.
    let app_data_count = stats
        .sent_frames
        .iter()
        .filter(|(t, _)| *t == sonde_protocol::MSG_APP_DATA)
        .count();
    assert_eq!(
        app_data_count, 1,
        "node should send exactly one APP_DATA frame"
    );

    // Handler returned empty reply → gateway must NOT send APP_DATA_REPLY.
    let reply_count = stats
        .received_msg_types
        .iter()
        .filter(|&&t| t == sonde_protocol::MSG_APP_DATA_REPLY)
        .count();
    assert_eq!(
        reply_count, 0,
        "gateway must not produce APP_DATA_REPLY for fire-and-forget (handler returned empty)"
    );

    // Verify the handler was actually invoked and received the correct blob.
    let receipt = std::fs::read(receipt_dir.path().join("receipt.bin")).unwrap();
    assert_eq!(
        receipt,
        [0x01, 0x02],
        "handler must receive the correct APP_DATA blob"
    );
}

/// T-E2E-032 — APP_DATA AEAD end-to-end.
///
/// Verifies that APP_DATA sent by a BPF program through the AEAD frame
/// path is correctly decrypted by the gateway and delivered to the handler.
/// The on-wire AEAD format (11-byte header + ciphertext + 16-byte GCM tag)
/// is validated by the gateway's successful decryption — if the frame
/// format were wrong, `process_frame` would silently discard it.
///
/// Validates: GW-0500, GW-0600, ND-0300, ND-0602.
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_032_app_data_aead_end_to_end() {
    use sonde_node::sonde_bpf_adapter::SondeBpfInterpreter;

    let receipt_dir = tempfile::tempdir().unwrap();
    let stub = env!("CARGO_BIN_EXE_stub_handler");
    let receipt_path = receipt_dir.path().to_str().unwrap();
    let env = E2eTestEnv::new_with_handler(stub, &["--receipt-dir", receipt_path]);

    let psk = [0x32; 32];
    let (program, hash) = make_send_program_dead();
    setup_node_with_program(&env, "handler-aead", 1, psk, &program, &hash).await;

    let mut node = NodeProxy::new(1, psk);
    let mut interpreter = SondeBpfInterpreter::new();
    let stats = node.run_wake_cycle_with(&env, &mut interpreter);

    assert_eq!(stats.outcome, WakeCycleOutcome::Sleep { seconds: 60 });

    // Verify the AEAD APP_DATA frame was sent by the node.
    let app_data_count = stats
        .sent_frames
        .iter()
        .filter(|(t, _)| *t == sonde_protocol::MSG_APP_DATA)
        .count();
    assert_eq!(
        app_data_count, 1,
        "node should send exactly one AEAD APP_DATA frame"
    );

    // Verify the APP_DATA frame is in AEAD format: 11B header + ciphertext + 16B tag.
    let app_data_frame = stats
        .sent_raw_frames
        .iter()
        .find(|f| {
            f.len() >= sonde_protocol::HEADER_SIZE
                && f[sonde_protocol::OFFSET_MSG_TYPE] == sonde_protocol::MSG_APP_DATA
        })
        .expect("APP_DATA raw frame must be captured");
    assert!(
        app_data_frame.len() >= sonde_protocol::HEADER_SIZE + 16,
        "AEAD APP_DATA frame must be at least 11B header + 16B GCM tag, got {} bytes",
        app_data_frame.len()
    );

    // Gateway decrypted the AEAD frame and routed to the handler.
    let reply_count = stats
        .received_msg_types
        .iter()
        .filter(|&&t| t == sonde_protocol::MSG_APP_DATA_REPLY)
        .count();
    assert_eq!(
        reply_count, 1,
        "gateway must decrypt AEAD APP_DATA, route to handler, and produce APP_DATA_REPLY"
    );

    // Verify the handler received the decrypted blob [0xDE, 0xAD].
    let receipt = std::fs::read(receipt_dir.path().join("receipt.bin")).unwrap();
    assert_eq!(
        receipt,
        [0xDE, 0xAD],
        "handler must receive the decrypted APP_DATA blob"
    );
}

/// T-E2E-033 — Live reload: handler add end-to-end.
///
/// The gateway starts with no handler configured. APP_DATA is silently
/// dropped (no handler match). After adding a handler for the specific
/// program hash via the router, APP_DATA is routed correctly on the
/// next wake cycle.
///
/// Validates: GW-1404, GW-1407.
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_033_live_reload_handler_add() {
    use sonde_gateway::handler::{HandlerConfig, ProgramMatcher};
    use sonde_node::sonde_bpf_adapter::SondeBpfInterpreter;

    // Start with no handlers.
    let env = E2eTestEnv::new();

    let psk = [0x33; 32];
    let (program, hash) = make_send_program();
    setup_node_with_program(&env, "handler-add", 1, psk, &program, &hash).await;

    // --- Cycle 1: no handler configured → APP_DATA not routed ---
    let mut node = NodeProxy::new(1, psk);
    let mut interpreter = SondeBpfInterpreter::new();
    let stats1 = node.run_wake_cycle_with(&env, &mut interpreter);

    assert_eq!(stats1.outcome, WakeCycleOutcome::Sleep { seconds: 60 });
    let app_data_count_1 = stats1
        .sent_frames
        .iter()
        .filter(|(t, _)| *t == sonde_protocol::MSG_APP_DATA)
        .count();
    assert_eq!(
        app_data_count_1, 1,
        "cycle 1: node must send exactly one APP_DATA frame"
    );
    let reply_count_1 = stats1
        .received_msg_types
        .iter()
        .filter(|&&t| t == sonde_protocol::MSG_APP_DATA_REPLY)
        .count();
    assert_eq!(
        reply_count_1, 0,
        "cycle 1: no APP_DATA_REPLY expected (no handler configured)"
    );

    // --- Add handler for the specific program hash ---
    let stub = env!("CARGO_BIN_EXE_stub_handler");
    let config = HandlerConfig {
        matchers: vec![ProgramMatcher::Hash(hash.clone())],
        command: stub.to_string(),
        args: vec![],
        reply_timeout: None,
        working_dir: None,
    };
    {
        let router_arc = env.gateway.handler_router();
        let mut router = router_arc.write().await;
        let _removed = router.reload(vec![config]);
    }

    // --- Cycle 2: handler present → APP_DATA routed and replied ---
    let stats2 = node.run_wake_cycle_with(&env, &mut interpreter);

    assert_eq!(stats2.outcome, WakeCycleOutcome::Sleep { seconds: 60 });
    let app_data_count_2 = stats2
        .sent_frames
        .iter()
        .filter(|(t, _)| *t == sonde_protocol::MSG_APP_DATA)
        .count();
    assert_eq!(
        app_data_count_2, 1,
        "cycle 2: node must send exactly one APP_DATA frame"
    );
    let reply_count_2 = stats2
        .received_msg_types
        .iter()
        .filter(|&&t| t == sonde_protocol::MSG_APP_DATA_REPLY)
        .count();
    assert_eq!(
        reply_count_2, 1,
        "cycle 2: APP_DATA_REPLY expected (handler was live-added)"
    );
}

/// T-E2E-034 — Live reload: handler remove end-to-end.
///
/// The gateway starts with a catch-all handler. APP_DATA is routed and
/// replied. After removing the handler via the router, APP_DATA is
/// silently dropped on the next wake cycle and the handler process is
/// shut down.
///
/// Validates: GW-1404, GW-1407.
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_034_live_reload_handler_remove() {
    use sonde_gateway::handler::shutdown_removed_handlers;
    use sonde_node::sonde_bpf_adapter::SondeBpfInterpreter;

    let stub = env!("CARGO_BIN_EXE_stub_handler");
    let env = E2eTestEnv::new_with_handler(stub, &[]);

    let psk = [0x34; 32];
    let (program, hash) = make_send_program();
    setup_node_with_program(&env, "handler-rm", 1, psk, &program, &hash).await;

    // --- Cycle 1: catch-all handler present → APP_DATA routed ---
    let mut node = NodeProxy::new(1, psk);
    let mut interpreter = SondeBpfInterpreter::new();
    let stats1 = node.run_wake_cycle_with(&env, &mut interpreter);

    assert_eq!(stats1.outcome, WakeCycleOutcome::Sleep { seconds: 60 });
    let reply_count_1 = stats1
        .received_msg_types
        .iter()
        .filter(|&&t| t == sonde_protocol::MSG_APP_DATA_REPLY)
        .count();
    assert_eq!(
        reply_count_1, 1,
        "cycle 1: APP_DATA_REPLY expected (catch-all handler matched)"
    );

    // --- Remove all handlers via HandlerRouter ---
    let removed = {
        let router_arc = env.gateway.handler_router();
        let mut router = router_arc.write().await;
        router.reload(vec![])
    };
    // Shut down the removed handler processes.
    shutdown_removed_handlers(removed).await;

    // Verify the router has zero handlers after removal.
    {
        let router_arc = env.gateway.handler_router();
        let router = router_arc.read().await;
        assert_eq!(
            router.handler_count(),
            0,
            "router must have zero handlers after removal"
        );
    }

    // --- Cycle 2: no handler → APP_DATA not routed ---
    let stats2 = node.run_wake_cycle_with(&env, &mut interpreter);

    assert_eq!(stats2.outcome, WakeCycleOutcome::Sleep { seconds: 60 });
    let app_data_count_2 = stats2
        .sent_frames
        .iter()
        .filter(|(t, _)| *t == sonde_protocol::MSG_APP_DATA)
        .count();
    assert_eq!(
        app_data_count_2, 1,
        "cycle 2: node must still send APP_DATA even with no handler"
    );
    let reply_count_2 = stats2
        .received_msg_types
        .iter()
        .filter(|&&t| t == sonde_protocol::MSG_APP_DATA_REPLY)
        .count();
    assert_eq!(
        reply_count_2, 0,
        "cycle 2: no APP_DATA_REPLY expected (handler was live-removed)"
    );
}

// ---------------------------------------------------------------------------
// T-E2E-002 — AEAD authentication round-trip (base case)
// ---------------------------------------------------------------------------

/// T-E2E-002 — AEAD authentication round-trip.
///
/// Validates that AES-256-GCM authentication works correctly across
/// gateway (RustCryptoAead) and node (NodeAead) for a single wake cycle.
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_002_aead_authentication_round_trip() {
    let env = E2eTestEnv::new();
    let psk = [0xAA; 32];
    env.register_node("aead-002", 1, psk).await;

    let mut node = NodeProxy::new(1, psk);
    let stats = node.run_wake_cycle(&env);

    assert_eq!(stats.outcome, WakeCycleOutcome::Sleep { seconds: 60 });
    assert!(
        stats.response_count > 0,
        "gateway must respond to valid AEAD frame"
    );
    let rec = env.storage.get_node("aead-002").await.unwrap().unwrap();
    assert_eq!(rec.last_battery_mv, Some(3300));
}

// ---------------------------------------------------------------------------
// T-E2E-010 — Full program update cycle
// ---------------------------------------------------------------------------

/// T-E2E-010 — Full program update cycle with chunked transfer.
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_010_full_program_update_cycle() {
    use sonde_node::sonde_bpf_adapter::SondeBpfInterpreter;

    let env = E2eTestEnv::new();
    let psk = [0x10; 32];
    env.register_node("prog-update", 1, psk).await;

    let (program, hash) = make_send_program();
    env.storage.store_program(&program).await.unwrap();

    let mut node_rec = env.storage.get_node("prog-update").await.unwrap().unwrap();
    node_rec.assigned_program_hash = Some(hash.clone());
    env.storage.upsert_node(&node_rec).await.unwrap();

    let mut node = NodeProxy::new(1, psk);
    let mut interpreter = SondeBpfInterpreter::new();
    let stats = node.run_wake_cycle_with(&env, &mut interpreter);

    assert_eq!(stats.outcome, WakeCycleOutcome::Sleep { seconds: 60 });

    let get_chunk_count = stats
        .sent_frames
        .iter()
        .filter(|(t, _)| *t == sonde_protocol::MSG_GET_CHUNK)
        .count();
    assert!(get_chunk_count > 0, "node must send GET_CHUNK requests");

    let ack_count = stats
        .sent_frames
        .iter()
        .filter(|(t, _)| *t == sonde_protocol::MSG_PROGRAM_ACK)
        .count();
    assert_eq!(ack_count, 1, "node must send exactly one PROGRAM_ACK");

    let updated = env.storage.get_node("prog-update").await.unwrap().unwrap();
    assert_eq!(updated.current_program_hash, Some(hash));
}

// ---------------------------------------------------------------------------
// T-E2E-011 — Program already current → NOP
// ---------------------------------------------------------------------------

/// T-E2E-011 — Program already current → NOP (no chunked transfer).
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_011_program_already_current_nop() {
    use sonde_node::sonde_bpf_adapter::SondeBpfInterpreter;

    let env = E2eTestEnv::new();
    let psk = [0x11; 32];
    env.register_node("prog-current", 1, psk).await;

    let (program, hash) = make_send_program();
    env.storage.store_program(&program).await.unwrap();

    let mut node_rec = env.storage.get_node("prog-current").await.unwrap().unwrap();
    node_rec.assigned_program_hash = Some(hash.clone());
    env.storage.upsert_node(&node_rec).await.unwrap();

    // First cycle: node downloads and installs the program.
    let mut node = NodeProxy::new(1, psk);
    let mut interpreter = SondeBpfInterpreter::new();
    let stats1 = node.run_wake_cycle_with(&env, &mut interpreter);
    assert_eq!(stats1.outcome, WakeCycleOutcome::Sleep { seconds: 60 });
    let ack_count = stats1
        .sent_frames
        .iter()
        .filter(|(t, _)| *t == sonde_protocol::MSG_PROGRAM_ACK)
        .count();
    assert_eq!(ack_count, 1, "first cycle must install program");

    // Second cycle: hashes match → NOP.
    let stats2 = node.run_wake_cycle_with(&env, &mut interpreter);
    assert_eq!(stats2.outcome, WakeCycleOutcome::Sleep { seconds: 60 });

    let get_chunk_count = stats2
        .sent_frames
        .iter()
        .filter(|(t, _)| *t == sonde_protocol::MSG_GET_CHUNK)
        .count();
    assert_eq!(get_chunk_count, 0, "no GET_CHUNK when program is current");
}

// ---------------------------------------------------------------------------
// T-E2E-020 — UPDATE_SCHEDULE via admin
// ---------------------------------------------------------------------------

/// T-E2E-020 — UPDATE_SCHEDULE via admin command queue.
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_020_update_schedule_via_admin() {
    use sonde_gateway::engine::PendingCommand;

    let env = E2eTestEnv::new();
    let psk = [0x20; 32];
    env.register_node("sched-node", 1, psk).await;

    env.gateway
        .queue_command(
            "sched-node",
            PendingCommand::UpdateSchedule { interval_s: 120 },
        )
        .await;

    let mut node = NodeProxy::new(1, psk);
    let stats = node.run_wake_cycle(&env);

    assert_eq!(stats.outcome, WakeCycleOutcome::Sleep { seconds: 120 });
}

// ---------------------------------------------------------------------------
// T-E2E-021 — REBOOT via admin
// ---------------------------------------------------------------------------

/// T-E2E-021 — REBOOT via admin command queue.
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_021_reboot_via_admin() {
    use sonde_gateway::engine::PendingCommand;

    let env = E2eTestEnv::new();
    let psk = [0x21; 32];
    env.register_node("reboot-node", 1, psk).await;

    env.gateway
        .queue_command("reboot-node", PendingCommand::Reboot)
        .await;

    let mut node = NodeProxy::new(1, psk);
    let stats = node.run_wake_cycle(&env);

    assert_eq!(stats.outcome, WakeCycleOutcome::Reboot);
}

// ---------------------------------------------------------------------------
// T-E2E-022 — RUN_EPHEMERAL via admin
// ---------------------------------------------------------------------------

/// T-E2E-022 — RUN_EPHEMERAL via admin command queue.
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_022_run_ephemeral_via_admin() {
    use sonde_gateway::engine::PendingCommand;
    use sonde_node::sonde_bpf_adapter::SondeBpfInterpreter;

    let env = E2eTestEnv::new();
    let psk = [0x22; 32];
    env.register_node("eph-node", 1, psk).await;

    let mut node = NodeProxy::new(1, psk);
    let mut interpreter = SondeBpfInterpreter::new();

    // First cycle: NOP (establish session).
    let _stats0 = node.run_wake_cycle_with(&env, &mut interpreter);

    // Queue ephemeral program for second cycle.
    let nop_bytecode = [
        0xb7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r0, 0
        0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // exit
    ];
    let eph_image = ProgramImage {
        bytecode: nop_bytecode.to_vec(),
        maps: vec![],
        map_initial_data: vec![],
    };
    let eph_cbor = eph_image.encode_deterministic().unwrap();
    let sha = TestSha256;
    let eph_hash = sha.hash(&eph_cbor).to_vec();
    let eph_program = ProgramRecord {
        hash: eph_hash.clone(),
        image: eph_cbor.clone(),
        size: eph_cbor.len() as u32,
        verification_profile: VerificationProfile::Ephemeral,
        abi_version: None,
        source_filename: None,
    };
    env.storage.store_program(&eph_program).await.unwrap();

    env.gateway
        .queue_command(
            "eph-node",
            PendingCommand::RunEphemeral {
                program_hash: eph_hash,
            },
        )
        .await;

    // Second cycle: RunEphemeral command dispatched.
    let stats = node.run_wake_cycle_with(&env, &mut interpreter);

    assert_eq!(stats.outcome, WakeCycleOutcome::Sleep { seconds: 60 });

    let get_chunk_count = stats
        .sent_frames
        .iter()
        .filter(|(t, _)| *t == sonde_protocol::MSG_GET_CHUNK)
        .count();
    assert!(get_chunk_count > 0, "ephemeral program must be downloaded");

    // Verify PROGRAM_ACK sent (transfer completed).
    let ack_count = stats
        .sent_frames
        .iter()
        .filter(|(t, _)| *t == sonde_protocol::MSG_PROGRAM_ACK)
        .count();
    assert_eq!(
        ack_count, 1,
        "ephemeral transfer must complete with PROGRAM_ACK"
    );
}

// ---------------------------------------------------------------------------
// T-E2E-040 — Unknown node silent discard
// ---------------------------------------------------------------------------

/// T-E2E-040 — Unknown node silent discard.
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_040_unknown_node_silent_discard() {
    let env = E2eTestEnv::new();
    let psk = [0x40; 32];
    let mut node = NodeProxy::new(99, psk);
    let stats = node.run_wake_cycle(&env);

    assert_eq!(stats.outcome, WakeCycleOutcome::Sleep { seconds: 60 });
    assert_eq!(stats.response_count, 0, "gateway must not respond");
}

// ---------------------------------------------------------------------------
// T-E2E-041 — Sequence number enforcement
// ---------------------------------------------------------------------------

/// T-E2E-041 — Sequence number enforcement in chunked transfer.
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_041_sequence_number_enforcement() {
    use sonde_node::sonde_bpf_adapter::SondeBpfInterpreter;

    let env = E2eTestEnv::new();
    let psk = [0x41; 32];
    env.register_node("seq-node", 1, psk).await;

    // Use a large program (200 instructions: 199 NOPs + exit = 1600 bytes)
    // to force multiple chunks. DEFAULT_CHUNK_SIZE is 128 bytes.
    let mut large_bytecode = Vec::new();
    for _ in 0..199 {
        // mov r0, 0 (NOP-equivalent)
        large_bytecode.extend_from_slice(&[0xb7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
    }
    // exit
    large_bytecode.extend_from_slice(&[0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);

    let (program, hash) = make_program_from_bytecode(&large_bytecode);
    env.storage.store_program(&program).await.unwrap();

    let mut node_rec = env.storage.get_node("seq-node").await.unwrap().unwrap();
    node_rec.assigned_program_hash = Some(hash);
    env.storage.upsert_node(&node_rec).await.unwrap();

    let mut node = NodeProxy::new(1, psk);
    let mut interpreter = SondeBpfInterpreter::new();
    let stats = node.run_wake_cycle_with(&env, &mut interpreter);

    assert_eq!(stats.outcome, WakeCycleOutcome::Sleep { seconds: 60 });

    let get_chunk_nonces: Vec<u64> = stats
        .sent_frames
        .iter()
        .filter(|(t, _)| *t == sonde_protocol::MSG_GET_CHUNK)
        .map(|(_, n)| *n)
        .collect();
    assert!(
        get_chunk_nonces.len() >= 2,
        "program must require at least 2 chunks, got {}",
        get_chunk_nonces.len()
    );
    for window in get_chunk_nonces.windows(2) {
        assert_eq!(
            window[1],
            window[0] + 1,
            "GET_CHUNK nonces must increment: {:?}",
            get_chunk_nonces
        );
    }

    let ack_count = stats
        .sent_frames
        .iter()
        .filter(|(t, _)| *t == sonde_protocol::MSG_PROGRAM_ACK)
        .count();
    assert_eq!(ack_count, 1, "transfer must complete with PROGRAM_ACK");
}
