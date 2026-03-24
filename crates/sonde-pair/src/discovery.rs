// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! BLE device discovery with UUID filtering and stale device eviction.
//!
//! The [`DeviceScanner`] wraps a [`BleTransport`] and provides:
//!
//! - Scan lifecycle (start / stop / timeout)
//! - Filtering to only Gateway Pairing Service and Node Provisioning Service UUIDs
//! - Stale device eviction (devices not seen for a configurable duration are removed)

use std::collections::HashMap;
use std::time::{Duration, Instant};

use tracing::debug;

use crate::error::PairingError;
use crate::transport::BleTransport;
use crate::types::{ScannedDevice, GATEWAY_SERVICE_UUID, NODE_SERVICE_UUID};

/// Default scan timeout (30 seconds per PT-0202).
pub const DEFAULT_SCAN_TIMEOUT: Duration = Duration::from_secs(30);

/// Default stale device eviction threshold (10 seconds per PT-0202).
pub const DEFAULT_STALE_TIMEOUT: Duration = Duration::from_secs(10);

/// Classification of a discovered BLE device by its advertised service UUID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceType {
    /// Device advertises the Gateway Pairing Service UUID.
    Gateway,
    /// Device advertises the Node Provisioning Service UUID.
    Node,
}

/// Returns the [`ServiceType`] for a device based on its advertised service UUIDs.
///
/// Returns `None` if the device does not advertise a recognised service.
/// If a device advertises both UUIDs, [`ServiceType::Gateway`] takes precedence.
pub fn service_type(device: &ScannedDevice) -> Option<ServiceType> {
    if device.service_uuids.contains(&GATEWAY_SERVICE_UUID) {
        Some(ServiceType::Gateway)
    } else if device.service_uuids.contains(&NODE_SERVICE_UUID) {
        Some(ServiceType::Node)
    } else {
        None
    }
}

/// Returns `true` if `device` advertises at least one target service UUID.
fn is_target_device(device: &ScannedDevice) -> bool {
    device.service_uuids.contains(&GATEWAY_SERVICE_UUID)
        || device.service_uuids.contains(&NODE_SERVICE_UUID)
}

/// BLE device scanner with UUID filtering and stale eviction.
///
/// Wraps a [`BleTransport`] and maintains a time-stamped map of discovered
/// devices.  Call [`refresh`](Self::refresh) periodically to poll the transport
/// and evict stale entries.
pub struct DeviceScanner<T: BleTransport> {
    transport: T,
    /// Devices keyed by BLE address, with last-seen wall-clock timestamp.
    known: HashMap<[u8; 6], (ScannedDevice, Instant)>,
    scanning: bool,
    scan_timeout: Duration,
    stale_timeout: Duration,
    scan_started_at: Option<Instant>,
}

impl<T: BleTransport> DeviceScanner<T> {
    /// Create a scanner with default timeouts (30 s scan, 10 s stale eviction).
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            known: HashMap::new(),
            scanning: false,
            scan_timeout: DEFAULT_SCAN_TIMEOUT,
            stale_timeout: DEFAULT_STALE_TIMEOUT,
            scan_started_at: None,
        }
    }

    /// Create a scanner with custom timeouts.
    pub fn with_timeouts(transport: T, scan_timeout: Duration, stale_timeout: Duration) -> Self {
        Self {
            transport,
            known: HashMap::new(),
            scanning: false,
            scan_timeout,
            stale_timeout,
            scan_started_at: None,
        }
    }

    /// Start scanning for gateway and node devices.
    ///
    /// Initiates BLE scans for both the Gateway Pairing Service and Node
    /// Provisioning Service UUIDs.  Returns [`PairingError::ScanAlreadyActive`]
    /// if a scan is already running.
    pub async fn start(&mut self) -> Result<(), PairingError> {
        if self.scanning {
            return Err(PairingError::ScanAlreadyActive);
        }

        // Scan for both service UUIDs in a single call so the BLE adapter
        // filters for both simultaneously (avoids the second call replacing
        // the first filter on platforms like WinRT/btleplug).
        self.transport
            .start_scan(&[GATEWAY_SERVICE_UUID, NODE_SERVICE_UUID])
            .await?;

        self.scanning = true;
        self.scan_started_at = Some(Instant::now());
        self.known.clear();
        debug!(
            uuids = ?[GATEWAY_SERVICE_UUID, NODE_SERVICE_UUID],
            "scan started"
        );
        Ok(())
    }

    /// Stop the active scan.
    ///
    /// No-op if no scan is running.
    pub async fn stop(&mut self) -> Result<(), PairingError> {
        if self.scanning {
            self.transport.stop_scan().await?;
            self.scanning = false;
            self.scan_started_at = None;
            debug!("scan stopped");
        }
        Ok(())
    }

    /// Poll the transport for discovered devices, update timestamps, and evict
    /// stale entries.
    ///
    /// Only devices advertising a recognised service UUID are tracked.
    pub async fn refresh(&mut self) -> Result<(), PairingError> {
        let now = Instant::now();
        let discovered = self.transport.get_discovered_devices().await?;

        for device in discovered {
            if is_target_device(&device) {
                let is_new = !self.known.contains_key(&device.address);
                if is_new {
                    debug!(
                        name = %device.name,
                        address = ?device.address,
                        rssi = device.rssi,
                        service_uuids = ?device.service_uuids,
                        "device discovered"
                    );
                }
                self.known.insert(device.address, (device, now));
            }
        }

        let stale = self.stale_timeout;
        let before = self.known.len();
        self.known
            .retain(|_, (_, last_seen)| now.duration_since(*last_seen) < stale);
        let evicted = before - self.known.len();
        if evicted > 0 {
            debug!(evicted_count = evicted, "stale devices evicted");
        }

        Ok(())
    }

    /// Return the currently known, non-stale devices.
    pub fn devices(&self) -> Vec<ScannedDevice> {
        let now = Instant::now();
        self.known
            .values()
            .filter(|(_, last_seen)| now.duration_since(*last_seen) < self.stale_timeout)
            .map(|(device, _)| device.clone())
            .collect()
    }

    /// Returns `true` if the scan has exceeded its timeout duration.
    pub fn is_timed_out(&self) -> bool {
        self.scan_started_at
            .map(|start| Instant::now().duration_since(start) >= self.scan_timeout)
            .unwrap_or(false)
    }

    /// Returns `true` if a scan is currently active.
    pub fn is_scanning(&self) -> bool {
        self.scanning
    }

    /// Returns a mutable reference to the underlying transport.
    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    /// Consume the scanner and return the underlying transport.
    pub fn into_transport(self) -> T {
        self.transport
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::MockBleTransport;

    fn gateway_device(name: &str, addr: [u8; 6], rssi: i8) -> ScannedDevice {
        ScannedDevice {
            name: name.to_string(),
            address: addr,
            rssi,
            service_uuids: vec![GATEWAY_SERVICE_UUID],
        }
    }

    fn node_device(name: &str, addr: [u8; 6], rssi: i8) -> ScannedDevice {
        ScannedDevice {
            name: name.to_string(),
            address: addr,
            rssi,
            service_uuids: vec![NODE_SERVICE_UUID],
        }
    }

    fn unrelated_device(name: &str, addr: [u8; 6], rssi: i8) -> ScannedDevice {
        ScannedDevice {
            name: name.to_string(),
            address: addr,
            rssi,
            service_uuids: vec![0x0000_1800_0000_1000_8000_0080_5F9B_34FB],
        }
    }

    #[tokio::test]
    async fn test_scan_start_stop_lifecycle() {
        let mock = MockBleTransport::new(247);
        let mut scanner = DeviceScanner::new(mock);

        assert!(!scanner.is_scanning());

        scanner.start().await.unwrap();
        assert!(scanner.is_scanning());

        scanner.stop().await.unwrap();
        assert!(!scanner.is_scanning());
    }

    #[tokio::test]
    async fn test_start_while_scanning_returns_error() {
        let mock = MockBleTransport::new(247);
        let mut scanner = DeviceScanner::new(mock);

        scanner.start().await.unwrap();
        let err = scanner.start().await.unwrap_err();
        assert!(
            matches!(err, PairingError::ScanAlreadyActive),
            "expected ScanAlreadyActive, got {err:?}"
        );
    }

    #[tokio::test]
    async fn test_stop_when_not_scanning_is_noop() {
        let mock = MockBleTransport::new(247);
        let mut scanner = DeviceScanner::new(mock);

        // Should not error.
        scanner.stop().await.unwrap();
        assert!(!scanner.is_scanning());
    }

    #[tokio::test]
    async fn test_uuid_filtering_includes_gateway_and_node() {
        let mut mock = MockBleTransport::new(247);
        mock.add_device(gateway_device("GW-1", [1, 0, 0, 0, 0, 0], -40));
        mock.add_device(node_device("Node-1", [2, 0, 0, 0, 0, 0], -55));
        mock.add_device(unrelated_device("Other", [3, 0, 0, 0, 0, 0], -30));

        let mut scanner = DeviceScanner::new(mock);
        scanner.start().await.unwrap();
        scanner.refresh().await.unwrap();

        let devices = scanner.devices();
        assert_eq!(devices.len(), 2);

        let names: Vec<&str> = devices.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"GW-1"));
        assert!(names.contains(&"Node-1"));
        assert!(!names.contains(&"Other"));
    }

    #[tokio::test]
    async fn test_uuid_filtering_rejects_no_uuid() {
        let mut mock = MockBleTransport::new(247);
        mock.add_device(ScannedDevice {
            name: "Empty".to_string(),
            address: [4, 0, 0, 0, 0, 0],
            rssi: -60,
            service_uuids: vec![],
        });

        let mut scanner = DeviceScanner::new(mock);
        scanner.start().await.unwrap();
        scanner.refresh().await.unwrap();

        assert!(scanner.devices().is_empty());
    }

    #[tokio::test]
    async fn test_service_type_classification() {
        let gw = gateway_device("GW", [1, 0, 0, 0, 0, 0], -40);
        let node = node_device("Node", [2, 0, 0, 0, 0, 0], -55);
        let other = unrelated_device("Other", [3, 0, 0, 0, 0, 0], -30);

        assert_eq!(service_type(&gw), Some(ServiceType::Gateway));
        assert_eq!(service_type(&node), Some(ServiceType::Node));
        assert_eq!(service_type(&other), None);
    }

    #[tokio::test]
    async fn test_service_type_gateway_takes_precedence() {
        let both = ScannedDevice {
            name: "Both".to_string(),
            address: [5, 0, 0, 0, 0, 0],
            rssi: -45,
            service_uuids: vec![GATEWAY_SERVICE_UUID, NODE_SERVICE_UUID],
        };
        assert_eq!(service_type(&both), Some(ServiceType::Gateway));
    }

    #[tokio::test]
    async fn test_stale_device_eviction() {
        let mut mock = MockBleTransport::new(247);
        mock.add_device(gateway_device("GW-1", [1, 0, 0, 0, 0, 0], -40));

        let stale_ms = Duration::from_millis(50);
        let mut scanner = DeviceScanner::with_timeouts(mock, DEFAULT_SCAN_TIMEOUT, stale_ms);

        scanner.start().await.unwrap();
        scanner.refresh().await.unwrap();
        assert_eq!(scanner.devices().len(), 1);

        // Remove the device from the mock so subsequent refreshes won't re-add it.
        scanner.transport_mut().devices.clear();

        // Wait for the stale timeout to elapse.
        tokio::time::sleep(Duration::from_millis(80)).await;

        scanner.refresh().await.unwrap();
        assert!(
            scanner.devices().is_empty(),
            "expected stale device to be evicted"
        );
    }

    #[tokio::test]
    async fn test_refresh_updates_device_info() {
        let mut mock = MockBleTransport::new(247);
        mock.add_device(gateway_device("GW-1", [1, 0, 0, 0, 0, 0], -70));

        let mut scanner = DeviceScanner::new(mock);
        scanner.start().await.unwrap();
        scanner.refresh().await.unwrap();

        // Simulate RSSI change by replacing the device in the mock.
        scanner.transport_mut().devices.clear();
        scanner
            .transport_mut()
            .add_device(gateway_device("GW-1", [1, 0, 0, 0, 0, 0], -40));

        scanner.refresh().await.unwrap();
        let devices = scanner.devices();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].rssi, -40);
    }

    #[tokio::test]
    async fn test_scan_timeout() {
        let mock = MockBleTransport::new(247);
        let short_timeout = Duration::from_millis(50);
        let mut scanner = DeviceScanner::with_timeouts(mock, short_timeout, DEFAULT_STALE_TIMEOUT);

        assert!(!scanner.is_timed_out());

        scanner.start().await.unwrap();
        tokio::time::sleep(Duration::from_millis(80)).await;

        assert!(scanner.is_timed_out());
    }

    #[tokio::test]
    async fn test_into_transport_returns_transport() {
        let mut mock = MockBleTransport::new(247);
        mock.add_device(gateway_device("GW-1", [1, 0, 0, 0, 0, 0], -40));

        let scanner = DeviceScanner::new(mock);
        let transport = scanner.into_transport();
        assert_eq!(transport.devices.len(), 1);
    }

    #[tokio::test]
    async fn test_start_clears_known_devices() {
        let mut mock = MockBleTransport::new(247);
        mock.add_device(gateway_device("GW-1", [1, 0, 0, 0, 0, 0], -40));

        let mut scanner = DeviceScanner::new(mock);
        scanner.start().await.unwrap();
        scanner.refresh().await.unwrap();
        assert_eq!(scanner.devices().len(), 1);

        // Stop and restart — known devices should be cleared.
        scanner.stop().await.unwrap();
        scanner.start().await.unwrap();
        assert!(
            scanner.devices().is_empty(),
            "expected devices to be cleared on restart"
        );
    }
}
