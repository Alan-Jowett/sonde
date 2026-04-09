// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! IR-3: Physical placement constraints — board outline, zones, routing.

use serde::Deserialize;

use super::HasSchemaVersion;

#[derive(Debug, Deserialize)]
pub struct Ir3 {
    pub schema_version: String,
    pub project: String,
    pub backend: Option<String>,
    pub board: Board,
    pub connector_placement: Vec<ConnectorPlacement>,
    pub component_zones: Vec<ComponentZone>,
    pub keepout_zones: Option<Vec<KeepoutZone>>,
    pub routing_constraints: Option<RoutingConstraints>,
    pub silkscreen: Option<Silkscreen>,
}

impl HasSchemaVersion for Ir3 {
    fn schema_version(&self) -> &str {
        &self.schema_version
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Board {
    pub shape: Option<String>,
    pub width_mm: f64,
    pub height_mm: f64,
    pub area_mm2: Option<f64>,
    pub layers: u32,
    pub copper_weight_oz: Option<u32>,
    pub surface_finish: Option<String>,
    pub origin: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConnectorPlacement {
    pub ref_des: String,
    pub description: Option<String>,
    pub edge: Option<String>,
    pub position: Position,
    pub orientation: Option<String>,
    pub courtyard_mm: Option<super::ir1e::Dimensions>,
    pub mounting: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Position {
    pub x_mm: f64,
    pub y_mm: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ComponentZone {
    pub group: String,
    pub components: Vec<String>,
    pub zone: ZoneSpec,
    pub proximity_constraint_mm: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ZoneSpec {
    pub description: Option<String>,
    pub anchor: Position,
    pub extent_mm: super::ir1e::Dimensions,
}

#[derive(Debug, Clone, Deserialize)]
pub struct KeepoutZone {
    pub name: String,
    pub description: Option<String>,
    pub boundary: KeepoutBoundary,
    pub restriction: Option<String>,
    pub layer: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct KeepoutBoundary {
    #[serde(rename = "type")]
    pub boundary_type: Option<String>,
    pub x_mm: f64,
    pub y_mm: f64,
    pub width_mm: f64,
    pub height_mm: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RoutingConstraints {
    pub power_traces: Option<Vec<PowerTrace>>,
    pub signal_traces: Option<Vec<SignalTrace>>,
    pub via_constraints: Option<ViaConstraints>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PowerTrace {
    pub net: String,
    pub min_width_mm: Option<f64>,
    #[serde(rename = "type")]
    pub trace_type: Option<String>,
    pub layer: Option<String>,
    pub rationale: Option<String>,
    pub ir_pb_reference: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SignalTrace {
    pub nets: Option<Vec<String>>,
    pub net: Option<String>,
    pub width_mm: f64,
    pub max_length_mm: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ViaConstraints {
    pub diameter_mm: f64,
    pub drill_mm: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Silkscreen {
    pub labels: Option<Vec<SilkscreenLabel>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SilkscreenLabel {
    pub text: String,
    pub location: Option<String>,
    pub position: Option<Position>,
    pub layer: Option<String>,
}
