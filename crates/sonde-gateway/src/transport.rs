// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use std::collections::VecDeque;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex;

/// Opaque address type for the transport layer (e.g., MAC address for ESP-NOW).
pub type PeerAddress = Vec<u8>;

/// Errors returned by transport operations.
#[derive(Debug, Clone)]
pub enum TransportError {
    /// No more inbound frames available (mock transport only).
    NoMoreFrames,
    /// Generic I/O or transport-layer error.
    Io(String),
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TransportError::NoMoreFrames => write!(f, "no more inbound frames"),
            TransportError::Io(msg) => write!(f, "transport I/O error: {}", msg),
        }
    }
}

impl std::error::Error for TransportError {}

/// Abstract transport trait. Implementations wrap a specific radio or
/// network layer (ESP-NOW, UDP, etc.).
#[async_trait]
pub trait Transport: Send + Sync {
    /// Receive the next inbound frame (blocking until available).
    /// Returns the raw bytes (header + payload + HMAC) and the
    /// sender's transport-layer address.
    async fn recv(&self) -> Result<(Vec<u8>, PeerAddress), TransportError>;

    /// Send a frame to a specific peer by transport-layer address.
    async fn send(&self, frame: &[u8], peer: &PeerAddress) -> Result<(), TransportError>;
}

/// Captured outbound frame for test assertions.
#[derive(Debug, Clone)]
pub struct CapturedFrame {
    pub data: Vec<u8>,
    pub peer: PeerAddress,
}

type InboundQueue = VecDeque<(Vec<u8>, PeerAddress)>;

/// Mock transport for testing. Queue inbound frames and capture outbound frames.
pub struct MockTransport {
    inbound: Arc<Mutex<InboundQueue>>,
    outbound: Arc<Mutex<Vec<CapturedFrame>>>,
}

impl MockTransport {
    pub fn new() -> Self {
        Self {
            inbound: Arc::new(Mutex::new(VecDeque::new())),
            outbound: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Queue an inbound frame to be returned by the next `recv()` call.
    pub async fn queue_inbound(&self, frame: Vec<u8>, peer: PeerAddress) {
        self.inbound.lock().await.push_back((frame, peer));
    }

    /// Take all captured outbound frames.
    pub async fn take_outbound(&self) -> Vec<CapturedFrame> {
        let mut out = self.outbound.lock().await;
        std::mem::take(&mut *out)
    }

    /// Return the number of captured outbound frames.
    pub async fn outbound_count(&self) -> usize {
        self.outbound.lock().await.len()
    }
}

impl Default for MockTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Transport for MockTransport {
    async fn recv(&self) -> Result<(Vec<u8>, PeerAddress), TransportError> {
        self.inbound
            .lock()
            .await
            .pop_front()
            .ok_or(TransportError::NoMoreFrames)
    }

    async fn send(&self, frame: &[u8], peer: &PeerAddress) -> Result<(), TransportError> {
        self.outbound.lock().await.push(CapturedFrame {
            data: frame.to_vec(),
            peer: peer.clone(),
        });
        Ok(())
    }
}
