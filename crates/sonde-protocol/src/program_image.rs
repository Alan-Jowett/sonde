use alloc::vec::Vec;

use ciborium::Value;

use crate::constants::*;
use crate::error::{DecodeError, EncodeError};
use crate::traits::Sha256Provider;

#[cfg(feature = "alloc")]
use alloc::format;

#[derive(Debug, Clone, PartialEq)]
pub struct MapDef {
    pub map_type: u32,
    pub key_size: u32,
    pub value_size: u32,
    pub max_entries: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProgramImage {
    pub bytecode: Vec<u8>,
    pub maps: Vec<MapDef>,
}

impl ProgramImage {
    /// Encode using CBOR deterministic encoding (RFC 8949 §4.2).
    /// Keys are sorted in ascending numeric order with minimal-length encoding.
    pub fn encode_deterministic(&self) -> Result<Vec<u8>, EncodeError> {
        let map_values: Vec<Value> = self
            .maps
            .iter()
            .map(|m| {
                // Keys in ascending order: 1, 2, 3, 4
                Value::Map(alloc::vec![
                    (
                        Value::Integer((MAP_KEY_TYPE as i64).into()),
                        Value::Integer((m.map_type as i64).into()),
                    ),
                    (
                        Value::Integer((MAP_KEY_KEY_SIZE as i64).into()),
                        Value::Integer((m.key_size as i64).into()),
                    ),
                    (
                        Value::Integer((MAP_KEY_VALUE_SIZE as i64).into()),
                        Value::Integer((m.value_size as i64).into()),
                    ),
                    (
                        Value::Integer((MAP_KEY_MAX_ENTRIES as i64).into()),
                        Value::Integer((m.max_entries as i64).into()),
                    ),
                ])
            })
            .collect();

        // Outer map keys in ascending order: 1 (bytecode), 2 (maps)
        let outer = Value::Map(alloc::vec![
            (
                Value::Integer((IMG_KEY_BYTECODE as i64).into()),
                Value::Bytes(self.bytecode.clone()),
            ),
            (
                Value::Integer((IMG_KEY_MAPS as i64).into()),
                Value::Array(map_values),
            ),
        ]);

        let mut buf = Vec::new();
        ciborium::into_writer(&outer, &mut buf)
            .map_err(|e| EncodeError::CborError(format!("{}", e)))?;
        Ok(buf)
    }

    pub fn decode(cbor: &[u8]) -> Result<Self, DecodeError> {
        let value: Value =
            ciborium::from_reader(cbor).map_err(|e| DecodeError::CborError(format!("{}", e)))?;

        let fields = match &value {
            Value::Map(pairs) => pairs,
            _ => return Err(DecodeError::CborError(alloc::string::String::from("expected CBOR map"))),
        };

        let mut bytecode: Option<Vec<u8>> = None;
        let mut maps: Vec<MapDef> = Vec::new();

        for (k, v) in fields {
            let key = k
                .as_integer()
                .and_then(|i| u64::try_from(i).ok())
                .unwrap_or(u64::MAX);

            match key {
                IMG_KEY_BYTECODE => {
                    bytecode = Some(
                        v.as_bytes()
                            .ok_or(DecodeError::InvalidFieldType(IMG_KEY_BYTECODE))?
                            .to_vec(),
                    );
                }
                IMG_KEY_MAPS => {
                    let arr = v
                        .as_array()
                        .ok_or(DecodeError::InvalidFieldType(IMG_KEY_MAPS))?;
                    for map_val in arr {
                        let map_fields = map_val
                            .as_map()
                            .ok_or(DecodeError::InvalidFieldType(IMG_KEY_MAPS))?;

                        let mut map_type = None;
                        let mut key_size = None;
                        let mut value_size = None;
                        let mut max_entries = None;

                        for (mk, mv) in map_fields {
                            let mkey = mk
                                .as_integer()
                                .and_then(|i| u64::try_from(i).ok())
                                .unwrap_or(u64::MAX);
                            let mval = mv
                                .as_integer()
                                .and_then(|i| u64::try_from(i).ok())
                                .map(|v| v as u32);

                            match mkey {
                                MAP_KEY_TYPE => map_type = mval,
                                MAP_KEY_KEY_SIZE => key_size = mval,
                                MAP_KEY_VALUE_SIZE => value_size = mval,
                                MAP_KEY_MAX_ENTRIES => max_entries = mval,
                                _ => {} // ignore unknown keys
                            }
                        }

                        maps.push(MapDef {
                            map_type: map_type
                                .ok_or(DecodeError::MissingField(MAP_KEY_TYPE))?,
                            key_size: key_size
                                .ok_or(DecodeError::MissingField(MAP_KEY_KEY_SIZE))?,
                            value_size: value_size
                                .ok_or(DecodeError::MissingField(MAP_KEY_VALUE_SIZE))?,
                            max_entries: max_entries
                                .ok_or(DecodeError::MissingField(MAP_KEY_MAX_ENTRIES))?,
                        });
                    }
                }
                _ => {} // ignore unknown keys
            }
        }

        Ok(ProgramImage {
            bytecode: bytecode.ok_or(DecodeError::MissingField(IMG_KEY_BYTECODE))?,
            maps,
        })
    }
}

pub fn program_hash(image_cbor: &[u8], sha: &impl Sha256Provider) -> [u8; 32] {
    sha.hash(image_cbor)
}
