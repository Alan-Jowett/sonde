// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use crate::error::PairingError;
use crate::validation::compute_key_hint;
use ciborium::Value;
use zeroize::Zeroizing;

/// Fields of a decoded PairingRequest.
#[derive(Debug)]
pub struct PairingRequestFields {
    pub node_id: String,
    pub node_key_hint: u16,
    pub node_psk: Zeroizing<[u8; 32]>,
    pub rf_channel: u8,
    pub sensors: Vec<String>,
    pub timestamp: i64,
}

/// Encode a PairingRequest as deterministic CBOR with integer keys.
///
/// Map layout:
/// - 1: node_id (text)
/// - 2: node_key_hint (unsigned)
/// - 3: node_psk (bytes, 32)
/// - 4: rf_channel (unsigned)
/// - 5: sensors (array of text)
/// - 6: timestamp (integer)
pub fn encode_pairing_request(
    node_id: &str,
    node_psk: &[u8; 32],
    rf_channel: u8,
    sensors: &[&str],
    timestamp: i64,
) -> Result<Vec<u8>, PairingError> {
    let node_key_hint = compute_key_hint(node_psk);

    // Build CBOR map with integer keys in sorted order (1..6)
    let map = Value::Map(vec![
        (Value::Integer(1.into()), Value::Text(node_id.to_string())),
        (
            Value::Integer(2.into()),
            Value::Integer(node_key_hint.into()),
        ),
        (Value::Integer(3.into()), Value::Bytes(node_psk.to_vec())),
        (Value::Integer(4.into()), Value::Integer(rf_channel.into())),
        (
            Value::Integer(5.into()),
            Value::Array(sensors.iter().map(|s| Value::Text(s.to_string())).collect()),
        ),
        (Value::Integer(6.into()), Value::Integer(timestamp.into())),
    ]);

    let mut buf = Vec::new();
    ciborium::into_writer(&map, &mut buf)
        .map_err(|e| PairingError::CborDecodeFailed(format!("CBOR encode failed: {e}")))?;
    Ok(buf)
}

/// Decode a PairingRequest from CBOR.
pub fn decode_pairing_request(data: &[u8]) -> Result<PairingRequestFields, PairingError> {
    let value: Value = ciborium::from_reader(data)
        .map_err(|e| PairingError::CborDecodeFailed(format!("CBOR decode failed: {e}")))?;

    let map = match value {
        Value::Map(m) => m,
        _ => return Err(PairingError::CborDecodeFailed("expected CBOR map".into())),
    };

    let mut node_id = None;
    let mut node_key_hint = None;
    let mut node_psk = None;
    let mut rf_channel = None;
    let mut sensors = None;
    let mut timestamp = None;

    for (k, v) in &map {
        let key = match k {
            Value::Integer(i) => {
                let val: i128 = (*i).into();
                val as u64
            }
            _ => continue,
        };
        match key {
            1 => {
                node_id = match v {
                    Value::Text(s) => Some(s.clone()),
                    _ => {
                        return Err(PairingError::CborDecodeFailed(
                            "key 1 (node_id) must be text".into(),
                        ))
                    }
                };
            }
            2 => {
                node_key_hint = match v {
                    Value::Integer(i) => {
                        let val: i128 = (*i).into();
                        Some(val as u16)
                    }
                    _ => {
                        return Err(PairingError::CborDecodeFailed(
                            "key 2 (node_key_hint) must be integer".into(),
                        ))
                    }
                };
            }
            3 => {
                node_psk = match v {
                    Value::Bytes(b) => {
                        if b.len() != 32 {
                            return Err(PairingError::CborDecodeFailed(format!(
                                "key 3 (node_psk) must be 32 bytes, got {}",
                                b.len()
                            )));
                        }
                        let mut arr = Zeroizing::new([0u8; 32]);
                        arr.copy_from_slice(b);
                        Some(arr)
                    }
                    _ => {
                        return Err(PairingError::CborDecodeFailed(
                            "key 3 (node_psk) must be bytes".into(),
                        ))
                    }
                };
            }
            4 => {
                rf_channel = match v {
                    Value::Integer(i) => {
                        let val: i128 = (*i).into();
                        Some(val as u8)
                    }
                    _ => {
                        return Err(PairingError::CborDecodeFailed(
                            "key 4 (rf_channel) must be integer".into(),
                        ))
                    }
                };
            }
            5 => {
                sensors = match v {
                    Value::Array(arr) => {
                        let mut result = Vec::new();
                        for item in arr {
                            match item {
                                Value::Text(s) => result.push(s.clone()),
                                _ => {
                                    return Err(PairingError::CborDecodeFailed(
                                        "key 5 (sensors) elements must be text".into(),
                                    ))
                                }
                            }
                        }
                        Some(result)
                    }
                    _ => {
                        return Err(PairingError::CborDecodeFailed(
                            "key 5 (sensors) must be array".into(),
                        ))
                    }
                };
            }
            6 => {
                timestamp = match v {
                    Value::Integer(i) => {
                        let val: i128 = (*i).into();
                        Some(val as i64)
                    }
                    _ => {
                        return Err(PairingError::CborDecodeFailed(
                            "key 6 (timestamp) must be integer".into(),
                        ))
                    }
                };
            }
            _ => {} // ignore unknown keys
        }
    }

    Ok(PairingRequestFields {
        node_id: node_id
            .ok_or_else(|| PairingError::CborDecodeFailed("missing key 1 (node_id)".into()))?,
        node_key_hint: node_key_hint.ok_or_else(|| {
            PairingError::CborDecodeFailed("missing key 2 (node_key_hint)".into())
        })?,
        node_psk: node_psk
            .ok_or_else(|| PairingError::CborDecodeFailed("missing key 3 (node_psk)".into()))?,
        rf_channel: rf_channel
            .ok_or_else(|| PairingError::CborDecodeFailed("missing key 4 (rf_channel)".into()))?,
        sensors: sensors
            .ok_or_else(|| PairingError::CborDecodeFailed("missing key 5 (sensors)".into()))?,
        timestamp: timestamp
            .ok_or_else(|| PairingError::CborDecodeFailed("missing key 6 (timestamp)".into()))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_pairing_request() {
        let psk = [0x42u8; 32];
        let sensors = ["temp", "humidity"];
        let encoded = encode_pairing_request("sensor-1", &psk, 6, &sensors, 1700000000).unwrap();

        let decoded = decode_pairing_request(&encoded).unwrap();
        assert_eq!(decoded.node_id, "sensor-1");
        assert_eq!(decoded.node_key_hint, compute_key_hint(&psk));
        assert_eq!(*decoded.node_psk, psk);
        assert_eq!(decoded.rf_channel, 6);
        assert_eq!(decoded.sensors, vec!["temp", "humidity"]);
        assert_eq!(decoded.timestamp, 1700000000);
    }

    #[test]
    fn encode_empty_sensors() {
        let psk = [0x42u8; 32];
        let sensors: &[&str] = &[];
        let encoded = encode_pairing_request("node-x", &psk, 1, sensors, 0).unwrap();

        let decoded = decode_pairing_request(&encoded).unwrap();
        assert!(decoded.sensors.is_empty());
    }

    #[test]
    fn decode_invalid_cbor() {
        assert!(decode_pairing_request(&[0xFF, 0xFF]).is_err());
    }

    #[test]
    fn decode_wrong_type() {
        // Encode an integer instead of a map
        let mut buf = Vec::new();
        ciborium::into_writer(&Value::Integer(42.into()), &mut buf).unwrap();
        assert!(decode_pairing_request(&buf).is_err());
    }

    #[test]
    fn deterministic_encoding() {
        let psk = [0x42u8; 32];
        let sensors = ["a", "b"];
        let enc1 = encode_pairing_request("n1", &psk, 3, &sensors, 100).unwrap();
        let enc2 = encode_pairing_request("n1", &psk, 3, &sensors, 100).unwrap();
        assert_eq!(enc1, enc2, "encoding must be deterministic");
    }
}
