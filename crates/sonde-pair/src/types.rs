// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

// TODO: These 0xFE** short UUIDs are provisional placeholders (see ble-pairing-protocol.md §3.1).
// They MUST be replaced with randomly-generated vendor-specific 128-bit UUIDs before v1.0 release.

/// BLE service UUID advertised by gateways in pairing mode.
pub const GATEWAY_SERVICE_UUID: u128 = 0x0000FE60_0000_1000_8000_00805F9B34FB;

/// BLE characteristic UUID for gateway command writes/indications.
pub const GATEWAY_COMMAND_UUID: u128 = 0x0000FE61_0000_1000_8000_00805F9B34FB;

/// BLE service UUID advertised by nodes in pairing mode.
pub const NODE_SERVICE_UUID: u128 = 0x0000FE50_0000_1000_8000_00805F9B34FB;

/// BLE characteristic UUID for node command writes/indications.
pub const NODE_COMMAND_UUID: u128 = 0x0000FE51_0000_1000_8000_00805F9B34FB;

/// Minimum BLE MTU required for pairing messages.
pub const BLE_MTU_MIN: u16 = 247;

/// Maximum encrypted payload length for NODE_PROVISION peer messages.
///
/// Constrained by the 250-byte ESP-NOW frame budget on the node side.
/// The node firmware enforces this as `PEER_PAYLOAD_MAX_LEN`.
/// See `ble-pairing-protocol.md §11.1` for the full byte-budget derivation.
pub const PEER_PAYLOAD_MAX_LEN: usize = 202;

// BLE message type constants
pub const REQUEST_GW_INFO: u8 = 0x01;
pub const GW_INFO_RESPONSE: u8 = 0x81;
pub const REGISTER_PHONE: u8 = 0x02;
pub const PHONE_REGISTERED: u8 = 0x82;
pub const NODE_PROVISION: u8 = 0x01;
pub const NODE_ACK: u8 = 0x81;
pub const MSG_ERROR: u8 = 0xFF;

/// A BLE device discovered during scanning.
#[derive(Debug, Clone)]
pub struct ScannedDevice {
    pub name: String,
    pub address: [u8; 6],
    pub rssi: i8,
    pub service_uuids: Vec<u128>,
}

/// Result of Phase 2 node provisioning.
#[derive(Debug, Clone)]
pub struct NodeProvisionResult {
    pub status: NodeAckStatus,
}

/// Status codes from node provisioning acknowledgement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeAckStatus {
    Success,
    AlreadyPaired,
    StorageError,
    Unknown(u8),
}

impl NodeAckStatus {
    pub fn from_byte(b: u8) -> Self {
        match b {
            0x00 => Self::Success,
            0x01 => Self::AlreadyPaired,
            0x02 => Self::StorageError,
            other => Self::Unknown(other),
        }
    }

    pub fn to_byte(self) -> u8 {
        match self {
            Self::Success => 0x00,
            Self::AlreadyPaired => 0x01,
            Self::StorageError => 0x02,
            Self::Unknown(b) => b,
        }
    }
}

impl std::fmt::Display for NodeAckStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Success => write!(f, "success"),
            Self::AlreadyPaired => write!(f, "node already paired"),
            Self::StorageError => write!(f, "node storage error"),
            Self::Unknown(b) => write!(f, "unknown status (0x{b:02x})"),
        }
    }
}

/// BLE pairing method negotiated during connection (PT-0904).
///
/// The transport exposes this after a successful connection so application
/// logic can reject insecure methods before exchanging key material.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PairingMethod {
    /// LE Secure Connections (LESC) Numeric Comparison — required.
    NumericComparison,
    /// Just Works — insecure, must be rejected (PT-0904).
    JustWorks,
    /// Unknown — the transport cannot observe the pairing method.
    /// Must be rejected by `enforce_lesc()` per PT-0904.
    Unknown,
}

impl std::fmt::Display for PairingMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NumericComparison => write!(f, "Numeric Comparison"),
            Self::JustWorks => write!(f, "Just Works"),
            Self::Unknown => write!(f, "Unknown"),
        }
    }
}

/// Descriptor for a sensor attached to a node.
///
/// Encoded as a CBOR map: `{1: sensor_type, 2: sensor_id, 3: label?}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SensorDescriptor {
    /// Sensor bus type: 1=I²C, 2=ADC, 3=GPIO, 4=SPI.
    pub sensor_type: u8,
    /// Bus address or pin number (0–255).
    pub sensor_id: u8,
    /// Optional human-readable label (max 64 bytes UTF-8).
    pub label: Option<String>,
}

pub use sonde_protocol::BoardLayout;
