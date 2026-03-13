// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Plugin for the bpf_conformance test suite.
//!
//! Protocol: <https://github.com/Alan-Jowett/bpf_conformance>
//!
//! - First CLI argument (optional): hex-encoded memory contents.
//! - Program bytecode is read from stdin as hex.
//! - On success, prints the value of r0 in hex to stdout and exits 0.
//! - On failure, prints an error to stderr and exits 1.

use std::io::Read;

/// Helper function used by the `call_unwind_fail.data` conformance test.
fn unwind(a: u64, _b: u64, _c: u64, _d: u64, _e: u64) -> u64 {
    a
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();

    // First positional arg is hex-encoded memory (if present).
    let memory_hex = if args.len() > 1 { &args[1] } else { "" };
    let clean_mem: String = memory_hex.chars().filter(|c| !c.is_whitespace()).collect();
    let mut memory = hex_decode(&clean_mem).map_err(|e| format!("bad memory hex: {e}"))?;

    // Read program hex from stdin.
    let mut program_hex = String::new();
    std::io::stdin()
        .read_to_string(&mut program_hex)
        .map_err(|e| format!("stdin read error: {e}"))?;
    program_hex.retain(|c| !c.is_whitespace());
    let bytecode = hex_decode(&program_hex).map_err(|e| format!("bad program hex: {e}"))?;

    let helpers: &[sonde_bpf::interpreter::HelperDescriptor] =
        &[sonde_bpf::interpreter::HelperDescriptor {
            id: 5,
            func: unwind,
            ret: sonde_bpf::interpreter::HelperReturn::Scalar,
        }];

    // SAFETY: maps is empty — no map regions to validate.
    let result = unsafe {
        sonde_bpf::interpreter::execute_program(&bytecode, &mut memory, helpers, &[], false)
    }
    .map_err(|e| format!("{e}"))?;

    println!("{result:x}");
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

/// Minimal hex decoder (avoids adding a dependency).
fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    if s.is_empty() {
        return Ok(Vec::new());
    }
    if !s.len().is_multiple_of(2) {
        return Err("odd-length hex string".into());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&s[i..i + 2], 16)
                .map_err(|e| format!("invalid hex at offset {i}: {e}"))
        })
        .collect()
}
