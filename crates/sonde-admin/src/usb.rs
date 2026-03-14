// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! USB serial interface for node pairing.
//!
//! Drives the pairing protocol defined in `pairing-protocol.md` over a
//! serial port connected to a sonde node in pairing mode.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use rand::RngExt;
use sha2::{Digest, Sha256};

use sonde_protocol::modem::{
    encode_modem_frame, FrameDecoder, IdentityResponse, ModemCodecError, ModemMessage, PairRequest,
    PAIRING_STATUS_SUCCESS, PSK_SIZE,
};

const READY_TIMEOUT: Duration = Duration::from_secs(5);
const ACK_TIMEOUT: Duration = Duration::from_secs(5);
const IDENTITY_TIMEOUT: Duration = Duration::from_secs(2);

/// Generate a 256-bit PSK from the OS CSPRNG.
pub fn generate_psk() -> [u8; PSK_SIZE] {
    rand::rng().random()
}

/// Derive the `key_hint` from a PSK: upper 16 bits of SHA-256(PSK), big-endian.
pub fn derive_key_hint(psk: &[u8; PSK_SIZE]) -> u16 {
    let hash = Sha256::digest(psk);
    u16::from_be_bytes([hash[0], hash[1]])
}

/// Pair a node by sending `PAIR_REQUEST` with the given `key_hint` and PSK.
///
/// This function performs only the USB serial exchange. Callers are
/// responsible for printing the result and, for the auto-pairing flow,
/// for registering the node with the gateway afterward.
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
        let ack = read_message(&mut port, &mut decoder, deadline)?;
        match ack {
            ModemMessage::PairingReady(_) => continue, // §6.4: ignore re-sent ready
            ModemMessage::PairAck(a) if a.status == PAIRING_STATUS_SUCCESS => {
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
pub fn factory_reset_node(port_name: &str, json: bool) -> Result<(), String> {
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
        let ack = read_message(&mut port, &mut decoder, deadline)?;
        match ack {
            ModemMessage::PairingReady(_) => continue, // §6.4: ignore re-sent ready
            ModemMessage::ResetAck(a) if a.status == PAIRING_STATUS_SUCCESS => {
                if json {
                    println!("{}", serde_json::json!({"status": "success"}));
                } else {
                    println!("Factory reset successful");
                }
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
pub fn query_identity(port_name: &str, json: bool) -> Result<(), String> {
    let mut port = serialport::new(port_name, 115_200)
        .timeout(READY_TIMEOUT)
        .open()
        .map_err(|e| format!("failed to open {}: {}", port_name, e))?;

    let mut decoder = FrameDecoder::new();
    wait_for_ready(&mut port, &mut decoder)?;

    let identity = query_identity_inner(&mut port, &mut decoder)?;
    match identity {
        IdentityResponse::Paired { key_hint } => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({"status": "paired", "key_hint": format!("0x{:04x}", key_hint)})
                );
            } else {
                println!("Paired (key_hint=0x{:04x})", key_hint);
            }
        }
        IdentityResponse::Unpaired => {
            if json {
                println!("{}", serde_json::json!({"status": "unpaired"}));
            } else {
                println!("Unpaired");
            }
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
        let resp = read_message(port, decoder, deadline)?;
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
        let msg = read_message(port, decoder, deadline)?;
        if matches!(msg, ModemMessage::PairingReady(_)) {
            return Ok(());
        }
        // Discard non-PAIRING_READY frames (forward compat)
    }
}

fn read_message(
    port: &mut Box<dyn serialport::SerialPort>,
    decoder: &mut FrameDecoder,
    deadline: Instant,
) -> Result<ModemMessage, String> {
    let mut buf = [0u8; 256];
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Err("timeout waiting for response".into());
        }
        // Check for already-buffered frames first
        match decoder.decode() {
            Ok(Some(msg)) => return Ok(msg),
            Ok(None) => {}
            Err(ModemCodecError::EmptyFrame) => continue,
            Err(ModemCodecError::FrameTooLarge(_)) => {
                // ROM bootloader or console log garbage can produce
                // spurious large-frame errors. Reset the decoder and
                // keep trying — the next valid frame will sync up.
                decoder.reset();
                continue;
            }
            Err(e) => return Err(format!("decode: {}", e)),
        }
        // Set port timeout to remaining deadline so read() doesn't
        // block past the overall deadline.
        let remaining = deadline.duration_since(now);
        let read_timeout = remaining.min(Duration::from_millis(200));
        port.set_timeout(read_timeout)
            .map_err(|e| format!("set_timeout: {}", e))?;
        let n = match port.read(&mut buf) {
            Ok(0) => return Err("USB disconnected".into()),
            Ok(n) => n,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                continue
            }
            Err(e) => return Err(format!("read: {}", e)),
        };
        decoder.push(&buf[..n]);
    }
}
