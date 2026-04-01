// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! AES-256-GCM end-to-end integration tests.
//!
//! These tests mirror the HMAC-based E2E tests but exercise the AEAD frame
//! path: the node uses `run_wake_cycle_aead` (with `NodeAead`) and the
//! gateway processes frames via `process_frame_aead` (with `GatewayAead`).
//!
//! Only compiled when the `aes-gcm-codec` feature is enabled.

#![cfg(feature = "aes-gcm-codec")]

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

/// T-E2E-050 — AEAD NOP wake cycle.
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
async fn t_e2e_050_aead_nop_wake_cycle() {
    let env = E2eTestEnv::new();
    let psk = [0x50; 32];
    env.register_node("aead-nop", 1, psk).await;

    let mut node = NodeProxy::new(1, psk);
    let stats = node.run_wake_cycle_aead(&env);

    assert_eq!(stats.outcome, WakeCycleOutcome::Sleep { seconds: 60 });

    // Gateway should have produced a COMMAND response.
    assert!(
        stats.response_count > 0,
        "AEAD wake cycle must receive at least one response (COMMAND)"
    );

    // Verify gateway updated node telemetry.
    let record = env.storage.get_node("aead-nop").await.unwrap().unwrap();
    assert_eq!(record.last_battery_mv, Some(3300));
    assert!(record.last_seen.is_some());
    assert_eq!(
        record.firmware_abi_version,
        Some(sonde_node::FIRMWARE_ABI_VERSION)
    );
}

/// T-E2E-050b — Consecutive AEAD wake cycles (state persistence).
///
/// Runs two AEAD wake cycles on the same `NodeProxy` to verify that
/// persistent storage and monotonic RNG state work correctly across
/// multiple AES-256-GCM exchanges.
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_050b_aead_consecutive_wake_cycles() {
    let env = E2eTestEnv::new();
    let psk = [0x55; 32];
    env.register_node("aead-multi", 1, psk).await;

    let mut node = NodeProxy::new(1, psk);

    let stats1 = node.run_wake_cycle_aead(&env);
    assert_eq!(stats1.outcome, WakeCycleOutcome::Sleep { seconds: 60 });
    assert!(stats1.response_count > 0);

    let stats2 = node.run_wake_cycle_aead(&env);
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

/// T-E2E-051 — AEAD wake cycle with BPF APP_DATA.
///
/// Exercises the full AEAD wake cycle with a BPF program that calls the
/// `send()` helper. The WAKE/COMMAND exchange uses AES-256-GCM; the BPF
/// dispatch sends APP_DATA via the HMAC codec (this is the current design —
/// BPF helpers have not yet been migrated to AEAD).
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_051_aead_app_data_fire_and_forget() {
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
    let stats = node.run_wake_cycle_aead_with(&env, &mut interpreter);

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

/// T-E2E-052 — AEAD wrong PSK (silent discard).
///
/// When the node's PSK does not match the gateway's record, the gateway
/// cannot decrypt the AEAD frame and silently discards it. The node
/// exhausts its retries and sleeps.
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_052_aead_wrong_psk_rejected() {
    let env = E2eTestEnv::new();
    env.register_node("aead-wrong", 1, [0xAA; 32]).await;

    // Node has a different PSK.
    let mut node = NodeProxy::new(1, [0xBB; 32]);
    let stats = node.run_wake_cycle_aead(&env);

    assert_eq!(stats.outcome, WakeCycleOutcome::Sleep { seconds: 60 });
    assert_eq!(
        stats.response_count, 0,
        "gateway should send zero responses on AEAD authentication failure"
    );

    // Gateway must not have updated telemetry.
    let record = env.storage.get_node("aead-wrong").await.unwrap().unwrap();
    assert!(
        record.last_seen.is_none(),
        "`last_seen` should be None — gateway silently discarded the WAKE"
    );
    assert_eq!(
        record.last_battery_mv, None,
        "battery should not be updated on auth failure"
    );
}

/// T-E2E-053 — AEAD tampered frame (silent discard).
///
/// A bit-flip in the ciphertext region causes the GCM tag check to fail.
/// The gateway silently discards the corrupted frame and the node receives
/// no response, eventually exhausting retries.
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_053_aead_tampered_frame_discarded() {
    let env = E2eTestEnv::new();
    let psk = [0x53; 32];
    env.register_node("aead-tamper", 1, psk).await;

    let mut node = NodeProxy::new(1, psk);
    let stats = node.run_wake_cycle_aead_tampered(&env);

    assert_eq!(stats.outcome, WakeCycleOutcome::Sleep { seconds: 60 });
    assert_eq!(
        stats.response_count, 0,
        "gateway should send zero responses on tampered AEAD frame"
    );

    // Gateway must not have updated telemetry.
    let record = env.storage.get_node("aead-tamper").await.unwrap().unwrap();
    assert!(
        record.last_seen.is_none(),
        "`last_seen` should be None — gateway silently discarded the tampered WAKE"
    );
}
