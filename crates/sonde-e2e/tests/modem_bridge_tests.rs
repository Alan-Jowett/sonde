// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Modem-bridge end-to-end integration tests.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use sonde_e2e::harness::{E2eTestEnv, NodeProxy, RecordedNodeTransport, TestSha256};
use sonde_gateway::modem::UsbEspNowTransport;
use sonde_gateway::storage::Storage;
use sonde_gateway::transport::Transport;
use sonde_modem::bridge::{Bridge, Radio, SerialPort};
use sonde_modem::status::ModemCounters;
use sonde_node::error::{NodeError, NodeResult};
use sonde_node::traits::Transport as NodeTransport;
use sonde_node::wake_cycle::WakeCycleOutcome;
use sonde_protocol::modem::{RecvFrame, MAC_SIZE};
use sonde_protocol::{ProgramImage, Sha256Provider};
use tokio::io::{duplex, AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

use sonde_gateway::{ProgramRecord, VerificationProfile};

const TEST_CHANNEL: u8 = 6;
const MODEM_MAC: [u8; 6] = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
const NODE_MAC: [u8; 6] = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06];

struct BridgeHandle {
    stop: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
}

impl BridgeHandle {
    fn spawn<S, R>(mut bridge: Bridge<S, R>) -> Self
    where
        S: SerialPort + Send + 'static,
        R: Radio + Send + 'static,
    {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);
        let join = thread::spawn(move || {
            while !stop_thread.load(Ordering::Relaxed) {
                bridge.poll();
                thread::sleep(Duration::from_millis(1));
            }
        });
        Self {
            stop,
            join: Some(join),
        }
    }
}

impl Drop for BridgeHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

struct PipeSerial {
    incoming_rx: Receiver<Vec<u8>>,
    pending: VecDeque<u8>,
    outgoing_tx: UnboundedSender<Vec<u8>>,
    first_read: bool,
    _read_task: tokio::task::JoinHandle<()>,
    _write_task: tokio::task::JoinHandle<()>,
}

impl PipeSerial {
    fn new(stream: DuplexStream) -> Self {
        let (incoming_tx, incoming_rx) = mpsc::channel();
        let (outgoing_tx, mut outgoing_rx) = unbounded_channel::<Vec<u8>>();
        let (mut read_half, mut write_half) = tokio::io::split(stream);

        let read_task = tokio::spawn(async move {
            let mut buf = [0u8; 1024];
            loop {
                match read_half.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        if incoming_tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let write_task = tokio::spawn(async move {
            while let Some(data) = outgoing_rx.recv().await {
                if write_half.write_all(&data).await.is_err() {
                    break;
                }
                if write_half.flush().await.is_err() {
                    break;
                }
            }
        });

        Self {
            incoming_rx,
            pending: VecDeque::new(),
            outgoing_tx,
            first_read: true,
            _read_task: read_task,
            _write_task: write_task,
        }
    }

    fn drain_incoming(&mut self) {
        while let Ok(chunk) = self.incoming_rx.try_recv() {
            self.pending.extend(chunk);
        }
    }
}

impl SerialPort for PipeSerial {
    fn read(&mut self, buf: &mut [u8]) -> (usize, bool) {
        self.drain_incoming();

        if self.first_read {
            self.first_read = false;
        }
        let reconnected = false;

        let n = buf.len().min(self.pending.len());
        for slot in &mut buf[..n] {
            *slot = self.pending.pop_front().expect("pending length checked");
        }
        (n, reconnected)
    }

    fn write(&mut self, data: &[u8]) -> bool {
        self.outgoing_tx.send(data.to_vec()).is_ok()
    }

    fn is_connected(&self) -> bool {
        true
    }
}

struct ChannelRadio {
    tx: Sender<Vec<u8>>,
    rx: std::sync::Mutex<Receiver<Vec<u8>>>,
    channel: Arc<AtomicU8>,
}

impl ChannelRadio {
    fn new(tx: Sender<Vec<u8>>, rx: Receiver<Vec<u8>>, channel: Arc<AtomicU8>) -> Self {
        Self {
            tx,
            rx: std::sync::Mutex::new(rx),
            channel,
        }
    }
}

impl Radio for ChannelRadio {
    fn send(&mut self, _peer_mac: &[u8; MAC_SIZE], data: &[u8]) -> bool {
        self.tx.send(data.to_vec()).is_ok()
    }

    fn drain_one(&self) -> Option<RecvFrame> {
        match self.rx.lock().unwrap().try_recv() {
            Ok(frame_data) => Some(RecvFrame {
                peer_mac: NODE_MAC,
                rssi: -40,
                frame_data,
            }),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => None,
        }
    }

    fn set_channel(&mut self, channel: u8) -> Result<(), &'static str> {
        if !(1..=14).contains(&channel) {
            return Err("invalid channel");
        }
        self.channel.store(channel, Ordering::Relaxed);
        Ok(())
    }

    fn channel(&self) -> u8 {
        self.channel.load(Ordering::Relaxed)
    }

    fn scan_channels(&mut self) -> Vec<(u8, u8, i8)> {
        Vec::new()
    }

    fn mac_address(&self) -> [u8; MAC_SIZE] {
        MODEM_MAC
    }

    fn reset_state(&mut self) {}
}

struct ChannelTransport {
    tx: Sender<Vec<u8>>,
    rx: Receiver<Vec<u8>>,
    response_count: usize,
    wake_nonces: Vec<u64>,
    sent_frames: Vec<(u8, u64)>,
    sent_raw_frames: Vec<Vec<u8>>,
    received_msg_types: Vec<u8>,
}

impl ChannelTransport {
    fn new(tx: Sender<Vec<u8>>, rx: Receiver<Vec<u8>>) -> Self {
        Self {
            tx,
            rx,
            response_count: 0,
            wake_nonces: Vec::new(),
            sent_frames: Vec::new(),
            sent_raw_frames: Vec::new(),
            received_msg_types: Vec::new(),
        }
    }
}

impl NodeTransport for ChannelTransport {
    fn send(&mut self, frame: &[u8]) -> NodeResult<()> {
        if frame.len() >= sonde_protocol::HEADER_SIZE {
            let msg_type = frame[sonde_protocol::OFFSET_MSG_TYPE];
            let nonce = u64::from_be_bytes(
                frame[sonde_protocol::OFFSET_NONCE..sonde_protocol::OFFSET_NONCE + 8]
                    .try_into()
                    .expect("nonce slice must be 8 bytes"),
            );
            self.sent_frames.push((msg_type, nonce));
            if msg_type == sonde_protocol::MSG_WAKE {
                self.wake_nonces.push(nonce);
            }
            if msg_type == sonde_protocol::MSG_APP_DATA {
                self.sent_raw_frames.push(frame.to_vec());
            }
        }
        self.tx
            .send(frame.to_vec())
            .map_err(|_| NodeError::Transport("channel send failed"))?;
        Ok(())
    }

    fn recv(&mut self, timeout_ms: u32) -> NodeResult<Option<Vec<u8>>> {
        match self
            .rx
            .recv_timeout(Duration::from_millis(u64::from(timeout_ms)))
        {
            Ok(frame) => {
                self.response_count += 1;
                if frame.len() >= sonde_protocol::HEADER_SIZE {
                    self.received_msg_types
                        .push(frame[sonde_protocol::OFFSET_MSG_TYPE]);
                }
                Ok(Some(frame))
            }
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err(NodeError::Transport("channel recv failed")),
        }
    }
}

impl RecordedNodeTransport for ChannelTransport {
    fn response_count(&self) -> usize {
        self.response_count
    }

    fn wake_nonces(&self) -> &[u64] {
        &self.wake_nonces
    }

    fn sent_frames(&self) -> &[(u8, u64)] {
        &self.sent_frames
    }

    fn sent_raw_frames(&self) -> &[Vec<u8>] {
        &self.sent_raw_frames
    }

    fn received_msg_types(&self) -> &[u8] {
        &self.received_msg_types
    }
}

struct ModemBridgeEnv {
    e2e: E2eTestEnv,
    transport: Arc<UsbEspNowTransport>,
    node_transport: ChannelTransport,
    bridge: BridgeHandle,
    gateway_task: Option<tokio::task::JoinHandle<()>>,
    channel: Arc<AtomicU8>,
}

impl ModemBridgeEnv {
    async fn new() -> Self {
        let e2e = E2eTestEnv::new();
        let (gateway_client, gateway_server) = duplex(4096);
        let pipe_serial = PipeSerial::new(gateway_server);

        let (to_node_tx, to_node_rx) = mpsc::channel();
        let (to_bridge_tx, to_bridge_rx) = mpsc::channel();
        let channel = Arc::new(AtomicU8::new(1));
        let radio = ChannelRadio::new(to_node_tx, to_bridge_rx, Arc::clone(&channel));
        let bridge = Bridge::new(pipe_serial, radio, ModemCounters::new());

        let transport_task =
            tokio::spawn(
                async move { UsbEspNowTransport::new(gateway_client, TEST_CHANNEL).await },
            );

        tokio::time::sleep(Duration::from_millis(10)).await;
        let bridge = BridgeHandle::spawn(bridge);

        let transport = Arc::new(
            transport_task
                .await
                .expect("transport task must not panic")
                .expect("transport startup must succeed"),
        );

        Self {
            e2e,
            transport,
            node_transport: ChannelTransport::new(to_bridge_tx, to_node_rx),
            bridge,
            gateway_task: None,
            channel,
        }
    }

    fn start_gateway_loop(&mut self) {
        if self.gateway_task.is_some() {
            return;
        }

        let gateway = Arc::clone(&self.e2e.gateway);
        let transport = Arc::clone(&self.transport);
        self.gateway_task = Some(tokio::spawn(async move {
            loop {
                let (frame, peer, rssi) = match transport.recv_with_rssi().await {
                    Ok(msg) => msg,
                    Err(_) => break,
                };
                if let Some(response) = gateway
                    .process_frame_with_rssi(&frame, peer.clone(), Some(rssi))
                    .await
                {
                    if transport.send(&response, &peer).await.is_err() {
                        break;
                    }
                }
            }
        }));
    }
}

impl Drop for ModemBridgeEnv {
    fn drop(&mut self) {
        if let Some(task) = self.gateway_task.take() {
            task.abort();
        }
        let _ = &self.bridge;
    }
}

fn make_program_from_bytecode(bytecode: &[u8]) -> (ProgramRecord, Vec<u8>) {
    let image = ProgramImage {
        bytecode: bytecode.to_vec(),
        maps: vec![],
        map_initial_data: vec![],
    };
    let cbor = image
        .encode_deterministic()
        .expect("program image must encode");
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

fn make_send_program() -> (ProgramRecord, Vec<u8>) {
    let bytecode = [
        0x6a, 0x0a, 0xf8, 0xff, 0xAA, 0xBB, 0x00, 0x00, 0xbf, 0xa1, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x07, 0x01, 0x00, 0x00, 0xf8, 0xff, 0xff, 0xff, 0xb7, 0x02, 0x00, 0x00, 0x02, 0x00,
        0x00, 0x00, 0x85, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0xb7, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    make_program_from_bytecode(&bytecode)
}

#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_050_modem_startup_handshake() {
    let env = ModemBridgeEnv::new().await;

    assert_eq!(env.channel.load(Ordering::Relaxed), TEST_CHANNEL);
    assert_eq!(env.transport.modem_mac(), &MODEM_MAC);
}

#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_051_frame_round_trip_through_modem_bridge() {
    let mut env = ModemBridgeEnv::new().await;

    let outbound = vec![0x10, 0x20, 0x30, 0x40];
    env.node_transport
        .send(&outbound)
        .expect("node send through modem bridge must succeed");

    let (gateway_frame, peer) = env
        .transport
        .recv()
        .await
        .expect("gateway transport must receive node frame");
    assert_eq!(gateway_frame, outbound);
    assert_eq!(peer, NODE_MAC.to_vec());

    let response = vec![0xAA, 0xBB, 0xCC];
    env.transport
        .send(&response, &peer)
        .await
        .expect("gateway response through modem bridge must succeed");

    let node_frame = env
        .node_transport
        .recv(1_000)
        .expect("node recv must not error")
        .expect("node must receive gateway response");
    assert_eq!(node_frame, response);
}

#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_052_consecutive_wake_cycles_through_modem_bridge() {
    let mut env = ModemBridgeEnv::new().await;
    env.start_gateway_loop();

    let psk = [0x52; 32];
    env.e2e.register_node("modem-cycles", 1, psk).await;

    let mut node = NodeProxy::new(1, psk);

    let stats1 = node.run_wake_cycle_on(&mut env.node_transport);
    assert_eq!(stats1.outcome, WakeCycleOutcome::Sleep { seconds: 60 });

    let stats2 = node.run_wake_cycle_on(&mut env.node_transport);
    assert_eq!(stats2.outcome, WakeCycleOutcome::Sleep { seconds: 60 });

    assert!(!stats1.wake_nonces.is_empty(), "first cycle must send WAKE");
    assert!(
        !stats2.wake_nonces.is_empty(),
        "second cycle must send WAKE"
    );
    assert_ne!(stats1.wake_nonces[0], stats2.wake_nonces[0]);
}

#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_053_wrong_psk_through_modem_bridge() {
    let mut env = ModemBridgeEnv::new().await;
    env.start_gateway_loop();

    env.e2e
        .register_node("modem-wrong-psk", 1, [0xAA; 32])
        .await;

    let mut node = NodeProxy::new(1, [0xBB; 32]);
    let stats = node.run_wake_cycle_on(&mut env.node_transport);

    assert_eq!(stats.outcome, WakeCycleOutcome::Sleep { seconds: 60 });
    assert_eq!(stats.response_count, 0);
    assert!(env
        .e2e
        .gateway
        .session_manager()
        .get_last_seen("modem-wrong-psk")
        .await
        .is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn t_e2e_054_program_update_through_modem_bridge() {
    use sonde_node::sonde_bpf_adapter::SondeBpfInterpreter;

    let mut env = ModemBridgeEnv::new().await;
    env.start_gateway_loop();

    let psk = [0x54; 32];
    env.e2e.register_node("modem-program-update", 1, psk).await;

    let (program, hash) = make_send_program();
    env.e2e.storage.store_program(&program).await.unwrap();

    let mut node_record = env
        .e2e
        .storage
        .get_node("modem-program-update")
        .await
        .unwrap()
        .unwrap();
    node_record.assigned_program_hash = Some(hash.clone());
    env.e2e.storage.upsert_node(&node_record).await.unwrap();

    let mut node = NodeProxy::new(1, psk);
    let mut interpreter = SondeBpfInterpreter::new();
    let stats = node.run_wake_cycle_on_with(&mut env.node_transport, &mut interpreter);

    assert_eq!(stats.outcome, WakeCycleOutcome::Sleep { seconds: 60 });
    assert!(
        stats
            .sent_frames
            .iter()
            .any(|(msg_type, _)| *msg_type == sonde_protocol::MSG_GET_CHUNK),
        "node must request at least one chunk through the modem bridge"
    );
    assert_eq!(
        stats
            .sent_frames
            .iter()
            .filter(|(msg_type, _)| *msg_type == sonde_protocol::MSG_PROGRAM_ACK)
            .count(),
        1,
        "node must send exactly one PROGRAM_ACK through the modem bridge"
    );

    let updated = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let record = env
                .e2e
                .storage
                .get_node("modem-program-update")
                .await
                .unwrap()
                .unwrap();
            if record.current_program_hash.is_some() {
                break record;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("gateway must process PROGRAM_ACK promptly");
    assert_eq!(updated.current_program_hash, Some(hash));
}
