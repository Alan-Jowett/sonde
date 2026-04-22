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

use log::{debug, info, warn};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sonde_protocol::modem::{
    encode_modem_frame, BleConnected, BleDisconnected, BlePairingConfirm, BlePairingConfirmReply,
    BleRecv, EventButton, FrameDecoder, ModemCodecError, ModemError, ModemMessage, ModemReady,
    ModemStatus, RecvFrame, ScanEntry, ScanResult, SendFrame, BUTTON_TYPE_LONG, BUTTON_TYPE_SHORT,
    MAC_SIZE, MODEM_ERR_CHANNEL_SET_FAILED,
};

use crate::status::ModemCounters;

/// Parse a single Cargo version component (digits only, must fit in u8).
const fn parse_version_component(s: &str) -> u8 {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        panic!("empty Cargo version component");
    }

    let mut val = 0u16;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b < b'0' || b > b'9' {
            panic!("non-digit Cargo version component");
        }
        val = val * 10 + (b - b'0') as u16;
        if val > u8::MAX as u16 {
            panic!("Cargo version component exceeds u8 range");
        }
        i += 1;
    }

    val as u8
}

/// Firmware version derived from the workspace Cargo version at compile time.
/// Encoded as [major, minor, patch, build] (one byte each) for the
/// MODEM_READY wire format.
const FIRMWARE_VERSION: [u8; 4] = [
    parse_version_component(env!("CARGO_PKG_VERSION_MAJOR")),
    parse_version_component(env!("CARGO_PKG_VERSION_MINOR")),
    parse_version_component(env!("CARGO_PKG_VERSION_PATCH")),
    0,
];

/// Maximum number of received radio frames forwarded per `poll()` call.
/// Prevents starvation of serial decode and other poll() work under
/// sustained RX burst traffic.
const MAX_RX_FRAMES_PER_POLL: usize = 16;

/// Maximum number of BLE events forwarded per `poll()` call.
/// Prevents starvation of serial decode and radio under sustained BLE traffic.
const MAX_BLE_EVENTS_PER_POLL: usize = 16;

/// Human-readable label for a `ModemMessage` variant (used in debug logging).
fn msg_type_label(msg: &ModemMessage) -> &'static str {
    match msg {
        ModemMessage::Reset => "RESET",
        ModemMessage::SendFrame(_) => "SEND_FRAME",
        ModemMessage::SetChannel(_) => "SET_CHANNEL",
        ModemMessage::GetStatus => "GET_STATUS",
        ModemMessage::ScanChannels => "SCAN_CHANNELS",
        ModemMessage::ModemReady(_) => "MODEM_READY",
        ModemMessage::RecvFrame(_) => "RECV_FRAME",
        ModemMessage::SetChannelAck(_) => "SET_CHANNEL_ACK",
        ModemMessage::Status(_) => "STATUS",
        ModemMessage::ScanResult(_) => "SCAN_RESULT",
        ModemMessage::Error(_) => "ERROR",
        ModemMessage::BleIndicate(_) => "BLE_INDICATE",
        ModemMessage::BleEnable => "BLE_ENABLE",
        ModemMessage::BleDisable => "BLE_DISABLE",
        ModemMessage::BlePairingConfirmReply(_) => "BLE_PAIRING_CONFIRM_REPLY",
        ModemMessage::BleRecv(_) => "BLE_RECV",
        ModemMessage::BleConnected(_) => "BLE_CONNECTED",
        ModemMessage::BleDisconnected(_) => "BLE_DISCONNECTED",
        ModemMessage::BlePairingConfirm(_) => "BLE_PAIRING_CONFIRM",
        ModemMessage::EventButton(_) => "EVENT_BUTTON",
        _ => "UNKNOWN",
    }
}

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
    /// Send a frame to the given peer MAC. Returns `true` on success.
    fn send(&mut self, peer_mac: &[u8; MAC_SIZE], data: &[u8]) -> bool;
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
    /// Poll the NimBLE link security state as a fallback for Android LESC
    /// Numeric Comparison, in case `on_authentication_complete` did not fire
    /// due to a `ble_gap_conn_find` race in the ENC_CHANGE event handler.
    ///
    /// Must be called once per poll cycle.
    fn check_encryption_fallback(&self) {}
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

// ---------------------------------------------------------------------------
// Button scanner (MD-0600 – MD-0605)
// ---------------------------------------------------------------------------

/// Debounce window for button press/release transitions (MD-0601).
const BUTTON_DEBOUNCE_MS: u64 = 30;

/// Threshold separating short from long presses (MD-0602).
const BUTTON_LONG_THRESHOLD: Duration = Duration::from_secs(1);

/// Internal state of the button debounce/classification state machine.
#[derive(Debug, Clone, Copy, PartialEq)]
enum ButtonState {
    /// GPIO is HIGH (not pressed). Waiting for a LOW transition.
    Idle,
    /// GPIO went LOW. Waiting for debounce period to confirm press.
    DebouncePress { since: Instant },
    /// Debounced press confirmed. Recording hold duration.
    Pressed { press_start: Instant },
    /// GPIO went HIGH after press. Waiting for debounce period to confirm release.
    DebounceRelease {
        press_start: Instant,
        since: Instant,
    },
}

/// Platform-independent button scanner.
///
/// Polls a GPIO read function each main-loop iteration, debounces transitions,
/// classifies press duration, and returns button events. The scanner does not
/// perform any I/O — the caller (bridge) handles USB-CDC emission.
pub struct ButtonScanner<F: FnMut() -> bool> {
    read_gpio: F,
    state: ButtonState,
}

impl<F: FnMut() -> bool> ButtonScanner<F> {
    /// Create a new button scanner with the given GPIO read function.
    ///
    /// `read_gpio` should return `true` when the button is pressed (active-low
    /// GPIO reads LOW → caller inverts to `true`).
    pub fn new(read_gpio: F) -> Self {
        Self {
            read_gpio,
            state: ButtonState::Idle,
        }
    }

    /// Poll the button GPIO and advance the state machine.
    ///
    /// Returns `Some(BUTTON_TYPE_SHORT)` or `Some(BUTTON_TYPE_LONG)` when a
    /// debounced release is detected, or `None` otherwise.
    pub fn poll(&mut self) -> Option<u8> {
        let pressed = (self.read_gpio)();
        self.poll_at(Instant::now(), pressed)
    }

    /// Advance the state machine with an explicit timestamp and GPIO state.
    ///
    /// This is the core logic, separated from `poll()` so tests can inject
    /// a deterministic clock without sleeping.
    pub fn poll_at(&mut self, now: Instant, pressed: bool) -> Option<u8> {
        let debounce = Duration::from_millis(BUTTON_DEBOUNCE_MS);

        match self.state {
            ButtonState::Idle => {
                if pressed {
                    self.state = ButtonState::DebouncePress { since: now };
                }
                None
            }
            ButtonState::DebouncePress { since } => {
                if !pressed {
                    // Glitch — return to idle.
                    self.state = ButtonState::Idle;
                } else if now.duration_since(since) >= debounce {
                    // Debounce confirmed — record the actual debounced transition
                    // instant, not `now`, so poll-cadence jitter doesn't inflate
                    // or deflate the measured press duration (MD-0602).
                    self.state = ButtonState::Pressed {
                        press_start: since + debounce,
                    };
                }
                None
            }
            ButtonState::Pressed { press_start } => {
                if !pressed {
                    self.state = ButtonState::DebounceRelease {
                        press_start,
                        since: now,
                    };
                }
                None
            }
            ButtonState::DebounceRelease { press_start, since } => {
                if pressed {
                    // Bounce — return to pressed.
                    self.state = ButtonState::Pressed { press_start };
                    None
                } else if now.duration_since(since) >= debounce {
                    // Debounced release — classify using the true debounced
                    // release instant so poll-cadence jitter doesn't skew the
                    // measured duration.
                    self.state = ButtonState::Idle;
                    let debounced_release = since + debounce;
                    let duration = debounced_release.duration_since(press_start);
                    if duration >= BUTTON_LONG_THRESHOLD {
                        Some(BUTTON_TYPE_LONG)
                    } else {
                        Some(BUTTON_TYPE_SHORT)
                    }
                } else {
                    None
                }
            }
        }
    }
}

/// No-op button scanner for builds without GPIO (host-side testing).
pub struct NoButton;

impl NoButton {
    pub fn poll(&mut self) -> Option<u8> {
        None
    }
}

/// Abstraction over a button input (GPIO on device, mock/no-op in tests).
pub trait ButtonPoll {
    /// Poll the button and return `Some(button_type)` on a classified release.
    fn poll(&mut self) -> Option<u8>;
}

impl ButtonPoll for NoButton {
    fn poll(&mut self) -> Option<u8> {
        None
    }
}

impl<F: FnMut() -> bool> ButtonPoll for ButtonScanner<F> {
    fn poll(&mut self) -> Option<u8> {
        ButtonScanner::poll(self)
    }
}

/// Bridge between a serial port, a radio driver, an optional BLE driver,
/// and an optional button scanner.
pub struct Bridge<S: SerialPort, R: Radio, B: Ble = NoBle, Btn: ButtonPoll = NoButton> {
    usb: S,
    radio: R,
    ble: B,
    button: Btn,
    ble_enabled: bool,
    counters: Arc<ModemCounters>,
    decoder: FrameDecoder,
    rx_buf: [u8; 64],
}

impl<S: SerialPort, R: Radio> Bridge<S, R, NoBle, NoButton> {
    /// Create a bridge without BLE support (no-op BLE driver).
    pub fn new(usb: S, radio: R, counters: Arc<ModemCounters>) -> Self {
        Self {
            usb,
            radio,
            ble: NoBle,
            button: NoButton,
            ble_enabled: false,
            counters,
            decoder: FrameDecoder::new(),
            rx_buf: [0u8; 64],
        }
    }
}

impl<S: SerialPort, R: Radio, B: Ble> Bridge<S, R, B, NoButton> {
    /// Create a bridge with a BLE driver (BLE starts disabled, no button scanner).
    pub fn with_ble(usb: S, radio: R, mut ble: B, counters: Arc<ModemCounters>) -> Self {
        ble.disable();
        Self {
            usb,
            radio,
            ble,
            button: NoButton,
            ble_enabled: false,
            counters,
            decoder: FrameDecoder::new(),
            rx_buf: [0u8; 64],
        }
    }
}

impl<S: SerialPort, R: Radio, B: Ble, Btn: ButtonPoll> Bridge<S, R, B, Btn> {
    /// Create a bridge with a BLE driver and a button scanner.
    ///
    /// The BLE driver is explicitly disabled during construction so that
    /// `ble_enabled` and the hardware state are guaranteed to be in sync.
    /// Callers that need BLE active must send a `BLE_ENABLE` command after
    /// construction; the driver will never be silently left enabled.
    pub fn with_ble_and_button(
        usb: S,
        radio: R,
        mut ble: B,
        button: Btn,
        counters: Arc<ModemCounters>,
    ) -> Self {
        ble.disable();
        Self {
            usb,
            radio,
            ble,
            button,
            ble_enabled: false,
            counters,
            decoder: FrameDecoder::new(),
            rx_buf: [0u8; 64],
        }
    }

    /// Encode and write a modem message to the serial port. Returns true
    /// if the write succeeded.
    fn send_msg(&mut self, msg: &ModemMessage) -> bool {
        match encode_modem_frame(msg) {
            Ok(frame) => {
                let ok = self.usb.write(&frame);
                if ok {
                    debug!("USB-CDC TX: {} len={}", msg_type_label(msg), frame.len());
                } else {
                    debug!(
                        "USB-CDC TX failed: {} len={}",
                        msg_type_label(msg),
                        frame.len()
                    );
                }
                ok
            }
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
                    let peer_mac = rf.peer_mac;
                    let len = rf.frame_data.len();
                    let rssi = rf.rssi;
                    let msg = ModemMessage::RecvFrame(rf);
                    if self.send_msg(&msg) {
                        self.counters.inc_rx();
                        info!(
                            "ESP-NOW RX: peer={:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X} len={} rssi={}",
                            peer_mac[0],
                            peer_mac[1],
                            peer_mac[2],
                            peer_mac[3],
                            peer_mac[4],
                            peer_mac[5],
                            len,
                            rssi
                        );
                    }
                }
                None => break,
            }
        }

        // Advance fragmented BLE indications (one chunk per poll cycle).
        self.ble.advance_indication();

        // Enforce BLE pairing timeout (MD-0414 AC#4).
        self.ble.check_pairing_timeout();

        // Android LESC NC: polling fallback for missed on_authentication_complete.
        self.ble.check_encryption_fallback();

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

        // Poll button GPIO and emit EVENT_BUTTON on classified release (MD-0603).
        if let Some(button_type) = self.button.poll() {
            let msg = ModemMessage::EventButton(EventButton { button_type });
            if self.send_msg(&msg) {
                info!("EVENT_BUTTON: button_type={}", button_type);
            } else {
                warn!(
                    "EVENT_BUTTON dropped: button_type={} (USB-CDC not writable)",
                    button_type
                );
            }
        }
    }

    fn dispatch(&mut self, msg: ModemMessage) {
        debug!("USB-CDC RX: {}", msg_type_label(&msg));
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
        self.ble_enabled = false;
        self.counters.reset();
        self.decoder.reset();
        self.send_modem_ready();
    }

    fn handle_send_frame(&mut self, sf: SendFrame) {
        // NOTE: Radio::send() returns the esp_now_send() queue result, not
        // the delivery ACK.  Async delivery failures are tracked separately
        // via the send callback counter (tx_fail_count, MD-0202).
        let ok = self.radio.send(&sf.peer_mac, &sf.frame_data);
        if ok {
            info!(
                "ESP-NOW TX: peer={:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X} len={} result=ok",
                sf.peer_mac[0],
                sf.peer_mac[1],
                sf.peer_mac[2],
                sf.peer_mac[3],
                sf.peer_mac[4],
                sf.peer_mac[5],
                sf.frame_data.len()
            );
        } else {
            info!(
                "ESP-NOW TX: peer={:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X} len={} result=fail",
                sf.peer_mac[0],
                sf.peer_mac[1],
                sf.peer_mac[2],
                sf.peer_mac[3],
                sf.peer_mac[4],
                sf.peer_mac[5],
                sf.frame_data.len()
            );
            warn!(
                "ESP-NOW TX failed: peer={:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X} len={} result=fail",
                sf.peer_mac[0],
                sf.peer_mac[1],
                sf.peer_mac[2],
                sf.peer_mac[3],
                sf.peer_mac[4],
                sf.peer_mac[5],
                sf.frame_data.len()
            );
        }
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
        if self.ble_enabled {
            debug!("BLE_ENABLE received (already enabled; retrying enable)");
        } else {
            info!("BLE_ENABLE received");
        }
        self.ble.enable();
        self.ble_enabled = true;
    }

    fn handle_ble_disable(&mut self) {
        if !self.ble_enabled {
            debug!("BLE_DISABLE received (already disabled; retrying disable)");
        } else {
            info!("BLE_DISABLE received");
        }
        self.ble.disable();
        self.ble_enabled = false;
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

    /// ATT protocol overhead subtracted from the negotiated MTU to get the
    /// maximum ATT attribute value size (ATT_MTU − 3).
    const ATT_HEADER_BYTES: u16 = 3;

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
        fn send(&mut self, peer_mac: &[u8; MAC_SIZE], data: &[u8]) -> bool {
            if !self.peers.contains(peer_mac) {
                self.peers.push(*peer_mac);
            }
            self.sent.push((data.to_vec(), *peer_mac));
            true
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
        check_encryption_fallback_count: Cell<usize>,
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
                check_encryption_fallback_count: Cell::new(0),
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
        fn check_encryption_fallback(&self) {
            self.check_encryption_fallback_count
                .set(self.check_encryption_fallback_count.get() + 1);
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

    /// Validates: poll() calls check_encryption_fallback() exactly once per cycle.
    #[test]
    fn poll_calls_check_encryption_fallback() {
        let mut bridge = make_bridge_with_ble();
        assert_eq!(bridge.ble.check_encryption_fallback_count.get(), 0);
        bridge.poll();
        assert_eq!(bridge.ble.check_encryption_fallback_count.get(), 1);
        bridge.poll();
        assert_eq!(bridge.ble.check_encryption_fallback_count.get(), 2);
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

    // --- BLE pairing state machine flow tests (MD-0414/MD-0416/MD-0409/MD-0415) ---
    //
    // These tests inject realistic BLE event sequences into the bridge via
    // MockBle to verify end-to-end handling of pairing flows.  The actual
    // state machine (deferral, buffering, timeouts) lives in ble.rs; these
    // tests validate the bridge correctly processes the resulting events.

    /// Validates: MD-0416 AC1 — pairing accept event sequence at the bridge.
    ///
    /// Sequence: PairingConfirm → gateway reply(accept) → Connected.
    /// Verifies the bridge forwards the complete sequence as
    /// BLE_PAIRING_CONFIRM → BLE_PAIRING_CONFIRM_REPLY dispatch → BLE_CONNECTED.
    /// Note: deferral of Connected until operator approval is implemented in
    /// ble.rs and requires the real BLE state machine or ESP hardware to test.
    #[test]
    fn ble_pairing_accept_full_flow() {
        let mut bridge = make_bridge_with_ble();
        let peer = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66];

        // 1. BLE stack emits PairingConfirm.
        bridge
            .ble
            .inject_event(BleEvent::PairingConfirm { passkey: 123456 });
        bridge.poll();

        // Verify BLE_PAIRING_CONFIRM forwarded.
        let tx = bridge.usb.take_tx();
        let (msg, consumed) = decode_modem_frame(&tx).unwrap();
        assert_eq!(consumed, tx.len(), "only one message expected");
        match msg {
            ModemMessage::BlePairingConfirm(p) => assert_eq!(p.passkey, 123456),
            _ => panic!("expected BlePairingConfirm"),
        }

        // 2. Gateway sends BLE_PAIRING_CONFIRM_REPLY(accept).
        let reply = ModemMessage::BlePairingConfirmReply(BlePairingConfirmReply { accept: true });
        let frame = encode_modem_frame(&reply).unwrap();
        bridge.usb.inject(&frame);
        bridge.poll();
        assert_eq!(bridge.ble.pairing_replies, vec![true]);

        // 3. BLE stack emits Connected (deferred until operator accepted).
        bridge.ble.inject_event(BleEvent::Connected {
            peer_addr: peer,
            mtu: 247,
        });
        bridge.poll();

        let tx = bridge.usb.take_tx();
        let (msg, consumed) = decode_modem_frame(&tx).unwrap();
        assert_eq!(consumed, tx.len(), "only one message expected");
        match msg {
            ModemMessage::BleConnected(c) => {
                assert_eq!(c.peer_addr, peer);
                assert_eq!(c.mtu, 247);
            }
            _ => panic!("expected BleConnected"),
        }
    }

    /// Validates: MD-0416 AC4 — pairing reject flow.
    ///
    /// Sequence: PairingConfirm → gateway reply(reject) → Disconnected.
    /// After the operator rejects, the BLE stack disconnects the client.
    /// Verify no BLE_CONNECTED is ever sent.
    #[test]
    fn ble_pairing_reject_full_flow() {
        let mut bridge = make_bridge_with_ble();
        let peer = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66];

        // 1. BLE stack emits PairingConfirm.
        bridge
            .ble
            .inject_event(BleEvent::PairingConfirm { passkey: 654321 });
        bridge.poll();
        let tx = bridge.usb.take_tx();
        let (msg, _) = decode_modem_frame(&tx).unwrap();
        assert!(
            matches!(msg, ModemMessage::BlePairingConfirm(_)),
            "expected BlePairingConfirm"
        );

        // 2. Gateway sends BLE_PAIRING_CONFIRM_REPLY(reject).
        let reply = ModemMessage::BlePairingConfirmReply(BlePairingConfirmReply { accept: false });
        let frame = encode_modem_frame(&reply).unwrap();
        bridge.usb.inject(&frame);
        bridge.poll();
        assert_eq!(bridge.ble.pairing_replies, vec![false]);

        // 3. BLE stack disconnects the client.
        bridge.ble.inject_event(BleEvent::Disconnected {
            peer_addr: peer,
            reason: 0x13,
        });
        bridge.poll();

        let tx = bridge.usb.take_tx();
        let (msg, consumed) = decode_modem_frame(&tx).unwrap();
        assert_eq!(consumed, tx.len(), "only BleDisconnected expected");
        match msg {
            ModemMessage::BleDisconnected(d) => {
                assert_eq!(d.peer_addr, peer);
            }
            _ => panic!("expected BleDisconnected"),
        }
    }

    /// Validates: MD-0409 AC5 — bridge forwards buffered Recv before Connected.
    ///
    /// When ble.rs flushes a buffered pre-auth GATT write, it emits Recv
    /// immediately before Connected.  Verify the bridge preserves this
    /// ordering: BLE_RECV is forwarded before BLE_CONNECTED.
    /// Note: the actual buffering (suppressing Recv until auth) is implemented
    /// in ble.rs; this test validates bridge ordering of the resulting events.
    #[test]
    fn ble_pre_auth_gatt_write_buffered() {
        let mut bridge = make_bridge_with_ble();
        let peer = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
        let gatt_data = vec![0x01, 0x00, 0x03, 0xCA, 0xFE];

        // 1. PairingConfirm arrives.
        bridge
            .ble
            .inject_event(BleEvent::PairingConfirm { passkey: 111111 });
        bridge.poll();
        bridge.usb.take_tx(); // discard PairingConfirm serial msg

        // 2. Gateway accepts.
        let reply = ModemMessage::BlePairingConfirmReply(BlePairingConfirmReply { accept: true });
        let frame = encode_modem_frame(&reply).unwrap();
        bridge.usb.inject(&frame);
        bridge.poll();
        bridge.usb.take_tx(); // discard any output

        // 3. BLE stack emits the buffered GATT write (Recv) followed by Connected
        //    — this is the order ble.rs produces when flushing a pending_write.
        bridge.ble.inject_event(BleEvent::Recv(gatt_data.clone()));
        bridge.ble.inject_event(BleEvent::Connected {
            peer_addr: peer,
            mtu: 247,
        });
        bridge.poll();

        // Decode both messages from the TX buffer.
        let tx = bridge.usb.take_tx();
        let (msg1, consumed1) = decode_modem_frame(&tx).unwrap();
        let (msg2, consumed2) = decode_modem_frame(&tx[consumed1..]).unwrap();
        assert_eq!(
            consumed1 + consumed2,
            tx.len(),
            "exactly two messages expected in TX buffer"
        );

        // BLE_RECV must arrive before BLE_CONNECTED (buffered write flushed first).
        match msg1 {
            ModemMessage::BleRecv(r) => assert_eq!(r.ble_data, gatt_data),
            _ => panic!("expected BleRecv first"),
        }
        match msg2 {
            ModemMessage::BleConnected(c) => {
                assert_eq!(c.peer_addr, peer);
                assert_eq!(c.mtu, 247);
            }
            _ => panic!("expected BleConnected second"),
        }
    }

    /// Validates: MD-0414 AC4 / T-0622 — pairing confirm without reply.
    ///
    /// After BLE_PAIRING_CONFIRM is forwarded and no reply is sent by the
    /// bridge, repeated poll cycles must not auto-send a pairing reply.
    /// If the BLE layer later emits BLE_DISCONNECTED, the bridge must
    /// forward it and must not emit BLE_CONNECTED.
    #[test]
    fn ble_pairing_confirm_without_reply_forwards_later_disconnect() {
        let mut bridge = make_bridge_with_ble();
        let peer = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66];

        // 1. PairingConfirm arrives.
        bridge
            .ble
            .inject_event(BleEvent::PairingConfirm { passkey: 999999 });
        bridge.poll();
        let tx = bridge.usb.take_tx();
        assert!(
            matches!(
                decode_modem_frame(&tx).unwrap().0,
                ModemMessage::BlePairingConfirm(_)
            ),
            "PairingConfirm must be forwarded"
        );

        // 2. No reply sent.  Multiple polls pass (simulating time passing).
        for _ in 0..5 {
            bridge.poll();
        }
        // No pairing reply should have been sent by the bridge.
        assert!(
            bridge.ble.pairing_replies.is_empty(),
            "bridge must not auto-reply"
        );
        // check_pairing_timeout must have been called exactly once per poll cycle.
        assert_eq!(bridge.ble.check_pairing_timeout_count.get(), 6);

        // 3. BLE stack eventually emits Disconnected (timeout triggered in ble.rs).
        bridge.ble.inject_event(BleEvent::Disconnected {
            peer_addr: peer,
            reason: 0x13,
        });
        bridge.poll();

        let tx = bridge.usb.take_tx();
        let (msg, consumed) = decode_modem_frame(&tx).unwrap();
        match msg {
            ModemMessage::BleDisconnected(d) => {
                assert_eq!(d.peer_addr, peer);
            }
            _ => panic!("expected BleDisconnected"),
        }
        // No BLE_CONNECTED should have been sent at any point.
        assert_eq!(
            consumed,
            tx.len(),
            "no additional messages expected after disconnect"
        );
    }

    /// Validates: MD-0415 — bridge stays silent while BLE is idle and forwards disconnects.
    ///
    /// Repeated poll cycles with no BLE events produce no serial output.
    /// When the BLE layer later emits `Disconnected`, verify the bridge
    /// forwards `BLE_DISCONNECTED`.
    #[test]
    fn ble_idle_timeout_disconnects() {
        let mut bridge = make_bridge_with_ble();
        let peer = [0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x01];

        // 1. Multiple polls with no BLE events (client connected but idle
        //    at the BLE stack level — no events emitted to bridge).
        for _ in 0..10 {
            bridge.poll();
        }
        // check_pairing_timeout called exactly once per cycle.
        assert_eq!(bridge.ble.check_pairing_timeout_count.get(), 10);
        // No serial output during idle period.
        assert!(bridge.usb.take_tx().is_empty());

        // 2. BLE stack disconnects the idle client.
        bridge.ble.inject_event(BleEvent::Disconnected {
            peer_addr: peer,
            reason: 0x08, // connection timeout
        });
        bridge.poll();

        let tx = bridge.usb.take_tx();
        let (msg, consumed) = decode_modem_frame(&tx).unwrap();
        assert_eq!(consumed, tx.len(), "only BleDisconnected expected");
        match msg {
            ModemMessage::BleDisconnected(d) => {
                assert_eq!(d.peer_addr, peer);
                assert_eq!(d.reason, 0x08);
            }
            _ => panic!("expected BleDisconnected"),
        }
    }

    /// Validates: MD-0416 AC1/AC2 combined — deferred Connected with buffered write.
    ///
    /// Full flow: PairingConfirm → pre-auth GATT write → operator accept →
    /// buffered Recv + Connected emitted.  Tests the complete MD-0416/MD-0409
    /// interaction at the bridge level.
    #[test]
    fn ble_deferred_connected_with_buffered_write() {
        let mut bridge = make_bridge_with_ble();
        let peer = [0x42, 0x42, 0x42, 0x42, 0x42, 0x42];
        let gatt_payload = vec![0xDE, 0xAD, 0xBE, 0xEF];

        // 1. PairingConfirm arrives → forwarded.
        bridge
            .ble
            .inject_event(BleEvent::PairingConfirm { passkey: 222333 });
        bridge.poll();
        let tx = bridge.usb.take_tx();
        let (msg, _) = decode_modem_frame(&tx).unwrap();
        match msg {
            ModemMessage::BlePairingConfirm(p) => assert_eq!(p.passkey, 222333),
            _ => panic!("expected BlePairingConfirm"),
        }

        // 2. No Connected yet (deferred in ble.rs).
        bridge.poll();
        assert!(
            bridge.usb.take_tx().is_empty(),
            "no serial output before operator decision"
        );

        // 3. Operator accepts.
        let reply = ModemMessage::BlePairingConfirmReply(BlePairingConfirmReply { accept: true });
        bridge.usb.inject(&encode_modem_frame(&reply).unwrap());
        bridge.poll();
        assert_eq!(bridge.ble.pairing_replies, vec![true]);
        bridge.usb.take_tx(); // discard

        // 4. ble.rs flushes buffered write + Connected.
        bridge
            .ble
            .inject_event(BleEvent::Recv(gatt_payload.clone()));
        bridge.ble.inject_event(BleEvent::Connected {
            peer_addr: peer,
            mtu: 512,
        });
        bridge.poll();

        let tx = bridge.usb.take_tx();
        let (msg1, consumed1) = decode_modem_frame(&tx).unwrap();
        let (msg2, consumed2) = decode_modem_frame(&tx[consumed1..]).unwrap();
        assert_eq!(
            consumed1 + consumed2,
            tx.len(),
            "exactly two messages expected in TX buffer"
        );

        match msg1 {
            ModemMessage::BleRecv(r) => assert_eq!(r.ble_data, gatt_payload),
            _ => panic!("expected BleRecv"),
        }
        match msg2 {
            ModemMessage::BleConnected(c) => {
                assert_eq!(c.peer_addr, peer);
                assert_eq!(c.mtu, 512);
            }
            _ => panic!("expected BleConnected"),
        }
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

    // ---------------------------------------------------------------
    // GAP 1: MD-0403 AC3 — Indication fragmentation flow control
    // ---------------------------------------------------------------

    // Maximum queued indication chunks (mirrors `ble.rs::MAX_INDICATION_CHUNKS`).
    const MOCK_MAX_INDICATION_CHUNKS: usize = 64;

    /// Mock BLE driver that simulates ATT indication fragmentation and
    /// flow control, mirroring the real `EspBleDriver` pacing logic.
    ///
    /// Key behaviours:
    /// - `indicate()` fragments into chunks of (MTU − 3) bytes, enqueues
    ///   them, and sends the next chunk immediately (setting
    ///   `awaiting_confirm`) **only** when not already awaiting a confirm
    ///   and there were no pending chunks in the indication queue.
    /// - `advance_indication()` sends the next queued chunk **only** when
    ///   `awaiting_confirm` is false (i.e. the ATT confirmation arrived).
    /// - `simulate_confirm()` clears the flag, as `on_notify_tx` would.
    struct FragmentingMockBle {
        mtu: u16,
        indication_queue: RefCell<VecDeque<Vec<u8>>>,
        awaiting_confirm: Cell<bool>,
        chunks_sent: RefCell<Vec<Vec<u8>>>,
        event_queue: RefCell<VecDeque<BleEvent>>,
        #[allow(dead_code)]
        enabled: bool,
        #[allow(dead_code)]
        pairing_replies: Vec<bool>,
    }

    impl FragmentingMockBle {
        fn new(mtu: u16) -> Self {
            Self {
                mtu,
                indication_queue: RefCell::new(VecDeque::new()),
                awaiting_confirm: Cell::new(false),
                chunks_sent: RefCell::new(Vec::new()),
                event_queue: RefCell::new(VecDeque::new()),
                enabled: false,
                pairing_replies: Vec::new(),
            }
        }

        /// Simulate ATT Handle Value Confirmation (on_notify_tx success).
        fn simulate_confirm(&self) {
            self.awaiting_confirm.set(false);
        }

        fn chunks_sent(&self) -> std::cell::Ref<'_, Vec<Vec<u8>>> {
            self.chunks_sent.borrow()
        }

        fn is_awaiting_confirm(&self) -> bool {
            self.awaiting_confirm.get()
        }

        fn pending_chunks(&self) -> usize {
            self.indication_queue.borrow().len()
        }

        fn send_next_chunk(&self) {
            if let Some(chunk) = self.indication_queue.borrow_mut().pop_front() {
                self.chunks_sent.borrow_mut().push(chunk);
                self.awaiting_confirm.set(true);
            }
        }
    }

    impl Ble for FragmentingMockBle {
        fn enable(&mut self) {
            self.enabled = true;
        }
        fn disable(&mut self) {
            self.enabled = false;
            self.indication_queue.borrow_mut().clear();
            self.chunks_sent.borrow_mut().clear();
            self.event_queue.borrow_mut().clear();
            self.awaiting_confirm.set(false);
        }
        fn indicate(&mut self, data: &[u8]) {
            if data.is_empty() || self.mtu == 0 {
                return;
            }
            let chunk_size = (self.mtu.saturating_sub(ATT_HEADER_BYTES)) as usize;
            if chunk_size == 0 {
                return;
            }
            let num_chunks = data.len().div_ceil(chunk_size);
            let was_empty = self.indication_queue.borrow().is_empty();
            {
                let mut queue = self.indication_queue.borrow_mut();
                if queue.len() + num_chunks > MOCK_MAX_INDICATION_CHUNKS {
                    return; // Drop the payload to mirror EspBleDriver's queue-full behavior;
                            // the production warning log is intentionally not simulated here.
                }
                for chunk in data.chunks(chunk_size) {
                    queue.push_back(chunk.to_vec());
                }
            }
            if !self.awaiting_confirm.get() && was_empty {
                // Send the first chunk immediately (mirrors EspBleDriver)
                // while preserving the confirmation-gated pacing model.
                self.send_next_chunk();
            }
        }
        fn pairing_confirm_reply(&mut self, accept: bool) {
            self.pairing_replies.push(accept);
        }
        fn advance_indication(&self) {
            if !self.awaiting_confirm.get() && !self.indication_queue.borrow().is_empty() {
                self.send_next_chunk();
            }
        }
        fn drain_event(&self) -> Option<BleEvent> {
            self.event_queue.borrow_mut().pop_front()
        }
    }

    fn make_bridge_with_fragmenting_ble(
        mtu: u16,
    ) -> Bridge<MockSerial, MockRadio, FragmentingMockBle> {
        Bridge::with_ble(
            MockSerial::new(),
            MockRadio::new(),
            FragmentingMockBle::new(mtu),
            ModemCounters::new(),
        )
    }

    /// Feed `data` into the bridge serial port and poll until fully consumed.
    fn feed_and_drain<R: Radio, B: Ble>(bridge: &mut Bridge<MockSerial, R, B>, data: &[u8]) {
        bridge.usb.inject(data);
        let buf_len = bridge.rx_buf.len();
        for _ in 0..(data.len() / buf_len + 2) {
            bridge.poll();
        }
    }

    /// Validates: T-0605 / MD-0403 AC3 — flow control between indication
    /// chunks.
    ///
    /// Sends a payload that spans 3 chunks. Asserts that only one chunk is
    /// sent per confirmation cycle and that no chunk is sent while
    /// `awaiting_confirm` is true.
    #[test]
    fn t0605_indication_flow_control_awaits_confirm() {
        let mtu = 247u16;
        // Payload that spans 3 chunks (244 + 244 + 12 = 500 bytes).
        // Must stay ≤ 511 bytes (serial frame body limit).
        let payload: Vec<u8> = (0u16..500).map(|i| (i & 0xFF) as u8).collect();

        let mut bridge = make_bridge_with_fragmenting_ble(mtu);

        let frame = encode_modem_frame(&ModemMessage::BleIndicate(BleIndicate {
            ble_data: payload.clone(),
        }))
        .unwrap();
        feed_and_drain(&mut bridge, &frame);

        // After indicate(), first chunk is sent, awaiting confirm.
        assert_eq!(bridge.ble.chunks_sent().len(), 1, "only 1st chunk sent");
        assert!(
            bridge.ble.is_awaiting_confirm(),
            "must be awaiting confirm after 1st chunk"
        );

        // Poll again without confirming — no new chunk.
        bridge.poll();
        assert_eq!(
            bridge.ble.chunks_sent().len(),
            1,
            "no advance without confirm"
        );

        // Simulate ATT confirmation and poll — 2nd chunk sent.
        bridge.ble.simulate_confirm();
        bridge.poll();
        assert_eq!(bridge.ble.chunks_sent().len(), 2, "2nd chunk after confirm");
        assert!(bridge.ble.is_awaiting_confirm());

        // Simulate confirm and poll — 3rd (last) chunk sent.
        bridge.ble.simulate_confirm();
        bridge.poll();
        assert_eq!(bridge.ble.chunks_sent().len(), 3, "3rd chunk after confirm");
        assert!(bridge.ble.is_awaiting_confirm());

        // Final confirm — queue is drained.
        bridge.ble.simulate_confirm();
        bridge.poll();
        assert_eq!(bridge.ble.pending_chunks(), 0, "queue fully drained");
        assert!(!bridge.ble.is_awaiting_confirm());

        // Reassembled chunks must match original payload.
        let reassembled: Vec<u8> = bridge.ble.chunks_sent().iter().flatten().copied().collect();
        assert_eq!(reassembled, payload, "reassembly must match original");
    }

    /// Validates: MD-0403 AC2 — each indication chunk ≤ (MTU − 3) bytes.
    #[test]
    fn t0605_indication_chunks_within_mtu_limit() {
        let mtu = 247u16;
        let chunk_size = mtu.saturating_sub(ATT_HEADER_BYTES) as usize;
        // 500 bytes → 3 chunks (244 + 244 + 12).
        let payload: Vec<u8> = vec![0xAB; 500];

        let mut bridge = make_bridge_with_fragmenting_ble(mtu);
        let frame = encode_modem_frame(&ModemMessage::BleIndicate(BleIndicate {
            ble_data: payload,
        }))
        .unwrap();
        feed_and_drain(&mut bridge, &frame);

        // Drain all chunks by simulating confirmations.
        while bridge.ble.pending_chunks() > 0 || bridge.ble.is_awaiting_confirm() {
            bridge.ble.simulate_confirm();
            bridge.poll();
        }

        for (i, chunk) in bridge.ble.chunks_sent().iter().enumerate() {
            assert!(
                chunk.len() <= chunk_size,
                "chunk {} is {} bytes, exceeds MTU-3 = {}",
                i,
                chunk.len(),
                chunk_size
            );
        }
    }

    /// Validates: MD-0403 — single-chunk payload needs no fragmentation.
    #[test]
    fn t0605_indication_single_chunk_no_fragmentation() {
        let mtu = 247u16;
        let chunk_size = mtu.saturating_sub(ATT_HEADER_BYTES) as usize;
        let payload: Vec<u8> = vec![0x42; chunk_size - 10]; // well under ATT value max

        let mut bridge = make_bridge_with_fragmenting_ble(mtu);
        let frame = encode_modem_frame(&ModemMessage::BleIndicate(BleIndicate {
            ble_data: payload.clone(),
        }))
        .unwrap();
        feed_and_drain(&mut bridge, &frame);

        assert_eq!(bridge.ble.chunks_sent().len(), 1, "no fragmentation needed");
        assert_eq!(bridge.ble.chunks_sent()[0], payload);
        assert_eq!(bridge.ble.pending_chunks(), 0, "queue empty");
    }

    /// Validates: MD-0403 — payload exactly (MTU − 3) bytes: single chunk.
    #[test]
    fn t0605_indication_exact_mtu_boundary() {
        let mtu = 247u16;
        let chunk_size = mtu.saturating_sub(ATT_HEADER_BYTES) as usize;
        let payload: Vec<u8> = vec![0x42; chunk_size]; // exactly ATT value max

        let mut bridge = make_bridge_with_fragmenting_ble(mtu);
        let frame = encode_modem_frame(&ModemMessage::BleIndicate(BleIndicate {
            ble_data: payload.clone(),
        }))
        .unwrap();
        feed_and_drain(&mut bridge, &frame);

        assert_eq!(bridge.ble.chunks_sent().len(), 1, "exact fit = 1 chunk");
        assert_eq!(bridge.ble.chunks_sent()[0], payload);
    }

    /// Validates: MD-0403 — `advance_indication()` is a no-op while
    /// `awaiting_confirm` is true, even with queued chunks.
    #[test]
    fn t0605_advance_indication_blocked_while_awaiting_confirm() {
        let mtu = 247u16;
        // 500 bytes → 3 chunks (244 + 244 + 12), within serial frame limit.
        let payload: Vec<u8> = vec![0x42; 500];

        let mut bridge = make_bridge_with_fragmenting_ble(mtu);
        let frame = encode_modem_frame(&ModemMessage::BleIndicate(BleIndicate {
            ble_data: payload,
        }))
        .unwrap();
        feed_and_drain(&mut bridge, &frame);

        // First chunk sent, 2 remaining.
        assert_eq!(bridge.ble.chunks_sent().len(), 1);
        assert!(bridge.ble.is_awaiting_confirm());
        assert_eq!(bridge.ble.pending_chunks(), 2);

        // 10 polls without confirm — still only 1 chunk sent.
        for _ in 0..10 {
            bridge.poll();
        }
        assert_eq!(
            bridge.ble.chunks_sent().len(),
            1,
            "must not advance without confirm"
        );
        assert_eq!(bridge.ble.pending_chunks(), 2, "queue unchanged");
    }

    /// Validates: MD-0403 — `indicate()` with empty data is silently
    /// discarded (defense-in-depth via FragmentingMockBle).
    #[test]
    fn t0605_fragmenting_ble_empty_indicate_discarded() {
        let mut bridge = make_bridge_with_fragmenting_ble(247);
        bridge.ble.indicate(&[]);
        assert!(bridge.ble.chunks_sent().is_empty());
        assert!(!bridge.ble.is_awaiting_confirm());
    }

    /// Validates: MD-0403 AC5 — indication fragment queue bounded at 64 chunks.
    ///
    /// Sends a payload that would produce 65 chunks (exceeding the 64-chunk
    /// limit).  The indication MUST be silently dropped — no chunks sent.
    #[test]
    fn t0605_indication_queue_overflow_rejected() {
        // MTU 247 → chunk_size = 244. 65 chunks × 244 bytes = 15,860 bytes.
        let mut bridge = make_bridge_with_fragmenting_ble(247);
        let chunk_size = (247 - ATT_HEADER_BYTES) as usize; // 244
        let payload_65_chunks = vec![0x42u8; chunk_size * 65];
        bridge.ble.indicate(&payload_65_chunks);
        // All 65 chunks should be rejected (exceeds MOCK_MAX_INDICATION_CHUNKS=64).
        assert!(
            bridge.ble.chunks_sent().is_empty(),
            "indication exceeding 64-chunk limit must be silently dropped"
        );
        assert_eq!(bridge.ble.pending_chunks(), 0);
    }

    /// Validates: MD-0403 AC5 — indication exactly at 64-chunk limit accepted.
    #[test]
    fn t0605_indication_queue_at_boundary_accepted() {
        let mut bridge = make_bridge_with_fragmenting_ble(247);
        let chunk_size = (247 - ATT_HEADER_BYTES) as usize; // 244
        let payload_64_chunks = vec![0x42u8; chunk_size * 64];
        bridge.ble.indicate(&payload_64_chunks);
        // Exactly 64 chunks — first chunk sent immediately, 63 queued.
        assert_eq!(bridge.ble.chunks_sent().len(), 1, "first chunk sent");
        assert_eq!(bridge.ble.pending_chunks(), 63, "remaining 63 queued");
    }

    // ---------------------------------------------------------------
    // GAP 2: MD-0409 AC2 — Write Long reassembly
    // ---------------------------------------------------------------

    /// Validates: T-0613 / MD-0409 AC2 — large BLE payload forwarded as
    /// a single `BLE_RECV`.
    ///
    /// NimBLE reassembles Prepare Write + Execute Write before invoking
    /// `on_write`, so the bridge receives a single large `BleEvent::Recv`.
    /// This test verifies the *bridge* forwarding path: it injects a
    /// pre-reassembled payload larger than (MTU − 3) = 244 bytes and
    /// asserts it arrives as one `BLE_RECV` serial message. The BLE stack
    /// reassembly itself is a NimBLE responsibility and is not exercised
    /// here.
    #[test]
    fn t0613_write_long_reassembled_as_single_ble_recv() {
        let mut bridge = make_bridge_with_ble();
        // 500 bytes exceeds MTU-3 = 244, which would trigger Write Long
        // on a real BLE stack. NimBLE reassembles before on_write fires.
        let large_write: Vec<u8> = (0u16..500).map(|i| (i & 0xFF) as u8).collect();
        bridge.ble.inject_event(BleEvent::Recv(large_write.clone()));
        bridge.poll();

        let tx = bridge.usb.take_tx();
        let (msg, _) = decode_modem_frame(&tx).unwrap();
        match msg {
            ModemMessage::BleRecv(r) => {
                assert_eq!(
                    r.ble_data, large_write,
                    "Write Long payload must arrive as single BLE_RECV"
                );
            }
            _ => panic!("expected BleRecv"),
        }
    }

    /// Validates: T-0613 / MD-0409 AC3 — payload forwarded unmodified.
    ///
    /// Sends a binary payload with all 256 byte values and verifies
    /// bit-exact forwarding through the bridge.
    #[test]
    fn t0613_ble_recv_payload_forwarded_unmodified() {
        let mut bridge = make_bridge_with_ble();
        let all_bytes: Vec<u8> = (0u16..256).map(|i| i as u8).collect();
        bridge.ble.inject_event(BleEvent::Recv(all_bytes.clone()));
        bridge.poll();

        let tx = bridge.usb.take_tx();
        let (msg, _) = decode_modem_frame(&tx).unwrap();
        match msg {
            ModemMessage::BleRecv(r) => {
                assert_eq!(r.ble_data, all_bytes, "payload must be bit-exact");
            }
            _ => panic!("expected BleRecv"),
        }
    }

    /// Validates: T-0613b / MD-0409 AC4 — empty BLE data handling.
    ///
    /// Part 1: Verifies the bridge does not emit a BLE_RECV when no BLE
    /// event is queued (baseline sanity check — no event means no output).
    ///
    /// Part 2: Injects an explicit empty `BleEvent::Recv` into the bridge
    /// and verifies the bridge handles it gracefully (defense-in-depth).
    /// The discard responsibility for empty GATT writes lies in the BLE
    /// driver layer, not the bridge; this test validates bridge resilience.
    #[test]
    fn t0613b_empty_gatt_write_no_ble_recv() {
        // Part 1: Baseline sanity — no BLE event queued means no BLE_RECV
        // output. This does not exercise empty-write discard logic (which is
        // a BLE driver responsibility), only verifies the bridge stays quiet.
        let mut bridge = make_bridge_with_fragmenting_ble(247);
        bridge.poll();
        assert!(
            bridge.usb.take_tx().is_empty(),
            "no BLE_RECV when BLE layer emits nothing"
        );

        // Part 2: Inject an explicit empty BleEvent::Recv to verify the
        // bridge handles it gracefully (defense-in-depth).
        let mut bridge2 = make_bridge_with_ble();
        bridge2.ble.inject_event(BleEvent::Recv(vec![]));
        bridge2.poll();

        // The bridge forwards whatever the BLE layer produces. The codec may
        // accept or reject empty BLE_RECV; either way no crash. We assert
        // that if a frame is produced, it is a BLE_RECV with empty payload,
        // and that no other modem messages are synthesized.
        let tx2 = bridge2.usb.take_tx();
        let mut decoder2 = FrameDecoder::new();
        decoder2.push(&tx2);

        // The codec is allowed to reject empty BLE_RECV frames; for this
        // test, a decode error is treated as "no valid message produced".
        let decoded = decoder2.decode().unwrap_or_default();
        if let Some(msg) = decoded {
            match msg {
                ModemMessage::BleRecv(r) => {
                    assert!(
                        r.ble_data.is_empty(),
                        "expected empty ble_data for empty BleEvent::Recv"
                    );
                }
                other => {
                    panic!(
                        "unexpected modem message for empty BleEvent::Recv: {:?}",
                        other
                    );
                }
            }
        } else {
            // No frame forwarded is also acceptable: empty BLE_RECV dropped.
            assert!(
                tx2.is_empty(),
                "decoder returned None but serial buffer was not empty"
            );
        }
    }

    /// Validates: MD-0409 AC2 — multiple Write Long payloads arrive as
    /// separate `BLE_RECV` messages (no merging across writes).
    #[test]
    fn t0613_multiple_write_longs_separate_ble_recv() {
        let mut bridge = make_bridge_with_ble();
        let write1: Vec<u8> = vec![0xAA; 300];
        let write2: Vec<u8> = vec![0xBB; 400];
        bridge.ble.inject_event(BleEvent::Recv(write1.clone()));
        bridge.ble.inject_event(BleEvent::Recv(write2.clone()));
        bridge.poll();

        let tx = bridge.usb.take_tx();
        let mut decoder = FrameDecoder::new();
        decoder.push(&tx);

        let msg1 = decoder.decode().unwrap().expect("first BLE_RECV");
        let msg2 = decoder.decode().unwrap().expect("second BLE_RECV");

        match (msg1, msg2) {
            (ModemMessage::BleRecv(r1), ModemMessage::BleRecv(r2)) => {
                assert_eq!(r1.ble_data, write1, "first Write Long intact");
                assert_eq!(r2.ble_data, write2, "second Write Long intact");
            }
            _ => panic!("expected two BleRecv messages"),
        }
    }

    // ---------------------------------------------------------------
    // GAP 3: USB TX backpressure / ring buffer overflow
    // ---------------------------------------------------------------

    /// Mock serial port that can simulate write failures (TX buffer full).
    struct BackpressureSerial {
        rx_data: Vec<u8>,
        tx_data: Vec<u8>,
        connected: bool,
        reconnect_once: bool,
        /// When true, `write()` returns false (simulating full TX buffer).
        reject_writes: bool,
        write_attempt_count: usize,
    }

    impl BackpressureSerial {
        fn new() -> Self {
            Self {
                rx_data: Vec::new(),
                tx_data: Vec::new(),
                connected: true,
                reconnect_once: false,
                reject_writes: false,
                write_attempt_count: 0,
            }
        }

        fn take_tx(&mut self) -> Vec<u8> {
            std::mem::take(&mut self.tx_data)
        }
    }

    impl SerialPort for BackpressureSerial {
        fn read(&mut self, buf: &mut [u8]) -> (usize, bool) {
            let reconnected = self.reconnect_once;
            self.reconnect_once = false;
            let n = std::cmp::min(buf.len(), self.rx_data.len());
            buf[..n].copy_from_slice(&self.rx_data[..n]);
            self.rx_data.drain(..n);
            (n, reconnected)
        }
        fn write(&mut self, data: &[u8]) -> bool {
            self.write_attempt_count += 1;
            if self.reject_writes {
                return false;
            }
            self.tx_data.extend_from_slice(data);
            true
        }
        fn is_connected(&self) -> bool {
            self.connected
        }
    }

    /// Validates: Design §4.3 — radio frame flood with full TX buffer.
    ///
    /// Injects radio frames while the USB serial rejects writes. The
    /// bridge must not crash, block, or panic. Frames are silently
    /// dropped (rx_count is NOT incremented).
    #[test]
    fn t0403_tx_backpressure_drops_frames_no_crash() {
        let usb = BackpressureSerial::new();
        let mut bridge: Bridge<BackpressureSerial, MockRadio> =
            Bridge::new(usb, MockRadio::new(), ModemCounters::new());

        let peer = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66];

        // Simulate TX buffer full.
        bridge.usb.reject_writes = true;

        // Flood with radio frames.
        for i in 0u8..32 {
            bridge.radio.inject_rx(RecvFrame {
                peer_mac: peer,
                rssi: -40,
                frame_data: vec![i],
            });
        }

        // Poll twice — should not crash or block.
        bridge.poll();
        bridge.poll();

        // All frames consumed from radio queue.
        assert!(
            bridge.radio.drain_one().is_none(),
            "radio queue must be drained"
        );

        // No frames forwarded (writes rejected).
        assert_eq!(
            bridge.counters.rx_count(),
            0,
            "rx_count must not increment on write failure"
        );

        // Write attempts were made (bridge tried to forward).
        assert!(
            bridge.usb.write_attempt_count > 0,
            "bridge must attempt writes"
        );
    }

    /// Validates: Design §4.3 — bridge recovers after TX backpressure
    /// clears.
    ///
    /// After a period of write failures, writes succeed again and frames
    /// are forwarded normally.
    #[test]
    fn t0403_tx_backpressure_recovery() {
        let usb = BackpressureSerial::new();
        let mut bridge: Bridge<BackpressureSerial, MockRadio> =
            Bridge::new(usb, MockRadio::new(), ModemCounters::new());

        let peer = [1, 2, 3, 4, 5, 6];

        // Phase 1: Reject writes.
        bridge.usb.reject_writes = true;
        for i in 0u8..5 {
            bridge.radio.inject_rx(RecvFrame {
                peer_mac: peer,
                rssi: -50,
                frame_data: vec![i],
            });
        }
        bridge.poll();
        assert_eq!(bridge.counters.rx_count(), 0, "all dropped under pressure");
        bridge.usb.take_tx(); // discard (empty)

        // Phase 2: Restore writes.
        bridge.usb.reject_writes = false;
        for i in 10u8..13 {
            bridge.radio.inject_rx(RecvFrame {
                peer_mac: peer,
                rssi: -55,
                frame_data: vec![i],
            });
        }
        bridge.poll();
        assert_eq!(
            bridge.counters.rx_count(),
            3,
            "3 frames forwarded after recovery"
        );

        // Verify forwarded frames are decodable.
        let tx = bridge.usb.take_tx();
        let mut decoder = FrameDecoder::new();
        decoder.push(&tx);
        let mut count = 0;
        while let Ok(Some(msg)) = decoder.decode() {
            assert!(matches!(msg, ModemMessage::RecvFrame(_)));
            count += 1;
        }
        assert_eq!(count, 3);
    }

    /// Validates: Design §4.3 — BLE events also tolerate TX
    /// backpressure without crash.
    #[test]
    fn t0403_tx_backpressure_ble_events_no_crash() {
        let usb = BackpressureSerial::new();
        let mut bridge: Bridge<BackpressureSerial, MockRadio, MockBle> =
            Bridge::with_ble(usb, MockRadio::new(), MockBle::new(), ModemCounters::new());

        bridge.usb.reject_writes = true;

        // Inject BLE events while TX is blocked.
        let peer = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
        bridge.ble.inject_event(BleEvent::Recv(vec![0x01, 0x02]));
        bridge.ble.inject_event(BleEvent::Connected {
            peer_addr: peer,
            mtu: 247,
        });
        bridge.ble.inject_event(BleEvent::Disconnected {
            peer_addr: peer,
            reason: 0x13,
        });

        // Must not crash or block.
        bridge.poll();
        assert!(bridge.usb.take_tx().is_empty(), "writes rejected");

        // Restore writes and verify bridge still works.
        bridge.usb.reject_writes = false;
        bridge
            .ble
            .inject_event(BleEvent::PairingConfirm { passkey: 42 });
        bridge.poll();
        let tx = bridge.usb.take_tx();
        let (msg, _) = decode_modem_frame(&tx).unwrap();
        assert!(
            matches!(msg, ModemMessage::BlePairingConfirm(_)),
            "bridge must recover after backpressure clears"
        );
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

    /// Validates: MD-0413 AC3 (BLE_ENABLE idempotent — duplicate is safe)
    ///
    /// Sending BLE_ENABLE when already enabled must not disrupt an active
    /// connection. The bridge always forwards the call to the BLE driver
    /// (retrying enable) so that transient start failures can be recovered.
    /// Asserts only externally observable behavior: no disconnect event,
    /// BLE stays enabled, indication works.
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

        // Bridge always retries enable to recover from transient failures.
        assert_eq!(
            bridge.ble.enable_count.get(),
            2,
            "duplicate BLE_ENABLE should retry Ble::enable()"
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

    /// Validates: MD-0413 AC4 (BLE_DISABLE idempotent — duplicate is safe)
    ///
    /// Sending BLE_DISABLE when already disabled must not crash or
    /// produce unexpected output. The bridge always forwards the call to
    /// the BLE driver (retrying disable) so transient failures are
    /// recovered. Asserts only externally observable behavior: no serial
    /// output, BLE stays disabled, bridge operational.
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
        // Bridge always retries disable to recover from transient failures.
        // The initial construction disable + 2 explicit calls = 3 total.
        assert_eq!(
            bridge.ble.disable_count.get(),
            3,
            "duplicate BLE_DISABLE should retry Ble::disable()"
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
        let mut remaining = tx.as_slice();
        let mut scan_results = Vec::new();
        while !remaining.is_empty() {
            let (msg, consumed) =
                decode_modem_frame(remaining).expect("failed to decode frame from tx buffer");
            if let ModemMessage::ScanResult(_) = &msg {
                scan_results.push(msg);
            }
            remaining = &remaining[consumed..];
        }
        assert!(!scan_results.is_empty(), "expected at least one ScanResult");

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
                // Compute expected uptime based on current time and boot time,
                // and allow a small tolerance to avoid flakiness on slow CI.
                let expected = Instant::now().duration_since(boot).as_secs();
                let lower = expected.saturating_sub(1);
                let upper = expected + 1;
                let uptime = u64::from(s.uptime_s);
                assert!(
                    (lower..=upper).contains(&uptime),
                    "uptime_s should be close to {}s (±1s), got {}",
                    expected,
                    uptime
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
        loop {
            match decoder.decode() {
                Ok(Some(msg)) => match msg {
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
                },
                Ok(None) => break,
                Err(e) => panic!("failed to decode modem frame in burst output: {:?}", e),
            }
        }
        assert_eq!(
            recv_count, total,
            "exactly one RECV_FRAME per injected frame (got {}, expected {})",
            recv_count, total
        );
        assert_eq!(bridge.counters.rx_count(), total as u32);
    }

    // -----------------------------------------------------------------------
    // Button scanner tests (T-0801 through T-0810, host-side)
    // -----------------------------------------------------------------------

    use std::cell::Cell as StdCell;
    use std::rc::Rc;

    /// Create a ButtonScanner with a controllable pressed state.
    fn make_test_scanner() -> (Rc<StdCell<bool>>, ButtonScanner<impl FnMut() -> bool>) {
        let pressed = Rc::new(StdCell::new(false));
        let pressed_clone = pressed.clone();
        let scanner = ButtonScanner::new(move || pressed_clone.get());
        (pressed, scanner)
    }

    /// Deterministic time offsets for button tests.
    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn button_short_press() {
        let (_, mut scanner) = make_test_scanner();
        let t0 = Instant::now();

        // Idle — not pressed.
        assert_eq!(scanner.poll_at(t0, false), None);

        // Press the button.
        assert_eq!(scanner.poll_at(t0, true), None); // DebouncePress

        // Before debounce completes — still nothing.
        assert_eq!(scanner.poll_at(t0 + MS * 20, true), None);

        // Past debounce (30 ms) — enters Pressed.
        assert_eq!(scanner.poll_at(t0 + MS * 30, true), None);

        // Release at 230 ms (200 ms hold).
        assert_eq!(scanner.poll_at(t0 + MS * 230, false), None); // DebounceRelease

        // Past release debounce — classify as SHORT.
        assert_eq!(
            scanner.poll_at(t0 + MS * 260, false),
            Some(BUTTON_TYPE_SHORT)
        );

        // Back to idle.
        assert_eq!(scanner.poll_at(t0 + MS * 300, false), None);
    }

    #[test]
    fn button_long_press() {
        let (_, mut scanner) = make_test_scanner();
        let t0 = Instant::now();

        // Press.
        scanner.poll_at(t0, true);
        scanner.poll_at(t0 + MS * 30, true); // Pressed

        // Release at 1200 ms (1170 ms hold).
        scanner.poll_at(t0 + MS * 1200, false); // DebounceRelease

        // Past release debounce.
        assert_eq!(
            scanner.poll_at(t0 + MS * 1230, false),
            Some(BUTTON_TYPE_LONG)
        );
    }

    #[test]
    fn button_glitch_rejected() {
        let (_, mut scanner) = make_test_scanner();
        let t0 = Instant::now();

        // Brief press shorter than debounce.
        scanner.poll_at(t0, true); // DebouncePress
                                   // Release before 30 ms.
        assert_eq!(scanner.poll_at(t0 + MS * 10, false), None); // back to Idle

        // No event.
        assert_eq!(scanner.poll_at(t0 + MS * 100, false), None);
    }

    #[test]
    fn button_no_event_while_held() {
        let (_, mut scanner) = make_test_scanner();
        let t0 = Instant::now();

        scanner.poll_at(t0, true);
        scanner.poll_at(t0 + MS * 30, true); // Pressed

        // Hold for 2 seconds — no event.
        for i in 1..=20 {
            assert_eq!(scanner.poll_at(t0 + MS * 30 + MS * 100 * i, true), None);
        }

        // Release at 2030 ms.
        scanner.poll_at(t0 + MS * 2030, false);
        assert_eq!(
            scanner.poll_at(t0 + MS * 2060, false),
            Some(BUTTON_TYPE_LONG)
        );
    }

    #[test]
    fn button_back_to_back_presses() {
        let (_, mut scanner) = make_test_scanner();
        let t0 = Instant::now();

        // First press — short (200 ms hold).
        scanner.poll_at(t0, true);
        scanner.poll_at(t0 + MS * 30, true); // Pressed
        scanner.poll_at(t0 + MS * 230, false); // DebounceRelease
        assert_eq!(
            scanner.poll_at(t0 + MS * 260, false),
            Some(BUTTON_TYPE_SHORT)
        );

        // Second press — long (1200 ms hold).
        scanner.poll_at(t0 + MS * 400, true);
        scanner.poll_at(t0 + MS * 430, true); // Pressed
        scanner.poll_at(t0 + MS * 1630, false); // DebounceRelease
        assert_eq!(
            scanner.poll_at(t0 + MS * 1660, false),
            Some(BUTTON_TYPE_LONG)
        );
    }

    #[test]
    fn button_boundary_999ms_short_1000ms_long() {
        // T-0803: exactly 999 ms → SHORT, exactly 1000 ms → LONG.
        let (_, mut scanner) = make_test_scanner();
        let t0 = Instant::now();

        // 999 ms press: debounced press at t0+30, release at t0+30+999=t0+1029,
        // debounced release at t0+1059. Duration = 1059 - 30 = 1029... no.
        // Duration is measured from press_start (set to `now` at t0+30) to
        // the classification `now` (at t0+30+999+30 = t0+1059).
        // duration = 1059 - 30 = 1029 ms. That's > 1000 ms. Need to be precise.
        //
        // press_start = t0+30 (when DebouncePress→Pressed transition fires).
        // For a 999 ms hold: release GPIO at t0+30+999 = t0+1029.
        // Debounced release fires at t0+1029+30 = t0+1059.
        // Duration at classification = t0+1059 - t0+30 = 1029 ms → LONG.
        // That's because debounce adds to the measured duration.
        //
        // The spec says duration is from debounced press to debounced release.
        // So 999 ms between those two events means:
        //   press_start = t0+30, classification_now = t0+30+999 = t0+1029.
        //   release GPIO must happen at t0+1029-30 = t0+999 (so release
        //   debounce completes at t0+1029).
        //
        // Simpler: press at t0, debounce at t0+30 (press_start=t0+30),
        // release at t0+30+969=t0+999, debounce release at t0+999+30=t0+1029.
        // classification now=t0+1029, duration=t0+1029 - t0+30 = 999 ms → SHORT. ✓

        // 999 ms between debounced press and debounced release → SHORT.
        scanner.poll_at(t0, true); // DebouncePress
        scanner.poll_at(t0 + MS * 30, true); // Pressed, press_start = t0+30
        scanner.poll_at(t0 + MS * 999, false); // DebounceRelease
        assert_eq!(
            scanner.poll_at(t0 + MS * 1029, false), // duration = 1029-30 = 999 ms
            Some(BUTTON_TYPE_SHORT)
        );

        // 1000 ms between debounced press and debounced release → LONG.
        scanner.poll_at(t0 + MS * 1100, true); // DebouncePress
        scanner.poll_at(t0 + MS * 1130, true); // Pressed, press_start = t0+1130
        scanner.poll_at(t0 + MS * 2100, false); // DebounceRelease
        assert_eq!(
            scanner.poll_at(t0 + MS * 2130, false), // duration = 2130-1130 = 1000 ms
            Some(BUTTON_TYPE_LONG)
        );
    }

    #[test]
    fn button_release_bounce_rejected() {
        // T-0809: bounce during release should not create a second event.
        let (_, mut scanner) = make_test_scanner();
        let t0 = Instant::now();

        // Press for 200 ms.
        scanner.poll_at(t0, true);
        scanner.poll_at(t0 + MS * 30, true); // Pressed

        // Release — but bounce back within debounce window.
        scanner.poll_at(t0 + MS * 230, false); // DebounceRelease
        assert_eq!(scanner.poll_at(t0 + MS * 240, true), None); // bounce → Pressed

        // Now truly release.
        scanner.poll_at(t0 + MS * 300, false); // DebounceRelease
        assert_eq!(
            scanner.poll_at(t0 + MS * 330, false),
            Some(BUTTON_TYPE_SHORT)
        );

        // No second event.
        assert_eq!(scanner.poll_at(t0 + MS * 400, false), None);
    }

    /// Mock ButtonPoll that fires a single event on a chosen poll count.
    struct MockButtonPoll {
        polls: usize,
        fire_on_poll: usize,
    }

    impl MockButtonPoll {
        fn new(fire_on_poll: usize) -> Self {
            Self {
                polls: 0,
                fire_on_poll,
            }
        }
    }

    impl ButtonPoll for MockButtonPoll {
        fn poll(&mut self) -> Option<u8> {
            self.polls += 1;
            if self.polls == self.fire_on_poll {
                Some(BUTTON_TYPE_SHORT)
            } else {
                None
            }
        }
    }

    #[test]
    fn button_event_bridge_integration() {
        // Verify that the bridge emits EVENT_BUTTON when the button poller
        // fires. Uses a deterministic mock instead of wall-clock sleeps.
        let mut bridge = Bridge::with_ble_and_button(
            MockSerial::new(),
            MockRadio::new(),
            MockBle::new(),
            MockButtonPoll::new(2),
            ModemCounters::new(),
        );

        // First poll: no event. Second poll: MockButtonPoll fires.
        bridge.poll();
        bridge.poll();

        // Decode the EVENT_BUTTON from the serial output.
        let tx = bridge.usb.take_tx();
        assert!(!tx.is_empty(), "expected EVENT_BUTTON on serial output");

        // Find the EVENT_BUTTON frame in the output (there may be a
        // MODEM_READY from with_ble_and_button's disable call too).
        let mut decoder = FrameDecoder::new();
        decoder.push(&tx);
        let mut found_button = false;
        loop {
            match decoder.decode() {
                Ok(Some(ModemMessage::EventButton(eb))) => {
                    assert_eq!(eb.button_type, BUTTON_TYPE_SHORT);
                    found_button = true;
                }
                Ok(Some(_)) => {} // skip MODEM_READY etc.
                Ok(None) => break,
                Err(_) => break,
            }
        }
        assert!(found_button, "EVENT_BUTTON not found in serial output");
    }
}
