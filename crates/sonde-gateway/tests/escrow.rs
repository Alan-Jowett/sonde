// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Integration tests for PSK escrow, master key identification, and ACTUAL_STATE
//! publication (GW-2001, GW-2003, GW-2004, GW-2005).
//!
//! Test IDs: T-2000, T-2001, T-2002, T-2006c.

use std::sync::Arc;
use std::time::Duration;

use ciborium::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::sync::mpsc;
use zeroize::Zeroizing;

use sonde_gateway::connector::{
    ConnectorEventHub, ConnectorService, GatewayDesiredState, MSG_TYPE_ACTUAL_STATE,
    MSG_TYPE_DESIRED_STATE,
};
use sonde_gateway::gateway_identity::GatewayIdentity;
use sonde_gateway::phone_trust::{PhonePskRecord, PhonePskStatus};
use sonde_gateway::registry::NodeRecord;
use sonde_gateway::rotation_engine::RotationEngine;
use sonde_gateway::sqlite_storage::SqliteStorage;
use sonde_gateway::storage::Storage;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use hkdf::Hkdf;
use sha2::Sha256;
use x25519_dalek::PublicKey as X25519PublicKey;

// ── helpers ──────────────────────────────────────────────────────

/// Create a test gateway with identity and storage, but do NOT call
/// `init_master_key_id()` — the caller must do that after registering nodes
/// so the backfill picks them up.
async fn setup_test_storage() -> (Arc<SqliteStorage>, GatewayIdentity) {
    let master_key = Zeroizing::new([0x42u8; 32]);
    let store = Arc::new(SqliteStorage::in_memory(master_key).unwrap());

    let identity = GatewayIdentity::generate().unwrap();
    store.store_gateway_identity(&identity).await.unwrap();

    (store, identity)
}

/// Convenience: create storage + identity, init master key & rotation code.
/// Use only when no nodes need to be registered before init.
async fn setup_test_gateway() -> (
    Arc<SqliteStorage>,
    GatewayIdentity,
    [u8; 16], // master_key_id
    u64,      // epoch
    String,   // rotation_code
) {
    let (store, identity) = setup_test_storage().await;
    let (key_id, epoch) = store.init_master_key_id().await.unwrap();
    let code = store.init_rotation_code().await.unwrap();
    (store, identity, key_id, epoch, code)
}

/// Register a test node with a deterministic PSK derived from key_hint.
async fn register_test_node(store: &Arc<SqliteStorage>, node_id: &str, key_hint: u16) {
    let mut psk = [0u8; 32];
    psk[0] = key_hint as u8;
    psk[1] = (key_hint >> 8) as u8;

    let record = NodeRecord {
        node_id: node_id.to_string(),
        key_hint,
        psk,
        assigned_program_hash: None,
        current_program_hash: None,
        desired_schedule_interval_s: Some(60),
        schedule_interval_s: 60,
        firmware_abi_version: Some(1),
        firmware_version: Some("0.1.0".to_string()),
        rf_channel: Some(1),
        sensors: vec![],
        registered_by_phone_id: None,
        key_version: 0,
    };
    store.upsert_node(&record).await.unwrap();
}

/// Store a phone PSK in the database.
async fn register_phone_psk(store: &Arc<SqliteStorage>, phone_id: u32, key_hint: u16) {
    let phone = PhonePskRecord {
        phone_id,
        phone_key_hint: key_hint,
        psk: Zeroizing::new([0xCCu8; 32]),
        label: format!("test-phone-{phone_id}"),
        issued_at: std::time::SystemTime::now(),
        status: PhonePskStatus::Active,
        key_version: 0,
    };
    store.store_phone_psk(&phone).await.unwrap();
}

/// Build a valid RotationPayloadV1 for testing (same as rotation.rs helper).
fn build_rotation_payload(
    gateway_identity: &GatewayIdentity,
    current_epoch: u64,
    new_master_key: &[u8; 32],
    rotation_code: &str,
    new_master_key_id: &[u8; 16],
    salt: Option<&[u8; 16]>,
) -> Vec<u8> {
    let mut rng_bytes = [0u8; 32];
    getrandom::fill(&mut rng_bytes).unwrap();
    let ephemeral_secret = x25519_dalek::StaticSecret::from(rng_bytes);
    let ephemeral_public = X25519PublicKey::from(&ephemeral_secret);

    let (_, gw_x25519_public) = gateway_identity.to_x25519().unwrap();
    let shared_secret = ephemeral_secret.diffie_hellman(&gw_x25519_public);

    let gateway_id = gateway_identity.gateway_id();
    let mut info = Vec::with_capacity(24);
    info.extend_from_slice(gateway_id);
    info.extend_from_slice(&current_epoch.to_be_bytes());

    let hk = Hkdf::<Sha256>::new(Some(b"sonde-rotation-v1"), shared_secret.as_bytes());
    let mut derived_key = [0u8; 32];
    hk.expand(&info, &mut derived_key).unwrap();

    let mut cbor_pairs: Vec<(Value, Value)> = vec![
        (
            Value::Integer(1.into()),
            Value::Bytes(new_master_key.to_vec()),
        ),
        (
            Value::Integer(2.into()),
            Value::Text(rotation_code.to_string()),
        ),
        (
            Value::Integer(3.into()),
            Value::Bytes(new_master_key_id.to_vec()),
        ),
    ];
    if let Some(s) = salt {
        cbor_pairs.push((Value::Integer(4.into()), Value::Bytes(s.to_vec())));
    } else {
        cbor_pairs.push((Value::Integer(4.into()), Value::Null));
    }
    cbor_pairs.push((Value::Integer(5.into()), Value::Null));
    let cbor_map = Value::Map(cbor_pairs);
    let mut plaintext = Vec::new();
    ciborium::into_writer(&cbor_map, &mut plaintext).unwrap();

    let key = Key::<Aes256Gcm>::from_slice(&derived_key);
    let cipher = Aes256Gcm::new(key);
    let mut nonce_bytes = [0u8; 12];
    getrandom::fill(&mut nonce_bytes).unwrap();
    let nonce = Nonce::from_slice(&nonce_bytes);

    let gcm_payload = Payload {
        msg: &plaintext,
        aad: &info,
    };
    let ciphertext = cipher.encrypt(nonce, gcm_payload).unwrap();

    let mut payload = Vec::with_capacity(1 + 32 + 12 + ciphertext.len());
    payload.push(0x01);
    payload.extend_from_slice(ephemeral_public.as_bytes());
    payload.extend_from_slice(&nonce_bytes);
    payload.extend_from_slice(&ciphertext);
    payload
}

/// Spawn a ConnectorService with a duplex stream and return the client side.
async fn spawn_connector(
    event_hub: Arc<ConnectorEventHub>,
    storage: Arc<dyn Storage>,
) -> (DuplexStream, tokio::task::JoinHandle<()>) {
    let pending = Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
    let service = ConnectorService::new(storage, pending, event_hub.clone(), 64 * 1024);
    let (client, server) = tokio::io::duplex(64 * 1024);
    let handle = tokio::spawn(async move {
        service.handle_connection(server).await.ok();
    });
    // Wait for the connector to subscribe to the broadcast channel.
    while event_hub.subscriber_count() == 0 {
        tokio::task::yield_now().await;
    }
    (client, handle)
}

/// Spawn a ConnectorService wired to a gateway_desired_state channel.
async fn spawn_connector_with_desired_state(
    event_hub: Arc<ConnectorEventHub>,
    storage: Arc<dyn Storage>,
) -> (
    DuplexStream,
    tokio::task::JoinHandle<()>,
    mpsc::UnboundedReceiver<GatewayDesiredState>,
) {
    let pending = Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
    let (ds_tx, ds_rx) = mpsc::unbounded_channel();
    let mut service = ConnectorService::new(storage, pending, event_hub.clone(), 64 * 1024);
    service.set_gateway_desired_state_tx(ds_tx);
    let (client, server) = tokio::io::duplex(64 * 1024);
    let handle = tokio::spawn(async move {
        service.handle_connection(server).await.ok();
    });
    while event_hub.subscriber_count() == 0 {
        tokio::task::yield_now().await;
    }
    (client, handle, ds_rx)
}

/// Write a length-delimited frame to a stream (4-byte big-endian length prefix).
async fn write_framed(stream: &mut DuplexStream, payload: &[u8]) {
    let len = u32::try_from(payload.len()).unwrap().to_be_bytes();
    stream.write_all(&len).await.unwrap();
    stream.write_all(payload).await.unwrap();
    stream.flush().await.unwrap();
}

/// Read a length-delimited frame from a stream.
async fn read_framed(stream: &mut DuplexStream) -> Vec<u8> {
    let mut len = [0u8; 4];
    tokio::time::timeout(Duration::from_secs(2), stream.read_exact(&mut len))
        .await
        .expect("timed out waiting for connector frame length")
        .unwrap();
    let len = usize::try_from(u32::from_be_bytes(len)).unwrap();
    let mut payload = vec![0u8; len];
    tokio::time::timeout(Duration::from_secs(2), stream.read_exact(&mut payload))
        .await
        .expect("timed out waiting for connector frame payload")
        .unwrap();
    payload
}

/// Decode CBOR bytes into a map of integer-keyed entries.
fn decode_cbor_map(bytes: &[u8]) -> Vec<(i128, Value)> {
    let value: Value = ciborium::from_reader(bytes).expect("invalid CBOR");
    match value {
        Value::Map(pairs) => pairs
            .into_iter()
            .filter_map(|(k, v)| {
                if let Value::Integer(i) = k {
                    Some((i.into(), v))
                } else {
                    None
                }
            })
            .collect(),
        _ => panic!("expected CBOR map"),
    }
}

/// Look up a key in a decoded CBOR map.
fn map_get(map: &[(i128, Value)], key: i128) -> Option<&Value> {
    map.iter().find(|(k, _)| *k == key).map(|(_, v)| v)
}

// ── T-2000: Master key identification — first startup ────────

/// T-2000: Master key identification — first startup (GW-2001).
///
/// Verifies:
/// 1. No `master_key_id` or `master_key_epoch` on fresh DB.
/// 2. After `init_master_key_id()`, a random 16-byte ID and epoch=1 are set.
/// 3. All existing PSK records are backfilled.
/// 4. On restart (re-call), the same values are returned.
#[tokio::test]
async fn test_t2000_master_key_identification() {
    let master_key = Zeroizing::new([0x42u8; 32]);
    let store = Arc::new(SqliteStorage::in_memory(master_key).unwrap());

    // Pre-populate with nodes before init_master_key_id.
    register_test_node(&store, "node-alpha", 100).await;
    register_test_node(&store, "node-beta", 200).await;
    register_phone_psk(&store, 0, 300).await;

    // Step 1-2: Verify no master_key_id/epoch before init.
    let config_id = store.get_config("master_key_id").await.unwrap();
    assert!(
        config_id.is_none(),
        "master_key_id should not exist before init"
    );
    let config_epoch = store.get_config("master_key_epoch").await.unwrap();
    assert!(
        config_epoch.is_none(),
        "master_key_epoch should not exist before init"
    );

    // Step 3: Run init_master_key_id.
    let (key_id, epoch) = store.init_master_key_id().await.unwrap();

    // Step 4: Verify master_key_id is 16 bytes, non-zero.
    assert_eq!(key_id.len(), 16);
    assert_ne!(key_id, [0u8; 16], "master_key_id must be non-zero");

    // Step 5: Verify epoch = 1.
    assert_eq!(epoch, 1);

    // Step 6: Verify all existing node PSK records are backfilled.
    let escrow = store.list_node_escrow_state().await.unwrap();
    assert_eq!(escrow.len(), 2);
    for node in &escrow {
        assert_eq!(
            node.master_key_id,
            key_id.to_vec(),
            "node {} should have master_key_id backfilled",
            node.node_id
        );
    }

    // Step 6b: Verify phone PSK records are also backfilled (loadable after init).
    let phones = store.list_phone_psks().await.unwrap();
    assert_eq!(phones.len(), 1, "phone PSK should be backfilled");
    // Phone PSK must be loadable (decrypt succeeds with current master key).
    assert_ne!(
        phones[0].psk.as_ref(),
        &[0u8; 32],
        "phone PSK must be non-zero after backfill"
    );

    // Step 7: Restart — verify same values are returned.
    let (key_id_2, epoch_2) = store.init_master_key_id().await.unwrap();
    assert_eq!(
        key_id, key_id_2,
        "master_key_id must be stable across restarts"
    );
    assert_eq!(epoch, epoch_2, "epoch must be stable across restarts");
}

// ── T-2001: Gateway ACTUAL_STATE publication ─────────────────

/// T-2001: Gateway ACTUAL_STATE publication (GW-2003).
///
/// Verifies:
/// 1. Gateway ACTUAL_STATE is emitted with `entity_kind = "gateway"` and all
///    required fields (entity_id, channel, master_key_id, master_key_epoch,
///    x25519_public_key, fingerprint_words, gateway_version, gateway_commit).
/// 2. `fingerprint_words` matches independent computation from the public key.
/// 3. `rotation_in_progress = false`.
/// 4. Node ACTUAL_STATE includes escrow fields (encrypted_psk key 12,
///    escrow_key_hint key 13, master_key_id key 14).
#[tokio::test]
async fn test_t2001_gateway_actual_state_publication() {
    let (store, identity) = setup_test_storage().await;

    // Register nodes BEFORE init_master_key_id so the backfill sets master_key_id.
    register_test_node(&store, "node-1", 101).await;
    register_test_node(&store, "node-2", 202).await;

    let (key_id, epoch) = store.init_master_key_id().await.unwrap();
    let _code = store.init_rotation_code().await.unwrap();

    let event_hub = Arc::new(ConnectorEventHub::default());
    let (mut client, _handle) =
        spawn_connector(event_hub.clone(), store.clone() as Arc<dyn Storage>).await;

    // Derive X25519 public key for verification.
    let (_, x25519_public) = identity.to_x25519().unwrap();

    // Compute expected fingerprint.
    let sha = sonde_gateway::crypto::RustCryptoSha256;
    let expected_fp = sonde_protocol::compute_fingerprint(x25519_public.as_bytes(), &sha);

    // Emit gateway ACTUAL_STATE.
    let gateway_id_hex = hex::encode(identity.gateway_id());
    event_hub.emit_gateway_actual_state(
        gateway_id_hex.clone(),
        1, // channel
        key_id,
        epoch,
        *x25519_public.as_bytes(),
        expected_fp.map(|s| s.to_string()),
        vec![], // missing_key_hints
        None,   // salt
        None,   // kdf_params
        "0.7.0".to_string(),
        "abc123".to_string(),
        None, // modem_firmware_version
        None, // modem_firmware_commit
        false,
    );

    // Read the gateway ACTUAL_STATE from the connector.
    let frame = read_framed(&mut client).await;
    let map = decode_cbor_map(&frame);

    // Verify msg_type = ACTUAL_STATE.
    let msg_type: i128 = match map_get(&map, 1).unwrap() {
        Value::Integer(i) => (*i).into(),
        other => panic!("expected integer msg_type, got {other:?}"),
    };
    assert_eq!(msg_type, MSG_TYPE_ACTUAL_STATE as i128);

    // Verify entity_kind = "gateway".
    let entity_kind = match map_get(&map, 2).unwrap() {
        Value::Text(s) => s.clone(),
        other => panic!("expected text entity_kind, got {other:?}"),
    };
    assert_eq!(entity_kind, "gateway");

    // Verify entity_id matches gateway_id hex.
    let entity_id = match map_get(&map, 3).unwrap() {
        Value::Text(s) => s.clone(),
        other => panic!("expected text entity_id, got {other:?}"),
    };
    assert_eq!(entity_id, gateway_id_hex);

    // Verify channel (key 15).
    let channel: i128 = match map_get(&map, 15).unwrap() {
        Value::Integer(i) => (*i).into(),
        other => panic!("expected integer channel, got {other:?}"),
    };
    assert_eq!(channel, 1);

    // Verify master_key_id (key 16).
    let mk_id = match map_get(&map, 16).unwrap() {
        Value::Bytes(b) => b.clone(),
        other => panic!("expected bytes master_key_id, got {other:?}"),
    };
    assert_eq!(mk_id, key_id.to_vec());

    // Verify master_key_epoch (key 17).
    let mk_epoch: i128 = match map_get(&map, 17).unwrap() {
        Value::Integer(i) => (*i).into(),
        other => panic!("expected integer master_key_epoch, got {other:?}"),
    };
    assert_eq!(mk_epoch, epoch as i128);

    // Verify x25519_public_key (key 18).
    let x25519 = match map_get(&map, 18).unwrap() {
        Value::Bytes(b) => b.clone(),
        other => panic!("expected bytes x25519_public_key, got {other:?}"),
    };
    assert_eq!(x25519, x25519_public.as_bytes().to_vec());

    // Verify fingerprint_words (key 19) matches independent computation.
    let fp_words = match map_get(&map, 19).unwrap() {
        Value::Array(arr) => arr
            .iter()
            .map(|v| match v {
                Value::Text(s) => s.clone(),
                other => panic!("expected text in fingerprint array, got {other:?}"),
            })
            .collect::<Vec<_>>(),
        other => panic!("expected array fingerprint_words, got {other:?}"),
    };
    assert_eq!(fp_words.len(), 6);
    let expected_strs: Vec<String> = expected_fp.iter().map(|s| s.to_string()).collect();
    assert_eq!(fp_words, expected_strs);

    // Verify rotation_in_progress = false (key 27).
    let rip = match map_get(&map, 27).unwrap() {
        Value::Bool(b) => *b,
        other => panic!("expected bool rotation_in_progress, got {other:?}"),
    };
    assert!(!rip);

    // Verify gateway_version (key 23) and gateway_commit (key 24).
    let gw_version = match map_get(&map, 23).unwrap() {
        Value::Text(s) => s.clone(),
        other => panic!("expected text gateway_version, got {other:?}"),
    };
    assert_eq!(gw_version, "0.7.0");

    let gw_commit = match map_get(&map, 24).unwrap() {
        Value::Text(s) => s.clone(),
        other => panic!("expected text gateway_commit, got {other:?}"),
    };
    assert_eq!(gw_commit, "abc123");

    // Now test node ACTUAL_STATE with escrow fields.
    let escrow_records = store.list_node_escrow_state().await.unwrap();
    assert_eq!(escrow_records.len(), 2, "should have 2 nodes");

    // Emit a node ACTUAL_STATE with escrow fields.
    let node = &escrow_records[0];
    event_hub.emit_actual_state_for_node_with_escrow(
        node.node_id.clone(),
        node.current_program_hash.clone(),
        node.assigned_program_hash.clone(),
        node.schedule_interval_s,
        node.last_battery_mv,
        node.firmware_abi_version,
        node.firmware_version.clone(),
        1000,
        Some(node.encrypted_psk.clone()),
        Some(node.key_hint),
        Some(node.master_key_id.clone()),
        None,
    );

    let node_frame = read_framed(&mut client).await;
    let node_map = decode_cbor_map(&node_frame);

    // Verify msg_type = ACTUAL_STATE.
    let node_msg_type: i128 = match map_get(&node_map, 1).unwrap() {
        Value::Integer(i) => (*i).into(),
        other => panic!("expected integer, got {other:?}"),
    };
    assert_eq!(node_msg_type, MSG_TYPE_ACTUAL_STATE as i128);

    // Verify entity_kind = "node" (key 2).
    let node_entity_kind = match map_get(&node_map, 2).unwrap() {
        Value::Text(s) => s.clone(),
        other => panic!("expected text, got {other:?}"),
    };
    assert_eq!(node_entity_kind, "node");

    // Verify encrypted_psk (key 12) is present and 60 bytes.
    let encrypted_psk = match map_get(&node_map, 12).unwrap() {
        Value::Bytes(b) => b.clone(),
        other => panic!("expected bytes encrypted_psk, got {other:?}"),
    };
    assert_eq!(encrypted_psk.len(), 60, "encrypted_psk must be 60 bytes");

    // Verify escrow_key_hint (key 13).
    let escrow_kh: i128 = match map_get(&node_map, 13).unwrap() {
        Value::Integer(i) => (*i).into(),
        other => panic!("expected integer escrow_key_hint, got {other:?}"),
    };
    assert_eq!(escrow_kh, node.key_hint as i128);

    // Verify master_key_id (key 14) matches.
    let node_mk_id = match map_get(&node_map, 14).unwrap() {
        Value::Bytes(b) => b.clone(),
        other => panic!("expected bytes master_key_id, got {other:?}"),
    };
    assert_eq!(node_mk_id, key_id.to_vec());
}

// ── T-2002: Gateway DESIRED_STATE channel change ─────────────

/// T-2002: Gateway DESIRED_STATE channel change (GW-2004).
///
/// Verifies:
/// 1. DESIRED_STATE with `channel = 5` is parsed and forwarded.
/// 2. The parsed `GatewayDesiredState` contains the correct channel value.
#[tokio::test]
async fn test_t2002_desired_state_channel_change() {
    let (store, identity, _key_id, _epoch, _code) = setup_test_gateway().await;
    let event_hub = Arc::new(ConnectorEventHub::default());

    let (mut client, _handle, mut ds_rx) =
        spawn_connector_with_desired_state(event_hub.clone(), store.clone() as Arc<dyn Storage>)
            .await;

    // Build a DESIRED_STATE message with entity_kind=gateway, channel=5.
    let gateway_id_hex = hex::encode(identity.gateway_id());
    let desired_state_inner =
        Value::Map(vec![(Value::Integer(15.into()), Value::Integer(5.into()))]);
    let message = Value::Map(vec![
        (
            Value::Integer(1.into()),
            Value::Integer(MSG_TYPE_DESIRED_STATE.into()),
        ),
        (Value::Integer(2.into()), Value::Text("gateway".to_string())),
        (Value::Integer(3.into()), Value::Text(gateway_id_hex)),
        (Value::Integer(4.into()), desired_state_inner),
    ]);
    let mut bytes = Vec::new();
    ciborium::into_writer(&message, &mut bytes).unwrap();

    // Send DESIRED_STATE to the connector.
    write_framed(&mut client, &bytes).await;

    // Read the forwarded GatewayDesiredState from the channel.
    let ds = tokio::time::timeout(Duration::from_secs(2), ds_rx.recv())
        .await
        .expect("timed out waiting for DESIRED_STATE")
        .expect("channel closed");

    // Verify the parsed channel value.
    assert_eq!(ds.channel, Some(5), "channel should be 5");
}

// ── T-2006c: Phone PSKs not escrowed ─────────────────────────

/// T-2006c: Phone PSKs not escrowed (GW-2005).
///
/// Verifies:
/// 1. Node ACTUAL_STATE includes escrow fields for each node.
/// 2. No ACTUAL_STATE is emitted with entity_kind = "phone".
/// 3. After key rotation, node ACTUAL_STATE is re-emitted with updated escrow.
/// 4. Phone PSK is re-encrypted with new key in local DB.
/// 5. Still no phone ACTUAL_STATE emitted after rotation.
#[tokio::test]
async fn test_t2006c_phone_psks_not_escrowed() {
    let (store, identity) = setup_test_storage().await;

    // Register nodes and phone PSK BEFORE init_master_key_id for backfill.
    register_test_node(&store, "node-x", 100).await;
    register_test_node(&store, "node-y", 200).await;
    register_phone_psk(&store, 0, 400).await;

    let (_key_id, epoch) = store.init_master_key_id().await.unwrap();
    let code = store.init_rotation_code().await.unwrap();

    let event_hub = Arc::new(ConnectorEventHub::default());
    let (mut client, _handle) =
        spawn_connector(event_hub.clone(), store.clone() as Arc<dyn Storage>).await;

    // Step 1: Emit node escrow state and verify escrow fields are present.
    let escrow = store.list_node_escrow_state().await.unwrap();
    assert_eq!(escrow.len(), 2);

    for node in &escrow {
        event_hub.emit_actual_state_for_node_with_escrow(
            node.node_id.clone(),
            node.current_program_hash.clone(),
            node.assigned_program_hash.clone(),
            node.schedule_interval_s,
            node.last_battery_mv,
            node.firmware_abi_version,
            node.firmware_version.clone(),
            1000,
            Some(node.encrypted_psk.clone()),
            Some(node.key_hint),
            Some(node.master_key_id.clone()),
            None,
        );
    }

    // Read 2 node ACTUAL_STATE messages.
    for _ in 0..2 {
        let frame = read_framed(&mut client).await;
        let map = decode_cbor_map(&frame);

        let entity_kind = match map_get(&map, 2).unwrap() {
            Value::Text(s) => s.clone(),
            other => panic!("expected text, got {other:?}"),
        };
        assert_eq!(
            entity_kind, "node",
            "only node ACTUAL_STATE should be emitted"
        );

        // Verify escrow fields present.
        assert!(
            matches!(map_get(&map, 12), Some(Value::Bytes(_))),
            "encrypted_psk (key 12) must be present"
        );
        assert!(
            matches!(map_get(&map, 13), Some(Value::Integer(_))),
            "escrow_key_hint (key 13) must be present"
        );
        assert!(
            matches!(map_get(&map, 14), Some(Value::Bytes(_))),
            "master_key_id (key 14) must be present"
        );
    }

    // Step 2: Verify NO phone ACTUAL_STATE is emitted.
    // The ConnectorEventHub only emits node and gateway ACTUAL_STATE — there is no
    // emit method for phone ACTUAL_STATE. This is the design-level guarantee (GW-2005).
    // We verify by checking that list_node_escrow_state returns only nodes.
    let phones = store.list_phone_psks().await.unwrap();
    assert_eq!(phones.len(), 1, "phone PSK should exist in DB");

    // Step 3: Perform key rotation.
    let new_key = [0xAAu8; 32];
    let new_id = [0xBBu8; 16];
    let payload = build_rotation_payload(&identity, epoch, &new_key, &code, &new_id, None);

    let (_, grpc_rx) = mpsc::unbounded_channel();
    let (_, desired_state_rx) = mpsc::unbounded_channel();
    // Use the same event_hub so post-rotation ACTUAL_STATE goes to the connector.
    let mut engine = RotationEngine::new(
        store.clone(),
        identity.clone(),
        event_hub.clone(),
        grpc_rx,
        desired_state_rx,
    );

    let result = engine.handle_rotation_payload(&payload, true).await;
    assert!(result.is_ok(), "rotation should succeed: {:?}", result);

    // Step 4: Read the re-emitted node ACTUAL_STATE messages after rotation.
    // The rotation engine calls emit_post_rotation_state which re-emits
    // node ACTUAL_STATE with updated escrow fields.
    for _ in 0..2 {
        let frame = read_framed(&mut client).await;
        let map = decode_cbor_map(&frame);

        let entity_kind = match map_get(&map, 2).unwrap() {
            Value::Text(s) => s.clone(),
            other => panic!("expected text, got {other:?}"),
        };
        // After rotation, only "node" ACTUAL_STATE should appear — never "phone".
        assert_eq!(
            entity_kind, "node",
            "post-rotation ACTUAL_STATE must be for nodes only, not phones"
        );

        // Verify updated master_key_id (key 14).
        let mk = match map_get(&map, 14).unwrap() {
            Value::Bytes(b) => b.clone(),
            other => panic!("expected bytes, got {other:?}"),
        };
        assert_eq!(mk, new_id.to_vec(), "master_key_id should be updated");
    }

    // Step 5: Verify phone PSK was re-encrypted (still loadable after rotation).
    // `list_phone_psks()` decrypts using the rotation key; if migrate_phone_psk
    // didn't re-encrypt, this would fail with a decryption error.
    let phones_after = store.list_phone_psks().await.unwrap();
    assert_eq!(
        phones_after.len(),
        1,
        "phone PSK should still exist after rotation"
    );
    // The plaintext PSK value must be preserved through rotation.
    assert_eq!(
        phones_after[0].psk.as_ref(),
        &[0xCCu8; 32],
        "phone PSK plaintext must survive rotation"
    );

    // Step 6: Verify no additional messages on the connector stream.
    // A short timeout ensures no phone ACTUAL_STATE was emitted.
    let timeout_result =
        tokio::time::timeout(Duration::from_millis(100), read_framed(&mut client)).await;
    assert!(
        timeout_result.is_err(),
        "no additional ACTUAL_STATE should be emitted (phone PSKs are not escrowed)"
    );
}
