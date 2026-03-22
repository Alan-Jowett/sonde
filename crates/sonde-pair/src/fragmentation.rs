// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Helper utilities for BLE ATT fragmentation (Write Long) and indication
//! reassembly as required by the pairing protocol (§3.4).
//!
//! These helpers do **not** automatically sit on the active BLE I/O path:
//! callers in the transport / pairing layers must explicitly:
//!
//! - Fragment outbound writes (phone → gateway/node) via
//!   [`fragment_for_write`], and
//! - Reassemble inbound indications (gateway/node → phone) via
//!   [`IndicationReassembler`] by feeding raw indication payloads until
//!   the accumulated length matches the `LEN` field from the envelope header.

use crate::error::PairingError;

/// Maximum envelope size the reassembler will accept (bytes).
///
/// Prevents unbounded buffering from a malicious/buggy peer that
/// advertises a large `LEN` field.  All pairing protocol messages are
/// well under 1 KiB; 4096 provides generous headroom.
const MAX_REASSEMBLY_SIZE: usize = 4096;

// ── Write Long fragmentation ────────────────────────────────────────────────

/// Fragment data into chunks for BLE Write Long.
///
/// Each chunk is at most `max_chunk_size` bytes.  Returns a single-element
/// vector if the data fits in one chunk.
///
/// # Errors
///
/// Returns an error if `max_chunk_size` is zero.
pub fn fragment_for_write(
    data: &[u8],
    max_chunk_size: usize,
) -> Result<Vec<Vec<u8>>, PairingError> {
    if max_chunk_size == 0 {
        return Err(PairingError::InvalidResponse {
            msg_type: 0,
            reason: "max_chunk_size must be > 0".into(),
        });
    }
    Ok(data.chunks(max_chunk_size).map(|c| c.to_vec()).collect())
}

// ── Indication reassembly ───────────────────────────────────────────────────

/// Stateful reassembler for multi-indication BLE messages.
///
/// Buffers indication payloads in order until the accumulated length
/// matches the `LEN` field from the envelope header
/// (`msg_type[1] + len[2] + payload[len]`).
pub struct IndicationReassembler {
    buffer: Vec<u8>,
    expected_total: Option<usize>,
}

impl IndicationReassembler {
    pub fn new() -> Self {
        Self {
            buffer: Vec::new(),
            expected_total: None,
        }
    }

    /// Feed an indication chunk into the reassembler.
    ///
    /// Returns `Ok(Some(complete_envelope))` when all chunks have been
    /// received.  Returns `Ok(None)` when more chunks are needed.
    /// Returns `Err` on malformed data (empty chunk, overflow, or
    /// advertised length exceeding [`MAX_REASSEMBLY_SIZE`]).
    ///
    /// On error the reassembler resets itself so subsequent calls start
    /// fresh — callers do not need to call [`reset`](Self::reset).
    pub fn push_chunk(&mut self, chunk: &[u8]) -> Result<Option<Vec<u8>>, PairingError> {
        if chunk.is_empty() {
            self.reset();
            return Err(PairingError::InvalidResponse {
                msg_type: 0,
                reason: "empty indication chunk".into(),
            });
        }

        // Guard against unbounded allocation before extending the buffer.
        // A malicious peer could send a large first chunk before the
        // header-derived size check runs.
        if self.buffer.len() + chunk.len() > MAX_REASSEMBLY_SIZE {
            let msg_type = if self.buffer.is_empty() {
                chunk[0]
            } else {
                self.buffer[0]
            };
            self.reset();
            return Err(PairingError::InvalidResponse {
                msg_type,
                reason: format!(
                    "chunk would exceed maximum reassembly size {MAX_REASSEMBLY_SIZE}"
                ),
            });
        }

        self.buffer.extend_from_slice(chunk);

        // Once we have the 3-byte header, derive the expected total length.
        if self.expected_total.is_none() && self.buffer.len() >= 3 {
            let payload_len = u16::from_be_bytes([self.buffer[1], self.buffer[2]]) as usize;
            let total = 3 + payload_len;
            if total > MAX_REASSEMBLY_SIZE {
                let msg_type = self.buffer[0];
                self.reset();
                return Err(PairingError::InvalidResponse {
                    msg_type,
                    reason: format!(
                        "indicated envelope size {total} exceeds maximum {MAX_REASSEMBLY_SIZE}"
                    ),
                });
            }
            self.expected_total = Some(total);
        }

        match self.expected_total {
            Some(expected) if self.buffer.len() > expected => {
                let msg_type = self.buffer[0];
                let actual = self.buffer.len();
                self.reset();
                Err(PairingError::InvalidResponse {
                    msg_type,
                    reason: format!(
                        "indication reassembly overflow: expected {expected} bytes, got {actual}",
                    ),
                })
            }
            Some(expected) if self.buffer.len() == expected => {
                let complete = std::mem::take(&mut self.buffer);
                self.expected_total = None;
                Ok(Some(complete))
            }
            _ => Ok(None),
        }
    }

    /// Reset the reassembler, discarding any partial data.
    pub fn reset(&mut self) {
        self.buffer.clear();
        self.expected_total = None;
    }

    /// Returns `true` if the reassembler has partial data buffered.
    pub fn has_partial_data(&self) -> bool {
        !self.buffer.is_empty()
    }
}

impl Default for IndicationReassembler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::build_envelope;

    // ── Write Long fragmentation tests ──────────────────────────────────

    #[test]
    fn fragment_single_chunk_fits() {
        let data = vec![0xAAu8; 100];
        let chunks = fragment_for_write(&data, 244).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], data);
    }

    #[test]
    fn fragment_multiple_chunks() {
        let data = vec![0xBBu8; 500];
        let chunks = fragment_for_write(&data, 244).unwrap();
        assert_eq!(chunks.len(), 3); // 244 + 244 + 12
        assert_eq!(chunks[0].len(), 244);
        assert_eq!(chunks[1].len(), 244);
        assert_eq!(chunks[2].len(), 12);
        // Verify all bytes round-trip
        let reassembled: Vec<u8> = chunks.into_iter().flatten().collect();
        assert_eq!(reassembled, data);
    }

    #[test]
    fn fragment_exact_boundary() {
        let data = vec![0xCCu8; 488]; // exactly 2 × 244
        let chunks = fragment_for_write(&data, 244).unwrap();
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 244);
        assert_eq!(chunks[1].len(), 244);
    }

    #[test]
    fn fragment_empty_data() {
        let chunks = fragment_for_write(&[], 244).unwrap();
        assert!(chunks.is_empty());
    }

    #[test]
    fn fragment_zero_chunk_size_returns_error() {
        let result = fragment_for_write(&[0x01], 0);
        assert!(result.is_err(), "zero chunk size must return error");
    }

    #[test]
    fn fragment_one_byte_chunks() {
        let data = vec![0x01, 0x02, 0x03];
        let chunks = fragment_for_write(&data, 1).unwrap();
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0], vec![0x01]);
        assert_eq!(chunks[1], vec![0x02]);
        assert_eq!(chunks[2], vec![0x03]);
    }

    // ── Indication reassembly tests ─────────────────────────────────────

    #[test]
    fn reassemble_single_chunk_complete() {
        let envelope = build_envelope(0x81, &[0xAA, 0xBB, 0xCC]).unwrap();
        let mut ra = IndicationReassembler::new();
        let result = ra.push_chunk(&envelope).unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap(), envelope);
        assert!(!ra.has_partial_data());
    }

    #[test]
    fn reassemble_two_chunks() {
        let envelope = build_envelope(0x81, &[0xAAu8; 20]).unwrap();
        // total = 23 bytes (3 header + 20 payload)
        let (first, second) = envelope.split_at(10);

        let mut ra = IndicationReassembler::new();
        assert_eq!(ra.push_chunk(first).unwrap(), None);
        assert!(ra.has_partial_data());

        let result = ra.push_chunk(second).unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap(), envelope);
    }

    #[test]
    fn reassemble_three_chunks() {
        let payload = vec![0xBBu8; 50];
        let envelope = build_envelope(0x01, &payload).unwrap();
        // total = 53 bytes

        let mut ra = IndicationReassembler::new();
        assert_eq!(ra.push_chunk(&envelope[..10]).unwrap(), None);
        assert_eq!(ra.push_chunk(&envelope[10..30]).unwrap(), None);
        let result = ra.push_chunk(&envelope[30..]).unwrap();
        assert_eq!(result.unwrap(), envelope);
    }

    #[test]
    fn reassemble_byte_at_a_time() {
        let envelope = build_envelope(0x02, &[0x01, 0x02]).unwrap();
        // total = 5 bytes
        let mut ra = IndicationReassembler::new();
        for (i, byte) in envelope.iter().enumerate() {
            let result = ra.push_chunk(std::slice::from_ref(byte)).unwrap();
            if i < envelope.len() - 1 {
                assert_eq!(result, None, "should not be complete at byte {i}");
            } else {
                assert!(result.is_some(), "should be complete at last byte");
                assert_eq!(result.unwrap(), envelope);
            }
        }
    }

    #[test]
    fn reassemble_overflow_rejected() {
        let envelope = build_envelope(0x81, &[0xAA]).unwrap();
        // total = 4 bytes, but we'll send 5
        let mut ra = IndicationReassembler::new();
        assert!(ra.push_chunk(&envelope).unwrap().is_some());

        // Try an overflow scenario — reassembler should auto-reset on error
        let mut data = envelope.clone();
        data.push(0xFF); // extra byte beyond expected
        let err = ra.push_chunk(&data).unwrap_err();
        assert!(
            format!("{err}").contains("overflow"),
            "expected overflow error, got: {err}"
        );
        // Reassembler should be usable again without explicit reset
        assert!(!ra.has_partial_data());
    }

    #[test]
    fn reassemble_empty_chunk_rejected() {
        let mut ra = IndicationReassembler::new();
        let err = ra.push_chunk(&[]).unwrap_err();
        assert!(
            format!("{err}").contains("empty indication chunk"),
            "expected empty chunk error, got: {err}"
        );
        // Should auto-reset after error
        assert!(!ra.has_partial_data());
    }

    #[test]
    fn reassemble_reset_clears_state() {
        let envelope = build_envelope(0x81, &[0xAA; 20]).unwrap();
        let mut ra = IndicationReassembler::new();

        // Push partial data
        ra.push_chunk(&envelope[..5]).unwrap();
        assert!(ra.has_partial_data());

        // Reset and verify clean state
        ra.reset();
        assert!(!ra.has_partial_data());

        // Should be able to reassemble a new message
        let new_envelope = build_envelope(0x02, &[0xBB]).unwrap();
        let result = ra.push_chunk(&new_envelope).unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap(), new_envelope);
    }

    #[test]
    fn reassemble_header_split_across_chunks() {
        // Header (3 bytes) split: first chunk has 1 byte, second has 2 bytes
        let envelope = build_envelope(0x81, &[0xDD; 10]).unwrap();
        let mut ra = IndicationReassembler::new();

        // Send only the msg_type byte
        assert_eq!(ra.push_chunk(&envelope[..1]).unwrap(), None);
        // Send the rest of the header + some payload
        assert_eq!(ra.push_chunk(&envelope[1..5]).unwrap(), None);
        // Send remaining payload
        let result = ra.push_chunk(&envelope[5..]).unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap(), envelope);
    }

    #[test]
    fn reassemble_zero_length_payload() {
        // Envelope with empty payload: [msg_type, 0x00, 0x00] = 3 bytes total
        let envelope = build_envelope(0x01, &[]).unwrap();
        assert_eq!(envelope.len(), 3);

        let mut ra = IndicationReassembler::new();
        let result = ra.push_chunk(&envelope).unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap(), envelope);
    }

    #[test]
    fn reassemble_mtu_boundary_244() {
        // Typical MTU 247 → max chunk = 244.  Test payload that requires
        // exactly 2 chunks at this boundary.
        let payload = vec![0xEEu8; 485]; // 3 + 485 = 488 total → 2 × 244
        let envelope = build_envelope(0x81, &payload).unwrap();
        assert_eq!(envelope.len(), 488);

        let chunks = fragment_for_write(&envelope, 244).unwrap();
        assert_eq!(chunks.len(), 2);

        let mut ra = IndicationReassembler::new();
        assert_eq!(ra.push_chunk(&chunks[0]).unwrap(), None);
        let result = ra.push_chunk(&chunks[1]).unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap(), envelope);
    }

    #[test]
    fn reassemble_consecutive_messages() {
        // After completing one message, the reassembler should handle a second.
        let env1 = build_envelope(0x01, &[0xAA]).unwrap();
        let env2 = build_envelope(0x02, &[0xBB, 0xCC]).unwrap();

        let mut ra = IndicationReassembler::new();

        let r1 = ra.push_chunk(&env1).unwrap();
        assert!(r1.is_some());
        assert_eq!(r1.unwrap(), env1);

        let r2 = ra.push_chunk(&env2).unwrap();
        assert!(r2.is_some());
        assert_eq!(r2.unwrap(), env2);
    }

    #[test]
    fn reassemble_rejects_oversized_envelope() {
        // Craft a header claiming a payload larger than MAX_REASSEMBLY_SIZE.
        // LEN = 0xFFFF → total = 3 + 65535 = 65538 > 4096
        let header = [0x01, 0xFF, 0xFF];
        let mut ra = IndicationReassembler::new();
        let err = ra.push_chunk(&header).unwrap_err();
        assert!(
            format!("{err}").contains("exceeds maximum"),
            "expected max-size error, got: {err}"
        );
        // Auto-reset after rejection
        assert!(!ra.has_partial_data());
    }

    #[test]
    fn reassemble_auto_reset_on_error_allows_reuse() {
        // After an error (empty chunk), the reassembler should accept a
        // fresh message without an explicit reset() call.
        let mut ra = IndicationReassembler::new();

        // Trigger an error
        let _ = ra.push_chunk(&[]);
        assert!(!ra.has_partial_data());

        // Should work cleanly for a new message
        let envelope = build_envelope(0x81, &[0xDD]).unwrap();
        let result = ra.push_chunk(&envelope).unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap(), envelope);
    }
}
