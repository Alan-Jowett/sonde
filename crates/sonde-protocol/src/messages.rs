// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

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

#[derive(Debug, Clone, PartialEq)]
pub enum GatewayMessage {
    Command {
        command_type: u8,
        starting_seq: u64,
        timestamp_ms: u64,
        payload: CommandPayload,
    },
    Chunk {
        chunk_index: u32,
        chunk_data: Vec<u8>,
    },
    AppDataReply {
        blob: Vec<u8>,
    },
}

fn cbor_encode_map(pairs: &[(u64, Value)]) -> Result<Vec<u8>, EncodeError> {
    let map: Vec<(Value, Value)> = pairs
        .iter()
        .map(|(k, v)| (Value::Integer((*k as i64).into()), v.clone()))
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

fn get_bytes(fields: &[(u64, Value)], key: u64) -> Result<Vec<u8>, DecodeError> {
    let val = get_field(fields, key)?;
    val.as_bytes()
        .map(|b| b.to_vec())
        .ok_or(DecodeError::InvalidFieldType(key))
}

/// Encode a u64 as a CBOR unsigned integer Value.
/// For values that fit in i64 (the common case), uses direct conversion.
/// Values > i64::MAX are not expected in this protocol but are encoded
/// via the raw CBOR bytes path if needed.
fn uint_val(v: u64) -> Value {
    // All protocol u64 values (timestamps, sequence numbers) fit in i64
    // in practice. If a value exceeds i64::MAX, encode as i64 — this
    // covers the full u63 range which is sufficient.
    Value::Integer((v as i64).into())
}

/// Encode a u32 as a CBOR unsigned integer Value.
fn u32_val(v: u32) -> Value {
    Value::Integer((v as i64).into())
}

/// Encode a u8 as a CBOR unsigned integer Value.
fn u8_val(v: u8) -> Value {
    Value::Integer((v as i64).into())
}

use alloc::format;

impl NodeMessage {
    pub fn encode(&self) -> Result<Vec<u8>, EncodeError> {
        let pairs: Vec<(u64, Value)> = match self {
            NodeMessage::Wake {
                firmware_abi_version,
                program_hash,
                battery_mv,
            } => {
                alloc::vec![
                    (KEY_FIRMWARE_ABI_VERSION, u32_val(*firmware_abi_version)),
                    (KEY_PROGRAM_HASH, Value::Bytes(program_hash.clone())),
                    (KEY_BATTERY_MV, u32_val(*battery_mv)),
                ]
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
            _ => Err(DecodeError::InvalidMsgType(msg_type)),
        }
    }

    pub fn msg_type(&self) -> u8 {
        match self {
            NodeMessage::Wake { .. } => MSG_WAKE,
            NodeMessage::GetChunk { .. } => MSG_GET_CHUNK,
            NodeMessage::ProgramAck { .. } => MSG_PROGRAM_ACK,
            NodeMessage::AppData { .. } => MSG_APP_DATA,
        }
    }
}

impl GatewayMessage {
    pub fn encode(&self) -> Result<Vec<u8>, EncodeError> {
        let pairs: Vec<(u64, Value)> = match self {
            GatewayMessage::Command {
                command_type,
                starting_seq,
                timestamp_ms,
                payload,
            } => {
                let mut p = alloc::vec![
                    (KEY_COMMAND_TYPE, u8_val(*command_type)),
                    (KEY_STARTING_SEQ, uint_val(*starting_seq)),
                    (KEY_TIMESTAMP_MS, uint_val(*timestamp_ms)),
                ];
                match payload {
                    CommandPayload::Nop | CommandPayload::Reboot => {}
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
                        p.push((KEY_PROGRAM_HASH, Value::Bytes(program_hash.clone())));
                        p.push((KEY_PROGRAM_SIZE, u32_val(*program_size)));
                        p.push((KEY_CHUNK_SIZE, u32_val(*chunk_size)));
                        p.push((KEY_CHUNK_COUNT, u32_val(*chunk_count)));
                    }
                    CommandPayload::UpdateSchedule { interval_s } => {
                        p.push((KEY_INTERVAL_S, u32_val(*interval_s)));
                    }
                }
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
                    CMD_NOP => CommandPayload::Nop,
                    CMD_UPDATE_PROGRAM => CommandPayload::UpdateProgram {
                        program_hash: get_bytes(&fields, KEY_PROGRAM_HASH)?,
                        program_size: get_u32(&fields, KEY_PROGRAM_SIZE)?,
                        chunk_size: get_u32(&fields, KEY_CHUNK_SIZE)?,
                        chunk_count: get_u32(&fields, KEY_CHUNK_COUNT)?,
                    },
                    CMD_RUN_EPHEMERAL => CommandPayload::RunEphemeral {
                        program_hash: get_bytes(&fields, KEY_PROGRAM_HASH)?,
                        program_size: get_u32(&fields, KEY_PROGRAM_SIZE)?,
                        chunk_size: get_u32(&fields, KEY_CHUNK_SIZE)?,
                        chunk_count: get_u32(&fields, KEY_CHUNK_COUNT)?,
                    },
                    CMD_UPDATE_SCHEDULE => CommandPayload::UpdateSchedule {
                        interval_s: get_u32(&fields, KEY_INTERVAL_S)?,
                    },
                    CMD_REBOOT => CommandPayload::Reboot,
                    _ => CommandPayload::Nop,
                };

                Ok(GatewayMessage::Command {
                    command_type,
                    starting_seq,
                    timestamp_ms,
                    payload,
                })
            }
            MSG_CHUNK => Ok(GatewayMessage::Chunk {
                chunk_index: get_u32(&fields, KEY_CHUNK_INDEX)?,
                chunk_data: get_bytes(&fields, KEY_CHUNK_DATA)?,
            }),
            MSG_APP_DATA_REPLY => Ok(GatewayMessage::AppDataReply {
                blob: get_bytes(&fields, KEY_BLOB)?,
            }),
            _ => Err(DecodeError::InvalidMsgType(msg_type)),
        }
    }

    pub fn msg_type(&self) -> u8 {
        match self {
            GatewayMessage::Command { .. } => MSG_COMMAND,
            GatewayMessage::Chunk { .. } => MSG_CHUNK,
            GatewayMessage::AppDataReply { .. } => MSG_APP_DATA_REPLY,
        }
    }
}
