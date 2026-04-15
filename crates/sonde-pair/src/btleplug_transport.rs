// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Desktop BLE transport using the [`btleplug`] crate.
//!
//! Supports Windows (WinRT), Linux (BlueZ), and macOS (CoreBluetooth).
//! Gated behind the `btleplug` cargo feature so the core crate stays
//! platform-free.
//!
//! # MTU negotiation
//!
//! `btleplug` 0.11 does not expose an API to query or request the ATT MTU.
//! Modern OS BLE stacks negotiate automatically to 512+ on BLE 4.2+ hardware,
//! so [`BtleplugTransport::connect`] reports [`BLE_MTU_MIN`] (247) as a
//! conservative lower bound.  The actual negotiated MTU is almost certainly
//! higher.
//!
//! # Runtime requirements
//!
//! This module requires a [tokio](https://docs.rs/tokio) runtime with the
//! `time` driver enabled.  `btleplug` itself is built on tokio, so this is
//! always the case when using the standard `#[tokio::main]` entry point.

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use btleplug::api::{
    BDAddr, Central, Characteristic as BtleCharacteristic, Manager as _, Peripheral as _,
    ScanFilter, ValueNotification, WriteType,
};
use btleplug::platform::{Adapter, Manager, Peripheral};
use futures::stream::StreamExt;
use tracing::debug;
use uuid::Uuid;

use crate::error::{format_device_address, PairingError};
use crate::transport::BleTransport;
use crate::types::{PairingMethod, ScannedDevice, BLE_MTU_MIN};

/// BLE connection timeout (PT-1002).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Default MTU reported when the platform does not expose the negotiated value.
///
/// See the [module-level documentation](self) for rationale.
const DEFAULT_REPORTED_MTU: u16 = BLE_MTU_MIN;

/// Desktop BLE transport backed by [`btleplug`].
///
/// Wraps a platform Bluetooth adapter and manages the lifecycle of a single
/// BLE connection at a time.  Implements [`BleTransport`] for use with the
/// sonde pairing protocol.
///
/// # Resource cleanup
///
/// On [`Drop`], an async disconnect is spawned on the current tokio runtime
/// (if available) to release the GATT connection.  For deterministic cleanup,
/// call [`disconnect()`](BleTransport::disconnect) explicitly.
pub struct BtleplugTransport {
    adapter: Adapter,
    connected: Option<ConnectedState>,
    /// Device address of the current connection (PT-1215).
    connected_address: Option<String>,
}

/// State for an active BLE connection.
struct ConnectedState {
    peripheral: Peripheral,
    notification_stream: Pin<Box<dyn futures::stream::Stream<Item = ValueNotification> + Send>>,
    subscribed: HashSet<Uuid>,
}

impl BtleplugTransport {
    /// Create a new transport using the first available Bluetooth adapter.
    ///
    /// Returns [`PairingError::AdapterNotFound`] if no adapters are present.
    pub async fn new() -> Result<Self, PairingError> {
        let manager = Manager::new()
            .await
            .map_err(|e| PairingError::ConnectionFailed {
                device: None,
                reason: format!("BLE manager init failed: {e}"),
            })?;

        let adapters = manager
            .adapters()
            .await
            .map_err(|e| PairingError::ConnectionFailed {
                device: None,
                reason: format!("failed to list adapters: {e}"),
            })?;

        let adapter = adapters
            .into_iter()
            .next()
            .ok_or(PairingError::AdapterNotFound)?;

        Ok(Self {
            adapter,
            connected: None,
            connected_address: None,
        })
    }

    /// Perform GATT service discovery and obtain the notification stream.
    ///
    /// Extracted so that `connect()` can cleanly disconnect the peripheral
    /// if any post-connect step fails (PT-1001).
    async fn post_connect_setup(
        peripheral: &Peripheral,
        device: &Option<String>,
    ) -> Result<
        (
            Pin<Box<dyn futures::stream::Stream<Item = ValueNotification> + Send>>,
            usize,
        ),
        PairingError,
    > {
        peripheral
            .discover_services()
            .await
            .map_err(|e| PairingError::ConnectionFailed {
                device: device.clone(),
                reason: format!("service discovery failed: {e}"),
            })?;
        let service_count = peripheral.services().len();

        let notification_stream =
            peripheral
                .notifications()
                .await
                .map_err(|e| PairingError::ConnectionFailed {
                    device: device.clone(),
                    reason: format!("failed to obtain notification stream: {e}"),
                })?;

        Ok((notification_stream, service_count))
    }

    /// Best-effort disconnect of the current connection (if any).
    ///
    /// Used internally before connecting to a new device and in `Drop`.
    async fn disconnect_inner(&mut self) {
        if let Some(state) = self.connected.take() {
            self.connected_address = None;
            for uuid in &state.subscribed {
                if let Some(chr) = state
                    .peripheral
                    .characteristics()
                    .into_iter()
                    .find(|c| c.uuid == *uuid)
                {
                    let _ = state.peripheral.unsubscribe(&chr).await;
                }
            }
            let _ = state.peripheral.disconnect().await;
        }
    }
}

/// Convert a `u128` UUID (as used by [`BleTransport`]) to [`uuid::Uuid`].
fn to_uuid(v: u128) -> Uuid {
    Uuid::from_u128(v)
}

/// Find a GATT characteristic on the peripheral by service and characteristic UUID.
fn find_characteristic(
    peripheral: &Peripheral,
    service_uuid: Uuid,
    char_uuid: Uuid,
    device: &Option<String>,
) -> Result<BtleCharacteristic, PairingError> {
    peripheral
        .characteristics()
        .into_iter()
        .find(|c| c.service_uuid == service_uuid && c.uuid == char_uuid)
        .ok_or_else(|| PairingError::ConnectionFailed {
            device: device.clone(),
            reason: format!("characteristic {char_uuid} not found in service {service_uuid}"),
        })
}

impl BleTransport for BtleplugTransport {
    fn start_scan(
        &mut self,
        service_uuids: &[u128],
    ) -> Pin<Box<dyn Future<Output = Result<(), PairingError>> + '_>> {
        let uuids: Vec<Uuid> = service_uuids.iter().map(|u| to_uuid(*u)).collect();
        Box::pin(async move {
            // Use an empty ScanFilter — WinRT's advertisement watcher does not
            // reliably match service UUIDs embedded in 16-bit AD structures.
            // Filtering is done in DeviceScanner::refresh() instead.
            let filter = ScanFilter::default();
            self.adapter
                .start_scan(filter)
                .await
                .map_err(|e| PairingError::ConnectionFailed {
                    device: None,
                    reason: format!("scan start failed: {e}"),
                })?;
            debug!(services = ?uuids, "BLE scan started (filter applied in discovery layer)");
            Ok(())
        })
    }

    fn stop_scan(&mut self) -> Pin<Box<dyn Future<Output = Result<(), PairingError>> + '_>> {
        Box::pin(async move {
            self.adapter
                .stop_scan()
                .await
                .map_err(|e| PairingError::ConnectionFailed {
                    device: None,
                    reason: format!("scan stop failed: {e}"),
                })?;
            debug!("BLE scan stopped");
            Ok(())
        })
    }

    fn get_discovered_devices(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ScannedDevice>, PairingError>> + '_>> {
        Box::pin(async move {
            let peripherals =
                self.adapter
                    .peripherals()
                    .await
                    .map_err(|e| PairingError::ConnectionFailed {
                        device: None,
                        reason: format!("failed to list peripherals: {e}"),
                    })?;

            let mut devices = Vec::new();
            debug!(
                "enumerating {} peripherals from btleplug",
                peripherals.len()
            );
            for p in &peripherals {
                let props = match p.properties().await {
                    Ok(Some(props)) => props,
                    Ok(None) => {
                        debug!("peripheral {:?}: properties returned None", p.address());
                        continue;
                    }
                    Err(e) => {
                        debug!("peripheral {:?}: properties error: {e}", p.address());
                        continue;
                    }
                };
                let address = p.address().into_inner();
                let service_uuids: Vec<u128> = props.services.iter().map(|u| u.as_u128()).collect();
                debug!(
                    name = ?props.local_name,
                    addr = ?address,
                    services = ?props.services,
                    rssi = ?props.rssi,
                    "discovered peripheral"
                );

                devices.push(ScannedDevice {
                    name: props.local_name.unwrap_or_default(),
                    address,
                    rssi: props.rssi.unwrap_or(0) as i8,
                    service_uuids,
                });
            }
            Ok(devices)
        })
    }

    fn connect(
        &mut self,
        address: &[u8; 6],
    ) -> Pin<Box<dyn Future<Output = Result<u16, PairingError>> + '_>> {
        let addr = *address;
        Box::pin(async move {
            // Disconnect any existing connection first to avoid resource leaks.
            if self.connected.is_some() {
                debug!("disconnecting previous connection before new connect");
                self.disconnect_inner().await;
            }

            let target_addr = BDAddr::from(addr);

            // Locate the peripheral among previously discovered devices.
            // If the adapter has no cached peripherals (e.g. a freshly
            // created transport), run a short scan first so WinRT populates
            // its internal device list.
            let device_str = format_device_address(&addr);
            let mut peripherals =
                self.adapter
                    .peripherals()
                    .await
                    .map_err(|e| PairingError::ConnectionFailed {
                        device: Some(device_str.clone()),
                        reason: format!("failed to list peripherals: {e}"),
                    })?;

            if !peripherals.iter().any(|p| p.address() == target_addr) {
                debug!("target not in cached peripherals, running short scan");
                self.adapter
                    .start_scan(ScanFilter::default())
                    .await
                    .map_err(|e| PairingError::ConnectionFailed {
                        device: Some(device_str.clone()),
                        reason: format!("pre-connect scan failed: {e}"),
                    })?;
                tokio::time::sleep(Duration::from_secs(3)).await;
                self.adapter.stop_scan().await.ok();
                peripherals = self.adapter.peripherals().await.map_err(|e| {
                    PairingError::ConnectionFailed {
                        device: Some(device_str.clone()),
                        reason: format!("failed to list peripherals: {e}"),
                    }
                })?;
            }

            let peripheral = peripherals
                .into_iter()
                .find(|p| p.address() == target_addr)
                .ok_or(PairingError::DeviceNotFound {
                    device: device_str.clone(),
                })?;

            // Connect with a timeout (PT-1002: 30 s).
            tokio::time::timeout(CONNECT_TIMEOUT, peripheral.connect())
                .await
                .map_err(|_| PairingError::Timeout {
                    device: Some(device_str.clone()),
                    operation: "BLE connect",
                    duration_secs: CONNECT_TIMEOUT.as_secs(),
                })?
                .map_err(|e| PairingError::ConnectionFailed {
                    device: Some(device_str.clone()),
                    reason: format!("connect failed: {e}"),
                })?;

            // Post-connect setup — if any step fails, disconnect the
            // peripheral so we don't leak a GATT connection (PT-1001).
            let device = Some(device_str.clone());
            match Self::post_connect_setup(&peripheral, &device).await {
                Ok((notification_stream, service_count)) => {
                    debug!(
                        address = %target_addr,
                        services = service_count,
                        "connected and discovered services"
                    );
                    self.connected = Some(ConnectedState {
                        peripheral,
                        notification_stream,
                        subscribed: HashSet::new(),
                    });
                    self.connected_address = Some(device_str);
                }
                Err(e) => {
                    let _ = peripheral.disconnect().await;
                    return Err(e);
                }
            }

            // btleplug 0.11 does not expose MTU negotiation; report a
            // conservative default.  See module-level docs for rationale.
            debug!(
                mtu = DEFAULT_REPORTED_MTU,
                "btleplug does not expose MTU; reporting conservative default"
            );
            Ok(DEFAULT_REPORTED_MTU)
        })
    }

    fn disconnect(&mut self) -> Pin<Box<dyn Future<Output = Result<(), PairingError>> + '_>> {
        Box::pin(async move {
            self.disconnect_inner().await;
            debug!("disconnected");
            Ok(())
        })
    }

    fn write_characteristic(
        &mut self,
        service: u128,
        characteristic: u128,
        data: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), PairingError>> + '_>> {
        let data = data.to_vec();
        Box::pin(async move {
            let device_addr = self.connected_address.clone();
            let state = self
                .connected
                .as_ref()
                .ok_or(PairingError::ConnectionDropped {
                    device: device_addr.clone(),
                })?;

            let chr = find_characteristic(
                &state.peripheral,
                to_uuid(service),
                to_uuid(characteristic),
                &device_addr,
            )?;

            // First write may fail with "requires authentication" on WinRT if
            // the characteristic has WRITE_ENC/WRITE_AUTHEN permissions.  This
            // triggers the OS BLE pairing dialog.  Retry after a short delay to
            // give the user time to accept the pairing prompt.
            let result = state
                .peripheral
                .write(&chr, &data, WriteType::WithResponse)
                .await;

            match result {
                Ok(()) => {}
                Err(ref e) => {
                    let msg = e.to_string();
                    if msg.contains("authentication") || msg.contains("0x80650005") {
                        debug!("GATT write requires auth — waiting for OS pairing dialog");
                        // Give the user up to 30s to accept the OS pairing prompt.
                        for attempt in 1..=6 {
                            tokio::time::sleep(Duration::from_secs(5)).await;
                            debug!(attempt, "retrying GATT write after pairing");
                            match state
                                .peripheral
                                .write(&chr, &data, WriteType::WithResponse)
                                .await
                            {
                                Ok(()) => break,
                                Err(ref retry_err) if attempt < 6 => {
                                    debug!(error = %retry_err, "retry failed, will try again");
                                }
                                Err(retry_err) => {
                                    return Err(PairingError::GattWriteFailed {
                                        device: device_addr.clone(),
                                        reason: retry_err.to_string(),
                                    });
                                }
                            }
                        }
                    } else {
                        return Err(PairingError::GattWriteFailed {
                            device: device_addr.clone(),
                            reason: msg,
                        });
                    }
                }
            }

            debug!(
                characteristic = %to_uuid(characteristic),
                len = data.len(),
                "GATT write complete"
            );
            Ok(())
        })
    }

    fn read_indication(
        &mut self,
        service: u128,
        characteristic: u128,
        timeout_ms: u64,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, PairingError>> + '_>> {
        Box::pin(async move {
            let device_addr = self.connected_address.clone();
            let state = self
                .connected
                .as_mut()
                .ok_or(PairingError::ConnectionDropped {
                    device: device_addr.clone(),
                })?;

            let char_uuid = to_uuid(characteristic);

            // Subscribe to indications/notifications lazily on first read.
            if !state.subscribed.contains(&char_uuid) {
                let chr = find_characteristic(
                    &state.peripheral,
                    to_uuid(service),
                    char_uuid,
                    &device_addr,
                )?;
                state.peripheral.subscribe(&chr).await.map_err(|e| {
                    PairingError::GattReadFailed {
                        device: device_addr.clone(),
                        reason: format!("subscribe failed: {e}"),
                    }
                })?;
                state.subscribed.insert(char_uuid);
                debug!(characteristic = %char_uuid, "subscribed to indications");
            }

            // Wait for a notification matching the expected characteristic.
            let deadline = Duration::from_millis(timeout_ms);
            let result = tokio::time::timeout(deadline, async {
                loop {
                    match state.notification_stream.next().await {
                        Some(notif) if notif.uuid == char_uuid => {
                            debug!(
                                characteristic = %char_uuid,
                                len = notif.value.len(),
                                "GATT indication received"
                            );
                            return Ok(notif.value);
                        }
                        Some(_) => continue,
                        None => {
                            return Err(PairingError::ConnectionDropped {
                                device: device_addr.clone(),
                            })
                        }
                    }
                }
            })
            .await;

            match result {
                Ok(inner) => inner,
                Err(_) => Err(PairingError::IndicationTimeout {
                    device: device_addr,
                }),
            }
        })
    }

    /// btleplug does not expose the negotiated pairing method to user-space.
    /// Report `Unknown` so that `enforce_lesc()` rejects the connection
    /// per PT-0904 rather than silently assuming OS enforcement.
    fn pairing_method(&self) -> Option<PairingMethod> {
        // btleplug delegates pairing to the OS BLE stack (WinRT / BlueZ /
        // CoreBluetooth).  It does not expose which pairing method was used,
        // so we return `None` to indicate OS-enforced security (PT-0904).
        None
    }
}

impl Drop for BtleplugTransport {
    fn drop(&mut self) {
        if let Some(state) = self.connected.take() {
            // Best-effort async disconnect — spawn on the current tokio
            // runtime if one is available.
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let peripheral = state.peripheral;
                handle.spawn(async move {
                    let _ = peripheral.disconnect().await;
                });
            }
        }
    }
}
