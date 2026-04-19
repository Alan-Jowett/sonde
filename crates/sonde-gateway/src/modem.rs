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
    encode_modem_frame, BleConnected, BleDisconnected, BleIndicate, BlePairingConfirm,
    BlePairingConfirmReply, BleRecv, FrameDecoder, ModemMessage, ModemReady, ModemStatus,
    ScanResult, SendFrame,
};

use sonde_protocol::constants::{
    MSG_APP_DATA, MSG_APP_DATA_REPLY, MSG_CHUNK, MSG_COMMAND, MSG_DIAG_REPLY, MSG_DIAG_REQUEST,
    MSG_GET_CHUNK, MSG_PEER_ACK, MSG_PEER_REQUEST, MSG_PROGRAM_ACK, MSG_WAKE, OFFSET_MSG_TYPE,
};

use crate::transport::{PeerAddress, Transport, TransportError};

/// Extract a human-readable protocol message type from a raw frame's header byte.
fn protocol_msg_type_label(frame: &[u8]) -> &'static str {
    if frame.len() <= OFFSET_MSG_TYPE {
        return "unknown";
    }
    match frame[OFFSET_MSG_TYPE] {
        MSG_WAKE => "WAKE",
        MSG_GET_CHUNK => "GET_CHUNK",
        MSG_PROGRAM_ACK => "PROGRAM_ACK",
        MSG_APP_DATA => "APP_DATA",
        MSG_PEER_REQUEST => "PEER_REQUEST",
        MSG_DIAG_REQUEST => "DIAG_REQUEST",
        MSG_COMMAND => "COMMAND",
        MSG_CHUNK => "CHUNK",
        MSG_APP_DATA_REPLY => "APP_DATA_REPLY",
        MSG_PEER_ACK => "PEER_ACK",
        MSG_DIAG_REPLY => "DIAG_REPLY",
        _ => "unknown",
    }
}

/// Return a human-readable label for a `ModemMessage` variant.
fn modem_msg_label(msg: &ModemMessage) -> &'static str {
    match msg {
        ModemMessage::Reset => "RESET",
        ModemMessage::ModemReady(_) => "MODEM_READY",
        ModemMessage::SendFrame(_) => "SEND_FRAME",
        ModemMessage::RecvFrame(_) => "RECV_FRAME",
        ModemMessage::SetChannel(_) => "SET_CHANNEL",
        ModemMessage::SetChannelAck(_) => "SET_CHANNEL_ACK",
        ModemMessage::GetStatus => "GET_STATUS",
        ModemMessage::Status(_) => "STATUS",
        ModemMessage::Error(_) => "ERROR",
        ModemMessage::BleEnable => "BLE_ENABLE",
        ModemMessage::BleDisable => "BLE_DISABLE",
        ModemMessage::BleIndicate(_) => "BLE_INDICATE",
        ModemMessage::BleRecv(_) => "BLE_RECV",
        ModemMessage::BleConnected(_) => "BLE_CONNECTED",
        ModemMessage::BleDisconnected(_) => "BLE_DISCONNECTED",
        ModemMessage::BlePairingConfirm(_) => "BLE_PAIRING_CONFIRM",
        ModemMessage::BlePairingConfirmReply(_) => "BLE_PAIRING_CONFIRM_REPLY",
        ModemMessage::ScanChannels => "SCAN_CHANNELS",
        ModemMessage::ScanResult(_) => "SCAN_RESULT",
        ModemMessage::Unknown { .. } | _ => "UNKNOWN",
    }
}

/// BLE event received from the modem, forwarded to the gateway's BLE
/// pairing state machine.
#[derive(Debug, Clone)]
pub enum BleEvent {
    /// BLE GATT write received from a connected phone.
    Recv(BleRecv),
    /// Phone connected via BLE (includes negotiated MTU).
    Connected(BleConnected),
    /// Phone disconnected from BLE.
    Disconnected(BleDisconnected),
    /// Numeric Comparison passkey from the modem for operator confirmation.
    PairingConfirm(BlePairingConfirm),
}

/// Type-erased async writer behind a shared mutex.
type SharedWriter = Arc<Mutex<Box<dyn AsyncWrite + Send + Unpin>>>;

/// Transport implementation backed by a USB-attached ESP-NOW radio modem.
///
/// The modem communicates over a serial link using the modem framing protocol.
/// A background reader task demultiplexes incoming messages and routes them to
/// the appropriate consumer (recv channel, oneshot signals, etc.).
pub struct UsbEspNowTransport {
    writer: SharedWriter,
    recv_rx: Mutex<mpsc::Receiver<(Vec<u8>, PeerAddress, i8)>>,
    ble_rx: Mutex<mpsc::Receiver<BleEvent>>,
    status_slot: Arc<Mutex<Option<oneshot::Sender<ModemStatus>>>>,
    channel_ack_slot: Arc<std::sync::Mutex<Option<oneshot::Sender<u8>>>>,
    scan_slot: Arc<std::sync::Mutex<Option<oneshot::Sender<ScanResult>>>>,
    modem_mac: [u8; 6],
    reader_handle: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    warm_reboot_notify: Arc<tokio::sync::Notify>,
    warm_reboot_flag: Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for UsbEspNowTransport {
    fn drop(&mut self) {
        let mut guard = match self.reader_handle.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(h) = guard.take() {
            h.abort();
        }
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

        let (recv_tx, recv_rx) = mpsc::channel::<(Vec<u8>, PeerAddress, i8)>(256);
        let (ble_tx, ble_rx) = mpsc::channel::<BleEvent>(64);
        let (ready_tx, ready_rx) = oneshot::channel::<ModemReady>();
        let (ack_tx, ack_rx) = oneshot::channel::<u8>();
        let status_slot: Arc<Mutex<Option<oneshot::Sender<ModemStatus>>>> =
            Arc::new(Mutex::new(None));
        let channel_ack_slot: Arc<std::sync::Mutex<Option<oneshot::Sender<u8>>>> =
            Arc::new(std::sync::Mutex::new(None));
        let scan_slot: Arc<std::sync::Mutex<Option<oneshot::Sender<ScanResult>>>> =
            Arc::new(std::sync::Mutex::new(None));

        let warm_reboot_notify: Arc<tokio::sync::Notify> = Arc::new(tokio::sync::Notify::new());
        let warm_reboot_flag: Arc<std::sync::atomic::AtomicBool> =
            Arc::new(std::sync::atomic::AtomicBool::new(false));

        // Start background reader task.
        let reader_handle = {
            let status_slot = Arc::clone(&status_slot);
            let channel_ack_slot = Arc::clone(&channel_ack_slot);
            let scan_slot = Arc::clone(&scan_slot);
            let warm_reboot_notify = Arc::clone(&warm_reboot_notify);
            let warm_reboot_flag = Arc::clone(&warm_reboot_flag);
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
                                            &ble_tx,
                                            &mut ready_tx,
                                            &mut ack_tx,
                                            &status_slot,
                                            &channel_ack_slot,
                                            &scan_slot,
                                            &warm_reboot_notify,
                                            &warm_reboot_flag,
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
                                        warn!(
                                            operation = "modem frame decode",
                                            error = %e,
                                            guidance = "resetting decoder; this typically indicates boot log garbage on the serial line",
                                            "modem frame too large — resetting decoder"
                                        );
                                        decoder.reset();
                                        break; // break inner loop, read more data
                                    }
                                    Err(e) => {
                                        warn!(
                                            operation = "modem frame decode",
                                            error = %e,
                                            "modem decode error; skipping frame"
                                        );
                                        continue;
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            error!(
                                operation = "modem serial read",
                                error = %e,
                                guidance = "check serial cable connection and port permissions; the gateway will attempt to reconnect",
                                "modem serial read error"
                            );
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
                    ble_rx: Mutex::new(ble_rx),
                    status_slot,
                    channel_ack_slot,
                    scan_slot,
                    modem_mac,
                    reader_handle: std::sync::Mutex::new(Some(reader_handle)),
                    warm_reboot_notify,
                    warm_reboot_flag,
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

    /// Return a clone of the warm-reboot notification primitive.
    ///
    /// The notify fires when the reader task detects an unexpected `MODEM_READY`
    /// (GW-1103 AC7).  Pair with [`warm_reboot_flag`] to avoid losing the
    /// notification if a competing `select!` arm wins.
    pub fn warm_reboot_notify(&self) -> Arc<tokio::sync::Notify> {
        Arc::clone(&self.warm_reboot_notify)
    }

    /// Return a clone of the warm-reboot flag.
    ///
    /// Set to `true` (Release ordering) immediately before
    /// [`warm_reboot_notify`] fires so the caller can detect a warm reboot
    /// even if the `notified()` future was dropped by a competing `select!` arm.
    pub fn warm_reboot_flag(&self) -> Arc<std::sync::atomic::AtomicBool> {
        Arc::clone(&self.warm_reboot_flag)
    }

    /// Abort the background reader task and wait for it to exit.
    ///
    /// Must be called before dropping the transport during warm-reboot
    /// recovery to ensure the serial port read half is released before the
    /// next `UsbEspNowTransport::new()` call opens the port.
    pub async fn abort_reader_and_wait(&self) {
        let handle = {
            let mut guard = self.reader_handle.lock().unwrap_or_else(|e| e.into_inner());
            guard.take()
        };
        if let Some(h) = handle {
            h.abort();
            let _ = h.await;
        }
    }

    /// Receive the next BLE event from the modem.
    ///
    /// Returns `None` when the reader task has stopped.
    pub async fn recv_ble_event(&self) -> Option<BleEvent> {
        self.ble_rx.lock().await.recv().await
    }

    /// Receive the next inbound frame with its RSSI measurement.
    ///
    /// Returns `(frame_data, peer_address, rssi)`.
    pub async fn recv_with_rssi(&self) -> Result<(Vec<u8>, PeerAddress, i8), TransportError> {
        self.recv_rx
            .lock()
            .await
            .recv()
            .await
            .ok_or(TransportError::Io("modem reader task stopped".into()))
    }

    /// Send a BLE_INDICATE command to the modem (relayed to the connected phone).
    pub async fn send_ble_indicate(&self, data: &[u8]) -> Result<(), TransportError> {
        let msg = ModemMessage::BleIndicate(BleIndicate {
            ble_data: data.to_vec(),
        });
        Self::send_encoded(&self.writer, &msg).await
    }

    /// Send a BLE_ENABLE command to the modem (start BLE advertising).
    pub async fn send_ble_enable(&self) -> Result<(), TransportError> {
        Self::send_encoded(&self.writer, &ModemMessage::BleEnable).await
    }

    /// Send a BLE_DISABLE command to the modem (stop BLE advertising).
    pub async fn send_ble_disable(&self) -> Result<(), TransportError> {
        Self::send_encoded(&self.writer, &ModemMessage::BleDisable).await
    }

    /// Send a BLE_PAIRING_CONFIRM_REPLY to the modem (accept/reject Numeric Comparison).
    pub async fn send_ble_pairing_confirm_reply(&self, accept: bool) -> Result<(), TransportError> {
        let msg = ModemMessage::BlePairingConfirmReply(BlePairingConfirmReply { accept });
        Self::send_encoded(&self.writer, &msg).await
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
                Err(TransportError::Io(
                    "poll status: status channel closed unexpectedly".into(),
                ))
            }
            Err(_) => {
                // Timeout — clear the slot so future calls work
                self.status_slot.lock().await.take();
                Err(TransportError::Io(
                    "poll status: modem did not respond within 2s; \
                     check modem health and serial connection"
                        .into(),
                ))
            }
        }
    }

    /// Send SET_CHANNEL and wait for the SET_CHANNEL_ACK response.
    ///
    /// Unlike the startup `set_channel` (which consumes a pre-created oneshot),
    /// this can be called at any time after construction.  The slot is cleared
    /// on drop (cancellation-safe).
    pub async fn change_channel(&self, channel: u8) -> Result<(), TransportError> {
        if !(1..=14).contains(&channel) {
            return Err(TransportError::Io(format!(
                "WiFi channel must be 1-14, got {channel}"
            )));
        }

        let rx = {
            let mut slot = self
                .channel_ack_slot
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if slot.is_some() {
                return Err(TransportError::Io(
                    "channel change already in progress".into(),
                ));
            }
            let (tx, rx) = oneshot::channel();
            *slot = Some(tx);
            rx
        };
        let _guard = SlotGuard(Arc::clone(&self.channel_ack_slot));

        Self::send_encoded(&self.writer, &ModemMessage::SetChannel(channel)).await?;

        match tokio::time::timeout(std::time::Duration::from_secs(2), rx).await {
            Ok(Ok(ack)) => {
                if ack != channel {
                    Err(TransportError::Io(format!(
                        "SET_CHANNEL_ACK mismatch: expected channel {channel}, got {ack}; \
                         modem may have a firmware issue"
                    )))
                } else {
                    info!(channel, "modem channel changed");
                    Ok(())
                }
            }
            Ok(Err(_)) => Err(TransportError::Io(format!(
                "change channel to {channel}: SET_CHANNEL_ACK channel closed unexpectedly"
            ))),
            Err(_) => Err(TransportError::Io(format!(
                "change channel to {channel}: SET_CHANNEL_ACK timeout (2s); \
                 check modem firmware and serial connection"
            ))),
        }
    }

    /// Send SCAN_CHANNELS and wait for the SCAN_RESULT response.
    ///
    /// The slot is cleared on drop (cancellation-safe).
    pub async fn scan_channels(&self) -> Result<ScanResult, TransportError> {
        let rx = {
            let mut slot = self.scan_slot.lock().unwrap_or_else(|e| e.into_inner());
            if slot.is_some() {
                return Err(TransportError::Io(
                    "channel scan already in progress".into(),
                ));
            }
            let (tx, rx) = oneshot::channel();
            *slot = Some(tx);
            rx
        };
        let _guard = SlotGuard(Arc::clone(&self.scan_slot));

        Self::send_encoded(&self.writer, &ModemMessage::ScanChannels).await?;

        match tokio::time::timeout(std::time::Duration::from_secs(10), rx).await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(_)) => Err(TransportError::Io("SCAN_RESULT channel closed".into())),
            Err(_) => Err(TransportError::Io("SCAN_RESULT timeout".into())),
        }
    }

    // -- internal helpers ---------------------------------------------------

    async fn send_encoded(writer: &SharedWriter, msg: &ModemMessage) -> Result<(), TransportError> {
        let msg_label = modem_msg_label(msg);
        let frame = encode_modem_frame(msg)
            .map_err(|e| TransportError::Io(format!("encode modem frame ({msg_label}): {e}")))?;
        let mut w = writer.lock().await;
        w.write_all(&frame).await.map_err(|e| {
            TransportError::Io(format!(
                "write modem frame ({msg_label}, {} bytes): {e}; \
                     check serial connection",
                frame.len()
            ))
        })?;
        w.flush().await.map_err(|e| {
            TransportError::Io(format!(
                "flush modem serial after {msg_label}: {e}; check serial connection"
            ))
        })?;
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
                    "modem startup failed: modem did not respond with MODEM_READY \
                     after 15s (1 initial + 2 retries); check that the modem is \
                     powered and the serial connection is correct"
                        .into(),
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
                            "modem startup failed: modem did not respond with MODEM_READY \
                             after 15s (1 initial + 2 retries); check that the modem is \
                             powered and the serial connection is correct"
                                .into(),
                        ));
                    }
                    warn!(
                        operation = "modem startup",
                        retry = retries,
                        "modem not ready, resending RESET"
                    );
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
            .map_err(|_| {
                TransportError::Io(format!(
                    "SET_CHANNEL_ACK timeout (channel {channel}): modem did not respond \
                     within 2s; check modem firmware and serial connection"
                ))
            })?
            .map_err(|_| {
                TransportError::Io(format!(
                    "SET_CHANNEL_ACK channel closed (channel {channel}): internal error"
                ))
            })?;

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
        let (frame, peer, _rssi) = self
            .recv_rx
            .lock()
            .await
            .recv()
            .await
            .ok_or(TransportError::Io("modem reader task stopped".into()))?;
        Ok((frame, peer))
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

        Self::send_encoded(&self.writer, &msg).await?;

        // GW-1302 AC2: log frame sent to modem at DEBUG level (after successful send).
        debug!(
            msg_type = protocol_msg_type_label(frame),
            peer_mac = ?peer_mac,
            len = frame.len(),
            "frame sent to modem"
        );

        Ok(())
    }
}

/// Default number of consecutive health poll failures before triggering a
/// modem reconnect (GW-1103 criterion 6).
pub const DEFAULT_MAX_HEALTH_POLL_FAILURES: u32 = 3;

/// Spawn a periodic health monitor for the modem transport.
///
/// Polls `GET_STATUS` every `interval` and logs tx_fail deltas and reboots.
/// Takes a `Weak` reference so the monitor exits automatically when the
/// transport is dropped (enabling the "drop + rebuild" recovery pattern).
///
/// After `max_consecutive_failures` consecutive poll failures the monitor
/// logs at `ERROR` level and returns `true`, signalling the caller that a
/// modem reconnect is needed.  Returns `false` on cancellation or when the
/// transport is dropped.
pub fn spawn_health_monitor(
    transport: std::sync::Weak<UsbEspNowTransport>,
    interval: std::time::Duration,
    cancel: tokio_util::sync::CancellationToken,
    max_consecutive_failures: u32,
) -> tokio::task::JoinHandle<bool> {
    tokio::spawn(async move {
        if interval.is_zero() {
            warn!("health monitor interval is zero, disabling");
            return false;
        }
        if max_consecutive_failures == 0 {
            warn!("max_consecutive_failures is zero, disabling health monitor");
            cancel.cancelled().await;
            return false;
        }
        let mut prev_tx_fail: Option<u32> = None;
        let mut prev_uptime: Option<u32> = None;
        let mut consecutive_failures: u32 = 0;

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    debug!("health monitor cancelled");
                    return false;
                }
                _ = tokio::time::sleep(interval) => {}
            }

            let transport = match transport.upgrade() {
                Some(t) => t,
                None => {
                    debug!("transport dropped, stopping health monitor");
                    return false;
                }
            };

            match transport.poll_status().await {
                Ok(status) => {
                    consecutive_failures = 0;
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
                    consecutive_failures = consecutive_failures.saturating_add(1);
                    if consecutive_failures >= max_consecutive_failures {
                        error!(
                            consecutive_failures,
                            error = %e,
                            "modem connection lost — sustained health poll failures, triggering reconnect"
                        );
                        return true;
                    }
                    warn!(
                        consecutive_failures,
                        max = max_consecutive_failures,
                        error = %e,
                        "modem health poll failed"
                    );
                }
            }
        }
    })
}

/// Cancellation-safe guard that clears a `std::sync::Mutex<Option<T>>` on drop.
struct SlotGuard<T>(Arc<std::sync::Mutex<Option<T>>>);

impl<T> Drop for SlotGuard<T> {
    fn drop(&mut self) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).take();
    }
}

/// Route a decoded modem message to the appropriate consumer.
#[allow(clippy::too_many_arguments)]
async fn dispatch_message(
    msg: ModemMessage,
    recv_tx: &mpsc::Sender<(Vec<u8>, PeerAddress, i8)>,
    ble_tx: &mpsc::Sender<BleEvent>,
    ready_tx: &mut Option<oneshot::Sender<ModemReady>>,
    ack_tx: &mut Option<oneshot::Sender<u8>>,
    status_slot: &Arc<Mutex<Option<oneshot::Sender<ModemStatus>>>>,
    channel_ack_slot: &Arc<std::sync::Mutex<Option<oneshot::Sender<u8>>>>,
    scan_slot: &Arc<std::sync::Mutex<Option<oneshot::Sender<ScanResult>>>>,
    warm_reboot_notify: &Arc<tokio::sync::Notify>,
    warm_reboot_flag: &Arc<std::sync::atomic::AtomicBool>,
) {
    match msg {
        ModemMessage::RecvFrame(rf) => {
            // GW-1302 AC1: log frame received from modem at DEBUG level.
            debug!(
                msg_type = protocol_msg_type_label(&rf.frame_data),
                peer_mac = ?rf.peer_mac,
                len = rf.frame_data.len(),
                rssi = rf.rssi,
                "frame received from modem"
            );
            let peer = rf.peer_mac.to_vec();
            let rssi = rf.rssi;
            match recv_tx.try_send((rf.frame_data, peer, rssi)) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    debug!("recv channel full, dropping frame");
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    debug!("recv channel closed, dropping frame");
                }
            }
        }
        ModemMessage::BleRecv(br) => {
            let send_result = tokio::time::timeout(
                std::time::Duration::from_millis(500),
                ble_tx.send(BleEvent::Recv(br)),
            )
            .await;
            if send_result.is_err() || matches!(send_result, Ok(Err(_))) {
                debug!("BLE event channel full/closed, dropping BLE_RECV");
            }
        }
        ModemMessage::BleConnected(bc) => {
            let send_result = tokio::time::timeout(
                std::time::Duration::from_millis(500),
                ble_tx.send(BleEvent::Connected(bc)),
            )
            .await;
            if send_result.is_err() || matches!(send_result, Ok(Err(_))) {
                debug!("BLE event channel full/closed, dropping BLE_CONNECTED");
            }
        }
        ModemMessage::BleDisconnected(bd) => {
            let send_result = tokio::time::timeout(
                std::time::Duration::from_millis(500),
                ble_tx.send(BleEvent::Disconnected(bd)),
            )
            .await;
            if send_result.is_err() || matches!(send_result, Ok(Err(_))) {
                debug!("BLE event channel full/closed, dropping BLE_DISCONNECTED");
            }
        }
        ModemMessage::BlePairingConfirm(pc) => {
            let send_result = tokio::time::timeout(
                std::time::Duration::from_millis(500),
                ble_tx.send(BleEvent::PairingConfirm(pc)),
            )
            .await;
            if send_result.is_err() || matches!(send_result, Ok(Err(_))) {
                debug!("BLE event channel full/closed, dropping BLE_PAIRING_CONFIRM");
            }
        }
        ModemMessage::ModemReady(mr) => {
            if let Some(tx) = ready_tx.take() {
                let _ = tx.send(mr);
            } else {
                // GW-1103 AC7: unexpected MODEM_READY signals a modem warm reboot.
                // Cancel all pending waiters so in-flight operations fail immediately.
                warn!(
                    guidance = "gateway will abort consumer tasks and re-run startup sequence",
                    "modem warm reboot detected"
                );
                if ack_tx.take().is_some() {
                    debug!("cancelling startup SET_CHANNEL_ACK waiter due to warm reboot");
                }
                {
                    let mut slot = status_slot.lock().await;
                    if slot.take().is_some() {
                        debug!("cancelling pending STATUS waiter due to warm reboot");
                    }
                }
                {
                    let mut slot = channel_ack_slot.lock().unwrap_or_else(|e| e.into_inner());
                    if slot.take().is_some() {
                        debug!("cancelling pending SET_CHANNEL_ACK waiter due to warm reboot");
                    }
                }
                {
                    let mut slot = scan_slot.lock().unwrap_or_else(|e| e.into_inner());
                    if slot.take().is_some() {
                        debug!("cancelling pending SCAN_RESULT waiter due to warm reboot");
                    }
                }
                // Set the flag before notifying so the caller can't miss the event.
                warm_reboot_flag.store(true, std::sync::atomic::Ordering::Release);
                warm_reboot_notify.notify_one();
            }
        }
        ModemMessage::SetChannelAck(ch) => {
            // During startup, use the local oneshot. After startup (ack_tx
            // consumed), fall through to the shared slot for runtime changes.
            if let Some(tx) = ack_tx.take() {
                let _ = tx.send(ch);
            } else {
                let mut slot = channel_ack_slot.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(tx) = slot.take() {
                    let _ = tx.send(ch);
                } else {
                    debug!(channel = ch, "unexpected SET_CHANNEL_ACK");
                }
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
        ModemMessage::ScanResult(sr) => {
            let mut slot = scan_slot.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(tx) = slot.take() {
                let _ = tx.send(sr);
            } else {
                debug!("SCAN_RESULT received with no pending request");
            }
        }
        ModemMessage::Error(e) => {
            error!(
                operation = "modem error report",
                error_code = e.error_code,
                message = ?String::from_utf8_lossy(&e.message),
                guidance = "modem reported an error; pending operations will fail. If errors persist, check modem firmware and serial connection",
                "modem error received from firmware"
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
            {
                let mut slot = channel_ack_slot.lock().unwrap_or_else(|e| e.into_inner());
                if slot.take().is_some() {
                    debug!("cancelling pending SET_CHANNEL_ACK waiter due to modem error");
                }
            }
            {
                let mut slot = scan_slot.lock().unwrap_or_else(|e| e.into_inner());
                if slot.take().is_some() {
                    debug!("cancelling pending SCAN_RESULT waiter due to modem error");
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

    #[tokio::test]
    async fn t1106_change_channel_success() {
        let (client, mut server) = duplex(1024);

        let startup = tokio::spawn(async move {
            do_startup_handshake(&mut server, 6).await;
            server
        });

        let transport = UsbEspNowTransport::new(client, 6).await.unwrap();
        let mut server = startup.await.unwrap();

        let change = tokio::spawn(async move { transport.change_channel(11).await });

        let mut decoder = FrameDecoder::new();
        let mut buf = [0u8; 256];
        let msg = read_next_message(&mut server, &mut decoder, &mut buf).await;
        assert!(
            matches!(msg, ModemMessage::SetChannel(11)),
            "expected SetChannel(11), got {msg:?}"
        );

        let ack = ModemMessage::SetChannelAck(11);
        server
            .write_all(&encode_modem_frame(&ack).unwrap())
            .await
            .unwrap();

        change.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn t1107_change_channel_invalid() {
        let (client, mut server) = duplex(1024);

        let startup = tokio::spawn(async move {
            do_startup_handshake(&mut server, 6).await;
            server
        });

        let transport = UsbEspNowTransport::new(client, 6).await.unwrap();
        let _server = startup.await.unwrap();

        let result = transport.change_channel(0).await;
        assert!(result.is_err());

        let result = transport.change_channel(15).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn t1108_scan_channels_success() {
        use sonde_protocol::modem::{ScanEntry, ScanResult};

        let (client, mut server) = duplex(1024);

        let startup = tokio::spawn(async move {
            do_startup_handshake(&mut server, 6).await;
            server
        });

        let transport = UsbEspNowTransport::new(client, 6).await.unwrap();
        let mut server = startup.await.unwrap();

        let scan = tokio::spawn(async move { transport.scan_channels().await });

        let mut decoder = FrameDecoder::new();
        let mut buf = [0u8; 256];
        let msg = read_next_message(&mut server, &mut decoder, &mut buf).await;
        assert!(
            matches!(msg, ModemMessage::ScanChannels),
            "expected ScanChannels, got {msg:?}"
        );

        let scan_result = ModemMessage::ScanResult(ScanResult {
            entries: vec![
                ScanEntry {
                    channel: 1,
                    ap_count: 3,
                    strongest_rssi: -40,
                },
                ScanEntry {
                    channel: 6,
                    ap_count: 0,
                    strongest_rssi: -127,
                },
                ScanEntry {
                    channel: 11,
                    ap_count: 1,
                    strongest_rssi: -65,
                },
            ],
        });
        server
            .write_all(&encode_modem_frame(&scan_result).unwrap())
            .await
            .unwrap();

        let result = scan.await.unwrap().unwrap();
        assert_eq!(result.entries.len(), 3);
        assert_eq!(result.entries[0].channel, 1);
        assert_eq!(result.entries[0].ap_count, 3);
        assert_eq!(result.entries[1].strongest_rssi, -127);
    }
}
