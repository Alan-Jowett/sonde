// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use alloc::vec::Vec;

use crate::constants::{HEADER_SIZE, HMAC_SIZE, MAX_FRAME_SIZE, MIN_FRAME_SIZE};
use crate::error::{DecodeError, EncodeError};
use crate::header::FrameHeader;
use crate::traits::HmacProvider;

#[derive(Debug, Clone)]
pub struct DecodedFrame {
    pub header: FrameHeader,
    pub payload: Vec<u8>,
    pub hmac: [u8; 32],
}

pub fn encode_frame(
    header: &FrameHeader,
    payload_cbor: &[u8],
    psk: &[u8],
    hmac: &impl HmacProvider,
) -> Result<Vec<u8>, EncodeError> {
    let total_size = HEADER_SIZE + payload_cbor.len() + HMAC_SIZE;
    if total_size > MAX_FRAME_SIZE {
        return Err(EncodeError::FrameTooLarge);
    }

    let header_bytes = header.to_bytes();

    // Compute HMAC over header + payload without an intermediate Vec.
    // Concat into a stack buffer since total is bounded by MAX_FRAME_SIZE.
    let mut auth_buf = [0u8; MAX_FRAME_SIZE];
    auth_buf[..HEADER_SIZE].copy_from_slice(&header_bytes);
    auth_buf[HEADER_SIZE..HEADER_SIZE + payload_cbor.len()].copy_from_slice(payload_cbor);
    let auth_len = HEADER_SIZE + payload_cbor.len();
    let mac = hmac.compute(psk, &auth_buf[..auth_len]);

    let mut frame = Vec::with_capacity(total_size);
    frame.extend_from_slice(&header_bytes);
    frame.extend_from_slice(payload_cbor);
    frame.extend_from_slice(&mac);

    Ok(frame)
}

pub fn decode_frame(raw: &[u8]) -> Result<DecodedFrame, DecodeError> {
    if raw.len() < MIN_FRAME_SIZE {
        return Err(DecodeError::TooShort);
    }
    if raw.len() > MAX_FRAME_SIZE {
        return Err(DecodeError::TooLong);
    }

    let header_bytes: [u8; HEADER_SIZE] = raw[..HEADER_SIZE]
        .try_into()
        .map_err(|_| DecodeError::TooShort)?;
    let header = FrameHeader::from_bytes(&header_bytes);

    let payload_end = raw.len() - HMAC_SIZE;
    let payload = raw[HEADER_SIZE..payload_end].to_vec();

    let mut hmac = [0u8; HMAC_SIZE];
    hmac.copy_from_slice(&raw[payload_end..]);

    Ok(DecodedFrame {
        header,
        payload,
        hmac,
    })
}

pub fn verify_frame(frame: &DecodedFrame, psk: &[u8], hmac: &impl HmacProvider) -> bool {
    let mut auth_buf = [0u8; MAX_FRAME_SIZE];
    let header_bytes = frame.header.to_bytes();
    auth_buf[..HEADER_SIZE].copy_from_slice(&header_bytes);
    auth_buf[HEADER_SIZE..HEADER_SIZE + frame.payload.len()].copy_from_slice(&frame.payload);
    let auth_len = HEADER_SIZE + frame.payload.len();
    hmac.verify(psk, &auth_buf[..auth_len], &frame.hmac)
}
