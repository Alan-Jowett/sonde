// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! TCP-backed BLE transport for hardware-free integration testing.
//!
//! Connects to a fake GATT peripheral over TCP instead of real Bluetooth.
//! Feature-gated under `loopback-ble` so it doesn't ship in production.
//!
//! The TCP protocol speaks raw BLE envelopes (`TYPE | LEN_BE16 | BODY`)
//! directly on the stream — no additional framing is needed because the
//! envelope format is self-delimiting.

use std::future::Future;
use std::pin::Pin;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::error::PairingError;
use crate::transport::BleTransport;
use crate::types::{PairingMethod, ScannedDevice, GATEWAY_SERVICE_UUID};

/// A [`BleTransport`] that tunnels BLE operations over TCP to a
/// [`fake_gatt_peripheral`](https://github.com/Alan-Jowett/sonde/issues/259).
///
/// Designed for integration testing — scan returns a synthetic device,
/// connect opens a TCP socket, and characteristic I/O maps to TCP reads/writes.
pub struct LoopbackBleTransport {
    /// `host:port` of the fake GATT peripheral.
    addr: String,
    /// Active TCP connection (set by `connect`, cleared by `disconnect`).
    stream: Option<TcpStream>,
}

impl LoopbackBleTransport {
    /// Create a new loopback transport targeting the given `host:port`.
    pub fn new(addr: &str) -> Self {
        Self {
            addr: addr.to_string(),
            stream: None,
        }
    }
}

/// Read a complete BLE envelope from a TCP stream.
///
/// Envelope layout: `TYPE(1B) | LEN(2B BE) | BODY(LEN bytes)`.
async fn read_envelope(stream: &mut TcpStream) -> Result<Vec<u8>, PairingError> {
    let mut header = [0u8; 3];
    stream
        .read_exact(&mut header)
        .await
        .map_err(|e| PairingError::GattReadFailed(format!("TCP read header: {e}")))?;

    let body_len = u16::from_be_bytes([header[1], header[2]]) as usize;
    let mut envelope = Vec::with_capacity(3 + body_len);
    envelope.extend_from_slice(&header);
    if body_len > 0 {
        envelope.resize(3 + body_len, 0);
        stream
            .read_exact(&mut envelope[3..])
            .await
            .map_err(|e| PairingError::GattReadFailed(format!("TCP read body: {e}")))?;
    }
    Ok(envelope)
}

impl BleTransport for LoopbackBleTransport {
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
        Box::pin(async {
            Ok(vec![ScannedDevice {
                name: "Sonde-GW-Loopback".into(),
                address: [0x10, 0x0B, 0xAC, 0x00, 0x00, 0x01],
                rssi: -30,
                service_uuids: vec![GATEWAY_SERVICE_UUID],
            }])
        })
    }

    fn connect(
        &mut self,
        _address: &[u8; 6],
    ) -> Pin<Box<dyn Future<Output = Result<u16, PairingError>> + '_>> {
        Box::pin(async {
            let stream = TcpStream::connect(&self.addr)
                .await
                .map_err(|e| PairingError::ConnectionFailed(format!("TCP connect: {e}")))?;
            self.stream = Some(stream);
            // Return a large MTU — TCP has no MTU constraint.
            Ok(512)
        })
    }

    fn disconnect(&mut self) -> Pin<Box<dyn Future<Output = Result<(), PairingError>> + '_>> {
        self.stream = None;
        Box::pin(async { Ok(()) })
    }

    fn write_characteristic(
        &mut self,
        _service: u128,
        _characteristic: u128,
        data: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), PairingError>> + '_>> {
        let data = data.to_vec();
        Box::pin(async move {
            let stream = self
                .stream
                .as_mut()
                .ok_or(PairingError::ConnectionDropped)?;
            stream
                .write_all(&data)
                .await
                .map_err(|e| PairingError::GattWriteFailed(format!("TCP write: {e}")))?;
            stream
                .flush()
                .await
                .map_err(|e| PairingError::GattWriteFailed(format!("TCP flush: {e}")))?;
            Ok(())
        })
    }

    fn read_indication(
        &mut self,
        _service: u128,
        _characteristic: u128,
        timeout_ms: u64,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, PairingError>> + '_>> {
        Box::pin(async move {
            let stream = self
                .stream
                .as_mut()
                .ok_or(PairingError::ConnectionDropped)?;
            let timeout = tokio::time::Duration::from_millis(timeout_ms);
            match tokio::time::timeout(timeout, read_envelope(stream)).await {
                Ok(result) => result,
                Err(_) => Err(PairingError::IndicationTimeout),
            }
        })
    }

    /// Loopback transport does not simulate BLE pairing negotiation.
    /// The OS BLE stack is assumed to enforce LESC.
    fn pairing_method(&self) -> Option<PairingMethod> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_returns_fake_device() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let transport = LoopbackBleTransport::new("127.0.0.1:0");
            let devices = transport.get_discovered_devices().await.unwrap();
            assert_eq!(devices.len(), 1);
            assert_eq!(devices[0].name, "Sonde-GW-Loopback");
            assert!(devices[0].service_uuids.contains(&GATEWAY_SERVICE_UUID));
        });
    }

    #[test]
    fn connect_fails_when_no_server() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut transport = LoopbackBleTransport::new("127.0.0.1:1");
            let result = transport.connect(&[0; 6]).await;
            assert!(
                matches!(result, Err(PairingError::ConnectionFailed(_))),
                "expected ConnectionFailed, got {result:?}"
            );
        });
    }

    #[test]
    fn disconnect_clears_stream() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut transport = LoopbackBleTransport::new("127.0.0.1:0");
            assert!(transport.stream.is_none());
            transport.disconnect().await.unwrap();
            assert!(transport.stream.is_none());
        });
    }

    #[test]
    fn write_without_connect_returns_error() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut transport = LoopbackBleTransport::new("127.0.0.1:0");
            let result = transport.write_characteristic(0, 0, &[1, 2, 3]).await;
            assert!(
                matches!(result, Err(PairingError::ConnectionDropped)),
                "expected ConnectionDropped, got {result:?}"
            );
        });
    }

    #[test]
    fn read_without_connect_returns_error() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut transport = LoopbackBleTransport::new("127.0.0.1:0");
            let result = transport.read_indication(0, 0, 100).await;
            assert!(
                matches!(result, Err(PairingError::ConnectionDropped)),
                "expected ConnectionDropped, got {result:?}"
            );
        });
    }
}
