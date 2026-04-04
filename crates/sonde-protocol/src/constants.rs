// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

// Frame structure
pub const HEADER_SIZE: usize = 11;
pub const MAX_FRAME_SIZE: usize = 250; // ESP-NOW reference

// AES-256-GCM constants
pub const AEAD_TAG_SIZE: usize = 16;
pub const GCM_NONCE_SIZE: usize = 12;
pub const MIN_FRAME_SIZE_AEAD: usize = HEADER_SIZE + AEAD_TAG_SIZE; // 27
pub const MAX_PAYLOAD_SIZE_AEAD: usize = MAX_FRAME_SIZE - HEADER_SIZE - AEAD_TAG_SIZE; // 223

// Header offsets
pub const OFFSET_KEY_HINT: usize = 0;
pub const OFFSET_MSG_TYPE: usize = 2;
pub const OFFSET_NONCE: usize = 3;

// msg_type codes (node -> gateway)
pub const MSG_WAKE: u8 = 0x01;
pub const MSG_GET_CHUNK: u8 = 0x02;
pub const MSG_PROGRAM_ACK: u8 = 0x03;
pub const MSG_APP_DATA: u8 = 0x04;
pub const MSG_PEER_REQUEST: u8 = 0x05;

// msg_type codes (gateway -> node)
pub const MSG_COMMAND: u8 = 0x81;
pub const MSG_CHUNK: u8 = 0x82;
pub const MSG_APP_DATA_REPLY: u8 = 0x83;
pub const MSG_PEER_ACK: u8 = 0x84;

// Command codes
pub const CMD_NOP: u8 = 0x00;
pub const CMD_UPDATE_PROGRAM: u8 = 0x01;
pub const CMD_RUN_EPHEMERAL: u8 = 0x02;
pub const CMD_UPDATE_SCHEDULE: u8 = 0x03;
pub const CMD_REBOOT: u8 = 0x04;

// CBOR integer keys (protocol messages)
pub const KEY_FIRMWARE_ABI_VERSION: u64 = 1;
pub const KEY_PROGRAM_HASH: u64 = 2;
pub const KEY_BATTERY_MV: u64 = 3;
pub const KEY_COMMAND_TYPE: u64 = 4;
pub const KEY_PAYLOAD: u64 = 5;
pub const KEY_PROGRAM_SIZE: u64 = 6;
pub const KEY_CHUNK_SIZE: u64 = 7;
pub const KEY_CHUNK_COUNT: u64 = 8;
pub const KEY_INTERVAL_S: u64 = 9;
pub const KEY_BLOB: u64 = 10;
pub const KEY_CHUNK_INDEX: u64 = 11;
pub const KEY_CHUNK_DATA: u64 = 12;
pub const KEY_STARTING_SEQ: u64 = 13;
pub const KEY_TIMESTAMP_MS: u64 = 14;

// CBOR integer keys (PEER_REQUEST / PEER_ACK messages)
// Keys are scoped per msg_type — key 1 in PEER_REQUEST (encrypted_payload)
// is unrelated to key 1 in WAKE (firmware_abi_version).
// PEER_REQUEST: { 1: encrypted_payload }
// PEER_ACK:     { 1: status, 2: registration_proof }
pub const PEER_REQ_KEY_PAYLOAD: u64 = 1;
pub const PEER_ACK_KEY_STATUS: u64 = 1;
pub const PEER_ACK_KEY_PROOF: u64 = 2;

// CBOR integer keys (program image -- separate keyspace)
pub const IMG_KEY_BYTECODE: u64 = 1;
pub const IMG_KEY_MAPS: u64 = 2;
pub const MAP_KEY_TYPE: u64 = 1;
pub const MAP_KEY_KEY_SIZE: u64 = 2;
pub const MAP_KEY_VALUE_SIZE: u64 = 3;
pub const MAP_KEY_MAX_ENTRIES: u64 = 4;
pub const MAP_KEY_INITIAL_DATA: u64 = 5;
