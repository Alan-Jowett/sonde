// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use std::process;

use clap::{Parser, Subcommand, ValueEnum};

use sonde_admin::grpc_client::AdminClient;
use sonde_admin::pb;

#[derive(Parser)]
#[command(name = "sonde-admin", about = "Sonde gateway administration CLI")]
struct Cli {
    /// Gateway admin socket path (UDS on Linux, named pipe on Windows).
    #[arg(
        long,
        default_value = default_socket(),
        global = true,
    )]
    socket: String,

    /// Output format.
    #[arg(long, default_value = "text", global = true)]
    format: OutputFormat,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Clone, ValueEnum)]
enum OutputFormat {
    Text,
    Json,
}

fn default_socket() -> &'static str {
    if cfg!(unix) {
        "/var/run/sonde/admin.sock"
    } else {
        r"\\.\pipe\sonde-admin"
    }
}

#[derive(Subcommand)]
enum Commands {
    /// Node management.
    Node {
        #[command(subcommand)]
        action: NodeAction,
    },
    /// Program management.
    Program {
        #[command(subcommand)]
        action: ProgramAction,
    },
    /// Set a node's wake schedule interval.
    Schedule {
        #[command(subcommand)]
        action: ScheduleAction,
    },
    /// Queue a reboot command for a node.
    Reboot {
        /// Node identifier.
        node_id: String,
    },
    /// Queue an ephemeral diagnostic program for a node.
    Ephemeral {
        /// Node identifier.
        node_id: String,
        /// Program hash (hex).
        program_hash: String,
    },
    /// Get node status.
    Status {
        /// Node identifier.
        node_id: String,
    },
    /// Gateway state export/import.
    State {
        #[command(subcommand)]
        action: StateAction,
    },
    /// Modem management.
    Modem {
        #[command(subcommand)]
        action: ModemAction,
    },
}

#[derive(Subcommand)]
enum NodeAction {
    /// List all registered nodes.
    List,
    /// Get details for a single node.
    Get {
        /// Node identifier.
        node_id: String,
    },
    /// Register a new node.
    Register {
        /// Node identifier.
        node_id: String,
        /// Key hint (0–65535).
        key_hint: u16,
        /// Pre-shared key (64 hex chars = 32 bytes).
        psk_hex: String,
    },
    /// Remove a node from the registry.
    Remove {
        /// Node identifier.
        node_id: String,
    },
}

#[derive(Subcommand)]
enum ProgramAction {
    /// Ingest a CBOR program image.
    Ingest {
        /// Path to the CBOR program image file.
        file: String,
        /// Verification profile.
        #[arg(long)]
        profile: Profile,
    },
    /// List all stored programs.
    List,
    /// Assign a program to a node.
    Assign {
        /// Node identifier.
        node_id: String,
        /// Program hash (hex).
        program_hash: String,
    },
    /// Remove a program from the library.
    Remove {
        /// Program hash (hex).
        program_hash: String,
    },
}

#[derive(Clone, ValueEnum)]
enum Profile {
    Resident,
    Ephemeral,
}

#[derive(Subcommand)]
enum ScheduleAction {
    /// Set the wake interval for a node.
    Set {
        /// Node identifier.
        node_id: String,
        /// Interval in seconds.
        interval_s: u32,
    },
}

#[derive(Subcommand)]
enum StateAction {
    /// Export gateway state to a file.
    Export {
        /// Output file path.
        file: String,
    },
    /// Import gateway state from a file.
    Import {
        /// Input file path.
        file: String,
    },
}

#[derive(Subcommand)]
enum ModemAction {
    /// Get modem status (channel, counters, uptime).
    Status,
    /// Set the modem's radio channel.
    SetChannel {
        /// Channel number (1–14).
        #[arg(value_parser = clap::value_parser!(u32).range(1..=14))]
        channel: u32,
    },
    /// Scan all WiFi channels for AP activity.
    Scan,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    let mut client = match AdminClient::connect(&cli.socket).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to connect to gateway at {}: {e}", cli.socket);
            process::exit(1);
        }
    };

    let result = run(&mut client, &cli).await;
    if let Err(e) = result {
        eprintln!("Error: {e}");
        process::exit(1);
    }
}

async fn run(client: &mut AdminClient, cli: &Cli) -> Result<(), Box<dyn std::error::Error>> {
    let json = matches!(cli.format, OutputFormat::Json);

    match &cli.command {
        Commands::Node { action } => match action {
            NodeAction::List => {
                let nodes = client.list_nodes().await?;
                if json {
                    print_json(&nodes.iter().map(node_to_json).collect::<Vec<_>>())?;
                } else {
                    if nodes.is_empty() {
                        println!("No nodes registered.");
                    }
                    for n in &nodes {
                        print_node(n);
                    }
                }
            }
            NodeAction::Get { node_id } => {
                let node = client.get_node(node_id).await?;
                if json {
                    print_json(&node_to_json(&node))?;
                } else {
                    print_node(&node);
                }
            }
            NodeAction::Register {
                node_id,
                key_hint,
                psk_hex,
            } => {
                let psk = hex::decode(psk_hex)?;
                if psk.len() != 32 {
                    return Err(format!(
                        "PSK must be exactly 32 bytes (64 hex chars), got {} bytes",
                        psk.len()
                    )
                    .into());
                }
                let id = client.register_node(node_id, *key_hint as u32, psk).await?;
                if json {
                    print_json(&serde_json::json!({"node_id": id}))?;
                } else {
                    println!("Registered node: {id}");
                }
            }
            NodeAction::Remove { node_id } => {
                client.remove_node(node_id).await?;
                if json {
                    print_json(&serde_json::json!({"removed": node_id}))?;
                } else {
                    println!("Removed node: {node_id}");
                }
            }
        },

        Commands::Program { action } => match action {
            ProgramAction::Ingest { file, profile } => {
                let image_data = std::fs::read(file)?;
                let profile_val = match profile {
                    Profile::Resident => 1,
                    Profile::Ephemeral => 2,
                };
                let (hash, size) = client.ingest_program(image_data, profile_val).await?;
                if json {
                    print_json(&serde_json::json!({
                        "program_hash": hex::encode(&hash),
                        "program_size": size,
                    }))?;
                } else {
                    println!("Ingested program: {} ({size} bytes)", hex::encode(&hash));
                }
            }
            ProgramAction::List => {
                let programs = client.list_programs().await?;
                if json {
                    print_json(
                        &programs
                            .iter()
                            .map(|p| {
                                serde_json::json!({
                                    "hash": hex::encode(&p.hash),
                                    "size": p.size,
                                    "profile": profile_name(p.verification_profile),
                                })
                            })
                            .collect::<Vec<_>>(),
                    )?;
                } else {
                    if programs.is_empty() {
                        println!("No programs stored.");
                    }
                    for p in &programs {
                        println!(
                            "  {} ({} bytes, {})",
                            hex::encode(&p.hash),
                            p.size,
                            profile_name(p.verification_profile)
                        );
                    }
                }
            }
            ProgramAction::Assign {
                node_id,
                program_hash,
            } => {
                let hash = hex::decode(program_hash)?;
                client.assign_program(node_id, hash).await?;
                if json {
                    print_json(&serde_json::json!({"assigned": true}))?;
                } else {
                    println!("Assigned program {program_hash} to node {node_id}");
                }
            }
            ProgramAction::Remove { program_hash } => {
                let hash = hex::decode(program_hash)?;
                client.remove_program(hash).await?;
                if json {
                    print_json(&serde_json::json!({"removed": program_hash}))?;
                } else {
                    println!("Removed program: {program_hash}");
                }
            }
        },

        Commands::Schedule { action } => match action {
            ScheduleAction::Set {
                node_id,
                interval_s,
            } => {
                client.set_schedule(node_id, *interval_s).await?;
                if json {
                    print_json(&serde_json::json!({"node_id": node_id, "interval_s": interval_s}))?;
                } else {
                    println!("Set schedule for {node_id}: {interval_s}s");
                }
            }
        },

        Commands::Reboot { node_id } => {
            client.queue_reboot(node_id).await?;
            if json {
                print_json(&serde_json::json!({"queued": "reboot", "node_id": node_id}))?;
            } else {
                println!("Queued reboot for node: {node_id}");
            }
        }

        Commands::Ephemeral {
            node_id,
            program_hash,
        } => {
            let hash = hex::decode(program_hash)?;
            client.queue_ephemeral(node_id, hash).await?;
            if json {
                print_json(
                    &serde_json::json!({"queued": "ephemeral", "node_id": node_id, "program_hash": program_hash}),
                )?;
            } else {
                println!("Queued ephemeral program {program_hash} for node {node_id}");
            }
        }

        Commands::Status { node_id } => {
            let status = client.get_node_status(node_id).await?;
            if json {
                print_json(&serde_json::json!({
                    "node_id": status.node_id,
                    "current_program_hash": hex::encode(&status.current_program_hash),
                    "battery_mv": status.battery_mv,
                    "firmware_abi_version": status.firmware_abi_version,
                    "last_seen_ms": status.last_seen_ms,
                    "has_active_session": status.has_active_session,
                }))?;
            } else {
                println!("Node:     {}", status.node_id);
                println!("Program:  {}", hex::encode(&status.current_program_hash));
                if let Some(mv) = status.battery_mv {
                    println!("Battery:  {mv} mV");
                }
                if let Some(abi) = status.firmware_abi_version {
                    println!("ABI:      {abi}");
                }
                if let Some(ms) = status.last_seen_ms {
                    println!("Last seen: {ms} ms (epoch)");
                }
                println!(
                    "Session:  {}",
                    if status.has_active_session {
                        "active"
                    } else {
                        "none"
                    }
                );
            }
        }

        Commands::State { action } => match action {
            StateAction::Export { file } => {
                let data = client.export_state().await?;
                std::fs::write(file, &data)?;
                if json {
                    print_json(&serde_json::json!({"exported_bytes": data.len(), "file": file}))?;
                } else {
                    println!("Exported {} bytes to {file}", data.len());
                }
            }
            StateAction::Import { file } => {
                let data = std::fs::read(file)?;
                client.import_state(data).await?;
                if json {
                    print_json(&serde_json::json!({"imported": true, "file": file}))?;
                } else {
                    println!("Imported state from {file}");
                }
            }
        },

        Commands::Modem { action } => match action {
            ModemAction::Status => {
                let status = client.get_modem_status().await?;
                if json {
                    print_json(&serde_json::json!({
                        "channel": status.channel,
                        "tx_count": status.tx_count,
                        "rx_count": status.rx_count,
                        "tx_fail_count": status.tx_fail_count,
                        "uptime_s": status.uptime_s,
                    }))?;
                } else {
                    println!("Channel:       {}", status.channel);
                    println!("TX count:      {}", status.tx_count);
                    println!("RX count:      {}", status.rx_count);
                    println!("TX fail count: {}", status.tx_fail_count);
                    println!("Uptime:        {}s", status.uptime_s);
                }
            }
            ModemAction::SetChannel { channel } => {
                client.set_modem_channel(*channel).await?;
                if json {
                    print_json(&serde_json::json!({"channel": channel}))?;
                } else {
                    println!("Set modem channel to {channel}");
                }
            }
            ModemAction::Scan => {
                let entries = client.scan_modem_channels().await?;
                if json {
                    print_json(
                        &entries
                            .iter()
                            .map(|e| {
                                serde_json::json!({
                                    "channel": e.channel,
                                    "ap_count": e.ap_count,
                                    "strongest_rssi": e.strongest_rssi,
                                })
                            })
                            .collect::<Vec<_>>(),
                    )?;
                } else {
                    println!("{:<10} {:<10} Best RSSI", "Channel", "APs");
                    for e in &entries {
                        println!(
                            "{:<10} {:<10} {} dBm",
                            e.channel, e.ap_count, e.strongest_rssi
                        );
                    }
                }
            }
        },
    }

    Ok(())
}

fn print_json(value: &impl serde::Serialize) -> Result<(), serde_json::Error> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn print_node(n: &pb::NodeInfo) {
    println!("  {} (key_hint={})", n.node_id, n.key_hint);
    if !n.assigned_program_hash.is_empty() {
        println!("    assigned: {}", hex::encode(&n.assigned_program_hash));
    }
    if !n.current_program_hash.is_empty() {
        println!("    current:  {}", hex::encode(&n.current_program_hash));
    }
    if let Some(mv) = n.last_battery_mv {
        println!("    battery:  {mv} mV");
    }
    if let Some(ms) = n.last_seen_ms {
        println!("    last seen: {ms} ms");
    }
    if let Some(s) = n.schedule_interval_s {
        println!("    schedule: {s}s");
    }
}

fn node_to_json(n: &pb::NodeInfo) -> serde_json::Value {
    serde_json::json!({
        "node_id": n.node_id,
        "key_hint": n.key_hint,
        "assigned_program_hash": hex::encode(&n.assigned_program_hash),
        "current_program_hash": hex::encode(&n.current_program_hash),
        "last_battery_mv": n.last_battery_mv,
        "last_firmware_abi_version": n.last_firmware_abi_version,
        "last_seen_ms": n.last_seen_ms,
        "schedule_interval_s": n.schedule_interval_s,
    })
}

fn profile_name(v: i32) -> &'static str {
    match v {
        1 => "resident",
        2 => "ephemeral",
        _ => "unknown",
    }
}
