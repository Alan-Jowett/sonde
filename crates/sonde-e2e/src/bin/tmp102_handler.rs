// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! TMP102 temperature sensor handler for the sonde gateway.
//!
//! Receives APP_DATA from nodes running the `tmp102_sensor` BPF program,
//! decodes the temperature, and appends readings to a CSV file named
//! after the node's assigned name (`node_id`).
//!
//! # Payload format (from `tmp102_sensor.c`)
//!
//! | Offset | Size | Field    | Description                              |
//! |--------|------|----------|------------------------------------------|
//! | 0      | 1    | raw_hi   | TMP102 temperature register byte 0 (MSB) |
//! | 1      | 1    | raw_lo   | TMP102 temperature register byte 1 (LSB) |
//! | 2      | 4    | temp_mc  | Temperature in millidegrees C (i32 LE)    |
//!
//! # Usage
//!
//! ```yaml
//! # handlers.yaml
//! handlers:
//!   - program_hash: "*"
//!     command: "tmp102_handler"
//!     args: ["--output-dir", "./sensor-data"]
//! ```

use sonde_gateway::handler::HandlerMessage;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Expected payload size from the TMP102 BPF program.
const TMP102_PAYLOAD_LEN: usize = 6;

/// Sanitize a node_id to a safe filename (alphanumeric, underscore, hyphen only).
fn sanitize_node_id(node_id: &str) -> String {
    let sanitized: String = node_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "unknown".to_string()
    } else {
        sanitized
    }
}

/// Decode the 6-byte TMP102 payload.
fn decode_tmp102(data: &[u8]) -> Result<(u8, u8, i32), String> {
    if data.len() != TMP102_PAYLOAD_LEN {
        return Err(format!(
            "expected exactly {} bytes, got {}",
            TMP102_PAYLOAD_LEN,
            data.len()
        ));
    }
    let raw_hi = data[0];
    let raw_lo = data[1];
    let temp_mc = i32::from_le_bytes([data[2], data[3], data[4], data[5]]);
    Ok((raw_hi, raw_lo, temp_mc))
}

/// Append a reading to the node's CSV file.
fn append_reading(
    output_dir: &Path,
    node_id: &str,
    timestamp: u64,
    raw_hi: u8,
    raw_lo: u8,
    temp_mc: i32,
) -> std::io::Result<()> {
    let safe_id = sanitize_node_id(node_id);
    let path = output_dir.join(format!("{safe_id}.csv"));
    let is_new = !path.exists();

    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    if is_new {
        writeln!(file, "timestamp,temp_c,temp_mc,raw_hi,raw_lo")?;
    }
    let temp_c = temp_mc as f64 / 1000.0;
    writeln!(
        file,
        "{timestamp},{temp_c:.3},{temp_mc},0x{raw_hi:02x},0x{raw_lo:02x}"
    )
}

/// Write a length-prefixed HandlerMessage to a writer.
fn write_message(writer: &mut impl Write, msg: &HandlerMessage) -> std::io::Result<()> {
    let payload = msg
        .encode()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    writer.write_all(&(payload.len() as u32).to_be_bytes())?;
    writer.write_all(&payload)?;
    writer.flush()
}

/// Send a LOG message to the gateway.
fn send_log(writer: &mut impl Write, level: &str, message: &str) {
    let msg = HandlerMessage::Log {
        level: level.to_string(),
        message: message.to_string(),
    };
    let _ = write_message(writer, &msg);
}

fn main() {
    // Parse --output-dir from args (default: ./sensor-data).
    let args: Vec<String> = std::env::args().collect();
    let output_dir = args
        .windows(2)
        .find(|w| w[0] == "--output-dir")
        .map(|w| PathBuf::from(&w[1]))
        .unwrap_or_else(|| PathBuf::from("./sensor-data"));

    if let Err(e) = fs::create_dir_all(&output_dir) {
        eprintln!("ERROR: cannot create output dir: {e}");
        std::process::exit(1);
    }

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut stdin = stdin.lock();
    let mut stdout = stdout.lock();

    loop {
        // Read 4-byte big-endian length prefix.
        let mut len_buf = [0u8; 4];
        if stdin.read_exact(&mut len_buf).is_err() {
            break;
        }
        let len = u32::from_be_bytes(len_buf) as usize;
        if len > 1_048_576 {
            continue;
        }

        // Read CBOR payload.
        let mut buf = vec![0u8; len];
        if stdin.read_exact(&mut buf).is_err() {
            break;
        }

        // Decode the message.
        let msg = match HandlerMessage::decode(&buf) {
            Ok(m) => m,
            Err(_) => continue,
        };

        match msg {
            HandlerMessage::Data {
                request_id,
                node_id,
                data,
                timestamp,
                ..
            } => {
                match decode_tmp102(&data) {
                    Ok((raw_hi, raw_lo, temp_mc)) => {
                        let temp_c = temp_mc as f64 / 1000.0;
                        send_log(&mut stdout, "info", &format!("{node_id}: {temp_c:.3} °C"));
                        if let Err(e) = append_reading(
                            &output_dir,
                            &node_id,
                            timestamp,
                            raw_hi,
                            raw_lo,
                            temp_mc,
                        ) {
                            send_log(
                                &mut stdout,
                                "error",
                                &format!("{node_id}: write error: {e}"),
                            );
                        }
                    }
                    Err(e) => {
                        send_log(
                            &mut stdout,
                            "error",
                            &format!("{node_id}: payload decode error: {e}"),
                        );
                    }
                }

                // Reply with empty data (no response back to node).
                let reply = HandlerMessage::DataReply {
                    request_id,
                    data: vec![],
                    delivery: 0,
                };
                if write_message(&mut stdout, &reply).is_err() {
                    break;
                }
            }
            HandlerMessage::Event {
                node_id,
                event_type,
                ..
            } => {
                send_log(
                    &mut stdout,
                    "info",
                    &format!("{node_id}: event {event_type}"),
                );
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_tmp102_valid() {
        // 25.125 °C = raw 0x192 → temp_mc = 25125
        let data = [0x19, 0x20, 0x25, 0x62, 0x00, 0x00]; // 25125 = 0x6225 LE
        let (raw_hi, raw_lo, temp_mc) = decode_tmp102(&data).unwrap();
        assert_eq!(raw_hi, 0x19);
        assert_eq!(raw_lo, 0x20);
        assert_eq!(temp_mc, 25125);
    }

    #[test]
    fn test_decode_tmp102_negative() {
        // -0.0625 °C → temp_mc = -62 (0xFFFFFFC2 LE = [0xC2, 0xFF, 0xFF, 0xFF])
        let data = [0xFF, 0xF0, 0xC2, 0xFF, 0xFF, 0xFF];
        let (_, _, temp_mc) = decode_tmp102(&data).unwrap();
        assert_eq!(temp_mc, -62);
    }

    #[test]
    fn test_decode_tmp102_wrong_length() {
        assert!(decode_tmp102(&[0x00, 0x00]).is_err());
        assert!(decode_tmp102(&[0x00; 7]).is_err());
    }

    #[test]
    fn test_sanitize_node_id() {
        assert_eq!(sanitize_node_id("greenhouse-1"), "greenhouse-1");
        assert_eq!(
            sanitize_node_id("../../../etc/passwd"),
            "_________etc_passwd"
        );
        assert_eq!(sanitize_node_id("node 1"), "node_1");
        assert_eq!(sanitize_node_id(""), "unknown");
    }

    #[test]
    fn test_append_reading_creates_csv() {
        let dir = std::env::temp_dir().join("sonde_test_tmp102");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        append_reading(&dir, "test-node", 1710000000, 0x19, 0x20, 25125).unwrap();

        let content = fs::read_to_string(dir.join("test-node.csv")).unwrap();
        assert!(content.starts_with("timestamp,temp_c,temp_mc,raw_hi,raw_lo\n"));
        assert!(content.contains("25.125"));
        assert!(content.contains("0x19"));

        let _ = fs::remove_dir_all(&dir);
    }
}
