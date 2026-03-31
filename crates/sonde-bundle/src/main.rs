// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! CLI entry point for `sonde-bundle`.

use clap::{Parser, Subcommand, ValueEnum};
use sonde_bundle::archive;
use sonde_bundle::error::BundleError;
use sonde_bundle::manifest::Manifest;
use std::path::PathBuf;
use std::process;

#[derive(Parser)]
#[command(name = "sonde-bundle", about = "Sonde App Bundle tool")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

/// Output format for the inspect command.
#[derive(Clone, ValueEnum)]
enum OutputFormat {
    Text,
    Json,
}

#[derive(Subcommand)]
enum Commands {
    /// Create a .sondeapp bundle from a directory
    Create {
        /// Directory containing app.yaml and bundle files
        source_dir: PathBuf,
        /// Output path (default: <name>-<version>.sondeapp)
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Validate a .sondeapp bundle
    Validate {
        /// Path to .sondeapp file
        bundle: PathBuf,
    },
    /// Show bundle contents and metadata
    Inspect {
        /// Path to .sondeapp file
        bundle: PathBuf,
        /// Output format
        #[arg(long, default_value = "text")]
        format: OutputFormat,
    },
}

fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Create { source_dir, output } => cmd_create(&source_dir, output.as_deref()),
        Commands::Validate { bundle } => cmd_validate(&bundle),
        Commands::Inspect { bundle, format } => cmd_inspect(&bundle, format),
    };

    if let Err(e) = result {
        eprintln!("error: {e}");
        process::exit(1);
    }
}

fn cmd_create(
    source_dir: &std::path::Path,
    output: Option<&std::path::Path>,
) -> Result<(), BundleError> {
    let manifest_path = source_dir.join("app.yaml");
    if !manifest_path.exists() {
        return Err(BundleError::MissingManifest);
    }
    let yaml = std::fs::read_to_string(&manifest_path)?;
    let manifest = Manifest::from_yaml(&yaml)?;

    let output_path = match output {
        Some(p) => p.to_path_buf(),
        None => PathBuf::from(format!("{}-{}.sondeapp", manifest.name, manifest.version)),
    };

    let info = archive::create_bundle(source_dir, &output_path)?;
    println!(
        "Created {} ({} bytes)",
        output_path.display(),
        info.archive_size
    );
    Ok(())
}

fn cmd_validate(bundle: &std::path::Path) -> Result<(), BundleError> {
    let result = archive::validate_bundle(bundle)?;

    if result.is_valid() {
        for w in &result.warnings {
            eprintln!("warning [{}]: {}", w.rule, w.message);
        }
        println!("Bundle is valid.");
        Ok(())
    } else {
        Err(BundleError::ValidationFailed(result))
    }
}

fn cmd_inspect(bundle: &std::path::Path, format: OutputFormat) -> Result<(), BundleError> {
    let info = archive::inspect_bundle(bundle)?;

    match format {
        OutputFormat::Json => {
            let json = serde_json::json!({
                "name": &info.manifest.name,
                "version": &info.manifest.version,
                "schema_version": info.manifest.schema_version,
                "description": &info.manifest.description,
                "archive_size": info.archive_size,
                "programs": info.manifest.programs.iter().map(|p| {
                    serde_json::json!({
                        "name": &p.name,
                        "path": &p.path,
                        "profile": p.profile.to_string(),
                    })
                }).collect::<Vec<_>>(),
                "handlers": info.manifest.handlers.iter().map(|h| {
                    serde_json::json!({
                        "program": &h.program,
                        "command": &h.command,
                        "args": &h.args,
                    })
                }).collect::<Vec<_>>(),
                "nodes": info.manifest.nodes.iter().map(|n| {
                    serde_json::json!({
                        "name": &n.name,
                        "program": &n.program,
                    })
                }).collect::<Vec<_>>(),
                "files": info.files.iter().map(|f| {
                    serde_json::json!({
                        "path": &f.path,
                        "size": f.size,
                    })
                }).collect::<Vec<_>>(),
            });
            let json_str = serde_json::to_string_pretty(&json).map_err(|e| {
                BundleError::Io(std::io::Error::other(format!(
                    "JSON serialization failed: {e}"
                )))
            })?;
            println!("{json_str}");
        }
        OutputFormat::Text => {
            println!("Bundle: {} v{}", info.manifest.name, info.manifest.version);
            if let Some(ref desc) = info.manifest.description {
                println!("Description: {desc}");
            }
            println!(
                "Schema version: {}",
                info.manifest
                    .schema_version
                    .map_or("(missing)".to_string(), |v| v.to_string())
            );
            println!("Archive size: {} bytes", info.archive_size);
            println!();

            println!("Programs:");
            for p in &info.manifest.programs {
                let size = info
                    .files
                    .iter()
                    .find(|f| f.path == p.path)
                    .map(|f| f.size)
                    .unwrap_or(0);
                println!("  {} ({}, {} bytes)", p.name, p.profile, size);
            }

            if !info.manifest.handlers.is_empty() {
                println!();
                println!("Handlers:");
                for h in &info.manifest.handlers {
                    let args = if h.args.is_empty() {
                        String::new()
                    } else {
                        format!(" {}", h.args.join(" "))
                    };
                    println!("  {} -> {}{}", h.program, h.command, args);
                }
            }

            println!();
            println!("Nodes:");
            for n in &info.manifest.nodes {
                print!("  {} -> {}", n.name, n.program);
                if let Some(ref hw) = n.hardware {
                    if !hw.sensors.is_empty() {
                        let sensors: Vec<String> = hw
                            .sensors
                            .iter()
                            .map(|s| {
                                let label = s
                                    .label
                                    .as_deref()
                                    .map(|l| format!(" ({l})"))
                                    .unwrap_or_default();
                                format!("{}@{}{}", s.sensor_type, s.id, label)
                            })
                            .collect();
                        print!(" [{}]", sensors.join(", "));
                    }
                }
                println!();
            }
        }
    }

    Ok(())
}
