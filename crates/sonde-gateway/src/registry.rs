// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use std::time::SystemTime;

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
    pub last_battery_mv: Option<u32>,
    pub last_seen: Option<SystemTime>,
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
            last_battery_mv: None,
            last_seen: None,
        }
    }

    /// Update battery and ABI fields (called on each WAKE).
    pub fn update_telemetry(&mut self, battery_mv: u32, firmware_abi_version: u32) {
        self.last_battery_mv = Some(battery_mv);
        self.firmware_abi_version = Some(firmware_abi_version);
        self.last_seen = Some(SystemTime::now());
    }

    /// Mark the node's current program hash (called on PROGRAM_ACK).
    pub fn confirm_program(&mut self, program_hash: Vec<u8>) {
        self.current_program_hash = Some(program_hash);
    }
}
