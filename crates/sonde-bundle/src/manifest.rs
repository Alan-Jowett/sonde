// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Manifest types and YAML parsing for Sonde App Bundles.

use serde::{Deserialize, Serialize};

/// Parsed `app.yaml` manifest.
///
/// Most manifest fields use `#[serde(default)]` so that missing required
/// fields produce validation errors (via `validate_manifest`) instead of
/// opaque YAML parse failures.  Some nested types (e.g. sensor descriptors)
/// intentionally rely on serde's required-field behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    #[serde(default)]
    pub schema_version: Option<u32>,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub programs: Vec<ProgramEntry>,
    #[serde(default)]
    pub nodes: Vec<NodeTarget>,
    #[serde(default)]
    pub handlers: Vec<HandlerEntry>,
}

/// A BPF program included in the bundle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgramEntry {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub profile: VerificationProfile,
}

/// Verification profile for a BPF program.
///
/// Unknown values are accepted at parse time and caught by validation,
/// allowing error collection across the entire manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerificationProfile {
    Resident,
    Ephemeral,
    /// Unrecognised profile string — reported as a validation error.
    Unknown(String),
}

impl Default for VerificationProfile {
    fn default() -> Self {
        VerificationProfile::Unknown(String::new())
    }
}

impl Serialize for VerificationProfile {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for VerificationProfile {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(match s.as_str() {
            "resident" => VerificationProfile::Resident,
            "ephemeral" => VerificationProfile::Ephemeral,
            _ => VerificationProfile::Unknown(s),
        })
    }
}

/// A handler process definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandlerEntry {
    #[serde(default)]
    pub program: String,
    #[serde(default)]
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
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub program: String,
    #[serde(default)]
    pub hardware: Option<HardwareProfile>,
}

/// Hardware profile describing physical sensors and pin assignments on a node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareProfile {
    #[serde(default)]
    pub sensors: Vec<SensorDescriptor>,
    #[serde(default)]
    pub rf_channel: Option<u8>,
    /// GPIO pin mapping for the node's bus peripherals.
    ///
    /// Required when any sensor has `type: "i2c"`.  The pairing tool
    /// provisions these values to the node via NODE_PROVISION (PT-1214,
    /// ND-0608) so a single firmware binary works across boards.
    #[serde(default)]
    pub pins: Option<PinMap>,
}

/// GPIO pin mapping for a node's bus peripherals.
///
/// All fields are required when the struct is present.  GPIO numbers
/// must be in the ESP32-C3 range (0–21) and `i2c0_sda` must differ
/// from `i2c0_scl`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinMap {
    /// I2C bus 0 SDA GPIO number (0–21).
    pub i2c0_sda: u8,
    /// I2C bus 0 SCL GPIO number (0–21).
    pub i2c0_scl: u8,
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
///
/// Unknown values are accepted at parse time and caught by validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SensorType {
    I2c,
    Adc,
    Gpio,
    Spi,
    /// Unrecognised sensor type — reported as a validation error.
    Unknown(String),
}

impl Serialize for SensorType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for SensorType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(match s.as_str() {
            "i2c" => SensorType::I2c,
            "adc" => SensorType::Adc,
            "gpio" => SensorType::Gpio,
            "spi" => SensorType::Spi,
            _ => SensorType::Unknown(s),
        })
    }
}

impl std::fmt::Display for VerificationProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VerificationProfile::Resident => write!(f, "resident"),
            VerificationProfile::Ephemeral => write!(f, "ephemeral"),
            VerificationProfile::Unknown(s) => write!(f, "{s}"),
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
            SensorType::Unknown(s) => write!(f, "{s}"),
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
