// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Bridge harness that wires gateway and node together via in-memory frame
//! queues for end-to-end integration testing.
//!
//! All frames are routed through the gateway's `process_frame` path
//! (AES-256-GCM) via `BridgeTransport`.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;

use sonde_gateway::engine::{Gateway, PendingCommand};
use sonde_gateway::handler::{HandlerConfig, HandlerRouter, ProgramMatcher};
use sonde_gateway::registry::NodeRecord;
use sonde_gateway::session::SessionManager;
use sonde_gateway::sqlite_storage::SqliteStorage;
use sonde_gateway::storage::Storage;
use zeroize::Zeroizing;

use sonde_node::async_queue::AsyncQueue;
use sonde_node::bpf_helpers::SondeContext;
use sonde_node::bpf_runtime::{BpfError, BpfInterpreter, HelperFn};
use sonde_node::error::NodeResult;
use sonde_node::hal::{BatteryReader, Hal};
use sonde_node::map_storage::MapStorage;
use sonde_node::traits::{Clock, PlatformStorage, Rng, Transport as NodeTransport};
use sonde_node::wake_cycle::WakeCycleOutcome;

use sonde_protocol::Sha256Provider;

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
            Arc::new(RwLock::new(HandlerRouter::new(Vec::new()))),
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
            working_dir: None,
        };
        let router = Arc::new(RwLock::new(HandlerRouter::new(vec![config])));
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
    /// Raw bytes of captured outbound APP_DATA frames only.
    pub sent_raw_frames: Vec<Vec<u8>>,
    /// `msg_type` for every non-`None` response with a valid protocol header.
    pub received_msg_types: Vec<u8>,
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
    async_queue: AsyncQueue,
}

impl NodeProxy {
    pub fn new(key_hint: u16, psk: [u8; 32]) -> Self {
        Self {
            mac: vec![0x01, 0x02, 0x03, 0x04, 0x05, 0x06],
            storage: MockNodeStorage::new_paired(key_hint, psk, 60),
            map_storage: MapStorage::new(4096),
            rng: MockRng(0),
            async_queue: AsyncQueue::new(),
        }
    }

    /// Create an unpaired node (no key material in storage).
    pub fn new_unpaired() -> Self {
        Self {
            mac: vec![0x01, 0x02, 0x03, 0x04, 0x05, 0x06],
            storage: MockNodeStorage::new_unpaired(),
            map_storage: MapStorage::new(4096),
            rng: MockRng(0),
            async_queue: AsyncQueue::new(),
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
            async_queue: AsyncQueue::new(),
        }
    }

    /// Run one AEAD wake cycle, relaying frames through the gateway's
    /// `process_frame` path (AES-256-GCM).
    ///
    /// Uses `block_in_place` internally, so the caller must be running
    /// inside a multi-thread Tokio runtime. All E2E tests that call this
    /// must use `#[tokio::test(flavor = "multi_thread")]`.
    pub fn run_wake_cycle(&mut self, env: &E2eTestEnv) -> WakeCycleStats {
        let mut interpreter = MockBpfInterpreter::new();
        self.run_wake_cycle_inner(env, &mut interpreter, false)
    }

    /// Like [`run_wake_cycle`] but accepts a caller-supplied BPF
    /// interpreter for tests that require real BPF program execution.
    ///
    /// Requires a multi-thread Tokio runtime (see [`run_wake_cycle`]).
    pub fn run_wake_cycle_with(
        &mut self,
        env: &E2eTestEnv,
        interpreter: &mut impl BpfInterpreter,
    ) -> WakeCycleStats {
        self.run_wake_cycle_inner(env, interpreter, false)
    }

    /// Run one AEAD wake cycle with outgoing frame tampering.
    ///
    /// A bit is flipped in the ciphertext region of every non-APP_DATA
    /// frame before forwarding to the gateway, causing GCM authentication
    /// failure and silent discard.
    ///
    /// Requires a multi-thread Tokio runtime (see [`run_wake_cycle`]).
    pub fn run_wake_cycle_tampered(&mut self, env: &E2eTestEnv) -> WakeCycleStats {
        let mut interpreter = MockBpfInterpreter::new();
        self.run_wake_cycle_inner(env, &mut interpreter, true)
    }

    fn run_wake_cycle_inner(
        &mut self,
        env: &E2eTestEnv,
        interpreter: &mut impl BpfInterpreter,
        tamper: bool,
    ) -> WakeCycleStats {
        use sonde_node::node_aead::NodeAead;
        use sonde_node::wake_cycle::run_wake_cycle;

        let mut hal = MockHal;
        let clock = MockClock::new();
        let battery = MockBattery;
        let sha = TestSha256;
        let aead = NodeAead;

        let mut transport = if tamper {
            BridgeTransport::new_tampered(env.gateway.clone(), self.mac.clone())
        } else {
            BridgeTransport::new(env.gateway.clone(), self.mac.clone())
        };

        let outcome = run_wake_cycle(
            &mut transport,
            &mut self.storage,
            &mut hal,
            &mut self.rng,
            &clock,
            &battery,
            interpreter,
            &mut self.map_storage,
            &sha,
            &aead,
            &mut self.async_queue,
        );
        WakeCycleStats {
            outcome,
            response_count: transport.response_count(),
            wake_nonces: transport.wake_nonces().to_vec(),
            sent_frames: transport.sent_frames().to_vec(),
            sent_raw_frames: transport.sent_raw_frames().to_vec(),
            received_msg_types: transport.received_msg_types().to_vec(),
        }
    }
}

// ---------------------------------------------------------------------------
// BridgeTransport — AEAD frame relay for AES-256-GCM E2E tests
// ---------------------------------------------------------------------------

/// In-memory frame relay that routes all frames through the gateway's
/// `process_frame` path (AES-256-GCM), including PEER_REQUEST.
struct BridgeTransport {
    gateway: Arc<Gateway>,
    peer: Vec<u8>,
    pending_response: Option<Vec<u8>>,
    response_count: usize,
    wake_nonces: Vec<u64>,
    sent_frames: Vec<(u8, u64)>,
    sent_raw_frames: Vec<Vec<u8>>,
    received_msg_types: Vec<u8>,
    rt: tokio::runtime::Handle,
    tamper_outgoing: bool,
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
            sent_raw_frames: Vec::new(),
            received_msg_types: Vec::new(),
            rt: tokio::runtime::Handle::try_current()
                .expect("BridgeTransport must be created inside a Tokio runtime"),
            tamper_outgoing: false,
        }
    }

    fn new_tampered(gateway: Arc<Gateway>, peer: Vec<u8>) -> Self {
        let mut t = Self::new(gateway, peer);
        t.tamper_outgoing = true;
        t
    }

    fn response_count(&self) -> usize {
        self.response_count
    }

    fn wake_nonces(&self) -> &[u64] {
        &self.wake_nonces
    }

    fn sent_frames(&self) -> &[(u8, u64)] {
        &self.sent_frames
    }

    fn received_msg_types(&self) -> &[u8] {
        &self.received_msg_types
    }

    fn sent_raw_frames(&self) -> &[Vec<u8>] {
        &self.sent_raw_frames
    }
}

impl NodeTransport for BridgeTransport {
    fn send(&mut self, frame: &[u8]) -> NodeResult<()> {
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
            // Only capture APP_DATA raw frames to avoid copying bulk
            // CHUNK data during program download.
            if msg_type == sonde_protocol::MSG_APP_DATA {
                self.sent_raw_frames.push(frame.to_vec());
            }
        }

        let gateway = self.gateway.clone();
        let peer = self.peer.clone();

        // All frames (including PEER_REQUEST) use the AEAD codec.
        let mut frame_vec = frame.to_vec();
        if self.tamper_outgoing && frame_vec.len() > sonde_protocol::HEADER_SIZE {
            // Flip a bit in the first ciphertext byte to trigger GCM auth failure.
            frame_vec[sonde_protocol::HEADER_SIZE] ^= 0x01;
        }
        let response = tokio::task::block_in_place(|| {
            self.rt.block_on(gateway.process_frame(&frame_vec, peer))
        });

        if let Some(ref resp) = response {
            self.response_count += 1;
            if resp.len() >= sonde_protocol::HEADER_SIZE {
                self.received_msg_types
                    .push(resp[sonde_protocol::OFFSET_MSG_TYPE]);
            }
        }
        self.pending_response = response;
        Ok(())
    }

    fn recv(&mut self, _timeout_ms: u32) -> NodeResult<Option<Vec<u8>>> {
        Ok(self.pending_response.take())
    }
}

// ---------------------------------------------------------------------------
// Crypto providers (SHA-256)
// ---------------------------------------------------------------------------

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
    use sonde_pair::envelope::{build_envelope, parse_envelope, parse_gw_info_response};
    use sonde_pair::rng::{OsRng, RngProvider};
    use sonde_pair::types;
    use std::sync::Arc;

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
    {
        use ed25519_dalek::{Signature, VerifyingKey};
        let vk = VerifyingKey::from_bytes(&gw_info.gw_public_key)
            .expect("invalid Ed25519 public key in GW_INFO_RESPONSE");
        let sig = Signature::from_bytes(&gw_info.signature);
        vk.verify_strict(&signed_data, &sig)
            .expect("GW_INFO_RESPONSE Ed25519 signature must be valid");
    }
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

    // Phase 1b: REGISTER_PHONE (AEAD — phone sends PSK directly)
    let mut phone_psk = Zeroizing::new([0u8; 32]);
    rng.fill_bytes(&mut *phone_psk).unwrap();
    let label = b"e2e-test-phone";
    let mut register_body = Vec::with_capacity(32 + 1 + label.len());
    register_body.extend_from_slice(&*phone_psk);
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

    // Parse PHONE_REGISTERED (AEAD): status(1) + rf_channel(1) + phone_key_hint(2 BE)
    let (msg_type, body) = parse_envelope(&register_response).unwrap();
    assert_eq!(msg_type, types::PHONE_REGISTERED);
    assert_eq!(body.len(), 4, "PHONE_REGISTERED body must be 4 bytes");
    assert_eq!(body[0], 0x00, "status must be success");
    assert_eq!(body[1], rf_channel, "rf_channel must match");
    let phone_key_hint = u16::from_be_bytes([body[2], body[3]]);

    (phone_psk, phone_key_hint)
}

/// Build the complete ESP-NOW PEER_REQUEST frame for NODE_PROVISION.
///
/// Uses `encrypt_pairing_request` to build a frame the node relays
/// verbatim during the PEER_REQUEST exchange.
#[allow(clippy::too_many_arguments)]
pub fn build_encrypted_payload(
    _gw_public_key: &[u8; 32],
    _gw_gateway_id: &[u8; 16],
    phone_psk: &[u8; 32],
    _phone_key_hint: u16,
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
        _gw_public_key,
        _gw_gateway_id,
        phone_psk,
        _phone_key_hint,
        node_id,
        node_psk,
        rf_channel,
        sensors,
        timestamp,
    )
}

/// Build the complete ESP-NOW PEER_REQUEST frame with a caller-supplied timestamp.
///
/// Same as [`build_encrypted_payload`] but accepts an explicit timestamp
/// for negative testing (e.g. stale timestamps outside the ±86400 s window).
#[allow(clippy::too_many_arguments)]
pub fn build_encrypted_payload_with_timestamp(
    _gw_public_key: &[u8; 32],
    _gw_gateway_id: &[u8; 16],
    phone_psk: &[u8; 32],
    _phone_key_hint: u16,
    node_id: &str,
    node_psk: &[u8; 32],
    rf_channel: u8,
    sensors: &[sonde_pair::types::SensorDescriptor],
    timestamp: i64,
) -> Vec<u8> {
    use sonde_pair::cbor::encode_pairing_request;
    use sonde_pair::crypto::encrypt_pairing_request;

    let cbor = encode_pairing_request(node_id, node_psk, rf_channel, sensors, timestamp).unwrap();
    encrypt_pairing_request(phone_psk, &cbor).expect("encrypt_pairing_request must succeed in test")
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
                Err(sonde_pair::error::PairingError::ConnectionFailed {
                    device: None,
                    reason: "unexpected device address in e2e harness".into(),
                })
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
                return Err(sonde_pair::error::PairingError::ConnectionFailed {
                    device: None,
                    reason: "unexpected GATT service/characteristic in e2e harness".into(),
                });
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
                return Err(sonde_pair::error::PairingError::ConnectionFailed {
                    device: None,
                    reason: "unexpected GATT service/characteristic in e2e harness".into(),
                });
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
                Err(_) => Err(sonde_pair::error::PairingError::IndicationTimeout { device: None }),
            }
        })
    }
}
