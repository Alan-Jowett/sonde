// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Bridge harness that wires gateway and node together via in-memory frame
//! queues for end-to-end integration testing.
//!
//! Two transport modes are available:
//!
//! - **Direct** (`BridgeTransport`): node calls `Gateway::process_frame`
//!   synchronously via `block_in_place`. Fast and deterministic.
//! - **Modem-bridged** (`ModemTestEnv` + `ChannelTransport`): frames flow
//!   through the real `sonde_modem::bridge::Bridge`, exercising the modem
//!   serial codec, peer table, and gateway `UsbEspNowTransport`.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::RwLock;

use sonde_gateway::engine::{Gateway, PendingCommand};
use sonde_gateway::handler::{HandlerConfig, HandlerRouter, ProgramMatcher};
use sonde_gateway::modem::UsbEspNowTransport;
use sonde_gateway::registry::NodeRecord;
use sonde_gateway::session::SessionManager;
use sonde_gateway::sqlite_storage::SqliteStorage;
use sonde_gateway::storage::Storage;
use sonde_gateway::transport::Transport as GatewayTransport;
use zeroize::Zeroizing;

use sonde_modem::bridge::{Bridge, Radio, SerialPort};
use sonde_modem::status::ModemCounters;

use sonde_node::bpf_helpers::SondeContext;
use sonde_node::bpf_runtime::{BpfError, BpfInterpreter, HelperFn};
use sonde_node::error::{NodeError, NodeResult};
use sonde_node::hal::{BatteryReader, Hal};
use sonde_node::map_storage::MapStorage;
use sonde_node::traits::{Clock, PlatformStorage, Rng, Transport as NodeTransport};
use sonde_node::wake_cycle::{run_wake_cycle, WakeCycleOutcome};

use sonde_protocol::modem::{RecvFrame, MAC_SIZE};
use sonde_protocol::{HmacProvider, Sha256Provider};

// ---------------------------------------------------------------------------
// E2eTestEnv — gateway-side test environment
// ---------------------------------------------------------------------------

/// Shared pending-command queue for test assertions.
pub type PendingCommandMap = Arc<RwLock<HashMap<String, Vec<PendingCommand>>>>;

/// Top-level test environment holding the gateway and its backing storage.
///
/// `pending_commands` is `Some` when created via [`new`] (shared with the
/// gateway), and `None` when created via [`new_with_handler`] (the gateway
/// owns its own internal command queue).
pub struct E2eTestEnv {
    pub gateway: Arc<Gateway>,
    pub storage: Arc<SqliteStorage>,
    pub pending_commands: Option<PendingCommandMap>,
}

impl Default for E2eTestEnv {
    fn default() -> Self {
        Self::new()
    }
}

impl E2eTestEnv {
    /// Create a fresh in-memory test environment.
    pub fn new() -> Self {
        let storage = Arc::new(
            SqliteStorage::in_memory(Zeroizing::new([0x42u8; 32]))
                .expect("failed to create in-memory SQLite storage"),
        );
        let session_manager = Arc::new(SessionManager::new(Duration::from_secs(30)));
        let pending_commands: PendingCommandMap = Arc::new(RwLock::new(HashMap::new()));
        let gateway = Arc::new(Gateway::new_with_pending(
            storage.clone(),
            pending_commands.clone(),
            session_manager,
        ));
        Self {
            gateway,
            storage,
            pending_commands: Some(pending_commands),
        }
    }

    /// Register a node in the gateway's storage.
    pub async fn register_node(&self, node_id: &str, key_hint: u16, psk: [u8; 32]) {
        let node = NodeRecord::new(node_id.into(), key_hint, psk);
        self.storage.upsert_node(&node).await.unwrap();
    }

    /// Create an environment with a handler router for APP_DATA tests.
    ///
    /// `handler_cmd` is the path to the handler binary and its arguments.
    pub fn new_with_handler(handler_cmd: &str, handler_args: &[&str]) -> Self {
        let storage = Arc::new(
            SqliteStorage::in_memory(Zeroizing::new([0x42u8; 32]))
                .expect("failed to create in-memory SQLite storage"),
        );
        let config = HandlerConfig {
            matchers: vec![ProgramMatcher::Any],
            command: handler_cmd.to_string(),
            args: handler_args.iter().map(|s| s.to_string()).collect(),
            reply_timeout: None,
        };
        let router = Arc::new(HandlerRouter::new(vec![config]));
        let gateway = Arc::new(Gateway::new_with_handler(
            storage.clone(),
            Duration::from_secs(30),
            router,
        ));
        Self {
            gateway,
            storage,
            pending_commands: None,
        }
    }
}

// ---------------------------------------------------------------------------
// NodeProxy — drives one node's wake cycle through the gateway
// ---------------------------------------------------------------------------

/// Statistics captured during a single wake cycle.
pub struct WakeCycleStats {
    /// The outcome returned by `run_wake_cycle`.
    pub outcome: WakeCycleOutcome,
    /// Number of non-`None` responses the gateway produced.
    pub response_count: usize,
    /// Nonces from WAKE frames the node sent during this cycle.
    pub wake_nonces: Vec<u64>,
    /// `(msg_type, nonce)` for every frame the node sent.
    pub sent_frames: Vec<(u8, u64)>,
}

/// Lightweight handle representing a remote node.
///
/// Identity and schedule are stored exclusively in `self.storage`
/// (`PlatformStorage`) — the wake cycle reads them from there.
pub struct NodeProxy {
    pub mac: Vec<u8>,
    pub storage: MockNodeStorage,
    pub map_storage: MapStorage,
    rng: MockRng,
}

impl NodeProxy {
    pub fn new(key_hint: u16, psk: [u8; 32]) -> Self {
        Self {
            mac: vec![0x01, 0x02, 0x03, 0x04, 0x05, 0x06],
            storage: MockNodeStorage::new_paired(key_hint, psk, 60),
            map_storage: MapStorage::new(4096),
            rng: MockRng(0),
        }
    }

    /// Create an unpaired node (no key material in storage).
    pub fn new_unpaired() -> Self {
        Self {
            mac: vec![0x01, 0x02, 0x03, 0x04, 0x05, 0x06],
            storage: MockNodeStorage::new_unpaired(),
            map_storage: MapStorage::new(4096),
            rng: MockRng(0),
        }
    }

    /// Create a BLE-provisioned node (PSK + peer_payload, not yet registered).
    ///
    /// Simulates a node that has completed BLE provisioning (Phase 2) and
    /// has key material + encrypted payload stored, but has not yet run
    /// the PEER_REQUEST/PEER_ACK exchange (reg_complete = false).
    pub fn new_ble_provisioned(
        key_hint: u16,
        psk: [u8; 32],
        channel: u8,
        peer_payload: Vec<u8>,
    ) -> Self {
        Self {
            mac: vec![0x01, 0x02, 0x03, 0x04, 0x05, 0x06],
            storage: MockNodeStorage::new_ble_provisioned(key_hint, psk, channel, peer_payload),
            map_storage: MapStorage::new(4096),
            rng: MockRng(0),
        }
    }

    /// Run one full wake cycle, relaying every frame through the real gateway.
    ///
    /// Uses `block_in_place` so the synchronous node code can call the async
    /// gateway inline. `block_in_place` requires a multi-thread tokio runtime.
    /// All E2E tests must use `#[tokio::test(flavor = "multi_thread")]`.
    ///
    /// Returns [`WakeCycleStats`] with the outcome, response count, and
    /// captured WAKE nonces for test assertions.
    pub fn run_wake_cycle(&mut self, env: &E2eTestEnv) -> WakeCycleStats {
        let mut interpreter = MockBpfInterpreter::new();
        self.run_wake_cycle_with(env, &mut interpreter)
    }

    /// Like [`run_wake_cycle`] but accepts a caller-supplied BPF interpreter.
    ///
    /// Use this with [`sonde_node::sonde_bpf_adapter::SondeBpfInterpreter`] when the
    /// test requires real BPF program execution (e.g. APP_DATA helpers).
    pub fn run_wake_cycle_with(
        &mut self,
        env: &E2eTestEnv,
        interpreter: &mut impl BpfInterpreter,
    ) -> WakeCycleStats {
        let mut hal = MockHal;
        let clock = MockClock::new();
        let battery = MockBattery;
        let hmac = TestHmac;
        let sha = TestSha256;

        let mut transport = BridgeTransport::new(env.gateway.clone(), self.mac.clone());

        let outcome = run_wake_cycle(
            &mut transport,
            &mut self.storage,
            &mut hal,
            &mut self.rng,
            &clock,
            &battery,
            interpreter,
            &mut self.map_storage,
            &hmac,
            &sha,
        );
        WakeCycleStats {
            outcome,
            response_count: transport.response_count(),
            wake_nonces: transport.wake_nonces().to_vec(),
            sent_frames: transport.sent_frames().to_vec(),
        }
    }

    /// Run one wake cycle through the real modem bridge.
    ///
    /// Frames flow: node → ChannelTransport → mpsc → ChannelRadio →
    /// Bridge → PipeSerial → duplex → UsbEspNowTransport → Gateway
    /// and back.
    ///
    /// A gateway frame pump runs concurrently via a tokio task.
    /// The node wake cycle runs via `block_in_place` since it is
    /// synchronous (uses `mpsc::recv_timeout` internally).
    pub async fn run_wake_cycle_bridged(
        &mut self,
        env: &ModemTestEnv,
        transport: &mut ChannelTransport,
    ) -> WakeCycleStats {
        transport.reset_stats();

        let pump_stop = Arc::new(AtomicBool::new(false));
        let pump = tokio::spawn(run_gateway_pump(
            env.gateway.clone(),
            env.transport.clone(),
            Arc::clone(&pump_stop),
        ));

        let mut hal = MockHal;
        let clock = MockClock::new();
        let battery = MockBattery;
        let hmac = TestHmac;
        let sha = TestSha256;
        let mut interpreter = MockBpfInterpreter::new();

        let outcome = tokio::task::block_in_place(|| {
            run_wake_cycle(
                transport,
                &mut self.storage,
                &mut hal,
                &mut self.rng,
                &clock,
                &battery,
                &mut interpreter,
                &mut self.map_storage,
                &hmac,
                &sha,
            )
        });

        pump_stop.store(true, Ordering::Relaxed);
        pump.await.expect("gateway pump task panicked");

        WakeCycleStats {
            outcome,
            response_count: 0, // Not tracked in bridged mode
            wake_nonces: transport.wake_nonces().to_vec(),
            sent_frames: transport.sent_frames().to_vec(),
        }
    }
}

// ---------------------------------------------------------------------------
// BridgeTransport — relays node frames through the gateway
// ---------------------------------------------------------------------------

/// In-memory frame relay between a node and the gateway.
///
/// Note: `block_in_place` requires `#[tokio::test(flavor = "multi_thread")]`.
struct BridgeTransport {
    gateway: Arc<Gateway>,
    peer: Vec<u8>,
    pending_response: Option<Vec<u8>>,
    response_count: usize,
    /// Nonces extracted from outbound WAKE frames.
    wake_nonces: Vec<u64>,
    /// `(msg_type, nonce)` for every outbound frame.
    sent_frames: Vec<(u8, u64)>,
    rt: tokio::runtime::Handle,
}

impl BridgeTransport {
    fn new(gateway: Arc<Gateway>, peer: Vec<u8>) -> Self {
        Self {
            gateway,
            peer,
            pending_response: None,
            response_count: 0,
            wake_nonces: Vec::new(),
            sent_frames: Vec::new(),
            rt: tokio::runtime::Handle::try_current()
                .expect("BridgeTransport must be created inside a Tokio runtime"),
        }
    }

    /// Number of non-`None` responses the gateway returned during this cycle.
    fn response_count(&self) -> usize {
        self.response_count
    }

    /// Nonces from WAKE frames sent during this cycle.
    fn wake_nonces(&self) -> &[u64] {
        &self.wake_nonces
    }

    /// `(msg_type, nonce)` for every outbound frame.
    fn sent_frames(&self) -> &[(u8, u64)] {
        &self.sent_frames
    }
}

impl NodeTransport for BridgeTransport {
    fn send(&mut self, frame: &[u8]) -> NodeResult<()> {
        // Capture header metadata from every outbound frame.
        if frame.len() >= sonde_protocol::HEADER_SIZE {
            let msg_type = frame[sonde_protocol::OFFSET_MSG_TYPE];
            let nonce_end = sonde_protocol::OFFSET_NONCE + 8;
            let nonce = u64::from_be_bytes(
                frame[sonde_protocol::OFFSET_NONCE..nonce_end]
                    .try_into()
                    .unwrap(),
            );
            self.sent_frames.push((msg_type, nonce));
            if msg_type == sonde_protocol::MSG_WAKE {
                self.wake_nonces.push(nonce);
            }
        }

        let gateway = self.gateway.clone();
        let peer = self.peer.clone();
        let frame = frame.to_vec();
        let response =
            tokio::task::block_in_place(|| self.rt.block_on(gateway.process_frame(&frame, peer)));
        if response.is_some() {
            self.response_count += 1;
        }
        self.pending_response = response;
        Ok(())
    }

    /// Returns the response captured by the preceding `send()` call.
    /// Timeout is not simulated because `send()` synchronously processes
    /// the frame through the gateway and captures any response. This is
    /// correct for the request-response pattern used by the wake cycle
    /// (send WAKE → recv COMMAND, send GET_CHUNK → recv CHUNK).
    fn recv(&mut self, _timeout_ms: u32) -> NodeResult<Option<Vec<u8>>> {
        Ok(self.pending_response.take())
    }
}

// ---------------------------------------------------------------------------
// ChannelRadio — mpsc-backed Radio trait for modem bridge tests
// ---------------------------------------------------------------------------

/// Simulates ESP-NOW radio between a modem bridge and a single node.
///
/// Uses `std::sync::mpsc` (not tokio) because `Radio::drain_one` takes
/// `&self` and `Radio::send` takes `&mut self` — both synchronous.
struct ChannelRadio {
    /// Frames sent by the bridge (gateway → node) arrive at the node.
    to_node: std::sync::mpsc::SyncSender<Vec<u8>>,
    /// Shared receiver for bridge→node frames, used by reset to drain stale frames.
    to_node_rx: Arc<Mutex<std::sync::mpsc::Receiver<Vec<u8>>>>,
    /// Frames sent by the node arrive here for the bridge.
    from_node: Mutex<std::sync::mpsc::Receiver<Vec<u8>>>,
    /// Current radio channel.
    channel: u8,
    /// Fixed MAC address for the simulated modem.
    mac: [u8; MAC_SIZE],
    /// MAC address of the simulated node, used as `peer_mac` in received frames.
    node_mac: [u8; MAC_SIZE],
}

impl Radio for ChannelRadio {
    fn send(&mut self, peer_mac: &[u8; MAC_SIZE], data: &[u8]) -> bool {
        assert_eq!(
            peer_mac, &self.node_mac,
            "ChannelRadio: send to unexpected peer MAC {:02x?}, expected {:02x?}",
            peer_mac, self.node_mac
        );
        use std::sync::mpsc::TrySendError;
        match self.to_node.try_send(data.to_vec()) {
            Ok(()) => true,
            // Node receiver dropped during teardown — harmless.
            Err(TrySendError::Disconnected(_)) => false,
            // Channel full — fail fast rather than deadlocking the bridge thread.
            Err(TrySendError::Full(_)) => {
                panic!("ChannelRadio: bridge→node channel full (cap 64); node is not draining")
            }
        }
    }

    fn drain_one(&self) -> Option<RecvFrame> {
        let rx = self.from_node.lock().unwrap();
        match rx.try_recv() {
            Ok(data) => Some(RecvFrame {
                peer_mac: self.node_mac,
                rssi: -40,
                frame_data: data,
            }),
            Err(_) => None,
        }
    }

    fn set_channel(&mut self, channel: u8) -> Result<(), &'static str> {
        if channel == 0 || channel > 14 {
            return Err("invalid channel");
        }
        self.channel = channel;
        Ok(())
    }

    fn channel(&self) -> u8 {
        self.channel
    }

    fn scan_channels(&mut self) -> Vec<(u8, u8, i8)> {
        vec![]
    }

    fn mac_address(&self) -> [u8; MAC_SIZE] {
        self.mac
    }

    fn reset_state(&mut self) {
        self.channel = 1;
        // Drain node→bridge frames.
        {
            let rx = self.from_node.lock().unwrap();
            while rx.try_recv().is_ok() {}
        }
        // Drain bridge→node frames so stale data from a previous cycle
        // is not delivered after RESET.
        {
            let rx = self.to_node_rx.lock().unwrap();
            while rx.try_recv().is_ok() {}
        }
    }
}

// ---------------------------------------------------------------------------
// ChannelTransport — mpsc-backed node Transport for modem bridge tests
// ---------------------------------------------------------------------------

/// Node-side transport backed by the same mpsc channels as `ChannelRadio`.
///
/// Uses `std::sync::mpsc::recv_timeout()` to implement the synchronous
/// `sonde_node::traits::Transport::recv(timeout_ms)` contract.
pub struct ChannelTransport {
    rx: Arc<Mutex<std::sync::mpsc::Receiver<Vec<u8>>>>,
    tx: std::sync::mpsc::SyncSender<Vec<u8>>,
    /// Nonces extracted from outbound WAKE frames.
    wake_nonces: Vec<u64>,
    /// `(msg_type, nonce)` for every outbound frame.
    sent_frames: Vec<(u8, u64)>,
}

impl ChannelTransport {
    /// Reset per-cycle tracking counters and drain any stale inbound
    /// frames so they don't leak into the next wake cycle.
    pub fn reset_stats(&mut self) {
        self.wake_nonces.clear();
        self.sent_frames.clear();
        let rx = self.rx.lock().unwrap();
        while rx.try_recv().is_ok() {}
    }

    /// Nonces from WAKE frames sent during the last cycle.
    pub fn wake_nonces(&self) -> &[u64] {
        &self.wake_nonces
    }

    /// `(msg_type, nonce)` for every outbound frame.
    pub fn sent_frames(&self) -> &[(u8, u64)] {
        &self.sent_frames
    }
}

impl NodeTransport for ChannelTransport {
    fn send(&mut self, frame: &[u8]) -> NodeResult<()> {
        // Capture header metadata (same as BridgeTransport).
        if frame.len() >= sonde_protocol::HEADER_SIZE {
            let msg_type = frame[sonde_protocol::OFFSET_MSG_TYPE];
            let nonce_end = sonde_protocol::OFFSET_NONCE + 8;
            let nonce = u64::from_be_bytes(
                frame[sonde_protocol::OFFSET_NONCE..nonce_end]
                    .try_into()
                    .unwrap(),
            );
            self.sent_frames.push((msg_type, nonce));
            if msg_type == sonde_protocol::MSG_WAKE {
                self.wake_nonces.push(nonce);
            }
        }

        use std::sync::mpsc::TrySendError;
        match self.tx.try_send(frame.to_vec()) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(NodeError::Transport(
                "node→bridge channel full (cap 64); bridge is not draining",
            )),
            Err(TrySendError::Disconnected(..)) => {
                Err(NodeError::Transport("channel disconnected"))
            }
        }
    }

    fn recv(&mut self, timeout_ms: u32) -> NodeResult<Option<Vec<u8>>> {
        let rx = self.rx.lock().unwrap();
        match rx.recv_timeout(Duration::from_millis(timeout_ms as u64)) {
            Ok(data) => Ok(Some(data)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                Err(NodeError::Transport("channel disconnected"))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// PipeSerial — bridges sync SerialPort to async tokio::io::duplex
// ---------------------------------------------------------------------------

/// Safety cap on PipeSerial rx_buf to prevent unbounded growth if the
/// gateway side writes faster than the bridge thread drains.
const MAX_RX_BUF: usize = 64 * 1024;

/// Adapter that implements the synchronous `SerialPort` trait backed by
/// shared ring buffers. A background tokio task shuttles bytes between
/// the duplex stream and these buffers.
///
/// The `reconnected` flag from `read()` is always `false` — the bridge
/// receives its initial `MODEM_READY` trigger from the gateway's RESET
/// command rather than a simulated USB reconnect event.
struct PipeSerial {
    rx_buf: Arc<Mutex<VecDeque<u8>>>,
    tx_buf: Arc<Mutex<VecDeque<u8>>>,
    tx_notify: Arc<tokio::sync::Notify>,
    connected: Arc<AtomicBool>,
}

impl SerialPort for PipeSerial {
    fn read(&mut self, buf: &mut [u8]) -> (usize, bool) {
        let mut rx = self.rx_buf.lock().unwrap();
        let n = buf.len().min(rx.len());
        for b in buf.iter_mut().take(n) {
            *b = rx.pop_front().unwrap();
        }
        (n, false)
    }

    /// Write is bounded by `MAX_TX_BUF` and panics on overflow so tests
    /// fail loudly instead of silently dropping bridge messages.
    /// Returns `false` if the duplex has been closed (i.e. `is_connected()`
    /// is already `false`), so the bridge observes write failure promptly.
    fn write(&mut self, data: &[u8]) -> bool {
        if !self.is_connected() {
            return false;
        }
        {
            let mut tx = self.tx_buf.lock().unwrap();
            const MAX_TX_BUF: usize = 64 * 1024;
            assert!(
                tx.len() + data.len() <= MAX_TX_BUF,
                "PipeSerial tx_buf exceeded {MAX_TX_BUF} bytes — \
                 bridge is writing faster than the duplex drains"
            );
            tx.extend(data);
        }
        self.tx_notify.notify_one();
        true
    }

    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }
}

/// Create a `PipeSerial` and spawn a background tokio task that shuttles
/// bytes between the duplex server half and the adapter's ring buffers.
fn create_pipe_serial(
    server: tokio::io::DuplexStream,
    stop: Arc<AtomicBool>,
) -> (PipeSerial, tokio::task::JoinHandle<()>) {
    let rx_buf: Arc<Mutex<VecDeque<u8>>> = Arc::new(Mutex::new(VecDeque::new()));
    let tx_buf: Arc<Mutex<VecDeque<u8>>> = Arc::new(Mutex::new(VecDeque::new()));
    let tx_notify = Arc::new(tokio::sync::Notify::new());
    let connected = Arc::new(AtomicBool::new(true));

    let pipe = PipeSerial {
        rx_buf: Arc::clone(&rx_buf),
        tx_buf: Arc::clone(&tx_buf),
        tx_notify: Arc::clone(&tx_notify),
        connected: Arc::clone(&connected),
    };

    let handle = {
        let rx_buf = Arc::clone(&rx_buf);
        let tx_buf = Arc::clone(&tx_buf);
        let tx_notify = Arc::clone(&tx_notify);

        tokio::spawn(async move {
            let (mut reader, mut writer) = tokio::io::split(server);
            let mut read_buf = [0u8; 1024];

            loop {
                if stop.load(Ordering::Relaxed) {
                    break;
                }

                tokio::select! {
                    result = reader.read(&mut read_buf) => {
                        match result {
                            Ok(0) => {
                                connected.store(false, Ordering::Relaxed);
                                stop.store(true, Ordering::Relaxed);
                                break;
                            }
                            Ok(n) => {
                                let mut rx = rx_buf.lock().unwrap();
                                assert!(
                                    rx.len() + n <= MAX_RX_BUF,
                                    "PipeSerial rx_buf exceeded {MAX_RX_BUF} bytes — \
                                     gateway is writing faster than bridge drains"
                                );
                                rx.extend(&read_buf[..n]);
                            }
                            Err(e) => {
                                connected.store(false, Ordering::Relaxed);
                                stop.store(true, Ordering::Relaxed);
                                panic!("PipeSerial: duplex read error: {e}");
                            }
                        }
                    }
                    _ = tx_notify.notified() => {
                        let data: Vec<u8> = {
                            let mut tx = tx_buf.lock().unwrap();
                            tx.drain(..).collect()
                        };
                        if !data.is_empty() {
                            if let Err(e) = writer.write_all(&data).await {
                                connected.store(false, Ordering::Relaxed);
                                stop.store(true, Ordering::Relaxed);
                                panic!("PipeSerial: duplex write error: {e}");
                            }
                            if let Err(e) = writer.flush().await {
                                connected.store(false, Ordering::Relaxed);
                                stop.store(true, Ordering::Relaxed);
                                panic!("PipeSerial: duplex flush error: {e}");
                            }
                        }
                    }
                    // Periodic stop-flag check so the task can shut down
                    // gracefully without relying on abort().
                    _ = tokio::time::sleep(Duration::from_millis(50)) => {}
                }
            }
        })
    };

    (pipe, handle)
}

// ---------------------------------------------------------------------------
// ModemTestEnv — modem-in-loop test environment
// ---------------------------------------------------------------------------

/// Test environment that wires the real modem `Bridge` between gateway
/// and node using in-memory channels and duplex streams.
///
/// ```text
/// Node ←(mpsc)→ ChannelRadio ←→ Bridge ←(PipeSerial/duplex)→ UsbEspNowTransport ←→ Gateway
/// ```
pub struct ModemTestEnv {
    pub gateway: Arc<Gateway>,
    pub storage: Arc<SqliteStorage>,
    pub transport: Arc<UsbEspNowTransport>,
    pub pending_commands: Option<PendingCommandMap>,
    stop: Arc<AtomicBool>,
    bridge_thread: Option<std::thread::JoinHandle<()>>,
    pipe_task: Option<tokio::task::JoinHandle<()>>,
}

impl ModemTestEnv {
    /// Create a modem-in-loop test environment.
    ///
    /// Spawns the real `Bridge` in a dedicated thread and connects it to
    /// `UsbEspNowTransport` via `tokio::io::duplex`. Returns both the
    /// environment and a `ChannelTransport` for the node to use.
    pub async fn new(channel: u8) -> (Self, ChannelTransport) {
        let stop = Arc::new(AtomicBool::new(false));

        // Bounded channels for radio simulation (bridge ↔ node).
        // Capacity of 64 frames provides backpressure and prevents OOM
        // if either side stalls.
        let (bridge_to_node_tx, bridge_to_node_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(64);
        let (node_to_bridge_tx, node_to_bridge_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(64);

        // Share the bridge→node receiver so both ChannelRadio::reset_state
        // and ChannelTransport::recv can access it.
        let to_node_rx = Arc::new(Mutex::new(bridge_to_node_rx));

        let channel_radio = ChannelRadio {
            to_node: bridge_to_node_tx,
            to_node_rx: Arc::clone(&to_node_rx),
            from_node: Mutex::new(node_to_bridge_rx),
            channel: 1,
            mac: [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF],
            node_mac: [0x01, 0x02, 0x03, 0x04, 0x05, 0x06],
        };

        let channel_transport = ChannelTransport {
            rx: to_node_rx,
            tx: node_to_bridge_tx,
            wake_nonces: Vec::new(),
            sent_frames: Vec::new(),
        };

        // Duplex stream for serial link (gateway transport ↔ bridge).
        let (client, server) = tokio::io::duplex(4096);

        // Create PipeSerial adapter with background task.
        let (pipe_serial, pipe_task) = create_pipe_serial(server, Arc::clone(&stop));

        // Start bridge in a dedicated thread.
        let bridge_stop = Arc::clone(&stop);
        let bridge_thread = std::thread::spawn(move || {
            let counters = ModemCounters::new();
            let mut bridge = Bridge::new(pipe_serial, channel_radio, counters);
            while !bridge_stop.load(Ordering::Relaxed) {
                bridge.poll();
                // 2ms poll interval — fast enough for E2E test latency
                // requirements while avoiding tight CPU-burning loops.
                std::thread::sleep(Duration::from_millis(2));
            }
        });

        // Create UsbEspNowTransport — performs startup handshake.
        // If the handshake fails, signal the bridge thread and pipe task to
        // stop so they don't leak.
        let transport = match UsbEspNowTransport::new(client, channel).await {
            Ok(t) => Arc::new(t),
            Err(e) => {
                stop.store(true, Ordering::Relaxed);
                pipe_task.abort();
                // Surface bridge thread panics (e.g. channel overflow
                // assertions) so the real root cause is not hidden behind
                // the handshake error.
                if let Err(panic_payload) = bridge_thread.join() {
                    std::panic::resume_unwind(panic_payload);
                }
                panic!("modem startup handshake failed: {e}");
            }
        };

        // Create gateway engine.
        let storage = Arc::new(
            SqliteStorage::in_memory(Zeroizing::new([0x42u8; 32]))
                .expect("failed to create in-memory SQLite storage"),
        );
        let session_manager = Arc::new(SessionManager::new(Duration::from_secs(30)));
        let pending_commands: PendingCommandMap = Arc::new(RwLock::new(HashMap::new()));
        let gateway = Arc::new(Gateway::new_with_pending(
            storage.clone(),
            pending_commands.clone(),
            session_manager,
        ));

        let env = Self {
            gateway,
            storage,
            transport,
            pending_commands: Some(pending_commands),
            stop,
            bridge_thread: Some(bridge_thread),
            pipe_task: Some(pipe_task),
        };

        (env, channel_transport)
    }

    /// Register a node in the gateway's storage.
    pub async fn register_node(&self, node_id: &str, key_hint: u16, psk: [u8; 32]) {
        let node = NodeRecord::new(node_id.into(), key_hint, psk);
        self.storage.upsert_node(&node).await.unwrap();
    }
}

impl ModemTestEnv {
    /// Gracefully shut down the modem environment, surfacing any panics
    /// from the pipe shuttle task or bridge thread.
    ///
    /// Tests should call this instead of relying on `Drop` so that task
    /// panics (e.g. from the `assert!` caps or I/O errors) propagate as
    /// test failures rather than being silently swallowed by `abort()`.
    pub async fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.pipe_task.take() {
            // Await (not abort) so panics propagate to the test.
            match handle.await {
                Ok(()) => {}
                Err(e) if e.is_cancelled() => {}
                Err(e) => std::panic::resume_unwind(e.into_panic()),
            }
        }
        if let Some(handle) = self.bridge_thread.take() {
            handle.join().expect("bridge thread panicked");
        }
    }
}

impl Drop for ModemTestEnv {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Abort the pipe shuttle task — `Drop` cannot `.await` so abort is
        // the only option here. Tests should call `shutdown().await` first
        // to surface panics; this is a safety-net for cleanup only.
        if let Some(handle) = self.pipe_task.take() {
            handle.abort();
        }
        if let Some(handle) = self.bridge_thread.take() {
            if std::thread::panicking() {
                let _ = handle.join();
            } else {
                handle.join().expect("bridge thread panicked");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Gateway frame pump — drives gateway recv/process/send loop
// ---------------------------------------------------------------------------

/// Run a gateway frame-processing loop over the modem transport.
///
/// Receives frames from the transport, processes them through the gateway
/// engine, and sends responses back. Runs until `stop` is set.
async fn run_gateway_pump(
    gateway: Arc<Gateway>,
    transport: Arc<UsbEspNowTransport>,
    stop: Arc<AtomicBool>,
) {
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        match tokio::time::timeout(
            Duration::from_millis(50),
            GatewayTransport::recv(&*transport),
        )
        .await
        {
            Ok(Ok((frame, peer))) => {
                if let Some(response) = gateway.process_frame(&frame, peer.clone()).await {
                    GatewayTransport::send(&*transport, &response, &peer)
                        .await
                        .expect("gateway pump: transport send failed");
                }
            }
            Ok(Err(_)) if stop.load(Ordering::Relaxed) => break,
            Ok(Err(e)) => panic!("gateway pump: transport recv failed: {e:?}"),
            Err(_) => {} // timeout — keep looping
        }
    }
}

// ---------------------------------------------------------------------------
// Crypto providers (real HMAC-SHA256 / SHA-256)
// ---------------------------------------------------------------------------

pub struct TestHmac;

impl HmacProvider for TestHmac {
    fn compute(&self, key: &[u8], data: &[u8]) -> [u8; 32] {
        use hmac::Mac;
        let mut mac =
            hmac::Hmac::<sha2::Sha256>::new_from_slice(key).expect("HMAC key length error");
        mac.update(data);
        mac.finalize().into_bytes().into()
    }

    fn verify(&self, key: &[u8], data: &[u8], expected: &[u8; 32]) -> bool {
        use hmac::Mac;
        let mut mac =
            hmac::Hmac::<sha2::Sha256>::new_from_slice(key).expect("HMAC key length error");
        mac.update(data);
        mac.verify_slice(expected).is_ok()
    }
}

pub struct TestSha256;

impl Sha256Provider for TestSha256 {
    fn hash(&self, data: &[u8]) -> [u8; 32] {
        use sha2::Digest;
        sha2::Sha256::digest(data).into()
    }
}

// ---------------------------------------------------------------------------
// Node-side mocks (derived from sonde-node wake_cycle.rs test mocks)
// ---------------------------------------------------------------------------

pub struct MockNodeStorage {
    key: Option<(u16, [u8; 32])>,
    schedule_interval: u32,
    active_partition: u8,
    programs: [Option<Vec<u8>>; 2],
    early_wake_flag: bool,
    channel: Option<u8>,
    peer_payload: Option<Vec<u8>>,
    reg_complete: bool,
}

impl MockNodeStorage {
    pub fn new_paired(key_hint: u16, psk: [u8; 32], schedule_interval_s: u32) -> Self {
        Self {
            key: Some((key_hint, psk)),
            schedule_interval: schedule_interval_s,
            active_partition: 0,
            programs: [None, None],
            early_wake_flag: false,
            channel: None,
            peer_payload: None,
            reg_complete: false,
        }
    }

    /// Create unpaired storage (no key material) — simulates a freshly
    /// flashed node that has never been paired.
    pub fn new_unpaired() -> Self {
        Self {
            key: None,
            schedule_interval: 60,
            active_partition: 0,
            programs: [None, None],
            early_wake_flag: false,
            channel: None,
            peer_payload: None,
            reg_complete: false,
        }
    }

    /// Create BLE-provisioned storage (PSK + encrypted payload, not yet
    /// registered with the gateway).
    pub fn new_ble_provisioned(
        key_hint: u16,
        psk: [u8; 32],
        channel: u8,
        peer_payload: Vec<u8>,
    ) -> Self {
        Self {
            key: Some((key_hint, psk)),
            schedule_interval: 60,
            active_partition: 0,
            programs: [None, None],
            early_wake_flag: false,
            channel: Some(channel),
            peer_payload: Some(peer_payload),
            reg_complete: false,
        }
    }
}

impl PlatformStorage for MockNodeStorage {
    fn read_key(&self) -> Option<(u16, [u8; 32])> {
        self.key
    }
    fn write_key(&mut self, key_hint: u16, psk: &[u8; 32]) -> NodeResult<()> {
        if self.key.is_some() {
            return Err(sonde_node::error::NodeError::StorageError("already paired"));
        }
        self.key = Some((key_hint, *psk));
        Ok(())
    }
    fn erase_key(&mut self) -> NodeResult<()> {
        self.key = None;
        Ok(())
    }
    fn read_schedule(&self) -> (u32, u8) {
        (self.schedule_interval, self.active_partition)
    }
    fn write_schedule_interval(&mut self, interval_s: u32) -> NodeResult<()> {
        self.schedule_interval = interval_s;
        Ok(())
    }
    fn write_active_partition(&mut self, partition: u8) -> NodeResult<()> {
        if partition > 1 {
            return Err(sonde_node::error::NodeError::StorageError(
                "partition must be 0 or 1",
            ));
        }
        self.active_partition = partition;
        Ok(())
    }
    fn reset_schedule(&mut self) -> NodeResult<()> {
        self.schedule_interval = 60;
        self.active_partition = 0;
        Ok(())
    }
    fn read_program(&self, partition: u8) -> Option<Vec<u8>> {
        self.programs.get(partition as usize).cloned().flatten()
    }
    fn write_program(&mut self, partition: u8, image: &[u8]) -> NodeResult<()> {
        if (partition as usize) >= self.programs.len() {
            return Err(sonde_node::error::NodeError::StorageError(
                "invalid partition",
            ));
        }
        self.programs[partition as usize] = Some(image.to_vec());
        Ok(())
    }
    fn erase_program(&mut self, partition: u8) -> NodeResult<()> {
        if (partition as usize) >= self.programs.len() {
            return Err(sonde_node::error::NodeError::StorageError(
                "invalid partition",
            ));
        }
        self.programs[partition as usize] = None;
        Ok(())
    }
    fn take_early_wake_flag(&mut self) -> bool {
        let v = self.early_wake_flag;
        self.early_wake_flag = false;
        v
    }
    fn set_early_wake_flag(&mut self) -> NodeResult<()> {
        self.early_wake_flag = true;
        Ok(())
    }

    fn read_channel(&self) -> Option<u8> {
        self.channel
    }
    fn write_channel(&mut self, channel: u8) -> NodeResult<()> {
        self.channel = Some(channel);
        Ok(())
    }
    fn read_peer_payload(&self) -> Option<Vec<u8>> {
        self.peer_payload.clone()
    }
    fn has_peer_payload(&self) -> bool {
        self.peer_payload.is_some()
    }
    fn write_peer_payload(&mut self, payload: &[u8]) -> NodeResult<()> {
        self.peer_payload = Some(payload.to_vec());
        Ok(())
    }
    fn erase_peer_payload(&mut self) -> NodeResult<()> {
        self.peer_payload = None;
        Ok(())
    }
    fn read_reg_complete(&self) -> bool {
        self.reg_complete
    }
    fn write_reg_complete(&mut self, complete: bool) -> NodeResult<()> {
        self.reg_complete = complete;
        Ok(())
    }
}

struct MockHal;

impl Hal for MockHal {
    fn i2c_read(&mut self, _h: u32, _buf: &mut [u8]) -> i32 {
        0
    }
    fn i2c_write(&mut self, _h: u32, _data: &[u8]) -> i32 {
        0
    }
    fn i2c_write_read(&mut self, _h: u32, _w: &[u8], _r: &mut [u8]) -> i32 {
        0
    }
    fn spi_transfer(
        &mut self,
        _h: u32,
        _tx: Option<&[u8]>,
        _rx: Option<&mut [u8]>,
        _l: usize,
    ) -> i32 {
        0
    }
    fn gpio_read(&self, _pin: u32) -> i32 {
        0
    }
    fn gpio_write(&mut self, _pin: u32, _val: u32) -> i32 {
        0
    }
    fn adc_read(&mut self, _ch: u32) -> i32 {
        0
    }
}

struct MockBattery;

impl BatteryReader for MockBattery {
    fn battery_mv(&self) -> u32 {
        3300
    }
}

struct MockRng(u64);

impl Rng for MockRng {
    fn random_u64(&mut self) -> u64 {
        self.0 += 1;
        self.0
    }
}

struct MockClock {
    elapsed: std::sync::atomic::AtomicU64,
}

impl MockClock {
    pub fn new() -> Self {
        Self {
            elapsed: std::sync::atomic::AtomicU64::new(100),
        }
    }
}

impl Clock for MockClock {
    fn elapsed_ms(&self) -> u64 {
        self.elapsed
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }
    fn delay_ms(&self, ms: u32) {
        self.elapsed
            .fetch_add(ms as u64, std::sync::atomic::Ordering::Relaxed);
    }
}

#[allow(dead_code)]
struct MockBpfInterpreter {
    loaded: bool,
    executed: bool,
    execute_result: Result<u64, BpfError>,
    captured_ctx: Option<SondeContext>,
}

impl MockBpfInterpreter {
    fn new() -> Self {
        Self {
            loaded: false,
            executed: false,
            execute_result: Ok(0),
            captured_ctx: None,
        }
    }
}

impl BpfInterpreter for MockBpfInterpreter {
    fn register_helper(&mut self, _id: u32, _func: HelperFn) -> Result<(), BpfError> {
        Ok(())
    }
    fn load(
        &mut self,
        _bytecode: &[u8],
        _map_ptrs: &[u64],
        _map_defs: &[sonde_protocol::MapDef],
    ) -> Result<(), BpfError> {
        self.loaded = true;
        Ok(())
    }
    fn execute(&mut self, ctx_ptr: u64, _budget: u64) -> Result<u64, BpfError> {
        self.executed = true;
        if ctx_ptr != 0 {
            // Safety: ctx_ptr points to a SondeContext on the caller's
            // stack, which is alive for the duration of this call.
            let ctx = unsafe { &*(ctx_ptr as *const SondeContext) };
            self.captured_ctx = Some(*ctx);
        }
        self.execute_result.clone()
    }
}

// ---------------------------------------------------------------------------
// BLE pairing E2E helpers
// ---------------------------------------------------------------------------

/// Generate and store a gateway Ed25519 identity.
///
/// Returns the identity for use in phone registration and encrypted
/// payload construction.
pub async fn setup_gateway_identity(storage: &SqliteStorage) -> sonde_gateway::GatewayIdentity {
    let identity =
        sonde_gateway::GatewayIdentity::generate().expect("GatewayIdentity::generate failed");
    Storage::store_gateway_identity(storage, &identity)
        .await
        .expect("store gateway identity failed");
    identity
}

/// Simulate BLE Phase 1 phone registration via `handle_ble_recv`.
///
/// Sends `REQUEST_GW_INFO` and `REGISTER_PHONE` to the gateway through
/// a fresh registration window. Returns the phone PSK and key_hint.
pub async fn simulate_phone_registration(
    identity: &sonde_gateway::GatewayIdentity,
    storage: &Arc<SqliteStorage>,
    rf_channel: u8,
) -> (Zeroizing<[u8; 32]>, u16) {
    use sonde_gateway::ble_pairing::{handle_ble_recv, RegistrationWindow};
    use sonde_pair::crypto::{
        aes256gcm_decrypt, ed25519_to_x25519_public, generate_x25519_keypair, hkdf_sha256,
        verify_ed25519_signature, x25519_ecdh,
    };
    use sonde_pair::envelope::{
        build_envelope, parse_envelope, parse_gw_info_response, parse_phone_registered,
    };
    use sonde_pair::rng::{OsRng, RngProvider};
    use sonde_pair::types;

    let mut window = RegistrationWindow::new();
    window.open(120);
    let dyn_storage: Arc<dyn Storage> = storage.clone();

    // Phase 1a: REQUEST_GW_INFO
    let rng = OsRng;
    let mut challenge = [0u8; 32];
    rng.fill_bytes(&mut challenge).unwrap();
    let request = build_envelope(types::REQUEST_GW_INFO, &challenge).unwrap();
    let response = handle_ble_recv(
        &request,
        identity,
        &dyn_storage,
        &mut window,
        rf_channel,
        None,
    )
    .await
    .expect("GW_INFO_RESPONSE must be returned");

    let (msg_type, body) = parse_envelope(&response).unwrap();
    assert_eq!(msg_type, types::GW_INFO_RESPONSE);
    let gw_info = parse_gw_info_response(body).unwrap();

    // Verify Ed25519 signature over (challenge ‖ gateway_id) and assert
    // the response fields match the expected gateway identity.
    let mut signed_data = Vec::with_capacity(32 + 16);
    signed_data.extend_from_slice(&challenge);
    signed_data.extend_from_slice(&gw_info.gateway_id);
    verify_ed25519_signature(&gw_info.gw_public_key, &signed_data, &gw_info.signature)
        .expect("GW_INFO_RESPONSE Ed25519 signature must be valid");
    assert_eq!(
        gw_info.gw_public_key,
        *identity.public_key(),
        "response public key must match gateway identity"
    );
    assert_eq!(
        gw_info.gateway_id,
        *identity.gateway_id(),
        "response gateway_id must match gateway identity"
    );

    // Phase 1b: REGISTER_PHONE
    let (eph_secret, eph_public) = generate_x25519_keypair(&rng).unwrap();
    let label = b"e2e-test-phone";
    let mut register_body = Vec::with_capacity(32 + 1 + label.len());
    register_body.extend_from_slice(&eph_public);
    register_body.push(label.len() as u8);
    register_body.extend_from_slice(label);
    let register_request = build_envelope(types::REGISTER_PHONE, &register_body).unwrap();

    let register_response = handle_ble_recv(
        &register_request,
        identity,
        &dyn_storage,
        &mut window,
        rf_channel,
        None,
    )
    .await
    .expect("PHONE_REGISTERED must be returned");

    // Decrypt PHONE_REGISTERED
    let (msg_type, body) = parse_envelope(&register_response).unwrap();
    assert_eq!(msg_type, types::PHONE_REGISTERED);
    let registered = parse_phone_registered(body).unwrap();

    // Decrypt using fields from the GW_INFO_RESPONSE (not the caller's
    // identity) so the helper validates the response like a real phone.
    let gw_x25519 = ed25519_to_x25519_public(&gw_info.gw_public_key).unwrap();
    let shared_secret = x25519_ecdh(&eph_secret, &gw_x25519);
    let aes_key = hkdf_sha256(&shared_secret, &gw_info.gateway_id, b"sonde-phone-reg-v1");
    let plaintext = aes256gcm_decrypt(
        &aes_key,
        &registered.nonce,
        &registered.ciphertext,
        &gw_info.gateway_id,
    )
    .unwrap();

    // Inner: status(1) + phone_psk(32) + phone_key_hint(2) + rf_channel(1)
    assert_eq!(
        plaintext.len(),
        36,
        "PHONE_REGISTERED inner must be 36 bytes"
    );
    assert_eq!(plaintext[0], 0x00, "status must be success");
    let mut phone_psk = Zeroizing::new([0u8; 32]);
    phone_psk.copy_from_slice(&plaintext[1..33]);
    let phone_key_hint = u16::from_be_bytes([plaintext[33], plaintext[34]]);
    assert_eq!(plaintext[35], rf_channel, "rf_channel must match");

    (phone_psk, phone_key_hint)
}

/// Build the encrypted_payload for NODE_PROVISION and PEER_REQUEST.
///
/// Mirrors the payload construction from `sonde_pair::phase2::provision_node`
/// using the public crypto and CBOR building blocks.
#[allow(clippy::too_many_arguments)]
pub fn build_encrypted_payload(
    gw_public_key: &[u8; 32],
    gw_gateway_id: &[u8; 16],
    phone_psk: &[u8; 32],
    phone_key_hint: u16,
    node_id: &str,
    node_psk: &[u8; 32],
    rf_channel: u8,
    sensors: &[sonde_pair::types::SensorDescriptor],
) -> Vec<u8> {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("failed to compute system time since UNIX_EPOCH")
        .as_secs() as i64;

    build_encrypted_payload_with_timestamp(
        gw_public_key,
        gw_gateway_id,
        phone_psk,
        phone_key_hint,
        node_id,
        node_psk,
        rf_channel,
        sensors,
        timestamp,
    )
}

/// Build the encrypted_payload with a caller-supplied timestamp.
///
/// Same as [`build_encrypted_payload`] but accepts an explicit timestamp
/// for negative testing (e.g. stale timestamps outside the ±86400 s window).
#[allow(clippy::too_many_arguments)]
pub fn build_encrypted_payload_with_timestamp(
    gw_public_key: &[u8; 32],
    gw_gateway_id: &[u8; 16],
    phone_psk: &[u8; 32],
    phone_key_hint: u16,
    node_id: &str,
    node_psk: &[u8; 32],
    rf_channel: u8,
    sensors: &[sonde_pair::types::SensorDescriptor],
    timestamp: i64,
) -> Vec<u8> {
    use sonde_pair::cbor::encode_pairing_request;
    use sonde_pair::crypto::{
        aes256gcm_encrypt, ed25519_to_x25519_public, generate_x25519_keypair, hkdf_sha256,
        hmac_sha256, x25519_ecdh,
    };
    use sonde_pair::rng::{OsRng, RngProvider};

    // Step 1: Encode PairingRequest as CBOR
    let cbor = encode_pairing_request(node_id, node_psk, rf_channel, sensors, timestamp).unwrap();

    // Step 2: authenticated_request = phone_key_hint(2) + cbor + hmac(32)
    let phone_hmac = hmac_sha256(phone_psk, &cbor);
    let mut auth_request = Vec::with_capacity(2 + cbor.len() + 32);
    auth_request.extend_from_slice(&phone_key_hint.to_be_bytes());
    auth_request.extend_from_slice(&cbor);
    auth_request.extend_from_slice(&phone_hmac);

    // Step 3: ECDH with gateway
    let gw_x25519 = ed25519_to_x25519_public(gw_public_key).unwrap();
    let rng = OsRng;
    let (eph_secret, eph_public) = generate_x25519_keypair(&rng).unwrap();
    let shared_secret = x25519_ecdh(&eph_secret, &gw_x25519);
    let aes_key = hkdf_sha256(&shared_secret, gw_gateway_id, b"sonde-node-pair-v1");

    // Step 4: Encrypt
    let mut nonce = [0u8; 12];
    rng.fill_bytes(&mut nonce).unwrap();
    let ciphertext = aes256gcm_encrypt(&aes_key, &nonce, &auth_request, gw_gateway_id).unwrap();

    // Step 5: encrypted_payload = eph_public(32) + nonce(12) + ciphertext
    let mut payload = Vec::with_capacity(32 + 12 + ciphertext.len());
    payload.extend_from_slice(&eph_public);
    payload.extend_from_slice(&nonce);
    payload.extend_from_slice(&ciphertext);

    payload
}

// ---------------------------------------------------------------------------
// GatewayBleAdapter — routes sonde-pair BleTransport calls to handle_ble_recv
// ---------------------------------------------------------------------------

/// Registration window duration for test adapters (seconds).
///
/// The BLE onboarding flow requires an open registration window. 300 s (5 min)
/// is generous enough for any realistic test scenario without risking timeouts.
const TEST_REG_WINDOW_SECS: u32 = 300;

/// BLE transport adapter that routes `sonde_pair::transport::BleTransport`
/// calls directly to the gateway's `handle_ble_recv`, bridging sonde-pair's
/// state machine to the gateway engine without network or BLE hardware.
pub struct GatewayBleAdapter {
    identity: sonde_gateway::GatewayIdentity,
    storage: Arc<dyn sonde_gateway::storage::Storage>,
    window: tokio::sync::Mutex<sonde_gateway::ble_pairing::RegistrationWindow>,
    rf_channel: u8,
    response_queue: tokio::sync::Mutex<VecDeque<Vec<u8>>>,
    response_notify: tokio::sync::Notify,
}

impl GatewayBleAdapter {
    /// Create a new adapter wired to the given gateway identity and storage.
    pub fn new(
        identity: sonde_gateway::GatewayIdentity,
        storage: Arc<dyn sonde_gateway::storage::Storage>,
        rf_channel: u8,
    ) -> Self {
        let mut window = sonde_gateway::ble_pairing::RegistrationWindow::new();
        window.open(TEST_REG_WINDOW_SECS);
        Self {
            identity,
            storage,
            window: tokio::sync::Mutex::new(window),
            rf_channel,
            response_queue: tokio::sync::Mutex::new(VecDeque::new()),
            response_notify: tokio::sync::Notify::new(),
        }
    }
}

impl sonde_pair::transport::BleTransport for GatewayBleAdapter {
    fn start_scan(
        &mut self,
        _service_uuids: &[u128],
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), sonde_pair::error::PairingError>> + '_>,
    > {
        Box::pin(async { Ok(()) })
    }

    fn stop_scan(
        &mut self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), sonde_pair::error::PairingError>> + '_>,
    > {
        Box::pin(async { Ok(()) })
    }

    fn get_discovered_devices(
        &self,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<
                        Vec<sonde_pair::types::ScannedDevice>,
                        sonde_pair::error::PairingError,
                    >,
                > + '_,
        >,
    > {
        Box::pin(async {
            Ok(vec![sonde_pair::types::ScannedDevice {
                name: "Sonde-GW-E2E".into(),
                address: [0x10, 0x0B, 0xAC, 0x00, 0x00, 0x01],
                rssi: -50,
                service_uuids: vec![sonde_pair::types::GATEWAY_SERVICE_UUID],
            }])
        })
    }

    fn connect(
        &mut self,
        address: &[u8; 6],
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<u16, sonde_pair::error::PairingError>> + '_>,
    > {
        let address = *address;
        Box::pin(async move {
            // Ensure we only "connect" to the device we advertised via get_discovered_devices,
            // so tests fail if the pairing state machine selects or routes to the wrong device.
            let expected_address: [u8; 6] = [0x10, 0x0B, 0xAC, 0x00, 0x00, 0x01];

            if address == expected_address {
                Ok(247)
            } else {
                Err(sonde_pair::error::PairingError::ConnectionFailed(
                    "unexpected device address in e2e harness".into(),
                ))
            }
        })
    }

    fn disconnect(
        &mut self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), sonde_pair::error::PairingError>> + '_>,
    > {
        Box::pin(async { Ok(()) })
    }

    fn write_characteristic(
        &mut self,
        service: u128,
        characteristic: u128,
        data: &[u8],
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), sonde_pair::error::PairingError>> + '_>,
    > {
        let data = data.to_vec();
        Box::pin(async move {
            if service != sonde_pair::types::GATEWAY_SERVICE_UUID
                || characteristic != sonde_pair::types::GATEWAY_COMMAND_UUID
            {
                return Err(sonde_pair::error::PairingError::ConnectionFailed(
                    "unexpected GATT service/characteristic in e2e harness".into(),
                ));
            }
            let mut window = self.window.lock().await;
            // Refresh the registration window if it has expired, so slow CI
            // environments don't cause nondeterministic test failures.
            if !window.is_open() {
                window.open(TEST_REG_WINDOW_SECS);
            }
            let response = sonde_gateway::ble_pairing::handle_ble_recv(
                &data,
                &self.identity,
                &self.storage,
                &mut window,
                self.rf_channel,
                None,
            )
            .await;
            if let Some(resp) = response {
                self.response_queue.lock().await.push_back(resp);
                self.response_notify.notify_one();
            }
            Ok(())
        })
    }

    fn pairing_method(&self) -> Option<sonde_pair::types::PairingMethod> {
        Some(sonde_pair::types::PairingMethod::NumericComparison)
    }

    fn read_indication(
        &mut self,
        service: u128,
        characteristic: u128,
        timeout_ms: u64,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Vec<u8>, sonde_pair::error::PairingError>> + '_,
        >,
    > {
        Box::pin(async move {
            if service != sonde_pair::types::GATEWAY_SERVICE_UUID
                || characteristic != sonde_pair::types::GATEWAY_COMMAND_UUID
            {
                return Err(sonde_pair::error::PairingError::ConnectionFailed(
                    "unexpected GATT service/characteristic in e2e harness".into(),
                ));
            }
            let timeout_duration = Duration::from_millis(timeout_ms);

            let wait_future = async {
                loop {
                    // Prepare the notified future *before* checking the queue
                    // to avoid losing a wakeup that fires between the check
                    // and the await.
                    let notified = self.response_notify.notified();

                    if let Some(response) = self.response_queue.lock().await.pop_front() {
                        return response;
                    }

                    notified.await;
                }
            };

            match tokio::time::timeout(timeout_duration, wait_future).await {
                Ok(response) => Ok(response),
                Err(_) => Err(sonde_pair::error::PairingError::IndicationTimeout),
            }
        })
    }
}
