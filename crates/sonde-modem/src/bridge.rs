// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Bridge logic: connects a serial port to a radio driver.
//!
//! Decodes inbound serial frames from the gateway, dispatches commands to
//! the radio driver, and encodes outbound frames (RECV_FRAME, STATUS, etc.)
//! back to the gateway.
//!
//! The bridge is generic over `SerialPort` and `Radio` traits, allowing
//! the same logic to be tested on a host with mock implementations.

use log::{info, warn};
use std::sync::Arc;

use sonde_protocol::modem::{
    encode_modem_frame, BleConnected, BleDisconnected, BlePairingConfirm, BlePairingConfirmReply,
    BleRecv, FrameDecoder, ModemCodecError, ModemError, ModemMessage, ModemReady, ModemStatus,
    RecvFrame, ScanEntry, ScanResult, SendFrame, MAC_SIZE, MODEM_ERR_CHANNEL_SET_FAILED,
};

use crate::status::ModemCounters;

/// Firmware version: major.minor.patch.build (one byte each).
const FIRMWARE_VERSION: [u8; 4] = [0, 1, 0, 0];

/// Maximum number of received radio frames forwarded per `poll()` call.
/// Prevents starvation of serial decode and other poll() work under
/// sustained RX burst traffic.
const MAX_RX_FRAMES_PER_POLL: usize = 16;

/// Maximum number of BLE events forwarded per `poll()` call.
/// Prevents starvation of serial decode and radio under sustained BLE traffic.
const MAX_BLE_EVENTS_PER_POLL: usize = 16;

/// Abstraction over a serial byte stream (USB-CDC on device, PTY in tests).
pub trait SerialPort {
    /// Read available bytes. Returns `(bytes_read, reconnected)` where
    /// `reconnected` is true if connectivity was just re-established.
    fn read(&mut self, buf: &mut [u8]) -> (usize, bool);
    /// Write bytes to the serial port. Always attempts the write so
    /// critical messages (e.g., MODEM_READY) are never silently dropped.
    /// Returns true if the write succeeded.
    fn write(&mut self, data: &[u8]) -> bool;
    /// Returns true if the last I/O operation succeeded.
    fn is_connected(&self) -> bool;
}

/// Abstraction over the radio layer (ESP-NOW on device, mock in tests).
pub trait Radio {
    /// Send a frame to the given peer MAC.
    fn send(&mut self, peer_mac: &[u8; MAC_SIZE], data: &[u8]);
    /// Drain one received frame from the queue, or `None` if empty.
    fn drain_one(&self) -> Option<RecvFrame>;
    /// Set the radio channel. Returns a descriptive error on failure.
    fn set_channel(&mut self, channel: u8) -> Result<(), &'static str>;
    /// Get the current channel.
    fn channel(&self) -> u8;
    /// Perform a channel scan. Returns (channel, ap_count, strongest_rssi).
    fn scan_channels(&mut self) -> Vec<(u8, u8, i8)>;
    /// Get the device's own MAC address.
    fn mac_address(&self) -> [u8; MAC_SIZE];
    /// Reset radio state (clear peers, reset channel to 1, drain queues).
    fn reset_state(&mut self);
}

/// Events emitted by the BLE layer (phone → modem → gateway via USB).
pub enum BleEvent {
    /// A BLE GATT write was received from the connected phone.
    Recv(Vec<u8>),
    /// A BLE client connected and completed LESC pairing.
    Connected { peer_addr: [u8; MAC_SIZE], mtu: u16 },
    /// The BLE client disconnected.
    Disconnected {
        peer_addr: [u8; MAC_SIZE],
        reason: u8,
    },
    /// Numeric Comparison passkey to relay to the gateway (MD-0414).
    PairingConfirm { passkey: u32 },
}

/// Abstraction over the BLE GATT server layer (ESP-IDF on device, no-op in tests).
///
/// The bridge calls `enable`/`disable`/`indicate`/`pairing_confirm_reply` in
/// response to gateway commands, and calls `drain_event` each poll cycle to
/// forward BLE events to the gateway over USB-CDC.
pub trait Ble {
    /// Enable BLE advertising for the Gateway Pairing Service (MD-0407).
    fn enable(&mut self);
    /// Disable BLE advertising and disconnect any active BLE client (MD-0407).
    ///
    /// Implementations **must not** drop disconnect events here; suppression
    /// of stale events across RESET is handled by the bridge reset logic.
    /// Implementations must clear the indication queue so partial fragments
    /// are not sent to the next client.
    fn disable(&mut self);
    /// Send an indication to the connected BLE client (MD-0408).
    ///
    /// If no client is connected or `data` is empty, silently discards.
    fn indicate(&mut self, data: &[u8]);
    /// Accept or reject a Numeric Comparison pairing (MD-0414).
    fn pairing_confirm_reply(&mut self, accept: bool);
    /// Advance the indication queue by at most one chunk.
    ///
    /// Must be called once per poll cycle to pace fragmented indications.
    fn advance_indication(&self) {}
    /// Check whether the current BLE connection has exceeded the pairing
    /// timeout (MD-0414 AC#4).  If so, force-disconnect the client.
    ///
    /// Must be called once per poll cycle.
    fn check_pairing_timeout(&self) {}
    /// Drain one queued BLE event, or `None` if empty.
    fn drain_event(&self) -> Option<BleEvent>;
}

/// No-op BLE driver for builds without BLE support (host-side testing).
pub struct NoBle;

impl Ble for NoBle {
    fn enable(&mut self) {}
    fn disable(&mut self) {}
    fn indicate(&mut self, _data: &[u8]) {}
    fn pairing_confirm_reply(&mut self, _accept: bool) {}
    fn drain_event(&self) -> Option<BleEvent> {
        None
    }
}

/// Bridge between a serial port, a radio driver, and an optional BLE driver.
pub struct Bridge<S: SerialPort, R: Radio, B: Ble = NoBle> {
    usb: S,
    radio: R,
    ble: B,
    counters: Arc<ModemCounters>,
    decoder: FrameDecoder,
    rx_buf: [u8; 64],
}

impl<S: SerialPort, R: Radio> Bridge<S, R, NoBle> {
    /// Create a bridge without BLE support (no-op BLE driver).
    pub fn new(usb: S, radio: R, counters: Arc<ModemCounters>) -> Self {
        Self {
            usb,
            radio,
            ble: NoBle,
            counters,
            decoder: FrameDecoder::new(),
            rx_buf: [0u8; 64],
        }
    }
}

impl<S: SerialPort, R: Radio, B: Ble> Bridge<S, R, B> {
    /// Create a bridge with a BLE driver.
    pub fn with_ble(usb: S, radio: R, ble: B, counters: Arc<ModemCounters>) -> Self {
        Self {
            usb,
            radio,
            ble,
            counters,
            decoder: FrameDecoder::new(),
            rx_buf: [0u8; 64],
        }
    }

    /// Encode and write a modem message to the serial port. Returns true
    /// if the write succeeded.
    fn send_msg(&mut self, msg: &ModemMessage) -> bool {
        match encode_modem_frame(msg) {
            Ok(frame) => self.usb.write(&frame),
            Err(e) => {
                warn!("encode error: {}", e);
                false
            }
        }
    }

    /// Send MODEM_READY to the gateway.
    pub fn send_modem_ready(&mut self) {
        let mac = self.radio.mac_address();
        let msg = ModemMessage::ModemReady(ModemReady {
            firmware_version: FIRMWARE_VERSION,
            mac_address: mac,
        });
        self.send_msg(&msg);
        info!(
            "sent MODEM_READY (fw={}.{}.{}.{}, commit={}, mac={:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X})",
            FIRMWARE_VERSION[0],
            FIRMWARE_VERSION[1],
            FIRMWARE_VERSION[2],
            FIRMWARE_VERSION[3],
            env!("SONDE_GIT_COMMIT"),
            mac[0],
            mac[1],
            mac[2],
            mac[3],
            mac[4],
            mac[5],
        );
    }

    /// Returns true if the USB serial port is connected.
    pub fn is_usb_connected(&self) -> bool {
        self.usb.is_connected()
    }

    /// Poll for serial data and radio received frames.
    pub fn poll(&mut self) {
        let (n, reconnected) = self.usb.read(&mut self.rx_buf);
        if reconnected {
            info!("USB reconnected, sending MODEM_READY");
            self.decoder.reset();
            self.send_modem_ready();
        }
        if n > 0 {
            self.decoder.push(&self.rx_buf[..n]);
        }

        // Decode and dispatch serial frames.
        loop {
            match self.decoder.decode() {
                Ok(Some(msg)) => self.dispatch(msg),
                Ok(None) => break,
                Err(ModemCodecError::EmptyFrame) => continue,
                Err(ModemCodecError::FrameTooLarge(len)) => {
                    warn!("framing error: len={}, resetting decoder", len);
                    // Clear the decoder buffer so subsequent bytes
                    // (including a RESET command from the gateway) can be
                    // parsed. We do NOT send MODEM_READY here — that is
                    // only sent in response to a RESET command (§2.3).
                    self.decoder.reset();
                    break;
                }
                Err(_) => {
                    // Malformed body (too short, too long, etc.) — the frame
                    // has already been consumed from the buffer by decode().
                    // Silently discard and continue to the next frame.
                    continue;
                }
            }
        }

        // Forward any received radio frames to the serial port, one at a time
        // to avoid a transient heap spike from bulk-draining the queue.
        // Cap at 16 frames per poll to prevent starvation of serial decode
        // and other poll() work under sustained RX load.
        for _ in 0..MAX_RX_FRAMES_PER_POLL {
            match self.radio.drain_one() {
                Some(rf) => {
                    let msg = ModemMessage::RecvFrame(rf);
                    if self.send_msg(&msg) {
                        self.counters.inc_rx();
                    }
                }
                None => break,
            }
        }

        // Advance fragmented BLE indications (one chunk per poll cycle).
        self.ble.advance_indication();

        // Enforce BLE pairing timeout (MD-0414 AC#4).
        self.ble.check_pairing_timeout();

        // Forward any BLE events to the gateway over USB-CDC.
        // Cap at MAX_BLE_EVENTS_PER_POLL to prevent starvation of serial
        // decode and radio under sustained BLE write traffic.
        for _ in 0..MAX_BLE_EVENTS_PER_POLL {
            match self.ble.drain_event() {
                Some(BleEvent::Recv(data)) => {
                    let msg = ModemMessage::BleRecv(BleRecv { ble_data: data });
                    self.send_msg(&msg);
                }
                Some(BleEvent::Connected { peer_addr, mtu }) => {
                    let msg = ModemMessage::BleConnected(BleConnected { peer_addr, mtu });
                    self.send_msg(&msg);
                }
                Some(BleEvent::Disconnected { peer_addr, reason }) => {
                    let msg = ModemMessage::BleDisconnected(BleDisconnected { peer_addr, reason });
                    self.send_msg(&msg);
                }
                Some(BleEvent::PairingConfirm { passkey }) => {
                    let msg = ModemMessage::BlePairingConfirm(BlePairingConfirm { passkey });
                    self.send_msg(&msg);
                }
                None => break,
            }
        }
    }

    fn dispatch(&mut self, msg: ModemMessage) {
        match msg {
            ModemMessage::Reset => self.handle_reset(),
            ModemMessage::SendFrame(sf) => self.handle_send_frame(sf),
            ModemMessage::SetChannel(ch) => self.handle_set_channel(ch),
            ModemMessage::GetStatus => self.handle_get_status(),
            ModemMessage::ScanChannels => self.handle_scan_channels(),
            ModemMessage::BleIndicate(ind) => self.handle_ble_indicate(ind.ble_data),
            ModemMessage::BleEnable => self.handle_ble_enable(),
            ModemMessage::BleDisable => self.handle_ble_disable(),
            ModemMessage::BlePairingConfirmReply(reply) => {
                self.handle_ble_pairing_confirm_reply(reply)
            }
            ModemMessage::Unknown { .. } => {}
            _ => {}
        }
    }

    fn handle_reset(&mut self) {
        info!("RESET received");
        self.radio.reset_state();
        // BLE advertising is off by default after RESET (MD-0412).
        self.ble.disable();
        // Drain any stale BLE events (including BLE_DISCONNECTED from the
        // disable call above) so they do not leak into the next session.
        // Hard upper bound prevents spinning if a BLE implementation
        // erroneously returns events indefinitely.
        const MAX_DRAIN: usize = 256;
        let mut drained = 0;
        while self.ble.drain_event().is_some() {
            drained += 1;
            if drained >= MAX_DRAIN {
                warn!("RESET: drained {drained} BLE events (limit reached)");
                break;
            }
        }
        self.counters.reset();
        self.decoder.reset();
        self.send_modem_ready();
    }

    fn handle_send_frame(&mut self, sf: SendFrame) {
        self.radio.send(&sf.peer_mac, &sf.frame_data);
    }

    fn handle_set_channel(&mut self, channel: u8) {
        match self.radio.set_channel(channel) {
            Ok(()) => {
                let ack = ModemMessage::SetChannelAck(channel);
                self.send_msg(&ack);
            }
            Err(reason) => {
                let err = ModemMessage::Error(ModemError {
                    error_code: MODEM_ERR_CHANNEL_SET_FAILED,
                    message: reason.as_bytes().to_vec(),
                });
                self.send_msg(&err);
            }
        }
    }

    fn handle_get_status(&mut self) {
        let status = ModemMessage::Status(ModemStatus {
            channel: self.radio.channel(),
            uptime_s: self.counters.uptime_s(),
            tx_count: self.counters.tx_count(),
            rx_count: self.counters.rx_count(),
            tx_fail_count: self.counters.tx_fail_count(),
        });
        self.send_msg(&status);
    }

    fn handle_scan_channels(&mut self) {
        let results = self.radio.scan_channels();
        let entries: Vec<ScanEntry> = results
            .into_iter()
            .map(|(ch, count, rssi)| ScanEntry {
                channel: ch,
                ap_count: count,
                strongest_rssi: rssi,
            })
            .collect();
        let msg = ModemMessage::ScanResult(ScanResult { entries });
        self.send_msg(&msg);
    }

    fn handle_ble_indicate(&mut self, data: Vec<u8>) {
        self.ble.indicate(&data);
    }

    fn handle_ble_enable(&mut self) {
        info!("BLE_ENABLE received");
        self.ble.enable();
    }

    fn handle_ble_disable(&mut self) {
        info!("BLE_DISABLE received");
        self.ble.disable();
    }

    fn handle_ble_pairing_confirm_reply(&mut self, reply: BlePairingConfirmReply) {
        self.ble.pairing_confirm_reply(reply.accept);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sonde_protocol::modem::{
        decode_modem_frame, BleIndicate, ModemMessage, SERIAL_MAX_FRAME_SIZE, SERIAL_MAX_LEN,
    };
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;

    /// Mock serial port that records writes and plays back reads.
    struct MockSerial {
        rx_data: Vec<u8>,
        tx_data: Vec<u8>,
        connected: bool,
        reconnect_once: bool,
    }

    impl MockSerial {
        fn new() -> Self {
            Self {
                rx_data: Vec::new(),
                tx_data: Vec::new(),
                connected: true,
                reconnect_once: false,
            }
        }

        fn inject(&mut self, data: &[u8]) {
            self.rx_data.extend_from_slice(data);
        }

        fn take_tx(&mut self) -> Vec<u8> {
            std::mem::take(&mut self.tx_data)
        }

        fn set_reconnect_once(&mut self) {
            self.reconnect_once = true;
        }

        fn set_connected(&mut self, connected: bool) {
            self.connected = connected;
        }
    }

    impl SerialPort for MockSerial {
        fn read(&mut self, buf: &mut [u8]) -> (usize, bool) {
            let reconnected = self.reconnect_once;
            self.reconnect_once = false;
            let n = std::cmp::min(buf.len(), self.rx_data.len());
            buf[..n].copy_from_slice(&self.rx_data[..n]);
            self.rx_data.drain(..n);
            (n, reconnected)
        }
        fn write(&mut self, data: &[u8]) -> bool {
            if !self.connected {
                return false;
            }
            self.tx_data.extend_from_slice(data);
            true
        }
        fn is_connected(&self) -> bool {
            self.connected
        }
    }

    /// Mock radio that captures sends and injects receives.
    struct MockRadio {
        sent: Vec<(Vec<u8>, [u8; MAC_SIZE])>,
        rx_queue: RefCell<VecDeque<RecvFrame>>,
        channel: u8,
        mac: [u8; MAC_SIZE],
        /// Tracks registered peers (mirrors real ESP-NOW peer table).
        peers: Vec<[u8; MAC_SIZE]>,
    }

    impl MockRadio {
        fn new() -> Self {
            Self {
                sent: Vec::new(),
                rx_queue: RefCell::new(VecDeque::new()),
                channel: 1,
                mac: [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF],
                peers: Vec::new(),
            }
        }

        fn inject_rx(&self, frame: RecvFrame) {
            self.rx_queue.borrow_mut().push_back(frame);
        }

        fn peer_count(&self) -> usize {
            self.peers.len()
        }
    }

    impl Radio for MockRadio {
        fn send(&mut self, peer_mac: &[u8; MAC_SIZE], data: &[u8]) {
            if !self.peers.contains(peer_mac) {
                self.peers.push(*peer_mac);
            }
            self.sent.push((data.to_vec(), *peer_mac));
        }
        fn drain_one(&self) -> Option<RecvFrame> {
            self.rx_queue.borrow_mut().pop_front()
        }
        fn set_channel(&mut self, channel: u8) -> Result<(), &'static str> {
            if channel == 0 || channel > 14 {
                return Err("invalid channel");
            }
            self.channel = channel;
            self.peers.clear();
            Ok(())
        }
        fn channel(&self) -> u8 {
            self.channel
        }
        fn scan_channels(&mut self) -> Vec<(u8, u8, i8)> {
            (1..=14).map(|ch| (ch, 0, 0)).collect()
        }
        fn mac_address(&self) -> [u8; MAC_SIZE] {
            self.mac
        }
        fn reset_state(&mut self) {
            self.channel = 1;
            self.sent.clear();
            self.rx_queue.borrow_mut().clear();
            self.peers.clear();
        }
    }

    fn make_bridge() -> Bridge<MockSerial, MockRadio> {
        Bridge::new(MockSerial::new(), MockRadio::new(), ModemCounters::new())
    }

    #[test]
    fn modem_ready_on_boot() {
        let mut bridge = make_bridge();
        bridge.send_modem_ready();
        let tx = bridge.usb.take_tx();
        let (msg, _) = decode_modem_frame(&tx).unwrap();
        match msg {
            ModemMessage::ModemReady(mr) => {
                assert_eq!(mr.firmware_version, FIRMWARE_VERSION);
                assert_eq!(mr.mac_address, [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
            }
            _ => panic!("expected ModemReady"),
        }
    }

    #[test]
    fn reset_sends_modem_ready() {
        let mut bridge = make_bridge();
        let reset_frame = encode_modem_frame(&ModemMessage::Reset).unwrap();
        bridge.usb.inject(&reset_frame);
        bridge.poll();
        let tx = bridge.usb.take_tx();
        let (msg, _) = decode_modem_frame(&tx).unwrap();
        assert!(matches!(msg, ModemMessage::ModemReady(_)));
    }

    #[test]
    fn send_frame_dispatched() {
        let mut bridge = make_bridge();
        let peer = [1, 2, 3, 4, 5, 6];
        let sf = ModemMessage::SendFrame(SendFrame {
            peer_mac: peer,
            frame_data: vec![0xDE, 0xAD],
        });
        let frame = encode_modem_frame(&sf).unwrap();
        bridge.usb.inject(&frame);
        bridge.poll();
        assert_eq!(bridge.radio.sent.len(), 1);
        assert_eq!(bridge.radio.sent[0].0, vec![0xDE, 0xAD]);
        assert_eq!(bridge.radio.sent[0].1, peer);
    }

    #[test]
    fn set_channel_ack() {
        let mut bridge = make_bridge();
        let frame = encode_modem_frame(&ModemMessage::SetChannel(6)).unwrap();
        bridge.usb.inject(&frame);
        bridge.poll();
        let tx = bridge.usb.take_tx();
        let (msg, _) = decode_modem_frame(&tx).unwrap();
        assert_eq!(msg, ModemMessage::SetChannelAck(6));
        assert_eq!(bridge.radio.channel(), 6);
    }

    #[test]
    fn set_channel_invalid_returns_error() {
        let mut bridge = make_bridge();
        let frame = encode_modem_frame(&ModemMessage::SetChannel(0)).unwrap();
        bridge.usb.inject(&frame);
        bridge.poll();
        let tx = bridge.usb.take_tx();
        let (msg, _) = decode_modem_frame(&tx).unwrap();
        match msg {
            ModemMessage::Error(e) => {
                assert_eq!(e.error_code, MODEM_ERR_CHANNEL_SET_FAILED);
            }
            _ => panic!("expected Error"),
        }
    }

    #[test]
    fn get_status_response() {
        let mut bridge = make_bridge();
        let frame = encode_modem_frame(&ModemMessage::GetStatus).unwrap();
        bridge.usb.inject(&frame);
        bridge.poll();
        let tx = bridge.usb.take_tx();
        let (msg, _) = decode_modem_frame(&tx).unwrap();
        match msg {
            ModemMessage::Status(s) => {
                assert_eq!(s.channel, 1);
                assert_eq!(s.tx_count, 0);
                assert_eq!(s.rx_count, 0);
                assert_eq!(s.tx_fail_count, 0);
            }
            _ => panic!("expected Status"),
        }
    }

    #[test]
    fn unknown_type_silently_discarded() {
        let mut bridge = make_bridge();
        // Send an unknown type, then GET_STATUS to verify bridge still works.
        let unknown = encode_modem_frame(&ModemMessage::Unknown {
            msg_type: 0x7F,
            body: vec![1, 2, 3],
        })
        .unwrap();
        let status = encode_modem_frame(&ModemMessage::GetStatus).unwrap();
        bridge.usb.inject(&unknown);
        bridge.usb.inject(&status);
        bridge.poll();
        let tx = bridge.usb.take_tx();
        let (msg, _) = decode_modem_frame(&tx).unwrap();
        assert!(matches!(msg, ModemMessage::Status(_)));
    }

    #[test]
    fn reset_clears_counters() {
        let mut bridge = make_bridge();
        bridge.counters.inc_tx();
        bridge.counters.inc_tx();
        bridge.counters.inc_rx();
        assert_eq!(bridge.counters.tx_count(), 2);

        let reset = encode_modem_frame(&ModemMessage::Reset).unwrap();
        bridge.usb.inject(&reset);
        bridge.poll();
        assert_eq!(bridge.counters.tx_count(), 0);
        assert_eq!(bridge.counters.rx_count(), 0);
    }

    #[test]
    fn scan_channels_response() {
        let mut bridge = make_bridge();
        let frame = encode_modem_frame(&ModemMessage::ScanChannels).unwrap();
        bridge.usb.inject(&frame);
        bridge.poll();
        let tx = bridge.usb.take_tx();
        let (msg, _) = decode_modem_frame(&tx).unwrap();
        match msg {
            ModemMessage::ScanResult(sr) => {
                assert_eq!(sr.entries.len(), 14);
            }
            _ => panic!("expected ScanResult"),
        }
    }

    // --- Radio → USB forwarding tests ---

    /// Validates: T-0200 (radio → USB forwarding)
    /// Received radio frames are forwarded as RECV_FRAME on serial.
    #[test]
    fn recv_frame_forwarded_to_serial() {
        let mut bridge = make_bridge();
        let peer = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66];
        bridge.radio.inject_rx(RecvFrame {
            peer_mac: peer,
            rssi: -42,
            frame_data: vec![0xCA, 0xFE],
        });
        bridge.poll();
        let tx = bridge.usb.take_tx();
        let (msg, _) = decode_modem_frame(&tx).unwrap();
        match msg {
            ModemMessage::RecvFrame(rf) => {
                assert_eq!(rf.peer_mac, peer);
                assert_eq!(rf.rssi, -42);
                assert_eq!(rf.frame_data, vec![0xCA, 0xFE]);
            }
            _ => panic!("expected RecvFrame"),
        }
    }

    /// Validates: T-0302 (rx_count incremented on forwarded frames)
    #[test]
    fn rx_count_incremented_on_forwarded_frame() {
        let mut bridge = make_bridge();
        let peer = [1, 2, 3, 4, 5, 6];
        bridge.radio.inject_rx(RecvFrame {
            peer_mac: peer,
            rssi: -50,
            frame_data: vec![0x01],
        });
        bridge.radio.inject_rx(RecvFrame {
            peer_mac: peer,
            rssi: -55,
            frame_data: vec![0x02],
        });
        bridge.radio.inject_rx(RecvFrame {
            peer_mac: peer,
            rssi: -60,
            frame_data: vec![0x03],
        });
        bridge.poll();
        assert_eq!(bridge.counters.rx_count(), 3);
    }

    /// Validates: T-0302 (status counter accuracy — tx and rx through bridge)
    /// Note: tx_count is incremented by the ESP-NOW driver (not by bridge),
    /// so it stays 0 here. Bridge only increments rx_count on forwarded frames.
    #[test]
    fn status_reflects_tx_and_rx_counts() {
        let mut bridge = make_bridge();
        let peer = [1, 2, 3, 4, 5, 6];

        // Send 5 frames USB → radio.
        for i in 0..5 {
            let sf = ModemMessage::SendFrame(SendFrame {
                peer_mac: peer,
                frame_data: vec![i],
            });
            let frame = encode_modem_frame(&sf).unwrap();
            bridge.usb.inject(&frame);
        }
        bridge.poll();
        bridge.usb.take_tx(); // discard

        // Inject 3 frames radio → USB.
        for i in 0..3 {
            bridge.radio.inject_rx(RecvFrame {
                peer_mac: peer,
                rssi: -40,
                frame_data: vec![i],
            });
        }
        bridge.poll();
        bridge.usb.take_tx(); // discard RECV_FRAMEs

        // Query status.
        let frame = encode_modem_frame(&ModemMessage::GetStatus).unwrap();
        bridge.usb.inject(&frame);
        bridge.poll();
        let tx = bridge.usb.take_tx();
        let (msg, _) = decode_modem_frame(&tx).unwrap();
        match msg {
            ModemMessage::Status(s) => {
                // tx_count is incremented by the radio driver, not bridge.
                assert_eq!(s.tx_count, 0);
                assert_eq!(s.rx_count, 3);
            }
            _ => panic!("expected Status"),
        }
    }

    /// Validates: T-0204 (multiple radio frames forwarded in order)
    #[test]
    fn multiple_recv_frames_forwarded_in_order() {
        let mut bridge = make_bridge();
        let peer = [1, 2, 3, 4, 5, 6];
        for i in 0u8..5 {
            bridge.radio.inject_rx(RecvFrame {
                peer_mac: peer,
                rssi: -(i as i8) - 30,
                frame_data: vec![i],
            });
        }
        bridge.poll();
        let tx = bridge.usb.take_tx();

        // Decode all frames and check they arrive in order.
        let mut offset = 0;
        for i in 0u8..5 {
            let (msg, consumed) = decode_modem_frame(&tx[offset..]).unwrap();
            offset += consumed;
            match msg {
                ModemMessage::RecvFrame(rf) => {
                    assert_eq!(rf.frame_data, vec![i], "frame {} out of order", i);
                }
                _ => panic!("expected RecvFrame at position {}", i),
            }
        }
    }

    // --- Validation scenario tests ---

    /// Validates: T-0300 (RESET clears state — including channel)
    #[test]
    fn reset_clears_channel_to_default() {
        let mut bridge = make_bridge();
        // Set channel to 11.
        let frame = encode_modem_frame(&ModemMessage::SetChannel(11)).unwrap();
        bridge.usb.inject(&frame);
        bridge.poll();
        bridge.usb.take_tx(); // discard ACK
        assert_eq!(bridge.radio.channel(), 11);

        // RESET should revert to channel 1.
        let reset = encode_modem_frame(&ModemMessage::Reset).unwrap();
        bridge.usb.inject(&reset);
        bridge.poll();
        bridge.usb.take_tx(); // discard MODEM_READY
        assert_eq!(bridge.radio.channel(), 1);

        // Verify via GET_STATUS.
        let frame = encode_modem_frame(&ModemMessage::GetStatus).unwrap();
        bridge.usb.inject(&frame);
        bridge.poll();
        let tx = bridge.usb.take_tx();
        let (msg, _) = decode_modem_frame(&tx).unwrap();
        match msg {
            ModemMessage::Status(s) => {
                assert_eq!(s.channel, 1);
                assert_eq!(s.tx_count, 0);
                assert_eq!(s.rx_count, 0);
                assert_eq!(s.tx_fail_count, 0);
            }
            _ => panic!("expected Status"),
        }
    }

    /// Validates: T-0303 (repeated RESET → MODEM_READY)
    #[test]
    fn repeated_reset_sends_modem_ready_each_time() {
        let mut bridge = make_bridge();
        for _ in 0..5 {
            let reset = encode_modem_frame(&ModemMessage::Reset).unwrap();
            bridge.usb.inject(&reset);
            bridge.poll();
            let tx = bridge.usb.take_tx();
            let (msg, _) = decode_modem_frame(&tx).unwrap();
            assert!(matches!(msg, ModemMessage::ModemReady(_)));
        }
    }

    /// Validates: T-0400 (SEND_FRAME with body too short is silently discarded)
    #[test]
    fn send_frame_body_too_short_discarded() {
        let mut bridge = make_bridge();
        // Manually craft a frame with SEND_FRAME type but only 3 bytes of body
        // (less than the 7-byte minimum: 6B MAC + 1B data).
        let msg_type: u8 = 0x02; // SEND_FRAME
        let body: [u8; 3] = [0x01, 0x02, 0x03];
        let len = 1 + body.len(); // type + body
        let mut raw = Vec::new();
        raw.push((len >> 8) as u8);
        raw.push(len as u8);
        raw.push(msg_type);
        raw.extend_from_slice(&body);
        bridge.usb.inject(&raw);
        bridge.poll();

        // Bridge should still be operational — no crash, no send.
        assert_eq!(bridge.radio.sent.len(), 0);

        // Verify bridge still works.
        let frame = encode_modem_frame(&ModemMessage::GetStatus).unwrap();
        bridge.usb.inject(&frame);
        bridge.poll();
        let tx = bridge.usb.take_tx();
        let (msg, _) = decode_modem_frame(&tx).unwrap();
        assert!(matches!(msg, ModemMessage::Status(_)));
    }

    /// Validates: T-0402 (framing error recovery — corrupt bytes → RESET → MODEM_READY)
    #[test]
    fn framing_error_recovery_via_reset() {
        let mut bridge = make_bridge();

        // Inject corrupt data: a length prefix claiming 600 bytes (> SERIAL_MAX_LEN).
        // This triggers FrameTooLarge and decoder reset.
        let corrupt: [u8; 4] = [0x02, 0x58, 0xFF, 0xFF]; // len=600
        bridge.usb.inject(&corrupt);
        bridge.poll();
        let tx = bridge.usb.take_tx();
        assert!(
            tx.is_empty(),
            "modem must not emit output on framing errors"
        );

        // Now send a proper RESET — the decoder should recover.
        let reset = encode_modem_frame(&ModemMessage::Reset).unwrap();
        bridge.usb.inject(&reset);
        bridge.poll();
        let tx = bridge.usb.take_tx();
        let (msg, _) = decode_modem_frame(&tx).unwrap();
        assert!(matches!(msg, ModemMessage::ModemReady(_)));
    }

    /// Validates: T-0500 (modem does not interpret frame contents)
    #[test]
    fn modem_forwards_opaque_payload() {
        let mut bridge = make_bridge();
        let peer = [1, 2, 3, 4, 5, 6];
        // Invalid CBOR — modem should not inspect it.
        let garbage_payload = vec![0xFF, 0xFF, 0xFF, 0x00, 0xFE];
        let sf = ModemMessage::SendFrame(SendFrame {
            peer_mac: peer,
            frame_data: garbage_payload.clone(),
        });
        let frame = encode_modem_frame(&sf).unwrap();
        bridge.usb.inject(&frame);
        bridge.poll();

        // Radio should receive exact bytes — no parsing or rejection.
        assert_eq!(bridge.radio.sent.len(), 1);
        assert_eq!(bridge.radio.sent[0].0, garbage_payload);

        // No ERROR message on serial output.
        let tx = bridge.usb.take_tx();
        assert!(
            tx.is_empty(),
            "modem should not send any response for SEND_FRAME"
        );
    }

    /// Validates: T-0301 (USB reconnection triggers MODEM_READY)
    #[test]
    fn usb_reconnect_triggers_modem_ready() {
        let mut bridge = make_bridge();
        // Simulate a reconnection event on next read.
        bridge.usb.set_reconnect_once();
        bridge.poll();
        let tx = bridge.usb.take_tx();
        let (msg, _) = decode_modem_frame(&tx).unwrap();
        assert!(matches!(msg, ModemMessage::ModemReady(_)));
    }

    /// Validates that USB reconnect clears stale partial frame data from
    /// the decoder.  Without `decoder.reset()`, leftover bytes from before
    /// the disconnect would corrupt the first post-reconnect frame.
    #[test]
    fn usb_reconnect_clears_decoder_state() {
        let mut bridge = make_bridge();

        // Inject stale bytes to leave a partial length-prefixed frame in the
        // decoder buffer.  These do not form a complete frame.
        bridge.usb.inject(&[0x01, 0x02, 0x03, 0xFF, 0xFE]);
        bridge.poll();
        bridge.usb.take_tx(); // discard any error output

        // Simulate USB reconnect — this should reset the decoder and send
        // MODEM_READY.
        bridge.usb.set_reconnect_once();
        bridge.poll();
        let tx = bridge.usb.take_tx();
        let (msg, _) = decode_modem_frame(&tx).unwrap();
        assert!(matches!(msg, ModemMessage::ModemReady(_)));

        // Now inject a complete GET_STATUS frame.  If the decoder was NOT
        // reset, the stale bytes would corrupt this frame.
        let status_frame = encode_modem_frame(&ModemMessage::GetStatus).unwrap();
        bridge.usb.inject(&status_frame);
        bridge.poll();
        let tx = bridge.usb.take_tx();
        let (msg, _) = decode_modem_frame(&tx).unwrap();
        assert!(
            matches!(msg, ModemMessage::Status(_)),
            "expected Status after reconnect, got {:?}",
            msg
        );
    }

    /// RX cap: poll() forwards at most MAX_RX_FRAMES_PER_POLL frames per call.
    /// Remaining frames are forwarded in subsequent poll() calls.
    #[test]
    fn rx_cap_limits_frames_per_poll() {
        let mut bridge = make_bridge();
        let peer = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66];
        // Inject more frames than the per-poll cap.
        let total = MAX_RX_FRAMES_PER_POLL + 5;
        for i in 0..total {
            bridge.radio.inject_rx(RecvFrame {
                peer_mac: peer,
                rssi: -40,
                frame_data: vec![i as u8],
            });
        }

        // First poll should forward exactly MAX_RX_FRAMES_PER_POLL.
        bridge.poll();
        let tx1 = bridge.usb.take_tx();
        let mut count1 = 0;
        let mut decoder = FrameDecoder::new();
        decoder.push(&tx1);
        while let Ok(Some(msg)) = decoder.decode() {
            if matches!(msg, ModemMessage::RecvFrame(_)) {
                count1 += 1;
            }
        }
        assert_eq!(count1, MAX_RX_FRAMES_PER_POLL);

        // Second poll should forward the remaining 5.
        bridge.poll();
        let tx2 = bridge.usb.take_tx();
        let mut count2 = 0;
        decoder = FrameDecoder::new();
        decoder.push(&tx2);
        while let Ok(Some(msg)) = decoder.decode() {
            if matches!(msg, ModemMessage::RecvFrame(_)) {
                count2 += 1;
            }
        }
        assert_eq!(count2, 5);

        // Total forwarded must equal total injected.
        assert_eq!(count1 + count2, total);
    }

    // --- BLE relay tests ---

    /// Mock BLE driver that records calls and queues events.
    struct MockBle {
        enabled: bool,
        indicated: Vec<Vec<u8>>,
        pairing_replies: Vec<bool>,
        event_queue: RefCell<VecDeque<BleEvent>>,
        check_pairing_timeout_count: Cell<usize>,
        enable_count: Cell<usize>,
        disable_count: Cell<usize>,
    }

    impl MockBle {
        fn new() -> Self {
            Self {
                enabled: false,
                indicated: Vec::new(),
                pairing_replies: Vec::new(),
                event_queue: RefCell::new(VecDeque::new()),
                check_pairing_timeout_count: Cell::new(0),
                enable_count: Cell::new(0),
                disable_count: Cell::new(0),
            }
        }

        fn inject_event(&self, event: BleEvent) {
            self.event_queue.borrow_mut().push_back(event);
        }
    }

    impl Ble for MockBle {
        fn enable(&mut self) {
            self.enabled = true;
            self.enable_count.set(self.enable_count.get() + 1);
        }
        fn disable(&mut self) {
            self.enabled = false;
            // Ble::disable() contract: clear indications so partial fragments
            // are not sent to the next client. Events are NOT cleared here —
            // the bridge reset logic drains stale events (MD-0412).
            self.indicated.clear();
            self.disable_count.set(self.disable_count.get() + 1);
        }
        fn indicate(&mut self, data: &[u8]) {
            // Ble contract: empty data is a no-op.
            if !data.is_empty() {
                self.indicated.push(data.to_vec());
            }
        }
        fn pairing_confirm_reply(&mut self, accept: bool) {
            self.pairing_replies.push(accept);
        }
        fn check_pairing_timeout(&self) {
            self.check_pairing_timeout_count
                .set(self.check_pairing_timeout_count.get() + 1);
        }
        fn drain_event(&self) -> Option<BleEvent> {
            self.event_queue.borrow_mut().pop_front()
        }
    }

    fn make_bridge_with_ble() -> Bridge<MockSerial, MockRadio, MockBle> {
        Bridge::with_ble(
            MockSerial::new(),
            MockRadio::new(),
            MockBle::new(),
            ModemCounters::new(),
        )
    }

    /// Validates: T-0618 (BLE_ENABLE starts advertising)
    #[test]
    fn ble_enable_starts_advertising() {
        let mut bridge = make_bridge_with_ble();
        assert!(!bridge.ble.enabled);
        let frame = encode_modem_frame(&ModemMessage::BleEnable).unwrap();
        bridge.usb.inject(&frame);
        bridge.poll();
        assert!(bridge.ble.enabled);
    }

    /// Validates: T-0617 (BLE disabled by default after RESET / MD-0412)
    #[test]
    fn ble_disabled_after_reset() {
        let mut bridge = make_bridge_with_ble();
        // Enable BLE first.
        let enable = encode_modem_frame(&ModemMessage::BleEnable).unwrap();
        bridge.usb.inject(&enable);
        bridge.poll();
        assert!(bridge.ble.enabled);
        bridge.usb.take_tx(); // discard

        // RESET must disable BLE.
        let reset = encode_modem_frame(&ModemMessage::Reset).unwrap();
        bridge.usb.inject(&reset);
        bridge.poll();
        assert!(!bridge.ble.enabled, "BLE must be off after RESET (MD-0412)");
    }

    /// Validates: T-0619 (BLE_DISABLE stops advertising)
    #[test]
    fn ble_disable_stops_advertising() {
        let mut bridge = make_bridge_with_ble();
        let enable = encode_modem_frame(&ModemMessage::BleEnable).unwrap();
        bridge.usb.inject(&enable);
        bridge.poll();
        assert!(bridge.ble.enabled);

        let disable = encode_modem_frame(&ModemMessage::BleDisable).unwrap();
        bridge.usb.inject(&disable);
        bridge.poll();
        assert!(!bridge.ble.enabled);
    }

    /// Validates: T-0604 / T-0611 (BLE_INDICATE dispatched to BLE layer)
    #[test]
    fn ble_indicate_dispatched() {
        let mut bridge = make_bridge_with_ble();
        let payload = vec![0x01, 0x00, 0x02, 0xDE, 0xAD];
        let frame = encode_modem_frame(&ModemMessage::BleIndicate(BleIndicate {
            ble_data: payload.clone(),
        }))
        .unwrap();
        bridge.usb.inject(&frame);
        bridge.poll();
        assert_eq!(bridge.ble.indicated.len(), 1);
        assert_eq!(bridge.ble.indicated[0], payload);
    }

    /// Validates: T-0613a (empty BLE_INDICATE silently discarded — MD-0408)
    ///
    /// An empty BLE_INDICATE body is invalid per modem-protocol.md §4.9.
    /// The decoder will reject it as BodyTooShort, so it never reaches
    /// handle_ble_indicate(). Verify the BLE layer receives nothing.
    #[test]
    fn ble_indicate_empty_body_silently_discarded() {
        let mut bridge = make_bridge_with_ble();
        // Craft a BLE_INDICATE frame with empty body (len = 1, just the type byte).
        let mut raw = Vec::new();
        raw.extend_from_slice(&1u16.to_be_bytes());
        raw.push(sonde_protocol::modem::MODEM_MSG_BLE_INDICATE);
        bridge.usb.inject(&raw);
        bridge.poll();
        // Nothing should have been indicated and no serial output.
        assert!(bridge.ble.indicated.is_empty());
        assert!(bridge.usb.take_tx().is_empty());
    }

    /// Validates: handle_ble_indicate gracefully handles empty data even if
    /// called directly (defense-in-depth, per the Ble trait contract).
    #[test]
    fn ble_indicate_direct_empty_no_panic() {
        let mut bridge = make_bridge_with_ble();
        // Directly call the dispatch path with BleIndicate(vec![]) to verify
        // the Ble::indicate() contract — empty data is a no-op without panicking.
        bridge.ble.indicate(&[]);
        assert!(
            bridge.ble.indicated.is_empty(),
            "empty indicate should not queue"
        );
    }

    /// Validates: T-0612 (BLE_INDICATE with no BLE client: silent discard via NoBle)
    #[test]
    fn ble_indicate_no_ble_client_silent_discard() {
        // NoBle no-op: no crash, no serial output.
        let mut bridge = make_bridge();
        let payload = vec![0x01, 0x00, 0x02, 0xDE, 0xAD];
        let frame = encode_modem_frame(&ModemMessage::BleIndicate(BleIndicate {
            ble_data: payload,
        }))
        .unwrap();
        bridge.usb.inject(&frame);
        bridge.poll();
        // No crash; serial output is empty (NoBle.indicate() is a no-op).
        assert!(bridge.usb.take_tx().is_empty());
    }

    /// Validates: T-0620 (BLE_PAIRING_CONFIRM_REPLY accept dispatched)
    #[test]
    fn ble_pairing_confirm_reply_accept() {
        let mut bridge = make_bridge_with_ble();
        let reply = ModemMessage::BlePairingConfirmReply(BlePairingConfirmReply { accept: true });
        let frame = encode_modem_frame(&reply).unwrap();
        bridge.usb.inject(&frame);
        bridge.poll();
        assert_eq!(bridge.ble.pairing_replies, vec![true]);
    }

    /// Validates: T-0621 (BLE_PAIRING_CONFIRM_REPLY reject dispatched)
    #[test]
    fn ble_pairing_confirm_reply_reject() {
        let mut bridge = make_bridge_with_ble();
        let reply = ModemMessage::BlePairingConfirmReply(BlePairingConfirmReply { accept: false });
        let frame = encode_modem_frame(&reply).unwrap();
        bridge.usb.inject(&frame);
        bridge.poll();
        assert_eq!(bridge.ble.pairing_replies, vec![false]);
    }

    /// Validates: poll() calls check_pairing_timeout() exactly once per cycle (MD-0414 AC#4).
    #[test]
    fn poll_calls_check_pairing_timeout() {
        let mut bridge = make_bridge_with_ble();
        assert_eq!(bridge.ble.check_pairing_timeout_count.get(), 0);
        bridge.poll();
        assert_eq!(bridge.ble.check_pairing_timeout_count.get(), 1);
        bridge.poll();
        assert_eq!(bridge.ble.check_pairing_timeout_count.get(), 2);
    }

    /// Validates: T-0613 (BLE_RECV forwarded to gateway)
    #[test]
    fn ble_recv_forwarded_to_gateway() {
        let mut bridge = make_bridge_with_ble();
        let data = vec![0x01, 0x00, 0x03, 0xCA, 0xFE, 0xBA];
        bridge.ble.inject_event(BleEvent::Recv(data.clone()));
        bridge.poll();
        let tx = bridge.usb.take_tx();
        let (msg, _) = decode_modem_frame(&tx).unwrap();
        match msg {
            ModemMessage::BleRecv(received) => assert_eq!(received.ble_data, data),
            _ => panic!("expected BleRecv"),
        }
    }

    /// Validates: T-0614 (BLE_CONNECTED notification forwarded)
    #[test]
    fn ble_connected_forwarded_to_gateway() {
        let mut bridge = make_bridge_with_ble();
        let peer = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66];
        bridge.ble.inject_event(BleEvent::Connected {
            peer_addr: peer,
            mtu: 247,
        });
        bridge.poll();
        let tx = bridge.usb.take_tx();
        let (msg, _) = decode_modem_frame(&tx).unwrap();
        match msg {
            ModemMessage::BleConnected(c) => {
                assert_eq!(c.peer_addr, peer);
                assert_eq!(c.mtu, 247);
            }
            _ => panic!("expected BleConnected"),
        }
    }

    /// Validates: T-0615 (BLE_DISCONNECTED notification forwarded)
    #[test]
    fn ble_disconnected_forwarded_to_gateway() {
        let mut bridge = make_bridge_with_ble();
        let peer = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
        bridge.ble.inject_event(BleEvent::Disconnected {
            peer_addr: peer,
            reason: 0x13,
        });
        bridge.poll();
        let tx = bridge.usb.take_tx();
        let (msg, _) = decode_modem_frame(&tx).unwrap();
        match msg {
            ModemMessage::BleDisconnected(d) => {
                assert_eq!(d.peer_addr, peer);
                assert_eq!(d.reason, 0x13);
            }
            _ => panic!("expected BleDisconnected"),
        }
    }

    /// Validates: T-0620 (BLE_PAIRING_CONFIRM forwarded to gateway)
    #[test]
    fn ble_pairing_confirm_forwarded_to_gateway() {
        let mut bridge = make_bridge_with_ble();
        bridge
            .ble
            .inject_event(BleEvent::PairingConfirm { passkey: 123456 });
        bridge.poll();
        let tx = bridge.usb.take_tx();
        let (msg, _) = decode_modem_frame(&tx).unwrap();
        match msg {
            ModemMessage::BlePairingConfirm(p) => assert_eq!(p.passkey, 123456),
            _ => panic!("expected BlePairingConfirm"),
        }
    }

    /// Validates: T-0616 (BLE relay round-trip: BLE_RECV → BLE_INDICATE)
    /// Injects a BLE_RECV event and a BLE_INDICATE command in the same poll.
    #[test]
    fn ble_relay_round_trip() {
        let mut bridge = make_bridge_with_ble();

        // Phone wrote to Gateway Command characteristic.
        let recv_data = vec![0x01, 0x00, 0x00]; // REQUEST_GW_INFO envelope
        bridge.ble.inject_event(BleEvent::Recv(recv_data.clone()));

        // Gateway responds with BLE_INDICATE.
        let indicate_data = vec![0x81, 0x00, 0x10]; // GW_INFO_RESPONSE envelope prefix
        let indicate_frame = encode_modem_frame(&ModemMessage::BleIndicate(BleIndicate {
            ble_data: indicate_data.clone(),
        }))
        .unwrap();
        bridge.usb.inject(&indicate_frame);

        bridge.poll();

        // BLE_RECV should appear on USB.
        let tx = bridge.usb.take_tx();
        let (msg, _) = decode_modem_frame(&tx).unwrap();
        match msg {
            ModemMessage::BleRecv(d) => assert_eq!(d.ble_data, recv_data),
            _ => panic!("expected BleRecv"),
        }

        // BLE layer should have received the indication.
        assert_eq!(bridge.ble.indicated.len(), 1);
        assert_eq!(bridge.ble.indicated[0], indicate_data);
    }

    /// Validates concurrent BLE + ESP-NOW (MD-0405): both paths work in same poll.
    #[test]
    fn ble_and_espnow_concurrent() {
        let mut bridge = make_bridge_with_ble();
        let peer = [1, 2, 3, 4, 5, 6];

        // Inject an ESP-NOW frame.
        bridge.radio.inject_rx(RecvFrame {
            peer_mac: peer,
            rssi: -50,
            frame_data: vec![0xDE, 0xAD],
        });

        // Inject a BLE event.
        bridge.ble.inject_event(BleEvent::Recv(vec![0xCA, 0xFE]));

        bridge.poll();

        let tx = bridge.usb.take_tx();
        // Both RECV_FRAME and BLE_RECV should appear on USB.
        let mut decoder = FrameDecoder::new();
        decoder.push(&tx);

        let mut got_recv_frame = false;
        let mut got_ble_recv = false;
        while let Ok(Some(msg)) = decoder.decode() {
            match msg {
                ModemMessage::RecvFrame(_) => got_recv_frame = true,
                ModemMessage::BleRecv(_) => got_ble_recv = true,
                _ => {}
            }
        }
        assert!(got_recv_frame, "expected RECV_FRAME from ESP-NOW");
        assert!(got_ble_recv, "expected BLE_RECV from BLE layer");
    }

    // --- T-0102: Serial framing — valid frame and max length (MD-0101, MD-0102) ---

    /// Validates: T-0102
    ///
    /// RESET → MODEM_READY, GET_STATUS → well-formed STATUS, then send a
    /// max-length frame (len=512, unknown type) and verify silent discard
    /// followed by successful GET_STATUS.
    #[test]
    fn serial_framing_valid_frame_and_max_length() {
        let mut bridge = make_bridge();

        // 1. RESET → MODEM_READY
        let reset = encode_modem_frame(&ModemMessage::Reset).unwrap();
        bridge.usb.inject(&reset);
        bridge.poll();
        let tx = bridge.usb.take_tx();
        let (msg, _) = decode_modem_frame(&tx).unwrap();
        assert!(matches!(msg, ModemMessage::ModemReady(_)));

        // 2. GET_STATUS → well-formed STATUS
        let get_status = encode_modem_frame(&ModemMessage::GetStatus).unwrap();
        bridge.usb.inject(&get_status);
        bridge.poll();
        let tx = bridge.usb.take_tx();
        let (msg, _) = decode_modem_frame(&tx).unwrap();
        assert!(matches!(msg, ModemMessage::Status(_)));

        // 3. Send max-length frame: len=SERIAL_MAX_LEN, type=0x7F (unknown), padding.
        //    The bridge reads 64 bytes per poll, so multiple polls are needed.
        let max_body = SERIAL_MAX_LEN as usize - 1; // 511 bytes (TYPE takes 1)
        let mut max_frame = Vec::new();
        max_frame.extend_from_slice(&SERIAL_MAX_LEN.to_be_bytes()); // LEN
        max_frame.push(0x7F); // TYPE = unknown
        max_frame.extend_from_slice(&vec![0xAA; max_body]); // padding
        bridge.usb.inject(&max_frame);
        // Frame is > 64 bytes (rx_buf size) — needs multiple polls.
        let max_polls = SERIAL_MAX_FRAME_SIZE / bridge.rx_buf.len() + 1;
        for _ in 0..max_polls {
            if bridge.usb.rx_data.is_empty() {
                break;
            }
            bridge.poll();
        }
        assert!(
            bridge.usb.rx_data.is_empty(),
            "frame must be fully consumed"
        );
        bridge.poll(); // final decode pass

        // No output expected (silent discard of unknown type).
        assert!(bridge.usb.take_tx().is_empty());

        // 4. GET_STATUS again — framing must remain synchronized.
        bridge.usb.inject(&get_status);
        bridge.poll();
        let tx = bridge.usb.take_tx();
        let (msg, _) = decode_modem_frame(&tx).unwrap();
        assert!(matches!(msg, ModemMessage::Status(_)));
    }

    // --- T-0103: Serial framing — oversized len (MD-0102) ---

    /// Validates: T-0103
    ///
    /// Send RESET, then an oversized LEN header (len = SERIAL_MAX_LEN + 1,
    /// exceeding the 512-byte max), then RESET again to resynchronize.
    /// Assert: modem recovers and sends MODEM_READY.
    #[test]
    fn serial_framing_oversized_len() {
        let mut bridge = make_bridge();

        // 1. RESET → MODEM_READY
        let reset = encode_modem_frame(&ModemMessage::Reset).unwrap();
        bridge.usb.inject(&reset);
        bridge.poll();
        let tx = bridge.usb.take_tx();
        assert!(matches!(
            decode_modem_frame(&tx).unwrap().0,
            ModemMessage::ModemReady(_)
        ));

        // 2. Send just the 2-byte LEN header exceeding SERIAL_MAX_LEN.
        //    The bridge detects FrameTooLarge and resets the decoder.
        let oversized_len = SERIAL_MAX_LEN + 1;
        bridge.usb.inject(&oversized_len.to_be_bytes());
        bridge.poll();

        // 3. RESET to resynchronize — decoder was reset, so RESET is parsed
        //    cleanly from the fresh stream.
        bridge.usb.inject(&reset);
        bridge.poll();
        let tx = bridge.usb.take_tx();
        let (msg, _) = decode_modem_frame(&tx).unwrap();
        assert!(
            matches!(msg, ModemMessage::ModemReady(_)),
            "modem must recover after oversized frame"
        );
    }

    // --- T-0601: BLE GATT characteristic setup (MD-0400) ---

    /// Validates: T-0601 (bridge level)
    ///
    /// Verifies that BLE_ENABLE activates the BLE driver (which registers the
    /// GATT service on real hardware). A BLE_CONNECTED event is then forwarded,
    /// confirming the bridge correctly chains enable → connect → notify.
    #[test]
    fn ble_gatt_setup_via_enable_and_connect() {
        let mut bridge = make_bridge_with_ble();

        // Enable BLE (registers GATT service on device).
        let enable = encode_modem_frame(&ModemMessage::BleEnable).unwrap();
        bridge.usb.inject(&enable);
        bridge.poll();
        assert!(bridge.ble.enabled);
        bridge.usb.take_tx(); // discard

        // Simulate a phone connecting after GATT discovery.
        let peer = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66];
        bridge.ble.inject_event(BleEvent::Connected {
            peer_addr: peer,
            mtu: 512,
        });
        bridge.poll();

        let tx = bridge.usb.take_tx();
        let (msg, _) = decode_modem_frame(&tx).unwrap();
        match msg {
            ModemMessage::BleConnected(c) => {
                assert_eq!(c.peer_addr, peer);
                assert_eq!(c.mtu, 512);
            }
            _ => panic!("expected BleConnected"),
        }
    }

    // --- T-0602: MTU negotiation ≥ 247 (MD-0402) ---

    /// Validates: T-0602 (bridge level)
    ///
    /// A BLE_CONNECTED event with mtu ≥ 247 is forwarded to the gateway with
    /// the exact negotiated MTU and peer address preserved.
    #[test]
    fn ble_mtu_negotiation_reported() {
        let mut bridge = make_bridge_with_ble();
        let peer = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66];
        bridge.ble.inject_event(BleEvent::Connected {
            peer_addr: peer,
            mtu: 247,
        });
        bridge.poll();

        let tx = bridge.usb.take_tx();
        let (msg, _) = decode_modem_frame(&tx).unwrap();
        match msg {
            ModemMessage::BleConnected(c) => {
                assert_eq!(c.mtu, 247, "negotiated MTU must be preserved exactly");
                assert_eq!(c.peer_addr, peer, "peer address must be preserved");
            }
            _ => panic!("expected BleConnected"),
        }
    }

    // --- T-0602a: MTU below minimum rejected (MD-0402) ---

    /// Validates: T-0602a (bridge level)
    ///
    /// The protocol codec rejects `BleConnected` with `mtu < BLE_MTU_MIN` at
    /// encode time, so even if the BLE stack emits a low-MTU Connected event
    /// the bridge will not forward it to the gateway.
    #[test]
    fn ble_mtu_below_minimum_no_connected_event() {
        let mut bridge = make_bridge_with_ble();
        let peer = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
        bridge.ble.inject_event(BleEvent::Connected {
            peer_addr: peer,
            mtu: 100, // below BLE_MTU_MIN (247)
        });
        bridge.poll();

        // The codec rejects encoding BleConnected with mtu < 247, so
        // nothing reaches the gateway.
        assert!(
            bridge.usb.take_tx().is_empty(),
            "low-MTU BLE_CONNECTED must not be forwarded (codec rejects mtu < 247)"
        );
    }

    // --- T-0603: BLE write → USB-CDC relay (MD-0401) ---

    /// Validates: T-0603
    ///
    /// A BLE GATT write produces a BLE_RECV event whose ble_data payload is
    /// forwarded verbatim to the gateway over USB-CDC.
    #[test]
    fn ble_write_to_usb_relay() {
        let mut bridge = make_bridge_with_ble();
        let ble_data = vec![0x01, 0x00, 0x05, 0xCA, 0xFE, 0xBA, 0xBE, 0x42];
        bridge.ble.inject_event(BleEvent::Recv(ble_data.clone()));
        bridge.poll();

        let tx = bridge.usb.take_tx();
        let (msg, _) = decode_modem_frame(&tx).unwrap();
        match msg {
            ModemMessage::BleRecv(r) => {
                assert_eq!(r.ble_data, ble_data, "BLE_RECV payload must match write");
            }
            _ => panic!("expected BleRecv"),
        }
    }

    // --- T-0604: USB-CDC → BLE indication relay (MD-0401) ---

    /// Validates: T-0604
    ///
    /// A BLE_INDICATE serial message from the gateway is relayed to the BLE
    /// layer as an indication with the exact payload.
    #[test]
    fn usb_to_ble_indication_relay() {
        let mut bridge = make_bridge_with_ble();
        let payload = vec![0x81, 0x00, 0x10, 0xAB, 0xCD];
        let frame = encode_modem_frame(&ModemMessage::BleIndicate(BleIndicate {
            ble_data: payload.clone(),
        }))
        .unwrap();
        bridge.usb.inject(&frame);
        bridge.poll();

        assert_eq!(bridge.ble.indicated.len(), 1);
        assert_eq!(
            bridge.ble.indicated[0], payload,
            "indication payload must match"
        );
    }

    // --- T-0605: Indication fragmentation (MD-0403) ---

    /// Validates: T-0605 (bridge level)
    ///
    /// A large BLE_INDICATE payload (> MTU-3 = 244 bytes) is passed intact to
    /// the BLE driver. Fragmentation into multiple ATT indications is the
    /// BLE driver's responsibility; the bridge must not truncate or reject it.
    #[test]
    fn ble_indicate_large_payload_forwarded() {
        let mut bridge = make_bridge_with_ble();
        // Payload larger than typical MTU-3 = 244 bytes.
        let large_payload: Vec<u8> = (0..400).map(|i| (i & 0xFF) as u8).collect();
        let frame = encode_modem_frame(&ModemMessage::BleIndicate(BleIndicate {
            ble_data: large_payload.clone(),
        }))
        .unwrap();
        bridge.usb.inject(&frame);
        // Frame is > 64 bytes (rx_buf size) — needs multiple polls.
        let max_polls = frame.len() / bridge.rx_buf.len() + 1;
        for _ in 0..max_polls {
            if bridge.usb.rx_data.is_empty() {
                break;
            }
            bridge.poll();
        }
        assert!(
            bridge.usb.rx_data.is_empty(),
            "frame must be fully consumed"
        );
        bridge.poll(); // final decode pass

        assert_eq!(bridge.ble.indicated.len(), 1);
        assert_eq!(
            bridge.ble.indicated[0], large_payload,
            "large payload must be forwarded intact for BLE-layer fragmentation"
        );
    }

    // --- T-0606: Opaque relay — no content inspection (MD-0401) ---

    /// Validates: T-0606
    ///
    /// Garbage bytes written via BLE are forwarded to USB-CDC without
    /// modification or error. The modem must not inspect or validate BLE
    /// payload contents.
    #[test]
    fn ble_opaque_relay_no_inspection() {
        let mut bridge = make_bridge_with_ble();
        // Invalid / garbage BLE data — not a valid BLE envelope.
        let garbage = vec![0xFF, 0xFE, 0xFD, 0x00, 0x01, 0x02, 0x03];
        bridge.ble.inject_event(BleEvent::Recv(garbage.clone()));
        bridge.poll();

        let tx = bridge.usb.take_tx();
        let (msg, _) = decode_modem_frame(&tx).unwrap();
        match msg {
            ModemMessage::BleRecv(r) => {
                assert_eq!(
                    r.ble_data, garbage,
                    "garbage payload must be relayed verbatim"
                );
            }
            _ => panic!("expected BleRecv, not an error"),
        }
    }

    // --- T-0622: Numeric Comparison timeout (MD-0414) ---

    /// Validates: T-0622 (bridge level)
    ///
    /// After BLE_PAIRING_CONFIRM is forwarded to the gateway, the bridge must
    /// NOT send an automatic reply. The gateway is responsible for sending
    /// BLE_PAIRING_CONFIRM_REPLY; if it never does, the BLE stack times out.
    /// This test verifies the bridge does not auto-accept or auto-reject.
    #[test]
    fn ble_pairing_confirm_no_auto_reply() {
        let mut bridge = make_bridge_with_ble();

        // BLE stack emits a pairing confirm event.
        bridge
            .ble
            .inject_event(BleEvent::PairingConfirm { passkey: 654321 });
        bridge.poll();

        // Verify the event was forwarded to the gateway.
        let tx = bridge.usb.take_tx();
        let (msg, _) = decode_modem_frame(&tx).unwrap();
        assert!(matches!(msg, ModemMessage::BlePairingConfirm(_)));

        // Poll again without sending a reply from the gateway.
        bridge.poll();

        // The bridge must NOT have sent any automatic pairing reply.
        assert!(
            bridge.ble.pairing_replies.is_empty(),
            "bridge must not auto-reply to pairing confirm (timeout is BLE stack's job)"
        );
    }

    // --- T-0628: RESET clears peer table (MD-0300) ---

    /// Validates: T-0628 (peer table gap)
    ///
    /// After sending frames to multiple peers (which registers them in the
    /// radio's peer table), RESET must clear the peer table so no phantom
    /// nodes remain. Stale peers after RESET can cause sends to nodes that
    /// have not re-authenticated.
    #[test]
    fn reset_clears_peer_table() {
        let mut bridge = make_bridge_with_ble();

        // Send frames to three different peers to register them.
        let peers: [[u8; MAC_SIZE]; 3] = [
            [0x11, 0x22, 0x33, 0x44, 0x55, 0x66],
            [0x22, 0x33, 0x44, 0x55, 0x66, 0x77],
            [0xDE, 0xAD, 0xBE, 0xEF, 0x42, 0x01],
        ];
        for peer in &peers {
            let frame = encode_modem_frame(&ModemMessage::SendFrame(SendFrame {
                peer_mac: *peer,
                frame_data: vec![0x42],
            }))
            .unwrap();
            bridge.usb.inject(&frame);
        }
        bridge.poll();
        bridge.usb.take_tx(); // discard any output

        assert_eq!(
            bridge.radio.peers.len(),
            3,
            "three peers must be registered before RESET"
        );

        // RESET must clear the peer table.
        let reset = encode_modem_frame(&ModemMessage::Reset).unwrap();
        bridge.usb.inject(&reset);
        bridge.poll();
        bridge.usb.take_tx(); // discard MODEM_READY

        assert!(
            bridge.radio.peers.is_empty(),
            "peer table must be empty after RESET (MD-0300)"
        );
    }

    // --- T-0633: BLE advertising off after RESET with active session (MD-0407/MD-0412) ---

    /// Validates: T-0633 (security gap — MD-0407 / MD-0412)
    ///
    /// Enables BLE, establishes a connection, queues indications and BLE
    /// events, then sends RESET. Verifies:
    /// 1. BLE advertising is off.
    /// 2. No stale BLE events leak to the gateway after RESET.
    /// 3. Pending indication queue is cleared.
    ///
    /// If RESET doesn't fully tear down BLE state, a phone could connect to
    /// a modem whose gateway session is uninitialized.
    #[test]
    fn reset_clears_ble_state_with_active_session() {
        let mut bridge = make_bridge_with_ble();

        // Enable BLE and simulate a full connection lifecycle.
        let enable = encode_modem_frame(&ModemMessage::BleEnable).unwrap();
        bridge.usb.inject(&enable);
        bridge.poll();
        assert!(bridge.ble.enabled);
        bridge.usb.take_tx(); // discard

        // Simulate phone connecting.
        let peer = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66];
        bridge.ble.inject_event(BleEvent::Connected {
            peer_addr: peer,
            mtu: 512,
        });
        bridge.poll();
        bridge.usb.take_tx(); // discard BLE_CONNECTED

        // Queue a pending indication (gateway → phone direction).
        let indicate = encode_modem_frame(&ModemMessage::BleIndicate(BleIndicate {
            ble_data: vec![0xCA, 0xFE],
        }))
        .unwrap();
        bridge.usb.inject(&indicate);
        bridge.poll();
        bridge.usb.take_tx(); // discard

        // Inject stale BLE events that should NOT survive RESET.
        bridge.ble.inject_event(BleEvent::Recv(vec![0xDE, 0xAD]));
        bridge
            .ble
            .inject_event(BleEvent::PairingConfirm { passkey: 999999 });

        // RESET — must tear down all BLE state.
        let reset = encode_modem_frame(&ModemMessage::Reset).unwrap();
        bridge.usb.inject(&reset);
        bridge.poll();

        // 1. BLE advertising must be off.
        assert!(!bridge.ble.enabled, "BLE must be off after RESET (MD-0412)");

        // 2. Only MODEM_READY should appear on USB — no stale BLE events.
        let tx = bridge.usb.take_tx();
        let mut decoder = FrameDecoder::new();
        decoder.push(&tx);
        let mut messages = Vec::new();
        loop {
            match decoder.decode() {
                Ok(Some(msg)) => messages.push(msg),
                Ok(None) => break,
                Err(e) => panic!("failed to decode modem frame after RESET: {:?}", e),
            }
        }
        assert_eq!(messages.len(), 1, "only MODEM_READY expected after RESET");
        assert!(
            matches!(messages[0], ModemMessage::ModemReady(_)),
            "first message after RESET must be MODEM_READY, got {:?}",
            &messages[0]
        );

        // 3. Indication queue must be cleared.
        assert!(
            bridge.ble.indicated.is_empty(),
            "indication queue must be empty after RESET"
        );

        // 4. BLE event queue must be drained (no stale events).
        assert!(
            bridge.ble.event_queue.borrow().is_empty(),
            "BLE event queue must be empty after RESET"
        );
    }

    // --- T-0630: BLE message boundary preservation (MD-0401) ---

    /// Validates: T-0630 (boundary preservation gap — MD-0401)
    ///
    /// Injects multiple rapid GATT writes and asserts exactly N separate
    /// `BLE_RECV` messages appear on USB, each with the correct payload.
    /// Merging or splitting under load would silently corrupt gateway
    /// message parsing.
    #[test]
    fn ble_multiple_writes_preserve_boundaries() {
        let mut bridge = make_bridge_with_ble();

        // Inject 5 rapid BLE writes with distinct payloads.
        let payloads: Vec<Vec<u8>> = vec![
            vec![0x01],
            vec![0x02, 0x03],
            vec![0x04, 0x05, 0x06],
            vec![0x07, 0x08, 0x09, 0x0A],
            vec![0x0B, 0x0C, 0x0D, 0x0E, 0x0F],
        ];
        for payload in &payloads {
            bridge.ble.inject_event(BleEvent::Recv(payload.clone()));
        }

        // Single poll — all events must be processed without merging.
        bridge.poll();

        // Decode all USB output.
        let tx = bridge.usb.take_tx();
        let mut decoder = FrameDecoder::new();
        decoder.push(&tx);
        let mut received: Vec<Vec<u8>> = Vec::new();
        loop {
            match decoder.decode() {
                Ok(Some(msg)) => match msg {
                    ModemMessage::BleRecv(r) => received.push(r.ble_data),
                    ModemMessage::Error(e) => panic!("unexpected Error on USB: {:?}", e),
                    _ => {
                        // Ignore non-error, non-BleRecv frames; this test only cares about
                        // preserving BLE_RECV boundaries and payloads.
                    }
                },
                Ok(None) => break,
                Err(e) => panic!("failed to decode modem frame: {:?}", e),
            }
        }

        // Exactly N messages, each with the correct payload.
        assert_eq!(
            received.len(),
            payloads.len(),
            "each GATT write must produce exactly one BLE_RECV (1:1 boundary)"
        );
        for (i, (got, want)) in received.iter().zip(payloads.iter()).enumerate() {
            assert_eq!(
                got, want,
                "BLE_RECV[{}] payload mismatch: boundary was not preserved",
                i
            );
        }
    }

    // --- Issue #339: Validation gap tests ---

    /// Validates: MD-0200 (ESP-NOW default channel is 1 at initial boot)
    ///
    /// The modem must initialize ESP-NOW on channel 1. Verifies both
    /// the radio layer and the STATUS response report channel 1 before
    /// any SET_CHANNEL command is sent.
    #[test]
    fn default_channel_is_one_at_boot() {
        let mut bridge = make_bridge();
        // Radio layer should start on channel 1.
        assert_eq!(bridge.radio.channel(), 1);

        // STATUS should also report channel 1.
        let frame = encode_modem_frame(&ModemMessage::GetStatus).unwrap();
        bridge.usb.inject(&frame);
        bridge.poll();
        let tx = bridge.usb.take_tx();
        let (msg, _) = decode_modem_frame(&tx).unwrap();
        match msg {
            ModemMessage::Status(s) => {
                assert_eq!(s.channel, 1, "default channel must be 1 at boot");
            }
            _ => panic!("expected Status"),
        }
    }

    /// Validates: MD-0413 AC3 (BLE_ENABLE idempotent — duplicate is no-op)
    ///
    /// Sending BLE_ENABLE when already enabled must not reinitialize BLE
    /// or disrupt an active connection. Asserts only externally observable
    /// behavior: no disconnect event, BLE stays enabled, indication works.
    #[test]
    fn ble_enable_idempotent() {
        let mut bridge = make_bridge_with_ble();

        // First BLE_ENABLE.
        let enable = encode_modem_frame(&ModemMessage::BleEnable).unwrap();
        bridge.usb.inject(&enable);
        bridge.poll();
        assert!(bridge.ble.enabled);
        bridge.usb.take_tx(); // discard

        // Simulate an active BLE connection.
        let peer = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66];
        bridge.ble.inject_event(BleEvent::Connected {
            peer_addr: peer,
            mtu: 247,
        });
        bridge.poll();
        bridge.usb.take_tx(); // discard BLE_CONNECTED

        // Second BLE_ENABLE — must not disrupt the connection.
        bridge.usb.inject(&enable);
        bridge.poll();
        assert!(bridge.ble.enabled, "BLE must remain enabled");

        // No BLE_DISCONNECTED event should have been emitted.
        let tx = bridge.usb.take_tx();
        assert!(
            tx.is_empty(),
            "duplicate BLE_ENABLE must not produce output or disconnect"
        );

        // Connection should still be usable — indicate data to BLE client.
        let indicate = encode_modem_frame(&ModemMessage::BleIndicate(BleIndicate {
            ble_data: vec![0xAB, 0xCD],
        }))
        .unwrap();
        bridge.usb.inject(&indicate);
        bridge.poll();
        assert_eq!(
            bridge.ble.indicated.len(),
            1,
            "BLE indicate must still work after duplicate enable"
        );
    }

    /// Validates: MD-0413 AC4 (BLE_DISABLE idempotent — duplicate is no-op)
    ///
    /// Sending BLE_DISABLE when already disabled must not crash or
    /// produce unexpected output. Asserts only externally observable
    /// behavior: no serial output, BLE stays disabled, bridge operational.
    #[test]
    fn ble_disable_idempotent() {
        let mut bridge = make_bridge_with_ble();

        // BLE starts disabled — first BLE_DISABLE is already a duplicate.
        assert!(!bridge.ble.enabled);
        let disable = encode_modem_frame(&ModemMessage::BleDisable).unwrap();
        bridge.usb.inject(&disable);
        bridge.poll();
        assert!(!bridge.ble.enabled);
        assert!(bridge.usb.take_tx().is_empty(), "no output expected");

        // Second BLE_DISABLE.
        bridge.usb.inject(&disable);
        bridge.poll();
        assert!(!bridge.ble.enabled);
        assert!(
            bridge.usb.take_tx().is_empty(),
            "duplicate BLE_DISABLE must not produce output"
        );

        // Bridge should still be operational.
        let frame = encode_modem_frame(&ModemMessage::GetStatus).unwrap();
        bridge.usb.inject(&frame);
        bridge.poll();
        let tx = bridge.usb.take_tx();
        let (msg, _) = decode_modem_frame(&tx).unwrap();
        assert!(matches!(msg, ModemMessage::Status(_)));
    }

    /// Validates: MD-0202 AC3 / MD-0303 AC3 (tx_fail_count reported in STATUS)
    ///
    /// Triggers a non-zero `tx_fail_count` and verifies STATUS reports it.
    /// On real hardware the ESP-NOW send callback increments `tx_fail_count`;
    /// here we simulate via the shared `ModemCounters`.
    #[test]
    fn tx_fail_count_reported_in_status() {
        let mut bridge = make_bridge();

        // Simulate the ESP-NOW driver reporting 3 tx failures.
        bridge.counters.inc_tx_fail();
        bridge.counters.inc_tx_fail();
        bridge.counters.inc_tx_fail();
        // Also simulate some successful sends.
        bridge.counters.inc_tx();
        bridge.counters.inc_tx();

        // Query status.
        let frame = encode_modem_frame(&ModemMessage::GetStatus).unwrap();
        bridge.usb.inject(&frame);
        bridge.poll();
        let tx = bridge.usb.take_tx();
        let (msg, _) = decode_modem_frame(&tx).unwrap();
        match msg {
            ModemMessage::Status(s) => {
                assert_eq!(
                    s.tx_fail_count, 3,
                    "tx_fail_count must reflect failed sends"
                );
                assert_eq!(s.tx_count, 2, "tx_count must reflect successful sends");
            }
            _ => panic!("expected Status"),
        }
    }

    /// Validates: MD-0301 (ESP-NOW frames during USB disconnect are discarded)
    ///
    /// Injects radio frames while USB is disconnected, then reconnects
    /// and verifies no stale frames are flushed to the gateway.
    #[test]
    fn frames_during_usb_disconnect_are_discarded() {
        let mut bridge = make_bridge();
        let peer = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66];

        // Simulate USB disconnect.
        bridge.usb.set_connected(false);

        // Inject radio frames during the disconnect.
        for i in 0u8..5 {
            bridge.radio.inject_rx(RecvFrame {
                peer_mac: peer,
                rssi: -50,
                frame_data: vec![i],
            });
        }

        // Poll while disconnected — frames should be drained but writes fail.
        bridge.poll();
        assert_eq!(
            bridge.counters.rx_count(),
            0,
            "rx_count must not increment when USB is disconnected"
        );

        // Reconnect USB.
        bridge.usb.set_connected(true);
        bridge.usb.set_reconnect_once();
        bridge.poll();

        // Only MODEM_READY should appear — no stale RECV_FRAMEs.
        // Decode the entire tx buffer to catch any trailing messages.
        let tx = bridge.usb.take_tx();
        let mut remaining = tx.as_slice();
        let mut messages = Vec::new();
        while !remaining.is_empty() {
            let (msg, consumed) =
                decode_modem_frame(remaining).expect("failed to decode frame from tx buffer");
            messages.push(msg);
            remaining = &remaining[consumed..];
        }
        assert_eq!(
            messages.len(),
            1,
            "expected exactly one message after reconnect, got {}",
            messages.len()
        );
        assert!(
            matches!(messages[0], ModemMessage::ModemReady(_)),
            "only MODEM_READY expected after reconnect, got {:?}",
            messages[0]
        );

        // Verify the radio queue is empty (frames were consumed, not re-queued).
        assert!(
            bridge.radio.drain_one().is_none(),
            "radio queue must be empty — stale frames must not survive reconnect"
        );
    }

    /// Validates: MD-0206 AC3 (peer table cleared on channel change)
    ///
    /// Sends frames to populate the peer table, then changes the channel
    /// and verifies the peer table was cleared.
    #[test]
    fn peer_table_cleared_on_channel_change() {
        let mut bridge = make_bridge();

        // Send frames to three distinct peers to populate the peer table.
        for i in 1u8..=3 {
            let peer = [i, 0, 0, 0, 0, 0];
            let sf = ModemMessage::SendFrame(SendFrame {
                peer_mac: peer,
                frame_data: vec![0xAA],
            });
            let frame = encode_modem_frame(&sf).unwrap();
            bridge.usb.inject(&frame);
        }
        bridge.poll();
        assert_eq!(
            bridge.radio.peer_count(),
            3,
            "should have 3 peers before channel change"
        );

        // Change channel.
        let set_ch = encode_modem_frame(&ModemMessage::SetChannel(6)).unwrap();
        bridge.usb.inject(&set_ch);
        bridge.poll();
        bridge.usb.take_tx(); // discard SET_CHANNEL_ACK

        // Peer table must be empty after channel change.
        assert_eq!(
            bridge.radio.peer_count(),
            0,
            "peer table must be cleared on channel change (MD-0206 AC3)"
        );
    }

    /// Validates: MD-0207 AC3 (ESP-NOW resumes after channel scan)
    ///
    /// After SCAN_CHANNELS completes, verifies that the radio can still
    /// send and receive ESP-NOW frames.
    #[test]
    fn espnow_resumes_after_channel_scan() {
        let mut bridge = make_bridge();

        // Perform a channel scan.
        let scan = encode_modem_frame(&ModemMessage::ScanChannels).unwrap();
        bridge.usb.inject(&scan);
        bridge.poll();
        let tx = bridge.usb.take_tx();
        let (msg, _) = decode_modem_frame(&tx).unwrap();
        assert!(matches!(msg, ModemMessage::ScanResult(_)));

        // Send a frame after scan — radio TX must work.
        let peer = [1, 2, 3, 4, 5, 6];
        let sf = ModemMessage::SendFrame(SendFrame {
            peer_mac: peer,
            frame_data: vec![0xDE, 0xAD],
        });
        let frame = encode_modem_frame(&sf).unwrap();
        bridge.usb.inject(&frame);
        bridge.poll();
        assert_eq!(bridge.radio.sent.len(), 1, "radio TX must work after scan");
        assert_eq!(bridge.radio.sent[0].0, vec![0xDE, 0xAD]);

        // Receive a frame after scan — radio RX must work.
        bridge.radio.inject_rx(RecvFrame {
            peer_mac: peer,
            rssi: -45,
            frame_data: vec![0xBE, 0xEF],
        });
        bridge.poll();
        let tx = bridge.usb.take_tx();
        let (msg, _) = decode_modem_frame(&tx).unwrap();
        match msg {
            ModemMessage::RecvFrame(rf) => {
                assert_eq!(rf.frame_data, vec![0xBE, 0xEF]);
                assert_eq!(rf.peer_mac, peer);
            }
            _ => panic!("expected RecvFrame after scan"),
        }
    }

    /// Validates: MD-0303 AC5 (uptime_s accuracy — not just > 0)
    ///
    /// Uses a backdated boot time to verify `uptime_s` reflects actual
    /// elapsed seconds, not a stuck-at-1 sentinel.
    #[test]
    fn uptime_accuracy_reflects_elapsed_time() {
        use std::time::{Duration, Instant};

        // Backdate boot_time by 7 seconds.
        let boot = Instant::now() - Duration::from_secs(7);
        let counters = ModemCounters::new_with_boot_time(boot);
        let mut bridge = Bridge::new(MockSerial::new(), MockRadio::new(), counters);

        let frame = encode_modem_frame(&ModemMessage::GetStatus).unwrap();
        bridge.usb.inject(&frame);
        bridge.poll();
        let tx = bridge.usb.take_tx();
        let (msg, _) = decode_modem_frame(&tx).unwrap();
        match msg {
            ModemMessage::Status(s) => {
                assert!(
                    (6..=8).contains(&s.uptime_s),
                    "uptime_s should be ~7s, got {}",
                    s.uptime_s
                );
            }
            _ => panic!("expected Status"),
        }
    }

    /// Validates: MD-0201 (rapid radio frame burst — exactly one RECV_FRAME
    /// per ESP-NOW frame received)
    ///
    /// Injects 32 frames in a rapid burst (spanning multiple poll cycles
    /// due to MAX_RX_FRAMES_PER_POLL cap) and verifies a strict 1:1 mapping
    /// between injected and forwarded frames with correct payloads.
    #[test]
    fn rapid_radio_burst_one_recv_per_frame() {
        let mut bridge = make_bridge();
        let peer = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66];
        let total: usize = 32;

        // Inject a burst of frames.
        for i in 0..total {
            bridge.radio.inject_rx(RecvFrame {
                peer_mac: peer,
                rssi: -30 - (i as i8 % 30),
                frame_data: vec![(i & 0xFF) as u8, ((i >> 8) & 0xFF) as u8],
            });
        }

        // Drain across multiple poll cycles (cap is MAX_RX_FRAMES_PER_POLL = 16).
        let mut all_tx = Vec::new();
        let max_polls = total / MAX_RX_FRAMES_PER_POLL + 2;
        for _ in 0..max_polls {
            bridge.poll();
            all_tx.extend_from_slice(&bridge.usb.take_tx());
        }

        // Decode all forwarded frames and verify 1:1 mapping.
        let mut decoder = FrameDecoder::new();
        decoder.push(&all_tx);
        let mut recv_count = 0usize;
        while let Ok(Some(msg)) = decoder.decode() {
            match msg {
                ModemMessage::RecvFrame(rf) => {
                    assert_eq!(
                        rf.frame_data,
                        vec![(recv_count & 0xFF) as u8, ((recv_count >> 8) & 0xFF) as u8],
                        "frame {} payload mismatch",
                        recv_count
                    );
                    assert_eq!(rf.peer_mac, peer);
                    recv_count += 1;
                }
                _ => panic!("unexpected message type in burst output"),
            }
        }
        assert_eq!(
            recv_count, total,
            "exactly one RECV_FRAME per injected frame (got {}, expected {})",
            recv_count, total
        );
        assert_eq!(bridge.counters.rx_count(), total as u32);
    }
}
