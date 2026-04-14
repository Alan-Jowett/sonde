// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! IR cross-validation — verify consistency between IR files.

use crate::ir::IrBundle;
use crate::Error;

/// Validate cross-references between IR files in the bundle.
pub fn validate_cross_references(bundle: &IrBundle) -> Result<(), Error> {
    // Every ref_des in IR-2 netlist must exist in IR-1e
    let ir1e_refs: std::collections::HashSet<&str> = bundle
        .ir1e
        .components
        .iter()
        .map(|c| c.ref_des.as_str())
        .collect();

    for entry in &bundle.ir2.netlist {
        if !ir1e_refs.contains(entry.ref_des.as_str()) {
            return Err(Error::CrossValidation(format!(
                "component `{}` in IR-2 netlist not found in IR-1e",
                entry.ref_des
            )));
        }
    }

    // Every component in IR-1e must have library_status == "FOUND"
    for comp in &bundle.ir1e.components {
        if comp.library_status != "FOUND" {
            return Err(Error::CrossValidation(format!(
                "component `{}` in IR-1e has library_status `{}`, expected `FOUND`",
                comp.ref_des, comp.library_status
            )));
        }
    }

    // If IR-3 present, check component zones reference valid components
    if let Some(ir3) = &bundle.ir3 {
        for zone in &ir3.component_zones {
            for ref_des in &zone.components {
                if !ir1e_refs.contains(ref_des.as_str()) {
                    return Err(Error::CrossValidation(format!(
                        "component `{ref_des}` in IR-3 zone `{}` not found in IR-1e",
                        zone.group
                    )));
                }
            }
        }
    }

    Ok(())
}
