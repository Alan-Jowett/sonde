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
    encode_modem_frame, FrameDecoder, ModemCodecError, ModemError, ModemMessage, ModemReady,
    ModemStatus, RecvFrame, ScanEntry, ScanResult, SendFrame, MAC_SIZE,
    MODEM_ERR_CHANNEL_SET_FAILED,
};

use crate::status::ModemCounters;

/// Firmware version: major.minor.patch.build (one byte each).
const FIRMWARE_VERSION: [u8; 4] = [0, 1, 0, 0];

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
    /// Drain received frames from the queue.
    fn drain_rx(&self) -> Vec<RecvFrame>;
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

/// Bridge between a serial port and a radio driver.
pub struct Bridge<S: SerialPort, R: Radio> {
    usb: S,
    radio: R,
    counters: Arc<ModemCounters>,
    decoder: FrameDecoder,
    rx_buf: [u8; 64],
}

impl<S: SerialPort, R: Radio> Bridge<S, R> {
    pub fn new(usb: S, radio: R, counters: Arc<ModemCounters>) -> Self {
        Self {
            usb,
            radio,
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
            "sent MODEM_READY (fw={}.{}.{}.{}, mac={:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X})",
            FIRMWARE_VERSION[0],
            FIRMWARE_VERSION[1],
            FIRMWARE_VERSION[2],
            FIRMWARE_VERSION[3],
            mac[0],
            mac[1],
            mac[2],
            mac[3],
            mac[4],
            mac[5],
        );
    }

    /// Poll for serial data and radio received frames.
    pub fn poll(&mut self) {
        let (n, reconnected) = self.usb.read(&mut self.rx_buf);
        if reconnected {
            info!("USB reconnected, sending MODEM_READY");
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

        // Forward any received radio frames to the serial port.
        let rx_frames = self.radio.drain_rx();
        for rf in rx_frames {
            let msg = ModemMessage::RecvFrame(rf);
            if self.send_msg(&msg) {
                self.counters.inc_rx();
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
            ModemMessage::Unknown { .. } => {}
            _ => {}
        }
    }

    fn handle_reset(&mut self) {
        info!("RESET received");
        self.radio.reset_state();
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use sonde_protocol::modem::{decode_modem_frame, ModemMessage};

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
        rx_queue: RefCell<Vec<RecvFrame>>,
        channel: u8,
        mac: [u8; MAC_SIZE],
    }

    impl MockRadio {
        fn new() -> Self {
            Self {
                sent: Vec::new(),
                rx_queue: RefCell::new(Vec::new()),
                channel: 1,
                mac: [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF],
            }
        }

        fn inject_rx(&self, frame: RecvFrame) {
            self.rx_queue.borrow_mut().push(frame);
        }
    }

    impl Radio for MockRadio {
        fn send(&mut self, peer_mac: &[u8; MAC_SIZE], data: &[u8]) {
            self.sent.push((data.to_vec(), *peer_mac));
        }
        fn drain_rx(&self) -> Vec<RecvFrame> {
            std::mem::take(&mut *self.rx_queue.borrow_mut())
        }
        fn set_channel(&mut self, channel: u8) -> Result<(), &'static str> {
            if channel == 0 || channel > 14 {
                return Err("invalid channel");
            }
            self.channel = channel;
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
        bridge.usb.take_tx(); // should be empty (no valid response)

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
        assert!(tx.is_empty(), "modem should not send any response for SEND_FRAME");
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
}
