// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Error types for bundle operations.

use crate::validate::ValidationResult;
use std::fmt;

/// Errors from bundle operations.
#[derive(Debug)]
pub enum BundleError {
    /// I/O error reading or writing files.
    Io(std::io::Error),
    /// YAML parse error.
    Yaml(String),
    /// Archive is not valid gzip/tar.
    InvalidArchive(String),
    /// Path traversal detected in archive entry.
    PathTraversal(String),
    /// Symlink detected in archive entry.
    SymlinkNotAllowed(String),
    /// Manifest is missing from archive.
    MissingManifest,
    /// Bundle validation failed.
    ValidationFailed(ValidationResult),
}

impl fmt::Display for BundleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BundleError::Io(e) => write!(f, "I/O error: {e}"),
            BundleError::Yaml(e) => write!(f, "YAML parse error: {e}"),
            BundleError::InvalidArchive(e) => write!(f, "invalid archive format: {e}"),
            BundleError::PathTraversal(p) => write!(f, "path traversal detected: {p}"),
            BundleError::SymlinkNotAllowed(p) => write!(f, "symlinks not allowed: {p}"),
            BundleError::MissingManifest => write!(f, "missing manifest: app.yaml not found"),
            BundleError::ValidationFailed(r) => {
                writeln!(f, "bundle validation failed:")?;
                for e in &r.errors {
                    writeln!(f, "  error [{}]: {}", e.rule, e.message)?;
                }
                for w in &r.warnings {
                    writeln!(f, "  warning [{}]: {}", w.rule, w.message)?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for BundleError {}

impl From<std::io::Error> for BundleError {
    fn from(e: std::io::Error) -> Self {
        BundleError::Io(e)
    }
}
