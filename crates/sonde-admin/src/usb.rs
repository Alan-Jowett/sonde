// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! USB serial interface for node pairing.
//!
//! Drives the pairing protocol defined in `pairing-protocol.md` over a
//! serial port connected to a sonde node in pairing mode.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use sonde_protocol::modem::{
    encode_modem_frame, FrameDecoder, IdentityResponse, ModemCodecError, ModemMessage, PairRequest,
    PAIRING_STATUS_SUCCESS, PSK_SIZE,
};

const READY_TIMEOUT: Duration = Duration::from_secs(5);
const ACK_TIMEOUT: Duration = Duration::from_secs(5);
const IDENTITY_TIMEOUT: Duration = Duration::from_secs(2);

/// Pair a node by sending `PAIR_REQUEST` with the given `key_hint` and PSK.
pub fn pair_node(port_name: &str, key_hint: u16, psk: [u8; PSK_SIZE]) -> Result<(), String> {
    let mut port = serialport::new(port_name, 115_200)
        .timeout(READY_TIMEOUT)
        .open()
        .map_err(|e| format!("failed to open {}: {}", port_name, e))?;

    let mut decoder = FrameDecoder::new();
    wait_for_ready(&mut port, &mut decoder)?;

    // Check identity first — abort if already paired
    let identity = query_identity_inner(&mut port, &mut decoder)?;
    if let IdentityResponse::Paired { key_hint: kh } = identity {
        return Err(format!(
            "node is already paired (key_hint=0x{:04x}). Factory reset first.",
            kh
        ));
    }

    let req = ModemMessage::PairRequest(PairRequest { key_hint, psk });
    let frame = encode_modem_frame(&req).map_err(|e| format!("encode: {}", e))?;
    port.write_all(&frame)
        .map_err(|e| format!("write: {}", e))?;
    port.flush().map_err(|e| format!("flush: {}", e))?;

    port.set_timeout(ACK_TIMEOUT)
        .map_err(|e| format!("timeout: {}", e))?;
    let deadline = Instant::now() + ACK_TIMEOUT;
    loop {
        if Instant::now() >= deadline {
            return Err("timeout waiting for pair ack".into());
        }
        let ack = read_message(&mut port, &mut decoder)?;
        match ack {
            ModemMessage::PairingReady(_) => continue, // §6.4: ignore re-sent ready
            ModemMessage::PairAck(a) if a.status == PAIRING_STATUS_SUCCESS => {
                println!("Pairing successful (key_hint=0x{:04x})", key_hint);
                return Ok(());
            }
            ModemMessage::PairAck(a) => {
                return Err(format!("pairing failed: status 0x{:02x}", a.status))
            }
            _ => continue, // forward compat: skip unknown types
        }
    }
}

/// Factory-reset a node by sending `RESET_REQUEST`.
pub fn factory_reset_node(port_name: &str) -> Result<(), String> {
    let mut port = serialport::new(port_name, 115_200)
        .timeout(READY_TIMEOUT)
        .open()
        .map_err(|e| format!("failed to open {}: {}", port_name, e))?;

    let mut decoder = FrameDecoder::new();
    wait_for_ready(&mut port, &mut decoder)?;

    // Check identity first — abort if already unpaired
    let identity = query_identity_inner(&mut port, &mut decoder)?;
    if matches!(identity, IdentityResponse::Unpaired) {
        return Err("node is already unpaired — nothing to reset".into());
    }

    let req = ModemMessage::ResetRequest;
    let frame = encode_modem_frame(&req).map_err(|e| format!("encode: {}", e))?;
    port.write_all(&frame)
        .map_err(|e| format!("write: {}", e))?;
    port.flush().map_err(|e| format!("flush: {}", e))?;

    port.set_timeout(ACK_TIMEOUT)
        .map_err(|e| format!("timeout: {}", e))?;
    let deadline = Instant::now() + ACK_TIMEOUT;
    loop {
        if Instant::now() >= deadline {
            return Err("timeout waiting for reset ack".into());
        }
        let ack = read_message(&mut port, &mut decoder)?;
        match ack {
            ModemMessage::PairingReady(_) => continue, // §6.4: ignore re-sent ready
            ModemMessage::ResetAck(a) if a.status == PAIRING_STATUS_SUCCESS => {
                println!("Factory reset successful");
                return Ok(());
            }
            ModemMessage::ResetAck(a) => {
                return Err(format!("reset failed: status 0x{:02x}", a.status))
            }
            _ => continue, // forward compat: skip unknown types
        }
    }
}

/// Query a node's pairing identity by sending `IDENTITY_REQUEST`.
pub fn query_identity(port_name: &str) -> Result<(), String> {
    let mut port = serialport::new(port_name, 115_200)
        .timeout(READY_TIMEOUT)
        .open()
        .map_err(|e| format!("failed to open {}: {}", port_name, e))?;

    let mut decoder = FrameDecoder::new();
    wait_for_ready(&mut port, &mut decoder)?;

    let identity = query_identity_inner(&mut port, &mut decoder)?;
    match identity {
        IdentityResponse::Paired { key_hint } => {
            println!("Paired (key_hint=0x{:04x})", key_hint);
        }
        IdentityResponse::Unpaired => {
            println!("Unpaired");
        }
    }
    Ok(())
}

/// Send `IDENTITY_REQUEST` and return the parsed response.
fn query_identity_inner(
    port: &mut Box<dyn serialport::SerialPort>,
    decoder: &mut FrameDecoder,
) -> Result<IdentityResponse, String> {
    let req = ModemMessage::IdentityRequest;
    let frame = encode_modem_frame(&req).map_err(|e| format!("encode: {}", e))?;
    port.write_all(&frame)
        .map_err(|e| format!("write: {}", e))?;
    port.flush().map_err(|e| format!("flush: {}", e))?;

    port.set_timeout(IDENTITY_TIMEOUT)
        .map_err(|e| format!("timeout: {}", e))?;
    let deadline = Instant::now() + IDENTITY_TIMEOUT;
    loop {
        if Instant::now() >= deadline {
            return Err("timeout waiting for identity response".into());
        }
        let resp = read_message(port, decoder)?;
        match resp {
            ModemMessage::PairingReady(_) => continue, // §6.4: ignore re-sent ready
            ModemMessage::IdentityResponse(ir) => return Ok(ir),
            _ => continue, // forward compat: skip unknown types
        }
    }
}

fn wait_for_ready(
    port: &mut Box<dyn serialport::SerialPort>,
    decoder: &mut FrameDecoder,
) -> Result<(), String> {
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        if Instant::now() >= deadline {
            return Err("timeout waiting for pairing ready".into());
        }
        let msg = read_message(port, decoder)?;
        if matches!(msg, ModemMessage::PairingReady(_)) {
            return Ok(());
        }
        // Discard non-PAIRING_READY frames (forward compat)
    }
}

fn read_message(
    port: &mut Box<dyn serialport::SerialPort>,
    decoder: &mut FrameDecoder,
) -> Result<ModemMessage, String> {
    let mut buf = [0u8; 256];
    loop {
        // Check for already-buffered frames first
        match decoder.decode() {
            Ok(Some(msg)) => return Ok(msg),
            Ok(None) => {}
            Err(ModemCodecError::EmptyFrame) => continue,
            Err(ModemCodecError::FrameTooLarge(_)) => {
                return Err("framing error: frame too large. Close and re-open port.".into());
            }
            Err(e) => return Err(format!("decode: {}", e)),
        }
        let n = match port.read(&mut buf) {
            Ok(0) => return Err("USB disconnected".into()),
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
            Err(e) => return Err(format!("read: {}", e)),
        };
        decoder.push(&buf[..n]);
    }
}
