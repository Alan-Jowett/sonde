// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! CLI entry point for `sonde-kicad`.

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

use sonde_kicad::ir;
use sonde_kicad::validate::validate_cross_references;

#[derive(Parser)]
#[command(
    name = "sonde-kicad",
    about = "Convert sonde-hw-design IR to KiCad 8 files"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Generate KiCad schematic (.kicad_sch)
    Schematic {
        #[arg(long, default_value = ".")]
        ir_dir: PathBuf,
        #[arg(long, default_value = "./output")]
        output_dir: PathBuf,
    },
    /// Generate BOM CSV
    Bom {
        #[arg(long, default_value = ".")]
        ir_dir: PathBuf,
        #[arg(long, default_value = "./output")]
        output_dir: PathBuf,
    },
    /// Run full pipeline: schematic → BOM
    Build {
        #[arg(long, default_value = ".")]
        ir_dir: PathBuf,
        #[arg(long, default_value = "./output")]
        output_dir: PathBuf,
    },
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(cli) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<(), sonde_kicad::Error> {
    match cli.command {
        Commands::Schematic { ir_dir, output_dir } => {
            cmd_schematic(&ir_dir, &output_dir)
        }
        Commands::Bom { ir_dir, output_dir } => {
            cmd_bom(&ir_dir, &output_dir)
        }
        Commands::Build { ir_dir, output_dir } => {
            cmd_schematic(&ir_dir, &output_dir)?;
            cmd_bom(&ir_dir, &output_dir)?;
            eprintln!("build complete");
            Ok(())
        }
    }
}

fn cmd_schematic(ir_dir: &Path, output_dir: &Path) -> Result<(), sonde_kicad::Error> {
    let bundle = ir::load_ir(ir_dir)?;
    validate_cross_references(&bundle)?;

    let ir_hash = ir::compute_ir_hash(ir_dir)?;
    let mut uuid_gen =
        sonde_kicad::uuid_gen::UuidGenerator::new(&bundle.project, &ir_hash);

    let content = sonde_kicad::schematic::emit_schematic(&bundle, &mut uuid_gen)?;

    std::fs::create_dir_all(output_dir)?;
    let output_path = output_dir.join(format!("{}.kicad_sch", bundle.project));
    std::fs::write(&output_path, &content)?;
    eprintln!("wrote {}", output_path.display());
    Ok(())
}

fn cmd_bom(ir_dir: &Path, output_dir: &Path) -> Result<(), sonde_kicad::Error> {
    let bundle = ir::load_ir(ir_dir)?;

    let content = sonde_kicad::manufacturing::bom::emit_bom_csv(&bundle)?;

    std::fs::create_dir_all(output_dir)?;
    let output_path = output_dir.join(format!("{}-bom.csv", bundle.project));
    std::fs::write(&output_path, &content)?;
    eprintln!("wrote {}", output_path.display());
    Ok(())
}
