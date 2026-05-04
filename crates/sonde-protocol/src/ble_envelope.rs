// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! BLE message envelope codec (ble-pairing-protocol.md §4).
//!
//! ```text
//! ┌──────────┬──────────┬────────────────────────────┐
//! │ TYPE (1) │ LEN (2B) │ BODY (LEN bytes)            │
//! └──────────┴──────────┴────────────────────────────┘
//! ```
//!
//! Used by both the node (BLE pairing provisioning) and the gateway
//! (phone registration over BLE relay).

use alloc::format;
use alloc::vec;
use alloc::vec::Vec;

use ciborium::Value;

use crate::constants::{
    MAX_FRAME_SIZE, TEST_CMD_KEY_PAYLOAD, TEST_CMD_KEY_RF_CHANNEL, TEST_CMD_KEY_TEST_TYPE,
    TEST_RESULT_KEY_ATTEMPT_COUNT, TEST_RESULT_KEY_ELAPSED_MS, TEST_RESULT_KEY_REPLY_FRAME,
    TEST_RESULT_KEY_REPLY_RSSI_DBM, TEST_RESULT_KEY_STATUS, TEST_RESULT_KEY_TEST_TYPE,
    TEST_RESULT_NO_RESULT, TEST_RESULT_OK, TEST_TYPE_DIAG_FRAME,
};
use crate::error::{DecodeError, EncodeError};

/// Parsed `RUN_TEST_COMMAND` body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunTestCommand {
    pub test_type: u64,
    pub rf_channel: Option<u8>,
    pub payload: Vec<u8>,
}

/// Parsed `TEST_RESULT` body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestResult {
    pub status: u8,
    pub test_type: Option<u64>,
    pub reply_frame: Option<Vec<u8>>,
    pub reply_rssi_dbm: Option<i8>,
    pub attempt_count: u64,
    pub elapsed_ms: u64,
}

/// Parse a BLE message envelope.
pub fn parse_ble_envelope(data: &[u8]) -> Option<(u8, &[u8])> {
    if data.len() < 3 {
        return None;
    }
    let msg_type = data[0];
    let body_len = u16::from_be_bytes([data[1], data[2]]) as usize;
    if data.len() != 3 + body_len {
        return None;
    }
    Some((msg_type, &data[3..3 + body_len]))
}

/// Encode a BLE message envelope.
pub fn encode_ble_envelope(msg_type: u8, body: &[u8]) -> Option<Vec<u8>> {
    if body.len() > u16::MAX as usize {
        return None;
    }
    let len = body.len() as u16;
    let mut out = Vec::with_capacity(3 + body.len());
    out.push(msg_type);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(body);
    Some(out)
}

fn cbor_encode_map(pairs: &[(u64, Value)]) -> Result<Vec<u8>, EncodeError> {
    let value = Value::Map(
        pairs
            .iter()
            .map(|(key, value)| (Value::Integer((*key).into()), value.clone()))
            .collect(),
    );
    let mut buf = Vec::new();
    ciborium::into_writer(&value, &mut buf).map_err(|e| EncodeError::CborError(format!("{e}")))?;
    Ok(buf)
}

fn cbor_decode_map(data: &[u8]) -> Result<Vec<(u64, Value)>, DecodeError> {
    let mut remaining = data;
    let value: Value = ciborium::from_reader(&mut remaining)
        .map_err(|e| DecodeError::CborError(format!("{e}")))?;
    if !remaining.is_empty() {
        return Err(DecodeError::TooLong);
    }
    let map = value
        .as_map()
        .ok_or_else(|| DecodeError::CborError("expected CBOR map".into()))?;
    let mut decoded = Vec::with_capacity(map.len());
    for (key, value) in map {
        let Some(key) = key
            .as_integer()
            .and_then(|integer| u64::try_from(integer).ok())
        else {
            return Err(DecodeError::InvalidParameter(
                "BLE test message key must be unsigned integer".into(),
            ));
        };
        decoded.push((key, value.clone()));
    }
    Ok(decoded)
}

fn get_field<'a>(fields: &'a [(u64, Value)], key: u64) -> Result<&'a Value, DecodeError> {
    fields
        .iter()
        .find(|(candidate, _)| *candidate == key)
        .map(|(_, value)| value)
        .ok_or(DecodeError::MissingField(key))
}

fn get_uint(fields: &[(u64, Value)], key: u64) -> Result<u64, DecodeError> {
    get_field(fields, key)?
        .as_integer()
        .and_then(|value| u64::try_from(value).ok())
        .ok_or(DecodeError::InvalidFieldType(key))
}

fn get_u8(fields: &[(u64, Value)], key: u64) -> Result<u8, DecodeError> {
    u8::try_from(get_uint(fields, key)?).map_err(|_| DecodeError::InvalidFieldType(key))
}

fn get_bytes(fields: &[(u64, Value)], key: u64) -> Result<Vec<u8>, DecodeError> {
    get_field(fields, key)?
        .as_bytes()
        .map(|bytes| bytes.to_vec())
        .ok_or(DecodeError::InvalidFieldType(key))
}

fn get_optional_uint(fields: &[(u64, Value)], key: u64) -> Result<Option<u64>, DecodeError> {
    match fields.iter().find(|(candidate, _)| *candidate == key) {
        None => Ok(None),
        Some((_, value)) => value
            .as_integer()
            .and_then(|integer| u64::try_from(integer).ok())
            .map(Some)
            .ok_or(DecodeError::InvalidFieldType(key)),
    }
}

fn get_optional_i8(fields: &[(u64, Value)], key: u64) -> Result<Option<i8>, DecodeError> {
    match fields.iter().find(|(candidate, _)| *candidate == key) {
        None => Ok(None),
        Some((_, value)) => {
            let integer = value
                .as_integer()
                .ok_or(DecodeError::InvalidFieldType(key))?;
            let value = i64::try_from(integer).map_err(|_| DecodeError::InvalidFieldType(key))?;
            i8::try_from(value)
                .map(Some)
                .map_err(|_| DecodeError::InvalidFieldType(key))
        }
    }
}

fn get_optional_bytes(fields: &[(u64, Value)], key: u64) -> Result<Option<Vec<u8>>, DecodeError> {
    match fields.iter().find(|(candidate, _)| *candidate == key) {
        None => Ok(None),
        Some((_, value)) => value
            .as_bytes()
            .map(|bytes| Some(bytes.to_vec()))
            .ok_or(DecodeError::InvalidFieldType(key)),
    }
}

/// Encode a `RUN_TEST_COMMAND` BLE body.
pub fn encode_run_test_command(
    test_type: u64,
    rf_channel: Option<u8>,
    payload: &[u8],
) -> Result<Vec<u8>, EncodeError> {
    if test_type == TEST_TYPE_DIAG_FRAME {
        let rf_channel = rf_channel.ok_or_else(|| {
            EncodeError::InvalidParameter("DIAG_FRAME requires rf_channel".into())
        })?;
        if !(1..=13).contains(&rf_channel) {
            return Err(EncodeError::InvalidParameter(format!(
                "DIAG_FRAME rf_channel {} out of range 1-13",
                rf_channel
            )));
        }
        if payload.is_empty() {
            return Err(EncodeError::InvalidParameter(
                "DIAG_FRAME payload must not be empty".into(),
            ));
        }
        if payload.len() > MAX_FRAME_SIZE {
            return Err(EncodeError::FrameTooLarge);
        }
    }

    let mut pairs = vec![
        (TEST_CMD_KEY_TEST_TYPE, Value::Integer(test_type.into())),
        (TEST_CMD_KEY_PAYLOAD, Value::Bytes(payload.to_vec())),
    ];
    if let Some(rf_channel) = rf_channel {
        pairs.insert(
            1,
            (TEST_CMD_KEY_RF_CHANNEL, Value::Integer(rf_channel.into())),
        );
    }
    cbor_encode_map(&pairs)
}

/// Decode a `RUN_TEST_COMMAND` BLE body.
pub fn decode_run_test_command(body: &[u8]) -> Result<RunTestCommand, DecodeError> {
    let fields = cbor_decode_map(body)?;
    let test_type = get_uint(&fields, TEST_CMD_KEY_TEST_TYPE)?;
    let payload = get_bytes(&fields, TEST_CMD_KEY_PAYLOAD)?;
    let rf_channel = match get_optional_uint(&fields, TEST_CMD_KEY_RF_CHANNEL)? {
        None => None,
        Some(channel) => Some(
            u8::try_from(channel)
                .map_err(|_| DecodeError::InvalidFieldType(TEST_CMD_KEY_RF_CHANNEL))?,
        ),
    };

    if test_type == TEST_TYPE_DIAG_FRAME {
        let rf_channel = rf_channel.ok_or(DecodeError::MissingField(TEST_CMD_KEY_RF_CHANNEL))?;
        if !(1..=13).contains(&rf_channel) {
            return Err(DecodeError::InvalidParameter(format!(
                "DIAG_FRAME rf_channel {} out of range 1-13",
                rf_channel
            )));
        }
        if payload.is_empty() {
            return Err(DecodeError::InvalidParameter(
                "DIAG_FRAME payload must not be empty".into(),
            ));
        }
        if payload.len() > MAX_FRAME_SIZE {
            return Err(DecodeError::InvalidParameter(format!(
                "DIAG_FRAME payload too large: {} > {}",
                payload.len(),
                MAX_FRAME_SIZE
            )));
        }
    }

    Ok(RunTestCommand {
        test_type,
        rf_channel,
        payload,
    })
}

/// Encode a `RUN_TEST_ACK` BLE body.
pub fn encode_run_test_ack(status: u8) -> Result<Vec<u8>, EncodeError> {
    Ok(vec![status])
}

/// Decode a `RUN_TEST_ACK` BLE body.
pub fn decode_run_test_ack(body: &[u8]) -> Result<u8, DecodeError> {
    if body.len() != 1 {
        return if body.is_empty() {
            Err(DecodeError::TooShort)
        } else {
            Err(DecodeError::TooLong)
        };
    }
    Ok(body[0])
}

/// Encode an empty `READ_TEST_RESULT` BLE body.
pub fn encode_read_test_result() -> Vec<u8> {
    Vec::new()
}

/// Decode a `READ_TEST_RESULT` BLE body.
pub fn decode_read_test_result(body: &[u8]) -> Result<(), DecodeError> {
    if body.is_empty() {
        Ok(())
    } else {
        Err(DecodeError::TooLong)
    }
}

/// Encode a `TEST_RESULT` BLE body.
pub fn encode_test_result(result: &TestResult) -> Result<Vec<u8>, EncodeError> {
    if result.status == TEST_RESULT_OK {
        if result.reply_frame.is_none() || result.reply_rssi_dbm.is_none() {
            return Err(EncodeError::InvalidParameter(
                "successful TEST_RESULT requires reply_frame and reply_rssi_dbm".into(),
            ));
        }
    } else if result.reply_frame.is_some() || result.reply_rssi_dbm.is_some() {
        return Err(EncodeError::InvalidParameter(
            "non-success TEST_RESULT must not include reply fields".into(),
        ));
    }

    if result.status != TEST_RESULT_NO_RESULT && result.test_type.is_none() {
        return Err(EncodeError::InvalidParameter(
            "TEST_RESULT missing test_type".into(),
        ));
    }

    if let Some(reply_frame) = &result.reply_frame {
        if reply_frame.is_empty() {
            return Err(EncodeError::InvalidParameter(
                "reply_frame must not be empty when present".into(),
            ));
        }
        if reply_frame.len() > MAX_FRAME_SIZE {
            return Err(EncodeError::FrameTooLarge);
        }
    }

    let mut pairs = vec![(TEST_RESULT_KEY_STATUS, Value::Integer(result.status.into()))];
    if let Some(test_type) = result.test_type {
        pairs.push((TEST_RESULT_KEY_TEST_TYPE, Value::Integer(test_type.into())));
    }
    if let Some(reply_frame) = &result.reply_frame {
        pairs.push((
            TEST_RESULT_KEY_REPLY_FRAME,
            Value::Bytes(reply_frame.clone()),
        ));
    }
    if let Some(reply_rssi_dbm) = result.reply_rssi_dbm {
        pairs.push((
            TEST_RESULT_KEY_REPLY_RSSI_DBM,
            Value::Integer(reply_rssi_dbm.into()),
        ));
    }
    pairs.push((
        TEST_RESULT_KEY_ATTEMPT_COUNT,
        Value::Integer(result.attempt_count.into()),
    ));
    pairs.push((
        TEST_RESULT_KEY_ELAPSED_MS,
        Value::Integer(result.elapsed_ms.into()),
    ));

    cbor_encode_map(&pairs)
}

/// Decode a `TEST_RESULT` BLE body.
pub fn decode_test_result(body: &[u8]) -> Result<TestResult, DecodeError> {
    let fields = cbor_decode_map(body)?;
    let status = get_u8(&fields, TEST_RESULT_KEY_STATUS)?;
    let test_type = get_optional_uint(&fields, TEST_RESULT_KEY_TEST_TYPE)?;
    let reply_frame = get_optional_bytes(&fields, TEST_RESULT_KEY_REPLY_FRAME)?;
    let reply_rssi_dbm = get_optional_i8(&fields, TEST_RESULT_KEY_REPLY_RSSI_DBM)?;
    let attempt_count = get_uint(&fields, TEST_RESULT_KEY_ATTEMPT_COUNT)?;
    let elapsed_ms = get_uint(&fields, TEST_RESULT_KEY_ELAPSED_MS)?;

    if status == TEST_RESULT_OK {
        if reply_frame.is_none() || reply_rssi_dbm.is_none() {
            return Err(DecodeError::InvalidParameter(
                "successful TEST_RESULT missing reply fields".into(),
            ));
        }
    } else if reply_frame.is_some() || reply_rssi_dbm.is_some() {
        return Err(DecodeError::InvalidParameter(
            "non-success TEST_RESULT must not include reply fields".into(),
        ));
    }

    if status != TEST_RESULT_NO_RESULT && test_type.is_none() {
        return Err(DecodeError::MissingField(TEST_RESULT_KEY_TEST_TYPE));
    }

    if let Some(reply_frame) = &reply_frame {
        if reply_frame.is_empty() {
            return Err(DecodeError::InvalidParameter(
                "reply_frame must not be empty when present".into(),
            ));
        }
        if reply_frame.len() > MAX_FRAME_SIZE {
            return Err(DecodeError::InvalidParameter(format!(
                "reply_frame too large: {} > {}",
                reply_frame.len(),
                MAX_FRAME_SIZE
            )));
        }
    }

    Ok(TestResult {
        status,
        test_type,
        reply_frame,
        reply_rssi_dbm,
        attempt_count,
        elapsed_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::{
        BLE_READ_TEST_RESULT, BLE_RUN_TEST_ACK, BLE_RUN_TEST_COMMAND, BLE_TEST_RESULT,
        RUN_TEST_ACK_INVALID, RUN_TEST_ACK_OK, RUN_TEST_ACK_UNSUPPORTED,
        TEST_RESULT_EXECUTION_ERROR, TEST_RESULT_TIMEOUT,
    };

    #[test]
    fn round_trip() {
        let body = [0x42u8; 10];
        let encoded = encode_ble_envelope(0x01, &body).unwrap();
        let (msg_type, decoded) = parse_ble_envelope(&encoded).unwrap();
        assert_eq!(msg_type, 0x01);
        assert_eq!(decoded, &body);
    }

    #[test]
    fn empty_body() {
        let encoded = encode_ble_envelope(0x81, &[]).unwrap();
        let (msg_type, body) = parse_ble_envelope(&encoded).unwrap();
        assert_eq!(msg_type, 0x81);
        assert!(body.is_empty());
    }

    #[test]
    fn too_short() {
        assert!(parse_ble_envelope(&[0x01, 0x00]).is_none());
    }

    #[test]
    fn truncated() {
        assert!(parse_ble_envelope(&[0x01, 0x00, 0x04, 0xAA, 0xBB]).is_none());
    }

    #[test]
    fn trailing_bytes() {
        assert!(parse_ble_envelope(&[0x01, 0x00, 0x02, 0xAA, 0xBB, 0xCC]).is_none());
    }

    #[test]
    fn run_test_command_round_trip() {
        let payload = [0x42u8; 50];
        let body = encode_run_test_command(TEST_TYPE_DIAG_FRAME, Some(6), &payload).unwrap();
        let envelope = encode_ble_envelope(BLE_RUN_TEST_COMMAND, &body).unwrap();
        let (msg_type, decoded_body) = parse_ble_envelope(&envelope).unwrap();
        assert_eq!(msg_type, BLE_RUN_TEST_COMMAND);
        let decoded = decode_run_test_command(decoded_body).unwrap();
        assert_eq!(decoded.test_type, TEST_TYPE_DIAG_FRAME);
        assert_eq!(decoded.rf_channel, Some(6));
        assert_eq!(decoded.payload, &payload);
    }

    #[test]
    fn run_test_command_invalid_channel_rejected() {
        let payload = [0x42u8; 50];
        assert!(encode_run_test_command(TEST_TYPE_DIAG_FRAME, Some(14), &payload).is_err());
    }

    #[test]
    fn run_test_ack_round_trip() {
        let body = encode_run_test_ack(RUN_TEST_ACK_OK).unwrap();
        let envelope = encode_ble_envelope(BLE_RUN_TEST_ACK, &body).unwrap();
        let (msg_type, decoded_body) = parse_ble_envelope(&envelope).unwrap();
        assert_eq!(msg_type, BLE_RUN_TEST_ACK);
        assert_eq!(decode_run_test_ack(decoded_body).unwrap(), RUN_TEST_ACK_OK);
    }

    #[test]
    fn read_test_result_round_trip() {
        let body = encode_read_test_result();
        let envelope = encode_ble_envelope(BLE_READ_TEST_RESULT, &body).unwrap();
        let (msg_type, decoded_body) = parse_ble_envelope(&envelope).unwrap();
        assert_eq!(msg_type, BLE_READ_TEST_RESULT);
        decode_read_test_result(decoded_body).unwrap();
    }

    #[test]
    fn test_result_round_trip_success_and_timeout() {
        let success = TestResult {
            status: TEST_RESULT_OK,
            test_type: Some(TEST_TYPE_DIAG_FRAME),
            reply_frame: Some(vec![0xAA; 30]),
            reply_rssi_dbm: Some(-64),
            attempt_count: 2,
            elapsed_ms: 4_100,
        };
        let body = encode_test_result(&success).unwrap();
        let envelope = encode_ble_envelope(BLE_TEST_RESULT, &body).unwrap();
        let (msg_type, decoded_body) = parse_ble_envelope(&envelope).unwrap();
        assert_eq!(msg_type, BLE_TEST_RESULT);
        assert_eq!(decode_test_result(decoded_body).unwrap(), success);

        let timeout = TestResult {
            status: TEST_RESULT_TIMEOUT,
            test_type: Some(TEST_TYPE_DIAG_FRAME),
            reply_frame: None,
            reply_rssi_dbm: None,
            attempt_count: 4,
            elapsed_ms: 8_600,
        };
        assert_eq!(
            decode_test_result(&encode_test_result(&timeout).unwrap()).unwrap(),
            timeout
        );
    }

    #[test]
    fn test_result_no_result_allows_missing_test_type() {
        let no_result = TestResult {
            status: TEST_RESULT_NO_RESULT,
            test_type: None,
            reply_frame: None,
            reply_rssi_dbm: None,
            attempt_count: 0,
            elapsed_ms: 0,
        };
        assert_eq!(
            decode_test_result(&encode_test_result(&no_result).unwrap()).unwrap(),
            no_result
        );
    }

    #[test]
    fn test_result_reply_fields_required_on_success() {
        let result = TestResult {
            status: TEST_RESULT_OK,
            test_type: Some(TEST_TYPE_DIAG_FRAME),
            reply_frame: None,
            reply_rssi_dbm: Some(-55),
            attempt_count: 1,
            elapsed_ms: 1,
        };
        assert!(encode_test_result(&result).is_err());
    }

    #[test]
    fn test_result_reply_fields_rejected_on_non_success() {
        let result = TestResult {
            status: TEST_RESULT_EXECUTION_ERROR,
            test_type: Some(TEST_TYPE_DIAG_FRAME),
            reply_frame: Some(vec![0xAA]),
            reply_rssi_dbm: Some(-55),
            attempt_count: 1,
            elapsed_ms: 1,
        };
        assert!(encode_test_result(&result).is_err());
    }

    #[test]
    fn run_test_command_rejects_empty_diag_payload() {
        assert!(encode_run_test_command(TEST_TYPE_DIAG_FRAME, Some(6), &[]).is_err());
    }

    #[test]
    fn decode_run_test_command_missing_rf_channel_for_diag() {
        let body = cbor_encode_map(&[
            (
                TEST_CMD_KEY_TEST_TYPE,
                Value::Integer(TEST_TYPE_DIAG_FRAME.into()),
            ),
            (TEST_CMD_KEY_PAYLOAD, Value::Bytes(vec![0x42; 5])),
        ])
        .unwrap();
        assert!(matches!(
            decode_run_test_command(&body),
            Err(DecodeError::MissingField(TEST_CMD_KEY_RF_CHANNEL))
        ));
    }

    #[test]
    fn decode_run_test_ack_wrong_length() {
        assert!(matches!(
            decode_run_test_ack(&[]),
            Err(DecodeError::TooShort)
        ));
        assert!(matches!(
            decode_run_test_ack(&[RUN_TEST_ACK_INVALID, RUN_TEST_ACK_UNSUPPORTED]),
            Err(DecodeError::TooLong)
        ));
    }

    #[test]
    fn decode_read_test_result_rejects_non_empty_body() {
        assert!(decode_read_test_result(&[0xAA]).is_err());
    }

    #[test]
    fn decode_test_result_rejects_success_without_reply_fields() {
        let body = cbor_encode_map(&[
            (
                TEST_RESULT_KEY_STATUS,
                Value::Integer(TEST_RESULT_OK.into()),
            ),
            (
                TEST_RESULT_KEY_TEST_TYPE,
                Value::Integer(TEST_TYPE_DIAG_FRAME.into()),
            ),
            (TEST_RESULT_KEY_ATTEMPT_COUNT, Value::Integer(1.into())),
            (TEST_RESULT_KEY_ELAPSED_MS, Value::Integer(10.into())),
        ])
        .unwrap();
        assert!(decode_test_result(&body).is_err());
    }

    #[test]
    fn decode_test_result_rejects_trailing_bytes() {
        let mut body = encode_test_result(&TestResult {
            status: TEST_RESULT_TIMEOUT,
            test_type: Some(TEST_TYPE_DIAG_FRAME),
            reply_frame: None,
            reply_rssi_dbm: None,
            attempt_count: 4,
            elapsed_ms: 8_600,
        })
        .unwrap();
        body.push(0x00);
        assert!(matches!(
            decode_test_result(&body),
            Err(DecodeError::TooLong)
        ));
    }
}
