// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Protocol crate validation tests.
//!
//! Validation tests from `docs/protocol-crate-validation.md`.

use sonde_protocol::*;

use sha2::{Digest, Sha256};

// ---------------------------------------------------------------------------
// Software providers
// ---------------------------------------------------------------------------

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
// 3  Frame codec tests (HMAC codec removed — see AEAD tests at end of file)
// ---------------------------------------------------------------------------

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
        map_initial_data: vec![Vec::new()],
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
        map_initial_data: vec![],
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
        map_initial_data: vec![Vec::new()],
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
        map_initial_data: vec![Vec::new()],
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
        map_initial_data: vec![Vec::new()],
    };
    let img_b = ProgramImage {
        bytecode,
        maps: vec![MapDef {
            map_type: 1,
            key_size: 4,
            value_size: 64,
            max_entries: 32,
        }],
        map_initial_data: vec![Vec::new()],
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
        map_initial_data: vec![Vec::new(); maps.len()],
    };
    let img_b = ProgramImage {
        bytecode: vec![0x02],
        map_initial_data: vec![Vec::new(); maps.len()],
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
        map_initial_data: vec![Vec::new()],
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
        map_initial_data: vec![Vec::new()],
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
        map_initial_data: vec![],
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
        map_initial_data: vec![Vec::new(); 2],
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
// 7  Full integration tests (HMAC tests removed — see AEAD tests at end of file)
// ---------------------------------------------------------------------------

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
        map_initial_data: vec![Vec::new(); 3],
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

// ---------------------------------------------------------------------------
// T-P090: CommandPayload::command_type() derivation covers all variants
// ---------------------------------------------------------------------------

/// Verify that `CommandPayload::command_type()` returns the correct wire code
/// for every variant, and that encode → decode round-trips preserve it.
#[test]
fn test_p090_command_type_derived_from_payload() {
    let variants: Vec<(CommandPayload, u8)> = vec![
        (CommandPayload::Nop, CMD_NOP),
        (
            CommandPayload::UpdateProgram {
                program_hash: vec![0x42; 32],
                program_size: 1024,
                chunk_size: 190,
                chunk_count: 6,
            },
            CMD_UPDATE_PROGRAM,
        ),
        (
            CommandPayload::RunEphemeral {
                program_hash: vec![0x42; 32],
                program_size: 512,
                chunk_size: 190,
                chunk_count: 3,
            },
            CMD_RUN_EPHEMERAL,
        ),
        (
            CommandPayload::UpdateSchedule { interval_s: 60 },
            CMD_UPDATE_SCHEDULE,
        ),
        (CommandPayload::Reboot, CMD_REBOOT),
    ];

    for (payload, expected_code) in &variants {
        // Verify the accessor returns the correct code.
        assert_eq!(
            payload.command_type(),
            *expected_code,
            "command_type() mismatch for {:?}",
            payload
        );

        // Encode and decode a full Command, then verify the round-tripped
        // payload returns the same command_type code.
        let msg = GatewayMessage::Command {
            starting_seq: 1,
            timestamp_ms: 1000,
            payload: payload.clone(),
        };
        let cbor = msg.encode().unwrap();
        let decoded = GatewayMessage::decode(MSG_COMMAND, &cbor).unwrap();
        if let GatewayMessage::Command {
            payload: decoded_payload,
            ..
        } = &decoded
        {
            assert_eq!(
                decoded_payload.command_type(),
                *expected_code,
                "round-trip command_type() mismatch for {:?}",
                payload
            );
        } else {
            panic!("expected GatewayMessage::Command");
        }
        assert_eq!(decoded, msg);
    }
}

// ---------------------------------------------------------------------------
// T-P048  §7.2 Deterministic CBOR — minimal-length integer encoding
//
// RFC 8949 §4.2 requires smallest possible encoding for integers.
// Values 0–23 encode in 1 byte, 24–255 in 2 bytes, etc.
// This test verifies that encode_deterministic() uses minimal encoding
// by inspecting raw CBOR byte sequences and comparing against a
// reference encoder (ciborium, which also uses minimal encoding).
// ---------------------------------------------------------------------------

#[test]
fn test_p048() {
    // Test with values that span encoding boundaries.
    let img = ProgramImage {
        bytecode: vec![0xAB],
        maps: vec![MapDef {
            map_type: 1,      // 0–23 range → 1-byte encoding
            key_size: 23,     // boundary: last 1-byte value
            value_size: 24,   // boundary: first 2-byte value
            max_entries: 256, // boundary: first 3-byte value
        }],
        map_initial_data: vec![Vec::new()],
    };

    let cbor = img.encode_deterministic().unwrap();
    let decoded = ProgramImage::decode(&cbor).unwrap();
    assert_eq!(decoded, img, "round-trip must preserve all values");

    // Minimal integer encodings for 23, 24, and 256 are validated via the
    // reference encoder comparison below (ciborium uses minimal-length CBOR
    // integers), so we do not perform additional raw byte subsequence checks.

    // Build a reference version of the same map using ciborium. ciborium
    // uses minimal-length CBOR integers, so this is a reference-encoder
    // comparison proving our encoder matches a known-good implementation.
    let reference_map = ciborium::Value::Map(vec![
        (
            ciborium::Value::Integer(IMG_KEY_BYTECODE.into()),
            ciborium::Value::Bytes(vec![0xAB]),
        ),
        (
            ciborium::Value::Integer(IMG_KEY_MAPS.into()),
            ciborium::Value::Array(vec![ciborium::Value::Map(vec![
                (
                    ciborium::Value::Integer(MAP_KEY_TYPE.into()),
                    ciborium::Value::Integer(1.into()),
                ),
                (
                    ciborium::Value::Integer(MAP_KEY_KEY_SIZE.into()),
                    ciborium::Value::Integer(23.into()),
                ),
                (
                    ciborium::Value::Integer(MAP_KEY_VALUE_SIZE.into()),
                    ciborium::Value::Integer(24.into()),
                ),
                (
                    ciborium::Value::Integer(MAP_KEY_MAX_ENTRIES.into()),
                    ciborium::Value::Integer(256.into()),
                ),
            ])]),
        ),
    ]);

    let mut reference_encoded = Vec::new();
    ciborium::ser::into_writer(&reference_map, &mut reference_encoded).unwrap();

    // Our output must match the reference encoder byte-for-byte.
    assert_eq!(
        cbor, reference_encoded,
        "encode_deterministic must produce the same minimal encoding as ciborium"
    );
}

// ---------------------------------------------------------------------------
// T-P049  §7.3 ProgramImage::decode() with malformed CBOR
// ---------------------------------------------------------------------------

// T-P049a: truncated CBOR
#[test]
fn test_p049a() {
    // Encode a valid image, then truncate at various points.
    let img = ProgramImage {
        bytecode: vec![0xAB; 8],
        maps: vec![MapDef {
            map_type: 1,
            key_size: 4,
            value_size: 64,
            max_entries: 16,
        }],
        map_initial_data: vec![Vec::new()],
    };
    let cbor = img.encode_deterministic().unwrap();
    assert!(cbor.len() > 10, "encoded image must be non-trivial");

    // Truncate to 0 bytes.
    let err = ProgramImage::decode(&[]).unwrap_err();
    assert!(
        matches!(err, DecodeError::CborError(_)),
        "empty input: expected CborError, got {:?}",
        err
    );

    // Truncate to 1 byte.
    let err = ProgramImage::decode(&cbor[..1]).unwrap_err();
    assert!(
        matches!(err, DecodeError::CborError(_)),
        "1-byte truncation: expected CborError, got {:?}",
        err
    );

    // Truncate to half the encoded size.
    let half = cbor.len() / 2;
    let err = ProgramImage::decode(&cbor[..half]).unwrap_err();
    assert!(
        matches!(err, DecodeError::CborError(_)),
        "half-truncation: expected CborError, got {:?}",
        err
    );
}

// ---------------------------------------------------------------------------
// T-P049b  §7.3 ProgramImage::decode() — missing bytecode field
// ---------------------------------------------------------------------------

#[test]
fn test_p049b() {
    // CBOR map with only the maps field (key 2), no bytecode (key 1).
    let maps_value = ciborium::Value::Array(vec![ciborium::Value::Map(vec![
        (
            ciborium::Value::Integer(MAP_KEY_TYPE.into()),
            ciborium::Value::Integer(1.into()),
        ),
        (
            ciborium::Value::Integer(MAP_KEY_KEY_SIZE.into()),
            ciborium::Value::Integer(4.into()),
        ),
        (
            ciborium::Value::Integer(MAP_KEY_VALUE_SIZE.into()),
            ciborium::Value::Integer(64.into()),
        ),
        (
            ciborium::Value::Integer(MAP_KEY_MAX_ENTRIES.into()),
            ciborium::Value::Integer(16.into()),
        ),
    ])]);

    let map = vec![(ciborium::Value::Integer(IMG_KEY_MAPS.into()), maps_value)];

    let mut cbor = Vec::new();
    ciborium::ser::into_writer(&ciborium::Value::Map(map), &mut cbor).unwrap();

    let err = ProgramImage::decode(&cbor).unwrap_err();
    assert!(
        matches!(err, DecodeError::MissingField(IMG_KEY_BYTECODE)),
        "missing bytecode: expected MissingField(IMG_KEY_BYTECODE), got {:?}",
        err
    );
}

// ---------------------------------------------------------------------------
// T-P049c  §7.3 ProgramImage::decode() — wrong field types
// ---------------------------------------------------------------------------

#[test]
fn test_p049c() {
    // bytecode (key 1) is an integer instead of bytes.
    {
        let map = vec![
            (
                ciborium::Value::Integer(IMG_KEY_BYTECODE.into()),
                ciborium::Value::Integer(42.into()),
            ),
            (
                ciborium::Value::Integer(IMG_KEY_MAPS.into()),
                ciborium::Value::Array(vec![]),
            ),
        ];
        let mut cbor = Vec::new();
        ciborium::ser::into_writer(&ciborium::Value::Map(map), &mut cbor).unwrap();

        let err = ProgramImage::decode(&cbor).unwrap_err();
        assert!(
            matches!(err, DecodeError::InvalidFieldType(IMG_KEY_BYTECODE)),
            "bytecode as integer: expected InvalidFieldType(IMG_KEY_BYTECODE), got {:?}",
            err
        );
    }

    // maps (key 2) is an integer instead of array.
    {
        let map = vec![
            (
                ciborium::Value::Integer(IMG_KEY_BYTECODE.into()),
                ciborium::Value::Bytes(vec![0xAB; 4]),
            ),
            (
                ciborium::Value::Integer(IMG_KEY_MAPS.into()),
                ciborium::Value::Integer(99.into()),
            ),
        ];
        let mut cbor = Vec::new();
        ciborium::ser::into_writer(&ciborium::Value::Map(map), &mut cbor).unwrap();

        let err = ProgramImage::decode(&cbor).unwrap_err();
        assert!(
            matches!(err, DecodeError::InvalidFieldType(IMG_KEY_MAPS)),
            "maps as integer: expected InvalidFieldType(IMG_KEY_MAPS), got {:?}",
            err
        );
    }

    // maps array entry is a text string instead of a nested map.
    {
        let map = vec![
            (
                ciborium::Value::Integer(IMG_KEY_BYTECODE.into()),
                ciborium::Value::Bytes(vec![0xAB; 4]),
            ),
            (
                ciborium::Value::Integer(IMG_KEY_MAPS.into()),
                ciborium::Value::Array(vec![ciborium::Value::Text("not a map".into())]),
            ),
        ];
        let mut cbor = Vec::new();
        ciborium::ser::into_writer(&ciborium::Value::Map(map), &mut cbor).unwrap();

        let err = ProgramImage::decode(&cbor).unwrap_err();
        assert!(
            matches!(err, DecodeError::InvalidFieldType(IMG_KEY_MAPS)),
            "map entry as text: expected InvalidFieldType(IMG_KEY_MAPS), got {:?}",
            err
        );
    }

    // Nested map has MAP_KEY_TYPE as text instead of integer.
    // The decoder's integer parser returns None for non-integer values,
    // so the field is treated as missing (MissingField) rather than as
    // an invalid type. This is the correct decoder behavior — text values
    // are not silently accepted.
    {
        let map = vec![
            (
                ciborium::Value::Integer(IMG_KEY_BYTECODE.into()),
                ciborium::Value::Bytes(vec![0xAB; 4]),
            ),
            (
                ciborium::Value::Integer(IMG_KEY_MAPS.into()),
                ciborium::Value::Array(vec![ciborium::Value::Map(vec![
                    (
                        ciborium::Value::Integer(MAP_KEY_TYPE.into()),
                        ciborium::Value::Text("wrong".into()),
                    ),
                    (
                        ciborium::Value::Integer(MAP_KEY_KEY_SIZE.into()),
                        ciborium::Value::Integer(4.into()),
                    ),
                    (
                        ciborium::Value::Integer(MAP_KEY_VALUE_SIZE.into()),
                        ciborium::Value::Integer(64.into()),
                    ),
                    (
                        ciborium::Value::Integer(MAP_KEY_MAX_ENTRIES.into()),
                        ciborium::Value::Integer(16.into()),
                    ),
                ])]),
            ),
        ];
        let mut cbor = Vec::new();
        ciborium::ser::into_writer(&ciborium::Value::Map(map), &mut cbor).unwrap();

        let err = ProgramImage::decode(&cbor).unwrap_err();
        assert!(
            matches!(err, DecodeError::MissingField(MAP_KEY_TYPE)),
            "map_type as text: expected MissingField(MAP_KEY_TYPE), got {:?}",
            err
        );
    }

    // Top-level value is not a map (it's an array).
    {
        let val = ciborium::Value::Array(vec![ciborium::Value::Integer(1.into())]);
        let mut cbor = Vec::new();
        ciborium::ser::into_writer(&val, &mut cbor).unwrap();

        let err = ProgramImage::decode(&cbor).unwrap_err();
        assert!(
            matches!(err, DecodeError::CborError(_)),
            "top-level array: expected CborError, got {:?}",
            err
        );
    }
}

/// T-P070: ProgramImage initial data round-trip (protocol-crate-validation.md).
///
/// Validates: protocol.md §6 — key 5 `initial_data`.
#[test]
fn test_p070() {
    let initial = vec![0xDE, 0xAD, 0xBE, 0xEF];
    let img = ProgramImage {
        bytecode: vec![0x01],
        maps: vec![MapDef {
            map_type: 1,
            key_size: 4,
            value_size: 4,
            max_entries: 1,
        }],
        map_initial_data: vec![initial.clone()],
    };
    let cbor = img.encode_deterministic().unwrap();
    let decoded = ProgramImage::decode(&cbor).unwrap();

    // Round-trip: initial data survives encode → decode.
    assert_eq!(decoded.map_initial_data.len(), 1);
    assert_eq!(decoded.map_initial_data[0], initial);

    // Verify raw CBOR contains key 5 in the map definition entry.
    let raw: ciborium::Value = ciborium::from_reader(cbor.as_slice()).unwrap();
    let maps_arr = raw.as_map().unwrap()[1].1.as_array().unwrap();
    let map_entry = maps_arr[0].as_map().unwrap();
    let has_key5 = map_entry
        .iter()
        .any(|(k, _)| k.as_integer() == Some(ciborium::value::Integer::from(5)));
    assert!(has_key5, "key 5 (initial_data) must be present in CBOR");
}

/// T-P071: ProgramImage initial data absent when empty (protocol-crate-validation.md).
///
/// Validates: protocol.md §6 — key 5 omission for empty initial data.
#[test]
fn test_p071() {
    let img = ProgramImage {
        bytecode: vec![0x01],
        maps: vec![MapDef {
            map_type: 1,
            key_size: 4,
            value_size: 64,
            max_entries: 16,
        }],
        map_initial_data: vec![Vec::new()],
    };
    let cbor = img.encode_deterministic().unwrap();

    // Raw CBOR must NOT contain key 5 when initial data is empty.
    let raw: ciborium::Value = ciborium::from_reader(cbor.as_slice()).unwrap();
    let maps_arr = raw.as_map().unwrap()[1].1.as_array().unwrap();
    let map_entry = maps_arr[0].as_map().unwrap();
    let has_key5 = map_entry
        .iter()
        .any(|(k, _)| k.as_integer() == Some(ciborium::value::Integer::from(5)));
    assert!(
        !has_key5,
        "key 5 (initial_data) must be absent when data is empty"
    );

    // Round-trip: decoded initial data is empty.
    let decoded = ProgramImage::decode(&cbor).unwrap();
    assert_eq!(decoded.map_initial_data.len(), 1);
    assert!(decoded.map_initial_data[0].is_empty());
}

/// Encode rejects `map_initial_data` / `maps` length mismatch.
///
/// Validates: protocol.md §6 — `map_initial_data` is parallel to `maps`.
#[test]
fn test_p072() {
    let img = ProgramImage {
        bytecode: vec![0x01],
        maps: vec![MapDef {
            map_type: 1,
            key_size: 4,
            value_size: 4,
            max_entries: 1,
        }],
        // 2 entries vs 1 map — mismatch
        map_initial_data: vec![vec![0xDE, 0xAD, 0xBE, 0xEF], vec![0x01, 0x02, 0x03, 0x04]],
    };
    let result = img.encode_deterministic();
    assert!(result.is_err(), "length mismatch must be rejected");
}

// ---------------------------------------------------------------------------
// AES-256-GCM codec tests
// ---------------------------------------------------------------------------

mod aead_tests {
    use aes_gcm::aead::{Aead, KeyInit, Payload};
    use aes_gcm::{Aes256Gcm, Nonce};
    use sha2::{Digest, Sha256};
    use sonde_protocol::*;

    struct SoftwareAead;

    impl AeadProvider for SoftwareAead {
        fn seal(&self, key: &[u8; 32], nonce: &[u8; 12], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
            let cipher = Aes256Gcm::new_from_slice(key).expect("valid 32-byte key");
            let gcm_nonce = Nonce::from_slice(nonce);
            cipher
                .encrypt(
                    gcm_nonce,
                    Payload {
                        msg: plaintext,
                        aad,
                    },
                )
                .expect("encryption should not fail")
        }

        fn open(
            &self,
            key: &[u8; 32],
            nonce: &[u8; 12],
            aad: &[u8],
            ciphertext_and_tag: &[u8],
        ) -> Option<Vec<u8>> {
            let cipher = Aes256Gcm::new_from_slice(key).ok()?;
            let gcm_nonce = Nonce::from_slice(nonce);
            cipher
                .decrypt(
                    gcm_nonce,
                    Payload {
                        msg: ciphertext_and_tag,
                        aad,
                    },
                )
                .ok()
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

    #[test]
    fn aead_round_trip() {
        let hdr = FrameHeader {
            key_hint: 1,
            msg_type: MSG_WAKE,
            nonce: 42,
        };
        let payload = vec![0xA1, 0x01, 0x02];
        let psk = [0x42u8; 32];

        let raw = encode_frame_aead(&hdr, &payload, &psk, &SoftwareAead, &SoftwareSha256).unwrap();
        let decoded = decode_frame_aead(&raw).unwrap();
        assert_eq!(decoded.header.key_hint, 1);
        assert_eq!(decoded.header.msg_type, MSG_WAKE);
        assert_eq!(decoded.header.nonce, 42);

        let plaintext = open_frame(&decoded, &psk, &SoftwareAead, &SoftwareSha256).unwrap();
        assert_eq!(plaintext, payload);
    }

    #[test]
    fn aead_wrong_key() {
        let hdr = FrameHeader {
            key_hint: 1,
            msg_type: MSG_WAKE,
            nonce: 1,
        };
        let psk_a = [0x42u8; 32];
        let psk_b = [0x24u8; 32];
        let raw = encode_frame_aead(&hdr, &[0xA0], &psk_a, &SoftwareAead, &SoftwareSha256).unwrap();
        let decoded = decode_frame_aead(&raw).unwrap();
        let result = open_frame(&decoded, &psk_b, &SoftwareAead, &SoftwareSha256);
        assert_eq!(result, Err(DecodeError::AuthenticationFailed));
    }

    #[test]
    fn aead_tampered_ciphertext() {
        let hdr = FrameHeader {
            key_hint: 1,
            msg_type: MSG_WAKE,
            nonce: 1,
        };
        let psk = [0x42u8; 32];
        let mut raw = encode_frame_aead(
            &hdr,
            &[0xA1, 0x01, 0x02],
            &psk,
            &SoftwareAead,
            &SoftwareSha256,
        )
        .unwrap();
        // Flip one bit in the ciphertext portion (byte right after header).
        raw[HEADER_SIZE] ^= 0x01;
        let decoded = decode_frame_aead(&raw).unwrap();
        let result = open_frame(&decoded, &psk, &SoftwareAead, &SoftwareSha256);
        assert_eq!(result, Err(DecodeError::AuthenticationFailed));
    }

    #[test]
    fn aead_tampered_header() {
        let hdr = FrameHeader {
            key_hint: 1,
            msg_type: MSG_WAKE,
            nonce: 1,
        };
        let psk = [0x42u8; 32];
        let mut raw =
            encode_frame_aead(&hdr, &[0xA0], &psk, &SoftwareAead, &SoftwareSha256).unwrap();
        // Flip one bit in the header (msg_type byte) — header is AAD.
        raw[2] ^= 0x01;
        let decoded = decode_frame_aead(&raw).unwrap();
        let result = open_frame(&decoded, &psk, &SoftwareAead, &SoftwareSha256);
        assert_eq!(result, Err(DecodeError::AuthenticationFailed));
    }

    #[test]
    fn aead_tampered_tag() {
        let hdr = FrameHeader {
            key_hint: 1,
            msg_type: MSG_WAKE,
            nonce: 1,
        };
        let psk = [0x42u8; 32];
        let mut raw =
            encode_frame_aead(&hdr, &[0xA0], &psk, &SoftwareAead, &SoftwareSha256).unwrap();
        // Flip one bit in the GCM tag (last byte).
        let last = raw.len() - 1;
        raw[last] ^= 0x01;
        let decoded = decode_frame_aead(&raw).unwrap();
        let result = open_frame(&decoded, &psk, &SoftwareAead, &SoftwareSha256);
        assert_eq!(result, Err(DecodeError::AuthenticationFailed));
    }

    #[test]
    fn aead_nonce_construction() {
        let psk = [0x42u8; 32];
        let frame_nonce: [u8; 8] = [1, 2, 3, 4, 5, 6, 7, 8];
        let nonce = build_gcm_nonce(&psk, MSG_WAKE, &frame_nonce, &SoftwareSha256);
        assert_eq!(nonce.len(), 12);

        // First 3 bytes = SHA-256(psk)[0..3]
        let hash = SoftwareSha256.hash(&psk);
        assert_eq!(&nonce[0..3], &hash[0..3]);

        // Byte 3 = msg_type
        assert_eq!(nonce[3], MSG_WAKE);

        // Bytes 4..12 = frame_nonce
        assert_eq!(&nonce[4..12], &frame_nonce);
    }

    #[test]
    fn aead_payload_capacity() {
        assert_eq!(MAX_PAYLOAD_SIZE_AEAD, 223);
        assert_eq!(AEAD_TAG_SIZE, 16);
        assert_eq!(MIN_FRAME_SIZE_AEAD, HEADER_SIZE + AEAD_TAG_SIZE);
    }
}
