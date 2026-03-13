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
    make_program_from_bytecode(
        &[
            0xb7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r0, 0
            0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // exit
        ],
        VerificationProfile::Resident,
    )
}

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
    };
    (record, hash)
}

/// Read a single modem message from an async stream using `FrameDecoder`.
///
/// Handles partial reads, frame coalescing, and empty-frame errors.
async fn read_modem_msg(
    reader: &mut (impl tokio::io::AsyncReadExt + Unpin),
    decoder: &mut sonde_protocol::modem::FrameDecoder,
    buf: &mut [u8],
) -> sonde_protocol::modem::ModemMessage {
    use sonde_protocol::modem::ModemCodecError;

    loop {
        match decoder.decode() {
            Ok(Some(msg)) => return msg,
            Ok(None) => {}
            Err(ModemCodecError::EmptyFrame) => continue,
            Err(e) => panic!("modem decode error: {e}"),
        }
        let n = reader.read(buf).await.expect("read from modem stream");
        assert!(n > 0, "unexpected EOF on modem stream");
        decoder.push(&buf[..n]);
    }
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
    let pending = env.pending_commands.as_ref().unwrap();
    let cmds = pending.read().await;
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

    // Create an ephemeral program (same NOP bytecode, ephemeral profile).
    let (record, hash) = make_program_from_bytecode(
        &[
            0xb7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r0, 0
            0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // exit
        ],
        VerificationProfile::Ephemeral,
    );
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

// ===========================================================================
// Modem transport tests
// ===========================================================================

/// T-E2E-050 — Modem startup handshake.
///
/// Validates the RESET → MODEM_READY → SET_CHANNEL → SET_CHANNEL_ACK
/// startup sequence between `UsbEspNowTransport` and a simulated modem.
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_050_modem_startup_handshake() {
    use sonde_gateway::modem::UsbEspNowTransport;
    use sonde_protocol::modem::{encode_modem_frame, FrameDecoder, ModemMessage, ModemReady};
    use tokio::io::AsyncWriteExt;

    let (client, mut server) = tokio::io::duplex(1024);

    let test_mac = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
    let channel: u8 = 6;

    // Simulate modem on the server side in a background task.
    let modem_task = tokio::spawn(async move {
        let mut decoder = FrameDecoder::new();
        let mut buf = [0u8; 256];

        // 1. Read RESET frame.
        let msg = read_modem_msg(&mut server, &mut decoder, &mut buf).await;
        assert!(
            matches!(msg, ModemMessage::Reset),
            "first message should be RESET"
        );

        // 2. Send MODEM_READY.
        let ready = encode_modem_frame(&ModemMessage::ModemReady(ModemReady {
            firmware_version: [0x01, 0x00, 0x00, 0x00],
            mac_address: test_mac,
        }))
        .unwrap();
        server.write_all(&ready).await.unwrap();

        // 3. Read SET_CHANNEL.
        let msg = read_modem_msg(&mut server, &mut decoder, &mut buf).await;
        let ch = match msg {
            ModemMessage::SetChannel(c) => c,
            other => panic!("expected SET_CHANNEL, got {:?}", other),
        };

        // 4. Send SET_CHANNEL_ACK.
        let ack = encode_modem_frame(&ModemMessage::SetChannelAck(ch)).unwrap();
        server.write_all(&ack).await.unwrap();
    });

    // Create transport — this performs the full handshake.
    let transport = UsbEspNowTransport::new(client, channel).await;
    assert!(
        transport.is_ok(),
        "modem startup handshake should succeed: {:?}",
        transport.err()
    );

    modem_task.await.unwrap();
}

/// T-E2E-051 — Frame round-trip through modem bridge.
///
/// Sends a frame from the gateway transport to a simulated modem and
/// verifies the modem receives it correctly. Then the modem sends a
/// frame back and the gateway transport receives it.
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_051_modem_frame_round_trip() {
    use sonde_gateway::modem::UsbEspNowTransport;
    use sonde_gateway::transport::Transport;
    use sonde_protocol::modem::{
        encode_modem_frame, FrameDecoder, ModemMessage, ModemReady, RecvFrame,
    };
    use tokio::io::AsyncWriteExt;

    let (client, mut server) = tokio::io::duplex(4096);

    let modem_mac = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66];
    let peer_mac = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
    let channel: u8 = 1;

    // Simulate modem: startup handshake + frame echo.
    let modem_task = tokio::spawn(async move {
        let mut decoder = FrameDecoder::new();
        let mut buf = [0u8; 1024];

        // Startup handshake.
        let msg = read_modem_msg(&mut server, &mut decoder, &mut buf).await;
        assert!(matches!(msg, ModemMessage::Reset));

        let ready = encode_modem_frame(&ModemMessage::ModemReady(ModemReady {
            firmware_version: [0x01, 0x00, 0x00, 0x00],
            mac_address: modem_mac,
        }))
        .unwrap();
        server.write_all(&ready).await.unwrap();

        let msg = read_modem_msg(&mut server, &mut decoder, &mut buf).await;
        let ch = match msg {
            ModemMessage::SetChannel(c) => c,
            other => panic!("expected SET_CHANNEL, got {:?}", other),
        };
        let ack = encode_modem_frame(&ModemMessage::SetChannelAck(ch)).unwrap();
        server.write_all(&ack).await.unwrap();

        // Read SEND_FRAME from gateway transport.
        let msg = read_modem_msg(&mut server, &mut decoder, &mut buf).await;
        let frame_data = match msg {
            ModemMessage::SendFrame(sf) => {
                assert_eq!(sf.peer_mac, peer_mac, "peer MAC should match");
                sf.frame_data
            }
            other => panic!("expected SEND_FRAME, got {:?}", other),
        };
        assert_eq!(
            frame_data,
            vec![0x01, 0x02, 0x03],
            "frame data should match"
        );

        // Send RECV_FRAME back (simulating a response from the peer).
        let recv = encode_modem_frame(&ModemMessage::RecvFrame(RecvFrame {
            peer_mac,
            rssi: -50,
            frame_data: vec![0x04, 0x05, 0x06],
        }))
        .unwrap();
        server.write_all(&recv).await.unwrap();
    });

    // Create transport.
    let transport = UsbEspNowTransport::new(client, channel).await.unwrap();

    // Send a frame through the transport.
    transport
        .send(&[0x01, 0x02, 0x03], &peer_mac.to_vec())
        .await
        .unwrap();

    // Receive the response frame.
    let (recv_data, recv_peer) = transport.recv().await.unwrap();
    assert_eq!(recv_data, vec![0x04, 0x05, 0x06]);
    assert_eq!(recv_peer, peer_mac.to_vec());

    modem_task.await.unwrap();
}

// ===========================================================================
// APP_DATA tests
// ===========================================================================

/// T-E2E-031 — APP_DATA fire-and-forget.
///
/// A BPF program running on the node calls `send()` (helper 8) to transmit
/// an APP_DATA frame. The gateway accepts the frame (no handler configured,
/// so it is silently discarded). The node does not wait for a reply.
///
/// Uses the real `SondeBpfInterpreter` to execute BPF bytecode that calls
/// the `send()` helper, triggering the full APP_DATA dispatch path.
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_031_app_data_fire_and_forget() {
    use sonde_node::sonde_bpf_adapter::SondeBpfInterpreter;

    let env = E2eTestEnv::new();
    let psk = [0x31; 32];
    env.register_node("appdata-node", 1, psk).await;

    // Create a BPF program that calls helper 8 (send).
    let (program, hash) = make_send_program();
    env.storage.store_program(&program).await.unwrap();

    // Assign the program to the node.
    let mut node_rec = env.storage.get_node("appdata-node").await.unwrap().unwrap();
    node_rec.assigned_program_hash = Some(hash);
    env.storage.upsert_node(&node_rec).await.unwrap();

    // Use the real BPF interpreter.
    let mut node = NodeProxy::new(1, psk);
    let mut interpreter = SondeBpfInterpreter::new();
    let stats = node.run_wake_cycle_with(&env, &mut interpreter);

    assert_eq!(stats.outcome, WakeCycleOutcome::Sleep { seconds: 60 });

    // The node should have sent an APP_DATA frame (msg_type 0x04).
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

/// T-E2E-030 — APP_DATA round-trip with handler.
///
/// A BPF program calls `send_recv()` (helper 9) to send data and receive
/// a reply from a handler subprocess. The stub handler echoes back
/// `[0xCC, 0xDD]` for any incoming data.
///
/// Uses the real `SondeBpfInterpreter` and a real `HandlerRouter` wired to
/// the `stub_handler` binary built alongside this crate.
#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_030_app_data_round_trip() {
    use sonde_node::sonde_bpf_adapter::SondeBpfInterpreter;

    // Locate the stub_handler binary (built alongside the test).
    let stub = env!("CARGO_BIN_EXE_stub_handler");

    let env = E2eTestEnv::new_with_handler(stub, &[]);
    let psk = [0x30; 32];
    env.register_node("roundtrip-node", 1, psk).await;

    // Create a BPF program that calls helper 9 (send_recv).
    //
    // The program stores 2 bytes [0xAA, 0xBB] on the stack, calls
    // send_recv with a 64-byte reply buffer, and exits with the
    // helper return value (reply length or negative error).
    let bytecode = [
        // sth [r10-16], 0xBBAA — store send data
        0x6a, 0x0a, 0xf0, 0xff, 0xAA, 0xBB, 0x00, 0x00,
        // mov r1, r10; add r1, -16 — r1 = send ptr
        0xbf, 0xa1, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x07, 0x01, 0x00, 0x00, 0xf0, 0xff, 0xff,
        0xff, // mov r2, 2 — send len
        0xb7, 0x02, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00,
        // mov r3, r10; add r3, -256 — r3 = reply buf
        0xbf, 0xa3, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x07, 0x03, 0x00, 0x00, 0x00, 0xff, 0xff,
        0xff, // -256
        // mov r4, 64 — reply capacity
        0xb7, 0x04, 0x00, 0x00, 0x40, 0x00, 0x00, 0x00, // mov r5, 5000 — timeout ms
        0xb7, 0x05, 0x00, 0x00, 0x88, 0x13, 0x00, 0x00, // call 9 — helper_send_recv
        0x85, 0x00, 0x00, 0x00, 0x09, 0x00, 0x00, 0x00, // exit (r0 = reply_len or error)
        0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    let (program, hash) = make_program_from_bytecode(&bytecode, VerificationProfile::Resident);
    env.storage.store_program(&program).await.unwrap();

    // Assign the program and set it as current so the gateway knows the
    // running program hash (needed for handler routing).
    let mut node_rec = env
        .storage
        .get_node("roundtrip-node")
        .await
        .unwrap()
        .unwrap();
    node_rec.assigned_program_hash = Some(hash.clone());
    node_rec.current_program_hash = Some(hash);
    env.storage.upsert_node(&node_rec).await.unwrap();

    // Pre-load the program in node storage so the WAKE hash matches.
    let mut node = NodeProxy::new(1, psk);
    node.storage
        .write_program(0, &program.image)
        .expect("write program to node storage");

    let mut interpreter = SondeBpfInterpreter::new();
    let stats = node.run_wake_cycle_with(&env, &mut interpreter);

    assert_eq!(stats.outcome, WakeCycleOutcome::Sleep { seconds: 60 });

    // The node should have sent an APP_DATA frame.
    let app_data_count = stats
        .sent_frames
        .iter()
        .filter(|(t, _)| *t == sonde_protocol::MSG_APP_DATA)
        .count();
    assert_eq!(
        app_data_count, 1,
        "node should send exactly one APP_DATA frame"
    );

    // The gateway should have responded with APP_DATA_REPLY (response_count
    // increments for each non-None gateway response, which includes the
    // COMMAND and the APP_DATA_REPLY).
    assert!(
        stats.response_count >= 2,
        "gateway should produce at least 2 responses (COMMAND + APP_DATA_REPLY)"
    );
}
