// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Tauri v2 backend for the Sonde BLE pairing tool.
//!
//! On desktop, BLE operations use [`BtleplugTransport`] and [`FilePairingStore`].
//! On Android, BLE operations use [`AndroidBleTransport`] and [`AndroidPairingStore`],
//! initialized from the Tauri Android activity via JNI.
//!
//! All BLE operations use `spawn_blocking` + `Handle::block_on` so that
//! non-Send futures from [`sonde_pair::transport::BleTransport`] work on
//! the tokio multi-threaded runtime.

use std::sync::{Arc, Mutex};

use serde::Serialize;
use sonde_pair::discovery::{service_type, DeviceScanner, ServiceType};
use sonde_pair::rng::OsRng;
use sonde_pair::store::PairingStore;
use sonde_pair::types::ScannedDevice;
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
    #[cfg(target_os = "android")]
    store: Mutex<Option<AndroidPairingStore>>,
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

// ---------------------------------------------------------------------------
// Tauri commands — desktop (btleplug + FilePairingStore)
// ---------------------------------------------------------------------------

#[cfg(not(target_os = "android"))]
#[tauri::command]
async fn start_scan(state: tauri::State<'_, AppState>) -> Result<(), String> {
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
    force: Option<bool>,
) -> Result<(), String> {
    *state.scanner.lock().unwrap() = None;

    // PT-0601: check for existing pairing and require explicit confirmation.
    if !force.unwrap_or(false) {
        let store = FilePairingStore::new().map_err(|e| e.to_string())?;
        if let Some(identity) = store.load_gateway_identity().map_err(|e| e.to_string())? {
            let gw_hex = hex::encode(identity.gateway_id);
            *state.phase.lock().unwrap() = format!("Error: already_paired:{gw_hex}");
            return Err(format!("already_paired:{gw_hex}"));
        }
    }

    let addr = match parse_address(&address) {
        Ok(a) => a,
        Err(e) => {
            *state.phase.lock().unwrap() = format!("Error: {e}");
            return Err(e);
        }
    };

    *state.phase.lock().unwrap() = "Connecting".into();

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

#[cfg(not(target_os = "android"))]
#[tauri::command]
async fn provision_node(
    state: tauri::State<'_, AppState>,
    address: String,
    node_id: String,
) -> Result<String, String> {
    *state.scanner.lock().unwrap() = None;

    let addr = match parse_address(&address) {
        Ok(a) => a,
        Err(e) => {
            *state.phase.lock().unwrap() = format!("Error: {e}");
            return Err(e);
        }
    };

    *state.phase.lock().unwrap() = "Provisioning".into();

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

#[cfg(not(target_os = "android"))]
#[tauri::command]
fn get_pairing_status() -> Result<PairingStatus, String> {
    let store = FilePairingStore::new().map_err(|e| e.to_string())?;
    let identity = store.load_gateway_identity().map_err(|e| e.to_string())?;
    Ok(PairingStatus {
        paired: identity.is_some(),
        gateway_id: identity.map(|id| hex::encode(id.gateway_id)),
    })
}

#[cfg(not(target_os = "android"))]
#[tauri::command]
fn clear_pairing(state: tauri::State<'_, AppState>) -> Result<(), String> {
    let mut store = FilePairingStore::new().map_err(|e| e.to_string())?;
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
    *state.scanner.lock().unwrap() = None;
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
    force: Option<bool>,
) -> Result<(), String> {
    *state.scanner.lock().unwrap() = None;

    // PT-0601: check for existing pairing and require explicit confirmation.
    if !force.unwrap_or(false) {
        let guard = get_or_init_store(&state.store)?;
        let store = guard.as_ref().unwrap();
        if let Some(identity) = store.load_gateway_identity().map_err(|e| e.to_string())? {
            let gw_hex = hex::encode(identity.gateway_id);
            *state.phase.lock().unwrap() = format!("Error: already_paired:{gw_hex}");
            return Err(format!("already_paired:{gw_hex}"));
        }
    }

    let addr = match parse_address(&address) {
        Ok(a) => a,
        Err(e) => {
            *state.phase.lock().unwrap() = format!("Error: {e}");
            return Err(e);
        }
    };

    *state.phase.lock().unwrap() = "Connecting".into();

    let result = tokio::task::spawn_blocking(move || {
        tokio::runtime::Handle::current().block_on(async {
            let mut transport = AndroidBleTransport::from_cached_vm()?;
            let mut store = AndroidPairingStore::from_cached_vm()?;
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

#[cfg(target_os = "android")]
#[tauri::command]
async fn provision_node(
    state: tauri::State<'_, AppState>,
    address: String,
    node_id: String,
) -> Result<String, String> {
    *state.scanner.lock().unwrap() = None;

    let addr = match parse_address(&address) {
        Ok(a) => a,
        Err(e) => {
            *state.phase.lock().unwrap() = format!("Error: {e}");
            return Err(e);
        }
    };

    *state.phase.lock().unwrap() = "Provisioning".into();

    let result = tokio::task::spawn_blocking(move || {
        tokio::runtime::Handle::current().block_on(async {
            let mut transport = AndroidBleTransport::from_cached_vm()?;
            let store = AndroidPairingStore::from_cached_vm()?;
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

#[cfg(target_os = "android")]
fn get_or_init_store(
    store_lock: &Mutex<Option<AndroidPairingStore>>,
) -> Result<std::sync::MutexGuard<'_, Option<AndroidPairingStore>>, String> {
    let mut guard = store_lock.lock().unwrap();
    if guard.is_none() {
        *guard = Some(AndroidPairingStore::from_cached_vm().map_err(|e| e.to_string())?);
    }
    Ok(guard)
}

#[cfg(target_os = "android")]
#[tauri::command]
fn get_pairing_status(state: tauri::State<'_, AppState>) -> Result<PairingStatus, String> {
    let guard = get_or_init_store(&state.store)?;
    let store = guard.as_ref().unwrap();
    let identity = store.load_gateway_identity().map_err(|e| e.to_string())?;
    Ok(PairingStatus {
        paired: identity.is_some(),
        gateway_id: identity.map(|id| hex::encode(id.gateway_id)),
    })
}

#[cfg(target_os = "android")]
#[tauri::command]
fn clear_pairing(state: tauri::State<'_, AppState>) -> Result<(), String> {
    let mut guard = get_or_init_store(&state.store)?;
    let store = guard.as_mut().unwrap();
    store.clear().map_err(|e| e.to_string())?;
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

mod hex {
    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        bytes.as_ref().iter().map(|b| format!("{b:02x}")).collect()
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
    let vm1 = unsafe { jni::JavaVM::from_raw(vm)? };
    let vm2 = unsafe { jni::JavaVM::from_raw(vm)? };
    AndroidBleTransport::cache_vm(vm1);
    AndroidPairingStore::cache_vm(vm2);

    // Resolve app-defined classes on the main thread.
    let vm_env = unsafe { jni::JavaVM::from_raw(vm)? };
    let mut env = vm_env.get_env()?;
    AndroidBleTransport::cache_helper_class(&mut env)
        .map_err(|e| format!("cache BleHelper: {e}"))?;
    AndroidPairingStore::cache_store_class(&mut env)
        .map_err(|e| format!("cache SecureStore: {e}"))?;

    Ok(jni::JNIVersion::V6.into())
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
        .with(tracing_subscriber::EnvFilter::new(
            "sonde_pair=debug,sonde_pair_ui=debug",
        ))
        .init();

    let state = AppState {
        scanner: Mutex::new(None),
        #[cfg(target_os = "android")]
        store: Mutex::new(None),
        phase: Mutex::new("Idle".into()),
        logs,
    };

    tauri::Builder::default()
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
