// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use std::time::SystemTime;

/// A timestamped battery voltage reading.
#[derive(Debug, Clone, PartialEq)]
pub struct BatteryReading {
    /// When the reading was taken.
    pub timestamp: SystemTime,
    /// Battery voltage in millivolts.
    pub battery_mv: u32,
}

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
/// correlate a node across sessions and handler API calls. The `last_seen` and
/// `last_battery_mv` fields are runtime-only overlays and are not persisted by
/// durable storage backends.
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
    /// Most recent WAKE battery reading observed by the current gateway process.
    /// Durable storage backends intentionally do not persist this field.
    pub last_battery_mv: Option<u32>,
    pub last_seen: Option<SystemTime>,
    /// RF channel the node operates on (1–13). Set during BLE pairing.
    pub rf_channel: Option<u8>,
    /// Attached sensor descriptors. Set during BLE pairing.
    pub sensors: Vec<SensorDescriptor>,
    /// Phone ID that registered this node (audit trail). Set during BLE pairing.
    pub registered_by_phone_id: Option<u32>,
    /// Historical battery voltage readings retained only for struct/schema
    /// compatibility with legacy battery-persistence code paths. New gateway
    /// code does not populate or append to this list.
    pub battery_history: Vec<BatteryReading>,
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
            last_battery_mv: None,
            last_seen: None,
            rf_channel: None,
            sensors: Vec::new(),
            registered_by_phone_id: None,
            battery_history: Vec::new(),
            key_version: 0,
        }
    }

    /// Update the in-memory WAKE overlay on a `NodeRecord`.
    ///
    /// This caches the runtime-only battery reading plus the latest firmware
    /// metadata in a cloned record used by admin/connector shaping. Durable
    /// persistence of firmware metadata is handled separately by storage.
    pub fn update_telemetry(
        &mut self,
        battery_mv: u32,
        firmware_abi_version: u32,
        firmware_version: String,
    ) {
        self.last_battery_mv = Some(battery_mv);
        self.firmware_abi_version = Some(firmware_abi_version);
        self.firmware_version = Some(firmware_version);
    }

    /// Mark the node's current program hash (called on PROGRAM_ACK).
    pub fn confirm_program(&mut self, program_hash: Vec<u8>) {
        self.current_program_hash = Some(program_hash);
    }
}
