// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use crate::error::PairingError;
use crate::types::{PairingMethod, ScannedDevice};
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use tracing::debug;

/// Platform-independent BLE transport abstraction.
///
/// All pairing logic uses this trait, allowing platform implementations
/// (iOS, Android, desktop) and test mocks to be swapped freely.
pub trait BleTransport {
    fn start_scan(
        &mut self,
        service_uuids: &[u128],
    ) -> Pin<Box<dyn Future<Output = Result<(), PairingError>> + '_>>;

    fn stop_scan(&mut self) -> Pin<Box<dyn Future<Output = Result<(), PairingError>> + '_>>;

    fn get_discovered_devices(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ScannedDevice>, PairingError>> + '_>>;

    /// Connect to the device at the given address. Returns the negotiated MTU.
    fn connect(
        &mut self,
        address: &[u8; 6],
    ) -> Pin<Box<dyn Future<Output = Result<u16, PairingError>> + '_>>;

    fn disconnect(&mut self) -> Pin<Box<dyn Future<Output = Result<(), PairingError>> + '_>>;

    fn write_characteristic(
        &mut self,
        service: u128,
        characteristic: u128,
        data: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), PairingError>> + '_>>;

    fn read_indication(
        &mut self,
        service: u128,
        characteristic: u128,
        timeout_ms: u64,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, PairingError>> + '_>>;

    /// Returns the BLE pairing method observed during the last connection.
    ///
    /// Platform transports that can observe the negotiated pairing method
    /// (e.g., via Android `onBondStateChanged` or Windows BLE APIs) MUST
    /// return the actual method.  The application layer rejects any method
    /// other than `NumericComparison`.
    ///
    /// Return `None` only when the OS BLE stack guarantees LESC and refuses
    /// Just Works without app intervention (the caller treats `None` as
    /// "OS-enforced LESC").
    ///
    /// This is a required method (no default) so that new transport
    /// implementations are forced to make an explicit choice — forgetting
    /// to implement it is a compile error, not a silent security bypass.
    fn pairing_method(&self) -> Option<PairingMethod>;
}

/// Mock BLE transport for testing pairing logic without hardware.
pub struct MockBleTransport {
    /// Queued indication responses, consumed in order.
    pub responses: VecDeque<Result<Vec<u8>, PairingError>>,
    /// Log of writes: (service_uuid, characteristic_uuid, data).
    pub written: Vec<(u128, u128, Vec<u8>)>,
    /// Devices returned by `get_discovered_devices`.
    pub devices: Vec<ScannedDevice>,
    /// MTU value returned by `connect`.
    pub mtu: u16,
    /// Whether the transport is currently connected.
    pub connected: bool,
    /// If `Some`, the next `connect()` call takes and returns this error.
    pub connect_error: Option<PairingError>,
    /// If `Some`, the next `write_characteristic()` call takes and returns this error.
    pub write_error: Option<PairingError>,
    /// Count of `disconnect()` calls for resource-leak verification.
    pub disconnect_count: usize,
    /// Count of `read_indication()` calls for retry verification.
    pub read_call_count: usize,
    /// BLE pairing method reported after connection (PT-0904).
    pub pairing_method: Option<PairingMethod>,
    /// If true, `connect()` returns `ConnectionFailed` (simulates Just Works
    /// rejection at the transport layer, per T-PT-109).
    pub fail_connect: bool,
}

impl MockBleTransport {
    pub fn new(mtu: u16) -> Self {
        Self {
            responses: VecDeque::new(),
            written: Vec::new(),
            devices: Vec::new(),
            mtu,
            connected: false,
            connect_error: None,
            write_error: None,
            disconnect_count: 0,
            read_call_count: 0,
            pairing_method: None,
            fail_connect: false,
        }
    }

    /// Queue an indication response to be returned by the next `read_indication` call.
    pub fn queue_response(&mut self, response: Result<Vec<u8>, PairingError>) {
        self.responses.push_back(response);
    }

    /// Add a device to the scan results.
    pub fn add_device(&mut self, device: ScannedDevice) {
        self.devices.push(device);
    }
}

impl BleTransport for MockBleTransport {
    fn start_scan(
        &mut self,
        _service_uuids: &[u128],
    ) -> Pin<Box<dyn Future<Output = Result<(), PairingError>> + '_>> {
        Box::pin(async { Ok(()) })
    }

    fn stop_scan(&mut self) -> Pin<Box<dyn Future<Output = Result<(), PairingError>> + '_>> {
        Box::pin(async { Ok(()) })
    }

    fn get_discovered_devices(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ScannedDevice>, PairingError>> + '_>> {
        Box::pin(async { Ok(self.devices.clone()) })
    }

    fn connect(
        &mut self,
        _address: &[u8; 6],
    ) -> Pin<Box<dyn Future<Output = Result<u16, PairingError>> + '_>> {
        if let Some(err) = self.connect_error.take() {
            self.connected = false;
            return Box::pin(async move { Err(err) });
        }
        if self.fail_connect {
            return Box::pin(async {
                Err(PairingError::ConnectionFailed {
                    device: None,
                    reason:
                        "Numeric Comparison pairing required but peripheral only supports Just Works"
                            .into(),
                })
            });
        }
        self.connected = true;
        let mtu = self.mtu;
        Box::pin(async move { Ok(mtu) })
    }

    fn disconnect(&mut self) -> Pin<Box<dyn Future<Output = Result<(), PairingError>> + '_>> {
        self.connected = false;
        self.disconnect_count += 1;
        Box::pin(async { Ok(()) })
    }

    fn write_characteristic(
        &mut self,
        service: u128,
        characteristic: u128,
        data: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), PairingError>> + '_>> {
        // Record every attempted write, even when an injected error is returned.
        self.written.push((service, characteristic, data.to_vec()));

        if let Some(err) = self.write_error.take() {
            return Box::pin(async move { Err(err) });
        }
        Box::pin(async { Ok(()) })
    }

    fn read_indication(
        &mut self,
        _service: u128,
        _characteristic: u128,
        _timeout_ms: u64,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, PairingError>> + '_>> {
        self.read_call_count += 1;
        let response = self.responses.pop_front();
        Box::pin(async move {
            match response {
                Some(result) => result,
                None => Err(PairingError::IndicationTimeout { device: None }),
            }
        })
    }

    fn pairing_method(&self) -> Option<PairingMethod> {
        self.pairing_method
    }
}

/// Enforce LESC Numeric Comparison (PT-0904).
///
/// Checks the transport's reported pairing method and rejects anything
/// other than `NumericComparison`.  When `pairing_method()` returns `None`,
/// the platform is assumed to enforce LESC at the OS BLE-stack level.
///
/// On failure the transport is disconnected before returning the error.
pub async fn enforce_lesc(transport: &mut dyn BleTransport) -> Result<(), PairingError> {
    if let Some(method) = transport.pairing_method() {
        if method != PairingMethod::NumericComparison {
            if let Err(e) = transport.disconnect().await {
                debug!(
                    error = ?e,
                    ?method,
                    "BLE disconnect after insecure pairing method failed"
                );
            }
            return Err(PairingError::InsecurePairingMethod { method });
        }
        debug!(?method, "BLE pairing method verified");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ensure that when the pairing method is `None` (OS-enforced LESC),
    /// `enforce_lesc` allows pairing to proceed and does not disconnect.
    #[tokio::test]
    async fn enforce_lesc_allows_os_enforced_pairing() {
        let mut transport = MockBleTransport::new(185);
        // Simulate an active connection; `pairing_method` is `None` by default.
        transport.connected = true;

        let result = enforce_lesc(&mut transport).await;

        assert!(result.is_ok(), "enforce_lesc should allow OS-enforced LESC");
        assert!(
            transport.connected,
            "transport should remain connected when pairing_method is None"
        );
    }
}
