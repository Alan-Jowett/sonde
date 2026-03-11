// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use core::fmt;

/// Errors produced by the node firmware.
#[derive(Debug, Clone, PartialEq)]
pub enum NodeError {
    /// HMAC verification failed on an inbound frame.
    AuthFailure,
    /// Echoed nonce/seq in response does not match the value we sent.
    ResponseBindingMismatch,
    /// Frame has an unexpected or unknown `msg_type`.
    UnexpectedMsgType(u8),
    /// CBOR payload could not be decoded.
    MalformedPayload(String),
    /// Transport-level send/receive failure.
    Transport(String),
    /// No PSK provisioned — node is unpaired.
    Unpaired,
    /// Program hash mismatch after chunked transfer.
    ProgramHashMismatch,
    /// Program image CBOR decoding failed.
    ProgramDecodeFailed(String),
    /// Map definitions exceed the sleep-persistent memory budget.
    MapBudgetExceeded { required: usize, available: usize },
    /// Map key/index is out of bounds.
    MapKeyOutOfBounds { key: u32, max_entries: u32 },
    /// BPF execution error.
    BpfError(String),
    /// A BPF helper returned an error.
    HelperError { helper_id: u32, code: i64 },
    /// Operation not permitted for ephemeral programs.
    EphemeralRestriction { helper_id: u32 },
    /// Chunk transfer failed after exhausting retries.
    ChunkTransferFailed { chunk_index: u32 },
    /// WAKE retries exhausted — no gateway response.
    WakeRetriesExhausted,
    /// Response timeout.
    Timeout,
    /// Flash storage operation failed.
    StorageError(String),
    /// Chunk index mismatch in CHUNK response.
    ChunkIndexMismatch { expected: u32, received: u32 },
    /// Reboot requested by gateway.
    RebootRequested,
}

impl fmt::Display for NodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NodeError::AuthFailure => write!(f, "HMAC verification failed"),
            NodeError::ResponseBindingMismatch => write!(f, "response binding mismatch"),
            NodeError::UnexpectedMsgType(t) => write!(f, "unexpected msg_type: 0x{:02x}", t),
            NodeError::MalformedPayload(msg) => write!(f, "malformed payload: {}", msg),
            NodeError::Transport(msg) => write!(f, "transport error: {}", msg),
            NodeError::Unpaired => write!(f, "node is unpaired (no PSK)"),
            NodeError::ProgramHashMismatch => write!(f, "program hash mismatch"),
            NodeError::ProgramDecodeFailed(msg) => write!(f, "program decode failed: {}", msg),
            NodeError::MapBudgetExceeded {
                required,
                available,
            } => write!(
                f,
                "map budget exceeded: need {} bytes, have {} bytes",
                required, available
            ),
            NodeError::MapKeyOutOfBounds { key, max_entries } => {
                write!(f, "map key {} out of bounds (max {})", key, max_entries)
            }
            NodeError::BpfError(msg) => write!(f, "BPF error: {}", msg),
            NodeError::HelperError { helper_id, code } => {
                write!(f, "helper {} returned error {}", helper_id, code)
            }
            NodeError::EphemeralRestriction { helper_id } => {
                write!(f, "helper {} not permitted for ephemeral programs", helper_id)
            }
            NodeError::ChunkTransferFailed { chunk_index } => {
                write!(f, "chunk transfer failed at index {}", chunk_index)
            }
            NodeError::WakeRetriesExhausted => write!(f, "WAKE retries exhausted"),
            NodeError::Timeout => write!(f, "response timeout"),
            NodeError::StorageError(msg) => write!(f, "storage error: {}", msg),
            NodeError::ChunkIndexMismatch { expected, received } => {
                write!(
                    f,
                    "chunk index mismatch: expected {}, received {}",
                    expected, received
                )
            }
            NodeError::RebootRequested => write!(f, "reboot requested"),
        }
    }
}

impl std::error::Error for NodeError {}

/// Convenience alias used throughout the node crate.
pub type NodeResult<T> = Result<T, NodeError>;
