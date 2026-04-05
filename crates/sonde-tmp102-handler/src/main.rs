// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Gateway handler for TMP102 temperature sensor.
//!
//! Receives `DATA` messages via the sonde handler protocol (4-byte BE
//! length + CBOR on stdin), decodes TMP102 payloads, and writes JSON
//! records to `temperature_log.jsonl`.
//!
//! # Handler protocol
//!
//! The gateway sends DATA messages as length-prefixed CBOR:
//!
//! ```text
//! {1: 0x01, 2: request_id, 3: node_id, 4: program_hash, 5: data, 6: timestamp}
//! ```
//!
//! The handler replies with DATA_REPLY (no reply data):
//!
//! ```text
//! {1: 0x81, 2: request_id, 3: b""}
//! ```
//!
//! # Usage
//!
//! ```yaml
//! handlers:
//!   - program_hash: "*"
//!     command: "sonde-tmp102-handler"
//! ```

use std::fs::OpenOptions;
use std::io::{self, Read, Write};

use ciborium::Value;

const OUTPUT_FILE: &str = "temperature_log.jsonl";

/// Handler protocol message types.
const MSG_TYPE_DATA: u64 = 0x01;
const MSG_TYPE_DATA_REPLY: u64 = 0x81;

/// CBOR integer keys for the handler protocol.
///
/// Keys 1–5 are used in DATA (gateway → handler). DATA_REPLY
/// (handler → gateway) reuses keys 1–2 and uses key 3 for reply data.
const KEY_MSG_TYPE: i64 = 1;
const KEY_REQUEST_ID: i64 = 2;
const KEY_NODE_ID: i64 = 3;
const KEY_REPLY_DATA: i64 = 3;
const KEY_DATA: i64 = 5;

/// Look up a value by integer key in a CBOR map (represented as Vec of pairs).
fn map_get(entries: &[(Value, Value)], key: i64) -> Option<&Value> {
    entries.iter().find_map(|(k, v)| {
        if let Value::Integer(i) = k {
            let n: i128 = (*i).into();
            if n == key as i128 {
                return Some(v);
            }
        }
        None
    })
}

/// Read exactly `n` bytes from `reader`, returning `None` on clean EOF.
fn read_exact(reader: &mut impl Read, n: usize) -> io::Result<Option<Vec<u8>>> {
    let mut buf = vec![0u8; n];
    let mut filled = 0;
    while filled < n {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => {
                if filled == 0 {
                    return Ok(None);
                }
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "unexpected EOF mid-message",
                ));
            }
            Ok(k) => filled += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(Some(buf))
}

/// Read a length-prefixed CBOR message from stdin.
fn read_message(reader: &mut impl Read) -> io::Result<Option<Vec<(Value, Value)>>> {
    let len_buf = match read_exact(reader, 4)? {
        Some(b) => b,
        None => return Ok(None),
    };
    let length = u32::from_be_bytes([len_buf[0], len_buf[1], len_buf[2], len_buf[3]]) as usize;

    // Reject messages exceeding the 1 MB protocol limit (GW-0502)
    if length > 1_048_576 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message exceeds 1 MB limit",
        ));
    }

    let payload = match read_exact(reader, length)? {
        Some(b) => b,
        None => {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "unexpected EOF reading CBOR payload",
            ))
        }
    };

    let value: Value = ciborium::from_reader(&payload[..])
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

    match value {
        Value::Map(entries) => Ok(Some(entries)),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "expected CBOR map",
        )),
    }
}

/// Write a length-prefixed CBOR message to stdout.
fn write_message(writer: &mut impl Write, msg: Vec<(Value, Value)>) -> io::Result<()> {
    let mut payload = Vec::new();
    ciborium::into_writer(&Value::Map(msg), &mut payload)
        .map_err(|e| io::Error::other(e.to_string()))?;

    let len_bytes = (payload.len() as u32).to_be_bytes();
    writer.write_all(&len_bytes)?;
    writer.write_all(&payload)?;
    writer.flush()
}

/// Decode a 6-byte TMP102 payload into temperature data.
fn decode_tmp102(data: &[u8]) -> Option<(u16, f64)> {
    if data.len() != 6 {
        return None;
    }
    let raw_hi = data[0] as u16;
    let raw_lo = data[1] as u16;
    let raw_12bit = (raw_hi << 4) | (raw_lo >> 4);

    let temp_mc = i32::from_le_bytes([data[2], data[3], data[4], data[5]]);
    let temperature_c = temp_mc as f64 / 1000.0;

    Some((raw_12bit, temperature_c))
}

/// Extract a u64 from a CBOR integer value.
fn value_as_u64(v: &Value) -> Option<u64> {
    match v {
        Value::Integer(i) => {
            let n: i128 = (*i).into();
            u64::try_from(n).ok()
        }
        _ => None,
    }
}

/// Extract a string from a CBOR text value.
fn value_as_str(v: &Value) -> Option<&str> {
    match v {
        Value::Text(s) => Some(s.as_str()),
        _ => None,
    }
}

/// Extract bytes from a CBOR bytes value.
fn value_as_bytes(v: &Value) -> Option<&[u8]> {
    match v {
        Value::Bytes(b) => Some(b.as_slice()),
        _ => None,
    }
}

fn main() {
    let mut stdin = io::stdin().lock();
    let mut stdout = io::stdout().lock();

    loop {
        let msg = match read_message(&mut stdin) {
            Ok(Some(m)) => m,
            Ok(None) => break,
            Err(e) => {
                eprintln!("[TMP102] read error: {e}");
                break;
            }
        };

        let msg_type = map_get(&msg, KEY_MSG_TYPE)
            .and_then(value_as_u64)
            .unwrap_or(0);
        if msg_type != MSG_TYPE_DATA {
            continue;
        }

        let request_id = map_get(&msg, KEY_REQUEST_ID)
            .and_then(value_as_u64)
            .unwrap_or(0);
        let node_id = map_get(&msg, KEY_NODE_ID)
            .and_then(value_as_str)
            .unwrap_or("unknown");
        let data = map_get(&msg, KEY_DATA)
            .and_then(value_as_bytes)
            .unwrap_or(&[]);

        let decoded = decode_tmp102(data);

        // Build JSON record
        let mut record = serde_json::Map::new();
        record.insert(
            "timestamp".into(),
            serde_json::Value::String(format_utc_now()),
        );
        record.insert(
            "device".into(),
            serde_json::Value::String(node_id.to_string()),
        );
        record.insert(
            "raw_hex".into(),
            serde_json::Value::String(hex_encode(data)),
        );
        if let Some((raw_12bit, temperature_c)) = decoded {
            record.insert(
                "temperature_c".into(),
                serde_json::Value::Number(
                    serde_json::Number::from_f64(temperature_c)
                        .unwrap_or_else(|| serde_json::Number::from(0)),
                ),
            );
            record.insert(
                "raw_12bit".into(),
                serde_json::Value::Number(serde_json::Number::from(raw_12bit)),
            );
        }

        if let Err(e) = append_record(&record) {
            eprintln!("[TMP102] write error: {e}");
        }

        let temp_str = match decoded {
            Some((_, t)) => format!("{t:.3}\u{00B0}C"),
            None => "decode failed".to_string(),
        };
        eprintln!("[TMP102] {node_id}: {temp_str}");

        let reply = vec![
            (
                Value::Integer(KEY_MSG_TYPE.into()),
                Value::Integer((MSG_TYPE_DATA_REPLY as i64).into()),
            ),
            (
                Value::Integer(KEY_REQUEST_ID.into()),
                Value::Integer(request_id.into()),
            ),
            (Value::Integer(KEY_REPLY_DATA.into()), Value::Bytes(vec![])),
        ];
        if let Err(e) = write_message(&mut stdout, reply) {
            eprintln!("[TMP102] write error: {e}");
            break;
        }
    }
}

/// Append a JSON record to the output file.
fn append_record(record: &serde_json::Map<String, serde_json::Value>) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(OUTPUT_FILE)?;
    let json = serde_json::to_string(record)?;
    writeln!(file, "{json}")
}

/// Encode bytes as lowercase hex.
fn hex_encode(data: &[u8]) -> String {
    let mut s = String::with_capacity(data.len() * 2);
    for b in data {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Format the current time as an ISO 8601 UTC string.
///
/// Uses `std::time::SystemTime` to avoid a `chrono` dependency. Produces
/// output like `"2026-04-05T00:11:00Z"`.
fn format_utc_now() -> String {
    let now = std::time::SystemTime::now();
    let dur = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs();

    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm)
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02}T{hours:02}:{minutes:02}:{seconds:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_tmp102_valid() {
        // raw_hi=0x19, raw_lo=0x80 → raw_12bit = 0x198 = 408
        // temp_mc = 25500 LE i32 → 25.5°C
        let data = [0x19, 0x80, 0x9C, 0x63, 0x00, 0x00];
        let (raw, temp) = decode_tmp102(&data).unwrap();
        assert_eq!(raw, 0x198);
        assert!((temp - 25.5).abs() < 0.001);
    }

    #[test]
    fn test_decode_tmp102_wrong_length() {
        assert!(decode_tmp102(&[0x00; 5]).is_none());
        assert!(decode_tmp102(&[0x00; 7]).is_none());
        assert!(decode_tmp102(&[]).is_none());
    }

    #[test]
    fn test_decode_tmp102_negative_temp() {
        let temp_mc: i32 = -5000;
        let le = temp_mc.to_le_bytes();
        let data = [0x00, 0x00, le[0], le[1], le[2], le[3]];
        let (_, temp) = decode_tmp102(&data).unwrap();
        assert!((temp - (-5.0)).abs() < 0.001);
    }

    #[test]
    fn test_hex_encode() {
        assert_eq!(hex_encode(&[0xAB, 0xCD, 0x01]), "abcd01");
        assert_eq!(hex_encode(&[]), "");
    }

    #[test]
    fn test_read_write_roundtrip() {
        let msg = vec![
            (Value::Integer(1.into()), Value::Integer(1.into())),
            (Value::Integer(2.into()), Value::Integer(42.into())),
            (
                Value::Integer(3.into()),
                Value::Text("sensor-1".to_string()),
            ),
            (
                Value::Integer(5.into()),
                Value::Bytes(vec![0x19, 0x80, 0x9C, 0x63, 0x00, 0x00]),
            ),
        ];

        let mut buf = Vec::new();
        write_message(&mut buf, msg).unwrap();

        let mut cursor = io::Cursor::new(buf);
        let parsed = read_message(&mut cursor).unwrap().unwrap();

        assert_eq!(
            map_get(&parsed, KEY_MSG_TYPE).and_then(value_as_u64),
            Some(MSG_TYPE_DATA)
        );
        assert_eq!(
            map_get(&parsed, KEY_REQUEST_ID).and_then(value_as_u64),
            Some(42)
        );
        assert_eq!(
            map_get(&parsed, KEY_NODE_ID).and_then(value_as_str),
            Some("sensor-1")
        );
    }

    #[test]
    fn test_read_message_eof() {
        let mut cursor = io::Cursor::new(Vec::<u8>::new());
        assert!(read_message(&mut cursor).unwrap().is_none());
    }

    #[test]
    fn test_read_message_truncated_length() {
        let mut cursor = io::Cursor::new(vec![0x00, 0x00]);
        let result = read_message(&mut cursor);
        assert!(result.is_err());
    }

    #[test]
    fn test_format_utc_epoch() {
        let ts = format_utc_now();
        assert!(ts.ends_with('Z'));
        assert_eq!(ts.len(), 20);
    }

    #[test]
    fn test_map_get_finds_key() {
        let entries = vec![
            (Value::Integer(1.into()), Value::Integer(42.into())),
            (Value::Integer(5.into()), Value::Bytes(vec![0xAB])),
        ];
        assert_eq!(map_get(&entries, 1).and_then(value_as_u64), Some(42));
        assert_eq!(
            map_get(&entries, 5).and_then(value_as_bytes),
            Some(&[0xAB][..])
        );
        assert!(map_get(&entries, 99).is_none());
    }
}
