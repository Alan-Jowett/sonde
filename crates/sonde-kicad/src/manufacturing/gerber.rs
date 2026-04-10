// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Gerber export via `kicad-cli`.

use std::path::Path;
use std::process::Command;

use crate::Error;

/// Export Gerber files from a `.kicad_pcb` using `kicad-cli`.
pub fn export_gerber(pcb_path: &Path, output_dir: &Path) -> Result<(), Error> {
    std::fs::create_dir_all(output_dir)?;

    // Gerber export
    let status = Command::new("kicad-cli")
        .args([
            "pcb", "export", "gerbers",
            &pcb_path.to_string_lossy(),
            "--output", &output_dir.to_string_lossy(),
            "--layers", "F.Cu,B.Cu,F.SilkS,B.SilkS,F.Mask,B.Mask,Edge.Cuts",
        ])
        .status()
        .map_err(|e| Error::KicadCliNotFound(format!(
            "failed to run kicad-cli: {e}. Install KiCad 8 or export Gerbers manually from the KiCad GUI."
        )))?;

    if !status.success() {
        return Err(Error::KicadCliNotFound(format!(
            "kicad-cli exited with status {status}"
        )));
    }

    // Drill export
    let drill_status = Command::new("kicad-cli")
        .args([
            "pcb",
            "export",
            "drill",
            &pcb_path.to_string_lossy(),
            "--output",
            &output_dir.to_string_lossy(),
            "--format",
            "excellon",
        ])
        .status()
        .map_err(|e| {
            Error::KicadCliNotFound(format!("failed to run kicad-cli for drill export: {e}"))
        })?;

    if !drill_status.success() {
        return Err(Error::KicadCliNotFound(format!(
            "kicad-cli drill export exited with status {drill_status}"
        )));
    }

    Ok(())
}
