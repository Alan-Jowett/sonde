// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! IR-1e: Enriched component bill with KiCad symbol/footprint mappings.

use serde::Deserialize;

use super::HasSchemaVersion;

#[derive(Debug, Deserialize)]
pub struct Ir1e {
    pub schema_version: String,
    pub project: String,
    pub backend: String,
    pub components: Vec<Ir1eComponent>,
}

impl HasSchemaVersion for Ir1e {
    fn schema_version(&self) -> &str {
        &self.schema_version
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Ir1eComponent {
    pub ref_des: String,
    pub ir1_generic_footprint: Option<String>,
    pub kicad_symbol: String,
    pub kicad_footprint: String,
    pub library_status: String,
    pub bbox_mm: Option<Dimensions>,
    pub courtyard_mm: Option<Dimensions>,
    pub courtyard_area_mm2: Option<f64>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Dimensions {
    pub width: f64,
    pub height: f64,
}
