// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Integration tests for master key rotation (GW-2006, GW-2007, GW-2013).
//!
//! Test IDs: T-2003, T-2004, T-2004a, T-2004b, T-2005, T-2008, T-2009, T-2010.

use std::sync::Arc;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use hkdf::Hkdf;
use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, oneshot};
use x25519_dalek::PublicKey as X25519PublicKey;
use zeroize::Zeroizing;

use sonde_gateway::connector::ConnectorEventHub;
use sonde_gateway::gateway_identity::GatewayIdentity;
use sonde_gateway::rotation_engine::RotationEngine;
use sonde_gateway::sqlite_storage::SqliteStorage;
use sonde_gateway::storage::Storage;

/// Build a valid RotationPayloadV1 for testing.
///
/// Encrypts a rotation payload using X25519 + HKDF-SHA-256 + AES-256-GCM,
/// matching the format specified in evolve-962 §2.6.1.
fn build_rotation_payload(
    gateway_identity: &GatewayIdentity,
    current_epoch: u64,
    new_master_key: &[u8; 32],
    rotation_code: &str,
) -> Vec<u8> {
    // Generate ephemeral X25519 keypair.
    let mut rng_bytes = [0u8; 32];
    getrandom::fill(&mut rng_bytes).unwrap();
    let ephemeral_secret = x25519_dalek::StaticSecret::from(rng_bytes);
    let ephemeral_public = X25519PublicKey::from(&ephemeral_secret);

    // Derive gateway's X25519 public key.
    let (_, gw_x25519_public) = gateway_identity.to_x25519().unwrap();

    // X25519 key agreement.
    let shared_secret = ephemeral_secret.diffie_hellman(&gw_x25519_public);

    // HKDF info: gateway_id_raw || current_epoch_be64.
    let gateway_id = gateway_identity.gateway_id();
    let mut info = Vec::with_capacity(24);
    info.extend_from_slice(gateway_id);
    info.extend_from_slice(&current_epoch.to_be_bytes());

    // HKDF-SHA-256.
    let hk = Hkdf::<Sha256>::new(Some(b"sonde-rotation-v1"), shared_secret.as_bytes());
    let mut derived_key = [0u8; 32];
    hk.expand(&info, &mut derived_key).unwrap();

    // Build CBOR plaintext: {1: new_master_key, 2: rotation_code}
    let cbor_map = ciborium::Value::Map(vec![
        (
            ciborium::Value::Integer(1.into()),
            ciborium::Value::Bytes(new_master_key.to_vec()),
        ),
        (
            ciborium::Value::Integer(2.into()),
            ciborium::Value::Text(rotation_code.to_string()),
        ),
    ]);
    let mut plaintext = Vec::new();
    ciborium::into_writer(&cbor_map, &mut plaintext).unwrap();

    // AES-256-GCM encrypt.
    let key = Key::<Aes256Gcm>::from_slice(&derived_key);
    let cipher = Aes256Gcm::new(key);
    let mut nonce_bytes = [0u8; 12];
    getrandom::fill(&mut nonce_bytes).unwrap();
    let nonce = Nonce::from_slice(&nonce_bytes);

    let aad = info; // gateway_id || epoch_be64
    let gcm_payload = Payload {
        msg: &plaintext,
        aad: &aad,
    };
    let ciphertext = cipher.encrypt(nonce, gcm_payload).unwrap();

    // Assemble RotationPayloadV1.
    let mut payload = Vec::with_capacity(1 + 32 + 12 + ciphertext.len());
    payload.push(0x01); // version
    payload.extend_from_slice(ephemeral_public.as_bytes());
    payload.extend_from_slice(&nonce_bytes);
    payload.extend_from_slice(&ciphertext);
    payload
}

/// Helper to create a test gateway with identity, storage, and master key ID.
async fn setup_test_gateway() -> (
    Arc<SqliteStorage>,
    GatewayIdentity,
    [u8; 32], // master_key_id
    u64,      // epoch
    String,   // rotation_code
) {
    let master_key = Zeroizing::new([0x42u8; 32]);
    let store = Arc::new(SqliteStorage::in_memory(master_key).unwrap());

    let identity = GatewayIdentity::generate().unwrap();
    store.store_gateway_identity(&identity).await.unwrap();

    let (key_id, epoch) = store.init_master_key_id().await.unwrap();
    let code = store.init_rotation_code().await.unwrap();

    (store, identity, key_id, epoch, code)
}

/// Helper to register a test node with the given PSK.
async fn register_test_node(store: &Arc<SqliteStorage>, node_id: &str, key_hint: u16) {
    use sonde_gateway::registry::NodeRecord;

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

/// Create a writable test key provider backed by a temp file.
///
/// The temp file is initialized with the given master key in hex format.
/// Returns both the provider (as `Arc<dyn KeyProvider>`) and the
/// `NamedTempFile` handle (must be kept alive for the file to persist).
#[allow(dead_code)] // useful for post-commit recovery tests (T-2015b)
fn test_key_provider(
    master_key: &[u8; 32],
) -> (
    Arc<dyn sonde_gateway::key_provider::KeyProvider>,
    tempfile::NamedTempFile,
) {
    use sonde_gateway::key_provider::FileKeyProvider;
    use std::io::Write as _;

    let mut f = tempfile::NamedTempFile::new().unwrap();
    let hex: String = master_key.iter().map(|b| format!("{b:02x}")).collect();
    writeln!(f, "{hex}").unwrap();
    let provider = FileKeyProvider::new(f.path().to_path_buf());
    (Arc::new(provider), f)
}

/// A writable key provider for tests that don't need actual key persistence.
/// Implements `is_writable() → true` and `write_master_key` as a no-op success.
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

/// T-2003: Rotation code authentication.
#[tokio::test]
async fn test_t2003_rotation_code_authentication() {
    let (store, identity, _, epoch, code) = setup_test_gateway().await;

    // Verify the rotation code was generated (6 chars, [A-Z0-9]).
    assert_eq!(code.len(), 6);
    assert!(code
        .chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()));

    // Build a payload with the WRONG rotation code.
    let new_key = [0xAAu8; 32];
    let wrong_code = "ZZZZZZ";
    assert_ne!(wrong_code, code);

    let payload = build_rotation_payload(&identity, epoch, &new_key, wrong_code);

    // Submit via the engine — should be rejected.
    let (_grpc_tx, grpc_rx) = mpsc::unbounded_channel();
    let (_, desired_state_rx) = mpsc::unbounded_channel();
    let event_hub = Arc::new(ConnectorEventHub::default());

    let mut engine = RotationEngine::new(
        store.clone(),
        identity.clone(),
        event_hub,
        Arc::new(NoopWritableKeyProvider),
        grpc_rx,
        desired_state_rx,
    );

    let result = engine.handle_rotation_payload(&payload, true).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("rotation code does not match"));

    // Submit with the CORRECT rotation code — should succeed.
    let payload = build_rotation_payload(&identity, epoch, &new_key, &code);

    let (_grpc_tx2, grpc_rx2) = mpsc::unbounded_channel();
    let (_, desired_state_rx2) = mpsc::unbounded_channel();
    let event_hub2 = Arc::new(ConnectorEventHub::default());
    let mut engine2 = RotationEngine::new(
        store.clone(),
        identity.clone(),
        event_hub2,
        Arc::new(NoopWritableKeyProvider),
        grpc_rx2,
        desired_state_rx2,
    );

    let result = engine2.handle_rotation_payload(&payload, true).await;
    assert!(
        result.is_ok(),
        "valid rotation should succeed: {:?}",
        result
    );
}

/// T-2004: Master key rotation — happy path.
#[tokio::test]
async fn test_t2004_rotation_happy_path() {
    let (store, identity, _old_key_id, old_epoch, code) = setup_test_gateway().await;

    // Register 3 nodes and 1 phone PSK.
    register_test_node(&store, "node-a", 100).await;
    register_test_node(&store, "node-b", 200).await;
    register_test_node(&store, "node-c", 300).await;

    use sonde_gateway::phone_trust::PhonePskRecord;
    let phone = PhonePskRecord {
        phone_id: 0,
        phone_key_hint: 400,
        psk: Zeroizing::new([0xCCu8; 32]),
        label: "test-phone".to_string(),
        issued_at: std::time::SystemTime::now(),
        status: sonde_gateway::phone_trust::PhonePskStatus::Active,
        key_version: 0,
    };
    store.store_phone_psk(&phone).await.unwrap();

    // Verify old PSKs are loadable.
    let nodes = store.list_nodes().await.unwrap();
    assert_eq!(nodes.len(), 3);
    let phones = store.list_phone_psks().await.unwrap();
    assert_eq!(phones.len(), 1);

    // Build and submit rotation.
    let new_key = [0xAAu8; 32];
    let new_id: [u8; 32] = Sha256::digest(new_key).into();
    let payload = build_rotation_payload(&identity, old_epoch, &new_key, &code);

    let (_, grpc_rx) = mpsc::unbounded_channel();
    let (_, desired_state_rx) = mpsc::unbounded_channel();
    let event_hub = Arc::new(ConnectorEventHub::default());
    let mut engine = RotationEngine::new(
        store.clone(),
        identity.clone(),
        event_hub,
        Arc::new(NoopWritableKeyProvider),
        grpc_rx,
        desired_state_rx,
    );

    let result = engine.handle_rotation_payload(&payload, true).await;
    assert!(result.is_ok(), "rotation should succeed: {:?}", result);

    // Verify: all PSKs still loadable (now encrypted with new key).
    let nodes = store.list_nodes().await.unwrap();
    assert_eq!(nodes.len(), 3);
    // Verify PSK values are preserved.
    for node in &nodes {
        assert_ne!(node.psk, [0u8; 32], "PSK should be non-zero");
    }

    let phones = store.list_phone_psks().await.unwrap();
    assert_eq!(phones.len(), 1);

    // Verify epoch incremented.
    let (new_stored_id, new_stored_epoch) = store.init_master_key_id().await.unwrap();
    assert_eq!(new_stored_epoch, old_epoch + 1);
    assert_eq!(new_stored_id, new_id);

    // Verify pending_rotation deleted.
    assert!(!store.is_rotation_in_progress().await.unwrap());

    // Verify rotation code changed.
    let new_code = store.init_rotation_code().await.unwrap();
    assert_ne!(new_code, code, "rotation code should change after rotation");
}

/// T-2004a: Rotation validation failures.
#[tokio::test]
async fn test_t2004a_rotation_validation_failures() {
    let (store, identity, _, epoch, code) = setup_test_gateway().await;

    let new_key = [0xAAu8; 32];

    // 1. Wrong epoch (build with epoch+1 — AAD won't match).
    let wrong_epoch_payload = build_rotation_payload(&identity, epoch + 1, &new_key, &code);
    let (_, grpc_rx) = mpsc::unbounded_channel();
    let (_, desired_state_rx) = mpsc::unbounded_channel();
    let event_hub = Arc::new(ConnectorEventHub::default());
    let mut engine = RotationEngine::new(
        store.clone(),
        identity.clone(),
        event_hub,
        Arc::new(NoopWritableKeyProvider),
        grpc_rx,
        desired_state_rx,
    );
    let result = engine
        .handle_rotation_payload(&wrong_epoch_payload, true)
        .await;
    assert!(result.is_err(), "wrong epoch should be rejected");

    // 2. Wrong rotation code.
    let wrong_code_payload = build_rotation_payload(&identity, epoch, &new_key, "BADCOD");
    let (_, grpc_rx) = mpsc::unbounded_channel();
    let (_, desired_state_rx) = mpsc::unbounded_channel();
    let event_hub = Arc::new(ConnectorEventHub::default());
    let mut engine = RotationEngine::new(
        store.clone(),
        identity.clone(),
        event_hub,
        Arc::new(NoopWritableKeyProvider),
        grpc_rx,
        desired_state_rx,
    );
    let result = engine
        .handle_rotation_payload(&wrong_code_payload, true)
        .await;
    assert!(result.is_err(), "wrong code should be rejected");

    // 3. Corrupted ciphertext.
    let mut corrupted = build_rotation_payload(&identity, epoch, &new_key, &code);
    // Flip a byte in the ciphertext region.
    let last = corrupted.len() - 1;
    corrupted[last] ^= 0xFF;
    let (_, grpc_rx) = mpsc::unbounded_channel();
    let (_, desired_state_rx) = mpsc::unbounded_channel();
    let event_hub = Arc::new(ConnectorEventHub::default());
    let mut engine = RotationEngine::new(
        store.clone(),
        identity.clone(),
        event_hub,
        Arc::new(NoopWritableKeyProvider),
        grpc_rx,
        desired_state_rx,
    );
    let result = engine.handle_rotation_payload(&corrupted, true).await;
    assert!(result.is_err(), "corrupted ciphertext should be rejected");

    // 4. Replay (submit valid, then replay — epoch already incremented).
    let valid_payload = build_rotation_payload(&identity, epoch, &new_key, &code);
    let (_, grpc_rx) = mpsc::unbounded_channel();
    let (_, desired_state_rx) = mpsc::unbounded_channel();
    let event_hub = Arc::new(ConnectorEventHub::default());
    let mut engine = RotationEngine::new(
        store.clone(),
        identity.clone(),
        event_hub,
        Arc::new(NoopWritableKeyProvider),
        grpc_rx,
        desired_state_rx,
    );
    let result = engine.handle_rotation_payload(&valid_payload, true).await;
    assert!(result.is_ok(), "first submission should succeed");

    // Replay same payload — epoch is now incremented, AAD won't match.
    let (_, grpc_rx) = mpsc::unbounded_channel();
    let (_, desired_state_rx) = mpsc::unbounded_channel();
    let event_hub = Arc::new(ConnectorEventHub::default());
    let mut engine = RotationEngine::new(
        store.clone(),
        identity.clone(),
        event_hub,
        Arc::new(NoopWritableKeyProvider),
        grpc_rx,
        desired_state_rx,
    );
    let result = engine.handle_rotation_payload(&valid_payload, true).await;
    assert!(
        result.is_err(),
        "replay should be rejected (epoch incremented)"
    );
}

/// T-2004b: Concurrent rotation discard.
#[tokio::test]
async fn test_t2004b_concurrent_rotation_discard() {
    let (store, identity, _, epoch, code) = setup_test_gateway().await;
    register_test_node(&store, "node-a", 100).await;

    let new_key = [0xAAu8; 32];
    let new_id: [u8; 32] = Sha256::digest(new_key).into();

    // Manually create a pending_rotation to simulate an in-progress rotation.
    store
        .write_pending_rotation(&new_key, &new_id, epoch + 1)
        .await
        .unwrap();
    assert!(store.is_rotation_in_progress().await.unwrap());

    // Submit a second rotation payload — should be rejected.
    let payload = build_rotation_payload(&identity, epoch, &[0xCCu8; 32], &code);

    let (_, grpc_rx) = mpsc::unbounded_channel();
    let (_, desired_state_rx) = mpsc::unbounded_channel();
    let event_hub = Arc::new(ConnectorEventHub::default());
    let mut engine = RotationEngine::new(
        store.clone(),
        identity.clone(),
        event_hub,
        Arc::new(NoopWritableKeyProvider),
        grpc_rx,
        desired_state_rx,
    );
    let result = engine.handle_rotation_payload(&payload, true).await;
    assert!(result.is_err());
    assert!(
        result.unwrap_err().contains("already in progress"),
        "concurrent rotation should be rejected"
    );

    // DESIRED_STATE path should also silently discard.
    let (_, grpc_rx) = mpsc::unbounded_channel();
    let (_, desired_state_rx) = mpsc::unbounded_channel();
    let event_hub = Arc::new(ConnectorEventHub::default());
    let mut engine = RotationEngine::new(
        store.clone(),
        identity.clone(),
        event_hub,
        Arc::new(NoopWritableKeyProvider),
        grpc_rx,
        desired_state_rx,
    );
    let result = engine.handle_rotation_payload(&payload, false).await;
    assert!(result.is_err());
}

/// T-2005: Crash-safe key rotation.
#[tokio::test]
async fn test_t2005_crash_safe_rotation() {
    let master_key = Zeroizing::new([0x42u8; 32]);
    let store = Arc::new(SqliteStorage::in_memory(master_key.clone()).unwrap());

    let identity = GatewayIdentity::generate().unwrap();
    store.store_gateway_identity(&identity).await.unwrap();
    let (_, epoch) = store.init_master_key_id().await.unwrap();
    let _code = store.init_rotation_code().await.unwrap();

    // Register 10 nodes.
    for i in 0..10 {
        register_test_node(&store, &format!("node-{i}"), (100 + i) as u16).await;
    }

    // Build and submit rotation — manually execute only partial migration to
    // simulate a crash.
    let new_key = [0xAAu8; 32];
    let new_id: [u8; 32] = Sha256::digest(new_key).into();
    let new_epoch = epoch + 1;

    // Step 4: Prepare.
    store
        .write_pending_rotation(&new_key, &new_id, new_epoch)
        .await
        .unwrap();

    // Migrate only 5 of 10 nodes (simulating crash after partial migration).
    let unmigrated = store.list_unmigrated_node_ids(new_epoch).await.unwrap();
    assert_eq!(unmigrated.len(), 10);
    for node_id in &unmigrated[..5] {
        store
            .migrate_node_psk(node_id, &master_key, &new_key, &new_id, new_epoch)
            .await
            .unwrap();
    }

    // Verify 5 migrated, 5 not.
    let still_unmigrated = store.list_unmigrated_node_ids(new_epoch).await.unwrap();
    assert_eq!(still_unmigrated.len(), 5);

    // Simulate crash recovery.
    let recovered =
        RotationEngine::resume_pending_rotation(&store, &identity, &NoopWritableKeyProvider)
            .await
            .unwrap();
    assert!(
        recovered.is_some(),
        "crash recovery should detect and resume rotation"
    );

    // Verify all 10 nodes are now migrated.
    let remaining = store.list_unmigrated_node_ids(new_epoch).await.unwrap();
    assert_eq!(
        remaining.len(),
        0,
        "all nodes should be migrated after recovery"
    );

    // Verify pending_rotation deleted.
    assert!(!store.is_rotation_in_progress().await.unwrap());

    // Verify all nodes are loadable.
    let nodes = store.list_nodes().await.unwrap();
    assert_eq!(nodes.len(), 10);

    // Verify epoch updated.
    let (stored_id, stored_epoch) = store.init_master_key_id().await.unwrap();
    assert_eq!(stored_epoch, new_epoch);
    assert_eq!(stored_id, new_id);
}

/// T-2008: gRPC rotation path.
#[tokio::test]
async fn test_t2008_grpc_rotation_path() {
    let (store, identity, _, epoch, code) = setup_test_gateway().await;
    register_test_node(&store, "node-a", 100).await;

    let new_key = [0xAAu8; 32];
    let payload = build_rotation_payload(&identity, epoch, &new_key, &code);

    // Submit via gRPC channel.
    let (grpc_tx, grpc_rx) = mpsc::unbounded_channel();
    let (_, desired_state_rx) = mpsc::unbounded_channel();
    let event_hub = Arc::new(ConnectorEventHub::default());

    let engine = RotationEngine::new(
        store.clone(),
        identity.clone(),
        event_hub,
        Arc::new(NoopWritableKeyProvider),
        grpc_rx,
        desired_state_rx,
    );

    // Spawn the engine and send a rotation.
    let engine_handle = tokio::spawn(engine.run());

    let (reply_tx, reply_rx) = oneshot::channel();
    grpc_tx.send((payload.clone(), reply_tx)).unwrap();

    let result: Result<Result<(), String>, _> =
        tokio::time::timeout(std::time::Duration::from_secs(5), reply_rx)
            .await
            .expect("timeout waiting for rotation result");
    let result = result.expect("channel closed");

    assert!(result.is_ok(), "gRPC rotation should succeed: {:?}", result);

    // Verify epoch incremented.
    let (_, new_epoch) = store.init_master_key_id().await.unwrap();
    assert_eq!(new_epoch, epoch + 1);

    // Submit same payload again — should fail (epoch incremented).
    let (reply_tx2, reply_rx2) = oneshot::channel();
    grpc_tx.send((payload, reply_tx2)).unwrap();

    let result2: Result<Result<(), String>, _> =
        tokio::time::timeout(std::time::Duration::from_secs(5), reply_rx2)
            .await
            .expect("timeout");
    let result2 = result2.expect("channel closed");

    assert!(result2.is_err(), "replay should fail");

    // Clean shutdown.
    drop(grpc_tx);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), engine_handle).await;
}

/// T-2009: Crash recovery — all rotation phases.
#[tokio::test]
async fn test_t2009_crash_recovery_all_phases() {
    let master_key = Zeroizing::new([0x42u8; 32]);
    let new_key = [0xAAu8; 32];
    let new_id: [u8; 32] = Sha256::digest(new_key).into();

    // Phase 1: Crash during migrating_psks (tested in T-2005).

    // Phase 2: Crash during rewrapping_identity.
    {
        let store = Arc::new(SqliteStorage::in_memory(master_key.clone()).unwrap());
        let identity = GatewayIdentity::generate().unwrap();
        store.store_gateway_identity(&identity).await.unwrap();
        let (_, epoch) = store.init_master_key_id().await.unwrap();
        store.init_rotation_code().await.unwrap();
        register_test_node(&store, "node-a", 100).await;
        let new_epoch = epoch + 1;

        // Prepare + migrate all PSKs + set phase to rewrapping_identity.
        store
            .write_pending_rotation(&new_key, &new_id, new_epoch)
            .await
            .unwrap();
        let nodes = store.list_unmigrated_node_ids(new_epoch).await.unwrap();
        for nid in &nodes {
            store
                .migrate_node_psk(nid, &master_key, &new_key, &new_id, new_epoch)
                .await
                .unwrap();
        }
        store
            .update_rotation_phase("rewrapping_identity")
            .await
            .unwrap();

        // Crash recovery.
        let recovered =
            RotationEngine::resume_pending_rotation(&store, &identity, &NoopWritableKeyProvider)
                .await
                .unwrap();
        assert!(recovered.is_some());

        // Verify identity is loadable with the new key.
        let loaded = store.load_gateway_identity().await.unwrap();
        assert!(
            loaded.is_some(),
            "identity should be loadable after rewrap recovery"
        );
        assert!(!store.is_rotation_in_progress().await.unwrap());
    }

    // Phase 3: Crash during committing.
    {
        let store = Arc::new(SqliteStorage::in_memory(master_key.clone()).unwrap());
        let identity = GatewayIdentity::generate().unwrap();
        store.store_gateway_identity(&identity).await.unwrap();
        let (_, epoch) = store.init_master_key_id().await.unwrap();
        store.init_rotation_code().await.unwrap();
        register_test_node(&store, "node-b", 200).await;
        let new_epoch = epoch + 1;

        // Prepare + migrate + rewrap + set phase to committing.
        store
            .write_pending_rotation(&new_key, &new_id, new_epoch)
            .await
            .unwrap();
        let nodes = store.list_unmigrated_node_ids(new_epoch).await.unwrap();
        for nid in &nodes {
            store
                .migrate_node_psk(nid, &master_key, &new_key, &new_id, new_epoch)
                .await
                .unwrap();
        }
        store
            .rewrap_identity_seed(&master_key, &new_key)
            .await
            .unwrap();
        store.update_rotation_phase("committing").await.unwrap();

        // Crash recovery.
        let recovered =
            RotationEngine::resume_pending_rotation(&store, &identity, &NoopWritableKeyProvider)
                .await
                .unwrap();
        assert!(recovered.is_some());
        assert!(!store.is_rotation_in_progress().await.unwrap());

        // Verify identity is loadable.
        let loaded = store.load_gateway_identity().await.unwrap();
        assert!(loaded.is_some());
        let (stored_id, stored_epoch) = store.init_master_key_id().await.unwrap();
        assert_eq!(stored_epoch, new_epoch);
        assert_eq!(stored_id, new_id);
    }
}

/// T-2010: Pending recovery purge on rotation.
#[tokio::test]
async fn test_t2010_pending_recovery_purge() {
    let (store, identity, _, epoch, code) = setup_test_gateway().await;
    register_test_node(&store, "node-a", 100).await;

    // Insert records into pending_recovery.
    let fake_key_id = [0x11u8; 32];
    store
        .insert_pending_recovery(500, "recovered-node", &[0xEEu8; 60], &fake_key_id, epoch)
        .await
        .unwrap();

    // Verify pending_recovery has a record.
    let pending = store.lookup_pending_recovery(500).await.unwrap();
    assert_eq!(pending.len(), 1);

    // Initiate rotation.
    let new_key = [0xAAu8; 32];
    let new_id: [u8; 32] = Sha256::digest(new_key).into();
    let payload = build_rotation_payload(&identity, epoch, &new_key, &code);

    let (_, grpc_rx) = mpsc::unbounded_channel();
    let (_, desired_state_rx) = mpsc::unbounded_channel();
    let event_hub = Arc::new(ConnectorEventHub::default());
    let mut engine = RotationEngine::new(
        store.clone(),
        identity.clone(),
        event_hub,
        Arc::new(NoopWritableKeyProvider),
        grpc_rx,
        desired_state_rx,
    );

    let result = engine.handle_rotation_payload(&payload, true).await;
    assert!(result.is_ok(), "rotation should succeed: {:?}", result);

    // Verify pending_recovery was purged.
    let pending = store.lookup_pending_recovery(500).await.unwrap();
    assert!(
        pending.is_empty(),
        "pending_recovery should be purged after rotation"
    );

    // Verify nodes are re-emitted (checked via list_node_escrow_state).
    let escrow = store.list_node_escrow_state().await.unwrap();
    assert_eq!(escrow.len(), 1);
    assert_eq!(escrow[0].master_key_id, new_id.to_vec());
}
