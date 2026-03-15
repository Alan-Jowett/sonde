// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Protocol crate validation tests (T-P001 … T-P062).
//!
//! All 41 test cases from `docs/protocol-crate-validation.md`.

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
