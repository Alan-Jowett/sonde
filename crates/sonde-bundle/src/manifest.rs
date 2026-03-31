// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Manifest types and YAML parsing for Sonde App Bundles.

use serde::{Deserialize, Serialize};

/// Parsed `app.yaml` manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub schema_version: u32,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: Option<String>,
    pub programs: Vec<ProgramEntry>,
    pub nodes: Vec<NodeTarget>,
    #[serde(default)]
    pub handlers: Vec<HandlerEntry>,
}

/// A BPF program included in the bundle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgramEntry {
    pub name: String,
    pub path: String,
    pub profile: VerificationProfile,
}

/// Verification profile for a BPF program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VerificationProfile {
    Resident,
    Ephemeral,
}

/// A handler process definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandlerEntry {
    pub program: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub working_dir: Option<String>,
    #[serde(default)]
    pub reply_timeout_ms: Option<u32>,
}

/// A node target with optional hardware profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeTarget {
    pub name: String,
    pub program: String,
    #[serde(default)]
    pub hardware: Option<HardwareProfile>,
}

/// Hardware profile describing physical sensors on a node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareProfile {
    #[serde(default)]
    pub sensors: Vec<SensorDescriptor>,
    #[serde(default)]
    pub rf_channel: Option<u8>,
}

/// A sensor attached to a node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensorDescriptor {
    #[serde(rename = "type")]
    pub sensor_type: SensorType,
    pub id: u16,
    #[serde(default)]
    pub label: Option<String>,
}

/// Sensor bus type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SensorType {
    I2c,
    Adc,
    Gpio,
    Spi,
}

impl std::fmt::Display for VerificationProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VerificationProfile::Resident => write!(f, "resident"),
            VerificationProfile::Ephemeral => write!(f, "ephemeral"),
        }
    }
}

impl std::fmt::Display for SensorType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SensorType::I2c => write!(f, "i2c"),
            SensorType::Adc => write!(f, "adc"),
            SensorType::Gpio => write!(f, "gpio"),
            SensorType::Spi => write!(f, "spi"),
        }
    }
}

impl Manifest {
    /// Parse a manifest from a YAML string.
    pub fn from_yaml(yaml: &str) -> Result<Self, crate::error::BundleError> {
        serde_yaml_ng::from_str(yaml).map_err(|e| crate::error::BundleError::Yaml(e.to_string()))
    }

    /// Serialize the manifest to a YAML string.
    pub fn to_yaml(&self) -> Result<String, crate::error::BundleError> {
        serde_yaml_ng::to_string(self).map_err(|e| crate::error::BundleError::Yaml(e.to_string()))
    }
}
