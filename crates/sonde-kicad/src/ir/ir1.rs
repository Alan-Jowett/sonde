// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! IR-1: Component bill (EDA-agnostic).

use serde::Deserialize;
use serde::Deserializer;

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
    #[serde(deserialize_with = "deserialize_ref_des")]
    pub ref_des: Vec<String>,
    pub description: Option<String>,
    pub manufacturer: Option<String>,
    pub part_number: Option<String>,
    pub package: Option<String>,
    pub generic_footprint: Option<String>,
    pub sourcing: Option<Sourcing>,
}

impl Ir1Component {
    pub fn contains_ref_des(&self, ref_des: &str) -> bool {
        self.ref_des.iter().any(|candidate| candidate == ref_des)
    }
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

fn deserialize_ref_des<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum RefDes {
        One(String),
        Many(Vec<String>),
    }

    match RefDes::deserialize(deserializer)? {
        RefDes::One(ref_des) => Ok(vec![ref_des]),
        RefDes::Many(ref_des) => Ok(ref_des),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_scalar_ref_des() {
        let input = r#"
schema_version: "1.0.0"
project: demo
components:
  - ref_des: U1
"#;

        let ir1: Ir1 = serde_yaml_ng::from_str(input).unwrap();
        assert_eq!(ir1.components[0].ref_des, vec!["U1"]);
    }

    #[test]
    fn parses_grouped_ref_des() {
        let input = r#"
schema_version: "1.0.0"
project: demo
components:
  - ref_des: [R1, R2]
"#;

        let ir1: Ir1 = serde_yaml_ng::from_str(input).unwrap();
        assert_eq!(ir1.components[0].ref_des, vec!["R1", "R2"]);
        assert!(ir1.components[0].contains_ref_des("R2"));
    }
}
