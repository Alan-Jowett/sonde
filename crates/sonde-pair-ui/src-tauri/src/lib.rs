// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Tauri v2 backend for the Sonde BLE pairing tool.
//!
//! On desktop, BLE operations use [`BtleplugTransport`].
//! On Android, BLE operations use [`AndroidBleTransport`].
//!
//! Pairing artifacts (phone PSK) are held in memory during the session
//! and persisted to platform-appropriate secure storage:
//! [`FilePairingStore`] on desktop, [`AndroidPairingStore`] (backed by
//! `EncryptedSharedPreferences`) on Android. The simplified AEAD flow
//! does not use ECDH or gateway identity TOFU.
//!
//! All BLE operations use `spawn_blocking` + `Handle::block_on` so that
//! non-Send futures from [`sonde_pair::transport::BleTransport`] work on
//! the tokio multi-threaded runtime.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use sonde_pair::discovery::{service_type, DeviceScanner, ServiceType};
use sonde_pair::error::PairingError;
use sonde_pair::phase1::PairingProgress;
use sonde_pair::rng::OsRng;
use sonde_pair::transport::BleTransport;
use sonde_pair::types::{BoardLayout, ScannedDevice, BLE_MTU_MIN};
use sonde_pair::{phase1, phase2};

#[cfg(not(target_os = "android"))]
use sonde_pair::btleplug_transport::BtleplugTransport;
#[cfg(not(target_os = "android"))]
use sonde_pair::file_store::FilePairingStore;

#[cfg(target_os = "android")]
use sonde_pair::android_store::AndroidPairingStore;
#[cfg(target_os = "android")]
use sonde_pair::android_transport::AndroidBleTransport;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

struct AppState {
    #[cfg(not(target_os = "android"))]
    scanner: Mutex<Option<DeviceScanner<BtleplugTransport>>>,
    #[cfg(target_os = "android")]
    scanner: Mutex<Option<DeviceScanner<AndroidBleTransport>>>,
    #[cfg(not(target_os = "android"))]
    connected_node: Mutex<Option<ConnectedNodeSession<BtleplugTransport>>>,
    #[cfg(target_os = "android")]
    connected_node: Mutex<Option<ConnectedNodeSession<AndroidBleTransport>>>,
    signal_check_cancel: Mutex<Option<Arc<AtomicBool>>>,
    phase: Arc<Mutex<String>>,
    logs: Arc<Mutex<Vec<String>>>,
    /// Phase 1 AEAD artifacts, held in memory for Phase 2 provisioning.
    pairing_artifacts: Mutex<Option<Arc<phase1::PairingArtifacts>>>,
}

struct ConnectedNodeSession<T> {
    address: [u8; 6],
    transport: T,
}

/// Reports Phase 1 sub-phase transitions to the UI via the shared `phase` mutex.
struct UiPairingProgress {
    phase: Arc<Mutex<String>>,
}

impl PairingProgress for UiPairingProgress {
    fn on_phase(&self, phase: &str) {
        *self.phase.lock().unwrap() = phase.to_string();
    }
}

// ---------------------------------------------------------------------------
// Serializable types for the frontend
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct DeviceInfo {
    address: String,
    name: String,
    rssi: i8,
    service_type: String,
}

#[derive(Serialize)]
struct PairingStatus {
    paired: bool,
    gateway_id: Option<String>,
}

#[derive(Serialize)]
struct ConnectedNodeInfo {
    address: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DiagnosticInfo {
    rssi_dbm: i8,
    signal_quality: u8,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BoardLayoutInput {
    i2c0_sda: Option<u8>,
    i2c0_scl: Option<u8>,
    one_wire_data: Option<u8>,
    battery_adc: Option<u8>,
    sensor_enable: Option<u8>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn format_address(addr: &[u8; 6]) -> String {
    format!(
        "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        addr[0], addr[1], addr[2], addr[3], addr[4], addr[5]
    )
}

fn parse_address(s: &str) -> Result<[u8; 6], String> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 6 {
        return Err(format!("invalid address `{s}`: expected AA:BB:CC:DD:EE:FF"));
    }
    let mut addr = [0u8; 6];
    for (i, part) in parts.iter().enumerate() {
        addr[i] = u8::from_str_radix(part, 16)
            .map_err(|_| format!("invalid hex byte `{part}` in address"))?;
    }
    Ok(addr)
}

async fn stop_and_discard_scanner<T>(
    scanner_slot: &Mutex<Option<DeviceScanner<T>>>,
) -> Result<(), String>
where
    T: BleTransport + Send + 'static,
{
    let scanner = scanner_slot.lock().unwrap().take();
    if let Some(scanner) = scanner {
        tokio::task::spawn_blocking(move || {
            tokio::runtime::Handle::current().block_on(async move {
                let mut scanner = scanner;
                scanner.stop().await.map_err(|e| e.to_string())
            })
        })
        .await
        .map_err(|e| format!("task panicked: {e}"))??;
    }
    Ok(())
}

async fn reuse_or_connect_session<T, F, Fut>(
    existing: Option<ConnectedNodeSession<T>>,
    addr: [u8; 6],
    create_transport: F,
) -> Result<ConnectedNodeSession<T>, PairingError>
where
    T: BleTransport,
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T, PairingError>>,
{
    if let Some(mut session) = existing {
        if session.address == addr {
            return Ok(session);
        }
        let _ = session.transport.disconnect().await;
    }

    let mut transport = create_transport().await?;
    transport.set_defer_bonding(true);
    let mtu_result = transport.connect(&addr).await;
    transport.set_defer_bonding(false);
    let mtu = mtu_result?;
    if mtu < BLE_MTU_MIN {
        transport.disconnect().await.ok();
        return Err(PairingError::MtuTooLow {
            device: format_address(&addr),
            negotiated: mtu,
            required: BLE_MTU_MIN,
        });
    }
    Ok(ConnectedNodeSession {
        address: addr,
        transport,
    })
}

async fn finalize_signal_check<T: BleTransport>(
    mut session: ConnectedNodeSession<T>,
    outcome: Result<phase2::DiagnosticResult, PairingError>,
    cancelled: &AtomicBool,
) -> (
    Option<ConnectedNodeSession<T>>,
    Result<DiagnosticInfo, PairingError>,
) {
    if cancelled.load(Ordering::Relaxed) {
        let _ = session.transport.disconnect().await;
        return (
            None,
            Err(PairingError::Cancelled {
                operation: "signal check",
            }),
        );
    }
    match outcome {
        Ok(diag) => (
            Some(session),
            Ok(DiagnosticInfo {
                rssi_dbm: diag.rssi_dbm,
                signal_quality: diag.signal_quality,
            }),
        ),
        Err(PairingError::DiagnosticFailed(message)) => {
            (Some(session), Err(PairingError::DiagnosticFailed(message)))
        }
        Err(PairingError::InvalidResponse { msg_type, reason }) => (
            Some(session),
            Err(PairingError::InvalidResponse { msg_type, reason }),
        ),
        Err(e) => {
            let _ = session.transport.disconnect().await;
            (None, Err(e))
        }
    }
}

fn resolve_legacy_i2c_layout(
    i2c_sda: Option<u8>,
    i2c_scl: Option<u8>,
) -> Result<Option<BoardLayout>, String> {
    match (i2c_sda, i2c_scl) {
        (Some(sda), Some(scl)) => Ok(Some(BoardLayout {
            i2c0_sda: Some(sda),
            i2c0_scl: Some(scl),
            one_wire_data: None,
            battery_adc: None,
            sensor_enable: None,
        })),
        (None, None) => Ok(None),
        _ => Err("Provide both I2C SDA and I2C SCL pins, or leave both empty.".into()),
    }
}

fn validate_supported_battery_adc(layout: &BoardLayout) -> Result<(), String> {
    match layout.battery_adc {
        Some(0..=4) | None => Ok(()),
        Some(pin) => Err(format!(
            "battery_adc GPIO {pin} is not ADC-capable on ESP32-C3; use GPIO 0-4 or leave it blank"
        )),
    }
}

fn resolve_board_layout(
    board_layout: Option<BoardLayoutInput>,
    legacy_i2c_sda: Option<u8>,
    legacy_i2c_scl: Option<u8>,
) -> Result<Option<BoardLayout>, String> {
    let layout = match board_layout {
        Some(layout) => Some(BoardLayout {
            i2c0_sda: layout.i2c0_sda,
            i2c0_scl: layout.i2c0_scl,
            one_wire_data: layout.one_wire_data,
            battery_adc: layout.battery_adc,
            sensor_enable: layout.sensor_enable,
        }),
        None => resolve_legacy_i2c_layout(legacy_i2c_sda, legacy_i2c_scl)?,
    };

    if let Some(layout) = layout {
        layout.validate().map_err(str::to_string)?;
        validate_supported_battery_adc(&layout)?;
        Ok(Some(layout))
    } else {
        Ok(None)
    }
}

fn device_to_info(d: &ScannedDevice) -> DeviceInfo {
    let svc = service_type(d);
    DeviceInfo {
        address: format_address(&d.address),
        name: d.name.clone(),
        rssi: d.rssi,
        service_type: match svc {
            Some(ServiceType::Gateway) => "Gateway".into(),
            Some(ServiceType::Node) => "Node".into(),
            None => "Unknown".into(),
        },
    }
}

#[cfg(not(target_os = "android"))]
fn load_pairing_artifacts(state: &AppState) -> Result<Arc<phase1::PairingArtifacts>, String> {
    let mut guard = state.pairing_artifacts.lock().unwrap();
    if guard.is_none() {
        let store = FilePairingStore::new().map_err(|e| e.to_string())?;
        match store.load_artifacts() {
            Ok(Some(loaded)) => {
                *guard = Some(Arc::new(loaded));
            }
            Ok(None) => {}
            Err(e) => return Err(format!("failed to load pairing artifacts: {e}")),
        }
    }
    guard
        .clone()
        .ok_or_else(|| "Not paired — run pair_gateway first".to_string())
}

#[cfg(target_os = "android")]
fn load_pairing_artifacts(state: &AppState) -> Result<Arc<phase1::PairingArtifacts>, String> {
    let mut guard = state.pairing_artifacts.lock().unwrap();
    if guard.is_none() {
        let store = AndroidPairingStore::from_cached_vm().map_err(|e| e.to_string())?;
        match store.load_artifacts() {
            Ok(Some(loaded)) => {
                *guard = Some(Arc::new(loaded));
            }
            Ok(None) => {}
            Err(e) => return Err(format!("failed to load pairing artifacts: {e}")),
        }
    }
    guard
        .clone()
        .ok_or_else(|| "Not paired — run pair_gateway first".to_string())
}

// ---------------------------------------------------------------------------
// Tauri commands — desktop (btleplug + FilePairingStore)
// ---------------------------------------------------------------------------

#[cfg(not(target_os = "android"))]
#[tauri::command]
async fn start_scan(state: tauri::State<'_, AppState>) -> Result<(), String> {
    stop_and_discard_scanner(&state.scanner).await?;
    *state.phase.lock().unwrap() = "Scanning".into();

    let scanner = tokio::task::spawn_blocking(|| {
        tokio::runtime::Handle::current().block_on(async {
            let transport = BtleplugTransport::new().await.map_err(|e| e.to_string())?;
            let mut scanner = DeviceScanner::new(transport);
            scanner.start().await.map_err(|e| e.to_string())?;
            Ok::<_, String>(scanner)
        })
    })
    .await
    .map_err(|e| format!("task panicked: {e}"))??;

    *state.scanner.lock().unwrap() = Some(scanner);
    Ok(())
}

#[cfg(not(target_os = "android"))]
#[tauri::command]
async fn stop_scan(state: tauri::State<'_, AppState>) -> Result<(), String> {
    let scanner = {
        state
            .scanner
            .lock()
            .unwrap()
            .take()
            .ok_or_else(|| "not scanning".to_string())?
    };

    let scanner = tokio::task::spawn_blocking(move || {
        let mut scanner = scanner;
        let _ = tokio::runtime::Handle::current().block_on(async { scanner.stop().await });
        scanner
    })
    .await
    .map_err(|e| format!("task panicked: {e}"))?;

    *state.scanner.lock().unwrap() = Some(scanner);
    *state.phase.lock().unwrap() = "Idle".into();
    Ok(())
}

#[cfg(not(target_os = "android"))]
#[tauri::command]
async fn get_devices(state: tauri::State<'_, AppState>) -> Result<Vec<DeviceInfo>, String> {
    let scanner = state
        .scanner
        .lock()
        .unwrap()
        .take()
        .ok_or_else(|| "not scanning".to_string())?;

    let (scanner, devices) = tokio::task::spawn_blocking(move || {
        let mut scanner = scanner;
        let _ = tokio::runtime::Handle::current().block_on(async { scanner.refresh().await });
        let devices: Vec<DeviceInfo> = scanner.devices().iter().map(device_to_info).collect();
        (scanner, devices)
    })
    .await
    .map_err(|e| format!("task panicked: {e}"))?;

    *state.scanner.lock().unwrap() = Some(scanner);
    Ok(devices)
}

#[cfg(not(target_os = "android"))]
#[tauri::command]
async fn pair_gateway(
    state: tauri::State<'_, AppState>,
    address: String,
    phone_label: String,
    _force: Option<bool>,
) -> Result<(), String> {
    stop_and_discard_scanner(&state.scanner).await?;

    let addr = match parse_address(&address) {
        Ok(a) => a,
        Err(e) => {
            *state.phase.lock().unwrap() = format!("Error: {e}");
            return Err(e);
        }
    };

    // Set an immediate initial phase so the UI doesn't show stale state
    // while the blocking task is being spawned.
    *state.phase.lock().unwrap() = "Connecting".into();

    let phase = state.phase.clone();
    let progress = UiPairingProgress { phase };

    let result = tokio::task::spawn_blocking(move || {
        tokio::runtime::Handle::current().block_on(async {
            let mut transport = BtleplugTransport::new().await?;
            let rng = OsRng;
            phase1::pair_with_gateway(&mut transport, &rng, &addr, &phone_label, Some(&progress))
                .await
        })
    })
    .await
    .map_err(|e| format!("task panicked: {e}"))?;

    match result {
        Ok(artifacts) => {
            // Persist to file store so provisioning works across app restarts.
            let store = FilePairingStore::new().map_err(|e| e.to_string())?;
            store
                .save_artifacts(&artifacts)
                .map_err(|e| e.to_string())?;
            *state.pairing_artifacts.lock().unwrap() = Some(Arc::new(artifacts));
            *state.phase.lock().unwrap() = "Complete".into();
            Ok(())
        }
        Err(e) => {
            let msg = e.to_string();
            *state.phase.lock().unwrap() = format!("Error: {msg}");
            Err(msg)
        }
    }
}

#[cfg(not(target_os = "android"))]
#[tauri::command]
async fn provision_node(
    state: tauri::State<'_, AppState>,
    address: String,
    node_id: String,
    board_layout: Option<BoardLayoutInput>,
    i2c_sda: Option<u8>,
    i2c_scl: Option<u8>,
) -> Result<String, String> {
    stop_and_discard_scanner(&state.scanner).await?;

    let addr = match parse_address(&address) {
        Ok(a) => a,
        Err(e) => {
            *state.phase.lock().unwrap() = format!("Error: {e}");
            return Err(e);
        }
    };

    let board_layout = resolve_board_layout(board_layout, i2c_sda, i2c_scl)?;
    let artifacts = load_pairing_artifacts(&state)?;

    *state.phase.lock().unwrap() = "Provisioning".into();
    let session = state.connected_node.lock().unwrap().take();
    let result = tokio::task::spawn_blocking(move || {
        tokio::runtime::Handle::current().block_on(async {
            let rng = OsRng;
            match session {
                Some(mut session) if session.address == addr => {
                    let result = phase2::provision_connected_node(
                        &mut session.transport,
                        &artifacts,
                        &rng,
                        &node_id,
                        &[],
                        board_layout,
                    )
                    .await;
                    let _ = session.transport.disconnect().await;
                    result
                }
                Some(mut session) => {
                    let _ = session.transport.disconnect().await;
                    let mut transport = BtleplugTransport::new().await?;
                    phase2::provision_node(
                        &mut transport,
                        &artifacts,
                        &rng,
                        &addr,
                        &node_id,
                        &[],
                        board_layout,
                    )
                    .await
                }
                None => {
                    let mut transport = BtleplugTransport::new().await?;
                    phase2::provision_node(
                        &mut transport,
                        &artifacts,
                        &rng,
                        &addr,
                        &node_id,
                        &[],
                        board_layout,
                    )
                    .await
                }
            }
        })
    })
    .await
    .map_err(|e| format!("task panicked: {e}"))?;

    match result {
        Ok(r) => {
            *state.phase.lock().unwrap() = "Complete".into();
            Ok(format!("{}", r.status))
        }
        Err(e) => {
            let msg = e.to_string();
            *state.phase.lock().unwrap() = format!("Error: {msg}");
            Err(msg)
        }
    }
}

#[cfg(not(target_os = "android"))]
#[tauri::command]
async fn connect_node(
    state: tauri::State<'_, AppState>,
    address: String,
) -> Result<ConnectedNodeInfo, String> {
    stop_and_discard_scanner(&state.scanner).await?;

    let addr = parse_address(&address).map_err(|e| {
        *state.phase.lock().unwrap() = format!("Error: {e}");
        e
    })?;
    let existing = state.connected_node.lock().unwrap().take();
    *state.phase.lock().unwrap() = "Connecting".into();

    let result: Result<ConnectedNodeSession<BtleplugTransport>, PairingError> =
        tokio::task::spawn_blocking(move || {
            tokio::runtime::Handle::current().block_on(async move {
                reuse_or_connect_session(existing, addr, BtleplugTransport::new).await
            })
        })
        .await
        .map_err(|e| format!("task panicked: {e}"))?;

    match result {
        Ok(session) => {
            *state.connected_node.lock().unwrap() = Some(session);
            *state.signal_check_cancel.lock().unwrap() = None;
            *state.phase.lock().unwrap() = "Connected".into();
            Ok(ConnectedNodeInfo { address })
        }
        Err(e) => {
            let msg = e.to_string();
            *state.phase.lock().unwrap() = format!("Error: {msg}");
            Err(msg)
        }
    }
}

#[cfg(not(target_os = "android"))]
#[tauri::command]
async fn check_rssi(
    state: tauri::State<'_, AppState>,
    address: String,
) -> Result<DiagnosticInfo, String> {
    let artifacts = load_pairing_artifacts(&state)?;
    let addr = parse_address(&address).map_err(|e| {
        *state.phase.lock().unwrap() = format!("Error: {e}");
        e
    })?;
    let session = state.connected_node.lock().unwrap().take();
    let cancel = Arc::new(AtomicBool::new(false));
    *state.signal_check_cancel.lock().unwrap() = Some(cancel.clone());
    *state.phase.lock().unwrap() = "Signal Check".into();

    let result = tokio::task::spawn_blocking(move || {
        tokio::runtime::Handle::current().block_on(async move {
            let session = reuse_or_connect_session(session, addr, BtleplugTransport::new).await?;
            let mut session = session;
            let outcome =
                phase2::check_rssi(&mut session.transport, &artifacts, &addr, cancel.as_ref())
                    .await;
            Ok::<_, PairingError>(finalize_signal_check(session, outcome, cancel.as_ref()).await)
        })
    })
    .await
    .map_err(|e| format!("task panicked: {e}"))?;

    let (session, outcome) = result.map_err(|e| e.to_string())?;
    *state.signal_check_cancel.lock().unwrap() = None;
    *state.connected_node.lock().unwrap() = session;
    match outcome {
        Ok(diag) => {
            *state.phase.lock().unwrap() = "Connected".into();
            Ok(diag)
        }
        Err(PairingError::Cancelled { .. }) => {
            *state.phase.lock().unwrap() = "Idle".into();
            Err("signal check cancelled".into())
        }
        Err(e) => {
            let msg = e.to_string();
            *state.phase.lock().unwrap() = format!("Error: {msg}");
            Err(msg)
        }
    }
}

#[cfg(not(target_os = "android"))]
#[tauri::command]
async fn disconnect_node(state: tauri::State<'_, AppState>) -> Result<(), String> {
    if let Some(cancel) = state.signal_check_cancel.lock().unwrap().as_ref() {
        cancel.store(true, Ordering::Relaxed);
    }
    let session = state.connected_node.lock().unwrap().take();
    if let Some(session) = session {
        tokio::task::spawn_blocking(move || {
            tokio::runtime::Handle::current().block_on(async move {
                let mut session = session;
                session.transport.disconnect().await
            })
        })
        .await
        .map_err(|e| format!("task panicked: {e}"))?
        .map_err(|e| e.to_string())?;
    }
    *state.signal_check_cancel.lock().unwrap() = None;
    *state.phase.lock().unwrap() = "Idle".into();
    Ok(())
}

#[cfg(not(target_os = "android"))]
#[tauri::command]
fn get_pairing_status(state: tauri::State<'_, AppState>) -> Result<PairingStatus, String> {
    let mut paired = state.pairing_artifacts.lock().unwrap().is_some();
    if !paired {
        let store = FilePairingStore::new().map_err(|e| e.to_string())?;
        match store.load_artifacts() {
            Ok(Some(_)) => paired = true,
            Ok(None) => {}
            Err(e) => return Err(format!("failed to check pairing status: {e}")),
        }
    }
    Ok(PairingStatus {
        paired,
        gateway_id: None,
    })
}

#[cfg(not(target_os = "android"))]
#[tauri::command]
async fn clear_pairing(state: tauri::State<'_, AppState>) -> Result<(), String> {
    if let Some(cancel) = state.signal_check_cancel.lock().unwrap().as_ref() {
        cancel.store(true, Ordering::Relaxed);
    }
    let session = state.connected_node.lock().unwrap().take();
    if let Some(session) = session {
        tokio::task::spawn_blocking(move || {
            tokio::runtime::Handle::current().block_on(async move {
                let mut session = session;
                session.transport.disconnect().await
            })
        })
        .await
        .map_err(|e| format!("task panicked: {e}"))?
        .map_err(|e| e.to_string())?;
    }
    *state.signal_check_cancel.lock().unwrap() = None;
    *state.pairing_artifacts.lock().unwrap() = None;
    let store = FilePairingStore::new().map_err(|e| e.to_string())?;
    store.clear().map_err(|e| e.to_string())?;
    *state.phase.lock().unwrap() = "Idle".into();
    Ok(())
}

// ---------------------------------------------------------------------------
// Tauri commands — Android (AndroidBleTransport + AndroidPairingStore)
// ---------------------------------------------------------------------------

#[cfg(target_os = "android")]
#[tauri::command]
async fn start_scan(state: tauri::State<'_, AppState>) -> Result<(), String> {
    stop_and_discard_scanner(&state.scanner).await?;
    *state.phase.lock().unwrap() = "Scanning".into();

    let scanner = tokio::task::spawn_blocking(|| {
        tokio::runtime::Handle::current().block_on(async {
            let transport = AndroidBleTransport::from_cached_vm().map_err(|e| e.to_string())?;
            let mut scanner = DeviceScanner::new(transport);
            scanner.start().await.map_err(|e| e.to_string())?;
            Ok::<_, String>(scanner)
        })
    })
    .await
    .map_err(|e| format!("task panicked: {e}"))??;

    *state.scanner.lock().unwrap() = Some(scanner);
    Ok(())
}

#[cfg(target_os = "android")]
#[tauri::command]
async fn stop_scan(state: tauri::State<'_, AppState>) -> Result<(), String> {
    let scanner = {
        state
            .scanner
            .lock()
            .unwrap()
            .take()
            .ok_or_else(|| "not scanning".to_string())?
    };

    let scanner = tokio::task::spawn_blocking(move || {
        let mut scanner = scanner;
        let _ = tokio::runtime::Handle::current().block_on(async { scanner.stop().await });
        scanner
    })
    .await
    .map_err(|e| format!("task panicked: {e}"))?;

    *state.scanner.lock().unwrap() = Some(scanner);
    *state.phase.lock().unwrap() = "Idle".into();
    Ok(())
}

#[cfg(target_os = "android")]
#[tauri::command]
async fn get_devices(state: tauri::State<'_, AppState>) -> Result<Vec<DeviceInfo>, String> {
    let scanner = state
        .scanner
        .lock()
        .unwrap()
        .take()
        .ok_or_else(|| "not scanning".to_string())?;

    let (scanner, devices) = tokio::task::spawn_blocking(move || {
        let mut scanner = scanner;
        let _ = tokio::runtime::Handle::current().block_on(async { scanner.refresh().await });
        let devices: Vec<DeviceInfo> = scanner.devices().iter().map(device_to_info).collect();
        (scanner, devices)
    })
    .await
    .map_err(|e| format!("task panicked: {e}"))?;

    *state.scanner.lock().unwrap() = Some(scanner);
    Ok(devices)
}

#[cfg(target_os = "android")]
#[tauri::command]
async fn pair_gateway(
    state: tauri::State<'_, AppState>,
    address: String,
    phone_label: String,
    _force: Option<bool>,
) -> Result<(), String> {
    stop_and_discard_scanner(&state.scanner).await?;

    let addr = match parse_address(&address) {
        Ok(a) => a,
        Err(e) => {
            *state.phase.lock().unwrap() = format!("Error: {e}");
            return Err(e);
        }
    };

    *state.phase.lock().unwrap() = "Connecting".into();

    let phase = state.phase.clone();
    let progress = UiPairingProgress { phase };

    let result = tokio::task::spawn_blocking(move || {
        tokio::runtime::Handle::current().block_on(async {
            let mut transport = AndroidBleTransport::from_cached_vm()?;
            let rng = OsRng;
            phase1::pair_with_gateway(&mut transport, &rng, &addr, &phone_label, Some(&progress))
                .await
        })
    })
    .await
    .map_err(|e| format!("task panicked: {e}"))?;

    match result {
        Ok(artifacts) => {
            // Persist to Android secure storage so provisioning works across
            // app restarts (PT-0800, PT-0801).
            let mut store = match AndroidPairingStore::from_cached_vm() {
                Ok(s) => s,
                Err(e) => {
                    let msg = e.to_string();
                    *state.phase.lock().unwrap() = format!("Error: {msg}");
                    return Err(msg);
                }
            };
            if let Err(e) = store.save_artifacts(&artifacts) {
                // Best-effort clear to prevent mixed old+new state (e.g., new
                // phone_psk committed but old phone_key_hint left from a
                // previous pairing).  Also clear the in-memory cache so it
                // stays consistent with the (now-empty) persistent store.
                let _ = store.clear();
                *state.pairing_artifacts.lock().unwrap() = None;
                let msg = e.to_string();
                *state.phase.lock().unwrap() = format!("Error: {msg}");
                return Err(msg);
            }
            *state.pairing_artifacts.lock().unwrap() = Some(Arc::new(artifacts));
            *state.phase.lock().unwrap() = "Complete".into();
            Ok(())
        }
        Err(e) => {
            let msg = e.to_string();
            *state.phase.lock().unwrap() = format!("Error: {msg}");
            Err(msg)
        }
    }
}

#[cfg(target_os = "android")]
#[tauri::command]
async fn provision_node(
    state: tauri::State<'_, AppState>,
    address: String,
    node_id: String,
    board_layout: Option<BoardLayoutInput>,
    i2c_sda: Option<u8>,
    i2c_scl: Option<u8>,
) -> Result<String, String> {
    stop_and_discard_scanner(&state.scanner).await?;

    let addr = match parse_address(&address) {
        Ok(a) => a,
        Err(e) => {
            *state.phase.lock().unwrap() = format!("Error: {e}");
            return Err(e);
        }
    };

    let board_layout = resolve_board_layout(board_layout, i2c_sda, i2c_scl)?;
    let artifacts = load_pairing_artifacts(&state)?;

    *state.phase.lock().unwrap() = "Provisioning".into();
    let session = state.connected_node.lock().unwrap().take();
    let result = tokio::task::spawn_blocking(move || {
        tokio::runtime::Handle::current().block_on(async {
            let rng = OsRng;
            match session {
                Some(mut session) if session.address == addr => {
                    let result = phase2::provision_connected_node(
                        &mut session.transport,
                        &artifacts,
                        &rng,
                        &node_id,
                        &[],
                        board_layout,
                    )
                    .await;
                    let _ = session.transport.disconnect().await;
                    result
                }
                Some(mut session) => {
                    let _ = session.transport.disconnect().await;
                    let mut transport = AndroidBleTransport::from_cached_vm()?;
                    phase2::provision_node(
                        &mut transport,
                        &artifacts,
                        &rng,
                        &addr,
                        &node_id,
                        &[],
                        board_layout,
                    )
                    .await
                }
                None => {
                    let mut transport = AndroidBleTransport::from_cached_vm()?;
                    phase2::provision_node(
                        &mut transport,
                        &artifacts,
                        &rng,
                        &addr,
                        &node_id,
                        &[],
                        board_layout,
                    )
                    .await
                }
            }
        })
    })
    .await
    .map_err(|e| format!("task panicked: {e}"))?;

    match result {
        Ok(r) => {
            *state.phase.lock().unwrap() = "Complete".into();
            Ok(format!("{}", r.status))
        }
        Err(e) => {
            let msg = e.to_string();
            *state.phase.lock().unwrap() = format!("Error: {msg}");
            Err(msg)
        }
    }
}

#[cfg(target_os = "android")]
#[tauri::command]
async fn connect_node(
    state: tauri::State<'_, AppState>,
    address: String,
) -> Result<ConnectedNodeInfo, String> {
    stop_and_discard_scanner(&state.scanner).await?;

    let addr = parse_address(&address).map_err(|e| {
        *state.phase.lock().unwrap() = format!("Error: {e}");
        e
    })?;
    let existing = state.connected_node.lock().unwrap().take();
    *state.phase.lock().unwrap() = "Connecting".into();

    let result: Result<ConnectedNodeSession<AndroidBleTransport>, PairingError> =
        tokio::task::spawn_blocking(move || {
            tokio::runtime::Handle::current().block_on(async move {
                reuse_or_connect_session(existing, addr, || async {
                    AndroidBleTransport::from_cached_vm()
                })
                .await
            })
        })
        .await
        .map_err(|e| format!("task panicked: {e}"))?;

    match result {
        Ok(session) => {
            *state.connected_node.lock().unwrap() = Some(session);
            *state.signal_check_cancel.lock().unwrap() = None;
            *state.phase.lock().unwrap() = "Connected".into();
            Ok(ConnectedNodeInfo { address })
        }
        Err(e) => {
            let msg = e.to_string();
            *state.phase.lock().unwrap() = format!("Error: {msg}");
            Err(msg)
        }
    }
}

#[cfg(target_os = "android")]
#[tauri::command]
async fn check_rssi(
    state: tauri::State<'_, AppState>,
    address: String,
) -> Result<DiagnosticInfo, String> {
    let artifacts = load_pairing_artifacts(&state)?;
    let addr = parse_address(&address).map_err(|e| {
        *state.phase.lock().unwrap() = format!("Error: {e}");
        e
    })?;
    let session = state.connected_node.lock().unwrap().take();
    let cancel = Arc::new(AtomicBool::new(false));
    *state.signal_check_cancel.lock().unwrap() = Some(cancel.clone());
    *state.phase.lock().unwrap() = "Signal Check".into();

    let result = tokio::task::spawn_blocking(move || {
        tokio::runtime::Handle::current().block_on(async move {
            let session = reuse_or_connect_session(session, addr, || async {
                AndroidBleTransport::from_cached_vm()
            })
            .await?;
            let mut session = session;
            let outcome =
                phase2::check_rssi(&mut session.transport, &artifacts, &addr, cancel.as_ref())
                    .await;
            Ok::<_, PairingError>(finalize_signal_check(session, outcome, cancel.as_ref()).await)
        })
    })
    .await
    .map_err(|e| format!("task panicked: {e}"))?;

    let (session, outcome) = result.map_err(|e| e.to_string())?;
    *state.signal_check_cancel.lock().unwrap() = None;
    *state.connected_node.lock().unwrap() = session;
    match outcome {
        Ok(diag) => {
            *state.phase.lock().unwrap() = "Connected".into();
            Ok(diag)
        }
        Err(PairingError::Cancelled { .. }) => {
            *state.phase.lock().unwrap() = "Idle".into();
            Err("signal check cancelled".into())
        }
        Err(e) => {
            let msg = e.to_string();
            *state.phase.lock().unwrap() = format!("Error: {msg}");
            Err(msg)
        }
    }
}

#[cfg(target_os = "android")]
#[tauri::command]
async fn disconnect_node(state: tauri::State<'_, AppState>) -> Result<(), String> {
    if let Some(cancel) = state.signal_check_cancel.lock().unwrap().as_ref() {
        cancel.store(true, Ordering::Relaxed);
    }
    let session = state.connected_node.lock().unwrap().take();
    if let Some(session) = session {
        tokio::task::spawn_blocking(move || {
            tokio::runtime::Handle::current().block_on(async move {
                let mut session = session;
                session.transport.disconnect().await
            })
        })
        .await
        .map_err(|e| format!("task panicked: {e}"))?
        .map_err(|e| e.to_string())?;
    }
    *state.signal_check_cancel.lock().unwrap() = None;
    *state.phase.lock().unwrap() = "Idle".into();
    Ok(())
}

#[cfg(target_os = "android")]
#[tauri::command]
fn get_pairing_status(state: tauri::State<'_, AppState>) -> Result<PairingStatus, String> {
    let mut paired = state.pairing_artifacts.lock().unwrap().is_some();
    if !paired {
        let store = AndroidPairingStore::from_cached_vm().map_err(|e| e.to_string())?;
        match store.load_artifacts() {
            Ok(Some(_)) => paired = true,
            Ok(None) => {}
            Err(e) => return Err(format!("failed to check pairing status: {e}")),
        }
    }
    Ok(PairingStatus {
        paired,
        gateway_id: None,
    })
}

#[cfg(target_os = "android")]
#[tauri::command]
async fn clear_pairing(state: tauri::State<'_, AppState>) -> Result<(), String> {
    if let Some(cancel) = state.signal_check_cancel.lock().unwrap().as_ref() {
        cancel.store(true, Ordering::Relaxed);
    }
    let session = state.connected_node.lock().unwrap().take();
    if let Some(session) = session {
        tokio::task::spawn_blocking(move || {
            tokio::runtime::Handle::current().block_on(async move {
                let mut session = session;
                session.transport.disconnect().await
            })
        })
        .await
        .map_err(|e| format!("task panicked: {e}"))?
        .map_err(|e| e.to_string())?;
    }
    let mut store = AndroidPairingStore::from_cached_vm().map_err(|e| e.to_string())?;
    store.clear().map_err(|e| e.to_string())?;
    *state.signal_check_cancel.lock().unwrap() = None;
    *state.pairing_artifacts.lock().unwrap() = None;
    *state.phase.lock().unwrap() = "Idle".into();
    Ok(())
}

// ---------------------------------------------------------------------------
// Platform-independent commands
// ---------------------------------------------------------------------------

#[tauri::command]
fn get_phase(state: tauri::State<'_, AppState>) -> String {
    state.phase.lock().unwrap().clone()
}

#[tauri::command]
fn get_logs(state: tauri::State<'_, AppState>) -> Vec<String> {
    std::mem::take(&mut *state.logs.lock().unwrap())
}

#[tauri::command]
fn cancel_signal_check(state: tauri::State<'_, AppState>) {
    if let Some(cancel) = state.signal_check_cancel.lock().unwrap().as_ref() {
        cancel.store(true, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------
// Tracing subscriber that captures log output for the verbose panel
// ---------------------------------------------------------------------------

mod log_capture {
    use std::io;
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    pub struct LogMakeWriter(pub Arc<Mutex<Vec<String>>>);

    pub struct LogWriter {
        buf: Vec<u8>,
        dest: Arc<Mutex<Vec<String>>>,
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogMakeWriter {
        type Writer = LogWriter;

        fn make_writer(&'a self) -> Self::Writer {
            LogWriter {
                buf: Vec::new(),
                dest: self.0.clone(),
            }
        }
    }

    impl io::Write for LogWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.buf.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Drop for LogWriter {
        fn drop(&mut self) {
            if !self.buf.is_empty() {
                let msg = String::from_utf8_lossy(&self.buf).trim_end().to_string();
                if !msg.is_empty() {
                    self.dest.lock().unwrap().push(msg);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Android JNI initialisation
// ---------------------------------------------------------------------------

/// Called by the Android runtime when this native library is loaded.
/// Caches the `JavaVM` and resolves app-defined Java classes while we are
/// on the main thread (which has the application classloader).  Natively-
/// attached threads (e.g. tokio blocking pool) only see the system
/// classloader, so `FindClass` for app classes would fail there.
#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn JNI_OnLoad(
    vm: *mut jni::sys::JavaVM,
    _reserved: *mut std::ffi::c_void,
) -> jni::sys::jint {
    // Wrap the fallible body so we can return JNI_ERR on failure instead
    // of panicking (unwinding across extern "system" is UB).
    match jni_on_load_inner(vm) {
        Ok(ver) => ver,
        Err(_) => jni::sys::JNI_ERR,
    }
}

#[cfg(target_os = "android")]
fn jni_on_load_inner(
    vm: *mut jni::sys::JavaVM,
) -> Result<jni::sys::jint, Box<dyn std::error::Error>> {
    let vm = unsafe { jni::JavaVM::from_raw(vm) };
    AndroidBleTransport::cache_vm(vm.clone());
    AndroidPairingStore::cache_vm(vm.clone());

    // Resolve app-defined classes on the main thread.
    vm.attach_current_thread(|env| -> Result<(), Box<dyn std::error::Error>> {
        AndroidBleTransport::cache_helper_class(env).map_err(
            |e| -> Box<dyn std::error::Error> { format!("cache BleHelper: {e}").into() },
        )?;
        AndroidPairingStore::cache_store_class(env).map_err(|e| -> Box<dyn std::error::Error> {
            format!("cache SecureStore: {e}").into()
        })?;
        Ok(())
    })?;

    Ok(jni::JNIVersion::V1_6.into())
}

// ---------------------------------------------------------------------------
// App entry point
// ---------------------------------------------------------------------------

#[cfg(mobile)]
#[tauri::mobile_entry_point]
fn main() {
    run();
}

pub fn run() {
    let logs = Arc::new(Mutex::new(Vec::<String>::new()));

    use tracing_subscriber::prelude::*;
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(log_capture::LogMakeWriter(logs.clone()))
                .with_ansi(false)
                .with_target(true)
                .with_level(true),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_ansi(false)
                .with_target(true)
                .with_level(true),
        )
        .with({
            #[cfg(debug_assertions)]
            const DEFAULT_FILTER: &str = "sonde_pair=info,sonde_pair_ui=info";
            #[cfg(not(debug_assertions))]
            const DEFAULT_FILTER: &str = "sonde_pair=warn,sonde_pair_ui=warn";

            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| DEFAULT_FILTER.into())
        })
        .init();

    let state = AppState {
        scanner: Mutex::new(None),
        connected_node: Mutex::new(None),
        signal_check_cancel: Mutex::new(None),
        phase: Arc::new(Mutex::new("Idle".into())),
        logs,
        pairing_artifacts: Mutex::new(None),
    };

    tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            start_scan,
            stop_scan,
            get_devices,
            pair_gateway,
            connect_node,
            check_rssi,
            disconnect_node,
            provision_node,
            get_phase,
            get_pairing_status,
            clear_pairing,
            get_logs,
            cancel_signal_check,
        ])
        .run(tauri::generate_context!())
        .expect("error running Sonde Pairing Tool");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use sonde_pair::error::PairingError;
    use sonde_pair::transport::{BleTransport, MockBleTransport};
    use sonde_pair::types::{PairingMethod, ScannedDevice};
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    struct CountingTransport {
        inner: MockBleTransport,
        disconnects: Arc<AtomicUsize>,
        stop_scans: Arc<AtomicUsize>,
    }

    impl CountingTransport {
        fn new(mtu: u16, disconnects: Arc<AtomicUsize>, stop_scans: Arc<AtomicUsize>) -> Self {
            Self {
                inner: MockBleTransport::new(mtu),
                disconnects,
                stop_scans,
            }
        }
    }

    impl BleTransport for CountingTransport {
        fn start_scan(
            &mut self,
            service_uuids: &[u128],
        ) -> Pin<Box<dyn Future<Output = Result<(), PairingError>> + '_>> {
            self.inner.start_scan(service_uuids)
        }

        fn stop_scan(&mut self) -> Pin<Box<dyn Future<Output = Result<(), PairingError>> + '_>> {
            self.stop_scans.fetch_add(1, AtomicOrdering::Relaxed);
            self.inner.stop_scan()
        }

        fn get_discovered_devices(
            &self,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<ScannedDevice>, PairingError>> + '_>> {
            self.inner.get_discovered_devices()
        }

        fn connect(
            &mut self,
            address: &[u8; 6],
        ) -> Pin<Box<dyn Future<Output = Result<u16, PairingError>> + '_>> {
            self.inner.connect(address)
        }

        fn disconnect(&mut self) -> Pin<Box<dyn Future<Output = Result<(), PairingError>> + '_>> {
            self.disconnects.fetch_add(1, AtomicOrdering::Relaxed);
            self.inner.disconnect()
        }

        fn write_characteristic(
            &mut self,
            service: u128,
            characteristic: u128,
            data: &[u8],
        ) -> Pin<Box<dyn Future<Output = Result<(), PairingError>> + '_>> {
            self.inner
                .write_characteristic(service, characteristic, data)
        }

        fn read_indication(
            &mut self,
            service: u128,
            characteristic: u128,
            timeout_ms: u64,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, PairingError>> + '_>> {
            self.inner
                .read_indication(service, characteristic, timeout_ms)
        }

        fn pairing_method(&self) -> Option<PairingMethod> {
            self.inner.pairing_method()
        }

        fn set_defer_bonding(&mut self, defer: bool) {
            self.inner.set_defer_bonding(defer);
        }
    }

    fn run_async_test<F>(future: F)
    where
        F: Future<Output = ()>,
    {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future);
    }

    #[test]
    fn resolve_legacy_i2c_layout_both_present() {
        let result = resolve_legacy_i2c_layout(Some(5), Some(6)).unwrap();
        let layout = result.unwrap();
        assert_eq!(layout.i2c0_sda, Some(5));
        assert_eq!(layout.i2c0_scl, Some(6));
    }

    #[test]
    fn resolve_legacy_i2c_layout_both_absent() {
        let result = resolve_legacy_i2c_layout(None, None).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn resolve_legacy_i2c_layout_only_sda_rejected() {
        let result = resolve_legacy_i2c_layout(Some(5), None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("both"));
    }

    #[test]
    fn resolve_legacy_i2c_layout_only_scl_rejected() {
        let result = resolve_legacy_i2c_layout(None, Some(6));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("both"));
    }

    #[test]
    fn resolve_board_layout_validates_custom_board() {
        let result = resolve_board_layout(
            Some(BoardLayoutInput {
                i2c0_sda: Some(6),
                i2c0_scl: Some(7),
                one_wire_data: Some(3),
                battery_adc: Some(2),
                sensor_enable: Some(4),
            }),
            None,
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(result, BoardLayout::SONDE_SENSOR_NODE_REV_A);
    }

    #[test]
    fn resolve_board_layout_rejects_half_i2c_assignment() {
        let result = resolve_board_layout(
            Some(BoardLayoutInput {
                i2c0_sda: Some(5),
                i2c0_scl: None,
                one_wire_data: None,
                battery_adc: None,
                sensor_enable: None,
            }),
            None,
            None,
        );
        assert!(result.is_err());
    }

    #[test]
    fn resolve_board_layout_rejects_non_adc_battery_pin() {
        let result = resolve_board_layout(
            Some(BoardLayoutInput {
                i2c0_sda: Some(6),
                i2c0_scl: Some(7),
                one_wire_data: None,
                battery_adc: Some(7),
                sensor_enable: None,
            }),
            None,
            None,
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("ADC-capable"));
    }

    #[test]
    fn stop_and_discard_scanner_stops_active_scan() {
        run_async_test(async {
            let disconnects = Arc::new(AtomicUsize::new(0));
            let stop_scans = Arc::new(AtomicUsize::new(0));
            let transport = CountingTransport::new(247, disconnects, stop_scans.clone());
            let mut scanner = DeviceScanner::new(transport);
            scanner.start().await.unwrap();
            let scanner_slot = Mutex::new(Some(scanner));

            stop_and_discard_scanner(&scanner_slot).await.unwrap();

            assert!(scanner_slot.lock().unwrap().is_none());
            assert_eq!(stop_scans.load(AtomicOrdering::Relaxed), 1);
        });
    }

    #[test]
    fn reuse_or_connect_session_reuses_matching_session() {
        run_async_test(async {
            let disconnects = Arc::new(AtomicUsize::new(0));
            let stop_scans = Arc::new(AtomicUsize::new(0));
            let transport = CountingTransport::new(247, disconnects.clone(), stop_scans);
            let existing = ConnectedNodeSession {
                address: [0xAA; 6],
                transport,
            };

            let session = reuse_or_connect_session(Some(existing), [0xAA; 6], || async {
                Ok(CountingTransport::new(
                    247,
                    Arc::new(AtomicUsize::new(0)),
                    Arc::new(AtomicUsize::new(0)),
                ))
            })
            .await
            .unwrap();

            assert_eq!(session.address, [0xAA; 6]);
            assert_eq!(disconnects.load(AtomicOrdering::Relaxed), 0);
        });
    }

    #[test]
    fn reuse_or_connect_session_disconnects_stale_session() {
        run_async_test(async {
            let old_disconnects = Arc::new(AtomicUsize::new(0));
            let old_stop_scans = Arc::new(AtomicUsize::new(0));
            let mut old_transport =
                CountingTransport::new(247, old_disconnects.clone(), old_stop_scans);
            old_transport.inner.connected = true;
            let existing = ConnectedNodeSession {
                address: [0x11; 6],
                transport: old_transport,
            };
            let new_disconnects = Arc::new(AtomicUsize::new(0));
            let new_stop_scans = Arc::new(AtomicUsize::new(0));

            let session = reuse_or_connect_session(Some(existing), [0x22; 6], || async {
                Ok(CountingTransport::new(
                    247,
                    new_disconnects.clone(),
                    new_stop_scans.clone(),
                ))
            })
            .await
            .unwrap();

            assert_eq!(old_disconnects.load(AtomicOrdering::Relaxed), 1);
            assert_eq!(session.address, [0x22; 6]);
            assert!(session.transport.inner.connected);
        });
    }

    #[test]
    fn finalize_signal_check_retains_session_for_diagnostic_failure() {
        run_async_test(async {
            let disconnects = Arc::new(AtomicUsize::new(0));
            let stop_scans = Arc::new(AtomicUsize::new(0));
            let cancelled = AtomicBool::new(false);
            let session = ConnectedNodeSession {
                address: [0xAA; 6],
                transport: CountingTransport::new(247, disconnects.clone(), stop_scans),
            };

            let (session, outcome) = finalize_signal_check(
                session,
                Err(PairingError::DiagnosticFailed("timed out".into())),
                &cancelled,
            )
            .await;

            assert!(session.is_some());
            assert!(matches!(outcome, Err(PairingError::DiagnosticFailed(_))));
            assert_eq!(disconnects.load(AtomicOrdering::Relaxed), 0);
        });
    }

    #[test]
    fn finalize_signal_check_drops_session_when_cancelled_late() {
        run_async_test(async {
            let disconnects = Arc::new(AtomicUsize::new(0));
            let stop_scans = Arc::new(AtomicUsize::new(0));
            let cancelled = AtomicBool::new(true);
            let session = ConnectedNodeSession {
                address: [0xAA; 6],
                transport: CountingTransport::new(247, disconnects.clone(), stop_scans),
            };

            let (session, outcome) = finalize_signal_check(
                session,
                Ok(phase2::DiagnosticResult {
                    rssi_dbm: -55,
                    signal_quality: 0,
                }),
                &cancelled,
            )
            .await;

            assert!(session.is_none());
            assert!(matches!(outcome, Err(PairingError::Cancelled { .. })));
            assert_eq!(disconnects.load(AtomicOrdering::Relaxed), 1);
        });
    }
}
