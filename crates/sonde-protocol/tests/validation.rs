// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Protocol crate validation tests.
//!
//! Validation tests from `docs/protocol-crate-validation.md`.

use sonde_protocol::*;

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

// ---------------------------------------------------------------------------
// Software providers
// ---------------------------------------------------------------------------

struct SoftwareHmac;

impl HmacProvider for SoftwareHmac {
    fn compute(&self, key: &[u8], data: &[u8]) -> [u8; 32] {
        let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC can take key of any size");
        mac.update(data);
        mac.finalize().into_bytes().into()
    }

    fn verify(&self, key: &[u8], data: &[u8], expected: &[u8; 32]) -> bool {
        let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC can take key of any size");
        mac.update(data);
        mac.verify_slice(expected).is_ok()
    }
}

struct SoftwareSha256;

impl Sha256Provider for SoftwareSha256 {
    fn hash(&self, data: &[u8]) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(data);
        hasher.finalize().into()
    }
}

// ---------------------------------------------------------------------------
// 2  Frame header tests
// ---------------------------------------------------------------------------

#[test]
fn test_p001() {
    let hdr = FrameHeader {
        key_hint: 0x1234,
        msg_type: 0x01,
        nonce: 0xDEAD_BEEF_CAFE_BABE,
    };
    let bytes = hdr.to_bytes();
    let hdr2 = FrameHeader::from_bytes(&bytes);
    assert_eq!(hdr2.key_hint, 0x1234);
    assert_eq!(hdr2.msg_type, 0x01);
    assert_eq!(hdr2.nonce, 0xDEAD_BEEF_CAFE_BABE);
}

#[test]
fn test_p002() {
    let hdr = FrameHeader {
        key_hint: 0x1234,
        msg_type: 0x01,
        nonce: 0xDEAD_BEEF_CAFE_BABE,
    };
    let b = hdr.to_bytes();
    assert_eq!(b.len(), HEADER_SIZE);
    assert_eq!(b.len(), 11);
    // key_hint big-endian
    assert_eq!(u16::from_be_bytes([b[0], b[1]]), 0x1234);
    // msg_type
    assert_eq!(b[2], 0x01);
    // nonce big-endian
    assert_eq!(
        u64::from_be_bytes(b[3..11].try_into().unwrap()),
        0xDEAD_BEEF_CAFE_BABE
    );
}

#[test]
fn test_p003() {
    let hdr = FrameHeader {
        key_hint: 0,
        msg_type: 0,
        nonce: 0,
    };
    let hdr2 = FrameHeader::from_bytes(&hdr.to_bytes());
    assert_eq!(hdr2.key_hint, 0);
    assert_eq!(hdr2.msg_type, 0);
    assert_eq!(hdr2.nonce, 0);
}

#[test]
fn test_p004() {
    let hdr = FrameHeader {
        key_hint: 0xFFFF,
        msg_type: 0xFF,
        nonce: u64::MAX,
    };
    let hdr2 = FrameHeader::from_bytes(&hdr.to_bytes());
    assert_eq!(hdr2.key_hint, 0xFFFF);
    assert_eq!(hdr2.msg_type, 0xFF);
    assert_eq!(hdr2.nonce, u64::MAX);
}

// ---------------------------------------------------------------------------
// 3  Frame codec tests
// ---------------------------------------------------------------------------

#[test]
fn test_p010() {
    let hdr = FrameHeader {
        key_hint: 1,
        msg_type: MSG_WAKE,
        nonce: 42,
    };
    let payload = vec![0xA1, 0x01, 0x02]; // small CBOR map
    let psk = b"test-key";
    let raw = encode_frame(&hdr, &payload, psk, &SoftwareHmac).unwrap();
    let decoded = decode_frame(&raw).unwrap();
    assert_eq!(decoded.header.key_hint, 1);
    assert_eq!(decoded.header.msg_type, MSG_WAKE);
    assert_eq!(decoded.header.nonce, 42);
    assert_eq!(decoded.payload, payload);
}

#[test]
fn test_p011() {
    let hdr = FrameHeader {
        key_hint: 1,
        msg_type: MSG_WAKE,
        nonce: 1,
    };
    let psk_a = b"psk-a";
    let raw = encode_frame(&hdr, &[0xA0], psk_a, &SoftwareHmac).unwrap();
    let decoded = decode_frame(&raw).unwrap();
    assert!(verify_frame(&decoded, psk_a, &SoftwareHmac));
}

#[test]
fn test_p012() {
    let hdr = FrameHeader {
        key_hint: 1,
        msg_type: MSG_WAKE,
        nonce: 1,
    };
    let psk_a = b"psk-a";
    let psk_b = b"psk-b";
    let raw = encode_frame(&hdr, &[0xA0], psk_a, &SoftwareHmac).unwrap();
    let decoded = decode_frame(&raw).unwrap();
    assert!(!verify_frame(&decoded, psk_b, &SoftwareHmac));
}

#[test]
fn test_p013() {
    let hdr = FrameHeader {
        key_hint: 1,
        msg_type: MSG_WAKE,
        nonce: 1,
    };
    let psk = b"key";
    let payload = vec![0xA1, 0x01, 0x02];
    let mut raw = encode_frame(&hdr, &payload, psk, &SoftwareHmac).unwrap();
    // Flip one bit in the payload portion (byte right after header).
    raw[HEADER_SIZE] ^= 0x01;
    let decoded = decode_frame(&raw).unwrap();
    assert!(!verify_frame(&decoded, psk, &SoftwareHmac));
}

#[test]
fn test_p014() {
    let hdr = FrameHeader {
        key_hint: 1,
        msg_type: MSG_WAKE,
        nonce: 1,
    };
    let psk = b"key";
    let mut raw = encode_frame(&hdr, &[0xA0], psk, &SoftwareHmac).unwrap();
    // Flip one bit in the header (msg_type byte).
    raw[2] ^= 0x01;
    let decoded = decode_frame(&raw).unwrap();
    assert!(!verify_frame(&decoded, psk, &SoftwareHmac));
}

#[test]
fn test_p015() {
    let hdr = FrameHeader {
        key_hint: 1,
        msg_type: MSG_WAKE,
        nonce: 1,
    };
    let psk = b"key";
    let mut raw = encode_frame(&hdr, &[0xA0], psk, &SoftwareHmac).unwrap();
    // Flip one bit in the HMAC trailer (last byte).
    let last = raw.len() - 1;
    raw[last] ^= 0x01;
    let decoded = decode_frame(&raw).unwrap();
    assert!(!verify_frame(&decoded, psk, &SoftwareHmac));
}

#[test]
fn test_p016() {
    let short = vec![0u8; 42];
    let err = decode_frame(&short).unwrap_err();
    assert!(matches!(err, DecodeError::TooShort));
}

#[test]
fn test_p017() {
    let hdr = FrameHeader {
        key_hint: 0,
        msg_type: 0,
        nonce: 0,
    };
    let raw = encode_frame(&hdr, &[], b"k", &SoftwareHmac).unwrap();
    assert_eq!(raw.len(), HEADER_SIZE + HMAC_SIZE); // 11 + 32 = 43
    assert_eq!(raw.len(), MIN_FRAME_SIZE);
    assert!(decode_frame(&raw).is_ok());
}

#[test]
fn test_p018() {
    let hdr = FrameHeader {
        key_hint: 0,
        msg_type: 0,
        nonce: 0,
    };
    // 208 bytes payload → 11 + 208 + 32 = 251 > 250
    let big_payload = vec![0u8; MAX_PAYLOAD_SIZE + 1];
    let err = encode_frame(&hdr, &big_payload, b"k", &SoftwareHmac).unwrap_err();
    assert!(matches!(err, EncodeError::FrameTooLarge));
}

#[test]
fn test_p019() {
    let hdr = FrameHeader {
        key_hint: 0,
        msg_type: 0,
        nonce: 0,
    };
    let payload = vec![0u8; MAX_PAYLOAD_SIZE]; // 207
    let raw = encode_frame(&hdr, &payload, b"k", &SoftwareHmac).unwrap();
    assert_eq!(raw.len(), MAX_FRAME_SIZE); // 250
    assert!(decode_frame(&raw).is_ok());
}

#[test]
fn test_p019a() {
    // Gap 1: DecodeError::TooLong — construct a MAX_FRAME_SIZE + 1 buffer.
    let oversized_len = MAX_FRAME_SIZE + 1;
    assert_eq!(oversized_len, 251);
    let oversized = vec![0u8; oversized_len];
    let err = decode_frame(&oversized).unwrap_err();
    assert!(
        matches!(err, DecodeError::TooLong),
        "expected TooLong, got {:?}",
        err
    );
}

#[test]
fn test_p019b() {
    // Gap 3: DecodeError::CborError — invalid CBOR bytes in payload.
    // Build a valid frame containing invalid CBOR (0xFF 0xFF) so
    // decode_frame() succeeds but NodeMessage::decode() returns CborError.
    let invalid_cbor = [0xFF, 0xFF];
    let hdr = FrameHeader {
        key_hint: 0,
        msg_type: MSG_WAKE,
        nonce: 0,
    };
    let psk = b"test-psk";
    let raw = encode_frame(&hdr, &invalid_cbor, psk, &SoftwareHmac).unwrap();
    let decoded_frame = decode_frame(&raw).unwrap();
    assert!(verify_frame(&decoded_frame, psk, &SoftwareHmac));

    // Frame-level decode succeeded; message-level must fail with CborError.
    let err = NodeMessage::decode(MSG_WAKE, &decoded_frame.payload).unwrap_err();
    assert!(
        matches!(err, DecodeError::CborError(_)),
        "expected CborError, got {:?}",
        err
    );
}

#[test]
fn test_p019c() {
    // Gap 2: DecodeError::InvalidFieldType — text string where uint expected.
    // Build CBOR Wake with KEY_BATTERY_MV set to "hello" instead of uint.
    let map = vec![
        (
            ciborium::Value::Integer(KEY_FIRMWARE_ABI_VERSION.into()),
            ciborium::Value::Integer(1.into()),
        ),
        (
            ciborium::Value::Integer(KEY_PROGRAM_HASH.into()),
            ciborium::Value::Bytes(vec![0xAA; 32]),
        ),
        (
            ciborium::Value::Integer(KEY_BATTERY_MV.into()),
            ciborium::Value::Text("hello".into()),
        ),
    ];
    let mut cbor = Vec::new();
    ciborium::ser::into_writer(&ciborium::Value::Map(map), &mut cbor).unwrap();

    let err = NodeMessage::decode(MSG_WAKE, &cbor).unwrap_err();
    assert!(
        matches!(err, DecodeError::InvalidFieldType(KEY_BATTERY_MV)),
        "expected InvalidFieldType(KEY_BATTERY_MV), got {:?}",
        err
    );
}

// ---------------------------------------------------------------------------
// 4  Message encoding tests
// ---------------------------------------------------------------------------

#[test]
fn test_p020() {
    let msg = NodeMessage::Wake {
        firmware_abi_version: 1,
        program_hash: vec![0xAA; 32],
        battery_mv: 3300,
    };
    let cbor = msg.encode().unwrap();
    let decoded = NodeMessage::decode(MSG_WAKE, &cbor).unwrap();
    match decoded {
        NodeMessage::Wake {
            firmware_abi_version,
            program_hash,
            battery_mv,
        } => {
            assert_eq!(firmware_abi_version, 1);
            assert_eq!(program_hash, vec![0xAA; 32]);
            assert_eq!(battery_mv, 3300);
        }
        _ => panic!("expected Wake"),
    }
}

#[test]
fn test_p021() {
    let msg = NodeMessage::Wake {
        firmware_abi_version: 1,
        program_hash: vec![],
        battery_mv: 3300,
    };
    let cbor = msg.encode().unwrap();
    let decoded = NodeMessage::decode(MSG_WAKE, &cbor).unwrap();
    match decoded {
        NodeMessage::Wake { program_hash, .. } => {
            assert!(program_hash.is_empty());
        }
        _ => panic!("expected Wake"),
    }
}

#[test]
fn test_p022() {
    let msg = GatewayMessage::Command {
        starting_seq: 42,
        timestamp_ms: 1_710_000_000_000,
        payload: CommandPayload::Nop,
    };
    let cbor = msg.encode().unwrap();
    let decoded = GatewayMessage::decode(MSG_COMMAND, &cbor).unwrap();
    match decoded {
        GatewayMessage::Command {
            starting_seq,
            timestamp_ms,
            payload,
        } => {
            assert_eq!(starting_seq, 42);
            assert_eq!(timestamp_ms, 1_710_000_000_000);
            assert!(matches!(payload, CommandPayload::Nop));
        }
        _ => panic!("expected Command"),
    }
}

#[test]
fn test_p023() {
    let hash = vec![0xBB; 32];
    let msg = GatewayMessage::Command {
        starting_seq: 1,
        timestamp_ms: 1_710_000_000_000,
        payload: CommandPayload::UpdateProgram {
            program_hash: hash.clone(),
            program_size: 4000,
            chunk_size: 190,
            chunk_count: 22,
        },
    };
    let cbor = msg.encode().unwrap();
    let decoded = GatewayMessage::decode(MSG_COMMAND, &cbor).unwrap();
    match decoded {
        GatewayMessage::Command {
            starting_seq,
            timestamp_ms,
            payload:
                CommandPayload::UpdateProgram {
                    program_hash,
                    program_size,
                    chunk_size,
                    chunk_count,
                },
        } => {
            assert_eq!(starting_seq, 1);
            assert_eq!(timestamp_ms, 1_710_000_000_000);
            assert_eq!(program_hash, hash);
            assert_eq!(program_size, 4000);
            assert_eq!(chunk_size, 190);
            assert_eq!(chunk_count, 22);
        }
        _ => panic!("expected Command/UpdateProgram"),
    }
}

#[test]
fn test_p024() {
    let msg = GatewayMessage::Command {
        starting_seq: 1,
        timestamp_ms: 0,
        payload: CommandPayload::UpdateSchedule { interval_s: 300 },
    };
    let cbor = msg.encode().unwrap();
    let decoded = GatewayMessage::decode(MSG_COMMAND, &cbor).unwrap();
    match decoded {
        GatewayMessage::Command {
            payload: CommandPayload::UpdateSchedule { interval_s },
            ..
        } => {
            assert_eq!(interval_s, 300);
        }
        _ => panic!("expected UpdateSchedule"),
    }
}

#[test]
fn test_p025() {
    let msg = NodeMessage::GetChunk { chunk_index: 7 };
    let cbor = msg.encode().unwrap();
    let decoded = NodeMessage::decode(MSG_GET_CHUNK, &cbor).unwrap();
    match decoded {
        NodeMessage::GetChunk { chunk_index } => assert_eq!(chunk_index, 7),
        _ => panic!("expected GetChunk"),
    }
}

#[test]
fn test_p026() {
    let data = vec![0x55; 190];
    let msg = GatewayMessage::Chunk {
        chunk_index: 7,
        chunk_data: data.clone(),
    };
    let cbor = msg.encode().unwrap();
    let decoded = GatewayMessage::decode(MSG_CHUNK, &cbor).unwrap();
    match decoded {
        GatewayMessage::Chunk {
            chunk_index,
            chunk_data,
        } => {
            assert_eq!(chunk_index, 7);
            assert_eq!(chunk_data.len(), 190);
            assert_eq!(chunk_data, data);
        }
        _ => panic!("expected Chunk"),
    }
}

#[test]
fn test_p027() {
    let msg = NodeMessage::AppData {
        blob: vec![1, 2, 3, 4, 5],
    };
    let cbor = msg.encode().unwrap();
    let decoded = NodeMessage::decode(MSG_APP_DATA, &cbor).unwrap();
    match decoded {
        NodeMessage::AppData { blob } => assert_eq!(blob, vec![1, 2, 3, 4, 5]),
        _ => panic!("expected AppData"),
    }
}

#[test]
fn test_p028() {
    let msg = GatewayMessage::AppDataReply {
        blob: vec![0xAA, 0xBB],
    };
    let cbor = msg.encode().unwrap();
    let decoded = GatewayMessage::decode(MSG_APP_DATA_REPLY, &cbor).unwrap();
    match decoded {
        GatewayMessage::AppDataReply { blob } => assert_eq!(blob, vec![0xAA, 0xBB]),
        _ => panic!("expected AppDataReply"),
    }
}

#[test]
fn test_p029() {
    // Encode a Wake, then inject an extra CBOR key.
    let msg = NodeMessage::Wake {
        firmware_abi_version: 1,
        program_hash: vec![0xAA; 32],
        battery_mv: 3300,
    };
    let cbor = msg.encode().unwrap();

    // Decode the CBOR into a ciborium Value, add an extra key, re-encode.
    let val: ciborium::Value = ciborium::de::from_reader(&cbor[..]).unwrap();
    let mut map = match val {
        ciborium::Value::Map(m) => m,
        _ => panic!("expected CBOR map"),
    };
    map.push((
        ciborium::Value::Integer(99.into()),
        ciborium::Value::Text("unknown".into()),
    ));
    let mut modified = Vec::new();
    ciborium::ser::into_writer(&ciborium::Value::Map(map), &mut modified).unwrap();

    let decoded = NodeMessage::decode(MSG_WAKE, &modified).unwrap();
    match decoded {
        NodeMessage::Wake {
            firmware_abi_version,
            program_hash,
            battery_mv,
        } => {
            assert_eq!(firmware_abi_version, 1);
            assert_eq!(program_hash, vec![0xAA; 32]);
            assert_eq!(battery_mv, 3300);
        }
        _ => panic!("expected Wake"),
    }
}

#[test]
fn test_p030() {
    // Manually construct CBOR for Wake with battery_mv (key 3) omitted.
    let map = vec![
        (
            ciborium::Value::Integer(KEY_FIRMWARE_ABI_VERSION.into()),
            ciborium::Value::Integer(1.into()),
        ),
        (
            ciborium::Value::Integer(KEY_PROGRAM_HASH.into()),
            ciborium::Value::Bytes(vec![0xAA; 32]),
        ),
    ];
    // KEY_BATTERY_MV deliberately omitted
    let mut cbor = Vec::new();
    ciborium::ser::into_writer(&ciborium::Value::Map(map), &mut cbor).unwrap();

    let err = NodeMessage::decode(MSG_WAKE, &cbor).unwrap_err();
    assert!(
        matches!(err, DecodeError::MissingField(KEY_BATTERY_MV)),
        "expected MissingField(KEY_BATTERY_MV), got {:?}",
        err
    );
}

#[test]
fn test_p031() {
    let valid_cbor = NodeMessage::Wake {
        firmware_abi_version: 1,
        program_hash: vec![],
        battery_mv: 3300,
    }
    .encode()
    .unwrap();
    let err = NodeMessage::decode(0xFF, &valid_cbor).unwrap_err();
    assert!(
        matches!(err, DecodeError::InvalidMsgType(0xFF)),
        "expected InvalidMsgType(0xFF), got {:?}",
        err
    );
}

#[test]
fn test_p032() {
    let msg = NodeMessage::Wake {
        firmware_abi_version: 1,
        program_hash: vec![0xAA; 32],
        battery_mv: 3300,
    };
    let cbor = msg.encode().unwrap();
    // Decode as generic CBOR and check that keys are positive integers, not strings.
    let val: ciborium::Value = ciborium::de::from_reader(&cbor[..]).unwrap();
    let map = match val {
        ciborium::Value::Map(m) => m,
        _ => panic!("expected CBOR map"),
    };
    for (key, _) in &map {
        match key {
            ciborium::Value::Integer(i) => {
                let v: i128 = (*i).into();
                assert!(v > 0, "keys should be small positive integers, got {v}");
            }
            _ => panic!("expected integer key, got {:?}", key),
        }
    }
    assert!(!map.is_empty());
}

// ---------------------------------------------------------------------------
// T-P033  ProgramAck round-trip
// ---------------------------------------------------------------------------

#[test]
fn test_p033() {
    let program_hash = vec![0xABu8; 32];
    let msg = NodeMessage::ProgramAck {
        program_hash: program_hash.clone(),
    };
    let cbor = msg.encode().unwrap();
    let decoded = NodeMessage::decode(MSG_PROGRAM_ACK, &cbor).unwrap();
    match decoded {
        NodeMessage::ProgramAck { program_hash: ph } => {
            assert_eq!(ph, program_hash);
        }
        _ => panic!("expected ProgramAck"),
    }
}

// ---------------------------------------------------------------------------
// T-P034  Cmd(RunEphemeral) round-trip
// ---------------------------------------------------------------------------

#[test]
fn test_p034() {
    let hash = vec![0xBBu8; 32];
    let msg = GatewayMessage::Command {
        starting_seq: 100,
        timestamp_ms: 1_710_000_000_000,
        payload: CommandPayload::RunEphemeral {
            program_hash: hash.clone(),
            program_size: 4000,
            chunk_size: 190,
            chunk_count: 22,
        },
    };
    let cbor = msg.encode().unwrap();
    let decoded = GatewayMessage::decode(MSG_COMMAND, &cbor).unwrap();
    match decoded {
        GatewayMessage::Command {
            starting_seq,
            timestamp_ms,
            payload:
                CommandPayload::RunEphemeral {
                    program_hash,
                    program_size,
                    chunk_size,
                    chunk_count,
                },
        } => {
            assert_eq!(starting_seq, 100);
            assert_eq!(timestamp_ms, 1_710_000_000_000);
            assert_eq!(program_hash, hash);
            assert_eq!(program_size, 4000);
            assert_eq!(chunk_size, 190);
            assert_eq!(chunk_count, 22);
        }
        _ => panic!("expected Command/RunEphemeral"),
    }
}

// ---------------------------------------------------------------------------
// T-P035  Cmd(Reboot) round-trip
// ---------------------------------------------------------------------------

#[test]
fn test_p035() {
    let msg = GatewayMessage::Command {
        starting_seq: 1,
        timestamp_ms: 1_710_000_000_000,
        payload: CommandPayload::Reboot,
    };
    let cbor = msg.encode().unwrap();

    // Inspect raw CBOR: KEY_COMMAND_TYPE present with value 0x04, no KEY_PAYLOAD
    let raw: ciborium::Value = ciborium::from_reader(cbor.as_slice()).expect("valid CBOR");
    if let ciborium::Value::Map(pairs) = &raw {
        let keys: Vec<u64> = pairs
            .iter()
            .filter_map(|(k, _)| k.as_integer().and_then(|i| u64::try_from(i).ok()))
            .collect();
        // Must have KEY_COMMAND_TYPE (4), KEY_STARTING_SEQ (13), KEY_TIMESTAMP_MS (14)
        assert!(keys.contains(&KEY_COMMAND_TYPE));
        // Must NOT have KEY_PAYLOAD (5)
        assert!(!keys.contains(&KEY_PAYLOAD));
        // Verify command_type value is 0x04 (REBOOT)
        let cmd_type_val = pairs
            .iter()
            .find(|(k, _)| {
                k.as_integer().and_then(|i| u64::try_from(i).ok()) == Some(KEY_COMMAND_TYPE)
            })
            .map(|(_, v)| v)
            .expect("KEY_COMMAND_TYPE present");
        let cmd_type: u64 = cmd_type_val
            .as_integer()
            .and_then(|i| u64::try_from(i).ok())
            .expect("integer value");
        assert_eq!(cmd_type, CMD_REBOOT as u64);
    } else {
        panic!("expected CBOR map");
    }

    let decoded = GatewayMessage::decode(MSG_COMMAND, &cbor).unwrap();
    match decoded {
        GatewayMessage::Command {
            starting_seq,
            timestamp_ms,
            payload,
        } => {
            assert_eq!(starting_seq, 1);
            assert_eq!(timestamp_ms, 1_710_000_000_000);
            assert!(matches!(payload, CommandPayload::Reboot));
        }
        _ => panic!("expected Command/Reboot"),
    }
}

// ---------------------------------------------------------------------------
// T-P036  Missing-field detection for non-Wake types
// ---------------------------------------------------------------------------

#[test]
fn test_p036() {
    // Gap 4: Missing required field for each non-Wake message type.

    // Command — omit KEY_STARTING_SEQ
    {
        let map = vec![
            (
                ciborium::Value::Integer(KEY_COMMAND_TYPE.into()),
                ciborium::Value::Integer(0.into()), // NOP
            ),
            (
                ciborium::Value::Integer(KEY_TIMESTAMP_MS.into()),
                ciborium::Value::Integer(1000.into()),
            ),
        ];
        let mut cbor = Vec::new();
        ciborium::ser::into_writer(&ciborium::Value::Map(map), &mut cbor).unwrap();
        let err = GatewayMessage::decode(MSG_COMMAND, &cbor).unwrap_err();
        assert!(
            matches!(err, DecodeError::MissingField(KEY_STARTING_SEQ)),
            "Command missing starting_seq: expected MissingField(KEY_STARTING_SEQ), got {:?}",
            err
        );
    }

    // GetChunk — omit KEY_CHUNK_INDEX
    {
        let map: Vec<(ciborium::Value, ciborium::Value)> = vec![];
        let mut cbor = Vec::new();
        ciborium::ser::into_writer(&ciborium::Value::Map(map), &mut cbor).unwrap();
        let err = NodeMessage::decode(MSG_GET_CHUNK, &cbor).unwrap_err();
        assert!(
            matches!(err, DecodeError::MissingField(KEY_CHUNK_INDEX)),
            "GetChunk missing chunk_index: expected MissingField(KEY_CHUNK_INDEX), got {:?}",
            err
        );
    }

    // Chunk — omit KEY_CHUNK_DATA (keep KEY_CHUNK_INDEX)
    {
        let map = vec![(
            ciborium::Value::Integer(KEY_CHUNK_INDEX.into()),
            ciborium::Value::Integer(0.into()),
        )];
        let mut cbor = Vec::new();
        ciborium::ser::into_writer(&ciborium::Value::Map(map), &mut cbor).unwrap();
        let err = GatewayMessage::decode(MSG_CHUNK, &cbor).unwrap_err();
        assert!(
            matches!(err, DecodeError::MissingField(KEY_CHUNK_DATA)),
            "Chunk missing chunk_data: expected MissingField(KEY_CHUNK_DATA), got {:?}",
            err
        );
    }

    // ProgramAck — omit KEY_PROGRAM_HASH
    {
        let map: Vec<(ciborium::Value, ciborium::Value)> = vec![];
        let mut cbor = Vec::new();
        ciborium::ser::into_writer(&ciborium::Value::Map(map), &mut cbor).unwrap();
        let err = NodeMessage::decode(MSG_PROGRAM_ACK, &cbor).unwrap_err();
        assert!(
            matches!(err, DecodeError::MissingField(KEY_PROGRAM_HASH)),
            "ProgramAck missing program_hash: expected MissingField(KEY_PROGRAM_HASH), got {:?}",
            err
        );
    }

    // AppData — omit KEY_BLOB
    {
        let map: Vec<(ciborium::Value, ciborium::Value)> = vec![];
        let mut cbor = Vec::new();
        ciborium::ser::into_writer(&ciborium::Value::Map(map), &mut cbor).unwrap();
        let err = NodeMessage::decode(MSG_APP_DATA, &cbor).unwrap_err();
        assert!(
            matches!(err, DecodeError::MissingField(KEY_BLOB)),
            "AppData missing blob: expected MissingField(KEY_BLOB), got {:?}",
            err
        );
    }

    // AppDataReply — omit KEY_BLOB
    {
        let map: Vec<(ciborium::Value, ciborium::Value)> = vec![];
        let mut cbor = Vec::new();
        ciborium::ser::into_writer(&ciborium::Value::Map(map), &mut cbor).unwrap();
        let err = GatewayMessage::decode(MSG_APP_DATA_REPLY, &cbor).unwrap_err();
        assert!(
            matches!(err, DecodeError::MissingField(KEY_BLOB)),
            "AppDataReply missing blob: expected MissingField(KEY_BLOB), got {:?}",
            err
        );
    }
}

// ---------------------------------------------------------------------------
// T-P037  Unknown CBOR keys ignored in non-Wake messages
// ---------------------------------------------------------------------------

#[test]
fn test_p037() {
    // Gap 5: Inject unknown key 99 into each non-Wake message type.

    // Helper: decode CBOR, inject key 99, re-encode.
    fn inject_unknown_key(cbor: &[u8]) -> Vec<u8> {
        let val: ciborium::Value = ciborium::de::from_reader(cbor).unwrap();
        let mut map = match val {
            ciborium::Value::Map(m) => m,
            _ => panic!("expected CBOR map"),
        };
        map.push((
            ciborium::Value::Integer(99.into()),
            ciborium::Value::Text("unknown".into()),
        ));
        let mut out = Vec::new();
        ciborium::ser::into_writer(&ciborium::Value::Map(map), &mut out).unwrap();
        out
    }

    // Command (NOP)
    {
        let orig = GatewayMessage::Command {
            starting_seq: 42,
            timestamp_ms: 1_710_000_000_000,
            payload: CommandPayload::Nop,
        };
        let cbor = orig.encode().unwrap();
        let modified = inject_unknown_key(&cbor);
        let decoded = GatewayMessage::decode(MSG_COMMAND, &modified).unwrap();
        assert_eq!(decoded, orig);
    }

    // GetChunk
    {
        let orig = NodeMessage::GetChunk { chunk_index: 7 };
        let cbor = orig.encode().unwrap();
        let modified = inject_unknown_key(&cbor);
        let decoded = NodeMessage::decode(MSG_GET_CHUNK, &modified).unwrap();
        assert_eq!(decoded, orig);
    }

    // Chunk
    {
        let orig = GatewayMessage::Chunk {
            chunk_index: 3,
            chunk_data: vec![0x55; 10],
        };
        let cbor = orig.encode().unwrap();
        let modified = inject_unknown_key(&cbor);
        let decoded = GatewayMessage::decode(MSG_CHUNK, &modified).unwrap();
        assert_eq!(decoded, orig);
    }

    // ProgramAck
    {
        let orig = NodeMessage::ProgramAck {
            program_hash: vec![0xAB; 32],
        };
        let cbor = orig.encode().unwrap();
        let modified = inject_unknown_key(&cbor);
        let decoded = NodeMessage::decode(MSG_PROGRAM_ACK, &modified).unwrap();
        assert_eq!(decoded, orig);
    }

    // AppData
    {
        let orig = NodeMessage::AppData {
            blob: vec![1, 2, 3],
        };
        let cbor = orig.encode().unwrap();
        let modified = inject_unknown_key(&cbor);
        let decoded = NodeMessage::decode(MSG_APP_DATA, &modified).unwrap();
        assert_eq!(decoded, orig);
    }

    // AppDataReply
    {
        let orig = GatewayMessage::AppDataReply {
            blob: vec![0xAA, 0xBB],
        };
        let cbor = orig.encode().unwrap();
        let modified = inject_unknown_key(&cbor);
        let decoded = GatewayMessage::decode(MSG_APP_DATA_REPLY, &modified).unwrap();
        assert_eq!(decoded, orig);
    }
}

// ---------------------------------------------------------------------------
// T-P038  COMMAND nested payload CBOR byte inspection
// ---------------------------------------------------------------------------

#[test]
fn test_p038() {
    // Gap 6: Verify COMMAND uses nested CBOR structure on the wire.

    // UpdateProgram — top-level keys {4, 5, 13, 14}, key 5 holds nested map
    // with UpdateProgram sub-fields {2, 6, 7, 8}.
    {
        let msg = GatewayMessage::Command {
            starting_seq: 1,
            timestamp_ms: 1_710_000_000_000,
            payload: CommandPayload::UpdateProgram {
                program_hash: vec![0xBB; 32],
                program_size: 4000,
                chunk_size: 190,
                chunk_count: 22,
            },
        };
        let cbor = msg.encode().unwrap();
        let val: ciborium::Value = ciborium::from_reader(cbor.as_slice()).expect("valid CBOR");
        let pairs = match &val {
            ciborium::Value::Map(m) => m,
            _ => panic!("expected CBOR map"),
        };

        // Collect top-level keys
        let keys: Vec<u64> = pairs
            .iter()
            .filter_map(|(k, _)| k.as_integer().and_then(|i| u64::try_from(i).ok()))
            .collect();
        assert!(keys.contains(&KEY_COMMAND_TYPE), "missing KEY_COMMAND_TYPE");
        assert!(keys.contains(&KEY_PAYLOAD), "missing KEY_PAYLOAD");
        assert!(keys.contains(&KEY_STARTING_SEQ), "missing KEY_STARTING_SEQ");
        assert!(keys.contains(&KEY_TIMESTAMP_MS), "missing KEY_TIMESTAMP_MS");

        // Key 5 (PAYLOAD) must be a nested CBOR map with sub-keys {2, 6, 7, 8}
        let payload_val = pairs
            .iter()
            .find(|(k, _)| k.as_integer().and_then(|i| u64::try_from(i).ok()) == Some(KEY_PAYLOAD))
            .map(|(_, v)| v)
            .expect("KEY_PAYLOAD present");
        let inner_pairs = match payload_val {
            ciborium::Value::Map(m) => m,
            _ => panic!("KEY_PAYLOAD must be a nested CBOR map"),
        };
        let inner_keys: Vec<u64> = inner_pairs
            .iter()
            .filter_map(|(k, _)| k.as_integer().and_then(|i| u64::try_from(i).ok()))
            .collect();
        assert!(
            inner_keys.contains(&KEY_PROGRAM_HASH),
            "nested map missing KEY_PROGRAM_HASH"
        );
        assert!(
            inner_keys.contains(&KEY_PROGRAM_SIZE),
            "nested map missing KEY_PROGRAM_SIZE"
        );
        assert!(
            inner_keys.contains(&KEY_CHUNK_SIZE),
            "nested map missing KEY_CHUNK_SIZE"
        );
        assert!(
            inner_keys.contains(&KEY_CHUNK_COUNT),
            "nested map missing KEY_CHUNK_COUNT"
        );
    }

    // NOP — top-level keys {4, 13, 14} only, no KEY_PAYLOAD
    {
        let msg = GatewayMessage::Command {
            starting_seq: 1,
            timestamp_ms: 1_710_000_000_000,
            payload: CommandPayload::Nop,
        };
        let cbor = msg.encode().unwrap();
        let val: ciborium::Value = ciborium::from_reader(cbor.as_slice()).expect("valid CBOR");
        let pairs = match &val {
            ciborium::Value::Map(m) => m,
            _ => panic!("expected CBOR map"),
        };
        let keys: Vec<u64> = pairs
            .iter()
            .filter_map(|(k, _)| k.as_integer().and_then(|i| u64::try_from(i).ok()))
            .collect();
        assert!(keys.contains(&KEY_COMMAND_TYPE));
        assert!(keys.contains(&KEY_STARTING_SEQ));
        assert!(keys.contains(&KEY_TIMESTAMP_MS));
        assert!(
            !keys.contains(&KEY_PAYLOAD),
            "NOP must not have KEY_PAYLOAD"
        );
    }

    // Reboot — top-level keys {4, 13, 14} only, no KEY_PAYLOAD
    {
        let msg = GatewayMessage::Command {
            starting_seq: 1,
            timestamp_ms: 1_710_000_000_000,
            payload: CommandPayload::Reboot,
        };
        let cbor = msg.encode().unwrap();
        let val: ciborium::Value = ciborium::from_reader(cbor.as_slice()).expect("valid CBOR");
        let pairs = match &val {
            ciborium::Value::Map(m) => m,
            _ => panic!("expected CBOR map"),
        };
        let keys: Vec<u64> = pairs
            .iter()
            .filter_map(|(k, _)| k.as_integer().and_then(|i| u64::try_from(i).ok()))
            .collect();
        assert!(keys.contains(&KEY_COMMAND_TYPE));
        assert!(keys.contains(&KEY_STARTING_SEQ));
        assert!(keys.contains(&KEY_TIMESTAMP_MS));
        assert!(
            !keys.contains(&KEY_PAYLOAD),
            "Reboot must not have KEY_PAYLOAD"
        );
    }
}

// ---------------------------------------------------------------------------
// T-P039  Large u64 values round-trip
// ---------------------------------------------------------------------------

#[test]
fn test_p039() {
    // Gap 7: Large integer values — CBOR 8-byte encoding.

    // Wake with battery_mv = u32::MAX
    {
        let msg = NodeMessage::Wake {
            firmware_abi_version: 1,
            program_hash: vec![0xAA; 32],
            battery_mv: u32::MAX,
        };
        let cbor = msg.encode().unwrap();
        let decoded = NodeMessage::decode(MSG_WAKE, &cbor).unwrap();
        match decoded {
            NodeMessage::Wake { battery_mv, .. } => {
                assert_eq!(battery_mv, u32::MAX);
            }
            _ => panic!("expected Wake"),
        }

        // Inspect CBOR bytes: u32::MAX (0xFFFFFFFF) should be encoded as
        // major type 0, additional info 26 (4-byte uint) → byte 0x1A.
        let val: ciborium::Value = ciborium::from_reader(cbor.as_slice()).expect("valid CBOR");
        let pairs = match &val {
            ciborium::Value::Map(m) => m,
            _ => panic!("expected map"),
        };
        let battery_val = pairs
            .iter()
            .find(|(k, _)| {
                k.as_integer().and_then(|i| u64::try_from(i).ok()) == Some(KEY_BATTERY_MV)
            })
            .map(|(_, v)| v)
            .expect("KEY_BATTERY_MV present");
        let battery: u64 = battery_val
            .as_integer()
            .and_then(|i| u64::try_from(i).ok())
            .expect("integer value");
        assert_eq!(battery, u32::MAX as u64);

        // Verify CBOR encoding length for battery_mv: search for 0x1A prefix
        // (major type 0 | additional info 26 = 4-byte uint).
        // The value 0xFFFFFFFF follows as 4 bytes.
        let has_4byte_encoding = cbor
            .windows(5)
            .any(|w| w[0] == 0x1A && w[1..] == [0xFF, 0xFF, 0xFF, 0xFF]);
        assert!(
            has_4byte_encoding,
            "u32::MAX should be CBOR-encoded as 4-byte uint (0x1A prefix)"
        );
    }

    // Command with starting_seq = u64::MAX and timestamp_ms = u64::MAX
    {
        let msg = GatewayMessage::Command {
            starting_seq: u64::MAX,
            timestamp_ms: u64::MAX,
            payload: CommandPayload::Nop,
        };
        let cbor = msg.encode().unwrap();
        let decoded = GatewayMessage::decode(MSG_COMMAND, &cbor).unwrap();
        match decoded {
            GatewayMessage::Command {
                starting_seq,
                timestamp_ms,
                ..
            } => {
                assert_eq!(starting_seq, u64::MAX);
                assert_eq!(timestamp_ms, u64::MAX);
            }
            _ => panic!("expected Command"),
        }

        // Inspect CBOR bytes: u64::MAX should be encoded as
        // major type 0, additional info 27 (8-byte uint) → byte 0x1B.
        let val: ciborium::Value = ciborium::from_reader(cbor.as_slice()).expect("valid CBOR");
        let pairs = match &val {
            ciborium::Value::Map(m) => m,
            _ => panic!("expected map"),
        };

        // Verify starting_seq
        let seq_val = pairs
            .iter()
            .find(|(k, _)| {
                k.as_integer().and_then(|i| u64::try_from(i).ok()) == Some(KEY_STARTING_SEQ)
            })
            .map(|(_, v)| v)
            .expect("KEY_STARTING_SEQ present");
        let seq: u64 = seq_val
            .as_integer()
            .and_then(|i| u64::try_from(i).ok())
            .expect("integer value");
        assert_eq!(seq, u64::MAX);

        // Verify timestamp_ms
        let ts_val = pairs
            .iter()
            .find(|(k, _)| {
                k.as_integer().and_then(|i| u64::try_from(i).ok()) == Some(KEY_TIMESTAMP_MS)
            })
            .map(|(_, v)| v)
            .expect("KEY_TIMESTAMP_MS present");
        let ts: u64 = ts_val
            .as_integer()
            .and_then(|i| u64::try_from(i).ok())
            .expect("integer value");
        assert_eq!(ts, u64::MAX);

        // Verify CBOR encoding: 0x1B prefix for 8-byte uint.
        // u64::MAX = 0xFFFFFFFFFFFFFFFF, should appear as
        // 0x1B 0xFF 0xFF 0xFF 0xFF 0xFF 0xFF 0xFF 0xFF
        let eight_byte_count = cbor
            .windows(9)
            .filter(|w| w[0] == 0x1B && w[1..] == [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF])
            .count();
        assert!(
            eight_byte_count >= 2,
            "expected at least two 8-byte uint encodings for u64::MAX, found {}",
            eight_byte_count
        );
    }
}

// ---------------------------------------------------------------------------
// 5  Program image tests
// ---------------------------------------------------------------------------

#[test]
fn test_p040() {
    let img = ProgramImage {
        bytecode: vec![0x18, 0x01, 0x00, 0x00],
        maps: vec![MapDef {
            map_type: 1,
            key_size: 4,
            value_size: 64,
            max_entries: 16,
        }],
    };
    let cbor = img.encode_deterministic().unwrap();
    let decoded = ProgramImage::decode(&cbor).unwrap();
    assert_eq!(decoded.bytecode, img.bytecode);
    assert_eq!(decoded.maps.len(), 1);
    assert_eq!(decoded.maps[0].map_type, 1);
    assert_eq!(decoded.maps[0].key_size, 4);
    assert_eq!(decoded.maps[0].value_size, 64);
    assert_eq!(decoded.maps[0].max_entries, 16);
}

#[test]
fn test_p041() {
    let img = ProgramImage {
        bytecode: vec![0x01],
        maps: vec![],
    };
    let cbor = img.encode_deterministic().unwrap();
    let decoded = ProgramImage::decode(&cbor).unwrap();
    assert!(decoded.maps.is_empty());
}

#[test]
fn test_p042() {
    let make_img = || ProgramImage {
        bytecode: vec![0x18, 0x01],
        maps: vec![MapDef {
            map_type: 1,
            key_size: 4,
            value_size: 64,
            max_entries: 16,
        }],
    };
    let cbor_a = make_img().encode_deterministic().unwrap();
    let cbor_b = make_img().encode_deterministic().unwrap();
    assert_eq!(
        cbor_a, cbor_b,
        "deterministic encoding must be byte-identical"
    );
}

#[test]
fn test_p043() {
    let img = ProgramImage {
        bytecode: vec![0x18, 0x01],
        maps: vec![MapDef {
            map_type: 1,
            key_size: 4,
            value_size: 64,
            max_entries: 16,
        }],
    };
    let cbor = img.encode_deterministic().unwrap();
    let reference_hash = program_hash(&cbor, &SoftwareSha256);
    for _ in 0..100 {
        let h = program_hash(&cbor, &SoftwareSha256);
        assert_eq!(h, reference_hash);
    }
}

#[test]
fn test_p044() {
    let bytecode = vec![0x18, 0x01];
    let img_a = ProgramImage {
        bytecode: bytecode.clone(),
        maps: vec![MapDef {
            map_type: 1,
            key_size: 4,
            value_size: 64,
            max_entries: 16,
        }],
    };
    let img_b = ProgramImage {
        bytecode,
        maps: vec![MapDef {
            map_type: 1,
            key_size: 4,
            value_size: 64,
            max_entries: 32,
        }],
    };
    let ha = program_hash(&img_a.encode_deterministic().unwrap(), &SoftwareSha256);
    let hb = program_hash(&img_b.encode_deterministic().unwrap(), &SoftwareSha256);
    assert_ne!(ha, hb);
}

#[test]
fn test_p045() {
    let maps = vec![MapDef {
        map_type: 1,
        key_size: 4,
        value_size: 64,
        max_entries: 16,
    }];
    let img_a = ProgramImage {
        bytecode: vec![0x01],
        maps: maps.clone(),
    };
    let img_b = ProgramImage {
        bytecode: vec![0x02],
        maps,
    };
    let ha = program_hash(&img_a.encode_deterministic().unwrap(), &SoftwareSha256);
    let hb = program_hash(&img_b.encode_deterministic().unwrap(), &SoftwareSha256);
    assert_ne!(ha, hb);
}

#[test]
fn test_p046() {
    let img = ProgramImage {
        bytecode: vec![0x18, 0x01],
        maps: vec![MapDef {
            map_type: 1,
            key_size: 4,
            value_size: 64,
            max_entries: 16,
        }],
    };
    let cbor = img.encode_deterministic().unwrap();
    // Decode as generic CBOR and verify key ordering.
    let val: ciborium::Value = ciborium::de::from_reader(&cbor[..]).unwrap();
    let map = match val {
        ciborium::Value::Map(m) => m,
        _ => panic!("expected CBOR map"),
    };
    let keys: Vec<i128> = map
        .iter()
        .map(|(k, _)| match k {
            ciborium::Value::Integer(i) => (*i).into(),
            _ => panic!("expected integer key"),
        })
        .collect();
    for w in keys.windows(2) {
        assert!(
            w[0] < w[1],
            "keys must be in ascending order: {} >= {}",
            w[0],
            w[1]
        );
    }
}

// T-P047  ProgramImage with empty bytecode round-trip
#[test]
fn test_p047() {
    let img = ProgramImage {
        bytecode: vec![],
        maps: vec![MapDef {
            map_type: 1,
            key_size: 4,
            value_size: 64,
            max_entries: 16,
        }],
    };
    let cbor = img.encode_deterministic().unwrap();
    let decoded = ProgramImage::decode(&cbor).unwrap();
    assert!(decoded.bytecode.is_empty());
    assert_eq!(decoded.maps.len(), 1);
    assert_eq!(decoded.maps[0].map_type, 1);
    assert_eq!(decoded.maps[0].key_size, 4);
    assert_eq!(decoded.maps[0].value_size, 64);
    assert_eq!(decoded.maps[0].max_entries, 16);

    // Also test fully empty image (no bytecode, no maps).
    let img2 = ProgramImage {
        bytecode: vec![],
        maps: vec![],
    };
    let cbor2 = img2.encode_deterministic().unwrap();
    let decoded2 = ProgramImage::decode(&cbor2).unwrap();
    assert!(decoded2.bytecode.is_empty());
    assert!(decoded2.maps.is_empty());
}

// ---------------------------------------------------------------------------
// 6  Chunking helper tests
// ---------------------------------------------------------------------------

#[test]
fn test_p050() {
    assert_eq!(chunk_count(4000, 190), Some(22));
    assert_eq!(chunk_count(190, 190), Some(1));
    assert_eq!(chunk_count(0, 190), Some(0));
    assert_eq!(chunk_count(1, 190), Some(1));
    assert_eq!(chunk_count(380, 190), Some(2));
    assert_eq!(chunk_count(100, 0), None);
}

#[test]
fn test_p051() {
    let image = (0u16..400).map(|i| (i & 0xFF) as u8).collect::<Vec<_>>();
    let c0 = get_chunk(&image, 0, 190).unwrap();
    assert_eq!(c0, &image[..190]);
    let c1 = get_chunk(&image, 1, 190).unwrap();
    assert_eq!(c1, &image[190..380]);
    let c2 = get_chunk(&image, 2, 190).unwrap();
    assert_eq!(c2, &image[380..400]);
    assert_eq!(c2.len(), 20);
}

#[test]
fn test_p052() {
    let image = vec![0u8; 400];
    assert!(get_chunk(&image, 3, 190).is_none());
    assert!(get_chunk(&image, 100, 190).is_none());
}

#[test]
fn test_p053() {
    let img = ProgramImage {
        bytecode: vec![0xAB; 300],
        maps: vec![
            MapDef {
                map_type: 1,
                key_size: 4,
                value_size: 64,
                max_entries: 16,
            },
            MapDef {
                map_type: 2,
                key_size: 8,
                value_size: 128,
                max_entries: 32,
            },
        ],
    };
    let cbor = img.encode_deterministic().unwrap();
    let n = chunk_count(cbor.len(), 190).unwrap();
    let mut reassembled = Vec::new();
    for i in 0..n {
        let chunk = get_chunk(&cbor, i, 190).unwrap();
        reassembled.extend_from_slice(chunk);
    }
    assert_eq!(reassembled, cbor);
    let hash_orig = program_hash(&cbor, &SoftwareSha256);
    let hash_reasm = program_hash(&reassembled, &SoftwareSha256);
    assert_eq!(hash_orig, hash_reasm);
}

// T-P054  get_chunk with chunk_size = 0 returns None
#[test]
fn test_p054() {
    let image = vec![0x42u8; 100];
    // chunk_size = 0 must return None, not Some(&[]).
    assert!(get_chunk(&image, 0, 0).is_none());
    assert!(get_chunk(&image, 1, 0).is_none());
    // Empty image with chunk_size = 0 also returns None.
    let empty: &[u8] = &[];
    assert!(get_chunk(empty, 0, 0).is_none());
}

// T-P055  chunk_count overflow / extreme values
#[test]
fn test_p055() {
    // Exactly u32::MAX chunks should fit on all architectures.
    assert_eq!(chunk_count(u32::MAX as usize, 1), Some(u32::MAX));

    // Large image_size with large chunk_size that yields a small count.
    assert_eq!(chunk_count(usize::MAX, usize::MAX), Some(1));
    assert_eq!(chunk_count(usize::MAX - 1, usize::MAX), Some(1));

    if usize::BITS > 32 {
        // On 64-bit (or wider) targets, usize::MAX with chunk_size = 1 would overflow naive
        // (image_size + chunk_size - 1) arithmetic. The result (usize::MAX chunks) exceeds
        // u32::MAX, so must return None.
        assert_eq!(chunk_count(usize::MAX, 1), None);

        // u32::MAX + 1 chunks also doesn't fit in u32. Compute via u64 to avoid overflow.
        let too_many = (u32::MAX as u64 + 1) as usize;
        assert_eq!(chunk_count(too_many, 1), None);
    } else {
        // On 32-bit targets, usize::MAX == u32::MAX, so chunk_count(usize::MAX, 1) returns
        // Some(u32::MAX). The "u32::MAX + 1" case is not representable and is skipped.
        assert_eq!(chunk_count(usize::MAX, 1), Some(u32::MAX));
    }
}

// ---------------------------------------------------------------------------
// 7  Full integration tests
// ---------------------------------------------------------------------------

#[test]
fn test_p060() {
    let msg = NodeMessage::Wake {
        firmware_abi_version: 1,
        program_hash: vec![0xAA; 32],
        battery_mv: 3300,
    };
    let cbor = msg.encode().unwrap();
    let hdr = FrameHeader {
        key_hint: 0x0001,
        msg_type: MSG_WAKE,
        nonce: 12345,
    };
    let psk = b"integration-psk";
    let raw = encode_frame(&hdr, &cbor, psk, &SoftwareHmac).unwrap();
    let decoded = decode_frame(&raw).unwrap();
    assert!(verify_frame(&decoded, psk, &SoftwareHmac));
    let msg2 = NodeMessage::decode(decoded.header.msg_type, &decoded.payload).unwrap();
    match msg2 {
        NodeMessage::Wake {
            firmware_abi_version,
            program_hash,
            battery_mv,
        } => {
            assert_eq!(firmware_abi_version, 1);
            assert_eq!(program_hash, vec![0xAA; 32]);
            assert_eq!(battery_mv, 3300);
        }
        _ => panic!("expected Wake"),
    }
}

#[test]
fn test_p061() {
    let hash = vec![0xCC; 32];
    let msg = GatewayMessage::Command {
        starting_seq: 99,
        timestamp_ms: 1_710_000_000_000,
        payload: CommandPayload::UpdateProgram {
            program_hash: hash.clone(),
            program_size: 4000,
            chunk_size: 190,
            chunk_count: 22,
        },
    };
    let cbor = msg.encode().unwrap();
    let hdr = FrameHeader {
        key_hint: 0x0002,
        msg_type: MSG_COMMAND,
        nonce: 67890,
    };
    let psk = b"gw-psk";
    let raw = encode_frame(&hdr, &cbor, psk, &SoftwareHmac).unwrap();
    let decoded = decode_frame(&raw).unwrap();
    assert!(verify_frame(&decoded, psk, &SoftwareHmac));
    let msg2 = GatewayMessage::decode(decoded.header.msg_type, &decoded.payload).unwrap();
    match msg2 {
        GatewayMessage::Command {
            starting_seq,
            timestamp_ms,
            payload:
                CommandPayload::UpdateProgram {
                    program_hash,
                    program_size,
                    chunk_size,
                    chunk_count,
                },
        } => {
            assert_eq!(starting_seq, 99);
            assert_eq!(timestamp_ms, 1_710_000_000_000);
            assert_eq!(program_hash, hash);
            assert_eq!(program_size, 4000);
            assert_eq!(chunk_size, 190);
            assert_eq!(chunk_count, 22);
        }
        _ => panic!("expected Command/UpdateProgram"),
    }
}

#[test]
fn test_p062() {
    let img = ProgramImage {
        bytecode: vec![0xDE; 500],
        maps: vec![
            MapDef {
                map_type: 1,
                key_size: 4,
                value_size: 64,
                max_entries: 16,
            },
            MapDef {
                map_type: 2,
                key_size: 8,
                value_size: 128,
                max_entries: 64,
            },
            MapDef {
                map_type: 3,
                key_size: 16,
                value_size: 256,
                max_entries: 8,
            },
        ],
    };
    let cbor = img.encode_deterministic().unwrap();
    let hash_orig = program_hash(&cbor, &SoftwareSha256);

    let n = chunk_count(cbor.len(), 190).unwrap();
    let mut reassembled = Vec::new();
    for i in 0..n {
        reassembled.extend_from_slice(get_chunk(&cbor, i, 190).unwrap());
    }
    let hash_reasm = program_hash(&reassembled, &SoftwareSha256);
    assert_eq!(hash_orig, hash_reasm);

    let decoded = ProgramImage::decode(&reassembled).unwrap();
    assert_eq!(decoded.bytecode, img.bytecode);
    assert_eq!(decoded.maps.len(), 3);
    assert_eq!(decoded.maps[0].map_type, 1);
    assert_eq!(decoded.maps[1].map_type, 2);
    assert_eq!(decoded.maps[2].map_type, 3);
    assert_eq!(decoded.maps[0].max_entries, 16);
    assert_eq!(decoded.maps[1].max_entries, 64);
    assert_eq!(decoded.maps[2].max_entries, 8);
}

// ---------------------------------------------------------------------------
// Validation gap tests (issue #347)
// ---------------------------------------------------------------------------

// T-P063: NodeMessage::decode rejects gateway msg_types.
#[test]
fn test_p063() {
    // Protocol §4: 0x80–0xFF = Gateway→Node.
    // NodeMessage::decode must reject all gateway direction msg_types.
    let wake_cbor = NodeMessage::Wake {
        firmware_abi_version: 1,
        program_hash: vec![0xAA; 32],
        battery_mv: 3300,
    }
    .encode()
    .unwrap();

    for &gw_type in &[MSG_COMMAND, MSG_CHUNK, MSG_APP_DATA_REPLY, MSG_PEER_ACK] {
        let err = NodeMessage::decode(gw_type, &wake_cbor).unwrap_err();
        assert!(
            matches!(err, DecodeError::InvalidMsgType(t) if t == gw_type),
            "NodeMessage::decode should reject gateway msg_type 0x{:02x}, got {:?}",
            gw_type,
            err
        );
    }
}

// T-P064: GatewayMessage::decode rejects node msg_types.
#[test]
fn test_p064() {
    // Protocol §4: 0x01–0x7F = Node→Gateway.
    // GatewayMessage::decode must reject all node direction msg_types.
    let cmd_cbor = GatewayMessage::Command {
        starting_seq: 1,
        timestamp_ms: 1_710_000_000_000,
        payload: CommandPayload::Nop,
    }
    .encode()
    .unwrap();

    for &node_type in &[
        MSG_WAKE,
        MSG_GET_CHUNK,
        MSG_PROGRAM_ACK,
        MSG_APP_DATA,
        MSG_PEER_REQUEST,
    ] {
        let err = GatewayMessage::decode(node_type, &cmd_cbor).unwrap_err();
        assert!(
            matches!(err, DecodeError::InvalidMsgType(t) if t == node_type),
            "GatewayMessage::decode should reject node msg_type 0x{:02x}, got {:?}",
            node_type,
            err
        );
    }
}

// T-P065: Nonce echo verification — WAKE → COMMAND pair.
#[test]
fn test_p065() {
    // Protocol §7.3: The response frame echoes the request nonce so the
    // receiver can correlate request/response pairs.  This test verifies
    // round-trip fidelity of the nonce field through encode → decode and
    // also demonstrates that a mismatched nonce is detectable.
    let psk = [0x42u8; 32];
    let wake_nonce: u64 = 0x1234_5678_90AB_CDEF;

    // 1-2. Node sends WAKE; decode and verify nonce round-trips.
    let wake = NodeMessage::Wake {
        firmware_abi_version: 1,
        program_hash: vec![0xAA; 32],
        battery_mv: 3300,
    };
    let wake_frame = encode_frame(
        &FrameHeader {
            key_hint: 0x0001,
            msg_type: MSG_WAKE,
            nonce: wake_nonce,
        },
        &wake.encode().unwrap(),
        &psk,
        &SoftwareHmac,
    )
    .unwrap();
    let decoded_wake = decode_frame(&wake_frame).unwrap();
    assert!(verify_frame(&decoded_wake, &psk, &SoftwareHmac));
    assert_eq!(decoded_wake.header.nonce, wake_nonce);

    // 3-4. Gateway echoes the nonce in its COMMAND response.
    let cmd = GatewayMessage::Command {
        starting_seq: 1,
        timestamp_ms: 1_710_000_000_000,
        payload: CommandPayload::Nop,
    };
    // Note: this test only verifies encode→decode round-trip fidelity of
    // the nonce field, not protocol-level nonce binding enforcement (which
    // is the responsibility of the node/gateway state machines).
    let cmd_frame = encode_frame(
        &FrameHeader {
            key_hint: 0x0001,
            msg_type: MSG_COMMAND,
            nonce: decoded_wake.header.nonce,
        },
        &cmd.encode().unwrap(),
        &psk,
        &SoftwareHmac,
    )
    .unwrap();
    let decoded_cmd = decode_frame(&cmd_frame).unwrap();
    assert!(verify_frame(&decoded_cmd, &psk, &SoftwareHmac));
    assert_eq!(
        decoded_cmd.header.nonce, wake_nonce,
        "COMMAND must echo the WAKE nonce"
    );

    // Both messages must decode to the correct types.
    let _ = NodeMessage::decode(decoded_wake.header.msg_type, &decoded_wake.payload).unwrap();
    let _ = GatewayMessage::decode(decoded_cmd.header.msg_type, &decoded_cmd.payload).unwrap();

    // 5-6. Negative case: a COMMAND with a different nonce must be
    // detectable by comparing it to the original WAKE nonce.
    let bad_cmd_frame = encode_frame(
        &FrameHeader {
            key_hint: 0x0001,
            msg_type: MSG_COMMAND,
            nonce: 0xFFFF_FFFF_FFFF_FFFF,
        },
        &cmd.encode().unwrap(),
        &psk,
        &SoftwareHmac,
    )
    .unwrap();
    let decoded_bad_cmd = decode_frame(&bad_cmd_frame).unwrap();
    assert!(verify_frame(&decoded_bad_cmd, &psk, &SoftwareHmac));
    assert_ne!(
        decoded_bad_cmd.header.nonce, wake_nonce,
        "mismatched COMMAND nonce must be detectable by comparison with WAKE nonce"
    );
}

// T-P066: Nonce echo verification — GET_CHUNK → CHUNK and APP_DATA → APP_DATA_REPLY pairs.
#[test]
fn test_p066() {
    let psk = [0x42u8; 32];

    // GET_CHUNK → CHUNK nonce binding.
    let get_chunk_nonce: u64 = 0xAAAA_BBBB_CCCC_DDDD;
    let get_chunk = NodeMessage::GetChunk { chunk_index: 3 };
    let get_chunk_frame = encode_frame(
        &FrameHeader {
            key_hint: 0x0001,
            msg_type: MSG_GET_CHUNK,
            nonce: get_chunk_nonce,
        },
        &get_chunk.encode().unwrap(),
        &psk,
        &SoftwareHmac,
    )
    .unwrap();
    let decoded_gc = decode_frame(&get_chunk_frame).unwrap();
    assert!(verify_frame(&decoded_gc, &psk, &SoftwareHmac));
    assert_eq!(decoded_gc.header.nonce, get_chunk_nonce);

    let chunk = GatewayMessage::Chunk {
        chunk_index: 3,
        chunk_data: vec![0xDD; 32],
    };
    let chunk_frame = encode_frame(
        &FrameHeader {
            key_hint: 0x0001,
            msg_type: MSG_CHUNK,
            nonce: decoded_gc.header.nonce,
        },
        &chunk.encode().unwrap(),
        &psk,
        &SoftwareHmac,
    )
    .unwrap();
    let decoded_chunk = decode_frame(&chunk_frame).unwrap();
    assert!(verify_frame(&decoded_chunk, &psk, &SoftwareHmac));
    assert_eq!(
        decoded_chunk.header.nonce, get_chunk_nonce,
        "CHUNK must echo the GET_CHUNK nonce"
    );

    // APP_DATA → APP_DATA_REPLY nonce binding.
    let app_nonce: u64 = 0x1111_2222_3333_4444;
    let app_data = NodeMessage::AppData {
        blob: vec![0xEE; 16],
    };
    let app_frame = encode_frame(
        &FrameHeader {
            key_hint: 0x0001,
            msg_type: MSG_APP_DATA,
            nonce: app_nonce,
        },
        &app_data.encode().unwrap(),
        &psk,
        &SoftwareHmac,
    )
    .unwrap();
    let decoded_app = decode_frame(&app_frame).unwrap();
    assert!(verify_frame(&decoded_app, &psk, &SoftwareHmac));
    assert_eq!(decoded_app.header.nonce, app_nonce);

    let reply = GatewayMessage::AppDataReply {
        blob: vec![0xFF; 8],
    };
    let reply_frame = encode_frame(
        &FrameHeader {
            key_hint: 0x0001,
            msg_type: MSG_APP_DATA_REPLY,
            nonce: decoded_app.header.nonce,
        },
        &reply.encode().unwrap(),
        &psk,
        &SoftwareHmac,
    )
    .unwrap();
    let decoded_reply = decode_frame(&reply_frame).unwrap();
    assert!(verify_frame(&decoded_reply, &psk, &SoftwareHmac));
    assert_eq!(
        decoded_reply.header.nonce, app_nonce,
        "APP_DATA_REPLY must echo the APP_DATA nonce"
    );
}

// T-P067: Multiple APP_DATA with incrementing sequences.
#[test]
fn test_p067() {
    // This test verifies that the codec faithfully round-trips distinct
    // nonce values across multiple frames (sequence enforcement is the
    // responsibility of the node/gateway state machines, not the codec).
    // The assertion below sanity-checks nonce round-trip fidelity based
    // on test construction, not codec-enforced sequencing.
    let psk = [0x42u8; 32];

    let payloads: Vec<Vec<u8>> = vec![
        vec![0x11; 8],
        vec![0x22; 8],
        vec![0x33; 8],
        vec![0x44; 8],
        vec![0x55; 8],
    ];

    let mut frames = Vec::new();
    for (i, blob) in payloads.iter().enumerate() {
        let msg = NodeMessage::AppData { blob: blob.clone() };
        let hdr = FrameHeader {
            key_hint: 0x0001,
            msg_type: MSG_APP_DATA,
            nonce: (i + 1) as u64,
        };
        let frame = encode_frame(&hdr, &msg.encode().unwrap(), &psk, &SoftwareHmac).unwrap();
        frames.push(frame);
    }

    let mut prev_nonce = 0u64;
    for (i, raw) in frames.iter().enumerate() {
        let decoded = decode_frame(raw).unwrap();
        assert!(verify_frame(&decoded, &psk, &SoftwareHmac));
        assert_eq!(decoded.header.msg_type, MSG_APP_DATA);
        assert_eq!(
            decoded.header.nonce,
            (i + 1) as u64,
            "APP_DATA[{}] nonce mismatch",
            i
        );
        // Given the test construction (nonce = i + 1), each decoded nonce should
        // be strictly greater than the previous; this sanity-checks nonce
        // round-trip across multiple frames, not codec-enforced sequencing.
        assert!(
            decoded.header.nonce > prev_nonce,
            "APP_DATA[{i}] decoded nonce {} should be strictly greater than previous {prev_nonce} based on test construction",
            decoded.header.nonce
        );
        prev_nonce = decoded.header.nonce;

        let msg = NodeMessage::decode(decoded.header.msg_type, &decoded.payload).unwrap();
        match msg {
            NodeMessage::AppData { blob } => {
                assert_eq!(blob, payloads[i], "APP_DATA[{}] payload mismatch", i);
            }
            _ => panic!("expected AppData"),
        }
    }
}

// T-P068: HMAC constant-time comparison behavior.
#[test]
fn test_p068() {
    // This test exercises `SoftwareHmac::verify` directly (not through
    // `verify_frame`) to confirm it correctly accepts valid tags and
    // rejects any corruption.  Constant-time behavior itself cannot be
    // verified by functional tests — it is guaranteed by the `hmac` crate's
    // `Mac::verify_slice` which delegates to `subtle::ConstantTimeEq`.
    // That implementation detail is enforced via code review per
    // protocol-crate-validation.md T-P068.
    let hmac_provider = SoftwareHmac;
    let key = &[0x42u8; 32];
    let data = b"authenticated payload data";

    let correct_mac = hmac_provider.compute(key, data);

    // Correct tag must verify.
    assert!(
        hmac_provider.verify(key, data, &correct_mac),
        "verify must accept correct HMAC"
    );

    // Single-bit flip (first byte) must fail.
    let mut flipped_first = correct_mac;
    flipped_first[0] ^= 0x01;
    assert!(
        !hmac_provider.verify(key, data, &flipped_first),
        "verify must reject single-bit-flipped HMAC (first byte)"
    );

    // All-zero tag must fail.
    let zero_tag = [0u8; 32];
    assert!(
        !hmac_provider.verify(key, data, &zero_tag),
        "verify must reject all-zero HMAC tag"
    );

    // Single-bit flip in the last byte must fail.
    let mut flipped_last = correct_mac;
    let last_index = flipped_last.len() - 1;
    flipped_last[last_index] ^= 0x80;
    assert!(
        !hmac_provider.verify(key, data, &flipped_last),
        "verify must reject single-bit-flipped HMAC (last byte)"
    );

    // Wrong data with the correct tag must fail.
    let wrong_data = b"authenticated payload data (modified)";
    assert!(
        !hmac_provider.verify(key, wrong_data, &correct_mac),
        "verify must reject correct HMAC used with wrong data"
    );

    // Wrong key with the correct tag must fail.
    let wrong_key = &[0x24u8; 32];
    assert!(
        !hmac_provider.verify(wrong_key, data, &correct_mac),
        "verify must reject correct HMAC used with wrong key"
    );
}
