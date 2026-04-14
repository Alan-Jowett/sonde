// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! `sonde-kicad` — Convert sonde-hw-design IR files to KiCad 8 artifacts.
//!
//! This crate reads Intermediate Representation (IR) YAML files produced by
//! the sonde-hw-design pipeline and generates:
//!
//! - KiCad 8 schematics (`.kicad_sch`)
//! - KiCad 8 PCB layouts (`.kicad_pcb`) with component placement (no routing)
//! - Specctra DSN files (`.dsn`) for Freerouter autorouting
//! - Routed PCBs via Freerouter SES (`.ses`) import
//! - Manufacturing artifacts (BOM CSV, pick-and-place CSV)

pub mod error;
pub mod ir;
pub mod manufacturing;
pub mod sexpr;
pub mod uuid_gen;
pub mod validate;

pub mod dsn;
pub mod pcb;
pub mod schematic;
pub mod ses;

pub use error::Error;
pub use ir::IrBundle;
