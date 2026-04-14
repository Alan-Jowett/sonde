// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use ciborium::Value;

use crate::constants::*;
use crate::error::{DecodeError, EncodeError};

#[derive(Debug, Clone, PartialEq)]
pub enum NodeMessage {
    Wake {
        firmware_abi_version: u32,
        program_hash: Vec<u8>,
        battery_mv: u32,
        firmware_version: String,
        blob: Option<Vec<u8>>,
    },
    GetChunk {
        chunk_index: u32,
    },
    ProgramAck {
        program_hash: Vec<u8>,
    },
    AppData {
        blob: Vec<u8>,
    },
    DiagRequest {
        diagnostic_type: u8,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum CommandPayload {
    Nop,
    UpdateProgram {
        program_hash: Vec<u8>,
        program_size: u32,
        chunk_size: u32,
        chunk_count: u32,
    },
    RunEphemeral {
        program_hash: Vec<u8>,
        program_size: u32,
        chunk_size: u32,
        chunk_count: u32,
    },
    UpdateSchedule {
        interval_s: u32,
    },
    Reboot,
}

impl CommandPayload {
    /// Returns the command_type code for this payload variant.
    pub fn command_type(&self) -> u8 {
        match self {
            CommandPayload::Nop => CMD_NOP,
            CommandPayload::UpdateProgram { .. } => CMD_UPDATE_PROGRAM,
            CommandPayload::RunEphemeral { .. } => CMD_RUN_EPHEMERAL,
            CommandPayload::UpdateSchedule { .. } => CMD_UPDATE_SCHEDULE,
            CommandPayload::Reboot => CMD_REBOOT,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum GatewayMessage {
    Command {
        starting_seq: u64,
        timestamp_ms: u64,
        payload: CommandPayload,
        blob: Option<Vec<u8>>,
    },
    Chunk {
        chunk_index: u32,
        chunk_data: Vec<u8>,
    },
    AppDataReply {
        blob: Vec<u8>,
    },
    DiagReply {
        diagnostic_type: u8,
        rssi_dbm: i8,
        signal_quality: u8,
    },
}

fn cbor_encode_map(pairs: &[(u64, Value)]) -> Result<Vec<u8>, EncodeError> {
    let map: Vec<(Value, Value)> = pairs
        .iter()
        .map(|(k, v)| (Value::Integer((*k).into()), v.clone()))
        .collect();
    let value = Value::Map(map);
    let mut buf = Vec::new();
    ciborium::into_writer(&value, &mut buf)
        .map_err(|e| EncodeError::CborError(format!("{}", e)))?;
    Ok(buf)
}

fn cbor_decode_map(cbor: &[u8]) -> Result<Vec<(u64, Value)>, DecodeError> {
    let value: Value =
        ciborium::from_reader(cbor).map_err(|e| DecodeError::CborError(format!("{}", e)))?;
    match value {
        Value::Map(pairs) => {
            let mut result = Vec::new();
            for (k, v) in pairs {
                if let Some(key) = k.as_integer() {
                    let key_u64: u64 = key
                        .try_into()
                        .map_err(|_| DecodeError::CborError(String::from("negative CBOR key")))?;
                    result.push((key_u64, v));
                }
                // Unknown/non-integer keys are silently ignored (forward compatibility)
            }
            Ok(result)
        }
        _ => Err(DecodeError::CborError(String::from("expected CBOR map"))),
    }
}

fn get_field(fields: &[(u64, Value)], key: u64) -> Result<&Value, DecodeError> {
    fields
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v)
        .ok_or(DecodeError::MissingField(key))
}

fn get_uint(fields: &[(u64, Value)], key: u64) -> Result<u64, DecodeError> {
    let val = get_field(fields, key)?;
    val.as_integer()
        .and_then(|i| u64::try_from(i).ok())
        .ok_or(DecodeError::InvalidFieldType(key))
}

fn get_u32(fields: &[(u64, Value)], key: u64) -> Result<u32, DecodeError> {
    let v = get_uint(fields, key)?;
    u32::try_from(v).map_err(|_| DecodeError::InvalidFieldType(key))
}

fn get_u8(fields: &[(u64, Value)], key: u64) -> Result<u8, DecodeError> {
    let v = get_uint(fields, key)?;
    u8::try_from(v).map_err(|_| DecodeError::InvalidFieldType(key))
}

fn get_i8(fields: &[(u64, Value)], key: u64) -> Result<i8, DecodeError> {
    let val = get_field(fields, key)?;
    let i = val.as_integer().ok_or(DecodeError::InvalidFieldType(key))?;
    let v: i64 = i
        .try_into()
        .map_err(|_| DecodeError::InvalidFieldType(key))?;
    i8::try_from(v).map_err(|_| DecodeError::InvalidFieldType(key))
}

fn get_bytes(fields: &[(u64, Value)], key: u64) -> Result<Vec<u8>, DecodeError> {
    let val = get_field(fields, key)?;
    val.as_bytes()
        .map(|b| b.to_vec())
        .ok_or(DecodeError::InvalidFieldType(key))
}

fn get_bytes_optional(fields: &[(u64, Value)], key: u64) -> Result<Option<Vec<u8>>, DecodeError> {
    match fields.iter().find(|(k, _)| *k == key) {
        None => Ok(None),
        Some((_, val)) => val
            .as_bytes()
            .map(|b| Some(b.to_vec()))
            .ok_or(DecodeError::InvalidFieldType(key)),
    }
}

/// Maximum length for string fields decoded from the wire (e.g., firmware_version).
/// Prevents unbounded allocation from malicious/buggy peers.
const MAX_STRING_FIELD_LEN: usize = 32;

fn get_string(fields: &[(u64, Value)], key: u64) -> Result<String, DecodeError> {
    let val = get_field(fields, key)?;
    match val.as_text() {
        Some(s) if s.len() <= MAX_STRING_FIELD_LEN && s.is_ascii() => Ok(String::from(s)),
        Some(_) => Err(DecodeError::InvalidFieldType(key)),
        None => Err(DecodeError::InvalidFieldType(key)),
    }
}

fn decode_nested_map(fields: &[(u64, Value)], key: u64) -> Result<Vec<(u64, Value)>, DecodeError> {
    let val = get_field(fields, key)?;
    match val {
        Value::Map(pairs) => {
            let mut result = Vec::new();
            for (k, v) in pairs {
                if let Some(ik) = k.as_integer() {
                    if let Ok(key_u64) = u64::try_from(ik) {
                        result.push((key_u64, v.clone()));
                    }
                }
            }
            Ok(result)
        }
        _ => Err(DecodeError::InvalidFieldType(key)),
    }
}

/// Encode a u64 as a CBOR unsigned integer Value.
/// ciborium's Integer supports From<u64> directly, preserving the full range.
fn uint_val(v: u64) -> Value {
    Value::Integer(v.into())
}

/// Encode a u32 as a CBOR unsigned integer Value.
fn u32_val(v: u32) -> Value {
    Value::Integer(v.into())
}

/// Encode a u8 as a CBOR unsigned integer Value.
fn u8_val(v: u8) -> Value {
    Value::Integer(v.into())
}

/// Encode an i8 as a CBOR integer Value (signed).
fn i8_val(v: i8) -> Value {
    Value::Integer((v as i64).into())
}

impl NodeMessage {
    pub fn encode(&self) -> Result<Vec<u8>, EncodeError> {
        let pairs: Vec<(u64, Value)> = match self {
            NodeMessage::Wake {
                firmware_abi_version,
                program_hash,
                battery_mv,
                firmware_version,
                blob,
            } => {
                let mut pairs = alloc::vec![
                    (KEY_FIRMWARE_ABI_VERSION, u32_val(*firmware_abi_version)),
                    (KEY_PROGRAM_HASH, Value::Bytes(program_hash.clone())),
                    (KEY_BATTERY_MV, u32_val(*battery_mv)),
                ];
                if let Some(b) = blob {
                    pairs.push((KEY_BLOB, Value::Bytes(b.clone())));
                }
                pairs.push((KEY_FIRMWARE_VERSION, Value::Text(firmware_version.clone())));
                pairs
            }
            NodeMessage::GetChunk { chunk_index } => {
                alloc::vec![(KEY_CHUNK_INDEX, u32_val(*chunk_index))]
            }
            NodeMessage::ProgramAck { program_hash } => {
                alloc::vec![(KEY_PROGRAM_HASH, Value::Bytes(program_hash.clone()))]
            }
            NodeMessage::AppData { blob } => {
                alloc::vec![(KEY_BLOB, Value::Bytes(blob.clone()))]
            }
            NodeMessage::DiagRequest { diagnostic_type } => {
                alloc::vec![(DIAG_KEY_DIAGNOSTIC_TYPE, u8_val(*diagnostic_type))]
            }
        };
        cbor_encode_map(&pairs)
    }

    pub fn decode(msg_type: u8, cbor: &[u8]) -> Result<Self, DecodeError> {
        let fields = cbor_decode_map(cbor)?;
        match msg_type {
            MSG_WAKE => Ok(NodeMessage::Wake {
                firmware_abi_version: get_u32(&fields, KEY_FIRMWARE_ABI_VERSION)?,
                program_hash: get_bytes(&fields, KEY_PROGRAM_HASH)?,
                battery_mv: get_u32(&fields, KEY_BATTERY_MV)?,
                firmware_version: get_string(&fields, KEY_FIRMWARE_VERSION)?,
                blob: get_bytes_optional(&fields, KEY_BLOB)?,
            }),
            MSG_GET_CHUNK => Ok(NodeMessage::GetChunk {
                chunk_index: get_u32(&fields, KEY_CHUNK_INDEX)?,
            }),
            MSG_PROGRAM_ACK => Ok(NodeMessage::ProgramAck {
                program_hash: get_bytes(&fields, KEY_PROGRAM_HASH)?,
            }),
            MSG_APP_DATA => Ok(NodeMessage::AppData {
                blob: get_bytes(&fields, KEY_BLOB)?,
            }),
            MSG_DIAG_REQUEST => Ok(NodeMessage::DiagRequest {
                diagnostic_type: get_u8(&fields, DIAG_KEY_DIAGNOSTIC_TYPE)?,
            }),
            _ => Err(DecodeError::InvalidMsgType(msg_type)),
        }
    }

    pub fn msg_type(&self) -> u8 {
        match self {
            NodeMessage::Wake { .. } => MSG_WAKE,
            NodeMessage::GetChunk { .. } => MSG_GET_CHUNK,
            NodeMessage::ProgramAck { .. } => MSG_PROGRAM_ACK,
            NodeMessage::AppData { .. } => MSG_APP_DATA,
            NodeMessage::DiagRequest { .. } => MSG_DIAG_REQUEST,
        }
    }
}

impl GatewayMessage {
    pub fn encode(&self) -> Result<Vec<u8>, EncodeError> {
        let pairs: Vec<(u64, Value)> = match self {
            GatewayMessage::Command {
                starting_seq,
                timestamp_ms,
                payload,
                blob,
            } => {
                // Deterministic CBOR (RFC 8949 §4.2): keys present in the map must be in ascending order.
                // KEY_COMMAND_TYPE=4, KEY_PAYLOAD=5 (optional), KEY_BLOB=10 (optional, NOP only),
                // KEY_STARTING_SEQ=13, KEY_TIMESTAMP_MS=14
                let payload_val = match payload {
                    CommandPayload::Nop | CommandPayload::Reboot => None,
                    CommandPayload::UpdateProgram {
                        program_hash,
                        program_size,
                        chunk_size,
                        chunk_count,
                    }
                    | CommandPayload::RunEphemeral {
                        program_hash,
                        program_size,
                        chunk_size,
                        chunk_count,
                    } => {
                        let inner = alloc::vec![
                            (
                                Value::Integer(KEY_PROGRAM_HASH.into()),
                                Value::Bytes(program_hash.clone())
                            ),
                            (
                                Value::Integer(KEY_PROGRAM_SIZE.into()),
                                u32_val(*program_size)
                            ),
                            (Value::Integer(KEY_CHUNK_SIZE.into()), u32_val(*chunk_size)),
                            (
                                Value::Integer(KEY_CHUNK_COUNT.into()),
                                u32_val(*chunk_count)
                            ),
                        ];
                        Some(Value::Map(inner))
                    }
                    CommandPayload::UpdateSchedule { interval_s } => {
                        let inner = alloc::vec![(
                            Value::Integer(KEY_INTERVAL_S.into()),
                            u32_val(*interval_s)
                        ),];
                        Some(Value::Map(inner))
                    }
                };
                let mut p = alloc::vec![(KEY_COMMAND_TYPE, u8_val(payload.command_type())),];
                if let Some(pv) = payload_val {
                    p.push((KEY_PAYLOAD, pv));
                }
                if let Some(b) = blob {
                    p.push((KEY_BLOB, Value::Bytes(b.clone())));
                }
                p.push((KEY_STARTING_SEQ, uint_val(*starting_seq)));
                p.push((KEY_TIMESTAMP_MS, uint_val(*timestamp_ms)));
                p
            }
            GatewayMessage::Chunk {
                chunk_index,
                chunk_data,
            } => {
                alloc::vec![
                    (KEY_CHUNK_INDEX, u32_val(*chunk_index)),
                    (KEY_CHUNK_DATA, Value::Bytes(chunk_data.clone())),
                ]
            }
            GatewayMessage::AppDataReply { blob } => {
                alloc::vec![(KEY_BLOB, Value::Bytes(blob.clone()))]
            }
            GatewayMessage::DiagReply {
                diagnostic_type,
                rssi_dbm,
                signal_quality,
            } => {
                alloc::vec![
                    (DIAG_KEY_DIAGNOSTIC_TYPE, u8_val(*diagnostic_type)),
                    (DIAG_KEY_RSSI_DBM, i8_val(*rssi_dbm)),
                    (DIAG_KEY_SIGNAL_QUALITY, u8_val(*signal_quality)),
                ]
            }
        };
        cbor_encode_map(&pairs)
    }

    pub fn decode(msg_type: u8, cbor: &[u8]) -> Result<Self, DecodeError> {
        let fields = cbor_decode_map(cbor)?;
        match msg_type {
            MSG_COMMAND => {
                let command_type = get_u8(&fields, KEY_COMMAND_TYPE)?;
                let starting_seq = get_uint(&fields, KEY_STARTING_SEQ)?;
                let timestamp_ms = get_uint(&fields, KEY_TIMESTAMP_MS)?;

                let payload = match command_type {
                    CMD_NOP | CMD_REBOOT => {
                        if command_type == CMD_REBOOT {
                            CommandPayload::Reboot
                        } else {
                            CommandPayload::Nop
                        }
                    }
                    CMD_UPDATE_PROGRAM | CMD_RUN_EPHEMERAL => {
                        let inner_fields = decode_nested_map(&fields, KEY_PAYLOAD)?;
                        let program_hash = get_bytes(&inner_fields, KEY_PROGRAM_HASH)?;
                        let program_size = get_u32(&inner_fields, KEY_PROGRAM_SIZE)?;
                        let chunk_size = get_u32(&inner_fields, KEY_CHUNK_SIZE)?;
                        let chunk_count = get_u32(&inner_fields, KEY_CHUNK_COUNT)?;
                        if command_type == CMD_UPDATE_PROGRAM {
                            CommandPayload::UpdateProgram {
                                program_hash,
                                program_size,
                                chunk_size,
                                chunk_count,
                            }
                        } else {
                            CommandPayload::RunEphemeral {
                                program_hash,
                                program_size,
                                chunk_size,
                                chunk_count,
                            }
                        }
                    }
                    CMD_UPDATE_SCHEDULE => {
                        let inner_fields = decode_nested_map(&fields, KEY_PAYLOAD)?;
                        CommandPayload::UpdateSchedule {
                            interval_s: get_u32(&inner_fields, KEY_INTERVAL_S)?,
                        }
                    }
                    _ => return Err(DecodeError::InvalidCommandType(command_type)),
                };

                // Extract blob only for NOP commands; ignore key 10 on other command types
                let blob = if command_type == CMD_NOP {
                    get_bytes_optional(&fields, KEY_BLOB)?
                } else {
                    None
                };

                Ok(GatewayMessage::Command {
                    starting_seq,
                    timestamp_ms,
                    payload,
                    blob,
                })
            }
            MSG_CHUNK => Ok(GatewayMessage::Chunk {
                chunk_index: get_u32(&fields, KEY_CHUNK_INDEX)?,
                chunk_data: get_bytes(&fields, KEY_CHUNK_DATA)?,
            }),
            MSG_APP_DATA_REPLY => Ok(GatewayMessage::AppDataReply {
                blob: get_bytes(&fields, KEY_BLOB)?,
            }),
            MSG_DIAG_REPLY => Ok(GatewayMessage::DiagReply {
                diagnostic_type: get_u8(&fields, DIAG_KEY_DIAGNOSTIC_TYPE)?,
                rssi_dbm: get_i8(&fields, DIAG_KEY_RSSI_DBM)?,
                signal_quality: get_u8(&fields, DIAG_KEY_SIGNAL_QUALITY)?,
            }),
            _ => Err(DecodeError::InvalidMsgType(msg_type)),
        }
    }

    pub fn msg_type(&self) -> u8 {
        match self {
            GatewayMessage::Command { .. } => MSG_COMMAND,
            GatewayMessage::Chunk { .. } => MSG_CHUNK,
            GatewayMessage::AppDataReply { .. } => MSG_APP_DATA_REPLY,
            GatewayMessage::DiagReply { .. } => MSG_DIAG_REPLY,
        }
    }
}
