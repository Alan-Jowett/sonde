// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Master key rotation execution engine (GW-2006, GW-2007, GW-2013).
//!
//! Coordinates the 8-step rotation pipeline and crash recovery, receiving
//! rotation payloads from both gRPC (`SubmitRotation`) and DESIRED_STATE
//! ingress channels.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};
use tracing::{error, info, warn};
use zeroize::Zeroizing;

use crate::connector::{ConnectorEventHub, GatewayDesiredState};
use crate::gateway_identity::GatewayIdentity;
use crate::rotation::{
    decrypt_rotation_payload, DecryptedRotation, RotationError, RotationRateLimiter,
};
use crate::sqlite_storage::SqliteStorage;

/// Channel type matching the admin service's rotation submission channel.
pub type RotationSubmitChannel =
    mpsc::UnboundedSender<(Vec<u8>, oneshot::Sender<Result<(), String>>)>;

/// Receiver side of the rotation submission channel.
type RotationSubmitReceiver =
    mpsc::UnboundedReceiver<(Vec<u8>, oneshot::Sender<Result<(), String>>)>;

/// Notification sent when a rotation completes, carrying the new key state
/// so the gateway can emit a full gateway ACTUAL_STATE.
#[derive(Debug, Clone)]
pub struct RotationCompleteNotification {
    pub new_master_key_id: [u8; 16],
    pub new_epoch: u64,
}

/// Rotation execution engine.
///
/// Receives rotation payloads from two channels (gRPC and DESIRED_STATE),
/// validates them, and executes the 8-step rotation pipeline. At most one
/// rotation can be active at a time; concurrent requests are rejected.
///
/// ## Startup wiring
///
/// The gateway binary (or test harness) must:
/// 1. Call [`RotationEngine::resume_pending_rotation`] before starting the
///    connector or admin service (step 2a per gateway-design.md §23.11).
/// 2. Create channels, pass senders to [`AdminService::with_rotation_tx`]
///    and [`ConnectorService::set_gateway_desired_state_tx`].
/// 3. Spawn [`RotationEngine::run`] as a tokio task.
/// 4. Optionally subscribe to `rotation_complete_tx` to trigger gateway
///    ACTUAL_STATE re-emission on rotation completion.
pub struct RotationEngine {
    storage: Arc<SqliteStorage>,
    identity: GatewayIdentity,
    event_hub: Arc<ConnectorEventHub>,
    rate_limiter: RotationRateLimiter,
    /// Receives rotation payloads from the gRPC `SubmitRotation` RPC.
    grpc_rx: RotationSubmitReceiver,
    /// Receives parsed DESIRED_STATE from the connector.
    desired_state_rx: mpsc::UnboundedReceiver<GatewayDesiredState>,
    /// Set to `true` while a rotation is executing to reject concurrent requests.
    rotation_active: Arc<AtomicBool>,
    /// Optional notification channel for rotation completion. The gateway
    /// binary subscribes to trigger a full gateway ACTUAL_STATE re-emission
    /// (which requires runtime state not available to this engine).
    rotation_complete_tx: Option<mpsc::UnboundedSender<RotationCompleteNotification>>,
}

impl RotationEngine {
    pub fn new(
        storage: Arc<SqliteStorage>,
        identity: GatewayIdentity,
        event_hub: Arc<ConnectorEventHub>,
        grpc_rx: RotationSubmitReceiver,
        desired_state_rx: mpsc::UnboundedReceiver<GatewayDesiredState>,
    ) -> Self {
        Self {
            storage,
            identity,
            event_hub,
            rate_limiter: RotationRateLimiter::default(),
            grpc_rx,
            desired_state_rx,
            rotation_active: Arc::new(AtomicBool::new(false)),
            rotation_complete_tx: None,
        }
    }

    /// Set the notification channel for rotation completion events.
    ///
    /// When a rotation completes, the engine sends a [`RotationCompleteNotification`]
    /// so the gateway can emit a full gateway ACTUAL_STATE with all runtime fields
    /// (channel, version, fingerprint, etc.).
    pub fn with_rotation_complete_tx(
        mut self,
        tx: mpsc::UnboundedSender<RotationCompleteNotification>,
    ) -> Self {
        self.rotation_complete_tx = Some(tx);
        self
    }

    /// Run the rotation engine event loop.
    ///
    /// This should be spawned as a tokio task. It runs until both channels
    /// are closed.
    pub async fn run(mut self) {
        info!("rotation engine started");
        loop {
            tokio::select! {
                Some((payload, reply_tx)) = self.grpc_rx.recv() => {
                    let result = self.handle_rotation_payload(&payload, true).await;
                    let _ = reply_tx.send(result);
                }
                Some(desired_state) = self.desired_state_rx.recv() => {
                    self.handle_desired_state(desired_state).await;
                }
                else => {
                    info!("rotation engine channels closed, shutting down");
                    break;
                }
            }
        }
    }

    /// Handle a full DESIRED_STATE message — process rotation_payload if present.
    async fn handle_desired_state(&mut self, state: GatewayDesiredState) {
        if let Some(payload) = state.rotation_payload {
            // Errors from DESIRED_STATE rotation are silently discarded per spec.
            let _ = self.handle_rotation_payload(&payload, false).await;
        }
        // Future: handle recovered_psks, salt, kdf_params, channel changes here.
    }

    /// Process a raw rotation payload from either gRPC or DESIRED_STATE.
    ///
    /// `is_grpc` controls the error response style — gRPC gets error messages,
    /// DESIRED_STATE silently discards.
    pub async fn handle_rotation_payload(
        &mut self,
        payload: &[u8],
        is_grpc: bool,
    ) -> Result<(), String> {
        // Check for concurrent rotation — reject before any crypto work.
        if self.rotation_active.load(Ordering::SeqCst) {
            let msg = "rotation already in progress";
            if is_grpc {
                return Err(msg.into());
            }
            warn!("{msg} — discarding incoming rotation payload");
            return Err(msg.into());
        }

        // Also check the DB for pending_rotation (crash recovery may have set it).
        let db_in_progress = self
            .storage
            .is_rotation_in_progress()
            .await
            .map_err(|e| format!("storage error: {e}"))?;
        if db_in_progress {
            let msg = "rotation already in progress (pending_rotation exists)";
            if is_grpc {
                return Err(msg.into());
            }
            warn!("{msg} — discarding incoming rotation payload");
            return Err(msg.into());
        }

        // Load current epoch and rotation code for validation.
        let (_, current_epoch) = self
            .storage
            .init_master_key_id()
            .await
            .map_err(|e| format!("failed to load master_key_epoch: {e}"))?;

        // Rate limit check.
        if !self.rate_limiter.check(current_epoch) {
            if is_grpc {
                return Err("rotation attempt rate-limited".into());
            }
            // DESIRED_STATE: silently discard per evolve-962 §2.6.1 rule 8.
            return Err("rate-limited".into());
        }

        // Decrypt the rotation payload.
        let (x25519_secret, _) = self
            .identity
            .to_x25519()
            .map_err(|e| format!("X25519 conversion failed: {e}"))?;
        let gateway_id = self.identity.gateway_id();

        let decrypted =
            match decrypt_rotation_payload(payload, &x25519_secret, gateway_id, current_epoch) {
                Ok(d) => d,
                Err(RotationError::WrongRotationCode) => {
                    self.rate_limiter.record_failure(current_epoch);
                    return Err("rotation code does not match".into());
                }
                Err(RotationError::RateLimited) => {
                    return Err("rotation attempt rate-limited".into());
                }
                Err(e) => {
                    self.rate_limiter.record_failure(current_epoch);
                    return Err(format!("rotation payload error: {e}"));
                }
            };

        // Verify rotation code against stored code.
        let stored_code = self
            .storage
            .init_rotation_code()
            .await
            .map_err(|e| format!("failed to load rotation_code: {e}"))?;

        if decrypted.rotation_code != stored_code {
            self.rate_limiter.record_failure(current_epoch);
            return Err("rotation code does not match".into());
        }

        // Verify epoch (step 3): new_epoch must be current_epoch + 1.
        let new_epoch = current_epoch.checked_add(1).ok_or("epoch overflow")?;

        // Execute the rotation pipeline.
        self.rotation_active.store(true, Ordering::SeqCst);
        let result = self
            .execute_rotation(decrypted, new_epoch, current_epoch)
            .await;
        self.rotation_active.store(false, Ordering::SeqCst);

        result
    }

    /// Execute the full rotation pipeline (steps 4–8).
    async fn execute_rotation(
        &self,
        decrypted: DecryptedRotation,
        new_epoch: u64,
        current_epoch: u64,
    ) -> Result<(), String> {
        let old_key = {
            let mk = self.storage.master_key();
            Zeroizing::new(**mk)
        };

        info!(new_epoch, current_epoch, "starting master key rotation");

        // Step 4: Prepare — write pending_rotation + purge pending_recovery.
        self.storage
            .write_pending_rotation(
                &decrypted.new_master_key,
                &decrypted.new_master_key_id,
                new_epoch,
                decrypted.salt.as_deref(),
                decrypted.kdf_params.as_ref(),
            )
            .await
            .map_err(|e| format!("prepare failed: {e}"))?;

        // Set dual-key state for frame processing during migration.
        self.storage
            .set_rotation_new_key(Zeroizing::new(*decrypted.new_master_key), new_epoch);

        // Execute steps 5–7 (resume-safe).
        let result = self
            .execute_rotation_phases(
                &old_key,
                &decrypted.new_master_key,
                &decrypted.new_master_key_id,
                new_epoch,
                decrypted.salt.as_deref(),
                decrypted.kdf_params.as_ref(),
            )
            .await;

        if let Err(ref e) = result {
            error!(error = %e, "rotation failed during execution");
            // Leave pending_rotation in place for crash recovery.
            self.storage.clear_rotation_new_key();
            return result;
        }

        // Step 7 post: swap in-memory master key.
        self.storage
            .swap_master_key(Zeroizing::new(*decrypted.new_master_key));
        self.storage.clear_rotation_new_key();

        info!(new_epoch, "master key rotation committed successfully");

        // Step 8: Emit updated ACTUAL_STATE.
        self.emit_post_rotation_state(new_epoch, &decrypted.new_master_key_id)
            .await;

        Ok(())
    }

    /// Execute rotation steps 5–7. This is the resume-safe portion — can be
    /// called from both fresh rotation and crash recovery.
    async fn execute_rotation_phases(
        &self,
        old_key: &[u8; 32],
        new_key: &[u8; 32],
        new_key_id: &[u8; 16],
        new_epoch: u64,
        salt: Option<&[u8]>,
        kdf_params: Option<&crate::rotation::KdfParamsPayload>,
    ) -> Result<(), String> {
        // Read current phase to support resume.
        let pending = self
            .storage
            .read_pending_rotation()
            .await
            .map_err(|e| format!("read pending_rotation: {e}"))?
            .ok_or("pending_rotation disappeared during execution")?;

        let phase = pending.phase.as_str();

        // Step 5: Migrate PSKs.
        if phase == "migrating_psks" {
            self.migrate_all_psks(old_key, new_key, new_key_id, new_epoch)
                .await?;
            self.storage
                .update_rotation_phase("rewrapping_identity")
                .await
                .map_err(|e| format!("update phase to rewrapping_identity: {e}"))?;
        }

        // Step 6: Rewrap identity seed.
        let pending = self
            .storage
            .read_pending_rotation()
            .await
            .map_err(|e| format!("read pending_rotation: {e}"))?
            .ok_or("pending_rotation disappeared")?;

        if pending.phase == "rewrapping_identity" {
            self.storage
                .rewrap_identity_seed(old_key, new_key)
                .await
                .map_err(|e| format!("rewrap identity seed: {e}"))?;
            self.storage
                .update_rotation_phase("committing")
                .await
                .map_err(|e| format!("update phase to committing: {e}"))?;
        }

        // Step 7: Atomic commit.
        let pending = self
            .storage
            .read_pending_rotation()
            .await
            .map_err(|e| format!("read pending_rotation: {e}"))?
            .ok_or("pending_rotation disappeared")?;

        if pending.phase == "committing" {
            self.storage
                .commit_rotation(new_key_id, new_epoch, salt, kdf_params)
                .await
                .map_err(|e| format!("commit rotation: {e}"))?;
        }

        Ok(())
    }

    /// Migrate all PSKs (nodes and phone_psks) from old key to new key.
    async fn migrate_all_psks(
        &self,
        old_key: &[u8; 32],
        new_key: &[u8; 32],
        new_key_id: &[u8; 16],
        new_epoch: u64,
    ) -> Result<(), String> {
        // Migrate node PSKs.
        let node_ids = self
            .storage
            .list_unmigrated_node_ids(new_epoch)
            .await
            .map_err(|e| format!("list unmigrated nodes: {e}"))?;

        info!(count = node_ids.len(), "migrating node PSKs");
        for node_id in &node_ids {
            self.storage
                .migrate_node_psk(node_id, old_key, new_key, new_key_id, new_epoch)
                .await
                .map_err(|e| format!("migrate node `{node_id}`: {e}"))?;
        }

        // Migrate phone PSKs.
        let phone_ids = self
            .storage
            .list_unmigrated_phone_ids(new_epoch)
            .await
            .map_err(|e| format!("list unmigrated phones: {e}"))?;

        info!(count = phone_ids.len(), "migrating phone PSKs");
        for phone_id in &phone_ids {
            self.storage
                .migrate_phone_psk(*phone_id, old_key, new_key, new_key_id, new_epoch)
                .await
                .map_err(|e| format!("migrate phone {phone_id}: {e}"))?;
        }

        Ok(())
    }

    /// Emit updated ACTUAL_STATE after rotation completes (step 8).
    ///
    /// Emits per-node ACTUAL_STATE with updated escrow fields. For gateway-level
    /// ACTUAL_STATE (which requires runtime state like channel, version, fingerprint),
    /// sends a [`RotationCompleteNotification`] via the optional callback channel
    /// so the gateway binary can emit a full gateway ACTUAL_STATE.
    async fn emit_post_rotation_state(&self, new_epoch: u64, new_key_id: &[u8; 16]) {
        // Notify the gateway binary to emit a full gateway ACTUAL_STATE.
        if let Some(ref tx) = self.rotation_complete_tx {
            let _ = tx.send(RotationCompleteNotification {
                new_master_key_id: *new_key_id,
                new_epoch,
            });
        }

        // Re-emit node ACTUAL_STATE with updated escrow fields.
        match self.storage.list_node_escrow_state().await {
            Ok(nodes) => {
                for node in &nodes {
                    self.event_hub.emit_actual_state_for_node_with_escrow(
                        node.node_id.clone(),
                        node.current_program_hash.clone(),
                        node.assigned_program_hash.clone(),
                        node.schedule_interval_s,
                        0, // battery_mv not stored in DB
                        node.firmware_abi_version,
                        node.firmware_version.clone(),
                        current_time_ms(),
                        Some(node.encrypted_psk.clone()),
                        Some(node.key_hint),
                        Some(node.master_key_id.clone()),
                        None, // rssi
                    );
                }
                info!(
                    count = nodes.len(),
                    "re-emitted node ACTUAL_STATE after rotation"
                );
            }
            Err(e) => {
                error!(error = %e, "failed to list nodes for post-rotation ACTUAL_STATE");
            }
        }
    }

    /// Resume a pending rotation from crash recovery (GW-2007).
    ///
    /// Called at startup before the main event loop. If `pending_rotation`
    /// exists in the database, decrypts the new key and resumes from the
    /// recorded phase.
    pub async fn resume_pending_rotation(
        storage: &Arc<SqliteStorage>,
        identity: &GatewayIdentity,
    ) -> Result<bool, String> {
        let pending = storage
            .read_pending_rotation()
            .await
            .map_err(|e| format!("read pending_rotation at startup: {e}"))?;

        let pending = match pending {
            Some(p) => p,
            None => return Ok(false),
        };

        info!(
            phase = %pending.phase,
            new_epoch = pending.new_epoch,
            "detected pending rotation — resuming crash recovery"
        );

        // Decrypt the pending new key using the current (old) master key.
        let old_key = {
            let mk = storage.master_key();
            Zeroizing::new(**mk)
        };

        let new_key = SqliteStorage::decrypt_pending_new_key(&old_key, &pending.new_master_key_enc)
            .map_err(|e| format!("decrypt pending new key: {e}"))?;

        // Set dual-key state for frame processing during recovery.
        storage.set_rotation_new_key(Zeroizing::new(*new_key), pending.new_epoch);

        // Create a temporary engine for executing the phases.
        let engine = RotationEngine {
            storage: Arc::clone(storage),
            identity: identity.clone(),
            event_hub: Arc::new(ConnectorEventHub::default()),
            rate_limiter: RotationRateLimiter::default(),
            grpc_rx: mpsc::unbounded_channel().1,
            desired_state_rx: mpsc::unbounded_channel().1,
            rotation_active: Arc::new(AtomicBool::new(true)),
            rotation_complete_tx: None,
        };

        // Parse salt/kdf_params from the pending_rotation record for crash recovery.
        let kdf_params = pending.kdf_params_json.as_deref().and_then(|json| {
            let v: serde_json::Value = serde_json::from_str(json).ok()?;
            Some(crate::rotation::KdfParamsPayload {
                m_cost: v["m_cost"].as_u64()? as u32,
                t_cost: v["t_cost"].as_u64()? as u32,
                p_cost: v["p_cost"].as_u64()? as u32,
                kdf_version: v["kdf_version"].as_u64()? as u32,
            })
        });

        let result = engine
            .execute_rotation_phases(
                &old_key,
                &new_key,
                &pending.new_master_key_id,
                pending.new_epoch,
                pending.salt.as_deref(),
                kdf_params.as_ref(),
            )
            .await;

        if let Err(ref e) = result {
            error!(error = %e, "crash recovery rotation failed");
            storage.clear_rotation_new_key();
            return Err(format!("crash recovery failed: {e}"));
        }

        // Swap in-memory master key.
        storage.swap_master_key(Zeroizing::new(*new_key));
        storage.clear_rotation_new_key();

        info!(
            new_epoch = pending.new_epoch,
            "crash recovery rotation completed"
        );
        Ok(true)
    }
}

/// Helper to get current time in milliseconds.
fn current_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite_storage::SqliteStorage;

    #[tokio::test]
    async fn test_rotation_engine_concurrent_check() {
        let master_key = Zeroizing::new([0x42u8; 32]);
        let store = Arc::new(SqliteStorage::in_memory(master_key).unwrap());

        // The is_rotation_in_progress check works via DB.
        let in_progress = store.is_rotation_in_progress().await.unwrap();
        assert!(!in_progress); // No DB record yet.

        // After writing a pending_rotation, it should report in progress.
        store
            .write_pending_rotation(&[0xAAu8; 32], &[0xBBu8; 16], 2, None, None)
            .await
            .unwrap();
        let in_progress = store.is_rotation_in_progress().await.unwrap();
        assert!(in_progress);
    }
}
