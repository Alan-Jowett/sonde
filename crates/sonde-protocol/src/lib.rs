// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

#![no_std]

extern crate alloc;

pub mod aead_codec;
pub mod ble_envelope;
pub mod board_layout;
pub mod chunk;
pub mod constants;
pub mod error;
pub mod header;
pub mod messages;
pub mod modem;
pub mod program_image;
pub mod traits;

pub use aead_codec::{build_gcm_nonce, decode_frame, encode_frame, open_frame, DecodedFrame};
pub use ble_envelope::{
    decode_read_test_result, decode_run_test_ack, decode_run_test_command, decode_test_result,
    encode_ble_envelope, encode_read_test_result, encode_run_test_ack, encode_run_test_command,
    encode_test_result, parse_ble_envelope, RunTestCommand, TestResult,
};
pub use board_layout::{
    decode_board_layout_cbor, encode_board_layout_cbor, BoardLayout, BOARD_LAYOUT_KEY_BATTERY_ADC,
    BOARD_LAYOUT_KEY_I2C0_SCL, BOARD_LAYOUT_KEY_I2C0_SDA, BOARD_LAYOUT_KEY_ONE_WIRE_DATA,
    BOARD_LAYOUT_KEY_SENSOR_ENABLE,
};
pub use chunk::{chunk_count, get_chunk};
pub use constants::*;
pub use error::{DecodeError, EncodeError};
pub use header::FrameHeader;
pub use messages::{CommandPayload, GatewayMessage, NodeMessage};
pub use program_image::{program_hash, MapDef, ProgramImage};
pub use traits::{AeadProvider, Sha256Provider};

/// Derive the 2-byte key hint from a PSK.
///
/// `key_hint = u16::from_be_bytes(SHA-256(PSK)[30..32])`
///
/// This consolidates the derivation formula so the gateway and node
/// do not implement it independently.
pub fn key_hint_from_psk(psk: &[u8; 32], sha: &impl Sha256Provider) -> u16 {
    let hash = sha.hash(psk);
    u16::from_be_bytes([hash[30], hash[31]])
}
