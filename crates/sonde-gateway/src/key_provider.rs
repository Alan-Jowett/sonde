// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Key provider abstractions for loading the gateway master key.
//!
//! The [`KeyProvider`] trait abstracts how the 32-byte master key is obtained,
//! allowing pluggable backends that range from a simple plaintext hex file to
//! OS-native secret storage mechanisms.
//!
//! # Available backends
//!
//! | Backend | Platform | Mechanism |
//! |---------|----------|-----------|
//! | [`FileKeyProvider`] | All | Read a 64-hex-char key from a file (default) |
//! | [`EnvKeyProvider`] | All | Read from an environment variable |
//! | [`DpapiKeyProvider`] | Windows | DPAPI-encrypted blob tied to the user/machine account |
//! | [`SecretServiceKeyProvider`] | Linux | D-Bus Secret Service (GNOME Keyring / KWallet) |
//!
//! # Backend selection
//!
//! The gateway binary selects the backend via the `--key-provider` CLI flag
//! (default: `file`).  Existing `--master-key-file` and `SONDE_MASTER_KEY`
//! workflows are preserved unchanged.
//!
//! # Provisioning helpers
//!
//! Platform-specific helpers are provided for writing key material into the
//! backend:
//! - Windows: [`protect_with_dpapi`] — encrypts a raw key into a DPAPI blob file.
//! - Linux: [`store_in_secret_service`] — writes a raw key into the keyring.

use std::fmt;
use std::path::PathBuf;
use zeroize::Zeroizing;

// ─────────────────────────────────────────────────────────────────────────────
// Error type
// ─────────────────────────────────────────────────────────────────────────────

/// Errors returned by [`KeyProvider::load_master_key`] and
/// [`KeyProvider::generate_or_load_master_key`].
#[derive(Debug)]
pub enum KeyProviderError {
    /// An I/O error occurred while reading the key material.
    Io(String),
    /// The key material was present but had an unexpected format.
    Format(String),
    /// The requested backend is not available on this platform.
    NotAvailable(String),
    /// The backend returned an error.
    Backend(String),
    /// No key exists in this backend (used to distinguish absence from errors).
    NotFound(String),
}

impl fmt::Display for KeyProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(msg) => write!(f, "I/O error: {msg}"),
            Self::Format(msg) => write!(f, "invalid key format: {msg}"),
            Self::NotAvailable(msg) => write!(f, "backend not available: {msg}"),
            Self::Backend(msg) => write!(f, "backend error: {msg}"),
            Self::NotFound(msg) => write!(f, "key not found: {msg}"),
        }
    }
}

impl std::error::Error for KeyProviderError {}

// ─────────────────────────────────────────────────────────────────────────────
// KeyProvider trait
// ─────────────────────────────────────────────────────────────────────────────

/// Abstracts how the 32-byte gateway master key is obtained.
///
/// Implementations range from a simple plaintext hex file to OS-native secret
/// storage that provides hardware-backed or OS-managed encryption at rest.
///
/// Implementations must be `Send + Sync` so they can be used across async task
/// boundaries.
pub trait KeyProvider: Send + Sync {
    /// Load and return the 32-byte master key, zeroizing it on drop.
    ///
    /// This method is called once at gateway startup.  Errors are fatal.
    fn load_master_key(&self) -> Result<Zeroizing<[u8; 32]>, KeyProviderError>;

    /// Generate a random 32-byte master key and write it to the backend if no
    /// key exists, or load the existing key if one is already present.
    ///
    /// This method provides a safe, idempotent "generate-on-first-use" pattern:
    /// - If a key already exists in the backend, it is loaded unchanged.
    /// - If no key exists, a cryptographically random 32-byte key is generated
    ///   via `getrandom::fill()`, written to the backend, and returned.
    ///
    /// A `tracing::warn!` is emitted when a new key is generated so operators
    /// are aware that a new key was created.
    ///
    /// # Errors
    ///
    /// The default implementation returns [`KeyProviderError::NotAvailable`].
    /// Backends that do not support key generation (e.g. [`EnvKeyProvider`])
    /// will use this default.  Pass `--key-provider env` with
    /// `--generate-master-key` to receive a clear error message.
    fn generate_or_load_master_key(&self) -> Result<Zeroizing<[u8; 32]>, KeyProviderError> {
        Err(KeyProviderError::NotAvailable(
            "this key provider does not support key generation; \
             use --key-provider file, dpapi, or secret-service"
                .into(),
        ))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Shared hex parsing helper
// ─────────────────────────────────────────────────────────────────────────────

/// Parse a 64-character ASCII hex string into a 32-byte key.
///
/// Leading and trailing whitespace is stripped before validation.
pub(crate) fn parse_hex_key(hex: &str) -> Result<Zeroizing<[u8; 32]>, KeyProviderError> {
    let hex = hex.trim();
    if hex.len() != 64 {
        return Err(KeyProviderError::Format(format!(
            "key must be exactly 64 hex characters, got {}",
            hex.len()
        )));
    }
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(KeyProviderError::Format(
            "key contains non-hex characters".into(),
        ));
    }
    let mut key = Zeroizing::new([0u8; 32]);
    for (i, byte) in key.iter_mut().enumerate() {
        // Safety: hex.len() == 64 (checked above), i is 0..32,
        // so i*2..i*2+2 is always a valid ASCII pair within bounds.
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
            .map_err(|e| KeyProviderError::Format(format!("hex parse error at byte {i}: {e}")))?;
    }
    Ok(key)
}

/// Write a 64-character hex key string to a file, using mode 0o600 on Unix.
///
/// On Unix the file is created with owner-read/write only (`0o600`).
/// On other platforms the file is written with default OS permissions.
fn write_hex_key_file(path: &std::path::Path, hex: &str) -> Result<(), KeyProviderError> {
    use std::io::Write as _;

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| {
                KeyProviderError::Io(format!("cannot create key file {}: {e}", path.display()))
            })?;
        f.write_all(hex.as_bytes()).map_err(|e| {
            KeyProviderError::Io(format!("cannot write key file {}: {e}", path.display()))
        })?;
    }

    #[cfg(not(unix))]
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .map_err(|e| {
                KeyProviderError::Io(format!("cannot create key file {}: {e}", path.display()))
            })?;
        f.write_all(hex.as_bytes()).map_err(|e| {
            KeyProviderError::Io(format!("cannot write key file {}: {e}", path.display()))
        })?;
    }

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// FileKeyProvider
// ─────────────────────────────────────────────────────────────────────────────

/// Load the master key from a file containing 64 hex characters.
///
/// This is the default backend and maintains full backward compatibility with
/// the `--master-key-file` workflow.  The file may contain leading/trailing
/// whitespace (e.g. a trailing newline), which is stripped before parsing.
pub struct FileKeyProvider {
    path: PathBuf,
}

impl FileKeyProvider {
    /// Create a new provider that reads the hex key from `path`.
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl KeyProvider for FileKeyProvider {
    fn load_master_key(&self) -> Result<Zeroizing<[u8; 32]>, KeyProviderError> {
        let raw = std::fs::read_to_string(&self.path).map_err(|e| {
            KeyProviderError::Io(format!("cannot read {}: {e}", self.path.display()))
        })?;
        parse_hex_key(&raw)
    }

    fn generate_or_load_master_key(&self) -> Result<Zeroizing<[u8; 32]>, KeyProviderError> {
        // If the key file already exists, load it without overwriting.
        if self.path.exists() {
            tracing::info!(path = %self.path.display(), "master key file exists, loading");
            return self.load_master_key();
        }

        // Generate a fresh key from the OS CSPRNG.
        // The buffer starts zeroed; getrandom::fill overwrites all 32 bytes
        // with cryptographically random data before any use.
        let mut raw = Zeroizing::new([0u8; 32]);
        getrandom::fill(raw.as_mut())
            .map_err(|e| KeyProviderError::Backend(format!("getrandom failed: {e}")))?;

        // Encode as 64 lower-case hex characters using a pre-sized allocation.
        let mut hex = String::with_capacity(64);
        for b in raw.iter() {
            use std::fmt::Write as _;
            write!(hex, "{b:02x}").expect("write to String is infallible");
        }

        // Write the key file with restrictive permissions (0o600 on Unix).
        write_hex_key_file(&self.path, &hex)?;

        tracing::warn!(
            path = %self.path.display(),
            "master key generated and written; back this file up securely"
        );
        Ok(raw)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// EnvKeyProvider
// ─────────────────────────────────────────────────────────────────────────────

/// Load the master key from an environment variable containing 64 hex characters.
///
/// The default variable name is `SONDE_MASTER_KEY`, which preserves the
/// existing env-var workflow.
pub struct EnvKeyProvider {
    var_name: String,
}

impl EnvKeyProvider {
    /// Create a new provider that reads from the given environment variable.
    pub fn new(var_name: impl Into<String>) -> Self {
        Self {
            var_name: var_name.into(),
        }
    }
}

impl Default for EnvKeyProvider {
    fn default() -> Self {
        Self::new("SONDE_MASTER_KEY")
    }
}

impl KeyProvider for EnvKeyProvider {
    fn load_master_key(&self) -> Result<Zeroizing<[u8; 32]>, KeyProviderError> {
        let hex = std::env::var(&self.var_name).map_err(|_| {
            KeyProviderError::Io(format!("environment variable {} is not set", self.var_name))
        })?;
        parse_hex_key(&hex)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// DpapiKeyProvider  (Windows only)
// ─────────────────────────────────────────────────────────────────────────────

/// Load the master key from a Windows DPAPI-protected blob file.
///
/// The blob file contains the raw 32-byte key encrypted with Windows DPAPI
/// (`CryptProtectData`).  Decryption is tied to the Windows user or machine
/// account that created the blob, providing OS-managed key protection at rest.
///
/// # Provisioning
///
/// Create the blob file with [`protect_with_dpapi`] — for example, once during
/// initial deployment or after a key rotation:
///
/// ```no_run
/// # #[cfg(windows)] {
/// use sonde_gateway::key_provider::protect_with_dpapi;
/// let key: [u8; 32] = /* your 32-byte key */
/// #   [0u8; 32];
/// protect_with_dpapi(&key, std::path::Path::new("master.dpapi")).unwrap();
/// # }
/// ```
///
/// # Security
///
/// The DPAPI blob is tied to the Windows user or machine account (depending on
/// the `CRYPTPROTECT_LOCAL_MACHINE` flag used at creation time).  Without the
/// account credentials, the blob cannot be decrypted — even with direct access
/// to the file system.
#[cfg(windows)]
pub struct DpapiKeyProvider {
    blob_path: PathBuf,
}

#[cfg(windows)]
impl DpapiKeyProvider {
    /// Create a new provider that reads and decrypts the DPAPI blob at `blob_path`.
    pub fn new(blob_path: PathBuf) -> Self {
        Self { blob_path }
    }
}

#[cfg(windows)]
impl KeyProvider for DpapiKeyProvider {
    fn load_master_key(&self) -> Result<Zeroizing<[u8; 32]>, KeyProviderError> {
        let blob = std::fs::read(&self.blob_path).map_err(|e| {
            KeyProviderError::Io(format!(
                "cannot read DPAPI blob {}: {e}",
                self.blob_path.display()
            ))
        })?;

        let plaintext = dpapi::decrypt(&blob)
            .map_err(|e| KeyProviderError::Backend(format!("DPAPI decryption failed: {e}")))?;
        // Wrap in Zeroizing so the plaintext is cleared from memory on drop.
        let plaintext = Zeroizing::new(plaintext);

        if plaintext.len() != 32 {
            return Err(KeyProviderError::Format(format!(
                "DPAPI blob decrypted to {} bytes, expected 32",
                plaintext.len()
            )));
        }

        let mut key = Zeroizing::new([0u8; 32]);
        key.copy_from_slice(&plaintext);
        Ok(key)
    }

    fn generate_or_load_master_key(&self) -> Result<Zeroizing<[u8; 32]>, KeyProviderError> {
        // If the DPAPI blob already exists, load it without overwriting.
        if self.blob_path.exists() {
            tracing::info!(path = %self.blob_path.display(), "DPAPI key blob exists, loading");
            return self.load_master_key();
        }

        // Generate a fresh key from the OS CSPRNG.
        let mut raw = Zeroizing::new([0u8; 32]);
        getrandom::fill(raw.as_mut())
            .map_err(|e| KeyProviderError::Backend(format!("getrandom failed: {e}")))?;

        protect_with_dpapi(&raw, &self.blob_path)?;

        tracing::warn!(
            path = %self.blob_path.display(),
            "master key generated and stored as DPAPI blob; back this file up securely"
        );
        Ok(raw)
    }
}

/// Encrypt a raw 32-byte key with Windows DPAPI and write the blob to `blob_path`.
///
/// The resulting file can only be decrypted by the same Windows user or machine
/// account via [`DpapiKeyProvider`].  Call this once during initial deployment
/// or key rotation.
#[cfg(windows)]
pub fn protect_with_dpapi(
    key: &[u8; 32],
    blob_path: &std::path::Path,
) -> Result<(), KeyProviderError> {
    let blob = dpapi::encrypt(key)
        .map_err(|e| KeyProviderError::Backend(format!("DPAPI encryption failed: {e}")))?;
    std::fs::write(blob_path, &blob).map_err(|e| {
        KeyProviderError::Io(format!(
            "cannot write DPAPI blob {}: {e}",
            blob_path.display()
        ))
    })
}

#[cfg(windows)]
mod dpapi {
    use windows_sys::Win32::Foundation::{GetLastError, LocalFree};
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    /// Decrypt a DPAPI-protected blob, returning the plaintext bytes.
    pub fn decrypt(encrypted_data: &[u8]) -> Result<Vec<u8>, String> {
        let input = CRYPT_INTEGER_BLOB {
            cbData: encrypted_data.len() as u32,
            pbData: encrypted_data.as_ptr() as *mut u8,
        };
        let mut output = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };

        let ok = unsafe {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(), // description (out)
                std::ptr::null_mut(), // optional entropy
                std::ptr::null_mut(), // reserved
                std::ptr::null_mut(), // prompt struct
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };

        if ok == 0 {
            let code = unsafe { GetLastError() };
            return Err(format!(
                "CryptUnprotectData failed: error code {code:#010x}"
            ));
        }

        let plaintext =
            unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
        unsafe { LocalFree(output.pbData as *mut _) };
        Ok(plaintext)
    }

    /// Encrypt plaintext bytes with DPAPI, returning the blob.
    pub fn encrypt(plaintext: &[u8]) -> Result<Vec<u8>, String> {
        let input = CRYPT_INTEGER_BLOB {
            cbData: plaintext.len() as u32,
            pbData: plaintext.as_ptr() as *mut u8,
        };
        let mut output = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };

        let ok = unsafe {
            CryptProtectData(
                &input,
                std::ptr::null_mut(), // description
                std::ptr::null_mut(), // optional entropy
                std::ptr::null_mut(), // reserved
                std::ptr::null_mut(), // prompt struct
                0,                    // flags (user-account scope by default)
                &mut output,
            )
        };

        if ok == 0 {
            let code = unsafe { GetLastError() };
            return Err(format!("CryptProtectData failed: error code {code:#010x}"));
        }

        let encrypted =
            unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
        unsafe { LocalFree(output.pbData as *mut _) };
        Ok(encrypted)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// SecretServiceKeyProvider  (Linux only)
// ─────────────────────────────────────────────────────────────────────────────

/// Load the master key from the Linux Secret Service (D-Bus keyring).
///
/// The master key is stored as a 32-byte binary secret in the OS keyring
/// (GNOME Keyring, KWallet, or any other Secret Service-compatible
/// implementation) under the lookup attributes:
///
/// - `service = "sonde-gateway"`
/// - `account = <label>`
///
/// The default label is `"sonde-gateway-master-key"`.
///
/// # Provisioning
///
/// Write the key into the keyring with [`store_in_secret_service`] — for
/// example, once during initial deployment or after a key rotation:
///
/// ```no_run
/// # #[cfg(target_os = "linux")] {
/// use sonde_gateway::key_provider::store_in_secret_service;
/// let key: [u8; 32] = /* your 32-byte key */
/// #   [0u8; 32];
/// store_in_secret_service(&key, "sonde-gateway-master-key").unwrap();
/// # }
/// ```
///
/// # Security
///
/// The Secret Service stores the key encrypted inside the keyring daemon.  The
/// keyring may itself be protected by a master password or (on systems with a
/// TPM) hardware-backed encryption.  Access is mediated by the D-Bus policy,
/// which restricts reads to the gateway process user.
///
/// For headless servers without an interactive session, configure a
/// file-backed keyring (e.g. `gnome-keyring-daemon --daemonize --unlock`) or
/// use `systemd-creds` as an alternative.
#[cfg(target_os = "linux")]
pub struct SecretServiceKeyProvider {
    label: String,
}

#[cfg(target_os = "linux")]
impl SecretServiceKeyProvider {
    /// Create a provider that retrieves the secret with the given `label`.
    ///
    /// Use `"sonde-gateway-master-key"` (the [`Default`]) for a new deployment.
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
        }
    }
}

#[cfg(target_os = "linux")]
impl Default for SecretServiceKeyProvider {
    fn default() -> Self {
        Self::new("sonde-gateway-master-key")
    }
}

#[cfg(target_os = "linux")]
impl KeyProvider for SecretServiceKeyProvider {
    fn load_master_key(&self) -> Result<Zeroizing<[u8; 32]>, KeyProviderError> {
        let label = self.label.clone();

        // Drive the async secret-service call synchronously.
        // When called from within a tokio multi-thread runtime (the normal
        // startup path), use block_in_place so the scheduler can continue
        // servicing other tasks on remaining threads.  Otherwise — e.g. in
        // unit tests — spin up a temporary single-threaded runtime.
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            tokio::task::block_in_place(move || handle.block_on(ss_load(&label)))
        } else {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| {
                    KeyProviderError::Backend(format!("failed to build async runtime: {e}"))
                })?;
            rt.block_on(ss_load(&label))
        }
    }

    fn generate_or_load_master_key(&self) -> Result<Zeroizing<[u8; 32]>, KeyProviderError> {
        // The Secret Service API does not provide a lightweight "key exists"
        // check separate from loading the secret value — both operations
        // involve the same D-Bus round-trip.  We therefore attempt a load
        // first: if it succeeds the existing key is returned unchanged; if the
        // key is absent (`NotFound`) we generate and store a fresh one.  Any
        // other error (D-Bus connection failure, keyring locked, etc.) is
        // propagated as-is.  This matches the `exists → load` pattern used by
        // `FileKeyProvider` and `DpapiKeyProvider` at the semantic level, even
        // though the implementation is load-first rather than exists-first.
        match self.load_master_key() {
            Ok(key) => {
                tracing::info!(label = %self.label, "master key loaded from Secret Service");
                return Ok(key);
            }
            Err(KeyProviderError::NotFound(_)) => {
                // Key doesn't exist yet — fall through to generate it.
            }
            Err(e) => return Err(e),
        }

        // Generate a fresh key from the OS CSPRNG.
        // The buffer starts zeroed; getrandom::fill overwrites all 32 bytes
        // with cryptographically random data before any use.
        let mut raw = Zeroizing::new([0u8; 32]);
        getrandom::fill(raw.as_mut())
            .map_err(|e| KeyProviderError::Backend(format!("getrandom failed: {e}")))?;

        store_in_secret_service(&raw, &self.label)?;

        tracing::warn!(
            label = %self.label,
            "master key generated and stored in Secret Service"
        );
        Ok(raw)
    }
}

#[cfg(target_os = "linux")]
async fn ss_load(label: &str) -> Result<Zeroizing<[u8; 32]>, KeyProviderError> {
    use secret_service::{EncryptionType, SecretService};
    use std::collections::HashMap;

    let ss = SecretService::connect(EncryptionType::Dh)
        .await
        .map_err(|e| KeyProviderError::Backend(format!("cannot connect to Secret Service: {e}")))?;

    let collection = ss
        .get_default_collection()
        .await
        .map_err(|e| KeyProviderError::Backend(format!("cannot get default collection: {e}")))?;

    collection
        .unlock()
        .await
        .map_err(|e| KeyProviderError::Backend(format!("cannot unlock collection: {e}")))?;

    let attributes = HashMap::from([("service", "sonde-gateway"), ("account", label)]);

    let items = collection
        .search_items(attributes)
        .await
        .map_err(|e| KeyProviderError::Backend(format!("keyring search failed: {e}")))?;

    let item = items.into_iter().next().ok_or_else(|| {
        KeyProviderError::NotFound(format!("master key not found in keyring (label={label:?})"))
    })?;

    let secret_bytes = item
        .get_secret()
        .await
        .map_err(|e| KeyProviderError::Backend(format!("cannot retrieve secret: {e}")))?;
    // Wrap in Zeroizing so the raw key bytes are cleared from memory on drop.
    let secret_bytes = Zeroizing::new(secret_bytes);

    if secret_bytes.len() != 32 {
        return Err(KeyProviderError::Format(format!(
            "keyring secret has {} bytes, expected 32",
            secret_bytes.len()
        )));
    }

    let mut key = Zeroizing::new([0u8; 32]);
    key.copy_from_slice(&secret_bytes);
    Ok(key)
}

/// Store a 32-byte key in the Linux Secret Service keyring.
///
/// The secret is written under the attributes `service = "sonde-gateway"` and
/// `account = label`.  If an item with those attributes already exists it is
/// replaced.
///
/// Use this during initial deployment or after a key rotation, then switch to
/// `--key-provider secret-service` on the next gateway start.
#[cfg(target_os = "linux")]
pub fn store_in_secret_service(key: &[u8; 32], label: &str) -> Result<(), KeyProviderError> {
    // Use Zeroizing<Vec<u8>> so the key copy is cleared on drop.
    let key_bytes: Zeroizing<Vec<u8>> = Zeroizing::new(key.to_vec());
    let label = label.to_owned();

    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        tokio::task::block_in_place(move || handle.block_on(ss_store(&key_bytes, &label)))
    } else {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| {
                KeyProviderError::Backend(format!("failed to build async runtime: {e}"))
            })?;
        rt.block_on(ss_store(&key_bytes, &label))
    }
}

#[cfg(target_os = "linux")]
async fn ss_store(key_bytes: &[u8], label: &str) -> Result<(), KeyProviderError> {
    use secret_service::{EncryptionType, SecretService};
    use std::collections::HashMap;

    let ss = SecretService::connect(EncryptionType::Dh)
        .await
        .map_err(|e| KeyProviderError::Backend(format!("cannot connect to Secret Service: {e}")))?;

    let collection = ss
        .get_default_collection()
        .await
        .map_err(|e| KeyProviderError::Backend(format!("cannot get default collection: {e}")))?;

    collection
        .unlock()
        .await
        .map_err(|e| KeyProviderError::Backend(format!("cannot unlock collection: {e}")))?;

    let attributes = HashMap::from([("service", "sonde-gateway"), ("account", label)]);

    collection
        .create_item(
            label,
            attributes,
            key_bytes,
            true, // replace existing item
            "application/octet-stream",
        )
        .await
        .map_err(|e| KeyProviderError::Backend(format!("cannot store secret: {e}")))?;

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    const HEX_KEY: &str = "4242424242424242424242424242424242424242424242424242424242424242";

    // ── parse_hex_key ──────────────────────────────────────────────────────

    #[test]
    fn parse_hex_key_ok() {
        let key = parse_hex_key(HEX_KEY).unwrap();
        assert_eq!(*key, [0x42u8; 32]);
    }

    #[test]
    fn parse_hex_key_trims_whitespace() {
        let with_newline = format!("  {HEX_KEY}\n");
        let key = parse_hex_key(&with_newline).unwrap();
        assert_eq!(*key, [0x42u8; 32]);
    }

    #[test]
    fn parse_hex_key_wrong_length() {
        let err = parse_hex_key("abcd").unwrap_err();
        assert!(matches!(err, KeyProviderError::Format(_)));
    }

    #[test]
    fn parse_hex_key_non_hex_chars() {
        let bad: String = "z".repeat(64);
        let err = parse_hex_key(&bad).unwrap_err();
        assert!(matches!(err, KeyProviderError::Format(_)));
    }

    // ── FileKeyProvider ────────────────────────────────────────────────────

    #[test]
    fn file_key_provider_ok() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "{HEX_KEY}").unwrap();
        let provider = FileKeyProvider::new(f.path().to_path_buf());
        let key = provider.load_master_key().unwrap();
        assert_eq!(*key, [0x42u8; 32]);
    }

    #[test]
    fn file_key_provider_missing_file() {
        let provider = FileKeyProvider::new(PathBuf::from("/nonexistent/path/key.hex"));
        let err = provider.load_master_key().unwrap_err();
        assert!(matches!(err, KeyProviderError::Io(_)));
    }

    #[test]
    fn file_key_provider_bad_content() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "not-a-hex-key").unwrap();
        let provider = FileKeyProvider::new(f.path().to_path_buf());
        let err = provider.load_master_key().unwrap_err();
        assert!(matches!(err, KeyProviderError::Format(_)));
    }

    // ── EnvKeyProvider ─────────────────────────────────────────────────────

    #[test]
    fn env_key_provider_ok() {
        let var = "SONDE_TEST_KEY_PROVIDER_OK";
        std::env::set_var(var, HEX_KEY);
        let provider = EnvKeyProvider::new(var);
        let key = provider.load_master_key().unwrap();
        assert_eq!(*key, [0x42u8; 32]);
        std::env::remove_var(var);
    }

    #[test]
    fn env_key_provider_not_set() {
        let var = "SONDE_TEST_KEY_PROVIDER_ABSENT_XYZ";
        std::env::remove_var(var);
        let provider = EnvKeyProvider::new(var);
        let err = provider.load_master_key().unwrap_err();
        assert!(matches!(err, KeyProviderError::Io(_)));
    }

    #[test]
    fn env_key_provider_bad_value() {
        let var = "SONDE_TEST_KEY_PROVIDER_BAD";
        std::env::set_var(var, "short");
        let provider = EnvKeyProvider::new(var);
        let err = provider.load_master_key().unwrap_err();
        assert!(matches!(err, KeyProviderError::Format(_)));
        std::env::remove_var(var);
    }

    // ── generate_or_load_master_key (FileKeyProvider) ──────────────────────

    #[test]
    fn file_provider_generate_creates_key_file_when_missing() {
        // Use a path inside a temp dir that does not yet exist.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new_master.key");
        assert!(!path.exists(), "file should not exist before generate");

        let provider = FileKeyProvider::new(path.clone());
        let key = provider.generate_or_load_master_key().unwrap();

        // The file should now exist and contain a valid hex key.
        assert!(path.exists(), "key file should have been created");
        let contents = std::fs::read_to_string(&path).unwrap();
        let loaded_key = parse_hex_key(&contents).unwrap();

        // The returned key must match what was written.
        assert_eq!(*key, *loaded_key);
    }

    #[test]
    fn file_provider_generate_does_not_overwrite_existing_key() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "{HEX_KEY}").unwrap();

        let provider = FileKeyProvider::new(f.path().to_path_buf());
        // Call generate_or_load_master_key when the file already exists.
        let key = provider.generate_or_load_master_key().unwrap();

        // Should return the original key, not a freshly generated one.
        assert_eq!(*key, [0x42u8; 32]);
    }

    #[test]
    fn file_provider_generate_twice_returns_same_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("master.key");
        let provider = FileKeyProvider::new(path.clone());

        let key1 = provider.generate_or_load_master_key().unwrap();
        let key2 = provider.generate_or_load_master_key().unwrap();

        assert_eq!(*key1, *key2, "second call must return the same stored key");
    }

    #[test]
    fn env_provider_generate_not_supported() {
        let provider = EnvKeyProvider::default();
        let err = provider.generate_or_load_master_key().unwrap_err();
        assert!(
            matches!(err, KeyProviderError::NotAvailable(_)),
            "EnvKeyProvider must return NotAvailable for generate"
        );
    }

    #[cfg(unix)]
    #[test]
    fn file_provider_generate_sets_restrictive_permissions() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("master.key");
        let provider = FileKeyProvider::new(path.clone());
        provider.generate_or_load_master_key().unwrap();

        let meta = std::fs::metadata(&path).unwrap();
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "key file should have mode 0o600, got {mode:#o}");
    }
}
