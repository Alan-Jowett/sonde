// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use zeroize::Zeroizing;

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

// BLE message type constants
pub const REQUEST_GW_INFO: u8 = 0x01;
pub const GW_INFO_RESPONSE: u8 = 0x81;
pub const REGISTER_PHONE: u8 = 0x02;
pub const PHONE_REGISTERED: u8 = 0x82;
pub const NODE_PROVISION: u8 = 0x03;
pub const NODE_ACK: u8 = 0x83;
pub const MSG_ERROR: u8 = 0xFE;

/// A BLE device discovered during scanning.
#[derive(Debug, Clone)]
pub struct ScannedDevice {
    pub name: String,
    pub address: [u8; 6],
    pub rssi: i8,
    pub service_uuids: Vec<u128>,
}

/// Gateway identity established during Phase 1 (TOFU).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayIdentity {
    pub public_key: [u8; 32],
    pub gateway_id: [u8; 16],
}

/// Full result of Phase 1 gateway pairing.
#[derive(Clone)]
pub struct PairingArtifacts {
    pub gateway_identity: GatewayIdentity,
    pub phone_psk: Zeroizing<[u8; 32]>,
    pub phone_key_hint: u16,
    pub rf_channel: u8,
}

impl std::fmt::Debug for PairingArtifacts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairingArtifacts")
            .field("gateway_identity", &self.gateway_identity)
            .field("phone_key_hint", &self.phone_key_hint)
            .field("rf_channel", &self.rf_channel)
            .field("phone_psk", &"[REDACTED]")
            .finish()
    }
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
