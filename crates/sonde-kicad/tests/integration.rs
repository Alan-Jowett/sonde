// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Integration tests for sonde-kicad using minimal fixture IR files.

use std::path::PathBuf;

use sonde_kicad::ir::{self, IrBundle};
use sonde_kicad::uuid_gen::UuidGenerator;

/// Path to the minimal test board fixture.
fn minimal_board_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/minimal-board")
}

/// Path to the real carrier board IR files.
fn carrier_board_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../hw/carrier-board/ir")
}

/// Create a deterministic UuidGenerator for testing.
fn test_uuid_gen(bundle: &IrBundle, ir_dir: &std::path::Path) -> UuidGenerator {
    let hash = ir::compute_ir_hash(ir_dir).expect("compute_ir_hash");
    UuidGenerator::new(&bundle.project, &hash)
}

// ---------------------------------------------------------------------------
// IR Loading
// ---------------------------------------------------------------------------

#[test]
fn load_minimal_ir_bundle() {
    let dir = minimal_board_dir();
    let bundle = ir::load_ir(&dir).expect("load_ir should succeed");

    assert_eq!(bundle.project, "minimal-test-board");
    assert!(bundle.ir1.is_some(), "IR-1 should be loaded");
    assert!(bundle.ir3.is_some(), "IR-3 should be loaded");
    assert_eq!(bundle.ir1e.components.len(), 4);
    assert_eq!(bundle.ir2.netlist.len(), 4);
}

#[test]
fn load_carrier_board_ir() {
    let dir = carrier_board_dir();
    if !dir.exists() {
        eprintln!("skipping: carrier board IR not found at {}", dir.display());
        return;
    }
    let bundle = ir::load_ir(&dir).expect("load carrier board IR");
    assert_eq!(bundle.project, "sonde-carrier");
    assert!(bundle.ir3.is_some());
    assert!(!bundle.ir1e.components.is_empty());
}

#[test]
fn load_ir_missing_required_file() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    // This directory exists but has no IR files
    match ir::load_ir(&dir) {
        Ok(_) => panic!("should fail when IR files are missing"),
        Err(e) => {
            let msg = format!("{e}");
            assert!(
                msg.contains("missing required IR file"),
                "expected missing file error, got: {msg}"
            );
        }
    }
}

#[test]
fn compute_ir_hash_deterministic() {
    let dir = minimal_board_dir();
    let h1 = ir::compute_ir_hash(&dir).expect("hash 1");
    let h2 = ir::compute_ir_hash(&dir).expect("hash 2");
    assert_eq!(h1, h2, "same inputs should produce same hash");
    assert_ne!(h1, [0u8; 32], "hash should be non-zero");
}

// ---------------------------------------------------------------------------
// Cross-Reference Validation
// ---------------------------------------------------------------------------

#[test]
fn validate_minimal_board() {
    let dir = minimal_board_dir();
    let bundle = ir::load_ir(&dir).unwrap();
    sonde_kicad::validate::validate_cross_references(&bundle)
        .expect("cross-reference validation should pass");
}

#[test]
fn validate_carrier_board() {
    let dir = carrier_board_dir();
    if !dir.exists() {
        return;
    }
    let bundle = ir::load_ir(&dir).unwrap();
    sonde_kicad::validate::validate_cross_references(&bundle)
        .expect("carrier board validation should pass");
}

// ---------------------------------------------------------------------------
// Schematic Generation
// ---------------------------------------------------------------------------

#[test]
fn emit_schematic_structure() {
    let dir = minimal_board_dir();
    let bundle = ir::load_ir(&dir).unwrap();
    let mut uuid_gen = test_uuid_gen(&bundle, &dir);

    let sch = sonde_kicad::schematic::emit_schematic(&bundle, &mut uuid_gen)
        .expect("emit_schematic should succeed");

    // Must be a valid KiCad 8 schematic
    assert!(sch.starts_with("(kicad_sch"), "should start with kicad_sch");
    assert!(
        sch.contains("(version 20231120)"),
        "should have KiCad 8 version"
    );
    assert!(
        sch.contains("(generator \"sonde-kicad\")"),
        "should identify generator"
    );

    // Must contain all component symbols
    assert!(sch.contains("\"R1\""), "should contain R1");
    assert!(sch.contains("\"C1\""), "should contain C1");
    assert!(sch.contains("\"J1\""), "should contain J1");
    assert!(sch.contains("\"J2\""), "should contain J2");

    // Must contain net labels
    assert!(sch.contains("VCC"), "should contain VCC net");
    assert!(sch.contains("GND"), "should contain GND net");
    assert!(sch.contains("SIG"), "should contain SIG net");
}

#[test]
fn emit_schematic_deterministic() {
    let dir = minimal_board_dir();
    let bundle = ir::load_ir(&dir).unwrap();

    let mut gen1 = test_uuid_gen(&bundle, &dir);
    let sch1 = sonde_kicad::schematic::emit_schematic(&bundle, &mut gen1).unwrap();

    let mut gen2 = test_uuid_gen(&bundle, &dir);
    let sch2 = sonde_kicad::schematic::emit_schematic(&bundle, &mut gen2).unwrap();

    assert_eq!(
        sch1, sch2,
        "same inputs should produce identical schematics"
    );
}

// ---------------------------------------------------------------------------
// PCB Generation
// ---------------------------------------------------------------------------

#[test]
fn emit_pcb_structure() {
    let dir = minimal_board_dir();
    let bundle = ir::load_ir(&dir).unwrap();
    let mut uuid_gen = test_uuid_gen(&bundle, &dir);

    let pcb = sonde_kicad::pcb::emit_pcb(&bundle, &mut uuid_gen).expect("emit_pcb should succeed");

    // Must be a valid KiCad 8 PCB
    assert!(pcb.starts_with("(kicad_pcb"), "should start with kicad_pcb");
    assert!(
        pcb.contains("(version 20240108)"),
        "should have KiCad 8 PCB version"
    );

    // Must contain layers
    assert!(pcb.contains("\"F.Cu\""), "should have front copper layer");
    assert!(pcb.contains("\"B.Cu\""), "should have back copper layer");

    // Must contain nets
    assert!(pcb.contains("\"VCC\""), "should have VCC net");
    assert!(pcb.contains("\"GND\""), "should have GND net");

    // Must contain all components as footprints
    assert!(pcb.contains("\"R1\""), "should place R1");
    assert!(pcb.contains("\"C1\""), "should place C1");
    assert!(pcb.contains("\"J1\""), "should place J1");
    assert!(pcb.contains("\"J2\""), "should place J2");

    // Must contain board outline
    assert!(
        pcb.contains("\"Edge.Cuts\""),
        "should have board outline layer"
    );
}

#[test]
fn emit_pcb_requires_ir3() {
    let dir = minimal_board_dir();
    let mut bundle = ir::load_ir(&dir).unwrap();
    bundle.ir3 = None; // Remove IR-3

    let mut uuid_gen = test_uuid_gen(&bundle, &dir);
    let err = sonde_kicad::pcb::emit_pcb(&bundle, &mut uuid_gen).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("IR-3"),
        "should report missing IR-3, got: {msg}"
    );
}

#[test]
fn emit_pcb_deterministic() {
    let dir = minimal_board_dir();
    let bundle = ir::load_ir(&dir).unwrap();

    let mut gen1 = test_uuid_gen(&bundle, &dir);
    let pcb1 = sonde_kicad::pcb::emit_pcb(&bundle, &mut gen1).unwrap();

    let mut gen2 = test_uuid_gen(&bundle, &dir);
    let pcb2 = sonde_kicad::pcb::emit_pcb(&bundle, &mut gen2).unwrap();

    assert_eq!(pcb1, pcb2, "same inputs should produce identical PCBs");
}

// ---------------------------------------------------------------------------
// DSN Generation
// ---------------------------------------------------------------------------

#[test]
fn emit_dsn_structure() {
    let dir = minimal_board_dir();
    let bundle = ir::load_ir(&dir).unwrap();

    let dsn = sonde_kicad::dsn::emit_dsn(&bundle).expect("emit_dsn should succeed");

    assert!(
        dsn.starts_with("(pcb"),
        "DSN should start with pcb S-expression"
    );
    assert!(dsn.contains("(resolution"), "should contain resolution");
    assert!(
        dsn.contains("(structure"),
        "should contain structure section"
    );
    assert!(dsn.contains("(network"), "should contain network section");
}

#[test]
fn emit_dsn_requires_ir3() {
    let dir = minimal_board_dir();
    let mut bundle = ir::load_ir(&dir).unwrap();
    bundle.ir3 = None;

    let err = sonde_kicad::dsn::emit_dsn(&bundle).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("IR-3"), "should report missing IR-3");
}

// ---------------------------------------------------------------------------
// BOM Generation
// ---------------------------------------------------------------------------

#[test]
fn emit_bom_csv_format() {
    let dir = minimal_board_dir();
    let bundle = ir::load_ir(&dir).unwrap();

    let bom = sonde_kicad::manufacturing::bom::emit_bom_csv(&bundle)
        .expect("emit_bom_csv should succeed");

    let lines: Vec<&str> = bom.lines().collect();

    // Header row
    assert_eq!(
        lines[0],
        "Designator,Value,Footprint,Manufacturer,Part Number,LCSC Part Number,Quantity"
    );

    // Should have one row per component + header
    assert_eq!(lines.len(), 5, "header + 4 components");

    // Rows sorted by ref_des (C1, J1, J2, R1)
    assert!(lines[1].starts_with("C1,"), "first data row should be C1");
    assert!(lines[4].starts_with("R1,"), "last data row should be R1");

    // Check R1 has its value and LCSC part number
    assert!(lines[4].contains("4.7k"), "R1 should have value 4.7k");
    assert!(lines[4].contains("C25900"), "R1 should have LCSC C25900");
}

#[test]
fn emit_bom_without_ir1() {
    let dir = minimal_board_dir();
    let mut bundle = ir::load_ir(&dir).unwrap();
    bundle.ir1 = None;

    let bom = sonde_kicad::manufacturing::bom::emit_bom_csv(&bundle)
        .expect("BOM should work without IR-1");

    // Should still produce output, just without manufacturer/LCSC data
    let lines: Vec<&str> = bom.lines().collect();
    assert_eq!(lines.len(), 5, "should still have 4 components");
}

// ---------------------------------------------------------------------------
// CPL Generation
// ---------------------------------------------------------------------------

#[test]
fn emit_cpl_csv_format() {
    let dir = minimal_board_dir();
    let bundle = ir::load_ir(&dir).unwrap();

    let cpl = sonde_kicad::manufacturing::cpl::emit_cpl_csv(&bundle)
        .expect("emit_cpl_csv should succeed");

    let lines: Vec<&str> = cpl.lines().collect();

    // Header row
    assert_eq!(lines[0], "Designator,Mid X,Mid Y,Layer,Rotation");

    // Should have one row per component + header
    assert_eq!(lines.len(), 5, "header + 4 components");

    // Sorted by ref_des
    assert!(lines[1].starts_with("C1,"), "first data row should be C1");

    // Connectors should have rotation from edge placement
    // Zone components (R1, C1) should have rotation 0
    for line in &lines[1..] {
        if line.starts_with("C1,") || line.starts_with("R1,") {
            assert!(
                line.ends_with(",Top,0"),
                "zone components should have 0 rotation: {line}"
            );
        }
    }
}

#[test]
fn emit_cpl_requires_ir3() {
    let dir = minimal_board_dir();
    let mut bundle = ir::load_ir(&dir).unwrap();
    bundle.ir3 = None;

    let err = sonde_kicad::manufacturing::cpl::emit_cpl_csv(&bundle).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("IR-3"), "should report missing IR-3");
}

// ---------------------------------------------------------------------------
// S-Expression Parser Round-Trips
// ---------------------------------------------------------------------------

#[test]
fn sexpr_parse_round_trip() {
    use sonde_kicad::sexpr::{parser, SExpr};

    let original = SExpr::list(
        "kicad_sch",
        vec![
            SExpr::pair("version", "20231120"),
            SExpr::list(
                "lib_symbols",
                vec![SExpr::list(
                    "symbol",
                    vec![SExpr::Quoted("Device:R".into())],
                )],
            ),
        ],
    );

    let serialized = original.serialize();
    let parsed = parser::parse(&serialized).expect("should parse serialized output");
    let reserialized = parsed.serialize();

    assert_eq!(serialized, reserialized, "round-trip should be identical");
}

// ---------------------------------------------------------------------------
// UUID Generator
// ---------------------------------------------------------------------------

#[test]
fn uuid_gen_deterministic_across_calls() {
    let hash = [0x42u8; 32];
    let mut gen1 = UuidGenerator::new("test-project", &hash);
    let mut gen2 = UuidGenerator::new("test-project", &hash);

    let ids1: Vec<String> = (0..5).map(|i| gen1.next(&format!("path:{i}"))).collect();
    let ids2: Vec<String> = (0..5).map(|i| gen2.next(&format!("path:{i}"))).collect();

    assert_eq!(ids1, ids2, "same seed should produce same UUIDs");
}

#[test]
fn uuid_gen_unique_per_path() {
    let hash = [0x42u8; 32];
    let mut gen = UuidGenerator::new("test-project", &hash);

    let id_a = gen.next("component:R1");
    let id_b = gen.next("component:C1");

    assert_ne!(id_a, id_b, "different paths should produce different UUIDs");
}

// ---------------------------------------------------------------------------
// SES Parser
// ---------------------------------------------------------------------------

#[test]
fn ses_routing_report_empty() {
    // A PCB with no routes and a minimal SES
    let dir = minimal_board_dir();
    let bundle = ir::load_ir(&dir).unwrap();
    let mut uuid_gen = test_uuid_gen(&bundle, &dir);
    let pcb = sonde_kicad::pcb::emit_pcb(&bundle, &mut uuid_gen).unwrap();

    let ses = r#"(session "test.ses"
  (routes
    (resolution um 10)
    (network_out
    )
  )
)"#;

    let (routed, total, unrouted) =
        sonde_kicad::ses::routing_report(&pcb, ses).expect("routing_report should succeed");

    // total includes all nets from PCB (including unnamed net 0)
    assert!(total > 0, "should have nets from PCB");
    // No wires in SES, so signal nets should be unrouted
    assert!(!unrouted.is_empty(), "signal nets should be unrouted");
    // routed = total - unrouted (net 0 excluded from unrouted list)
    assert!(routed <= total, "routed count should not exceed total");
}

// ---------------------------------------------------------------------------
// Full Pipeline (end-to-end)
// ---------------------------------------------------------------------------

#[test]
fn full_pipeline_minimal_board() {
    let dir = minimal_board_dir();
    let bundle = ir::load_ir(&dir).unwrap();

    // Validate
    sonde_kicad::validate::validate_cross_references(&bundle).unwrap();

    // Generate all artifacts
    let mut uuid_gen = test_uuid_gen(&bundle, &dir);

    let sch = sonde_kicad::schematic::emit_schematic(&bundle, &mut uuid_gen).unwrap();
    let pcb = sonde_kicad::pcb::emit_pcb(&bundle, &mut uuid_gen).unwrap();
    let dsn = sonde_kicad::dsn::emit_dsn(&bundle).unwrap();
    let bom = sonde_kicad::manufacturing::bom::emit_bom_csv(&bundle).unwrap();
    let cpl = sonde_kicad::manufacturing::cpl::emit_cpl_csv(&bundle).unwrap();

    // All outputs should be non-empty
    assert!(!sch.is_empty(), "schematic should be non-empty");
    assert!(!pcb.is_empty(), "PCB should be non-empty");
    assert!(!dsn.is_empty(), "DSN should be non-empty");
    assert!(!bom.is_empty(), "BOM should be non-empty");
    assert!(!cpl.is_empty(), "CPL should be non-empty");

    // Component count consistency
    let bom_count = bom.lines().count() - 1; // minus header
    let cpl_count = cpl.lines().count() - 1;
    assert_eq!(
        bom_count, cpl_count,
        "BOM and CPL should have same component count"
    );
    assert_eq!(bom_count, bundle.ir1e.components.len());
}

#[test]
fn full_pipeline_carrier_board() {
    let dir = carrier_board_dir();
    if !dir.exists() {
        return;
    }
    let bundle = ir::load_ir(&dir).unwrap();
    sonde_kicad::validate::validate_cross_references(&bundle).unwrap();

    let mut uuid_gen = test_uuid_gen(&bundle, &dir);

    let sch = sonde_kicad::schematic::emit_schematic(&bundle, &mut uuid_gen).unwrap();
    let _pcb = sonde_kicad::pcb::emit_pcb(&bundle, &mut uuid_gen).unwrap();
    let _dsn = sonde_kicad::dsn::emit_dsn(&bundle).unwrap();
    let bom = sonde_kicad::manufacturing::bom::emit_bom_csv(&bundle).unwrap();
    let cpl = sonde_kicad::manufacturing::cpl::emit_cpl_csv(&bundle).unwrap();

    // Carrier board specific assertions
    assert!(
        sch.contains("sonde-carrier"),
        "schematic should reference project name"
    );

    // BOM/CPL consistency
    let bom_count = bom.lines().count() - 1;
    let cpl_count = cpl.lines().count() - 1;
    assert_eq!(bom_count, cpl_count);
    assert_eq!(bom_count, bundle.ir1e.components.len());
}
