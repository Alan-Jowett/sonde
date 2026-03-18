// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use crate::error::PairingError;
use crate::types::ScannedDevice;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;

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
}

impl MockBleTransport {
    pub fn new(mtu: u16) -> Self {
        Self {
            responses: VecDeque::new(),
            written: Vec::new(),
            devices: Vec::new(),
            mtu,
            connected: false,
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
        self.connected = true;
        let mtu = self.mtu;
        Box::pin(async move { Ok(mtu) })
    }

    fn disconnect(&mut self) -> Pin<Box<dyn Future<Output = Result<(), PairingError>> + '_>> {
        self.connected = false;
        Box::pin(async { Ok(()) })
    }

    fn write_characteristic(
        &mut self,
        service: u128,
        characteristic: u128,
        data: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), PairingError>> + '_>> {
        self.written.push((service, characteristic, data.to_vec()));
        Box::pin(async { Ok(()) })
    }

    fn read_indication(
        &mut self,
        _service: u128,
        _characteristic: u128,
        _timeout_ms: u64,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, PairingError>> + '_>> {
        let response = self.responses.pop_front();
        Box::pin(async move {
            match response {
                Some(result) => result,
                None => Err(PairingError::IndicationTimeout),
            }
        })
    }
}
