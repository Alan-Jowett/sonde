// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Gateway handler for SHT40 temperature and humidity sensor.
//!
//! Receives `DATA` messages via the sonde handler protocol (4-byte BE
//! length + CBOR on stdin), decodes SHT40 payloads, and writes JSON
//! records to `sht40_log.jsonl`.
//!
//! # SHT40 payload format (14 bytes)
//!
//! The BPF program (`test-programs/sht40_sensor.c`) sends:
//!
//! ```text
//! [0..5]   raw frame (T_msb, T_lsb, CRC_T, RH_msb, RH_lsb, CRC_RH)
//! [6..9]   temp_mC       (little-endian i32, milli-°C)
//! [10..13] rh_mpercent   (little-endian i32, milli-%RH)
//! ```
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
//!   - program_hash: "<sht40 program hash>"
//!     command: "sonde-sht40-handler"
//! ```

use std::fs::OpenOptions;
use std::io::{self, Read, Write};

use ciborium::Value;

const OUTPUT_FILE: &str = "sht40_log.jsonl";

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

/// SHT40 payload size: 6 raw bytes + 4 temp_mC + 4 rh_mpercent.
const SHT40_PAYLOAD_LEN: usize = 14;

/// CRC-8 per Sensirion SHT4x: polynomial 0x31, init 0xFF.
fn crc8_sensirion(data: &[u8; 2]) -> u8 {
    let mut crc: u8 = 0xFF;
    for &byte in data {
        crc ^= byte;
        for _ in 0..8 {
            if crc & 0x80 != 0 {
                crc = (crc << 1) ^ 0x31;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

/// Decoded SHT40 sensor reading.
#[derive(Debug, Clone, PartialEq)]
struct Sht40Reading {
    /// Raw 16-bit temperature value from the sensor.
    t_raw: u16,
    /// Raw 16-bit humidity value from the sensor.
    rh_raw: u16,
    /// Temperature in degrees Celsius.
    temperature_c: f64,
    /// Relative humidity in percent.
    humidity_pct: f64,
}

/// Decode a 14-byte SHT40 payload.
///
/// Returns `None` if the payload length is wrong or CRC validation fails.
fn decode_sht40(data: &[u8]) -> Result<Sht40Reading, &'static str> {
    if data.len() != SHT40_PAYLOAD_LEN {
        return Err("wrong payload length");
    }

    // Validate CRCs on the raw frame
    let t_crc = crc8_sensirion(&[data[0], data[1]]);
    if t_crc != data[2] {
        return Err("temperature CRC mismatch");
    }
    let rh_crc = crc8_sensirion(&[data[3], data[4]]);
    if rh_crc != data[5] {
        return Err("humidity CRC mismatch");
    }

    let t_raw = u16::from_be_bytes([data[0], data[1]]);
    let rh_raw = u16::from_be_bytes([data[3], data[4]]);

    let temp_mc = i32::from_le_bytes([data[6], data[7], data[8], data[9]]);
    let rh_mpercent = i32::from_le_bytes([data[10], data[11], data[12], data[13]]);

    Ok(Sht40Reading {
        t_raw,
        rh_raw,
        temperature_c: temp_mc as f64 / 1000.0,
        humidity_pct: rh_mpercent as f64 / 1000.0,
    })
}

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
                eprintln!("[SHT40] read error: {e}");
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

        let decoded = decode_sht40(data);

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

        match &decoded {
            Ok(reading) => {
                record.insert(
                    "temperature_c".into(),
                    serde_json::Value::Number(
                        serde_json::Number::from_f64(reading.temperature_c)
                            .unwrap_or_else(|| serde_json::Number::from(0)),
                    ),
                );
                record.insert(
                    "humidity_pct".into(),
                    serde_json::Value::Number(
                        serde_json::Number::from_f64(reading.humidity_pct)
                            .unwrap_or_else(|| serde_json::Number::from(0)),
                    ),
                );
                record.insert(
                    "t_raw".into(),
                    serde_json::Value::Number(serde_json::Number::from(reading.t_raw)),
                );
                record.insert(
                    "rh_raw".into(),
                    serde_json::Value::Number(serde_json::Number::from(reading.rh_raw)),
                );
            }
            Err(reason) => {
                record.insert(
                    "error".into(),
                    serde_json::Value::String((*reason).to_string()),
                );
            }
        }

        if let Err(e) = append_record(&record) {
            eprintln!("[SHT40] write error: {e}");
        }

        let status = match &decoded {
            Ok(r) => format!("{:.2}\u{00B0}C  {:.1}%RH", r.temperature_c, r.humidity_pct),
            Err(reason) => format!("decode failed: {reason}"),
        };
        eprintln!("[SHT40] {node_id}: {status}");

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
            eprintln!("[SHT40] write error: {e}");
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

    /// Build a valid 14-byte SHT40 payload from raw values and pre-computed
    /// integer results.
    fn build_payload(t_raw: u16, rh_raw: u16, temp_mc: i32, rh_mpercent: i32) -> [u8; 14] {
        let t_bytes = t_raw.to_be_bytes();
        let rh_bytes = rh_raw.to_be_bytes();
        let t_crc = crc8_sensirion(&t_bytes);
        let rh_crc = crc8_sensirion(&rh_bytes);
        let temp_le = temp_mc.to_le_bytes();
        let rh_le = rh_mpercent.to_le_bytes();

        [
            t_bytes[0], t_bytes[1], t_crc,
            rh_bytes[0], rh_bytes[1], rh_crc,
            temp_le[0], temp_le[1], temp_le[2], temp_le[3],
            rh_le[0], rh_le[1], rh_le[2], rh_le[3],
        ]
    }

    #[test]
    fn test_crc8_sensirion_known_values() {
        // Sensirion application note example: bytes [0xBE, 0xEF] → CRC 0x92
        assert_eq!(crc8_sensirion(&[0xBE, 0xEF]), 0x92);
    }

    #[test]
    fn test_decode_sht40_typical_reading() {
        // ~25°C, ~50%RH (representative mid-range values)
        // t_raw = 26214 → temp_mC = -45000 + 175000*26214/65535 ≈ 25003
        // rh_raw = 29360 → rh_mpercent = -6000 + 125000*29360/65535 ≈ 49979
        let temp_mc: i32 = 25003;
        let rh_mp: i32 = 49979;
        let data = build_payload(26214, 29360, temp_mc, rh_mp);

        let reading = decode_sht40(&data).unwrap();
        assert_eq!(reading.t_raw, 26214);
        assert_eq!(reading.rh_raw, 29360);
        assert!((reading.temperature_c - 25.003).abs() < 0.001);
        assert!((reading.humidity_pct - 49.979).abs() < 0.001);
    }

    #[test]
    fn test_decode_sht40_negative_temperature() {
        // Sub-zero reading
        let temp_mc: i32 = -10500;
        let rh_mp: i32 = 80000;
        let data = build_payload(5000, 45000, temp_mc, rh_mp);

        let reading = decode_sht40(&data).unwrap();
        assert!((reading.temperature_c - (-10.5)).abs() < 0.001);
        assert!((reading.humidity_pct - 80.0).abs() < 0.001);
    }

    #[test]
    fn test_decode_sht40_wrong_length() {
        assert!(decode_sht40(&[0x00; 13]).is_err());
        assert!(decode_sht40(&[0x00; 15]).is_err());
        assert!(decode_sht40(&[]).is_err());
    }

    #[test]
    fn test_decode_sht40_bad_temp_crc() {
        let mut data = build_payload(26214, 29360, 25003, 49979);
        data[2] ^= 0xFF; // corrupt temperature CRC
        assert_eq!(decode_sht40(&data), Err("temperature CRC mismatch"));
    }

    #[test]
    fn test_decode_sht40_bad_rh_crc() {
        let mut data = build_payload(26214, 29360, 25003, 49979);
        data[5] ^= 0xFF; // corrupt humidity CRC
        assert_eq!(decode_sht40(&data), Err("humidity CRC mismatch"));
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
                Value::Bytes(vec![0x00; 14]),
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
    fn test_read_message_truncated_payload() {
        let mut data = vec![0x00, 0x00, 0x00, 0x0A]; // length = 10
        data.extend_from_slice(&[0x01, 0x02, 0x03]); // only 3 bytes
        let mut cursor = io::Cursor::new(data);
        let result = read_message(&mut cursor);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn test_read_message_exceeds_1mb_limit() {
        let length: u32 = 1_048_577; // 1 MB + 1
        let mut data = Vec::new();
        data.extend_from_slice(&length.to_be_bytes());
        let mut cursor = io::Cursor::new(data);
        let result = read_message(&mut cursor);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
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
