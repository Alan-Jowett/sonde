// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Linux Secret Service-based [`PskProtector`] implementation.
//!
//! Stores and retrieves PSK material from the D-Bus Secret Service keyring
//! (GNOME Keyring, KWallet, or any other compatible implementation).
//!
//! The PSK is stored as a 32-byte binary secret under the attributes
//! `service = "sonde-pair"` and `account = <label>`.  The default label is
//! `"sonde-pair-phone-psk"`.
//!
//! Enabled by the `secret-service-store` cargo feature.

use zeroize::Zeroizing;

use crate::error::PairingError;
use crate::file_store::PskProtector;

const SERVICE_ATTR: &str = "sonde-pair";
const DEFAULT_LABEL: &str = "sonde-pair-phone-psk";

/// Protects PSK material using the Linux Secret Service keyring.
///
/// On [`protect`](PskProtector::protect), the PSK is stored in the keyring and
/// an opaque label is returned for storage in the JSON file.  On
/// [`unprotect`](PskProtector::unprotect), the label is decoded and used to
/// retrieve the PSK from the keyring.
pub struct SecretServicePskProtector {
    label: String,
}

impl SecretServicePskProtector {
    /// Create a protector with the given keyring label.
    ///
    /// Use the [`Default`] for new deployments.
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
        }
    }
}

impl Default for SecretServicePskProtector {
    fn default() -> Self {
        Self::new(DEFAULT_LABEL)
    }
}

impl PskProtector for SecretServicePskProtector {
    fn protect(&self, psk: &[u8; 32]) -> Result<Vec<u8>, PairingError> {
        let label = self.label.clone();
        let psk_bytes: Zeroizing<Vec<u8>> = Zeroizing::new(psk.to_vec());

        run_blocking(move || async move { ss_store(&psk_bytes, &label).await })?;

        // Return the label as opaque bytes — unprotect uses this to look up
        // the secret from the keyring.
        Ok(self.label.as_bytes().to_vec())
    }

    fn unprotect(&self, protected: &[u8]) -> Result<Zeroizing<[u8; 32]>, PairingError> {
        let label = std::str::from_utf8(protected)
            .map_err(|_| PairingError::StoreCorrupted("invalid Secret Service label".into()))?
            .to_owned();

        run_blocking(move || async move { ss_load(&label).await })
    }

    fn clear_protected(&self) -> Result<(), PairingError> {
        let label = self.label.clone();
        run_blocking(move || async move { ss_delete(&label).await })
    }
}

/// Drive an async Secret Service call synchronously.
///
/// Uses `block_in_place` when inside a multi-thread tokio runtime, otherwise
/// spins up a temporary single-threaded runtime.
fn run_blocking<F, Fut, T>(f: F) -> Result<T, PairingError>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<T, PairingError>>,
{
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        tokio::task::block_in_place(move || handle.block_on(f()))
    } else {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| {
                PairingError::EncryptionFailed(format!("failed to build async runtime: {e}"))
            })?;
        rt.block_on(f())
    }
}

async fn ss_store(key_bytes: &[u8], label: &str) -> Result<(), PairingError> {
    use secret_service::{EncryptionType, SecretService};
    use std::collections::HashMap;

    let ss = SecretService::connect(EncryptionType::Dh)
        .await
        .map_err(|e| PairingError::EncryptionFailed(format!("Secret Service connect: {e}")))?;

    let collection = ss
        .get_default_collection()
        .await
        .map_err(|e| PairingError::EncryptionFailed(format!("default collection: {e}")))?;

    collection
        .unlock()
        .await
        .map_err(|e| PairingError::EncryptionFailed(format!("unlock collection: {e}")))?;

    let attributes = HashMap::from([("service", SERVICE_ATTR), ("account", label)]);

    collection
        .create_item(
            label,
            attributes,
            key_bytes,
            true, // replace existing item
            "application/octet-stream",
        )
        .await
        .map_err(|e| PairingError::EncryptionFailed(format!("store secret: {e}")))?;

    Ok(())
}

async fn ss_load(label: &str) -> Result<Zeroizing<[u8; 32]>, PairingError> {
    use secret_service::{EncryptionType, SecretService};
    use std::collections::HashMap;

    let ss = SecretService::connect(EncryptionType::Dh)
        .await
        .map_err(|e| PairingError::EncryptionFailed(format!("Secret Service connect: {e}")))?;

    let collection = ss
        .get_default_collection()
        .await
        .map_err(|e| PairingError::EncryptionFailed(format!("default collection: {e}")))?;

    collection
        .unlock()
        .await
        .map_err(|e| PairingError::EncryptionFailed(format!("unlock collection: {e}")))?;

    let attributes = HashMap::from([("service", SERVICE_ATTR), ("account", label)]);

    let items = collection
        .search_items(attributes)
        .await
        .map_err(|e| PairingError::EncryptionFailed(format!("keyring search: {e}")))?;

    let item = items.into_iter().next().ok_or_else(|| {
        PairingError::StoreLoadFailed(format!("phone_psk not found in keyring (label={label:?})"))
    })?;

    let secret_bytes = item
        .get_secret()
        .await
        .map_err(|e| PairingError::EncryptionFailed(format!("retrieve secret: {e}")))?;
    let secret_bytes = Zeroizing::new(secret_bytes);

    if secret_bytes.len() != 32 {
        return Err(PairingError::StoreCorrupted(format!(
            "keyring secret has {} bytes, expected 32",
            secret_bytes.len()
        )));
    }

    let mut psk = Zeroizing::new([0u8; 32]);
    psk.copy_from_slice(&secret_bytes);
    Ok(psk)
}

async fn ss_delete(label: &str) -> Result<(), PairingError> {
    use secret_service::{EncryptionType, SecretService};
    use std::collections::HashMap;

    let ss = SecretService::connect(EncryptionType::Dh)
        .await
        .map_err(|e| PairingError::EncryptionFailed(format!("Secret Service connect: {e}")))?;

    let collection = ss
        .get_default_collection()
        .await
        .map_err(|e| PairingError::EncryptionFailed(format!("default collection: {e}")))?;

    collection
        .unlock()
        .await
        .map_err(|e| PairingError::EncryptionFailed(format!("unlock collection: {e}")))?;

    let attributes = HashMap::from([("service", SERVICE_ATTR), ("account", label)]);

    let items = collection
        .search_items(attributes)
        .await
        .map_err(|e| PairingError::EncryptionFailed(format!("keyring search: {e}")))?;

    for item in items {
        item.delete()
            .await
            .map_err(|e| PairingError::EncryptionFailed(format!("delete secret: {e}")))?;
    }

    Ok(())
}
