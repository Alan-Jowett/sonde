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
    /// Generate KiCad PCB layout (.kicad_pcb)
    Pcb {
        #[arg(long, default_value = ".")]
        ir_dir: PathBuf,
        #[arg(long, default_value = "./output")]
        output_dir: PathBuf,
    },
    /// Generate Specctra DSN file for Freerouter
    Dsn {
        #[arg(long, default_value = ".")]
        ir_dir: PathBuf,
        #[arg(long, default_value = "./output")]
        output_dir: PathBuf,
    },
    /// Import Freerouter session (.ses) into PCB
    ImportSes {
        #[arg(long)]
        pcb: PathBuf,
        #[arg(long)]
        ses: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    /// Generate BOM CSV
    Bom {
        #[arg(long, default_value = ".")]
        ir_dir: PathBuf,
        #[arg(long, default_value = "./output")]
        output_dir: PathBuf,
    },
    /// Generate pick-and-place CSV
    Cpl {
        #[arg(long, default_value = ".")]
        ir_dir: PathBuf,
        #[arg(long, default_value = "./output")]
        output_dir: PathBuf,
    },
    /// Export Gerber files via kicad-cli
    Gerber {
        #[arg(long)]
        pcb: PathBuf,
        #[arg(long, default_value = "./output/gerber")]
        output_dir: PathBuf,
    },
    /// Run full pipeline: schematic → PCB → DSN → BOM → CPL
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
        Commands::Schematic { ir_dir, output_dir } => cmd_schematic(&ir_dir, &output_dir),
        Commands::Pcb { ir_dir, output_dir } => cmd_pcb(&ir_dir, &output_dir),
        Commands::Dsn { ir_dir, output_dir } => cmd_dsn(&ir_dir, &output_dir),
        Commands::ImportSes { pcb, ses, output } => cmd_import_ses(&pcb, &ses, &output),
        Commands::Bom { ir_dir, output_dir } => cmd_bom(&ir_dir, &output_dir),
        Commands::Cpl { ir_dir, output_dir } => cmd_cpl(&ir_dir, &output_dir),
        Commands::Gerber { pcb, output_dir } => cmd_gerber(&pcb, &output_dir),
        Commands::Build { ir_dir, output_dir } => {
            cmd_schematic(&ir_dir, &output_dir)?;
            cmd_pcb(&ir_dir, &output_dir)?;
            cmd_dsn(&ir_dir, &output_dir)?;
            cmd_bom(&ir_dir, &output_dir)?;
            cmd_cpl(&ir_dir, &output_dir)?;
            eprintln!("build complete");
            Ok(())
        }
    }
}

fn make_uuid_gen(
    bundle: &sonde_kicad::IrBundle,
    ir_dir: &Path,
) -> Result<sonde_kicad::uuid_gen::UuidGenerator, sonde_kicad::Error> {
    let ir_hash = ir::compute_ir_hash(ir_dir)?;
    Ok(sonde_kicad::uuid_gen::UuidGenerator::new(
        &bundle.project,
        &ir_hash,
    ))
}

fn cmd_schematic(ir_dir: &Path, output_dir: &Path) -> Result<(), sonde_kicad::Error> {
    let bundle = ir::load_ir(ir_dir)?;
    validate_cross_references(&bundle)?;
    let mut uuid_gen = make_uuid_gen(&bundle, ir_dir)?;
    let content = sonde_kicad::schematic::emit_schematic(&bundle, &mut uuid_gen)?;
    write_output(
        output_dir,
        &format!("{}.kicad_sch", bundle.project),
        &content,
    )
}

fn cmd_pcb(ir_dir: &Path, output_dir: &Path) -> Result<(), sonde_kicad::Error> {
    let bundle = ir::load_ir(ir_dir)?;
    validate_cross_references(&bundle)?;
    let mut uuid_gen = make_uuid_gen(&bundle, ir_dir)?;
    let content = sonde_kicad::pcb::emit_pcb(&bundle, &mut uuid_gen)?;
    write_output(
        output_dir,
        &format!("{}.kicad_pcb", bundle.project),
        &content,
    )
}

fn cmd_dsn(ir_dir: &Path, output_dir: &Path) -> Result<(), sonde_kicad::Error> {
    let bundle = ir::load_ir(ir_dir)?;
    validate_cross_references(&bundle)?;
    let content = sonde_kicad::dsn::emit_dsn(&bundle)?;
    write_output(output_dir, &format!("{}.dsn", bundle.project), &content)
}

fn cmd_import_ses(
    pcb_path: &Path,
    ses_path: &Path,
    output_path: &Path,
) -> Result<(), sonde_kicad::Error> {
    let pcb_content = std::fs::read_to_string(pcb_path)?;
    let ses_content = std::fs::read_to_string(ses_path)?;

    let hash = [0x42u8; 32];
    let mut uuid_gen = sonde_kicad::uuid_gen::UuidGenerator::new("ses-import", &hash);

    let result = sonde_kicad::ses::import_ses(&pcb_content, &ses_content, &mut uuid_gen)?;

    let (routed, total, unrouted) = sonde_kicad::ses::routing_report(&pcb_content, &ses_content)?;
    eprintln!("{routed} of {total} nets routed");
    if !unrouted.is_empty() {
        eprintln!("unrouted: {}", unrouted.join(", "));
    }

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(output_path, &result)?;
    eprintln!("wrote {}", output_path.display());
    Ok(())
}

fn cmd_bom(ir_dir: &Path, output_dir: &Path) -> Result<(), sonde_kicad::Error> {
    let bundle = ir::load_ir(ir_dir)?;
    let content = sonde_kicad::manufacturing::bom::emit_bom_csv(&bundle)?;
    write_output(output_dir, &format!("{}-bom.csv", bundle.project), &content)
}

fn cmd_cpl(ir_dir: &Path, output_dir: &Path) -> Result<(), sonde_kicad::Error> {
    let bundle = ir::load_ir(ir_dir)?;
    let content = sonde_kicad::manufacturing::cpl::emit_cpl_csv(&bundle)?;
    write_output(output_dir, &format!("{}-cpl.csv", bundle.project), &content)
}

fn cmd_gerber(pcb_path: &Path, output_dir: &Path) -> Result<(), sonde_kicad::Error> {
    sonde_kicad::manufacturing::gerber::export_gerber(pcb_path, output_dir)?;
    eprintln!("gerber export complete: {}", output_dir.display());
    Ok(())
}

fn write_output(
    output_dir: &Path,
    filename: &str,
    content: &str,
) -> Result<(), sonde_kicad::Error> {
    std::fs::create_dir_all(output_dir)?;
    let path = output_dir.join(filename);
    std::fs::write(&path, content)?;
    eprintln!("wrote {}", path.display());
    Ok(())
}
