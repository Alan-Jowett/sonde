// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Tauri v2 backend for the Sonde BLE pairing tool.
//!
//! All BLE operations use `spawn_blocking` + `Handle::block_on` so that
//! non-Send futures from [`sonde_pair::transport::BleTransport`] work on
//! the tokio multi-threaded runtime.

use std::sync::{Arc, Mutex};

use serde::Serialize;
use sonde_pair::btleplug_transport::BtleplugTransport;
use sonde_pair::discovery::{service_type, DeviceScanner, ServiceType};
use sonde_pair::file_store::FilePairingStore;
use sonde_pair::rng::OsRng;
use sonde_pair::store::PairingStore;
use sonde_pair::types::ScannedDevice;
use sonde_pair::{phase1, phase2};

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

struct AppState {
    scanner: Mutex<Option<DeviceScanner<BtleplugTransport>>>,
    phase: Mutex<String>,
    logs: Arc<Mutex<Vec<String>>>,
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

/// Run a closure containing non-Send BLE futures on a blocking thread.
#[allow(dead_code)]
async fn ble_block<T, F>(f: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, sonde_pair::error::PairingError> + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let handle = tokio::runtime::Handle::current();
        handle.block_on(async { f() })
    })
    .await
    .map_err(|e| format!("task panicked: {e}"))?
    .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------

#[tauri::command]
async fn start_scan(state: tauri::State<'_, AppState>) -> Result<(), String> {
    // Drop any existing scanner.
    *state.scanner.lock().unwrap() = None;
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

#[tauri::command]
async fn stop_scan(state: tauri::State<'_, AppState>) -> Result<(), String> {
    let mut scanner = {
        state
            .scanner
            .lock()
            .unwrap()
            .take()
            .ok_or_else(|| "not scanning".to_string())?
    };

    let scanner = tokio::task::spawn_blocking(move || {
        let _ = tokio::runtime::Handle::current().block_on(async { scanner.stop().await });
        scanner
    })
    .await
    .map_err(|e| format!("task panicked: {e}"))?;

    *state.scanner.lock().unwrap() = Some(scanner);
    *state.phase.lock().unwrap() = "Idle".into();
    Ok(())
}

#[tauri::command]
async fn get_devices(state: tauri::State<'_, AppState>) -> Result<Vec<DeviceInfo>, String> {
    let mut scanner = state
        .scanner
        .lock()
        .unwrap()
        .take()
        .ok_or_else(|| "not scanning".to_string())?;

    let (scanner, devices) = tokio::task::spawn_blocking(move || {
        let _ = tokio::runtime::Handle::current().block_on(async { scanner.refresh().await });
        let devices: Vec<DeviceInfo> = scanner.devices().iter().map(device_to_info).collect();
        (scanner, devices)
    })
    .await
    .map_err(|e| format!("task panicked: {e}"))?;

    *state.scanner.lock().unwrap() = Some(scanner);
    Ok(devices)
}

#[tauri::command]
async fn pair_gateway(
    state: tauri::State<'_, AppState>,
    address: String,
    phone_label: String,
) -> Result<(), String> {
    // Drop scanner — we're done scanning.
    *state.scanner.lock().unwrap() = None;
    *state.phase.lock().unwrap() = "Pairing".into();

    let addr = parse_address(&address)?;

    let result = tokio::task::spawn_blocking(move || {
        tokio::runtime::Handle::current().block_on(async {
            let mut transport = BtleplugTransport::new().await?;
            let mut store = FilePairingStore::new()?;
            let rng = OsRng;
            phase1::pair_with_gateway(&mut transport, &mut store, &rng, &addr, &phone_label).await
        })
    })
    .await
    .map_err(|e| format!("task panicked: {e}"))?;

    match result {
        Ok(_) => {
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

#[tauri::command]
async fn provision_node(
    state: tauri::State<'_, AppState>,
    address: String,
    node_id: String,
) -> Result<String, String> {
    // Drop scanner — we're done scanning.
    *state.scanner.lock().unwrap() = None;
    *state.phase.lock().unwrap() = "Provisioning".into();

    let addr = parse_address(&address)?;

    let result = tokio::task::spawn_blocking(move || {
        tokio::runtime::Handle::current().block_on(async {
            let mut transport = BtleplugTransport::new().await?;
            let store = FilePairingStore::new()?;
            let rng = OsRng;
            phase2::provision_node(&mut transport, &store, &rng, &addr, &node_id, &[]).await
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

#[tauri::command]
fn get_phase(state: tauri::State<'_, AppState>) -> String {
    state.phase.lock().unwrap().clone()
}

#[tauri::command]
fn get_pairing_status() -> Result<PairingStatus, String> {
    let store = FilePairingStore::new().map_err(|e| e.to_string())?;
    let identity = store.load_gateway_identity().map_err(|e| e.to_string())?;
    Ok(PairingStatus {
        paired: identity.is_some(),
        gateway_id: identity.map(|id| hex::encode(id.gateway_id)),
    })
}

#[tauri::command]
fn clear_pairing(state: tauri::State<'_, AppState>) -> Result<(), String> {
    let mut store = FilePairingStore::new().map_err(|e| e.to_string())?;
    store.clear().map_err(|e| e.to_string())?;
    *state.phase.lock().unwrap() = "Idle".into();
    Ok(())
}

#[tauri::command]
fn get_logs(state: tauri::State<'_, AppState>) -> Vec<String> {
    std::mem::take(&mut *state.logs.lock().unwrap())
}

// ---------------------------------------------------------------------------
// Tracing subscriber that captures log output for the verbose panel
// ---------------------------------------------------------------------------

mod log_capture {
    use std::io;
    use std::sync::{Arc, Mutex};

    /// A [`tracing_subscriber::fmt::MakeWriter`] that collects formatted log
    /// lines into a shared buffer.
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
// Hex encoding (tiny helper — avoids adding a hex crate dep)
// ---------------------------------------------------------------------------

mod hex {
    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        bytes.as_ref().iter().map(|b| format!("{b:02x}")).collect()
    }
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

    // Install tracing subscriber that captures output for the verbose panel.
    use tracing_subscriber::prelude::*;
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(log_capture::LogMakeWriter(logs.clone()))
                .with_ansi(false)
                .with_target(true)
                .with_level(true),
        )
        .with(tracing_subscriber::EnvFilter::new(
            "sonde_pair=debug,sonde_pair_ui=debug",
        ))
        .init();

    let state = AppState {
        scanner: Mutex::new(None),
        phase: Mutex::new("Idle".into()),
        logs,
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            start_scan,
            stop_scan,
            get_devices,
            pair_gateway,
            provision_node,
            get_phase,
            get_pairing_status,
            clear_pairing,
            get_logs,
        ])
        .run(tauri::generate_context!())
        .expect("error running Sonde Pairing Tool");
}
