// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! End-to-end integration tests exercising the full gateway ↔ node protocol.

use sonde_e2e::harness::{E2eTestEnv, NodeProxy};
use sonde_gateway::engine::PendingCommand;
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

    let node = NodeProxy::new("test-node", 1, psk);
    let outcome = node.run_wake_cycle(&env).await;

    assert_eq!(outcome, WakeCycleOutcome::Sleep { seconds: 60 });

    // Verify gateway updated node telemetry
    let record = env.storage.get_node("test-node").await.unwrap().unwrap();
    assert_eq!(record.last_battery_mv, Some(3300));
    assert!(record.last_seen.is_some());
}

/// T-E2E-003 — Wrong PSK rejected (silent discard).
///
/// When the node's PSK does not match the gateway's record the gateway
/// silently discards the WAKE frame. The node exhausts its retries and
/// falls back to the default sleep interval.
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_003_wrong_psk_rejected() {
    let env = E2eTestEnv::new().await;
    env.register_node("test-node", 1, [0xAA; 32]).await;

    // Node has a different PSK
    let node = NodeProxy::new("test-node", 1, [0xBB; 32]);
    let outcome = node.run_wake_cycle(&env).await;

    // Should exhaust retries and sleep
    assert_eq!(outcome, WakeCycleOutcome::Sleep { seconds: 60 });
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
    env.pending_commands
        .write()
        .await
        .entry("sched-node".into())
        .or_default()
        .push(PendingCommand::UpdateSchedule { interval_s: 120 });

    let node = NodeProxy::new("sched-node", 1, psk);
    let outcome = node.run_wake_cycle(&env).await;

    assert_eq!(outcome, WakeCycleOutcome::Sleep { seconds: 120 });
}

/// T-E2E-021 — REBOOT command.
///
/// The gateway queues a Reboot command. The wake cycle returns `Reboot`.
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_021_reboot() {
    let env = E2eTestEnv::new().await;
    let psk = [0xDD; 32];
    env.register_node("reboot-node", 1, psk).await;

    env.pending_commands
        .write()
        .await
        .entry("reboot-node".into())
        .or_default()
        .push(PendingCommand::Reboot);

    let node = NodeProxy::new("reboot-node", 1, psk);
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

    let node = NodeProxy::new("unknown", 99, [0xFF; 32]);
    let outcome = node.run_wake_cycle(&env).await;

    assert_eq!(outcome, WakeCycleOutcome::Sleep { seconds: 60 });
}
