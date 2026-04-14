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
    let output = Command::new("kicad-cli")
        .args([
            "pcb", "export", "gerbers",
            &pcb_path.to_string_lossy(),
            "--output", &output_dir.to_string_lossy(),
            "--layers", "F.Cu,B.Cu,F.SilkS,B.SilkS,F.Mask,B.Mask,Edge.Cuts",
        ])
        .output()
        .map_err(|e| Error::KicadCliNotFound(format!(
            "failed to run kicad-cli: {e}. Install KiCad 8 or export Gerbers manually from the KiCad GUI."
        )))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::KicadCliFailed(format!(
            "kicad-cli gerber export exited with {}: {}",
            output.status,
            stderr.trim()
        )));
    }

    // Drill export
    let drill_output = Command::new("kicad-cli")
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
        .output()
        .map_err(|e| {
            Error::KicadCliNotFound(format!("failed to run kicad-cli for drill export: {e}"))
        })?;

    if !drill_output.status.success() {
        let stderr = String::from_utf8_lossy(&drill_output.stderr);
        return Err(Error::KicadCliFailed(format!(
            "kicad-cli drill export exited with {}: {}",
            drill_output.status,
            stderr.trim()
        )));
    }

    Ok(())
}
