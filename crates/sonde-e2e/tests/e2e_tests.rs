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
    make_program_from_bytecode(&bytecode, VerificationProfile::Resident)
}

fn make_program_from_bytecode(
    bytecode: &[u8],
    profile: VerificationProfile,
) -> (ProgramRecord, Vec<u8>) {
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
        verification_profile: profile,
        abi_version: None,
        source_filename: None,
    };
    (record, hash)
}

// ===========================================================================
// BLE pairing / onboarding tests (T-E2E-060 through T-E2E-070)
// ===========================================================================

use sonde_e2e::harness::{
    build_encrypted_payload, build_encrypted_payload_with_timestamp, setup_gateway_identity,
    simulate_phone_registration, GatewayBleAdapter,
};
use sonde_pair::validation::compute_key_hint;

/// T-E2E-060 — Gateway Ed25519 identity generated and persisted.
///
/// Validates GW-1200 and GW-1201: the gateway generates an Ed25519 keypair
/// and 16-byte gateway_id on first startup, and both are recoverable from
/// storage after a simulated restart.
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_060_gateway_identity_persistence() {
    let env = E2eTestEnv::new();
    let identity = setup_gateway_identity(&env.storage).await;

    // Reload from storage — simulates restart.
    let loaded = Storage::load_gateway_identity(&*env.storage)
        .await
        .unwrap()
        .expect("identity must be persisted");

    assert_eq!(loaded.public_key(), identity.public_key());
    assert_eq!(loaded.gateway_id(), identity.gateway_id());
}

/// T-E2E-061 — Phone registration: TOFU + registration window + PSK exchange.
///
/// Full Phase 1 round-trip: REQUEST_GW_INFO challenge-response, REGISTER_PHONE
/// with AEAD phone PSK exchange, and verification that:
/// - Phone PSK is stored in gateway storage.
/// - A closed registration window correctly returns ERROR 0x02.
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_061_phone_registration() {
    use sonde_gateway::ble_pairing::{handle_ble_recv, RegistrationWindow};
    use sonde_pair::envelope::{build_envelope, parse_envelope, parse_error_body};
    use sonde_pair::rng::{OsRng, RngProvider};
    use sonde_pair::types;
    use std::sync::Arc;

    let env = E2eTestEnv::new();
    let identity = setup_gateway_identity(&env.storage).await;
    let rf_channel = 6u8;

    // Phase 1: full registration via helper.
    let (phone_psk, phone_key_hint) =
        simulate_phone_registration(&identity, &env.storage, rf_channel).await;

    // Verify phone PSK stored in gateway.
    let psks = Storage::list_phone_psks(&*env.storage).await.unwrap();
    assert_eq!(psks.len(), 1, "exactly one phone PSK should be stored");
    assert_eq!(psks[0].phone_key_hint, phone_key_hint);
    assert_eq!(*psks[0].psk, *phone_psk);

    // Verify closed window rejects REGISTER_PHONE with ERROR 0x02.
    let mut window = RegistrationWindow::new(); // closed
    let dyn_storage: Arc<dyn Storage> = env.storage.clone();

    let rng = OsRng;
    let mut test_psk = [0u8; 32];
    rng.fill_bytes(&mut test_psk).unwrap();
    let label = b"test";
    let mut body = Vec::with_capacity(32 + 1 + label.len());
    body.extend_from_slice(&test_psk);
    body.push(label.len() as u8);
    body.extend_from_slice(label);
    let register_request = build_envelope(types::REGISTER_PHONE, &body).unwrap();
    let register_response = handle_ble_recv(
        &register_request,
        &identity,
        &dyn_storage,
        &mut window,
        rf_channel,
        None,
    )
    .await;

    assert!(
        register_response.is_some(),
        "should return ERROR envelope when window is closed"
    );
    let error_response = register_response.unwrap();
    let (msg_type, error_body) = parse_envelope(&error_response).unwrap();
    assert_eq!(msg_type, types::MSG_ERROR, "response must be ERROR type");
    let (status, _) = parse_error_body(error_body);
    assert_eq!(status, 0x02, "error status must be WINDOW_CLOSED (0x02)");
}

/// T-E2E-062 — Node BLE provisioning: Phase 2 NODE_PROVISION handling.
///
/// Constructs a valid NODE_PROVISION payload (including encrypted_payload
/// derived from Phase 1 artifacts) and feeds it to the node's BLE
/// provisioning handler. Verifies NVS state after provisioning.
///
/// Covers: T-N904
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_062_node_ble_provisioning() {
    use sonde_node::ble_pairing::{handle_node_provision, NodeProvision, NODE_ACK_SUCCESS};

    let env = E2eTestEnv::new();
    let identity = setup_gateway_identity(&env.storage).await;
    let rf_channel = 6u8;
    let (phone_psk, phone_key_hint) =
        simulate_phone_registration(&identity, &env.storage, rf_channel).await;

    // Generate node identity.
    let node_psk = [0xBBu8; 32];
    let node_key_hint = compute_key_hint(&node_psk);

    // Build encrypted payload (mirrors sonde-pair Phase 2).
    let encrypted_payload = build_encrypted_payload(
        identity.public_key(),
        identity.gateway_id(),
        &phone_psk,
        phone_key_hint,
        "ble-node-1",
        &node_psk,
        rf_channel,
        &[],
    );

    // Feed NODE_PROVISION to the node handler.
    let provision = NodeProvision {
        key_hint: node_key_hint,
        psk: node_psk,
        rf_channel,
        encrypted_payload: encrypted_payload.clone(),
        pin_config: None,
    };
    let mut node = NodeProxy::new_unpaired();
    let status = handle_node_provision(
        &provision,
        &mut node.storage,
        &mut node.map_storage,
        false,
        false,
    );
    assert_eq!(status, NODE_ACK_SUCCESS, "NODE_PROVISION must succeed");

    // Verify node storage state.
    assert_eq!(node.storage.read_key(), Some((node_key_hint, node_psk)));
    assert_eq!(node.storage.read_channel(), Some(rf_channel));
    assert_eq!(
        node.storage.read_peer_payload().as_deref(),
        Some(encrypted_payload.as_slice())
    );
    assert!(
        !node.storage.read_reg_complete(),
        "reg_complete must be false after provisioning"
    );
}

/// T-E2E-063 — PEER_REQUEST/PEER_ACK: node relays payload, gateway registers.
///
/// A BLE-provisioned node sends PEER_REQUEST over ESP-NOW. The gateway
/// decrypts the encrypted_payload, verifies the phone HMAC, registers the
/// node, and returns PEER_ACK with registration_proof.
///
/// Covers: T-N909, T-N912, T-N915
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_063_peer_request_ack() {
    let env = E2eTestEnv::new();
    let identity = setup_gateway_identity(&env.storage).await;
    let rf_channel = 6u8;
    let (phone_psk, phone_key_hint) =
        simulate_phone_registration(&identity, &env.storage, rf_channel).await;

    let node_psk = [0xCCu8; 32];
    let node_key_hint = compute_key_hint(&node_psk);
    let node_id = "peer-req-node";

    let encrypted_payload = build_encrypted_payload(
        identity.public_key(),
        identity.gateway_id(),
        &phone_psk,
        phone_key_hint,
        node_id,
        &node_psk,
        rf_channel,
        &[],
    );

    // Create BLE-provisioned node (PSK + payload, reg_complete=false).
    let mut node =
        NodeProxy::new_ble_provisioned(node_key_hint, node_psk, rf_channel, encrypted_payload);

    // Run wake cycle — node sends PEER_REQUEST, gateway processes + WAKE.
    let stats = node.run_wake_cycle(&env);

    // PEER_REQUEST succeeded: node should complete a normal WAKE cycle.
    assert_eq!(
        stats.outcome,
        WakeCycleOutcome::Sleep { seconds: 60 },
        "node should complete PEER_REQUEST + WAKE and sleep"
    );

    // Verify PEER_REQUEST frame was sent (msg_type 0x05).
    let peer_req_count = stats
        .sent_frames
        .iter()
        .filter(|(t, _)| *t == sonde_protocol::MSG_PEER_REQUEST)
        .count();
    assert_eq!(
        peer_req_count, 1,
        "node must send exactly one PEER_REQUEST frame"
    );

    // Verify the node is now registered in the gateway.
    let registered = env
        .storage
        .get_node(node_id)
        .await
        .unwrap()
        .expect("node must be registered after PEER_REQUEST");
    assert_eq!(registered.key_hint, node_key_hint);

    // Verify node's reg_complete flag is set.
    assert!(
        node.storage.read_reg_complete(),
        "reg_complete must be true after valid PEER_ACK"
    );
}

/// T-E2E-064 — Complete onboarding → first WAKE.
///
/// Full lifecycle: Phase 1 (phone registration) → Phase 2 (node
/// provisioning) → Phase 3 (PEER_REQUEST/PEER_ACK) → first WAKE/COMMAND.
/// Verifies the node transitions from bootstrap to steady-state.
///
/// Covers: T-N915, T-N916
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_064_onboarding_to_wake() {
    let env = E2eTestEnv::new();
    let identity = setup_gateway_identity(&env.storage).await;
    let rf_channel = 6u8;
    let (phone_psk, phone_key_hint) =
        simulate_phone_registration(&identity, &env.storage, rf_channel).await;

    let node_psk = [0xDDu8; 32];
    let node_key_hint = compute_key_hint(&node_psk);
    let node_id = "onboard-node";

    let encrypted_payload = build_encrypted_payload(
        identity.public_key(),
        identity.gateway_id(),
        &phone_psk,
        phone_key_hint,
        node_id,
        &node_psk,
        rf_channel,
        &[],
    );

    let mut node =
        NodeProxy::new_ble_provisioned(node_key_hint, node_psk, rf_channel, encrypted_payload);

    // First cycle: PEER_REQUEST + WAKE.
    let stats = node.run_wake_cycle(&env);
    assert_eq!(stats.outcome, WakeCycleOutcome::Sleep { seconds: 60 });

    // Verify steady-state: peer_payload erased (ND-0914), reg_complete set.
    assert!(node.storage.read_reg_complete());
    assert!(
        node.storage.read_peer_payload().is_none(),
        "peer_payload must be erased after first successful WAKE"
    );

    // Second cycle: pure WAKE (no PEER_REQUEST).
    let stats2 = node.run_wake_cycle(&env);
    assert_eq!(
        stats2.outcome,
        WakeCycleOutcome::Sleep { seconds: 60 },
        "steady-state wake cycle must succeed"
    );
    // No PEER_REQUEST in the second cycle.
    let peer_req_count = stats2
        .sent_frames
        .iter()
        .filter(|(t, _)| *t == sonde_protocol::MSG_PEER_REQUEST)
        .count();
    assert_eq!(
        peer_req_count, 0,
        "steady-state cycle must not send PEER_REQUEST"
    );
}

/// T-E2E-065 — Deferred erasure: peer_payload erased only after first WAKE.
///
/// Validates ND-0913 (retain peer_payload on PEER_ACK) and ND-0914 (erase
/// after first WAKE/COMMAND success). The encrypted_payload survives the
/// PEER_ACK step and is only erased when the gateway confirms the node
/// via the steady-state WAKE/COMMAND exchange.
///
/// Covers: T-N913, T-N916
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_065_deferred_erasure() {
    let env = E2eTestEnv::new();
    let identity = setup_gateway_identity(&env.storage).await;
    let rf_channel = 6u8;
    let (phone_psk, phone_key_hint) =
        simulate_phone_registration(&identity, &env.storage, rf_channel).await;

    let node_psk = [0xEEu8; 32];
    let node_key_hint = compute_key_hint(&node_psk);

    let encrypted_payload = build_encrypted_payload(
        identity.public_key(),
        identity.gateway_id(),
        &phone_psk,
        phone_key_hint,
        "defer-node",
        &node_psk,
        rf_channel,
        &[],
    );

    let mut node = NodeProxy::new_ble_provisioned(
        node_key_hint,
        node_psk,
        rf_channel,
        encrypted_payload.clone(),
    );

    // Before any cycle: payload present, reg_complete false.
    assert!(node.storage.read_peer_payload().is_some());
    assert!(!node.storage.read_reg_complete());

    // Run cycle: PEER_REQUEST + WAKE succeeds → payload erased.
    let stats = node.run_wake_cycle(&env);
    assert_eq!(stats.outcome, WakeCycleOutcome::Sleep { seconds: 60 });

    // After WAKE success: reg_complete set, peer_payload erased.
    assert!(
        node.storage.read_reg_complete(),
        "reg_complete must be true"
    );
    assert!(
        node.storage.read_peer_payload().is_none(),
        "peer_payload must be erased after WAKE success (ND-0914)"
    );
}

/// T-E2E-066 — Self-healing: forged ACK → WAKE failure → revert to PEER_REQUEST.
///
/// Simulates a scenario where reg_complete was set (e.g., by a forged PEER_ACK)
/// but the gateway does not actually know the node. The WAKE attempt fails,
/// triggering self-healing (ND-0915): reg_complete is cleared and the node
/// reverts to PEER_REQUEST mode on the next cycle.
///
/// Covers: T-N917
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_066_self_healing() {
    let env = E2eTestEnv::new();
    let identity = setup_gateway_identity(&env.storage).await;
    let rf_channel = 6u8;
    let (phone_psk, phone_key_hint) =
        simulate_phone_registration(&identity, &env.storage, rf_channel).await;

    let node_psk = [0x11u8; 32];
    let node_key_hint = compute_key_hint(&node_psk);

    let encrypted_payload = build_encrypted_payload(
        identity.public_key(),
        identity.gateway_id(),
        &phone_psk,
        phone_key_hint,
        "heal-node",
        &node_psk,
        rf_channel,
        &[],
    );

    // Simulate forged PEER_ACK: reg_complete = true but gateway doesn't
    // know the node.
    let mut node = NodeProxy::new_ble_provisioned(
        node_key_hint,
        node_psk,
        rf_channel,
        encrypted_payload.clone(),
    );
    node.storage
        .write_reg_complete(true)
        .expect("set reg_complete");

    // Cycle 1: reg_complete=true → skip PEER_REQUEST → WAKE fails
    // (gateway doesn't recognize key_hint) → self-healing clears reg_complete.
    let stats1 = node.run_wake_cycle(&env);
    assert_eq!(
        stats1.outcome,
        WakeCycleOutcome::Sleep { seconds: 60 },
        "WAKE failure should result in Sleep"
    );
    assert!(
        !node.storage.read_reg_complete(),
        "self-healing must clear reg_complete after WAKE failure (ND-0915)"
    );
    assert!(
        node.storage.read_peer_payload().is_some(),
        "peer_payload must be retained for PEER_REQUEST retry"
    );

    // Cycle 2: reg_complete=false, payload present → PEER_REQUEST →
    // gateway registers → PEER_ACK → WAKE → success.
    let stats2 = node.run_wake_cycle(&env);
    assert_eq!(
        stats2.outcome,
        WakeCycleOutcome::Sleep { seconds: 60 },
        "recovery cycle should succeed"
    );
    assert!(node.storage.read_reg_complete());
    assert!(node.storage.read_peer_payload().is_none());
}

/// T-E2E-067 — Agent revocation: revoked phone PSK causes silent discard.
///
/// A phone PSK is revoked after registration. A PEER_REQUEST built with
/// the revoked phone's credentials is silently discarded by the gateway.
/// The node times out waiting for PEER_ACK and returns to sleep.
///
/// Covers: T-N911
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_067_agent_revocation() {
    let env = E2eTestEnv::new();
    let identity = setup_gateway_identity(&env.storage).await;
    let rf_channel = 6u8;
    let (phone_psk, phone_key_hint) =
        simulate_phone_registration(&identity, &env.storage, rf_channel).await;

    // Revoke the phone PSK.
    let psks = Storage::list_phone_psks(&*env.storage).await.unwrap();
    assert_eq!(psks.len(), 1);
    Storage::revoke_phone_psk(&*env.storage, psks[0].phone_id)
        .await
        .unwrap();

    let node_psk = [0x22u8; 32];
    let node_key_hint = compute_key_hint(&node_psk);

    // Build payload with the now-revoked phone credentials.
    let encrypted_payload = build_encrypted_payload(
        identity.public_key(),
        identity.gateway_id(),
        &phone_psk,
        phone_key_hint,
        "revoke-node",
        &node_psk,
        rf_channel,
        &[],
    );

    let mut node =
        NodeProxy::new_ble_provisioned(node_key_hint, node_psk, rf_channel, encrypted_payload);

    // PEER_REQUEST should be silently discarded (revoked phone) → timeout → Sleep.
    let stats = node.run_wake_cycle(&env);
    assert_eq!(
        stats.outcome,
        WakeCycleOutcome::Sleep { seconds: 60 },
        "revoked phone PEER_REQUEST must result in Sleep (timeout)"
    );

    // PEER_REQUEST was sent but no registration occurred.
    let peer_req_count = stats
        .sent_frames
        .iter()
        .filter(|(t, _)| *t == sonde_protocol::MSG_PEER_REQUEST)
        .count();
    assert_eq!(peer_req_count, 1, "node must send PEER_REQUEST");

    assert!(
        !node.storage.read_reg_complete(),
        "reg_complete must remain false (no PEER_ACK received)"
    );
    assert!(
        env.storage.get_node("revoke-node").await.unwrap().is_none(),
        "node must NOT be registered with revoked phone credentials"
    );
}

/// T-E2E-068 — Factory reset and re-provisioning.
///
/// A BLE-provisioned node is factory-reset via USB pairing mode, clearing
/// all persistent state. The node can then be re-provisioned with a new
/// identity and successfully complete the PEER_REQUEST/PEER_ACK exchange.
///
/// Covers: T-N404, T-N904
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_068_factory_reset_reprovision() {
    use sonde_node::key_store::KeyStore;

    let env = E2eTestEnv::new();
    let identity = setup_gateway_identity(&env.storage).await;
    let rf_channel = 6u8;
    let (phone_psk, phone_key_hint) =
        simulate_phone_registration(&identity, &env.storage, rf_channel).await;

    let node_psk = [0x33u8; 32];
    let node_key_hint = compute_key_hint(&node_psk);

    let encrypted_payload = build_encrypted_payload(
        identity.public_key(),
        identity.gateway_id(),
        &phone_psk,
        phone_key_hint,
        "reset-node",
        &node_psk,
        rf_channel,
        &[],
    );

    // Provision node.
    let mut node =
        NodeProxy::new_ble_provisioned(node_key_hint, node_psk, rf_channel, encrypted_payload);

    // Run initial cycle to register the node.
    let stats = node.run_wake_cycle(&env);
    assert_eq!(stats.outcome, WakeCycleOutcome::Sleep { seconds: 60 });
    assert!(node.storage.read_reg_complete());

    // Factory reset via KeyStore (the firmware's factory-reset path).
    {
        let mut ks = KeyStore::new(&mut node.storage);
        ks.factory_reset(&mut node.map_storage).unwrap();
    }

    // Verify all state erased.
    assert_eq!(node.storage.read_key(), None, "key must be erased");
    assert!(
        node.storage.read_peer_payload().is_none(),
        "peer_payload must be erased"
    );

    // Re-provision with new identity.
    let new_node_psk = [0x44u8; 32];
    let new_node_key_hint = compute_key_hint(&new_node_psk);
    let new_encrypted_payload = build_encrypted_payload(
        identity.public_key(),
        identity.gateway_id(),
        &phone_psk,
        phone_key_hint,
        "reset-node-v2",
        &new_node_psk,
        rf_channel,
        &[],
    );

    // Simulate BLE provisioning.
    use sonde_node::ble_pairing::{handle_node_provision, NodeProvision, NODE_ACK_SUCCESS};
    let provision = NodeProvision {
        key_hint: new_node_key_hint,
        psk: new_node_psk,
        rf_channel,
        encrypted_payload: new_encrypted_payload,
        pin_config: None,
    };
    let status = handle_node_provision(
        &provision,
        &mut node.storage,
        &mut node.map_storage,
        false,
        false,
    );
    assert_eq!(status, NODE_ACK_SUCCESS);

    // Run PEER_REQUEST + WAKE with new identity.
    let stats2 = node.run_wake_cycle(&env);
    assert_eq!(
        stats2.outcome,
        WakeCycleOutcome::Sleep { seconds: 60 },
        "re-provisioned node must succeed"
    );
    assert!(node.storage.read_reg_complete());

    // New node registered in gateway.
    let registered = env
        .storage
        .get_node("reset-node-v2")
        .await
        .unwrap()
        .expect("re-provisioned node must be registered");
    assert_eq!(registered.key_hint, new_node_key_hint);
}

/// T-E2E-069 — Multi-node: two nodes onboarded and operating concurrently.
///
/// Two nodes are provisioned with distinct PSKs via the same phone. Both
/// complete PEER_REQUEST/PEER_ACK, then both run normal WAKE cycles. This
/// validates GW-1216 (node_id uniqueness) and confirms one node's key
/// compromise does not affect the other.
///
/// Covers: T-N200
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_069_multi_node() {
    let env = E2eTestEnv::new();
    let identity = setup_gateway_identity(&env.storage).await;
    let rf_channel = 6u8;
    let (phone_psk, phone_key_hint) =
        simulate_phone_registration(&identity, &env.storage, rf_channel).await;

    // Node A
    let psk_a = [0xAAu8; 32];
    let hint_a = compute_key_hint(&psk_a);
    let payload_a = build_encrypted_payload(
        identity.public_key(),
        identity.gateway_id(),
        &phone_psk,
        phone_key_hint,
        "multi-node-a",
        &psk_a,
        rf_channel,
        &[],
    );

    // Node B
    let psk_b = [0xBBu8; 32];
    let hint_b = compute_key_hint(&psk_b);
    let payload_b = build_encrypted_payload(
        identity.public_key(),
        identity.gateway_id(),
        &phone_psk,
        phone_key_hint,
        "multi-node-b",
        &psk_b,
        rf_channel,
        &[],
    );

    let mut node_a = NodeProxy::new_ble_provisioned(hint_a, psk_a, rf_channel, payload_a);
    let mut node_b = NodeProxy::new_ble_provisioned(hint_b, psk_b, rf_channel, payload_b);

    // Onboard node A.
    let stats_a = node_a.run_wake_cycle(&env);
    assert_eq!(stats_a.outcome, WakeCycleOutcome::Sleep { seconds: 60 });
    assert!(node_a.storage.read_reg_complete());

    // Onboard node B.
    let stats_b = node_b.run_wake_cycle(&env);
    assert_eq!(stats_b.outcome, WakeCycleOutcome::Sleep { seconds: 60 });
    assert!(node_b.storage.read_reg_complete());

    // Both registered in gateway.
    assert!(env
        .storage
        .get_node("multi-node-a")
        .await
        .unwrap()
        .is_some());
    assert!(env
        .storage
        .get_node("multi-node-b")
        .await
        .unwrap()
        .is_some());

    // Both operate in steady-state.
    let stats_a2 = node_a.run_wake_cycle(&env);
    let stats_b2 = node_b.run_wake_cycle(&env);
    assert_eq!(stats_a2.outcome, WakeCycleOutcome::Sleep { seconds: 60 });
    assert_eq!(stats_b2.outcome, WakeCycleOutcome::Sleep { seconds: 60 });

    // No PEER_REQUEST in steady-state.
    assert_eq!(
        stats_a2
            .sent_frames
            .iter()
            .filter(|(t, _)| *t == sonde_protocol::MSG_PEER_REQUEST)
            .count(),
        0
    );
    assert_eq!(
        stats_b2
            .sent_frames
            .iter()
            .filter(|(t, _)| *t == sonde_protocol::MSG_PEER_REQUEST)
            .count(),
        0
    );
}

/// T-E2E-070 — Full use case: deploy → pair → onboard → program → data.
///
/// Exercises the complete administrator workflow:
/// 1. Gateway deploys with fresh Ed25519 identity.
/// 2. Phone registers with the gateway (Phase 1).
/// 3. Node is BLE-provisioned (Phase 2 payload construction).
/// 4. Node relays PEER_REQUEST → gateway registers → PEER_ACK.
/// 5. A BPF program is deployed to the node.
/// 6. Node runs the program and sends APP_DATA.
///
/// Uses the real `SondeBpfInterpreter` for BPF execution.
///
/// Covers: T-N200, T-N500, T-N604, T-N904, T-N915, T-N916
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_070_full_use_case() {
    use sonde_node::sonde_bpf_adapter::SondeBpfInterpreter;

    let env = E2eTestEnv::new();

    // 1. Gateway identity.
    let identity = setup_gateway_identity(&env.storage).await;
    let rf_channel = 6u8;

    // 2. Phone registration.
    let (phone_psk, phone_key_hint) =
        simulate_phone_registration(&identity, &env.storage, rf_channel).await;

    // 3. Node provisioning.
    let node_psk = [0x70u8; 32];
    let node_key_hint = compute_key_hint(&node_psk);
    let node_id = "full-use-case-node";

    let encrypted_payload = build_encrypted_payload(
        identity.public_key(),
        identity.gateway_id(),
        &phone_psk,
        phone_key_hint,
        node_id,
        &node_psk,
        rf_channel,
        &[],
    );

    let mut node =
        NodeProxy::new_ble_provisioned(node_key_hint, node_psk, rf_channel, encrypted_payload);

    // 4. PEER_REQUEST/PEER_ACK + first WAKE.
    let stats = node.run_wake_cycle(&env);
    assert_eq!(stats.outcome, WakeCycleOutcome::Sleep { seconds: 60 });
    assert!(node.storage.read_reg_complete());
    assert!(node.storage.read_peer_payload().is_none());

    // 5. Deploy a BPF program (send APP_DATA via helper 8).
    let (program, hash) = make_send_program();
    env.storage.store_program(&program).await.unwrap();

    let mut node_rec = env.storage.get_node(node_id).await.unwrap().unwrap();
    node_rec.assigned_program_hash = Some(hash.clone());
    env.storage.upsert_node(&node_rec).await.unwrap();

    // 6. Run with real BPF — program calls send() helper.
    let mut interpreter = SondeBpfInterpreter::new();
    let stats2 = node.run_wake_cycle_with(&env, &mut interpreter);
    assert_eq!(stats2.outcome, WakeCycleOutcome::Sleep { seconds: 60 });

    // Verify APP_DATA was sent (msg_type 0x04).
    let app_data_count = stats2
        .sent_frames
        .iter()
        .filter(|(t, _)| *t == sonde_protocol::MSG_APP_DATA)
        .count();
    assert_eq!(
        app_data_count, 1,
        "node should send exactly one APP_DATA frame after full onboarding"
    );
}

// ===========================================================================
// BLE onboarding negative tests (issue #361)
// ===========================================================================

/// T-E2E-063a — Stale timestamp (>24 h) → gateway silent discard.
///
/// §7.3 step 9: MUST verify |now − timestamp| ≤ 86400; discard if exceeded.
/// An attacker replaying a captured PEER_REQUEST after 24 h must be rejected.
///
/// Covers: GW-1215
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_063a_stale_timestamp_discarded() {
    let env = E2eTestEnv::new();
    let identity = setup_gateway_identity(&env.storage).await;
    let rf_channel = 6u8;
    let (phone_psk, phone_key_hint) =
        simulate_phone_registration(&identity, &env.storage, rf_channel).await;

    let node_psk = [0xBBu8; 32];
    let node_id = "stale-ts-node";

    // Build payload with timestamp 86401 seconds in the past (1 second beyond window).
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let stale_timestamp = now
        .checked_sub(86401)
        .expect("system time must be at least 86401s after UNIX_EPOCH for this test")
        as i64;

    // build_encrypted_payload_with_timestamp returns a complete AEAD PEER_REQUEST frame.
    let stale_frame = build_encrypted_payload_with_timestamp(
        identity.public_key(),
        identity.gateway_id(),
        &phone_psk,
        phone_key_hint,
        node_id,
        &node_psk,
        rf_channel,
        &[],
        stale_timestamp,
    );

    let result = env
        .gateway
        .process_frame(&stale_frame, vec![0x01, 0x02, 0x03, 0x04, 0x05, 0x06])
        .await;

    assert!(
        result.is_none(),
        "stale timestamp must cause silent discard (no PEER_ACK)"
    );
    assert!(
        env.storage.get_node(node_id).await.unwrap().is_none(),
        "node must NOT be registered with stale timestamp"
    );

    // Positive control: same setup with a fresh timestamp MUST succeed,
    // proving the discard above was specifically due to the stale timestamp.
    let fresh_node_id = "fresh-ts-node";
    let fresh_frame = build_encrypted_payload(
        identity.public_key(),
        identity.gateway_id(),
        &phone_psk,
        phone_key_hint,
        fresh_node_id,
        &node_psk,
        rf_channel,
        &[],
    );
    let fresh_result = env
        .gateway
        .process_frame(&fresh_frame, vec![0x01, 0x02, 0x03, 0x04, 0x05, 0x06])
        .await;
    assert!(
        fresh_result.is_some(),
        "positive control: fresh timestamp must produce PEER_ACK"
    );
    assert!(
        env.storage.get_node(fresh_node_id).await.unwrap().is_some(),
        "positive control: node must be registered with fresh timestamp"
    );
}

/// T-E2E-063b — Frame phone_key_hint mismatch → gateway silent discard.
///
/// In the AEAD flow, the frame header key_hint identifies the phone PSK.
/// A wrong phone_key_hint yields no matching candidates → silent discard.
///
/// Covers: GW-1217
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_063b_key_hint_mismatch_discarded() {
    use sonde_pair::cbor::encode_pairing_request;
    use sonde_pair::crypto::{aes256gcm_encrypt, PAIRING_REQUEST_AAD};
    use sonde_pair::rng::{OsRng, RngProvider};
    use sonde_protocol::{encode_frame, FrameHeader, MSG_PEER_REQUEST};

    let env = E2eTestEnv::new();
    let identity = setup_gateway_identity(&env.storage).await;
    let rf_channel = 6u8;
    let (phone_psk, phone_key_hint) =
        simulate_phone_registration(&identity, &env.storage, rf_channel).await;

    let node_psk = [0xCDu8; 32];
    let node_id = "keyhint-mismatch-node";

    // Build inner PairingRequest CBOR and encrypt with phone_psk.
    let cbor = encode_pairing_request(node_id, &node_psk, rf_channel, &[], {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    })
    .unwrap();

    let rng = OsRng;
    let mut inner_nonce = [0u8; 12];
    rng.fill_bytes(&mut inner_nonce).unwrap();
    let inner_ct = aes256gcm_encrypt(&phone_psk, &inner_nonce, &cbor, PAIRING_REQUEST_AAD).unwrap();

    let mut encrypted_payload = Vec::with_capacity(12 + inner_ct.len());
    encrypted_payload.extend_from_slice(&inner_nonce);
    encrypted_payload.extend_from_slice(&inner_ct);

    let cbor_map = ciborium::Value::Map(vec![(
        ciborium::Value::Integer(sonde_protocol::PEER_REQ_KEY_PAYLOAD.into()),
        ciborium::Value::Bytes(encrypted_payload),
    )]);
    let mut cbor_buf = Vec::new();
    ciborium::into_writer(&cbor_map, &mut cbor_buf).unwrap();

    // Build PEER_REQUEST with a WRONG phone_key_hint in the frame header.
    let wrong_key_hint = phone_key_hint.wrapping_add(1);
    let sha = TestSha256;
    let aead = sonde_gateway::GatewayAead;
    let header = FrameHeader {
        key_hint: wrong_key_hint,
        msg_type: MSG_PEER_REQUEST,
        nonce: 0xAAAA_BBBB_CCCC_DDDD,
    };
    let frame = encode_frame(&header, &cbor_buf, &phone_psk, &aead, &sha).unwrap();

    let result = env
        .gateway
        .process_frame(&frame, vec![0x01, 0x02, 0x03, 0x04, 0x05, 0x06])
        .await;

    assert!(
        result.is_none(),
        "wrong phone_key_hint must cause silent discard (no PEER_ACK)"
    );
    assert!(
        env.storage.get_node(node_id).await.unwrap().is_none(),
        "node must NOT be registered with mismatched phone_key_hint"
    );

    // Positive control: correct phone_key_hint MUST succeed.
    let good_node_id = "keyhint-good-node";
    let good_frame = build_encrypted_payload(
        identity.public_key(),
        identity.gateway_id(),
        &phone_psk,
        phone_key_hint,
        good_node_id,
        &node_psk,
        rf_channel,
        &[],
    );
    let good_result = env
        .gateway
        .process_frame(&good_frame, vec![0x01, 0x02, 0x03, 0x04, 0x05, 0x06])
        .await;
    assert!(
        good_result.is_some(),
        "positive control: matching phone_key_hint must produce PEER_ACK"
    );
    assert!(
        env.storage.get_node(good_node_id).await.unwrap().is_some(),
        "positive control: node must be registered with matching phone_key_hint"
    );
}

/// T-E2E-063c — Duplicate node_id → gateway silent discard.
///
/// §7.3 step 10: MUST check node_id uniqueness; discard duplicate.
/// A replay or misconfigured node must not overwrite a valid registration.
///
/// Covers: GW-1216
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_063c_duplicate_node_id_discarded() {
    let env = E2eTestEnv::new();
    let identity = setup_gateway_identity(&env.storage).await;
    let rf_channel = 6u8;
    let (phone_psk, phone_key_hint) =
        simulate_phone_registration(&identity, &env.storage, rf_channel).await;

    let node_id = "dup-node";

    // First: register the node legitimately via PEER_REQUEST.
    let node_psk_1 = [0xE1u8; 32];
    let node_key_hint_1 = compute_key_hint(&node_psk_1);
    let encrypted_payload_1 = build_encrypted_payload(
        identity.public_key(),
        identity.gateway_id(),
        &phone_psk,
        phone_key_hint,
        node_id,
        &node_psk_1,
        rf_channel,
        &[],
    );

    let mut node1 = NodeProxy::new_ble_provisioned(
        node_key_hint_1,
        node_psk_1,
        rf_channel,
        encrypted_payload_1,
    );
    let stats1 = node1.run_wake_cycle(&env);
    assert!(
        matches!(stats1.outcome, WakeCycleOutcome::Sleep { .. }),
        "first registration must succeed (got {:?})",
        stats1.outcome
    );
    assert!(node1.storage.read_reg_complete());

    // Verify the node is registered with the first PSK.
    let registered = env
        .storage
        .get_node(node_id)
        .await
        .unwrap()
        .expect("node must be registered");
    assert_eq!(registered.key_hint, node_key_hint_1);

    // Second: attempt to register the same node_id with a different PSK.
    let node_psk_2 = [0xE2u8; 32];
    let node_key_hint_2 = compute_key_hint(&node_psk_2);
    let encrypted_payload_2 = build_encrypted_payload(
        identity.public_key(),
        identity.gateway_id(),
        &phone_psk,
        phone_key_hint,
        node_id,
        &node_psk_2,
        rf_channel,
        &[],
    );

    let mut node2 = NodeProxy::new_ble_provisioned(
        node_key_hint_2,
        node_psk_2,
        rf_channel,
        encrypted_payload_2,
    );

    // The second PEER_REQUEST should be silently discarded → timeout → Sleep.
    let stats2 = node2.run_wake_cycle(&env);
    assert!(
        matches!(stats2.outcome, WakeCycleOutcome::Sleep { .. }),
        "duplicate node_id PEER_REQUEST must result in Sleep (timeout) (got {:?})",
        stats2.outcome
    );

    // PEER_REQUEST was sent but no registration occurred.
    let peer_req_count = stats2
        .sent_frames
        .iter()
        .filter(|(t, _)| *t == sonde_protocol::MSG_PEER_REQUEST)
        .count();
    assert_eq!(peer_req_count, 1, "node must send PEER_REQUEST");

    assert!(
        !node2.storage.read_reg_complete(),
        "reg_complete must remain false (duplicate rejected)"
    );

    // Original registration must be untouched.
    let still_registered = env.storage.get_node(node_id).await.unwrap().unwrap();
    assert_eq!(
        still_registered.key_hint, node_key_hint_1,
        "original registration must not be overwritten"
    );
}

/// T-E2E-063d — Wrong PEER_ACK nonce → node rejects.
///
/// §7.2: PEER_ACK nonce MUST echo the PEER_REQUEST nonce. A PEER_ACK with
/// a wrong nonce (e.g. from a replay attack) must be rejected by the node.
///
/// Covers: T-N911
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_063d_wrong_peer_ack_nonce_rejected() {
    use sonde_node::key_store::NodeIdentity;
    use sonde_node::node_aead::NodeAead;
    use sonde_node::peer_request::verify_peer_ack;
    use sonde_protocol::decode_frame;

    let env = E2eTestEnv::new();
    let identity = setup_gateway_identity(&env.storage).await;
    let rf_channel = 6u8;
    let (phone_psk, phone_key_hint) =
        simulate_phone_registration(&identity, &env.storage, rf_channel).await;

    let node_psk = [0xDEu8; 32];
    let node_key_hint = compute_key_hint(&node_psk);
    let node_id = "nonce-test-node";

    // build_encrypted_payload returns a complete AEAD PEER_REQUEST frame.
    let complete_frame = build_encrypted_payload(
        identity.public_key(),
        identity.gateway_id(),
        &phone_psk,
        phone_key_hint,
        node_id,
        &node_psk,
        rf_channel,
        &[],
    );

    // Extract the nonce from the frame header.
    let decoded = decode_frame(&complete_frame).expect("frame must decode");
    let request_nonce = decoded.header.nonce;

    let node_identity = NodeIdentity {
        key_hint: node_key_hint,
        psk: node_psk,
    };

    // Send to gateway and get the PEER_ACK back.
    let ack_frame = env
        .gateway
        .process_frame(&complete_frame, vec![0x01, 0x02, 0x03, 0x04, 0x05, 0x06])
        .await
        .expect("gateway must return PEER_ACK for valid PEER_REQUEST");

    // Verify node accepts the real PEER_ACK with correct expected nonce.
    let aead = NodeAead;
    let sha = TestSha256;
    assert!(
        verify_peer_ack(&ack_frame, &node_identity, request_nonce, &aead, &sha).is_ok(),
        "node must accept PEER_ACK with correct nonce"
    );

    // Verify node REJECTS the same PEER_ACK when expected nonce differs.
    let wrong_nonce: u64 = 0x5555_6666_7777_8888;
    let result = verify_peer_ack(&ack_frame, &node_identity, wrong_nonce, &aead, &sha);
    assert!(
        result.is_err(),
        "node must reject PEER_ACK when nonce does not match PEER_REQUEST"
    );
    assert!(
        matches!(
            result.unwrap_err(),
            sonde_node::error::NodeError::ResponseBindingMismatch
        ),
        "error must be ResponseBindingMismatch"
    );
}

/// T-E2E-063e — sonde-pair → gateway end-to-end integration.
///
/// Wires the actual `sonde_pair::phase1::pair_with_gateway` state machine
/// to the gateway's `handle_ble_recv` via a `GatewayBleAdapter`, proving that
/// sonde-pair's Phase 1 AEAD output is compatible with the gateway. The
/// resulting `PairingArtifacts` are used to build the encrypted payload
/// for Phase 3 via `encrypt_pairing_request`, which flows through a
/// NodeProxy to the gateway via PEER_REQUEST/PEER_ACK.
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_063e_sonde_pair_gateway_integration() {
    use sonde_pair::cbor::encode_pairing_request;
    use sonde_pair::crypto::encrypt_pairing_request;
    use sonde_pair::phase1;
    use sonde_pair::rng::OsRng;
    use std::sync::Arc;

    let env = E2eTestEnv::new();
    let identity = setup_gateway_identity(&env.storage).await;
    let rf_channel = 6u8;

    // Phase 1: use sonde-pair's actual AEAD state machine against the gateway.
    let dyn_storage: Arc<dyn sonde_gateway::storage::Storage> = env.storage.clone();
    let mut transport = GatewayBleAdapter::new(identity.clone(), dyn_storage, rf_channel);
    let rng = OsRng;
    let device_addr = [0x10, 0x0B, 0xAC, 0x00, 0x00, 0x01];

    let artifacts = phase1::pair_with_gateway(
        &mut transport,
        &rng,
        &device_addr,
        "e2e-integration-phone",
        None,
    )
    .await
    .expect("Phase 1 AEAD pairing via GatewayBleAdapter must succeed");

    // Verify artifacts
    assert_eq!(artifacts.rf_channel, rf_channel);
    let expected_hint = compute_key_hint(&artifacts.phone_psk);
    assert_eq!(artifacts.phone_key_hint, expected_hint);

    // Verify phone PSK is stored in gateway.
    let psks = Storage::list_phone_psks(&*env.storage).await.unwrap();
    assert_eq!(psks.len(), 1, "exactly one phone PSK should be stored");

    // Phase 3: build encrypted payload using sonde-pair's AEAD crypto,
    // then wire through a NodeProxy to the gateway.
    let node_psk = [0xF0u8; 32];
    let node_key_hint = compute_key_hint(&node_psk);
    let node_id = "pair-integration-node";

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let cbor = encode_pairing_request(node_id, &node_psk, rf_channel, &[], timestamp).unwrap();
    let encrypted_payload = encrypt_pairing_request(&artifacts.phone_psk, &cbor)
        .expect("encrypt_pairing_request must succeed");

    // Wire the payload through a NodeProxy to the gateway.
    let mut node =
        NodeProxy::new_ble_provisioned(node_key_hint, node_psk, rf_channel, encrypted_payload);
    let stats = node.run_wake_cycle(&env);

    assert_eq!(
        stats.outcome,
        WakeCycleOutcome::Sleep { seconds: 60 },
        "sonde-pair → gateway integration must complete PEER_REQUEST + WAKE"
    );
    assert!(
        node.storage.read_reg_complete(),
        "reg_complete must be set after successful PEER_ACK"
    );

    // Verify node registered in gateway.
    let registered = env
        .storage
        .get_node(node_id)
        .await
        .unwrap()
        .expect("node must be registered via sonde-pair integration");
    assert_eq!(registered.key_hint, node_key_hint);
}

// ===========================================================================
// BPF execution context & helper boundary validation (issue #351)
// ===========================================================================

/// T-E2E-083 — Instruction budget enforcement through full stack.
///
/// Deploy a BPF program containing a long-running but finite loop through
/// the real gateway→node chunked transfer. Run with the real
/// `SondeBpfInterpreter`. A `set_next_wake(10)` sentinel after the loop
/// provides an observable side effect: if budget enforcement regresses and
/// the loop completes, the sentinel fires and changes sleep to 10 s, causing
/// the assertion to fail.
///
/// Uses a bounded loop (200 000 iterations ≈ 600 000 instructions) that
/// exceeds the 100 000 instruction budget but still terminates naturally
/// if budget enforcement regresses — preventing CI hangs.
///
/// Covers: bpf-environment.md §3.3, ND-0605
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_083_instruction_budget_enforcement() {
    use sonde_node::sonde_bpf_adapter::SondeBpfInterpreter;

    let env = E2eTestEnv::new();
    let psk = [0x83; 32];
    env.register_node("budget-node", 1, psk).await;

    // Bounded loop: 200 000 iterations × 3 body instructions = 600 000
    // total, well above the 100 000 budget. A set_next_wake(10) sentinel
    // follows the loop — if budget enforcement regresses, the sentinel
    // fires and changes sleep to 10 s, failing the assertion below.
    let bytecode = [
        0xb7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r0, 0
        0xb7, 0x01, 0x00, 0x00, 0x40, 0x0D, 0x03, 0x00, // mov r1, 200000
        0x07, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, // add r0, 1
        0x07, 0x01, 0x00, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, // add r1, -1
        0x55, 0x01, 0xFD, 0xFF, 0x00, 0x00, 0x00, 0x00, // jne r1, 0, -3
        // Sentinel: only reachable if budget enforcement regresses.
        0xb7, 0x01, 0x00, 0x00, 0x0A, 0x00, 0x00, 0x00, // mov r1, 10
        0x85, 0x00, 0x00, 0x00, 0x0F, 0x00, 0x00, 0x00, // call 15 (set_next_wake)
        0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // exit
    ];
    let (program, hash) = make_program_from_bytecode(&bytecode, VerificationProfile::Resident);
    env.storage.store_program(&program).await.unwrap();

    let mut node_rec = env.storage.get_node("budget-node").await.unwrap().unwrap();
    node_rec.assigned_program_hash = Some(hash);
    env.storage.upsert_node(&node_rec).await.unwrap();

    let mut node = NodeProxy::new(1, psk);
    let mut interpreter = SondeBpfInterpreter::new();
    let stats = node.run_wake_cycle_with(&env, &mut interpreter);

    // Verify the program was actually installed (PROGRAM_ACK sent).
    let ack_count = stats
        .sent_frames
        .iter()
        .filter(|(t, _)| *t == sonde_protocol::MSG_PROGRAM_ACK)
        .count();
    assert_eq!(
        ack_count, 1,
        "program must be installed (PROGRAM_ACK sent) before budget can be tested"
    );

    // The interpreter must terminate the loop and the node must return to
    // sleep normally — no hang or panic.
    assert_eq!(
        stats.outcome,
        WakeCycleOutcome::Sleep { seconds: 60 },
        "node must return to sleep after budget exhaustion"
    );
}

/// T-E2E-081 — Ephemeral program restrictions through full stack.
///
/// Validates that both `map_update_elem` (helper 11) and `set_next_wake`
/// (helper 15) are rejected for ephemeral programs.
///
/// 1. Install a resident program WITH a map so that `map_storage` has an
///    allocated map 0.
/// 2. Deploy an ephemeral program that calls `map_update_elem` then
///    `set_next_wake(10)`.
/// 3. Assert the sleep interval remains at 60 s (set_next_wake rejected).
/// 4. Assert map 0 was not written (map_update_elem rejected).
///
/// Covers: bpf-environment.md §2.2, ND-0603, ND-0604
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_081_ephemeral_restrictions() {
    use sonde_node::sonde_bpf_adapter::SondeBpfInterpreter;
    use sonde_protocol::MapDef;

    let env = E2eTestEnv::new();
    let psk = [0x80; 32];
    env.register_node("ephemeral-restrict-node", 1, psk).await;

    // Step 1: Install a resident program with a single map so map 0 exists.
    let resident_image = ProgramImage {
        bytecode: vec![
            0xb7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r0, 0
            0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // exit
        ],
        maps: vec![MapDef {
            map_type: 1,
            key_size: 4,
            value_size: 4,
            max_entries: 1,
        }],
        map_initial_data: vec![vec![]],
    };
    let resident_cbor = resident_image.encode_deterministic().unwrap();
    let sha = TestSha256;
    let resident_hash = sha.hash(&resident_cbor).to_vec();
    let resident_size = resident_cbor.len() as u32;
    let resident_record = ProgramRecord {
        hash: resident_hash.clone(),
        image: resident_cbor,
        size: resident_size,
        verification_profile: VerificationProfile::Resident,
        abi_version: None,
        source_filename: None,
    };
    env.storage.store_program(&resident_record).await.unwrap();

    let mut node_rec = env
        .storage
        .get_node("ephemeral-restrict-node")
        .await
        .unwrap()
        .unwrap();
    node_rec.assigned_program_hash = Some(resident_hash);
    env.storage.upsert_node(&node_rec).await.unwrap();

    let mut node = NodeProxy::new(1, psk);
    let mut interpreter = SondeBpfInterpreter::new();
    let stats = node.run_wake_cycle_with(&env, &mut interpreter);
    assert_eq!(
        stats.outcome,
        WakeCycleOutcome::Sleep { seconds: 60 },
        "resident install cycle must succeed"
    );
    // Confirm map 0 is allocated and empty after resident install.
    assert!(
        node.map_storage.get(0).is_some(),
        "map 0 must be allocated by resident program"
    );

    // Step 2: Deploy ephemeral program that calls map_update_elem then
    // set_next_wake. The ephemeral image must NOT declare its own maps
    // (the node rejects ephemeral programs with maps entirely).
    let ephemeral_bytecode = [
        // *(u32*)(r10 - 8) = 0        — key on stack
        0x62, 0x0A, 0xF8, 0xFF, 0x00, 0x00, 0x00, 0x00,
        // *(u32*)(r10 - 16) = 42       — value on stack
        0x62, 0x0A, 0xF0, 0xFF, 0x2A, 0x00, 0x00, 0x00,
        // r1 = 0                       — map_fd
        // Note: r1=0 is not a valid relocated map pointer, but the helper
        // checks the ephemeral restriction BEFORE map pointer validation
        // (bpf_dispatch.rs helper_map_update_elem), so this exercises the
        // correct rejection path. Ephemeral programs cannot obtain a valid
        // map pointer because LD_DW_IMM relocations resolve against the
        // program's own map declarations, which are empty for ephemeral.
        0xb7, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r1, 0 (invalid map fd)
        0xbf, 0xA2, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r2, r10
        // r2 += -8                     — key ptr
        0x07, 0x02, 0x00, 0x00, 0xF8, 0xFF, 0xFF, 0xFF, 0xbf, 0xA3, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, // mov r3, r10
        // r3 += -16                    — value ptr
        0x07, 0x03, 0x00, 0x00, 0xF0, 0xFF, 0xFF, 0xFF,
        // call 11                      — map_update_elem (rejected for ephemeral)
        0x85, 0x00, 0x00, 0x00, 0x0B, 0x00, 0x00, 0x00,
        // r1 = 10                      — seconds
        0xb7, 0x01, 0x00, 0x00, 0x0A, 0x00, 0x00, 0x00,
        // call 15                      — set_next_wake (rejected for ephemeral)
        0x85, 0x00, 0x00, 0x00, 0x0F, 0x00, 0x00, 0x00, // exit
        0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    let (eph_program, eph_hash) =
        make_program_from_bytecode(&ephemeral_bytecode, VerificationProfile::Ephemeral);
    env.storage.store_program(&eph_program).await.unwrap();

    env.gateway
        .queue_command(
            "ephemeral-restrict-node",
            PendingCommand::RunEphemeral {
                program_hash: eph_hash,
            },
        )
        .await;

    let stats = node.run_wake_cycle_with(&env, &mut interpreter);

    // Verify the ephemeral program was downloaded (GET_CHUNK sent).
    let get_chunk_count = stats
        .sent_frames
        .iter()
        .filter(|(t, _)| *t == sonde_protocol::MSG_GET_CHUNK)
        .count();
    assert!(
        get_chunk_count > 0,
        "ephemeral program must be downloaded via chunked transfer"
    );

    // set_next_wake must be rejected — sleep at base interval (60 s).
    assert_eq!(
        stats.outcome,
        WakeCycleOutcome::Sleep { seconds: 60 },
        "ephemeral set_next_wake must be rejected; base interval unchanged"
    );

    // map_update_elem must be rejected — map 0 must remain zero-initialized.
    let map = node
        .map_storage
        .get(0)
        .expect("map 0 must still be allocated");
    assert_eq!(
        map.lookup(0).unwrap(),
        &[0u8, 0, 0, 0],
        "ephemeral map_update_elem must be rejected; map value must remain zero"
    );
}
