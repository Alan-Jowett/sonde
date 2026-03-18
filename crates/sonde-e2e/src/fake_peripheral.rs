// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Fake GATT peripheral for hardware-free BLE pairing integration tests.
//!
//! Exposes the sonde Gateway Pairing Service over a TCP socket.
//! Incoming connections are treated as BLE clients — each message is a raw
//! BLE envelope (`TYPE | LEN_BE16 | BODY`) read from the stream and
//! dispatched to [`handle_ble_recv`] from the gateway crate.
//!
//! See [issue #259](https://github.com/Alan-Jowett/sonde/issues/259).

use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use sonde_gateway::ble_pairing::{handle_ble_recv, RegistrationWindow};
use sonde_gateway::gateway_identity::GatewayIdentity;
use sonde_gateway::storage::Storage;
use sonde_gateway::InMemoryStorage;

/// Configuration for the fake GATT peripheral.
pub struct FakePeripheralConfig {
    /// TCP bind address (e.g. `"127.0.0.1:0"` for OS-assigned port).
    pub bind_addr: String,
    /// RF channel reported in `PHONE_REGISTERED` responses.
    pub rf_channel: u8,
    /// Duration (seconds) the registration window stays open.
    pub window_duration_s: u32,
}

impl Default for FakePeripheralConfig {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1:0".into(),
            rf_channel: 6,
            window_duration_s: 300,
        }
    }
}

/// A running fake GATT peripheral.
///
/// Use [`start`] to create one. The server runs in a background tokio task
/// and can be shut down via the [`cancel`](FakePeripheral::cancel) token.
pub struct FakePeripheral {
    /// The actual TCP address the server is listening on.
    pub addr: std::net::SocketAddr,
    /// Cancel this token to shut down the server.
    cancel: tokio_util::sync::CancellationToken,
    /// Handle to the server task.
    _handle: tokio::task::JoinHandle<()>,
}

impl FakePeripheral {
    /// Shut down the server.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// The local TCP address the server is listening on.
    pub fn addr(&self) -> std::net::SocketAddr {
        self.addr
    }
}

/// Start a fake GATT peripheral on a background tokio task.
///
/// Returns a [`FakePeripheral`] with the bound address and a cancel token.
pub async fn start(config: FakePeripheralConfig) -> std::io::Result<FakePeripheral> {
    let identity = GatewayIdentity::generate().map_err(|e| std::io::Error::other(e.to_string()))?;

    let storage: Arc<dyn Storage> = Arc::new(InMemoryStorage::new());

    let window = Arc::new(Mutex::new(RegistrationWindow::new()));
    window.lock().await.open(config.window_duration_s);

    let listener = TcpListener::bind(&config.bind_addr).await?;
    let addr = listener.local_addr()?;
    info!(%addr, "fake GATT peripheral listening");

    let cancel = tokio_util::sync::CancellationToken::new();
    let cancel_clone = cancel.clone();

    let rf_channel = config.rf_channel;

    let handle = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = cancel_clone.cancelled() => {
                    debug!("fake GATT peripheral shutting down");
                    break;
                }
                accept = listener.accept() => {
                    match accept {
                        Ok((stream, peer)) => {
                            debug!(%peer, "fake GATT peripheral: client connected");
                            let identity = identity.clone();
                            let storage = storage.clone();
                            let window = window.clone();
                            let cancel = cancel_clone.clone();
                            tokio::spawn(async move {
                                if let Err(e) = handle_connection(
                                    stream,
                                    &identity,
                                    &storage,
                                    &window,
                                    rf_channel,
                                    cancel,
                                ).await {
                                    debug!(%peer, error = %e, "client session ended");
                                }
                            });
                        }
                        Err(e) => {
                            warn!(error = %e, "accept failed");
                        }
                    }
                }
            }
        }
    });

    Ok(FakePeripheral {
        addr,
        cancel,
        _handle: handle,
    })
}

/// Read a complete BLE envelope from a TCP stream.
///
/// Returns `None` on clean EOF (client disconnected).
async fn read_envelope(stream: &mut TcpStream) -> Result<Option<Vec<u8>>, std::io::Error> {
    let mut header = [0u8; 3];
    match stream.read_exact(&mut header).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }

    let body_len = u16::from_be_bytes([header[1], header[2]]) as usize;
    let mut envelope = Vec::with_capacity(3 + body_len);
    envelope.extend_from_slice(&header);
    if body_len > 0 {
        envelope.resize(3 + body_len, 0);
        stream.read_exact(&mut envelope[3..]).await?;
    }
    Ok(Some(envelope))
}

/// Handle a single TCP client connection (one BLE "session").
async fn handle_connection(
    mut stream: TcpStream,
    identity: &GatewayIdentity,
    storage: &Arc<dyn Storage>,
    window: &Mutex<RegistrationWindow>,
    rf_channel: u8,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<(), std::io::Error> {
    loop {
        let envelope = tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            result = read_envelope(&mut stream) => result?,
        };

        let envelope = match envelope {
            Some(e) => e,
            None => return Ok(()), // clean disconnect
        };

        debug!(len = envelope.len(), "received BLE envelope");

        let response = {
            let mut win = window.lock().await;
            handle_ble_recv(&envelope, identity, storage, &mut win, rf_channel, None).await
        };

        if let Some(resp) = response {
            debug!(len = resp.len(), "sending BLE response");
            stream.write_all(&resp).await?;
            stream.flush().await?;
        }
    }
}
