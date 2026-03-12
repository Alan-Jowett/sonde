// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Device integration tests for sonde-modem firmware.
//!
//! These tests talk to a real ESP32-S3 modem over USB-CDC serial and
//! validate the modem protocol behavior on actual hardware.
//!
//! # Usage
//!
//! ```bash
//! # Set the USB-CDC COM port (not the UART/log port):
//! MODEM_PORT=COM5 cargo test -p sonde-modem --features device-tests --test device_tests -- --test-threads=1
//! ```
//!
//! If `MODEM_PORT` is not set, all tests are skipped (not failed).
//! This allows CI to run without hardware attached.
//!
//! Tests must run single-threaded (`--test-threads=1`) since they
//! share the serial port. A process-wide mutex is used as a safety
//! net in case `--test-threads` is omitted.

#![cfg(feature = "device-tests")]

use sonde_protocol::modem::{
    encode_modem_frame, FrameDecoder, ModemMessage, ModemReady, SendFrame,
    MODEM_ERR_CHANNEL_SET_FAILED,
};
use std::io::{Read, Write};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Process-wide lock so tests are serialised even if run in parallel.
static PORT_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn port_lock() -> &'static Mutex<()> {
    PORT_LOCK.get_or_init(|| Mutex::new(()))
}

/// Open the modem serial port, or skip the test if MODEM_PORT is not set.
fn open_modem() -> Option<Box<dyn serialport::SerialPort>> {
    let port_name = match std::env::var("MODEM_PORT") {
        Ok(p) if !p.is_empty() => p,
        _ => {
            eprintln!("MODEM_PORT not set — skipping device test");
            return None;
        }
    };
    let port = serialport::new(&port_name, 115200)
        .timeout(Duration::from_millis(500))
        .open()
        .unwrap_or_else(|e| panic!("failed to open {}: {}", port_name, e));
    Some(port)
}

/// Drain any pending data from the port.
fn drain(port: &mut dyn serialport::SerialPort) {
    let mut buf = [0u8; 512];
    loop {
        match port.read(&mut buf) {
            Ok(0) => break,
            Ok(_) => continue,
            Err(_) => break,
        }
    }
}

/// Encode and send a modem message over the serial port.
fn send(port: &mut dyn serialport::SerialPort, msg: &ModemMessage) {
    let frame = encode_modem_frame(msg).expect("encode failed");
    port.write_all(&frame).expect("write failed");
    port.flush().expect("flush failed");
}

/// Read frames from the port using a streaming decoder, with a timeout.
fn recv(port: &mut dyn serialport::SerialPort, timeout: Duration) -> Vec<ModemMessage> {
    let mut decoder = FrameDecoder::new();
    let mut buf = [0u8; 512];
    let mut messages = Vec::new();
    let start = Instant::now();

    while start.elapsed() < timeout {
        match port.read(&mut buf) {
            Ok(n) if n > 0 => {
                decoder.push(&buf[..n]);
                loop {
                    match decoder.decode() {
                        Ok(Some(msg)) => messages.push(msg),
                        Ok(None) => break,
                        Err(_) => break, // malformed frame consumed; try next
                    }
                }
            }
            _ => {}
        }
        if !messages.is_empty() {
            // Give a brief window for additional frames.
            std::thread::sleep(Duration::from_millis(50));
            match port.read(&mut buf) {
                Ok(n) if n > 0 => {
                    decoder.push(&buf[..n]);
                    loop {
                        match decoder.decode() {
                            Ok(Some(msg)) => messages.push(msg),
                            Ok(None) => break,
                            Err(_) => break,
                        }
                    }
                }
                _ => {}
            }
            break;
        }
    }
    messages
}

/// Send RESET, drain stale data, and wait for MODEM_READY.
fn reset_and_wait(port: &mut dyn serialport::SerialPort) -> ModemReady {
    drain(port);
    send(port, &ModemMessage::Reset);
    let msgs = recv(port, Duration::from_secs(3));
    for msg in &msgs {
        if let ModemMessage::ModemReady(mr) = msg {
            return mr.clone();
        }
    }
    panic!("MODEM_READY not received after RESET (got {:?})", msgs);
}

/// Receive exactly one message of the expected type.
fn recv_one(port: &mut dyn serialport::SerialPort, timeout: Duration) -> ModemMessage {
    let msgs = recv(port, timeout);
    assert!(
        !msgs.is_empty(),
        "no response received within {:?}",
        timeout
    );
    msgs.into_iter().next().unwrap()
}

// ============================================================
// Tests
// ============================================================

/// T-0101: MODEM_READY after RESET.
#[test]
fn t0101_modem_ready_after_reset() {
    let _lock = port_lock().lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut port) = open_modem() else {
        return;
    };
    let mr = reset_and_wait(&mut *port);
    assert_eq!(mr.firmware_version, [0, 1, 0, 0]);
    assert_ne!(mr.mac_address, [0; 6], "MAC should not be all zeros");
    eprintln!(
        "MODEM_READY: fw={}.{}.{}.{} mac={:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        mr.firmware_version[0],
        mr.firmware_version[1],
        mr.firmware_version[2],
        mr.firmware_version[3],
        mr.mac_address[0],
        mr.mac_address[1],
        mr.mac_address[2],
        mr.mac_address[3],
        mr.mac_address[4],
        mr.mac_address[5],
    );
}

/// T-0102: GET_STATUS returns valid status with zeroed counters after RESET.
#[test]
fn t0102_get_status_after_reset() {
    let _lock = port_lock().lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut port) = open_modem() else {
        return;
    };
    reset_and_wait(&mut *port);
    send(&mut *port, &ModemMessage::GetStatus);
    let msg = recv_one(&mut *port, Duration::from_secs(2));
    match msg {
        ModemMessage::Status(s) => {
            assert_eq!(s.channel, 1, "channel should be 1 after RESET");
            assert_eq!(s.tx_count, 0);
            assert_eq!(s.rx_count, 0);
            assert_eq!(s.tx_fail_count, 0);
            eprintln!("STATUS: ch={} uptime={}s", s.channel, s.uptime_s);
        }
        other => panic!("expected Status, got {:?}", other),
    }
}

/// T-0104: Unknown message type is silently discarded.
#[test]
fn t0104_unknown_type_discarded() {
    let _lock = port_lock().lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut port) = open_modem() else {
        return;
    };
    reset_and_wait(&mut *port);

    // Send an unknown type.
    send(
        &mut *port,
        &ModemMessage::Unknown {
            msg_type: 0x7F,
            body: vec![1, 2, 3],
        },
    );

    // Then GET_STATUS to prove the modem is still alive.
    send(&mut *port, &ModemMessage::GetStatus);
    let msg = recv_one(&mut *port, Duration::from_secs(2));
    assert!(
        matches!(msg, ModemMessage::Status(_)),
        "modem should still respond after unknown type"
    );
}

/// T-0205: SET_CHANNEL and ACK.
#[test]
fn t0205_set_channel() {
    let _lock = port_lock().lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut port) = open_modem() else {
        return;
    };
    reset_and_wait(&mut *port);

    send(&mut *port, &ModemMessage::SetChannel(6));
    let msg = recv_one(&mut *port, Duration::from_secs(2));
    assert_eq!(msg, ModemMessage::SetChannelAck(6));

    // Verify via STATUS.
    send(&mut *port, &ModemMessage::GetStatus);
    let msg = recv_one(&mut *port, Duration::from_secs(2));
    match msg {
        ModemMessage::Status(s) => assert_eq!(s.channel, 6),
        other => panic!("expected Status, got {:?}", other),
    }
}

/// T-0206: SCAN_CHANNELS returns 14-channel scan result.
#[test]
fn t0206_scan_channels() {
    let _lock = port_lock().lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut port) = open_modem() else {
        return;
    };
    reset_and_wait(&mut *port);

    send(&mut *port, &ModemMessage::ScanChannels);
    // Scan can take 2-3 seconds on real hardware.
    let msg = recv_one(&mut *port, Duration::from_secs(10));
    match msg {
        ModemMessage::ScanResult(sr) => {
            assert_eq!(sr.entries.len(), 14, "should have 14 channel entries");
            for entry in &sr.entries {
                assert!(
                    entry.channel >= 1 && entry.channel <= 14,
                    "invalid channel {}",
                    entry.channel
                );
                eprintln!(
                    "  ch={:2} aps={} rssi={}",
                    entry.channel, entry.ap_count, entry.strongest_rssi
                );
            }
        }
        other => panic!("expected ScanResult, got {:?}", other),
    }
}

/// T-0300: RESET clears state (channel, counters).
#[test]
fn t0300_reset_clears_state() {
    let _lock = port_lock().lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut port) = open_modem() else {
        return;
    };
    reset_and_wait(&mut *port);

    // Change channel.
    send(&mut *port, &ModemMessage::SetChannel(11));
    recv_one(&mut *port, Duration::from_secs(2)); // consume ACK

    // Send some frames to increment tx_count.
    let peer = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];
    for _ in 0..3 {
        send(
            &mut *port,
            &ModemMessage::SendFrame(SendFrame {
                peer_mac: peer,
                frame_data: vec![0xDE, 0xAD],
            }),
        );
    }
    std::thread::sleep(Duration::from_millis(200));

    // Verify tx_count incremented before RESET.
    send(&mut *port, &ModemMessage::GetStatus);
    let pre = recv_one(&mut *port, Duration::from_secs(2));
    match pre {
        ModemMessage::Status(s) => {
            assert!(s.tx_count > 0, "tx_count should be > 0 before RESET");
        }
        other => panic!("expected Status before RESET, got {:?}", other),
    }

    // RESET should clear everything.
    reset_and_wait(&mut *port);

    send(&mut *port, &ModemMessage::GetStatus);
    let msg = recv_one(&mut *port, Duration::from_secs(2));
    match msg {
        ModemMessage::Status(s) => {
            assert_eq!(s.channel, 1, "channel should reset to 1");
            assert_eq!(s.tx_count, 0, "tx_count should be 0");
            assert_eq!(s.rx_count, 0, "rx_count should be 0");
            assert_eq!(s.tx_fail_count, 0, "tx_fail_count should be 0");
        }
        other => panic!("expected Status, got {:?}", other),
    }
}

/// T-0303: Repeated RESET → MODEM_READY stability.
#[test]
fn t0303_repeated_reset() {
    let _lock = port_lock().lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut port) = open_modem() else {
        return;
    };

    for i in 0..5 {
        let mr = reset_and_wait(&mut *port);
        assert_eq!(mr.firmware_version, [0, 1, 0, 0], "iteration {} failed", i);
    }
}

/// T-0401: SET_CHANNEL with invalid channel returns ERROR.
#[test]
fn t0401_invalid_channel() {
    let _lock = port_lock().lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut port) = open_modem() else {
        return;
    };
    reset_and_wait(&mut *port);

    // Channel 0 (invalid).
    send(&mut *port, &ModemMessage::SetChannel(0));
    let msg = recv_one(&mut *port, Duration::from_secs(2));
    match msg {
        ModemMessage::Error(e) => {
            assert_eq!(e.error_code, MODEM_ERR_CHANNEL_SET_FAILED);
        }
        other => panic!("expected Error for channel 0, got {:?}", other),
    }

    // Channel 15 (invalid).
    send(&mut *port, &ModemMessage::SetChannel(15));
    let msg = recv_one(&mut *port, Duration::from_secs(2));
    match msg {
        ModemMessage::Error(e) => {
            assert_eq!(e.error_code, MODEM_ERR_CHANNEL_SET_FAILED);
        }
        other => panic!("expected Error for channel 15, got {:?}", other),
    }

    // Verify channel unchanged.
    send(&mut *port, &ModemMessage::GetStatus);
    let msg = recv_one(&mut *port, Duration::from_secs(2));
    match msg {
        ModemMessage::Status(s) => assert_eq!(s.channel, 1, "channel should still be 1"),
        other => panic!("expected Status, got {:?}", other),
    }
}

/// T-0402: Framing error recovery via RESET.
#[test]
fn t0402_framing_error_recovery() {
    let _lock = port_lock().lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut port) = open_modem() else {
        return;
    };
    reset_and_wait(&mut *port);

    // Send garbage bytes to corrupt framing.
    let garbage = [0xFF; 100];
    port.write_all(&garbage).expect("write garbage");
    port.flush().expect("flush garbage");
    std::thread::sleep(Duration::from_millis(100));

    // RESET should recover.
    let mr = reset_and_wait(&mut *port);
    assert_eq!(mr.firmware_version, [0, 1, 0, 0]);
}

/// T-0302: Status counter accuracy (uptime > 0 after delay).
#[test]
fn t0302_status_uptime() {
    let _lock = port_lock().lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut port) = open_modem() else {
        return;
    };
    reset_and_wait(&mut *port);

    // Wait a bit so uptime ticks up.
    std::thread::sleep(Duration::from_secs(2));

    send(&mut *port, &ModemMessage::GetStatus);
    let msg = recv_one(&mut *port, Duration::from_secs(2));
    match msg {
        ModemMessage::Status(s) => {
            assert!(
                s.uptime_s >= 1,
                "uptime should be >= 1s, got {}",
                s.uptime_s
            );
            eprintln!("STATUS uptime={}s", s.uptime_s);
        }
        other => panic!("expected Status, got {:?}", other),
    }
}
