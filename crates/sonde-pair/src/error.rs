// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use crate::types::NodeAckStatus;

/// Errors that can occur during BLE pairing.
#[derive(Debug, thiserror::Error)]
pub enum PairingError {
    // Device errors
    #[error("no Bluetooth adapter found — check that BLE hardware is present and drivers are installed")]
    AdapterNotFound,

    #[error("Bluetooth is disabled — enable Bluetooth in system settings and retry")]
    BluetoothDisabled,

    #[error("target device not found during scan — check that the modem is powered on and in range")]
    DeviceNotFound,

    #[error("target device is out of BLE range — move closer and retry")]
    DeviceOutOfRange,

    // Transport errors
    #[error("BLE connection failed: {0} — check that the modem is powered on and not paired to another device")]
    ConnectionFailed(String),

    #[error("BLE connection dropped unexpectedly — check that the modem is powered on and in range, then retry")]
    ConnectionDropped,

    #[error("negotiated MTU {negotiated} is below required minimum {required} — the BLE adapter or modem firmware may need updating")]
    MtuTooLow { negotiated: u16, required: u16 },

    #[error("{operation} timed out after {duration_secs}s — check that the modem is powered on and in range")]
    Timeout {
        operation: &'static str,
        duration_secs: u64,
    },

    #[error("GATT write failed: {0} — check the BLE connection and retry")]
    GattWriteFailed(String),

    #[error("GATT read failed: {0} — check the BLE connection and retry")]
    GattReadFailed(String),

    #[error("indication not received before timeout — check that the modem is powered on and in range")]
    IndicationTimeout,

    // Protocol errors
    #[error("gateway authentication failed: {0} — verify the gateway is running and the registration window is open")]
    GatewayAuthFailed(String),

    #[error("Ed25519 signature verification failed — the gateway identity may have changed or data was corrupted")]
    SignatureVerificationFailed,

    #[error("gateway public key does not match stored identity (TOFU violation) — if the gateway was re-keyed, clear local pairing first")]
    PublicKeyMismatch,

    #[error("gateway registration window is closed — open the registration window on the gateway and retry")]
    RegistrationWindowClosed,

    #[error("AES-GCM decryption failed — wrong key or corrupted ciphertext")]
    DecryptionFailed,

    #[error("invalid response: msg_type=0x{msg_type:02x}, {reason}")]
    InvalidResponse { msg_type: u8, reason: String },

    #[error("CBOR decode failed: {0}")]
    CborDecodeFailed(String),

    #[error("CBOR encode failed: {0}")]
    CborEncodeFailed(String),

    #[error("node provisioning failed: {0}")]
    NodeProvisionFailed(NodeAckStatus),

    #[error("node error response: status=0x{status:02x}, {message}")]
    NodeErrorResponse { status: u8, message: String },

    #[error("already paired with this gateway")]
    GatewayAlreadyPaired,

    #[error("payload too large: {size} bytes exceeds {max}-byte limit")]
    PayloadTooLarge { size: usize, max: usize },

    #[error("invalid phone label: {0}")]
    InvalidPhoneLabel(String),

    // Store errors
    #[error("failed to save pairing artifacts: {0}")]
    StoreSaveFailed(String),

    #[error("failed to load pairing artifacts: {0}")]
    StoreLoadFailed(String),

    #[error("pairing store corrupted: {0}")]
    StoreCorrupted(String),

    // Validation errors
    #[error("invalid node ID: {0}")]
    InvalidNodeId(String),

    #[error("invalid RF channel {0}: must be 1-13")]
    InvalidRfChannel(u8),

    #[error("invalid key hint")]
    InvalidKeyHint,

    // Crypto errors
    #[error("RNG failed: {0}")]
    RngFailed(String),

    #[error("invalid public key: {0}")]
    InvalidPublicKey(String),

    #[error("encryption failed: {0}")]
    EncryptionFailed(String),

    #[error("not paired — run Phase 1 (gateway pairing) first before provisioning nodes")]
    NotPaired,

    // Scan errors
    #[error("scan is already active")]
    ScanAlreadyActive,
}
