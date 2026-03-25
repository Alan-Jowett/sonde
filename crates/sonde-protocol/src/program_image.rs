// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use alloc::vec::Vec;

use ciborium::Value;

use crate::constants::*;
use crate::error::{DecodeError, EncodeError};
use crate::traits::Sha256Provider;

use alloc::format;

#[derive(Debug, Clone, Copy, PartialEq)]
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
    /// Initial data for each map, parallel to `maps`.
    ///
    /// `map_initial_data[i]` contains the initial bytes for `maps[i]`.
    /// An empty `Vec<u8>` means the map has no initial data (zero-filled).
    /// For global variable maps (`.rodata` / `.data`), this carries the
    /// ELF section content so the node can pre-populate map memory before
    /// BPF execution.
    pub map_initial_data: Vec<Vec<u8>>,
}

impl ProgramImage {
    /// Encode using CBOR deterministic encoding (RFC 8949 §4.2).
    /// Keys are sorted in ascending numeric order with minimal-length encoding.
    pub fn encode_deterministic(&self) -> Result<Vec<u8>, EncodeError> {
        let map_values: Vec<Value> = self
            .maps
            .iter()
            .enumerate()
            .map(|(i, m)| {
                // Keys in ascending order: 1, 2, 3, 4, (5 if initial_data present)
                let mut entries = alloc::vec![
                    (
                        Value::Integer(MAP_KEY_TYPE.into()),
                        Value::Integer(m.map_type.into()),
                    ),
                    (
                        Value::Integer(MAP_KEY_KEY_SIZE.into()),
                        Value::Integer(m.key_size.into()),
                    ),
                    (
                        Value::Integer(MAP_KEY_VALUE_SIZE.into()),
                        Value::Integer(m.value_size.into()),
                    ),
                    (
                        Value::Integer(MAP_KEY_MAX_ENTRIES.into()),
                        Value::Integer(m.max_entries.into()),
                    ),
                ];
                // Include initial_data (key 5) only when non-empty.
                if let Some(data) = self.map_initial_data.get(i) {
                    if !data.is_empty() {
                        entries.push((
                            Value::Integer(MAP_KEY_INITIAL_DATA.into()),
                            Value::Bytes(data.clone()),
                        ));
                    }
                }
                Value::Map(entries)
            })
            .collect();

        // Outer map keys in ascending order: 1 (bytecode), 2 (maps)
        let outer = Value::Map(alloc::vec![
            (
                Value::Integer(IMG_KEY_BYTECODE.into()),
                Value::Bytes(self.bytecode.clone()),
            ),
            (
                Value::Integer(IMG_KEY_MAPS.into()),
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
            _ => {
                return Err(DecodeError::CborError(alloc::string::String::from(
                    "expected CBOR map",
                )))
            }
        };

        let mut bytecode: Option<Vec<u8>> = None;
        let mut maps: Vec<MapDef> = Vec::new();
        let mut map_initial_data: Vec<Vec<u8>> = Vec::new();

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
                        let mut initial_data: Vec<u8> = Vec::new();

                        for (mk, mv) in map_fields {
                            let mkey = mk
                                .as_integer()
                                .and_then(|i| u64::try_from(i).ok())
                                .unwrap_or(u64::MAX);
                            let mval = mv
                                .as_integer()
                                .and_then(|i| u64::try_from(i).ok())
                                .and_then(|v| u32::try_from(v).ok());

                            match mkey {
                                MAP_KEY_TYPE => map_type = mval,
                                MAP_KEY_KEY_SIZE => key_size = mval,
                                MAP_KEY_VALUE_SIZE => value_size = mval,
                                MAP_KEY_MAX_ENTRIES => max_entries = mval,
                                MAP_KEY_INITIAL_DATA => {
                                    initial_data = mv
                                        .as_bytes()
                                        .ok_or(DecodeError::InvalidFieldType(MAP_KEY_INITIAL_DATA))?
                                        .to_vec();
                                }
                                _ => {} // ignore unknown keys
                            }
                        }

                        maps.push(MapDef {
                            map_type: map_type.ok_or(DecodeError::MissingField(MAP_KEY_TYPE))?,
                            key_size: key_size
                                .ok_or(DecodeError::MissingField(MAP_KEY_KEY_SIZE))?,
                            value_size: value_size
                                .ok_or(DecodeError::MissingField(MAP_KEY_VALUE_SIZE))?,
                            max_entries: max_entries
                                .ok_or(DecodeError::MissingField(MAP_KEY_MAX_ENTRIES))?,
                        });
                        map_initial_data.push(initial_data);
                    }
                }
                _ => {} // ignore unknown keys
            }
        }

        Ok(ProgramImage {
            bytecode: bytecode.ok_or(DecodeError::MissingField(IMG_KEY_BYTECODE))?,
            maps,
            map_initial_data,
        })
    }
}

pub fn program_hash(image_cbor: &[u8], sha: &impl Sha256Provider) -> [u8; 32] {
    sha.hash(image_cbor)
}
