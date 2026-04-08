// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use std::time::SystemTime;

/// Maximum number of battery readings to retain per node (GW-0702 AC2).
const MAX_BATTERY_HISTORY: usize = 100;

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

/// Persisted node record. The `node_id` is an admin-assigned opaque
/// identifier used to correlate a node across sessions and handler API calls.
#[derive(Debug, Clone)]
pub struct NodeRecord {
    pub node_id: String,
    pub key_hint: u16,
    pub psk: [u8; 32],
    pub assigned_program_hash: Option<Vec<u8>>,
    pub current_program_hash: Option<Vec<u8>>,
    pub schedule_interval_s: u32,
    pub firmware_abi_version: Option<u32>,
    pub firmware_version: Option<String>,
    pub last_battery_mv: Option<u32>,
    pub last_seen: Option<SystemTime>,
    /// RF channel the node operates on (1–13). Set during BLE pairing.
    pub rf_channel: Option<u8>,
    /// Attached sensor descriptors. Set during BLE pairing.
    pub sensors: Vec<SensorDescriptor>,
    /// Phone ID that registered this node (audit trail). Set during BLE pairing.
    pub registered_by_phone_id: Option<u32>,
    /// Historical battery voltage readings (GW-0702 AC2). Most recent last.
    pub battery_history: Vec<BatteryReading>,
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
            schedule_interval_s: 60,
            firmware_abi_version: None,
            firmware_version: None,
            last_battery_mv: None,
            last_seen: None,
            rf_channel: None,
            sensors: Vec::new(),
            registered_by_phone_id: None,
            battery_history: Vec::new(),
        }
    }

    /// Update battery, ABI, and firmware version fields (called on each WAKE).
    pub fn update_telemetry(
        &mut self,
        battery_mv: u32,
        firmware_abi_version: u32,
        firmware_version: &str,
    ) {
        self.last_battery_mv = Some(battery_mv);
        self.firmware_abi_version = Some(firmware_abi_version);
        self.firmware_version = Some(firmware_version.to_string());
        let now = SystemTime::now();
        self.last_seen = Some(now);

        // GW-0702 AC2: maintain battery history, capped at MAX_BATTERY_HISTORY.
        self.battery_history.push(BatteryReading {
            timestamp: now,
            battery_mv,
        });
        if self.battery_history.len() > MAX_BATTERY_HISTORY {
            let excess = self.battery_history.len() - MAX_BATTERY_HISTORY;
            self.battery_history.drain(..excess);
        }
    }

    /// Mark the node's current program hash (called on PROGRAM_ACK).
    pub fn confirm_program(&mut self, program_hash: Vec<u8>) {
        self.current_program_hash = Some(program_hash);
    }
}
