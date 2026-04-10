// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! IR (Intermediate Representation) loading and data types.
//!
//! Loads YAML files produced by the sonde-hw-design pipeline.

pub mod ir1;
pub mod ir1e;
pub mod ir2;
pub mod ir3;

use std::path::Path;

use sha2::{Digest, Sha256};

use crate::Error;

pub use ir1::Ir1;
pub use ir1e::Ir1e;
pub use ir2::Ir2;
pub use ir3::Ir3;

const EXPECTED_SCHEMA_VERSION: &str = "1.0.0";

/// Bundle of all loaded IR files.
pub struct IrBundle {
    pub ir1: Option<Ir1>,
    pub ir1e: Ir1e,
    pub ir2: Ir2,
    pub ir3: Option<Ir3>,
    pub project: String,
}

/// Load all IR files from a directory.
///
/// Always requires IR-1e and IR-2. Other IR files are loaded if present.
pub fn load_ir(dir: &Path) -> Result<IrBundle, Error> {
    let ir1e: Ir1e = load_required(dir, "IR-1e.yaml")?;
    let ir2: Ir2 = load_required(dir, "IR-2.yaml")?;
    let ir1: Option<Ir1> = load_optional(dir, "IR-1.yaml")?;
    let ir3: Option<Ir3> = load_optional(dir, "IR-3.yaml")?;

    let project = ir1e.project.clone();

    Ok(IrBundle {
        ir1,
        ir1e,
        ir2,
        ir3,
        project,
    })
}

/// Compute a SHA-256 hash of all IR YAML files in the directory.
///
/// Files are sorted by name before hashing for determinism.
pub fn compute_ir_hash(dir: &Path) -> Result<[u8; 32], Error> {
    let mut files: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .is_some_and(|ext| ext == "yaml" || ext == "yml")
        })
        .collect();
    files.sort_by_key(|e| e.file_name());

    let mut hasher = Sha256::new();
    for entry in &files {
        hasher.update(std::fs::read(entry.path())?);
    }
    Ok(hasher.finalize().into())
}

fn load_required<T: serde::de::DeserializeOwned + HasSchemaVersion>(
    dir: &Path,
    filename: &str,
) -> Result<T, Error> {
    let path = dir.join(filename);
    if !path.exists() {
        return Err(Error::MissingIrFile(filename.to_string()));
    }
    let content = std::fs::read_to_string(&path).map_err(Error::Io)?;
    let value: T = serde_yaml::from_str(&content).map_err(|source| Error::YamlParse {
        file: filename.to_string(),
        source,
    })?;
    validate_schema_version(filename, value.schema_version())?;
    Ok(value)
}

fn load_optional<T: serde::de::DeserializeOwned + HasSchemaVersion>(
    dir: &Path,
    filename: &str,
) -> Result<Option<T>, Error> {
    let path = dir.join(filename);
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path).map_err(Error::Io)?;
    let value: T = serde_yaml::from_str(&content).map_err(|source| Error::YamlParse {
        file: filename.to_string(),
        source,
    })?;
    validate_schema_version(filename, value.schema_version())?;
    Ok(Some(value))
}

fn validate_schema_version(filename: &str, version: &str) -> Result<(), Error> {
    if version != EXPECTED_SCHEMA_VERSION {
        return Err(Error::SchemaVersion {
            file: filename.to_string(),
            found: version.to_string(),
            expected: EXPECTED_SCHEMA_VERSION.to_string(),
        });
    }
    Ok(())
}

/// Trait for IR types that carry a schema version.
pub trait HasSchemaVersion {
    fn schema_version(&self) -> &str;
}
