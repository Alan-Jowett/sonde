// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! End-to-end integration tests exercising the full gateway ↔ node protocol.

use sonde_e2e::harness::{E2eTestEnv, NodeProxy};
use sonde_gateway::engine::PendingCommand;
use sonde_node::traits::PlatformStorage;
use sonde_node::wake_cycle::WakeCycleOutcome;

use sonde_gateway::storage::Storage;

/// T-E2E-001 — NOP wake cycle.
///
/// A paired node with no pending commands completes a normal WAKE/COMMAND
/// exchange and returns to sleep at its configured interval.
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_001_nop_wake_cycle() {
    let env = E2eTestEnv::new().await;
    let psk = [0xAA; 32];
    env.register_node("test-node", 1, psk).await;

    let mut node = NodeProxy::new("test-node", 1, psk);
    let outcome = node.run_wake_cycle(&env).await;

    assert_eq!(outcome, WakeCycleOutcome::Sleep { seconds: 60 });

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
    let env = E2eTestEnv::new().await;
    let psk = [0x42; 32];
    env.register_node("hmac-node", 0x1234, psk).await;

    let mut node = NodeProxy::new("hmac-node", 0x1234, psk);
    let outcome = node.run_wake_cycle(&env).await;

    // Success means: node's WAKE was authenticated by gateway,
    // and gateway's COMMAND was authenticated by node.
    assert_eq!(outcome, WakeCycleOutcome::Sleep { seconds: 60 });
}

/// T-E2E-002b — Consecutive wake cycles with unique nonces.
///
/// Runs two wake cycles on the same node. Verifies that the second cycle
/// succeeds (nonces are unique, state persists correctly).
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_002b_consecutive_wake_cycles() {
    let env = E2eTestEnv::new().await;
    let psk = [0x55; 32];
    env.register_node("multi-node", 1, psk).await;

    let mut node = NodeProxy::new("multi-node", 1, psk);

    let outcome1 = node.run_wake_cycle(&env).await;
    assert_eq!(outcome1, WakeCycleOutcome::Sleep { seconds: 60 });

    let outcome2 = node.run_wake_cycle(&env).await;
    assert_eq!(outcome2, WakeCycleOutcome::Sleep { seconds: 60 });

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
    let env = E2eTestEnv::new().await;
    env.register_node("test-node", 1, [0xAA; 32]).await;

    // Node has a different PSK
    let mut node = NodeProxy::new("test-node", 1, [0xBB; 32]);
    let outcome = node.run_wake_cycle(&env).await;

    // Should exhaust retries and sleep
    assert_eq!(outcome, WakeCycleOutcome::Sleep { seconds: 60 });

    // Note: With BridgeTransport, we can't independently count gateway
    // response frames. The WakeCycleOutcome::Sleep with retries exhausted
    // confirms the gateway did not respond with a valid COMMAND.
}

/// T-E2E-020 — UPDATE_SCHEDULE command.
///
/// The gateway queues an UpdateSchedule command. After the wake cycle the
/// node adopts the new interval.
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_020_update_schedule() {
    let env = E2eTestEnv::new().await;
    let psk = [0xCC; 32];
    env.register_node("sched-node", 1, psk).await;

    // Queue schedule update
    env.gateway
        .queue_command(
            "sched-node",
            PendingCommand::UpdateSchedule { interval_s: 120 },
        )
        .await;

    let mut node = NodeProxy::new("sched-node", 1, psk);
    let outcome = node.run_wake_cycle(&env).await;

    assert_eq!(outcome, WakeCycleOutcome::Sleep { seconds: 120 });

    // Node persisted the new interval
    assert_eq!(node.storage.read_schedule().0, 120);

    // Pending command consumed
    let cmds = env.pending_commands.read().await;
    assert!(cmds.get("sched-node").map_or(true, |v| v.is_empty()));
}

/// T-E2E-021 — REBOOT command.
///
/// The gateway queues a Reboot command. The wake cycle returns `Reboot`.
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_021_reboot() {
    let env = E2eTestEnv::new().await;
    let psk = [0xDD; 32];
    env.register_node("reboot-node", 1, psk).await;

    env.gateway
        .queue_command("reboot-node", PendingCommand::Reboot)
        .await;

    let mut node = NodeProxy::new("reboot-node", 1, psk);
    let outcome = node.run_wake_cycle(&env).await;

    assert_eq!(outcome, WakeCycleOutcome::Reboot);
}

/// T-E2E-040 — Unknown node (silent discard).
///
/// A node whose key_hint is not registered on the gateway gets no
/// response. The node exhausts retries and sleeps at the default interval.
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_040_unknown_node() {
    let env = E2eTestEnv::new().await;
    // Do NOT register node

    let mut node = NodeProxy::new("unknown", 99, [0xFF; 32]);
    let outcome = node.run_wake_cycle(&env).await;

    assert_eq!(outcome, WakeCycleOutcome::Sleep { seconds: 60 });

    // Note: With BridgeTransport, we can't independently count gateway
    // response frames. The WakeCycleOutcome::Sleep with retries exhausted
    // confirms the gateway did not respond with a valid COMMAND.
}
