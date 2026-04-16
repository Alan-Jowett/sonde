// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Error types for the sonde-kicad crate.

/// Errors that can occur during IR loading, generation, or export.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("YAML parse error in `{file}`: {source}")]
    YamlParse {
        file: String,
        source: serde_yaml_ng::Error,
    },

    #[error("missing required IR file: `{0}`")]
    MissingIrFile(String),

    #[error("unsupported schema version in `{file}`: found `{found}`, expected `{expected}`")]
    SchemaVersion {
        file: String,
        found: String,
        expected: String,
    },

    #[error("IR cross-validation error: {0}")]
    CrossValidation(String),

    #[error("missing symbol definition: `{0}`")]
    MissingSymbol(String),

    #[error("missing footprint definition: `{0}`")]
    MissingFootprint(String),

    #[error("SES parse error: {0}")]
    SesParse(String),

    #[error("SES import error: {0}")]
    Ses(String),

    #[error("placement error: {0}")]
    Placement(String),

    #[error("kicad-cli not found: {0}")]
    KicadCliNotFound(String),

    #[error("kicad-cli failed: {0}")]
    KicadCliFailed(String),
}
