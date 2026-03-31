// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Sonde App Bundle library.
//!
//! Create, validate, and inspect `.sondeapp` bundles — single-file archives
//! containing BPF programs, handler definitions, and node targeting for
//! deploying applications onto the Sonde platform.

pub mod archive;
pub mod error;
pub mod manifest;
pub mod validate;
