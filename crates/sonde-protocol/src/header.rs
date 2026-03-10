// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use crate::constants::{HEADER_SIZE, OFFSET_KEY_HINT, OFFSET_MSG_TYPE, OFFSET_NONCE};

#[derive(Debug, Clone, PartialEq)]
pub struct FrameHeader {
    pub key_hint: u16,
    pub msg_type: u8,
    pub nonce: u64,
}

impl FrameHeader {
    pub fn to_bytes(&self) -> [u8; HEADER_SIZE] {
        let mut buf = [0u8; HEADER_SIZE];
        buf[OFFSET_KEY_HINT..OFFSET_KEY_HINT + 2].copy_from_slice(&self.key_hint.to_be_bytes());
        buf[OFFSET_MSG_TYPE] = self.msg_type;
        buf[OFFSET_NONCE..OFFSET_NONCE + 8].copy_from_slice(&self.nonce.to_be_bytes());
        buf
    }

    pub fn from_bytes(bytes: &[u8; HEADER_SIZE]) -> Self {
        let key_hint = u16::from_be_bytes([bytes[OFFSET_KEY_HINT], bytes[OFFSET_KEY_HINT + 1]]);
        let msg_type = bytes[OFFSET_MSG_TYPE];
        let nonce = u64::from_be_bytes([
            bytes[OFFSET_NONCE],
            bytes[OFFSET_NONCE + 1],
            bytes[OFFSET_NONCE + 2],
            bytes[OFFSET_NONCE + 3],
            bytes[OFFSET_NONCE + 4],
            bytes[OFFSET_NONCE + 5],
            bytes[OFFSET_NONCE + 6],
            bytes[OFFSET_NONCE + 7],
        ]);
        Self {
            key_hint,
            msg_type,
            nonce,
        }
    }
}
