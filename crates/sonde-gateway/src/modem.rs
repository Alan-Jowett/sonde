// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! USB-attached ESP-NOW modem transport.
//!
//! Implements the [`Transport`] trait over a serial link to an ESP-NOW radio
//! modem using the framing protocol defined in `sonde_protocol::modem`.

use std::sync::Arc;

use async_trait::async_trait;
use tokio::io::{split, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot, Mutex};
use tracing::{debug, error, info, warn};

use sonde_protocol::modem::{
    encode_modem_frame, FrameDecoder, ModemMessage, ModemReady, ModemStatus, SendFrame,
};

use crate::transport::{PeerAddress, Transport, TransportError};

/// Type-erased async writer behind a shared mutex.
type SharedWriter = Arc<Mutex<Box<dyn AsyncWrite + Send + Unpin>>>;

/// Transport implementation backed by a USB-attached ESP-NOW radio modem.
///
/// The modem communicates over a serial link using the modem framing protocol.
/// A background reader task demultiplexes incoming messages and routes them to
/// the appropriate consumer (recv channel, oneshot signals, etc.).
pub struct UsbEspNowTransport {
    writer: SharedWriter,
    recv_rx: Mutex<mpsc::Receiver<(Vec<u8>, PeerAddress)>>,
    status_slot: Arc<Mutex<Option<oneshot::Sender<ModemStatus>>>>,
    modem_mac: [u8; 6],
    reader_handle: tokio::task::JoinHandle<()>,
}

impl Drop for UsbEspNowTransport {
    fn drop(&mut self) {
        self.reader_handle.abort();
    }
}

impl UsbEspNowTransport {
    /// Create a new transport over the given serial port and configure the
    /// modem to operate on `channel`.
    ///
    /// Performs the full startup sequence: RESET → MODEM_READY → SET_CHANNEL
    /// → SET_CHANNEL_ACK.
    pub async fn new<S>(port: S, channel: u8) -> Result<Self, TransportError>
    where
        S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        let (read_half, write_half) = split(port);
        let writer: SharedWriter = Arc::new(Mutex::new(Box::new(write_half)));

        let (recv_tx, recv_rx) = mpsc::channel::<(Vec<u8>, PeerAddress)>(256);
        let (ready_tx, ready_rx) = oneshot::channel::<ModemReady>();
        let (ack_tx, ack_rx) = oneshot::channel::<u8>();
        let status_slot: Arc<Mutex<Option<oneshot::Sender<ModemStatus>>>> =
            Arc::new(Mutex::new(None));

        // Start background reader task.
        let reader_handle = {
            let status_slot = Arc::clone(&status_slot);
            let mut read_half = read_half;
            let mut decoder = FrameDecoder::new();
            // Oneshots are registered before RESET is sent. A stale
            // MODEM_READY from a prior session could consume the
            // oneshot early, but reset_and_wait handles this by
            // treating a closed channel as an error and retrying.
            let mut ready_tx = Some(ready_tx);
            let mut ack_tx = Some(ack_tx);

            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                loop {
                    match read_half.read(&mut buf).await {
                        Ok(0) => {
                            debug!("modem serial port closed");
                            break;
                        }
                        Ok(n) => {
                            decoder.push(&buf[..n]);
                            loop {
                                match decoder.decode() {
                                    Ok(Some(msg)) => {
                                        dispatch_message(
                                            msg,
                                            &recv_tx,
                                            &mut ready_tx,
                                            &mut ack_tx,
                                            &status_slot,
                                        )
                                        .await;
                                    }
                                    Ok(None) => break,
                                    Err(ref e)
                                        if matches!(
                                            e,
                                            sonde_protocol::modem::ModemCodecError::EmptyFrame
                                        ) =>
                                    {
                                        // EmptyFrame: frame was drained, try again
                                        continue;
                                    }
                                    Err(ref e)
                                        if matches!(
                                            e,
                                            sonde_protocol::modem::ModemCodecError::FrameTooLarge(
                                                _
                                            )
                                        ) =>
                                    {
                                        error!(
                                            "modem frame too large — terminating reader to force reconnect: {e}"
                                        );
                                        decoder.reset();
                                        // Reader task does not have write access to send a
                                        // RESET. Terminate so recv() returns an error and
                                        // higher-level code can tear down and rebuild the
                                        // transport (which sends RESET in its constructor).
                                        return;
                                    }
                                    Err(e) => {
                                        warn!("modem decode error: {e}");
                                        continue;
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            error!("modem serial read error: {e}");
                            break;
                        }
                    }
                }
            })
        };

        // Wrap handle in abort-on-drop guard so that if the constructor
        // future is cancelled/dropped mid-handshake, the task is aborted.
        struct AbortGuard(Option<tokio::task::JoinHandle<()>>);
        impl Drop for AbortGuard {
            fn drop(&mut self) {
                if let Some(h) = self.0.take() {
                    h.abort();
                }
            }
        }
        let mut guard = AbortGuard(Some(reader_handle));

        // Run startup sequence; guard aborts reader on failure or cancel.
        let startup_result = async {
            let modem_ready = Self::reset_and_wait(Arc::clone(&writer), ready_rx).await?;
            let modem_mac = modem_ready.mac_address;
            info!(
                firmware = ?modem_ready.firmware_version,
                mac = ?modem_mac,
                "modem ready"
            );
            Self::set_channel(Arc::clone(&writer), channel, ack_rx).await?;
            Ok::<_, TransportError>(modem_mac)
        }
        .await;

        match startup_result {
            Ok(modem_mac) => {
                let reader_handle = guard.0.take().expect("guard still holds handle");
                Ok(Self {
                    writer,
                    recv_rx: Mutex::new(recv_rx),
                    status_slot,
                    modem_mac,
                    reader_handle,
                })
            }
            Err(e) => {
                // Guard will abort the reader task on drop.
                Err(e)
            }
        }
    }

    /// Return the modem's MAC address reported during startup.
    pub fn modem_mac(&self) -> &[u8; 6] {
        &self.modem_mac
    }

    /// Send GET_STATUS and wait for the STATUS response.
    pub async fn poll_status(&self) -> Result<ModemStatus, TransportError> {
        let rx = {
            let mut slot = self.status_slot.lock().await;
            if slot.is_some() {
                return Err(TransportError::Io("status poll already in progress".into()));
            }
            let (tx, rx) = oneshot::channel();
            *slot = Some(tx);
            rx
        };

        if let Err(e) = Self::send_encoded(&self.writer, &ModemMessage::GetStatus).await {
            self.status_slot.lock().await.take();
            return Err(e);
        }

        match tokio::time::timeout(std::time::Duration::from_secs(2), rx).await {
            Ok(Ok(status)) => Ok(status),
            Ok(Err(_)) => {
                // Channel closed — slot already consumed by dispatch_message
                Err(TransportError::Io("status channel closed".into()))
            }
            Err(_) => {
                // Timeout — clear the slot so future calls work
                self.status_slot.lock().await.take();
                Err(TransportError::Io("STATUS response timeout".into()))
            }
        }
    }

    // -- internal helpers ---------------------------------------------------

    async fn send_encoded(writer: &SharedWriter, msg: &ModemMessage) -> Result<(), TransportError> {
        let frame = encode_modem_frame(msg)
            .map_err(|e| TransportError::Io(format!("encode modem frame: {e}")))?;
        let mut w = writer.lock().await;
        w.write_all(&frame)
            .await
            .map_err(|e| TransportError::Io(format!("write modem frame: {e}")))?;
        w.flush()
            .await
            .map_err(|e| TransportError::Io(format!("flush modem frame: {e}")))?;
        Ok(())
    }

    async fn reset_and_wait(
        writer: SharedWriter,
        ready_rx: oneshot::Receiver<ModemReady>,
    ) -> Result<ModemReady, TransportError> {
        // We can only receive from the oneshot once, so we attempt up to 3
        // RESETs but share a single receiver for the MODEM_READY response.
        // Because the background task sends on the oneshot at most once, we
        // send the first RESET immediately and retry by sending additional
        // RESETs while waiting on the same receiver.
        Self::send_encoded(&writer, &ModemMessage::Reset).await?;

        let total_timeout = std::time::Duration::from_secs(15);
        let retry_interval = std::time::Duration::from_secs(5);

        let ready_fut = ready_rx;
        tokio::pin!(ready_fut);

        let deadline = tokio::time::Instant::now() + total_timeout;
        let mut retries = 0u32;

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(TransportError::Io(
                    "modem did not respond with MODEM_READY (1 initial + 2 retries)".into(),
                ));
            }

            let wait = remaining.min(retry_interval);

            tokio::select! {
                result = &mut ready_fut => {
                    return result.map_err(|_| TransportError::Io("ready channel closed".into()));
                }
                _ = tokio::time::sleep(wait) => {
                    retries += 1;
                    if retries >= 3 {
                        return Err(TransportError::Io(
                            "modem did not respond with MODEM_READY (1 initial + 2 retries)".into(),
                        ));
                    }
                    warn!(retry = retries, "modem not ready, resending RESET");
                    Self::send_encoded(&writer, &ModemMessage::Reset).await?;
                }
            }
        }
    }

    async fn set_channel(
        writer: SharedWriter,
        channel: u8,
        ack_rx: oneshot::Receiver<u8>,
    ) -> Result<(), TransportError> {
        if !(1..=14).contains(&channel) {
            return Err(TransportError::Io(format!(
                "WiFi channel must be 1-14, got {channel}"
            )));
        }
        Self::send_encoded(&writer, &ModemMessage::SetChannel(channel)).await?;

        let ack = tokio::time::timeout(std::time::Duration::from_secs(2), ack_rx)
            .await
            .map_err(|_| TransportError::Io("SET_CHANNEL_ACK timeout".into()))?
            .map_err(|_| TransportError::Io("ack channel closed".into()))?;

        if ack != channel {
            return Err(TransportError::Io(format!(
                "SET_CHANNEL_ACK mismatch: expected channel {channel}, got {ack}"
            )));
        }

        info!(channel, "modem channel set");
        Ok(())
    }
}

#[async_trait]
impl Transport for UsbEspNowTransport {
    async fn recv(&self) -> Result<(Vec<u8>, PeerAddress), TransportError> {
        self.recv_rx
            .lock()
            .await
            .recv()
            .await
            .ok_or(TransportError::Io("modem reader task stopped".into()))
    }

    async fn send(&self, frame: &[u8], peer: &PeerAddress) -> Result<(), TransportError> {
        if peer.len() != 6 {
            return Err(TransportError::Io(format!(
                "peer address must be 6 bytes, got {}",
                peer.len()
            )));
        }
        if frame.is_empty() || frame.len() > sonde_protocol::modem::ESPNOW_MAX_DATA_SIZE {
            return Err(TransportError::Io(format!(
                "frame size {} out of range (1..={})",
                frame.len(),
                sonde_protocol::modem::ESPNOW_MAX_DATA_SIZE
            )));
        }
        let mut peer_mac = [0u8; 6];
        peer_mac.copy_from_slice(peer);

        let msg = ModemMessage::SendFrame(SendFrame {
            peer_mac,
            frame_data: frame.to_vec(),
        });

        Self::send_encoded(&self.writer, &msg).await
    }
}

/// Spawn a periodic health monitor for the modem transport.
///
/// Polls `GET_STATUS` every `interval` and logs tx_fail deltas and reboots.
/// Takes a `Weak` reference so the monitor exits automatically when the
/// transport is dropped (enabling the "drop + rebuild" recovery pattern).
pub fn spawn_health_monitor(
    transport: std::sync::Weak<UsbEspNowTransport>,
    interval: std::time::Duration,
    cancel: tokio_util::sync::CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if interval.is_zero() {
            warn!("health monitor interval is zero, disabling");
            return;
        }
        let mut prev_tx_fail: Option<u32> = None;
        let mut prev_uptime: Option<u32> = None;

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    debug!("health monitor cancelled");
                    return;
                }
                _ = tokio::time::sleep(interval) => {}
            }

            let transport = match transport.upgrade() {
                Some(t) => t,
                None => {
                    debug!("transport dropped, stopping health monitor");
                    return;
                }
            };

            match transport.poll_status().await {
                Ok(status) => {
                    if let Some(prev) = prev_tx_fail {
                        if status.tx_fail_count > prev {
                            let delta = status.tx_fail_count - prev;
                            warn!(
                                delta,
                                total = status.tx_fail_count,
                                "modem send failures detected"
                            );
                        }
                    }
                    if let Some(prev) = prev_uptime {
                        if status.uptime_s < prev {
                            warn!(
                                old_uptime = prev,
                                new_uptime = status.uptime_s,
                                "modem reboot detected (uptime decreased)"
                            );
                        }
                    }
                    prev_tx_fail = Some(status.tx_fail_count);
                    prev_uptime = Some(status.uptime_s);

                    debug!(
                        channel = status.channel,
                        uptime_s = status.uptime_s,
                        tx = status.tx_count,
                        rx = status.rx_count,
                        tx_fail = status.tx_fail_count,
                        "modem health poll"
                    );
                }
                Err(e) => {
                    warn!(error = %e, "modem health poll failed");
                }
            }
        }
    })
}

/// Route a decoded modem message to the appropriate consumer.
async fn dispatch_message(
    msg: ModemMessage,
    recv_tx: &mpsc::Sender<(Vec<u8>, PeerAddress)>,
    ready_tx: &mut Option<oneshot::Sender<ModemReady>>,
    ack_tx: &mut Option<oneshot::Sender<u8>>,
    status_slot: &Arc<Mutex<Option<oneshot::Sender<ModemStatus>>>>,
) {
    match msg {
        ModemMessage::RecvFrame(rf) => {
            let peer = rf.peer_mac.to_vec();
            match recv_tx.try_send((rf.frame_data, peer)) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    debug!("recv channel full, dropping frame");
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    debug!("recv channel closed, dropping frame");
                }
            }
        }
        ModemMessage::ModemReady(mr) => {
            if let Some(tx) = ready_tx.take() {
                let _ = tx.send(mr);
            } else {
                info!("unexpected MODEM_READY (no pending waiter)");
            }
        }
        ModemMessage::SetChannelAck(ch) => {
            if let Some(tx) = ack_tx.take() {
                let _ = tx.send(ch);
            } else {
                debug!(channel = ch, "unexpected SET_CHANNEL_ACK");
            }
        }
        ModemMessage::Status(s) => {
            let mut slot = status_slot.lock().await;
            if let Some(tx) = slot.take() {
                let _ = tx.send(s);
            } else {
                debug!("STATUS received with no pending request");
            }
        }
        ModemMessage::Error(e) => {
            error!(
                code = e.error_code,
                message = ?String::from_utf8_lossy(&e.message),
                "modem error"
            );
            // GW-1103: Error is logged and waiters unblocked so pending
            // operations fail immediately. The reader task continues
            // running (non-fatal). Full RESET recovery requires the
            // caller (gateway main loop) to cancel the health monitor,
            // drop the transport, and reconstruct it — which re-runs
            // the startup handshake including RESET.
            if ready_tx.take().is_some() {
                debug!("cancelling pending MODEM_READY waiter due to modem error");
            }
            if ack_tx.take().is_some() {
                debug!("cancelling pending SET_CHANNEL_ACK waiter due to modem error");
            }
            {
                let mut slot = status_slot.lock().await;
                if slot.take().is_some() {
                    debug!("cancelling pending STATUS waiter due to modem error");
                }
            }
        }
        other => {
            debug!(?other, "ignoring modem message");
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use sonde_protocol::modem::{
        encode_modem_frame, FrameDecoder, ModemMessage, ModemReady, ModemStatus, RecvFrame,
    };
    use tokio::io::{duplex, AsyncReadExt, AsyncWriteExt, DuplexStream};

    /// Run the modem startup handshake on the mock (server) side of a duplex.
    ///
    /// Reads the RESET command, writes MODEM_READY, reads SET_CHANNEL,
    /// writes SET_CHANNEL_ACK.  Returns the channel number requested.
    async fn do_startup_handshake(server: &mut DuplexStream, _channel: u8) -> u8 {
        let mut decoder = FrameDecoder::new();
        let mut buf = [0u8; 256];

        // 1. Read RESET
        let msg = read_next_message(server, &mut decoder, &mut buf).await;
        assert!(
            matches!(msg, ModemMessage::Reset),
            "expected Reset, got {msg:?}"
        );

        // 2. Send MODEM_READY
        let ready = ModemMessage::ModemReady(ModemReady {
            firmware_version: [1, 2, 3, 4],
            mac_address: [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF],
        });
        let frame = encode_modem_frame(&ready).unwrap();
        server.write_all(&frame).await.unwrap();

        // 3. Read SET_CHANNEL
        let msg = read_next_message(server, &mut decoder, &mut buf).await;
        let requested_channel = match msg {
            ModemMessage::SetChannel(ch) => ch,
            other => panic!("expected SetChannel, got {other:?}"),
        };

        // 4. Send SET_CHANNEL_ACK
        let ack = ModemMessage::SetChannelAck(requested_channel);
        let frame = encode_modem_frame(&ack).unwrap();
        server.write_all(&frame).await.unwrap();

        requested_channel
    }

    /// Read bytes from the stream until a complete modem message is decoded.
    async fn read_next_message(
        stream: &mut DuplexStream,
        decoder: &mut FrameDecoder,
        buf: &mut [u8],
    ) -> ModemMessage {
        loop {
            // First try to decode from already-buffered data.
            match decoder.decode() {
                Ok(Some(msg)) => return msg,
                Ok(None) => {}
                Err(e) => panic!("decode error: {e}"),
            }
            let n = stream.read(buf).await.expect("read failed");
            assert!(n > 0, "stream closed unexpectedly");
            decoder.push(&buf[..n]);
        }
    }

    #[tokio::test]
    async fn t1100_recv_delivers_recv_frame() {
        let (client, mut server) = duplex(1024);

        // Drive startup in background while constructor awaits.
        let startup = tokio::spawn(async move {
            do_startup_handshake(&mut server, 6).await;
            server
        });

        let transport = UsbEspNowTransport::new(client, 6).await.unwrap();
        let mut server = startup.await.unwrap();

        // Inject a RECV_FRAME on the mock side.
        let recv_msg = ModemMessage::RecvFrame(RecvFrame {
            peer_mac: [1, 2, 3, 4, 5, 6],
            rssi: -42,
            frame_data: vec![0xDE, 0xAD],
        });
        let frame = encode_modem_frame(&recv_msg).unwrap();
        server.write_all(&frame).await.unwrap();

        // transport.recv() should deliver it.
        let (data, peer) = transport.recv().await.unwrap();
        assert_eq!(data, vec![0xDE, 0xAD]);
        assert_eq!(peer, vec![1, 2, 3, 4, 5, 6]);
    }

    #[tokio::test]
    async fn t1101_send_produces_send_frame() {
        let (client, mut server) = duplex(1024);

        let startup = tokio::spawn(async move {
            do_startup_handshake(&mut server, 6).await;
            server
        });

        let transport = UsbEspNowTransport::new(client, 6).await.unwrap();
        let mut server = startup.await.unwrap();

        // Send via transport.
        let peer: PeerAddress = vec![0x10, 0x20, 0x30, 0x40, 0x50, 0x60];
        transport.send(&[0xCA, 0xFE], &peer).await.unwrap();

        // Read the SEND_FRAME from the mock side.
        let mut decoder = FrameDecoder::new();
        let mut buf = [0u8; 256];
        let msg = read_next_message(&mut server, &mut decoder, &mut buf).await;

        match msg {
            ModemMessage::SendFrame(sf) => {
                assert_eq!(sf.peer_mac, [0x10, 0x20, 0x30, 0x40, 0x50, 0x60]);
                assert_eq!(sf.frame_data, vec![0xCA, 0xFE]);
            }
            other => panic!("expected SendFrame, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn t1103_startup_sequence() {
        let (client, mut server) = duplex(1024);

        let startup = tokio::spawn(async move {
            let ch = do_startup_handshake(&mut server, 11).await;
            (server, ch)
        });

        let transport = UsbEspNowTransport::new(client, 11).await.unwrap();
        let (_server, ch) = startup.await.unwrap();

        assert_eq!(ch, 11, "channel mismatch");
        assert_eq!(transport.modem_mac(), &[0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
    }

    #[tokio::test]
    async fn t1104_startup_timeout() {
        let (client, _server) = duplex(1024);

        // No server-side handler — constructor should time out.
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            UsbEspNowTransport::new(client, 1),
        )
        .await;

        match result {
            Ok(Err(TransportError::Io(msg))) => {
                assert!(
                    msg.contains("MODEM_READY"),
                    "expected MODEM_READY timeout, got: {msg}"
                );
            }
            Ok(Ok(_)) => panic!("expected error, got Ok"),
            Ok(Err(e)) => panic!("unexpected error variant: {e:?}"),
            Err(_) => panic!("outer timeout — test took too long"),
        }
    }

    #[tokio::test]
    async fn t1105_poll_status_success() {
        let (client, mut server) = duplex(1024);

        let startup = tokio::spawn(async move {
            do_startup_handshake(&mut server, 6).await;
            server
        });

        let transport = UsbEspNowTransport::new(client, 6).await.unwrap();
        let mut server = startup.await.unwrap();

        // Drive poll_status in background.
        let poll = tokio::spawn(async move { transport.poll_status().await });

        // Read GET_STATUS from mock side and respond with STATUS.
        let mut decoder = FrameDecoder::new();
        let mut buf = [0u8; 256];
        let msg = read_next_message(&mut server, &mut decoder, &mut buf).await;
        assert!(matches!(msg, ModemMessage::GetStatus));

        let status_msg = ModemMessage::Status(ModemStatus {
            channel: 6,
            uptime_s: 120,
            tx_count: 10,
            rx_count: 5,
            tx_fail_count: 1,
        });
        server
            .write_all(&encode_modem_frame(&status_msg).unwrap())
            .await
            .unwrap();

        let status = poll.await.unwrap().unwrap();
        assert_eq!(status.channel, 6);
        assert_eq!(status.uptime_s, 120);
        assert_eq!(status.tx_fail_count, 1);
    }
}
