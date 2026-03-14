// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use std::process;

use clap::{Parser, Subcommand, ValueEnum};

use sonde_admin::grpc_client::AdminClient;
use sonde_admin::pb;
use sonde_admin::usb;

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
    /// USB pairing operations (direct node connection).
    Usb {
        #[command(subcommand)]
        action: UsbAction,
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
    /// Export gateway state to a file (AES-256-GCM encrypted).
    Export {
        /// Output file path.
        file: String,
        /// Passphrase used to encrypt the bundle.  If omitted, reads from
        /// SONDE_PASSPHRASE env var, or prompts on stdin.
        #[arg(long, env = "SONDE_PASSPHRASE")]
        passphrase: Option<String>,
    },
    /// Import gateway state from a previously exported file.
    Import {
        /// Input file path.
        file: String,
        /// Passphrase used when the bundle was exported.  If omitted, reads
        /// from SONDE_PASSPHRASE env var, or prompts on stdin.
        #[arg(long, env = "SONDE_PASSPHRASE")]
        passphrase: Option<String>,
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

#[derive(Subcommand)]
enum UsbAction {
    /// Pair a node via USB.
    Pair {
        /// Serial port (e.g., COM5, /dev/ttyACM0).
        port: String,
        /// Node identifier for gateway registration (auto mode).
        #[arg(long, conflicts_with = "raw", required_unless_present = "raw")]
        node_id: Option<String>,
        /// Raw mode: manually supply --key-hint and --psk; skip gateway registration.
        #[arg(long)]
        raw: bool,
        /// Key hint in decimal or 0x hex (raw mode only).
        #[arg(long, requires = "raw")]
        key_hint: Option<String>,
        /// 32-byte PSK as 64 hex chars (raw mode only).
        #[arg(long, requires = "raw")]
        psk: Option<String>,
        /// WiFi channel for ESP-NOW (1–13). If omitted the node retains its
        /// current channel (defaulting to 1 on first boot).
        #[arg(long, value_parser = clap::value_parser!(u8).range(1..=13))]
        channel: Option<u8>,
    },
    /// Factory reset a node via USB.
    FactoryReset {
        /// Serial port.
        port: String,
    },
    /// Query node identity via USB.
    Identity {
        /// Serial port.
        port: String,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // USB FactoryReset and Identity operate locally — no gateway connection needed.
    // Raw Pair also needs no gateway connection.
    if let Commands::Usb { action } = &cli.command {
        let json = matches!(cli.format, OutputFormat::Json);
        let needs_gateway = matches!(action, UsbAction::Pair { raw: false, .. });
        if !needs_gateway {
            let result = run_usb_local(action, json);
            if let Err(e) = result {
                eprintln!("Error: {e}");
                process::exit(1);
            }
            return;
        }
    }

    let mut client = match AdminClient::connect(&cli.socket).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to connect to gateway at {}: {e}", cli.socket);
            process::exit(1);
        }
    };

    // Handle auto-mode USB Pair (requires gateway connection).
    if let Commands::Usb {
        action:
            UsbAction::Pair {
                port,
                node_id,
                raw: false,
                channel,
                ..
            },
    } = &cli.command
    {
        let json = matches!(cli.format, OutputFormat::Json);
        // clap enforces `--node-id` is present when `--raw` is absent.
        let node_id = match node_id.as_deref() {
            Some(id) => id,
            None => {
                eprintln!("Error: --node-id is required unless --raw is set");
                process::exit(1);
            }
        };
        let result = run_usb_pair_auto(&mut client, port, node_id, *channel, json).await;
        if let Err(e) = result {
            eprintln!("Error: {e}");
            process::exit(1);
        }
        return;
    }

    let result = run(&mut client, &cli).await;
    if let Err(e) = result {
        eprintln!("Error: {e}");
        process::exit(1);
    }
}

fn parse_key_hint(s: &str) -> Result<u16, String> {
    if let Some(hex_str) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u16::from_str_radix(hex_str, 16).map_err(|e| format!("invalid key_hint hex: {e}"))
    } else {
        s.parse::<u16>()
            .map_err(|e| format!("invalid key_hint: {e}"))
    }
}

<<<<<<< HEAD
/// Run USB commands that operate locally (no gateway connection needed):
/// FactoryReset, Identity, and raw Pair.
fn run_usb_local(action: &UsbAction, json: bool) -> Result<(), String> {
=======
/// Resolve the passphrase from the CLI arg (which also reads `SONDE_PASSPHRASE`
/// env via clap's `env` attribute), or prompt on stdin if neither is set.
fn resolve_passphrase(arg: &Option<String>) -> Result<String, String> {
    if let Some(p) = arg {
        return Ok(p.clone());
    }
    eprint!("Passphrase: ");
    let mut buf = String::new();
    std::io::stdin()
        .read_line(&mut buf)
        .map_err(|e| format!("failed to read passphrase from stdin: {e}"))?;
    let trimmed = buf.trim_end_matches('\n').trim_end_matches('\r');
    if trimmed.is_empty() {
        return Err("passphrase must not be empty".into());
    }
    Ok(trimmed.to_string())
}

fn run_usb(action: &UsbAction, json: bool) -> Result<(), String> {
>>>>>>> 6c6328b (fix(gateway,admin): harden state bundle crypto and CLI passphrase handling)
    match action {
        UsbAction::Pair {
            port,
            raw: true,
            key_hint,
            psk,
            channel,
            ..
        } => {
            let key_hint_str = key_hint
                .as_deref()
                .ok_or("--key-hint is required in raw mode")?;
            let psk_str = psk.as_deref().ok_or("--psk is required in raw mode")?;
            let kh = parse_key_hint(key_hint_str)?;
            let psk_bytes = hex::decode(psk_str).map_err(|e| format!("invalid PSK hex: {e}"))?;
            if psk_bytes.len() != sonde_protocol::modem::PSK_SIZE {
                return Err(format!(
                    "PSK must be exactly 32 bytes (64 hex chars), got {} bytes",
                    psk_bytes.len()
                ));
            }
            let mut psk_arr = [0u8; sonde_protocol::modem::PSK_SIZE];
            psk_arr.copy_from_slice(&psk_bytes);
            usb::pair_node(port, kh, psk_arr, *channel, json)
        }
        UsbAction::Pair { raw: false, .. } => {
            unreachable!("auto pair is handled in the async path")
        }
        UsbAction::FactoryReset { port } => usb::factory_reset_node(port, json),
        UsbAction::Identity { port } => usb::query_identity(port, json),
    }
}

/// Auto pairing: generate PSK, pair via USB, register with gateway.
/// On gateway failure, silently send RESET_REQUEST to roll back the node,
/// then report the error.
async fn run_usb_pair_auto(
    client: &mut AdminClient,
    port: &str,
    node_id: &str,
    channel: Option<u8>,
    json: bool,
) -> Result<(), String> {
    let psk = usb::generate_psk()?;
    let key_hint = usb::derive_key_hint(&psk);

    usb::pair_node_inner(port, key_hint, psk, channel)?;

    match client
        .register_node(node_id, key_hint as u32, psk.to_vec())
        .await
    {
        Ok(_) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "status": "success",
                        "node_id": node_id,
                        "key_hint": format!("0x{:04x}", key_hint),
                    })
                );
            } else {
                println!(
                    "Paired and registered: {} (key_hint=0x{:04x})",
                    node_id, key_hint
                );
            }
            Ok(())
        }
        Err(e) => {
            // Gateway registration failed — silently roll back the node.
            let rollback = usb::factory_reset_silent(port);
            if let Err(rb_err) = rollback {
                Err(format!(
                    "gateway registration failed: {}. Rollback also failed: {}. \
                     Factory reset the node manually before re-pairing.",
                    e, rb_err
                ))
            } else {
                Err(format!(
                    "gateway registration failed: {}. Node has been factory reset.",
                    e
                ))
            }
        }
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
                let (hash, size) = client.ingest_program(image_data, profile_val, None).await?;
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
            StateAction::Export { file, passphrase } => {
                let pass = resolve_passphrase(passphrase)?;
                let data = client.export_state(&pass).await?;
                std::fs::write(file, &data)?;
                if json {
                    print_json(&serde_json::json!({"exported_bytes": data.len(), "file": file}))?;
                } else {
                    println!("Exported {} bytes to {file}", data.len());
                }
            }
            StateAction::Import { file, passphrase } => {
                let pass = resolve_passphrase(passphrase)?;
                let data = std::fs::read(file)?;
                client.import_state(data, &pass).await?;
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

        // USB commands are handled before reaching this match.
        Commands::Usb { .. } => {
            unreachable!("USB commands handled earlier and return before this match")
        }
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
