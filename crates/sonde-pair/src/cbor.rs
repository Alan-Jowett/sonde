// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use crate::error::PairingError;
use crate::types::SensorDescriptor;
use crate::validation::compute_key_hint;
use ciborium::Value;
use zeroize::{Zeroize, Zeroizing};

/// Fields of a decoded PairingRequest.
#[derive(Debug)]
pub struct PairingRequestFields {
    pub node_id: String,
    pub node_key_hint: u16,
    pub node_psk: Zeroizing<[u8; 32]>,
    pub rf_channel: u8,
    pub sensors: Vec<SensorDescriptor>,
    pub timestamp: i64,
}

/// Zeroize all `Value::Bytes` buffers in a ciborium Value tree so key material
/// does not linger in freed heap memory.
fn zeroize_cbor_values(value: &mut Value) {
    match value {
        Value::Bytes(b) => b.as_mut_slice().zeroize(),
        Value::Array(arr) => arr.iter_mut().for_each(zeroize_cbor_values),
        Value::Map(pairs) => {
            for (k, v) in pairs {
                zeroize_cbor_values(k);
                zeroize_cbor_values(v);
            }
        }
        _ => {}
    }
}

/// Encode a PairingRequest as deterministic CBOR with integer keys.
///
/// Map layout:
/// - 1: node_id (text)
/// - 2: node_key_hint (unsigned)
/// - 3: node_psk (bytes, 32)
/// - 4: rf_channel (unsigned)
/// - 5: sensors (array of maps `{1: sensor_type, 2: sensor_id, 3: label?}`)
/// - 6: timestamp (integer)
///
/// Returns `Zeroizing<Vec<u8>>` because the CBOR buffer contains key material
/// (node PSK) that must be zeroized on drop.
pub fn encode_pairing_request(
    node_id: &str,
    node_psk: &[u8; 32],
    rf_channel: u8,
    sensors: &[SensorDescriptor],
    timestamp: i64,
) -> Result<Zeroizing<Vec<u8>>, PairingError> {
    let node_key_hint = compute_key_hint(node_psk);

    let sensor_values: Vec<Value> = sensors
        .iter()
        .map(|s| {
            let mut map = vec![
                (
                    Value::Integer(1.into()),
                    Value::Integer(s.sensor_type.into()),
                ),
                (Value::Integer(2.into()), Value::Integer(s.sensor_id.into())),
            ];
            if let Some(ref label) = s.label {
                map.push((Value::Integer(3.into()), Value::Text(label.clone())));
            }
            Value::Map(map)
        })
        .collect();

    // Build CBOR map with integer keys in sorted order (1..6)
    let mut map = Value::Map(vec![
        (Value::Integer(1.into()), Value::Text(node_id.to_string())),
        (
            Value::Integer(2.into()),
            Value::Integer(node_key_hint.into()),
        ),
        (Value::Integer(3.into()), Value::Bytes(node_psk.to_vec())),
        (Value::Integer(4.into()), Value::Integer(rf_channel.into())),
        (Value::Integer(5.into()), Value::Array(sensor_values)),
        (Value::Integer(6.into()), Value::Integer(timestamp.into())),
    ]);

    let mut buf = Vec::new();
    ciborium::into_writer(&map, &mut buf)
        .map_err(|e| PairingError::CborEncodeFailed(format!("{e}")))?;
    zeroize_cbor_values(&mut map);
    Ok(Zeroizing::new(buf))
}

/// Decode a PairingRequest from CBOR.
///
/// The parsed CBOR Value tree is zeroized before returning (regardless of
/// success or failure) to prevent key material from lingering in heap memory.
pub fn decode_pairing_request(data: &[u8]) -> Result<PairingRequestFields, PairingError> {
    let value: Value =
        ciborium::from_reader(data).map_err(|e| PairingError::CborDecodeFailed(format!("{e}")))?;

    let map = match value {
        Value::Map(m) => m,
        _ => return Err(PairingError::CborDecodeFailed("expected CBOR map".into())),
    };

    let result = decode_from_map(&map);

    // Zeroize the Value tree (contains PSK in Value::Bytes) on all exit paths.
    let mut map_value = Value::Map(map);
    zeroize_cbor_values(&mut map_value);

    result
}

fn decode_from_map(map: &[(Value, Value)]) -> Result<PairingRequestFields, PairingError> {
    let mut node_id = None;
    let mut node_key_hint = None;
    let mut node_psk = None;
    let mut rf_channel = None;
    let mut sensors = None;
    let mut timestamp = None;

    for (k, v) in map {
        let key = match k {
            Value::Integer(i) => {
                let val: i128 = (*i).into();
                if val < 0 {
                    continue;
                }
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
                        if val < 0 || val > u16::MAX as i128 {
                            return Err(PairingError::CborDecodeFailed(format!(
                                "key 2 (node_key_hint) out of range: {val}"
                            )));
                        }
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
                        if !(1..=13).contains(&val) {
                            return Err(PairingError::CborDecodeFailed(format!(
                                "key 4 (rf_channel) out of range: {val}, must be 1-13"
                            )));
                        }
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
                                Value::Map(map_entries) => {
                                    let mut sensor_type = None;
                                    let mut sensor_id = None;
                                    let mut label = None;
                                    for (mk, mv) in map_entries {
                                        let mkey = match mk {
                                            Value::Integer(i) => {
                                                let val: i128 = (*i).into();
                                                if val < 0 {
                                                    continue;
                                                }
                                                val as u64
                                            }
                                            _ => continue,
                                        };
                                        match mkey {
                                            1 => {
                                                sensor_type = match mv {
                                                    Value::Integer(i) => {
                                                        let val: i128 = (*i).into();
                                                        if !(0..=255).contains(&val) {
                                                            return Err(
                                                                PairingError::CborDecodeFailed(
                                                                    format!(
                                                                    "sensor_type out of range: {val}"
                                                                ),
                                                                ),
                                                            );
                                                        }
                                                        Some(val as u8)
                                                    }
                                                    _ => {
                                                        return Err(PairingError::CborDecodeFailed(
                                                            "sensor_type must be integer".into(),
                                                        ))
                                                    }
                                                }
                                            }
                                            2 => {
                                                sensor_id = match mv {
                                                    Value::Integer(i) => {
                                                        let val: i128 = (*i).into();
                                                        if !(0..=255).contains(&val) {
                                                            return Err(
                                                                PairingError::CborDecodeFailed(
                                                                    format!(
                                                                    "sensor_id out of range: {val}"
                                                                ),
                                                                ),
                                                            );
                                                        }
                                                        Some(val as u8)
                                                    }
                                                    _ => {
                                                        return Err(PairingError::CborDecodeFailed(
                                                            "sensor_id must be integer".into(),
                                                        ))
                                                    }
                                                }
                                            }
                                            3 => {
                                                label = match mv {
                                                    Value::Text(s) => Some(s.clone()),
                                                    _ => {
                                                        return Err(PairingError::CborDecodeFailed(
                                                            "sensor label must be text".into(),
                                                        ))
                                                    }
                                                }
                                            }
                                            _ => {}
                                        }
                                    }
                                    let st = sensor_type.ok_or_else(|| {
                                        PairingError::CborDecodeFailed(
                                            "sensor missing sensor_type (key 1)".into(),
                                        )
                                    })?;
                                    let si = sensor_id.ok_or_else(|| {
                                        PairingError::CborDecodeFailed(
                                            "sensor missing sensor_id (key 2)".into(),
                                        )
                                    })?;
                                    result.push(SensorDescriptor {
                                        sensor_type: st,
                                        sensor_id: si,
                                        label,
                                    });
                                }
                                _ => {
                                    return Err(PairingError::CborDecodeFailed(
                                        "key 5 (sensors) elements must be CBOR maps".into(),
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
                        if val < i64::MIN as i128 || val > i64::MAX as i128 {
                            return Err(PairingError::CborDecodeFailed(format!(
                                "key 6 (timestamp) out of i64 range: {val}"
                            )));
                        }
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
        sensors: sensors.unwrap_or_default(),
        timestamp: timestamp
            .ok_or_else(|| PairingError::CborDecodeFailed("missing key 6 (timestamp)".into()))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_sensors() -> Vec<SensorDescriptor> {
        vec![
            SensorDescriptor {
                sensor_type: 1,
                sensor_id: 0x48,
                label: Some("temp".into()),
            },
            SensorDescriptor {
                sensor_type: 2,
                sensor_id: 3,
                label: Some("humidity".into()),
            },
        ]
    }

    #[test]
    fn round_trip_pairing_request() {
        let psk = [0x42u8; 32];
        let sensors = test_sensors();
        let encoded = encode_pairing_request("sensor-1", &psk, 6, &sensors, 1700000000).unwrap();

        let decoded = decode_pairing_request(&encoded).unwrap();
        assert_eq!(decoded.node_id, "sensor-1");
        assert_eq!(decoded.node_key_hint, compute_key_hint(&psk));
        assert_eq!(*decoded.node_psk, psk);
        assert_eq!(decoded.rf_channel, 6);
        assert_eq!(decoded.sensors, sensors);
        assert_eq!(decoded.timestamp, 1700000000);
    }

    #[test]
    fn encode_empty_sensors() {
        let psk = [0x42u8; 32];
        let sensors: Vec<SensorDescriptor> = vec![];
        let encoded = encode_pairing_request("node-x", &psk, 1, &sensors, 0).unwrap();

        let decoded = decode_pairing_request(&encoded).unwrap();
        assert!(decoded.sensors.is_empty());
    }

    #[test]
    fn encode_sensor_without_label() {
        let psk = [0x42u8; 32];
        let sensors = vec![SensorDescriptor {
            sensor_type: 3,
            sensor_id: 5,
            label: None,
        }];
        let encoded = encode_pairing_request("node-x", &psk, 1, &sensors, 0).unwrap();

        let decoded = decode_pairing_request(&encoded).unwrap();
        assert_eq!(decoded.sensors.len(), 1);
        assert_eq!(decoded.sensors[0].sensor_type, 3);
        assert_eq!(decoded.sensors[0].sensor_id, 5);
        assert!(decoded.sensors[0].label.is_none());
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
        let sensors = vec![SensorDescriptor {
            sensor_type: 1,
            sensor_id: 0x10,
            label: Some("a".into()),
        }];
        let enc1 = encode_pairing_request("n1", &psk, 3, &sensors, 100).unwrap();
        let enc2 = encode_pairing_request("n1", &psk, 3, &sensors, 100).unwrap();
        assert_eq!(enc1, enc2, "encoding must be deterministic");
    }
}
