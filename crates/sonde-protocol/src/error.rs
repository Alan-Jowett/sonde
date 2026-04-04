// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use alloc::string::String;
use core::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum EncodeError {
    FrameTooLarge,
    CborError(String),
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EncodeError::FrameTooLarge => write!(f, "frame exceeds maximum size"),
            EncodeError::CborError(msg) => write!(f, "CBOR encoding error: {}", msg),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum DecodeError {
    TooShort,
    TooLong,
    AuthenticationFailed,
    InvalidMsgType(u8),
    InvalidCommandType(u8),
    MissingField(u64),
    InvalidFieldType(u64),
    CborError(String),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::TooShort => write!(f, "frame too short"),
            DecodeError::TooLong => write!(f, "frame too long"),
            DecodeError::AuthenticationFailed => {
                write!(f, "AES-256-GCM authentication failed")
            }
            DecodeError::InvalidMsgType(t) => write!(f, "invalid msg_type: 0x{:02x}", t),
            DecodeError::InvalidCommandType(t) => {
                write!(f, "unsupported command_type: 0x{:02x}", t)
            }
            DecodeError::MissingField(k) => write!(f, "missing required CBOR key: {}", k),
            DecodeError::InvalidFieldType(k) => write!(f, "invalid type for CBOR key: {}", k),
            DecodeError::CborError(msg) => write!(f, "CBOR decoding error: {}", msg),
        }
    }
}
