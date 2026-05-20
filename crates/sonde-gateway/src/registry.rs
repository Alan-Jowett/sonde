// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

/// Sensor descriptor for a node's attached peripherals.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SensorDescriptor {
    /// Sensor bus type: 1=I2C, 2=ADC, 3=GPIO, 4=SPI.
    #[serde(rename = "t")]
    pub sensor_type: u8,
    /// Bus-specific address or channel (e.g., I2C address, ADC channel).
    #[serde(rename = "i")]
    pub sensor_id: u8,
    /// Optional human-readable label (max 64 bytes UTF-8).
    #[serde(rename = "l", skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Node record. The `node_id` is an admin-assigned opaque identifier used to
/// correlate a node across sessions and handler API calls.
#[derive(Debug, Clone)]
pub struct NodeRecord {
    pub node_id: String,
    pub key_hint: u16,
    pub psk: [u8; 32],
    pub assigned_program_hash: Option<Vec<u8>>,
    pub current_program_hash: Option<Vec<u8>>,
    /// Connector/admin desired schedule target. `None` means no schedule target
    /// is currently desired, while `schedule_interval_s` retains the latest
    /// persisted interval value for compatibility with existing runtime logic.
    pub desired_schedule_interval_s: Option<u32>,
    pub schedule_interval_s: u32,
    pub firmware_abi_version: Option<u32>,
    pub firmware_version: Option<String>,
    /// RF channel the node operates on (1–13). Set during BLE pairing.
    pub rf_channel: Option<u8>,
    /// Attached sensor descriptors. Set during BLE pairing.
    pub sensors: Vec<SensorDescriptor>,
    /// Phone ID that registered this node (audit trail). Set during BLE pairing.
    pub registered_by_phone_id: Option<u32>,
    /// Master key version used to encrypt this node's PSK (GW-2005).
    pub key_version: u64,
}

impl NodeRecord {
    /// Create a new node record with sensible defaults.
    pub fn new(node_id: String, key_hint: u16, psk: [u8; 32]) -> Self {
        Self {
            node_id,
            key_hint,
            psk,
            assigned_program_hash: None,
            current_program_hash: None,
            desired_schedule_interval_s: Some(60),
            schedule_interval_s: 60,
            firmware_abi_version: None,
            firmware_version: None,
            rf_channel: None,
            sensors: Vec::new(),
            registered_by_phone_id: None,
            key_version: 0,
        }
    }

    /// Update durable firmware metadata on a cloned `NodeRecord`.
    ///
    /// Runtime battery and last-seen data are tracked separately by
    /// `SessionManager`; only firmware metadata lives on the record.
    pub fn update_firmware_metadata(
        &mut self,
        firmware_abi_version: u32,
        firmware_version: String,
    ) {
        self.firmware_abi_version = Some(firmware_abi_version);
        self.firmware_version = Some(firmware_version);
    }

    /// Mark the node's current program hash (called on PROGRAM_ACK).
    pub fn confirm_program(&mut self, program_hash: Vec<u8>) {
        self.current_program_hash = Some(program_hash);
    }
}
