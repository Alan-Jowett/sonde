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

use sonde_modem::bridge::{Bridge, Radio, SerialPort};
use sonde_modem::status::ModemCounters;

use sonde_node::bpf_helpers::SondeContext;
use sonde_node::bpf_runtime::{BpfError, BpfInterpreter, HelperFn};
use sonde_node::error::{NodeError, NodeResult};
use sonde_node::hal::{BatteryReader, Hal};
use sonde_node::map_storage::MapStorage;
use sonde_node::traits::{Clock, PairingSerial, PlatformStorage, Rng, Transport as NodeTransport};
use sonde_node::wake_cycle::{run_wake_cycle, WakeCycleOutcome};

use sonde_protocol::modem::{encode_modem_frame, FrameDecoder, ModemMessage, RecvFrame, MAC_SIZE};
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
            SqliteStorage::in_memory().expect("failed to create in-memory SQLite storage"),
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
            SqliteStorage::in_memory().expect("failed to create in-memory SQLite storage"),
        );
        let config = HandlerConfig {
            matchers: vec![ProgramMatcher::Any],
            command: handler_cmd.to_string(),
            args: handler_args.iter().map(|s| s.to_string()).collect(),
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
/// Uses `std::sync::mpsc` (not tokio) because `Radio::drain_rx` takes
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
}

impl Radio for ChannelRadio {
    fn send(&mut self, _peer_mac: &[u8; MAC_SIZE], data: &[u8]) {
        use std::sync::mpsc::TrySendError;
        match self.to_node.try_send(data.to_vec()) {
            Ok(()) => {}
            // Node receiver dropped during teardown — harmless.
            Err(TrySendError::Disconnected(_)) => {}
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
                peer_mac: [0x01, 0x02, 0x03, 0x04, 0x05, 0x06],
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
                "node→bridge channel full (cap 64); bridge is not draining".into(),
            )),
            Err(TrySendError::Disconnected(..)) => {
                Err(NodeError::Transport("channel disconnected".into()))
            }
        }
    }

    fn recv(&mut self, timeout_ms: u32) -> NodeResult<Option<Vec<u8>>> {
        let rx = self.rx.lock().unwrap();
        match rx.recv_timeout(Duration::from_millis(timeout_ms as u64)) {
            Ok(data) => Ok(Some(data)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                Err(NodeError::Transport("channel disconnected".into()))
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
                            let _ = writer.flush().await;
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
                let _ = bridge_thread.join();
                panic!("modem startup handshake failed: {e}");
            }
        };

        // Create gateway engine.
        let storage = Arc::new(
            SqliteStorage::in_memory().expect("failed to create in-memory SQLite storage"),
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

struct TestHmac;

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
}

impl MockNodeStorage {
    pub fn new_paired(key_hint: u16, psk: [u8; 32], schedule_interval_s: u32) -> Self {
        Self {
            key: Some((key_hint, psk)),
            schedule_interval: schedule_interval_s,
            active_partition: 0,
            programs: [None, None],
            early_wake_flag: false,
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
        }
    }
}

impl PlatformStorage for MockNodeStorage {
    fn read_key(&self) -> Option<(u16, [u8; 32])> {
        self.key
    }
    fn write_key(&mut self, key_hint: u16, psk: &[u8; 32]) -> NodeResult<()> {
        if self.key.is_some() {
            return Err(sonde_node::error::NodeError::StorageError(
                "already paired".into(),
            ));
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
                "partition must be 0 or 1".into(),
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
                "invalid partition".into(),
            ));
        }
        self.programs[partition as usize] = Some(image.to_vec());
        Ok(())
    }
    fn erase_program(&mut self, partition: u8) -> NodeResult<()> {
        if (partition as usize) >= self.programs.len() {
            return Err(sonde_node::error::NodeError::StorageError(
                "invalid partition".into(),
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
// MockPairingSerial — simulated USB-CDC serial for pairing tests
// ---------------------------------------------------------------------------

/// Simulated serial port for testing [`sonde_node::pairing::run_pairing_mode`].
///
/// Feed encoded modem frames via [`enqueue`], then call `run_pairing_mode`.
/// After it returns, inspect [`received`] for all frames the node wrote back.
pub struct MockPairingSerial {
    /// Bytes the node will read, in order. Populated by [`enqueue`].
    rx_buf: VecDeque<u8>,
    /// Raw bytes the node wrote back. Decode with [`received`].
    tx_buf: Vec<u8>,
    /// When `rx_buf` is drained and this is true, reads return `Err`
    /// (simulating USB disconnect).
    disconnect_when_empty: bool,
}

impl Default for MockPairingSerial {
    fn default() -> Self {
        Self::new()
    }
}

impl MockPairingSerial {
    pub fn new() -> Self {
        Self {
            rx_buf: VecDeque::new(),
            tx_buf: Vec::new(),
            disconnect_when_empty: true,
        }
    }

    /// Enqueue a modem message for the node to read.
    pub fn enqueue(&mut self, msg: &ModemMessage) {
        let frame = encode_modem_frame(msg).expect("encode mock message");
        self.rx_buf.extend(frame);
    }

    /// Decode all frames the node sent back (PAIRING_READY + responses).
    pub fn received(&self) -> Vec<ModemMessage> {
        let mut decoder = FrameDecoder::new();
        decoder.push(&self.tx_buf);
        let mut msgs = Vec::new();
        loop {
            match decoder.decode() {
                Ok(Some(msg)) => msgs.push(msg),
                Ok(None) => break,
                Err(_) => continue,
            }
        }
        msgs
    }
}

impl PairingSerial for MockPairingSerial {
    fn read(&mut self, buf: &mut [u8], _timeout_ms: u32) -> NodeResult<usize> {
        if self.rx_buf.is_empty() {
            if self.disconnect_when_empty {
                return Err(NodeError::Transport("mock USB disconnect".into()));
            }
            return Ok(0);
        }
        let n = buf.len().min(self.rx_buf.len());
        for b in buf.iter_mut().take(n) {
            *b = self.rx_buf.pop_front().unwrap();
        }
        Ok(n)
    }

    fn write(&mut self, data: &[u8]) -> NodeResult<()> {
        self.tx_buf.extend_from_slice(data);
        Ok(())
    }
}
