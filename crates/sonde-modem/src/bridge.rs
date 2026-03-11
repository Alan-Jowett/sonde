// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Bridge logic: connects the USB-CDC serial codec to the ESP-NOW driver.
//!
//! Decodes inbound serial frames from the gateway, dispatches commands to
//! the ESP-NOW driver, and encodes outbound frames (RECV_FRAME, STATUS, etc.)
//! back to the gateway.

use log::{info, warn};
use std::sync::Arc;

use sonde_protocol::modem::{
    encode_modem_frame, FrameDecoder, ModemCodecError, ModemError, ModemMessage, ModemReady,
    ModemStatus, ScanEntry, ScanResult, SendFrame, MODEM_ERR_CHANNEL_SET_FAILED,
};

use crate::espnow::EspNowDriver;
use crate::status::ModemCounters;
use crate::usb_cdc::UsbCdcDriver;

/// Firmware version: major.minor.patch.build (one byte each).
const FIRMWARE_VERSION: [u8; 4] = [0, 1, 0, 0];

/// Bridge between USB-CDC and ESP-NOW.
pub struct Bridge {
    usb: UsbCdcDriver,
    espnow: EspNowDriver,
    counters: Arc<ModemCounters>,
    decoder: FrameDecoder,
    rx_buf: [u8; 64],
}

impl Bridge {
    pub fn new(usb: UsbCdcDriver, espnow: EspNowDriver, counters: Arc<ModemCounters>) -> Self {
        Self {
            usb,
            espnow,
            counters,
            decoder: FrameDecoder::new(),
            rx_buf: [0u8; 64],
        }
    }

    /// Encode and write a modem message to USB. Firmware-generated messages
    /// are always within size limits, so encoding failures are logged but
    /// otherwise ignored.
    fn send_msg(&mut self, msg: &ModemMessage) {
        match encode_modem_frame(msg) {
            Ok(frame) => self.usb.write(&frame),
            Err(e) => warn!("encode error: {}", e),
        }
    }

    /// Send MODEM_READY to the gateway.
    pub fn send_modem_ready(&mut self) {
        let mac = self.espnow.mac_address();
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

    /// Poll for USB data and ESP-NOW received frames.
    pub fn poll(&mut self) {
        // Read from USB and feed to the decoder.
        let n = self.usb.read(&mut self.rx_buf);
        if n > 0 {
            self.decoder.push(&self.rx_buf[..n]);
        }

        // Decode and dispatch serial frames.
        loop {
            match self.decoder.decode() {
                Ok(Some(msg)) => self.dispatch(msg),
                Ok(None) => break,
                Err(ModemCodecError::EmptyFrame) => {
                    // Silently discard zero-length frames; keep decoding.
                    continue;
                }
                Err(ModemCodecError::FrameTooLarge(len)) => {
                    warn!("framing error: len={}, resetting decoder", len);
                    // Clear the decoder so we can parse subsequent commands
                    // (including RESET) after the framing error.
                    self.decoder.reset();
                    break;
                }
                Err(e) => {
                    warn!("decode error: {}", e);
                    break;
                }
            }
        }

        // Forward any received ESP-NOW frames to USB.
        let rx_frames = self.espnow.drain_rx();
        for rf in rx_frames {
            let msg = ModemMessage::RecvFrame(rf);
            self.send_msg(&msg);
        }
    }

    fn dispatch(&mut self, msg: ModemMessage) {
        match msg {
            ModemMessage::Reset => self.handle_reset(),

            ModemMessage::SendFrame(sf) => self.handle_send_frame(sf),

            ModemMessage::SetChannel(ch) => self.handle_set_channel(ch),

            ModemMessage::GetStatus => self.handle_get_status(),

            ModemMessage::ScanChannels => self.handle_scan_channels(),

            ModemMessage::Unknown { msg_type, .. } => {
                // Silently discard unknown types (forward compatibility).
                info!("discarding unknown msg_type 0x{:02x}", msg_type);
            }

            _ => {
                // Modem should not receive modem→gateway messages; discard.
            }
        }
    }

    fn handle_reset(&mut self) {
        info!("RESET received");
        self.espnow.reinit();
        self.counters.reset();
        self.decoder.reset();
        self.send_modem_ready();
    }

    fn handle_send_frame(&mut self, sf: SendFrame) {
        self.espnow.send(&sf.peer_mac, &sf.frame_data);
    }

    fn handle_set_channel(&mut self, channel: u8) {
        match self.espnow.set_channel(channel) {
            Ok(()) => {
                let ack = ModemMessage::SetChannelAck(channel);
                self.send_msg(&ack);
            }
            Err(()) => {
                let err = ModemMessage::Error(ModemError {
                    error_code: MODEM_ERR_CHANNEL_SET_FAILED,
                    message: b"invalid channel".to_vec(),
                });
                self.send_msg(&err);
            }
        }
    }

    fn handle_get_status(&mut self) {
        let status = ModemMessage::Status(ModemStatus {
            channel: self.espnow.channel(),
            uptime_s: self.counters.uptime_s(),
            tx_count: self.counters.tx_count(),
            rx_count: self.counters.rx_count(),
            tx_fail_count: self.counters.tx_fail_count(),
        });
        self.send_msg(&status);
    }

    fn handle_scan_channels(&mut self) {
        let results = self.espnow.scan_channels();
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
