// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! IR-1: Component bill (EDA-agnostic).

use serde::Deserialize;

use super::HasSchemaVersion;

#[derive(Debug, Deserialize)]
pub struct Ir1 {
    pub schema_version: String,
    pub project: String,
    pub components: Vec<Ir1Component>,
}

impl HasSchemaVersion for Ir1 {
    fn schema_version(&self) -> &str {
        &self.schema_version
    }
}

#[derive(Debug, Deserialize)]
pub struct Ir1Component {
    pub ref_des: String,
    pub description: Option<String>,
    pub manufacturer: Option<String>,
    pub part_number: Option<String>,
    pub package: Option<String>,
    pub generic_footprint: Option<String>,
    pub sourcing: Option<Sourcing>,
}

#[derive(Debug, Deserialize)]
pub struct Sourcing {
    pub lcsc_pn: Option<String>,
    pub unit_price_usd_qty100: Option<f64>,
    pub stock_units: Option<u64>,
    pub lifecycle: Option<String>,
    pub date_verified: Option<String>,
    pub verification_label: Option<String>,
}
