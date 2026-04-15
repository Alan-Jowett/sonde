// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use crate::types::{NodeAckStatus, PairingMethod};
use std::fmt;

/// Format a 6-byte BLE device address as `"AA:BB:CC:DD:EE:FF"`.
pub fn format_device_address(addr: &[u8; 6]) -> String {
    format!(
        "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        addr[0], addr[1], addr[2], addr[3], addr[4], addr[5]
    )
}

/// Display helper for `Option<String>` device addresses in error messages.
///
/// Renders `Some("AA:BB:CC:DD:EE:FF")` as `"AA:BB:CC:DD:EE:FF"` and
/// `None` as `"(unknown device)"`.
struct OptionalDevice<'a>(&'a Option<String>);

impl fmt::Display for OptionalDevice<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(addr) => f.write_str(addr),
            None => f.write_str("(unknown device)"),
        }
    }
}

/// Errors that can occur during BLE pairing.
#[derive(Debug, thiserror::Error)]
pub enum PairingError {
    // Device errors
    #[error(
        "no Bluetooth adapter found — check that BLE hardware is present and drivers are installed"
    )]
    AdapterNotFound,

    #[error("Bluetooth is disabled — enable Bluetooth in system settings and retry")]
    BluetoothDisabled,

    #[error(
        "target device {device} not found during scan — check that the modem is powered on and in range"
    )]
    DeviceNotFound { device: String },

    #[error(
        "target device {} is out of BLE range — move closer and retry",
        OptionalDevice(device)
    )]
    DeviceOutOfRange { device: Option<String> },

    // Transport errors (PT-1215: include device context)
    #[error("BLE connection failed ({}): {reason} — check that the modem is powered on and not paired to another device", OptionalDevice(device))]
    ConnectionFailed {
        device: Option<String>,
        reason: String,
    },

    #[error("BLE connection to {} dropped unexpectedly — check that the modem is powered on and in range; if this persists, delete the stale Bluetooth pairing in OS settings and retry", OptionalDevice(device))]
    ConnectionDropped { device: Option<String> },

    #[error("negotiated MTU {negotiated} for {device} is below required minimum {required} — the BLE adapter or modem firmware may need updating")]
    MtuTooLow {
        device: String,
        negotiated: u16,
        required: u16,
    },

    #[error("{operation} on {} timed out after {duration_secs}s — check that the modem is powered on and in range", OptionalDevice(device))]
    Timeout {
        device: Option<String>,
        operation: &'static str,
        duration_secs: u64,
    },

    #[error(
        "GATT write to {} failed: {reason} — check the BLE connection and retry",
        OptionalDevice(device)
    )]
    GattWriteFailed {
        device: Option<String>,
        reason: String,
    },

    #[error(
        "GATT read from {} failed: {reason} — check the BLE connection and retry",
        OptionalDevice(device)
    )]
    GattReadFailed {
        device: Option<String>,
        reason: String,
    },

    #[error(
        "indication from {} not received before timeout — check that the modem is powered on and in range", OptionalDevice(device)
    )]
    IndicationTimeout { device: Option<String> },

    // Protocol errors
    #[error("registration failed: {0} — verify the gateway is running and the registration window is open")]
    RegistrationFailed(String),

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

    #[error("invalid pin config: {0}")]
    InvalidPinConfig(String),

    #[error("invalid RF channel {0}: must be 1-13")]
    InvalidRfChannel(u8),

    #[error("invalid key hint")]
    InvalidKeyHint,

    // Crypto errors
    #[error("RNG failed: {0}")]
    RngFailed(String),

    #[error("encryption failed: {0}")]
    EncryptionFailed(String),

    #[error("diagnostic failed: {0}")]
    DiagnosticFailed(String),

    #[error("BLE pairing used insecure method `{method}` — Numeric Comparison (LESC) is required")]
    InsecurePairingMethod { method: PairingMethod },

    #[error("invalid argument: {reason}")]
    InvalidArgument { reason: String },

    // Scan errors
    #[error("scan is already active")]
    ScanAlreadyActive,

    #[error("system clock error — check that the system time is set correctly")]
    TimestampUnavailable,

    // Platform / JNI errors (Android)
    #[cfg(feature = "android")]
    #[error("JNI error: {0}")]
    JniError(String),
}

#[cfg(feature = "android")]
impl From<jni::errors::Error> for PairingError {
    fn from(e: jni::errors::Error) -> Self {
        PairingError::JniError(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_device_address_canonical() {
        assert_eq!(
            format_device_address(&[0x00, 0x0A, 0xFF, 0x10, 0x0B, 0xAC]),
            "00:0A:FF:10:0B:AC"
        );
    }

    #[test]
    fn connection_dropped_includes_device_and_stale_hint() {
        let err = PairingError::ConnectionDropped {
            device: Some("AA:BB:CC:DD:EE:FF".into()),
        };
        let msg = err.to_string();
        assert!(msg.contains("AA:BB:CC:DD:EE:FF"), "missing device: {msg}");
        assert!(
            msg.contains("stale Bluetooth pairing"),
            "missing stale pairing hint: {msg}"
        );
    }

    #[test]
    fn mtu_too_low_includes_device() {
        let err = PairingError::MtuTooLow {
            device: "11:22:33:44:55:66".into(),
            negotiated: 100,
            required: 247,
        };
        let msg = err.to_string();
        assert!(msg.contains("11:22:33:44:55:66"), "missing device: {msg}");
        assert!(msg.contains("100"), "missing negotiated: {msg}");
        assert!(msg.contains("247"), "missing required: {msg}");
    }

    #[test]
    fn indication_timeout_includes_device() {
        let err = PairingError::IndicationTimeout {
            device: Some("AA:BB:CC:DD:EE:FF".into()),
        };
        let msg = err.to_string();
        assert!(msg.contains("AA:BB:CC:DD:EE:FF"), "missing device: {msg}");
    }

    #[test]
    fn optional_device_none_renders_unknown() {
        let err = PairingError::ConnectionDropped { device: None };
        let msg = err.to_string();
        assert!(
            msg.contains("(unknown device)"),
            "missing fallback text: {msg}"
        );
    }

    #[test]
    fn device_not_found_includes_device() {
        let err = PairingError::DeviceNotFound {
            device: "AA:BB:CC:DD:EE:FF".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("AA:BB:CC:DD:EE:FF"), "missing device: {msg}");
    }

    #[test]
    fn connection_failed_includes_device_and_reason() {
        let err = PairingError::ConnectionFailed {
            device: Some("AA:BB:CC:DD:EE:FF".into()),
            reason: "connect failed: timeout".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("AA:BB:CC:DD:EE:FF"), "missing device: {msg}");
        assert!(
            msg.contains("connect failed: timeout"),
            "missing reason: {msg}"
        );
    }

    #[test]
    fn gatt_write_failed_includes_device() {
        let err = PairingError::GattWriteFailed {
            device: Some("AA:BB:CC:DD:EE:FF".into()),
            reason: "auth required".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("AA:BB:CC:DD:EE:FF"), "missing device: {msg}");
        assert!(msg.contains("auth required"), "missing reason: {msg}");
    }
}
