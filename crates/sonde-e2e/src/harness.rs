// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Bridge harness that wires gateway and node together via in-memory frame
//! queues for end-to-end integration testing.
//!
//! Phase 1: direct in-process bridge — the node calls
//! `Gateway::process_frame` synchronously via `block_in_place`.
//! Modem / ESP-NOW radio integration will be added in a later phase.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;

use sonde_gateway::engine::{Gateway, PendingCommand};
use sonde_gateway::registry::NodeRecord;
use sonde_gateway::session::SessionManager;
use sonde_gateway::sqlite_storage::SqliteStorage;
use sonde_gateway::storage::Storage;

use sonde_node::bpf_helpers::SondeContext;
use sonde_node::bpf_runtime::{BpfError, BpfInterpreter, HelperFn};
use sonde_node::error::NodeResult;
use sonde_node::hal::{BatteryReader, Hal};
use sonde_node::map_storage::MapStorage;
use sonde_node::traits::{Clock, PlatformStorage, Rng, Transport as NodeTransport};
use sonde_node::wake_cycle::{run_wake_cycle, WakeCycleOutcome};

use sonde_protocol::{HmacProvider, Sha256Provider};

// ---------------------------------------------------------------------------
// E2eTestEnv — gateway-side test environment
// ---------------------------------------------------------------------------

/// Top-level test environment holding the gateway and its backing storage.
pub struct E2eTestEnv {
    pub gateway: Arc<Gateway>,
    pub storage: Arc<SqliteStorage>,
    pub pending_commands: Arc<RwLock<HashMap<String, Vec<PendingCommand>>>>,
}

impl E2eTestEnv {
    /// Create a fresh in-memory test environment.
    pub async fn new() -> Self {
        let storage = Arc::new(
            SqliteStorage::in_memory().expect("failed to create in-memory SQLite storage"),
        );
        let session_manager = Arc::new(SessionManager::new(Duration::from_secs(30)));
        let pending_commands: Arc<RwLock<HashMap<String, Vec<PendingCommand>>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let gateway = Arc::new(Gateway::new_with_pending(
            storage.clone(),
            pending_commands.clone(),
            session_manager,
        ));
        Self {
            gateway,
            storage,
            pending_commands,
        }
    }

    /// Register a node in the gateway's storage.
    pub async fn register_node(&self, node_id: &str, key_hint: u16, psk: [u8; 32]) {
        let node = NodeRecord::new(node_id.into(), key_hint, psk);
        self.storage.upsert_node(&node).await.unwrap();
    }
}

// ---------------------------------------------------------------------------
// NodeProxy — drives one node's wake cycle through the gateway
// ---------------------------------------------------------------------------

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
    pub fn new(node_id: &str, key_hint: u16, psk: [u8; 32]) -> Self {
        let _ = node_id; // used only to initialise storage
        Self {
            mac: vec![0x01, 0x02, 0x03, 0x04, 0x05, 0x06],
            storage: MockNodeStorage::new_paired(key_hint, psk, 60),
            map_storage: MapStorage::new(4096),
            rng: MockRng(0),
        }
    }

    /// Run one full wake cycle, relaying every frame through the real gateway.
    ///
    /// Uses `block_in_place` so the synchronous node code can call the async
    /// gateway inline. `block_in_place` requires a multi-thread tokio runtime.
    /// All E2E tests must use `#[tokio::test(flavor = "multi_thread")]`.
    pub async fn run_wake_cycle(&mut self, env: &E2eTestEnv) -> WakeCycleOutcome {
        let mut hal = MockHal;
        let clock = MockClock::new();
        let battery = MockBattery;
        let mut interpreter = MockBpfInterpreter::new();
        let hmac = TestHmac;
        let sha = TestSha256;

        let mut transport = BridgeTransport::new(env.gateway.clone(), self.mac.clone());

        run_wake_cycle(
            &mut transport,
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
    rt: tokio::runtime::Handle,
}

impl BridgeTransport {
    fn new(gateway: Arc<Gateway>, peer: Vec<u8>) -> Self {
        Self {
            gateway,
            peer,
            pending_response: None,
            rt: tokio::runtime::Handle::try_current()
                .expect("BridgeTransport must be created inside a multi-thread tokio runtime"),
        }
    }
}

impl NodeTransport for BridgeTransport {
    fn send(&mut self, frame: &[u8]) -> NodeResult<()> {
        let gateway = self.gateway.clone();
        let peer = self.peer.clone();
        let frame = frame.to_vec();
        let response = tokio::task::block_in_place(|| {
            self.rt
                .block_on(async { gateway.process_frame(&frame, peer).await })
        });
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

struct TestSha256;

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
        self.programs
            .get(partition as usize)
            .and_then(|p| p.clone())
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
    fn load(&mut self, _bytecode: &[u8], _map_ptrs: &[u64]) -> Result<(), BpfError> {
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
