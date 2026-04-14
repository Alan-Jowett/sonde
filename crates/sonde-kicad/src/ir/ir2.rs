// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! IR-2: Logical circuit description — nets, functional groups, netlist.

use serde::Deserialize;

use super::HasSchemaVersion;

#[derive(Debug, Deserialize)]
pub struct Ir2 {
    pub schema_version: String,
    pub project: String,
    pub nets: Vec<Net>,
    pub functional_groups: Vec<FunctionalGroup>,
    pub netlist: Vec<NetlistEntry>,
}

impl HasSchemaVersion for Ir2 {
    fn schema_version(&self) -> &str {
        &self.schema_version
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Net {
    pub name: String,
    pub description: Option<String>,
    #[serde(rename = "type")]
    pub net_type: String,
    pub power_source: Option<String>,
}

impl Net {
    pub fn is_power(&self) -> bool {
        self.net_type == "power"
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct FunctionalGroup {
    pub name: String,
    pub description: Option<String>,
    pub components: Vec<String>,
    pub signal_flow: Option<String>,
    pub requirements: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NetlistEntry {
    pub ref_des: String,
    pub component: Option<String>,
    pub group: Option<String>,
    pub pins: Vec<PinConnection>,
    pub value: Option<String>,
    pub value_rationale: Option<String>,
    pub value_citation: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PinConnection {
    pub pin: u32,
    pub name: Option<String>,
    pub net: String,
    pub label: Option<String>,
    pub status: Option<String>,
}

impl PinConnection {
    /// Returns true if this pin is not connected.
    pub fn is_nc(&self) -> bool {
        self.net == "NC"
            || self
                .status
                .as_ref()
                .is_some_and(|s| s.contains("NOT CONNECTED") || s.contains("spare"))
    }
}
