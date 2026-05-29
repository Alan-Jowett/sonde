// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Integration tests for PSK escrow, master key identification, and ACTUAL_STATE
//! publication (GW-2001, GW-2003, GW-2004, GW-2005).
//!
//! Test IDs: T-2000, T-2001, T-2002, T-2006c, T-2011, T-2012, T-2013.

use std::sync::Arc;
use std::time::Duration;

use ciborium::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::sync::mpsc;
use zeroize::Zeroizing;

use sonde_gateway::connector::{
    ConnectorEventHub, ConnectorService, GatewayDesiredState, KdfParams, MSG_TYPE_ACTUAL_STATE,
    MSG_TYPE_DESIRED_STATE,
};
use sonde_gateway::engine::Gateway;
use sonde_gateway::gateway_identity::GatewayIdentity;
use sonde_gateway::phone_trust::{PhonePskRecord, PhonePskStatus};
use sonde_gateway::registry::NodeRecord;
use sonde_gateway::rotation_engine::RotationEngine;
use sonde_gateway::sqlite_storage::SqliteStorage;
use sonde_gateway::storage::Storage;
use sonde_gateway::RustCryptoSha256;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use hkdf::Hkdf;
use sha2::Sha256;
use x25519_dalek::PublicKey as X25519PublicKey;

// ── helpers ──────────────────────────────────────────────────────

/// A writable key provider for tests that don't need actual key persistence.
struct NoopWritableKeyProvider;

impl sonde_gateway::key_provider::KeyProvider for NoopWritableKeyProvider {
    fn load_master_key(
        &self,
    ) -> Result<Zeroizing<[u8; 32]>, sonde_gateway::key_provider::KeyProviderError> {
        Err(sonde_gateway::key_provider::KeyProviderError::NotFound(
            "test provider".into(),
        ))
    }

    fn write_master_key(
        &self,
        _key: &[u8; 32],
    ) -> Result<(), sonde_gateway::key_provider::KeyProviderError> {
        Ok(())
    }

    fn is_writable(&self) -> bool {
        true
    }
}

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
        Arc::new(NoopWritableKeyProvider),
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

// ── Helper: emit gateway ACTUAL_STATE from storage (mirrors binary) ──

/// Emit a gateway ACTUAL_STATE from the given storage + identity, mirroring the
/// logic in the gateway binary's `emit_gateway_actual_state_from_storage`.
async fn emit_gateway_actual_state_from_storage(
    event_hub: &ConnectorEventHub,
    storage: &SqliteStorage,
    identity: &GatewayIdentity,
    gateway: &Gateway,
) {
    let entity_id: String = identity
        .gateway_id()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();

    let (mk_id, mk_epoch) = storage.init_master_key_id().await.unwrap();

    let (_, x25519_public) = identity.to_x25519().unwrap();

    let sha = RustCryptoSha256;
    let fp = sonde_protocol::fingerprint::compute_fingerprint(x25519_public.as_bytes(), &sha);
    let fingerprint_words: [String; 6] = fp.map(|w| w.to_string());

    let salt: Option<Vec<u8>> = storage.get_config("kdf_salt").await.unwrap().and_then(|s| {
        let bytes = hex::decode(&s).ok()?;
        if bytes.len() == 16 {
            Some(bytes)
        } else {
            None
        }
    });

    let kdf_params: Option<KdfParams> = storage
        .get_config("kdf_params_json")
        .await
        .unwrap()
        .and_then(|json| {
            #[derive(serde::Deserialize)]
            struct Kdf {
                m_cost: u32,
                t_cost: u32,
                p_cost: u32,
                kdf_version: u32,
            }
            let p: Kdf = serde_json::from_str(&json).ok()?;
            Some(KdfParams {
                m_cost: p.m_cost,
                t_cost: p.t_cost,
                p_cost: p.p_cost,
                kdf_version: p.kdf_version,
            })
        });

    let missing_key_hints = gateway.drain_missing_hints().await;

    event_hub.emit_gateway_actual_state(
        entity_id,
        1, // channel
        mk_id,
        mk_epoch,
        *x25519_public.as_bytes(),
        fingerprint_words,
        missing_key_hints,
        salt,
        kdf_params,
        "0.7.0".to_string(),
        "test".to_string(),
        None,
        None,
        false,
    );
}

// ── T-2012: Rotation complete triggers ACTUAL_STATE re-emission ──

/// T-2012: Rotation complete triggers ACTUAL_STATE re-emission (GW-2003 AC-5).
///
/// Verifies:
/// 1. After rotation completes, gateway ACTUAL_STATE is re-emitted.
/// 2. The re-emitted ACTUAL_STATE has updated `master_key_id` and `master_key_epoch`.
#[tokio::test]
async fn test_t2012_rotation_complete_triggers_reemission() {
    let (store, identity) = setup_test_storage().await;
    let (initial_key_id, initial_epoch) = store.init_master_key_id().await.unwrap();
    let rotation_code = store.init_rotation_code().await.unwrap();

    let event_hub = Arc::new(ConnectorEventHub::new(16));
    let gateway = Arc::new(Gateway::new(
        store.clone() as Arc<dyn Storage>,
        Duration::from_secs(300),
    ));

    // Spawn connector and read the initial ACTUAL_STATE we emit.
    let (mut client, _handle) =
        spawn_connector(event_hub.clone(), store.clone() as Arc<dyn Storage>).await;

    emit_gateway_actual_state_from_storage(&event_hub, &store, &identity, &gateway).await;
    let initial_frame = read_framed(&mut client).await;
    let initial_map = decode_cbor_map(&initial_frame);
    let initial_mk_id = match map_get(&initial_map, 16).unwrap() {
        Value::Bytes(b) => b.clone(),
        other => panic!("expected bytes master_key_id, got {other:?}"),
    };
    assert_eq!(initial_mk_id, initial_key_id.to_vec());
    let initial_mk_epoch: i128 = match map_get(&initial_map, 17).unwrap() {
        Value::Integer(i) => (*i).into(),
        other => panic!("expected integer master_key_epoch, got {other:?}"),
    };
    assert_eq!(initial_mk_epoch, initial_epoch as i128);

    // Set up rotation engine with rotation_complete_tx.
    let (rotation_complete_tx, mut rotation_complete_rx) = mpsc::unbounded_channel();
    let (grpc_tx, grpc_rx) = mpsc::unbounded_channel();
    let (ds_tx, ds_rx) = mpsc::unbounded_channel::<GatewayDesiredState>();
    let _ = ds_tx; // not used in this test

    let engine = RotationEngine::new(
        store.clone(),
        identity.clone(),
        event_hub.clone(),
        Arc::new(NoopWritableKeyProvider),
        grpc_rx,
        ds_rx,
    )
    .with_rotation_complete_tx(rotation_complete_tx);

    // Spawn the rotation engine.
    tokio::spawn(engine.run());

    // Spawn re-emission loop (mirrors the binary's tokio::select! logic).
    let reemit_store = store.clone();
    let reemit_hub = event_hub.clone();
    let reemit_gw = Arc::clone(&gateway);
    tokio::spawn(async move {
        while let Some(_notification) = rotation_complete_rx.recv().await {
            let id = reemit_store.load_gateway_identity().await.unwrap().unwrap();
            emit_gateway_actual_state_from_storage(&reemit_hub, &reemit_store, &id, &reemit_gw)
                .await;
        }
    });

    // Submit a valid rotation payload via gRPC channel.
    let new_master_key = [0x99u8; 32];
    let new_master_key_id = [0xBBu8; 16];
    let payload = build_rotation_payload(
        &identity,
        initial_epoch,
        &new_master_key,
        &rotation_code,
        &new_master_key_id,
        None,
    );

    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    grpc_tx.send((payload, reply_tx)).unwrap();

    // Wait for rotation to complete.
    let result = tokio::time::timeout(Duration::from_secs(5), reply_rx)
        .await
        .expect("rotation should complete within 5s")
        .unwrap();
    assert!(result.is_ok(), "rotation should succeed: {result:?}");

    // Read the re-emitted ACTUAL_STATE.
    let reemitted_frame = tokio::time::timeout(Duration::from_secs(2), read_framed(&mut client))
        .await
        .expect("ACTUAL_STATE should be re-emitted after rotation");
    let reemitted_map = decode_cbor_map(&reemitted_frame);

    // Verify master_key_id and master_key_epoch differ from initial values.
    let new_mk_id = match map_get(&reemitted_map, 16).unwrap() {
        Value::Bytes(b) => b.clone(),
        other => panic!("expected bytes master_key_id, got {other:?}"),
    };
    assert_eq!(
        new_mk_id,
        new_master_key_id.to_vec(),
        "master_key_id should be updated after rotation"
    );

    let new_mk_epoch: i128 = match map_get(&reemitted_map, 17).unwrap() {
        Value::Integer(i) => (*i).into(),
        other => panic!("expected integer master_key_epoch, got {other:?}"),
    };
    assert_eq!(
        new_mk_epoch,
        (initial_epoch + 1) as i128,
        "master_key_epoch should increment after rotation"
    );
}

// ── T-2013: Salt/KDF adoption triggers ACTUAL_STATE re-emission ──

/// T-2013: Salt/KDF adoption triggers ACTUAL_STATE re-emission (GW-2003 AC-7).
///
/// Verifies:
/// 1. Delivering a gateway DESIRED_STATE with salt and kdf_params triggers
///    ACTUAL_STATE re-emission with the adopted values.
/// 2. Delivering a second DESIRED_STATE with a different salt does NOT trigger
///    another re-emission (salt is immutable once set).
#[tokio::test]
async fn test_t2013_salt_kdf_adoption_triggers_reemission() {
    let (store, identity) = setup_test_storage().await;
    let (_key_id, _epoch) = store.init_master_key_id().await.unwrap();
    let _code = store.init_rotation_code().await.unwrap();

    let event_hub = Arc::new(ConnectorEventHub::new(16));
    let gateway = Arc::new(Gateway::new(
        store.clone() as Arc<dyn Storage>,
        Duration::from_secs(300),
    ));

    // Spawn connector.
    let (mut client, _handle) =
        spawn_connector(event_hub.clone(), store.clone() as Arc<dyn Storage>).await;

    // Emit initial ACTUAL_STATE (salt = null).
    emit_gateway_actual_state_from_storage(&event_hub, &store, &identity, &gateway).await;
    let initial_frame = read_framed(&mut client).await;
    let initial_map = decode_cbor_map(&initial_frame);
    // Salt (key 21) should be null or absent.
    let initial_salt = map_get(&initial_map, 21);
    assert!(
        initial_salt.is_none() || matches!(initial_salt, Some(Value::Null)),
        "initial salt should be absent or null"
    );
    // KDF params (key 22) should be null or absent.
    let initial_kdf = map_get(&initial_map, 22);
    assert!(
        initial_kdf.is_none() || matches!(initial_kdf, Some(Value::Null)),
        "initial kdf_params should be absent or null"
    );

    // Set up rotation engine with state_changed_tx.
    let (state_changed_tx, mut state_changed_rx) = mpsc::unbounded_channel();
    let (_grpc_tx, grpc_rx) = mpsc::unbounded_channel();
    let (ds_tx, ds_rx) = mpsc::unbounded_channel::<GatewayDesiredState>();

    let engine = RotationEngine::new(
        store.clone(),
        identity.clone(),
        event_hub.clone(),
        Arc::new(NoopWritableKeyProvider),
        grpc_rx,
        ds_rx,
    )
    .with_state_changed_tx(state_changed_tx);

    // Spawn the rotation engine.
    tokio::spawn(engine.run());

    // Spawn re-emission loop for state_changed notifications.
    let reemit_store = store.clone();
    let reemit_hub = event_hub.clone();
    let reemit_gw = Arc::clone(&gateway);
    tokio::spawn(async move {
        while let Some(_changed) = state_changed_rx.recv().await {
            let id = reemit_store.load_gateway_identity().await.unwrap().unwrap();
            emit_gateway_actual_state_from_storage(&reemit_hub, &reemit_store, &id, &reemit_gw)
                .await;
        }
    });

    // Deliver a DESIRED_STATE with salt and kdf_params via the ds channel.
    let gateway_id_hex: String = identity
        .gateway_id()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let test_salt = [0xAA; 16];
    ds_tx
        .send(GatewayDesiredState {
            entity_id: gateway_id_hex.clone(),
            channel: None,
            rotation_payload: None,
            recovered_psks: None,
            salt: Some(test_salt.to_vec()),
            kdf_params: Some(KdfParams {
                m_cost: 65536,
                t_cost: 3,
                p_cost: 1,
                kdf_version: 0x13,
            }),
        })
        .unwrap();

    // Read the re-emitted ACTUAL_STATE.
    let reemitted_frame = tokio::time::timeout(Duration::from_secs(2), read_framed(&mut client))
        .await
        .expect("ACTUAL_STATE should be re-emitted after salt adoption");
    let reemitted_map = decode_cbor_map(&reemitted_frame);

    // Verify salt (key 21) matches the adopted value.
    let adopted_salt = match map_get(&reemitted_map, 21).unwrap() {
        Value::Bytes(b) => b.clone(),
        other => panic!("expected bytes salt, got {other:?}"),
    };
    assert_eq!(adopted_salt, test_salt.to_vec());

    // Verify kdf_params (key 22) matches the adopted values.
    let kdf_map = match map_get(&reemitted_map, 22).unwrap() {
        Value::Map(pairs) => pairs.clone(),
        other => panic!("expected map kdf_params, got {other:?}"),
    };
    let kdf_get = |key: i128| -> i128 {
        kdf_map
            .iter()
            .find(|(k, _)| matches!(k, Value::Integer(i) if i128::from(*i) == key))
            .map(|(_, v)| match v {
                Value::Integer(i) => (*i).into(),
                other => panic!("expected integer in kdf_params, got {other:?}"),
            })
            .unwrap()
    };
    assert_eq!(kdf_get(1), 65536, "m_cost");
    assert_eq!(kdf_get(2), 3, "t_cost");
    assert_eq!(kdf_get(3), 1, "p_cost");
    assert_eq!(kdf_get(4), 0x13, "kdf_version");

    // Step 5: Deliver another DESIRED_STATE with different salt.
    let different_salt = [0xBB; 16];
    ds_tx
        .send(GatewayDesiredState {
            entity_id: gateway_id_hex.clone(),
            channel: None,
            rotation_payload: None,
            recovered_psks: None,
            salt: Some(different_salt.to_vec()),
            kdf_params: None,
        })
        .unwrap();

    // Step 6: Verify ACTUAL_STATE is NOT re-emitted (salt is immutable).
    let timeout_result =
        tokio::time::timeout(Duration::from_millis(500), read_framed(&mut client)).await;
    assert!(
        timeout_result.is_err(),
        "ACTUAL_STATE should NOT be re-emitted when salt is already set"
    );
}

// ── T-2011: Missing key hint triggers debounced ACTUAL_STATE re-emission ──

/// T-2011: Missing key hint triggers debounced ACTUAL_STATE re-emission
/// (GW-2003 AC-6).
///
/// Verifies:
/// 1. Sending frames with unknown key_hints does not trigger immediate
///    ACTUAL_STATE re-emission.
/// 2. After the debounce timer elapses, a single ACTUAL_STATE is emitted
///    containing all accumulated missing hints.
#[tokio::test]
async fn test_t2011_missing_key_hint_debounced_reemission() {
    let (store, identity) = setup_test_storage().await;
    let (_key_id, _epoch) = store.init_master_key_id().await.unwrap();
    let _code = store.init_rotation_code().await.unwrap();

    let event_hub = Arc::new(ConnectorEventHub::new(16));
    let gateway = Arc::new(Gateway::new(
        store.clone() as Arc<dyn Storage>,
        Duration::from_secs(300),
    ));

    // Spawn connector.
    let (mut client, _handle) =
        spawn_connector(event_hub.clone(), store.clone() as Arc<dyn Storage>).await;

    // Emit initial ACTUAL_STATE.
    emit_gateway_actual_state_from_storage(&event_hub, &store, &identity, &gateway).await;
    let _initial_frame = read_framed(&mut client).await;

    // Spawn re-emission loop with a SHORT debounce for testing (100ms instead of 60s).
    let missing_hints_notify = gateway.missing_hints_notify();
    let reemit_store = store.clone();
    let reemit_hub = event_hub.clone();
    let reemit_gw = Arc::clone(&gateway);
    tokio::spawn(async move {
        const TEST_HINT_DEBOUNCE: Duration = Duration::from_millis(100);
        let mut hint_debounce: Option<std::pin::Pin<Box<tokio::time::Sleep>>> = None;
        loop {
            tokio::select! {
                biased;
                _ = missing_hints_notify.notified() => {
                    hint_debounce = Some(Box::pin(tokio::time::sleep(TEST_HINT_DEBOUNCE)));
                }
                _ = async {
                    match hint_debounce.as_mut() {
                        Some(sleep) => sleep.as_mut().await,
                        None => std::future::pending().await,
                    }
                } => {
                    hint_debounce = None;
                    let id = reemit_store.load_gateway_identity().await.unwrap().unwrap();
                    emit_gateway_actual_state_from_storage(
                        &reemit_hub, &reemit_store, &id, &reemit_gw
                    ).await;
                }
            }
        }
    });

    // Build minimal frames with unknown key_hints.
    // Frame = key_hint(2 BE) + msg_type(1) + nonce(8 BE) + fake_tag(16) = 27 bytes.
    let build_frame = |key_hint: u16| {
        let mut frame = vec![0u8; 27];
        frame[0..2].copy_from_slice(&key_hint.to_be_bytes());
        frame[2] = 0x01; // MSG_WAKE
        frame
    };

    // Send first frame with unknown key_hint 0x9999.
    gateway
        .process_frame(&build_frame(0x9999), vec![0; 6])
        .await;

    // Verify NO immediate re-emission (within 50ms).
    let immediate_result =
        tokio::time::timeout(Duration::from_millis(50), read_framed(&mut client)).await;
    assert!(
        immediate_result.is_err(),
        "no immediate ACTUAL_STATE re-emission expected"
    );

    // Send second frame with different unknown key_hint 0x8888 (resets debounce).
    gateway
        .process_frame(&build_frame(0x8888), vec![0; 6])
        .await;

    // Wait for debounce to elapse (100ms from the last frame).
    let reemitted_frame =
        tokio::time::timeout(Duration::from_millis(300), read_framed(&mut client))
            .await
            .expect("ACTUAL_STATE should be re-emitted after debounce");
    let reemitted_map = decode_cbor_map(&reemitted_frame);

    // Verify missing_key_hints (key 20) contains both hints.
    let missing_hints = match map_get(&reemitted_map, 20).unwrap() {
        Value::Array(arr) => arr
            .iter()
            .map(|v| match v {
                Value::Integer(i) => {
                    let val: i128 = (*i).into();
                    val as u16
                }
                other => panic!("expected integer in missing_key_hints, got {other:?}"),
            })
            .collect::<Vec<u16>>(),
        other => panic!("expected array missing_key_hints, got {other:?}"),
    };
    assert_eq!(
        missing_hints.len(),
        2,
        "both unknown hints should be coalesced into a single emission"
    );
    assert!(missing_hints.contains(&0x9999));
    assert!(missing_hints.contains(&0x8888));
}
