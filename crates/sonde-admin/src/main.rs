// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use std::collections::HashMap;
use std::io::IsTerminal;
use std::process;

use clap::{Parser, Subcommand, ValueEnum};
use zeroize::Zeroizing;

use sonde_admin::format_epoch_ms;
use sonde_admin::grpc_client::AdminClient;
use sonde_admin::pb;
use sonde_protocol::normalize_display_filename;

#[derive(Parser)]
#[command(name = "sonde-admin", version = concat!(env!("CARGO_PKG_VERSION"), " (", env!("SONDE_GIT_COMMIT"), ")"), about = "Sonde gateway administration CLI")]
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

    /// Skip confirmation prompts for destructive commands. Required when running
    /// non-interactively (e.g. when stdin is piped or redirected).
    #[arg(long = "yes", short = 'y', global = true)]
    yes: bool,

    /// Show verbose diagnostics on errors (e.g. per-instruction verifier notes).
    #[arg(long, short = 'v', global = true)]
    verbose: bool,

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
    /// BLE phone pairing.
    Pairing {
        #[command(subcommand)]
        action: PairingAction,
    },
    /// Handler management.
    Handler {
        #[command(subcommand)]
        action: HandlerAction,
    },
    /// Master key management.
    Key {
        #[command(subcommand)]
        action: KeyAction,
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
    /// Ingest a BPF ELF program.
    Ingest {
        /// Path to the BPF ELF object file.
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
    /// Display 1 to 4 lines of text on the modem for 60 seconds.
    Display {
        /// Text lines to render; each argument becomes one display line.
        #[arg(num_args = 1..=4)]
        lines: Vec<String>,
    },
}

#[derive(Subcommand)]
enum PairingAction {
    /// Open BLE phone registration window.
    Start {
        /// Window duration in seconds (default: 120).
        #[arg(long, default_value = "120")]
        duration_s: u32,
    },
    /// Close BLE phone registration window.
    Stop,
    /// List registered phones.
    ListPhones,
    /// Revoke a phone's PSK.
    RevokePhone {
        /// Phone ID to revoke.
        phone_id: u32,
    },
}

#[derive(Subcommand)]
enum HandlerAction {
    /// Add a handler for a program hash (or "*" for catch-all).
    Add {
        /// Program hash (hex) or "*" for catch-all.
        program_hash: String,
        /// Command to run.
        command: String,
        /// Arguments to pass to the command.
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
        /// Working directory for the handler process.
        #[arg(long)]
        working_dir: Option<String>,
        /// Reply timeout in milliseconds.
        #[arg(long)]
        reply_timeout_ms: Option<u64>,
    },
    /// Remove a handler by program hash (or "*" for catch-all).
    Remove {
        /// Program hash (hex) or "*" for catch-all.
        program_hash: String,
    },
    /// List all configured handlers.
    List,
}

#[derive(Subcommand)]
enum KeyAction {
    /// Perform master key rotation (interactive).
    Rotate,
    /// Display the gateway's BIP-39 key fingerprint.
    Fingerprint,
    /// Display master key status (epoch, ID, rotation state).
    Status,
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
        if let Some(status) = e.downcast_ref::<tonic::Status>() {
            let msg = status.message();
            // Show full diagnostics in verbose mode; otherwise show a
            // compact summary.  The --verbose hint is only useful when
            // the message actually contains multi-line diagnostics
            // (i.e. Prevail invariants), not for single-line
            // verification errors like "ephemeral programs must not
            // declare maps" (GW-1305 criterion 3).
            let has_diagnostics = msg.contains('\n');
            if cli.verbose {
                eprintln!("Error: {msg}");
            } else if has_diagnostics {
                // Without --verbose, show the summary line and the first
                // verifier error, then a hint (GW-1305 criterion 3).
                // The gateway places find_first_error() output as the
                // first line after the summary, so lines.next() is
                // reliable here.
                let mut lines = msg.lines();
                let summary = lines.next().unwrap_or(msg);
                eprintln!("Error: {summary}");
                if let Some(first_error) = lines.next() {
                    let first_error = first_error.trim();
                    if !first_error.is_empty() {
                        eprintln!("{first_error}");
                    }
                }
                eprintln!("Hint: run with --verbose for full invariants.");
            } else {
                eprintln!("Error: {msg}");
            }
        } else {
            eprintln!("Error: {e}");
        }
        process::exit(1);
    }
}

/// Resolve the passphrase from the CLI arg (which also reads `SONDE_PASSPHRASE`
/// env via clap's `env` attribute), or prompt on the TTY without echo if
/// neither is set.
fn resolve_passphrase(arg: &Option<String>) -> Result<String, String> {
    if let Some(p) = arg {
        if p.is_empty() {
            return Err("passphrase must not be empty".into());
        }
        return Ok(p.clone());
    }
    eprint!("Passphrase: ");
    std::io::Write::flush(&mut std::io::stderr()).ok();
    let pass = rpassword::read_password().map_err(|e| format!("failed to read passphrase: {e}"))?;
    if pass.is_empty() {
        return Err("passphrase must not be empty".into());
    }
    Ok(pass)
}

/// Prompt the user for confirmation before a destructive action.
///
/// Returns `Ok(())` if the user confirms, or `Err` if they decline.
/// Skips the prompt (auto-confirms) only when `--yes` is passed.
/// In non-interactive mode (stdin is not a terminal), returns an error
/// unless `--yes` is provided, to avoid silently bypassing confirmation.
fn confirm(message: &str, yes: bool) -> Result<(), String> {
    if yes {
        return Ok(());
    }
    if !std::io::stdin().is_terminal() {
        return Err(
            "refusing to proceed without confirmation in non-interactive mode; re-run with --yes"
                .into(),
        );
    }
    eprint!("{message} [y/N]: ");
    std::io::Write::flush(&mut std::io::stderr()).ok();
    let mut buf = String::new();
    std::io::stdin()
        .read_line(&mut buf)
        .map_err(|e| format!("failed to read confirmation: {e}"))?;
    if buf.trim().eq_ignore_ascii_case("y") {
        Ok(())
    } else {
        Err("aborted".into())
    }
}

async fn run(client: &mut AdminClient, cli: &Cli) -> Result<(), Box<dyn std::error::Error>> {
    let json = matches!(cli.format, OutputFormat::Json);

    match &cli.command {
        Commands::Node { action } => {
            match action {
                NodeAction::List => {
                    let nodes = client.list_nodes().await?;
                    if json {
                        print_json(&nodes.iter().map(node_to_json).collect::<Vec<_>>())?;
                    } else {
                        if nodes.is_empty() {
                            println!("No nodes registered.");
                            return Ok(());
                        }
                        let program_names =
                            load_program_name_map_for_display(client, cli.verbose).await;
                        for n in &nodes {
                            print_node(n, &program_names, cli.verbose);
                        }
                    }
                }
                NodeAction::Get { node_id } => {
                    let node = client.get_node(node_id).await?;
                    if json {
                        print_json(&node_to_json(&node))?;
                    } else {
                        let program_names =
                            load_program_name_map_for_display(client, cli.verbose).await;
                        print_node(&node, &program_names, cli.verbose);
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
                    confirm(
                        &format!("Remove node `{node_id}`? This will delete the PSK and all session state."),
                        cli.yes,
                    )?;
                    client.remove_node(node_id).await?;
                    if json {
                        print_json(&serde_json::json!({"removed": node_id}))?;
                    } else {
                        println!("Removed node: {node_id}");
                    }
                }
            }
        }

        Commands::Program { action } => match action {
            ProgramAction::Ingest { file, profile } => {
                let source_filename = std::path::Path::new(&file)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned());
                let image_data = std::fs::read(file)?;
                let profile_val = match profile {
                    Profile::Resident => 1,
                    Profile::Ephemeral => 2,
                };
                let (hash, size) = client
                    .ingest_program(image_data, profile_val, None, source_filename)
                    .await?;
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
                                    "source_filename": p.source_filename.as_deref(),
                                    "has_decoder": p.has_decoder,
                                })
                            })
                            .collect::<Vec<_>>(),
                    )?;
                } else {
                    if programs.is_empty() {
                        println!("No programs stored.");
                    }
                    for p in &programs {
                        let decoder_tag = if p.has_decoder { ", decoder" } else { "" };
                        if let Some(f) = &p.source_filename {
                            println!(
                                "  {} {} ({} bytes, {}{})",
                                hex::encode(&p.hash),
                                f,
                                p.size,
                                profile_name(p.verification_profile),
                                decoder_tag
                            );
                        } else {
                            println!(
                                "  {} ({} bytes, {}{})",
                                hex::encode(&p.hash),
                                p.size,
                                profile_name(p.verification_profile),
                                decoder_tag
                            );
                        }
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
                confirm(&format!("Remove program `{program_hash}`?"), cli.yes)?;
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
                    "wake_rssi_dbm": status.last_wake_rssi_dbm,
                    "firmware_abi_version": status.firmware_abi_version,
                    "last_seen_ms": status.last_seen_ms,
                    "has_active_session": status.has_active_session,
                }))?;
            } else {
                let program_names = load_program_name_map_for_display(client, cli.verbose).await;
                println!("Node:     {}", status.node_id);
                println!(
                    "Program:  {}",
                    format_program_identifier(
                        &status.current_program_hash,
                        &program_names,
                        cli.verbose,
                    )
                );
                if let Some(mv) = status.battery_mv {
                    println!("Battery:  {mv} mV");
                }
                if let Some(rssi) = status.last_wake_rssi_dbm {
                    println!("RSSI:     {rssi} dBm");
                }
                if let Some(abi) = status.firmware_abi_version {
                    println!("ABI:      {abi}");
                }
                if let Some(ms) = status.last_seen_ms {
                    println!("Last seen: {}", format_epoch_ms(ms));
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
                confirm(
                    &format!("Import state from `{file}`? This will overwrite all gateway state."),
                    cli.yes,
                )?;
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
            ModemAction::Display { lines } => {
                client
                    .show_modem_display_message(lines.clone(), false)
                    .await?;
                if json {
                    print_json(&serde_json::json!({
                        "lines": lines,
                        "duration_s": 60,
                    }))?;
                } else {
                    println!("Displayed modem message for 60s");
                }
            }
        },

        Commands::Pairing { action } => match action {
            PairingAction::Start { duration_s } => {
                use tokio_stream::StreamExt;
                let resp = client.open_ble_pairing(*duration_s).await;
                match resp {
                    Ok(mut stream) => {
                        while let Some(event) = stream.next().await {
                            match event {
                                Ok(evt) => match evt.event {
                                    Some(pb::ble_pairing_event::Event::WindowOpened(w)) => {
                                        println!("BLE pairing window opened for {}s", w.duration_s);
                                    }
                                    Some(pb::ble_pairing_event::Event::Passkey(p)) => {
                                        println!("Passkey: {:06}", p.passkey);
                                        eprint!("Confirm pairing? (y/n): ");
                                        let _ = std::io::Write::flush(&mut std::io::stderr());
                                        let input = tokio::task::spawn_blocking(|| {
                                            let mut buf = String::new();
                                            std::io::stdin().read_line(&mut buf).ok();
                                            buf
                                        })
                                        .await
                                        .unwrap_or_default();
                                        let accept = input.trim().eq_ignore_ascii_case("y");
                                        if let Err(e) = client.confirm_ble_pairing(accept).await {
                                            eprintln!("Failed to confirm: {e}");
                                        }
                                    }
                                    Some(pb::ble_pairing_event::Event::PhoneConnected(c)) => {
                                        println!("Phone connected (MTU={})", c.mtu);
                                    }
                                    Some(pb::ble_pairing_event::Event::PhoneDisconnected(_)) => {
                                        println!("Phone disconnected");
                                    }
                                    Some(pb::ble_pairing_event::Event::PhoneRegistered(r)) => {
                                        println!(
                                            "Phone registered: {} (key_hint=0x{:04x})",
                                            r.label, r.phone_key_hint
                                        );
                                    }
                                    Some(pb::ble_pairing_event::Event::WindowClosed(_)) => {
                                        println!("BLE pairing window closed");
                                        break;
                                    }
                                    None => {}
                                },
                                Err(e) => {
                                    eprintln!("Stream error: {e}");
                                    break;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("Failed to open BLE pairing: {e}");
                        process::exit(1);
                    }
                }
            }
            PairingAction::Stop => {
                confirm("Close BLE pairing window?", cli.yes)?;
                client.close_ble_pairing().await?;
                if json {
                    print_json(&serde_json::json!({"status": "closed"}))?;
                } else {
                    println!("BLE pairing window closed");
                }
            }
            PairingAction::ListPhones => {
                let phones = client.list_phones().await?;
                if json {
                    print_json(
                        &phones
                            .iter()
                            .map(|p| {
                                serde_json::json!({
                                    "phone_id": p.phone_id,
                                    "phone_key_hint": format!("0x{:04x}", p.phone_key_hint),
                                    "label": p.label,
                                    "issued_at_ms": p.issued_at_ms,
                                    "status": p.status,
                                })
                            })
                            .collect::<Vec<_>>(),
                    )?;
                } else {
                    println!(
                        "{:<8} {:<12} {:<20} {:<12} Issued",
                        "ID", "Key Hint", "Label", "Status"
                    );
                    for p in &phones {
                        println!(
                            "{:<8} 0x{:04x}       {:<20} {:<12} {}",
                            p.phone_id,
                            p.phone_key_hint,
                            p.label,
                            p.status,
                            format_epoch_ms(p.issued_at_ms)
                        );
                    }
                }
            }
            PairingAction::RevokePhone { phone_id } => {
                confirm(&format!("Revoke phone `{phone_id}`?"), cli.yes)?;
                client.revoke_phone(*phone_id).await?;
                if json {
                    print_json(&serde_json::json!({"phone_id": phone_id, "status": "revoked"}))?;
                } else {
                    println!("Phone {phone_id} revoked");
                }
            }
        },

        Commands::Handler { action } => match action {
            HandlerAction::Add {
                program_hash,
                command,
                args,
                working_dir,
                reply_timeout_ms,
            } => {
                client
                    .add_handler(
                        program_hash,
                        command,
                        args.clone(),
                        working_dir.clone(),
                        *reply_timeout_ms,
                    )
                    .await?;
                if json {
                    print_json(&serde_json::json!({
                        "added": true,
                        "program_hash": program_hash,
                    }))?;
                } else {
                    println!("Added handler for program {program_hash}");
                }
            }
            HandlerAction::Remove { program_hash } => {
                client.remove_handler(program_hash).await?;
                if json {
                    print_json(&serde_json::json!({"removed": program_hash}))?;
                } else {
                    println!("Removed handler for program {program_hash}");
                }
            }
            HandlerAction::List => {
                let handlers = client.list_handlers().await?;
                if json {
                    print_json(
                        &handlers
                            .iter()
                            .map(|h| {
                                serde_json::json!({
                                    "program_hash": h.program_hash,
                                    "command": h.command,
                                    "args": h.args,
                                    "working_dir": h.working_dir,
                                    "reply_timeout_ms": h.reply_timeout_ms,
                                })
                            })
                            .collect::<Vec<_>>(),
                    )?;
                } else {
                    if handlers.is_empty() {
                        println!("No handlers configured.");
                    }
                    for h in &handlers {
                        let wd = if h.working_dir.is_empty() {
                            String::new()
                        } else {
                            format!(" (cwd={})", h.working_dir)
                        };
                        println!(
                            "  {} → {} {}{}",
                            h.program_hash,
                            h.command,
                            h.args.join(" "),
                            wd
                        );
                    }
                }
            }
        },

        Commands::Key { action } => match action {
            KeyAction::Fingerprint => {
                let state = client.get_gateway_state().await?;
                if json {
                    print_json(&serde_json::json!({
                        "fingerprint_words": state.fingerprint_words,
                    }))?;
                } else {
                    println!("{}", state.fingerprint_words.join(" "));
                }
            }
            KeyAction::Status => {
                let state = client.get_gateway_state().await?;
                if json {
                    print_json(&serde_json::json!({
                        "master_key_epoch": state.master_key_epoch,
                        "master_key_id": hex::encode(&state.master_key_id),
                        "rotation_in_progress": state.rotation_in_progress,
                    }))?;
                } else {
                    println!("Master key epoch:      {}", state.master_key_epoch);
                    println!(
                        "Master key ID:         {}",
                        hex::encode(&state.master_key_id)
                    );
                    println!("Rotation in progress:  {}", state.rotation_in_progress);
                }
            }
            KeyAction::Rotate => {
                key_rotate(client, cli).await?;
            }
        },
    }

    Ok(())
}

fn print_json(value: &impl serde::Serialize) -> Result<(), serde_json::Error> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

/// Validate a rotation passphrase per ADMIN-0900.
///
/// Rejects passphrases shorter than 20 characters **and** fewer than 6 words.
/// A passphrase passes if it has >= 20 chars OR >= 6 words.
fn validate_passphrase(passphrase: &str) -> Result<(), String> {
    let char_count = passphrase.chars().count();
    let word_count = passphrase.split_whitespace().count();
    if char_count < 20 && word_count < 6 {
        return Err(format!(
            "passphrase too short: {char_count} characters, {word_count} words \
             (need at least 20 characters or 6 words)"
        ));
    }
    Ok(())
}

fn validate_rotation_code(rotation_code: &str) -> Result<String, String> {
    let trimmed = rotation_code.trim();
    if trimmed.is_empty() {
        return Err("rotation code must not be empty".into());
    }
    if !trimmed.is_ascii() || !trimmed.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        return Err(
            "rotation code must contain only ASCII letters and digits (lowercase is normalized)"
                .into(),
        );
    }
    if trimmed.len() != 6 {
        return Err("rotation code must be exactly 6 characters".into());
    }
    let normalized = trimmed.to_ascii_uppercase();
    Ok(normalized)
}

/// Build a `RotationPayloadV1` binary envelope per `evolve-962-specification.md` §2.6.1.
///
/// Returns the serialized payload suitable for `SubmitRotation`.
fn build_rotation_payload(
    gw_x25519_public: &[u8; 32],
    gateway_id_raw: &[u8; 16],
    master_key_epoch: u64,
    new_master_key: &[u8; 32],
    rotation_code: &str,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    use aes_gcm::aead::{Aead, OsRng};
    use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
    use hkdf::Hkdf;
    use sha2::Sha256;
    use x25519_dalek::{EphemeralSecret, PublicKey};

    // Generate ephemeral X25519 keypair.
    let ephemeral_secret = EphemeralSecret::random_from_rng(OsRng);
    let ephemeral_public = PublicKey::from(&ephemeral_secret);

    // Compute shared secret.
    let gw_public = PublicKey::from(*gw_x25519_public);
    let shared_secret = ephemeral_secret.diffie_hellman(&gw_public);

    // Reject non-contributory DH results (low-order public key).
    if !shared_secret.was_contributory() {
        return Err("X25519 shared secret is non-contributory (low-order point)".into());
    }

    // Derive AES-256-GCM key via HKDF-SHA-256.
    // HKDF salt (protocol constant): b"sonde-rotation-v1"
    // info: gateway_id_raw || master_key_epoch_be64
    let hkdf_salt = b"sonde-rotation-v1";
    let mut info = Vec::with_capacity(24);
    info.extend_from_slice(gateway_id_raw);
    info.extend_from_slice(&master_key_epoch.to_be_bytes());

    let hkdf = Hkdf::<Sha256>::new(Some(hkdf_salt), shared_secret.as_bytes());
    let mut aes_key = Zeroizing::new([0u8; 32]);
    hkdf.expand(&info, &mut *aes_key)
        .map_err(|_| "HKDF expand failed")?;

    // Encode CBOR plaintext map with integer keys 1–2, deterministic ordering.
    // Wrapped in Zeroizing because it contains new_master_key in the clear.
    let plaintext = Zeroizing::new(encode_rotation_plaintext(new_master_key, rotation_code));

    // Encrypt with AES-256-GCM.
    // AAD: gateway_id_raw || master_key_epoch_be64
    let cipher = Aes256Gcm::new((&*aes_key).into());
    let mut nonce_bytes = [0u8; 12];
    getrandom::fill(&mut nonce_bytes).map_err(|e| format!("failed to generate nonce: {e}"))?;
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext_and_tag = cipher
        .encrypt(
            nonce,
            aes_gcm::aead::Payload {
                msg: &plaintext,
                aad: &info,
            },
        )
        .map_err(|e| format!("AES-256-GCM encryption failed: {e}"))?;

    // Serialize: version(1) || ephemeral_public(32) || nonce(12) || ciphertext_and_tag
    let mut payload = Vec::with_capacity(1 + 32 + 12 + ciphertext_and_tag.len());
    payload.push(0x01); // version
    payload.extend_from_slice(ephemeral_public.as_bytes());
    payload.extend_from_slice(&nonce_bytes);
    payload.extend_from_slice(&ciphertext_and_tag);

    Ok(payload)
}

/// Encode the CBOR plaintext map for `RotationPayloadV1`.
///
/// Deterministic CBOR encoding per RFC 8949 §4.2: integer keys in ascending
/// order, minimal-length encoding.
fn encode_rotation_plaintext(new_master_key: &[u8; 32], rotation_code: &str) -> Vec<u8> {
    let mut buf = Vec::with_capacity(64);

    // CBOR map with 2 entries: A2
    buf.push(0xA2);

    // Key 1: new_master_key (bstr, 32 bytes)
    buf.push(0x01);
    buf.push(0x58); // bytes, 1-byte length
    buf.push(32);
    buf.extend_from_slice(new_master_key);

    // Key 2: rotation_code (tstr)
    buf.push(0x02);
    cbor_encode_tstr(&mut buf, rotation_code);

    buf
}

/// Encode a CBOR unsigned integer in minimal form.
#[cfg(test)]
fn cbor_encode_uint(buf: &mut Vec<u8>, value: u64) {
    if value < 24 {
        buf.push(value as u8);
    } else if value <= 0xFF {
        buf.push(0x18);
        buf.push(value as u8);
    } else if value <= 0xFFFF {
        buf.push(0x19);
        buf.extend_from_slice(&(value as u16).to_be_bytes());
    } else if value <= 0xFFFF_FFFF {
        buf.push(0x1A);
        buf.extend_from_slice(&(value as u32).to_be_bytes());
    } else {
        buf.push(0x1B);
        buf.extend_from_slice(&value.to_be_bytes());
    }
}

/// Encode a CBOR text string.
fn cbor_encode_tstr(buf: &mut Vec<u8>, s: &str) {
    let len = s.len() as u64;
    if len < 24 {
        buf.push(0x60 + len as u8);
    } else if len <= 0xFF {
        buf.push(0x78);
        buf.push(len as u8);
    } else if len <= 0xFFFF {
        buf.push(0x79);
        buf.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        buf.push(0x7A);
        buf.extend_from_slice(&(len as u32).to_be_bytes());
    }
    buf.extend_from_slice(s.as_bytes());
}

/// Interactive key rotation flow per ADMIN-0900 and `admin-design.md` §11.3.
async fn key_rotate(client: &mut AdminClient, cli: &Cli) -> Result<(), Box<dyn std::error::Error>> {
    let json = matches!(cli.format, OutputFormat::Json);

    // Step 1: Fetch gateway ACTUAL_STATE.
    let state = client.get_gateway_state().await?;

    // Validate field lengths from proto (proto does not enforce byte sizes).
    if state.gateway_id.len() != 16 {
        return Err(format!(
            "gateway_id has unexpected length: {} (expected 16)",
            state.gateway_id.len()
        )
        .into());
    }
    if state.x25519_public_key.len() != 32 {
        return Err(format!(
            "x25519_public_key has unexpected length: {} (expected 32)",
            state.x25519_public_key.len()
        )
        .into());
    }
    if state.master_key_id.len() != 32 {
        return Err(format!(
            "master_key_id has unexpected length: {} (expected 32)",
            state.master_key_id.len()
        )
        .into());
    }

    // Step 2: Display fingerprint and prompt for confirmation.
    let fingerprint = state.fingerprint_words.join(" ");
    eprintln!("Gateway fingerprint: {fingerprint}");
    confirm("Verify this matches the modem display. Continue?", cli.yes)?;

    // Step 3: Prompt for rotation code.
    eprint!("Rotation code: ");
    std::io::Write::flush(&mut std::io::stderr()).ok();
    let mut rotation_code = String::new();
    std::io::stdin()
        .read_line(&mut rotation_code)
        .map_err(|e| format!("failed to read rotation code: {e}"))?;
    let rotation_code = validate_rotation_code(&rotation_code)?;

    // Step 4: Prompt for passphrase (masked).
    eprint!("Passphrase: ");
    std::io::Write::flush(&mut std::io::stderr()).ok();
    let passphrase = Zeroizing::new(
        rpassword::read_password().map_err(|e| format!("failed to read passphrase: {e}"))?,
    );
    validate_passphrase(&passphrase)?;

    // Step 5: Prompt for deployment label.
    eprint!("Deployment label: ");
    let mut stderr = std::io::stderr();
    std::io::Write::flush(&mut stderr)?;
    let mut deployment_label = String::new();
    std::io::stdin().read_line(&mut deployment_label)?;
    let deployment_label = deployment_label.trim();
    if deployment_label.is_empty() {
        return Err("deployment label must not be empty".into());
    }

    // Step 6: Derive the KDF salt from the deployment label and use fixed v1 params.
    use sha2::{Digest, Sha256};
    let salt_input = format!("sonde-kdf-v1:{deployment_label}");
    let salt_hash = Sha256::digest(salt_input.as_bytes());
    let salt: [u8; 16] = salt_hash[..16].try_into().expect("SHA-256 is 32 bytes");
    let m_cost = 65536u32;
    let t_cost = 3u32;
    let p_cost = 1u32;

    // Step 7: Derive new master key with Argon2id.
    let argon2_params = argon2::Params::new(m_cost, t_cost, p_cost, Some(32))
        .map_err(|e| format!("invalid Argon2id params: {e}"))?;
    let argon2 = argon2::Argon2::new(
        argon2::Algorithm::Argon2id,
        argon2::Version::V0x13,
        argon2_params,
    );
    let mut new_master_key = Zeroizing::new([0u8; 32]);
    argon2
        .hash_password_into(passphrase.as_bytes(), &salt, &mut *new_master_key)
        .map_err(|e| format!("Argon2id key derivation failed: {e}"))?;

    // Step 8: Build RotationPayloadV1.
    let gateway_id_raw: [u8; 16] = state.gateway_id[..16].try_into().unwrap();
    let gw_x25519_public: [u8; 32] = state.x25519_public_key[..32].try_into().unwrap();

    let rotation_payload = build_rotation_payload(
        &gw_x25519_public,
        &gateway_id_raw,
        state.master_key_epoch,
        &new_master_key,
        &rotation_code,
    )?;

    // Step 9: Submit rotation.
    let resp = client.submit_rotation(rotation_payload).await?;
    if !resp.accepted {
        return Err(format!("rotation rejected: {}", resp.error).into());
    }

    // Step 10: Poll until master_key_epoch increments or timeout.
    let original_epoch = state.master_key_epoch;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let new_state = client.get_gateway_state().await?;
        if new_state.master_key_epoch > original_epoch {
            if json {
                print_json(&serde_json::json!({
                    "rotated": true,
                    "new_master_key_epoch": new_state.master_key_epoch,
                    "new_master_key_id": hex::encode(&new_state.master_key_id),
                }))?;
            } else {
                eprintln!(
                    "Rotation complete. New epoch: {}, new key ID: {}",
                    new_state.master_key_epoch,
                    hex::encode(&new_state.master_key_id),
                );
            }
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("rotation timed out waiting for epoch increment".into());
        }
    }
}

async fn load_program_name_map(
    client: &mut AdminClient,
) -> Result<HashMap<Vec<u8>, String>, tonic::Status> {
    let programs = client.list_programs().await?;
    Ok(programs
        .into_iter()
        .filter_map(|program| {
            normalize_display_filename(&program.source_filename).map(|name| (program.hash, name))
        })
        .collect())
}

fn program_name_map_for_display(
    result: Result<HashMap<Vec<u8>, String>, tonic::Status>,
    verbose: bool,
) -> HashMap<Vec<u8>, String> {
    match result {
        Ok(program_names) => program_names,
        Err(error) => {
            if verbose {
                eprintln!("Warning: failed to load program metadata; showing hashes only: {error}");
            } else {
                eprintln!(
                    "Warning: failed to load program metadata; showing hashes only. Run with --verbose for details."
                );
            }
            HashMap::new()
        }
    }
}

async fn load_program_name_map_for_display(
    client: &mut AdminClient,
    verbose: bool,
) -> HashMap<Vec<u8>, String> {
    program_name_map_for_display(load_program_name_map(client).await, verbose)
}

fn format_program_identifier(
    hash: &[u8],
    program_names: &HashMap<Vec<u8>, String>,
    verbose: bool,
) -> String {
    match program_names.get(hash) {
        Some(name) if verbose => format!("{} ({})", name, hex::encode(hash)),
        Some(name) => name.clone(),
        None => hex::encode(hash),
    }
}

fn print_node(n: &pb::NodeInfo, program_names: &HashMap<Vec<u8>, String>, verbose: bool) {
    println!("  {} (key_hint={})", n.node_id, n.key_hint);
    if !n.assigned_program_hash.is_empty() {
        println!(
            "    assigned: {}",
            format_program_identifier(&n.assigned_program_hash, program_names, verbose)
        );
    }
    if !n.current_program_hash.is_empty() {
        println!(
            "    current:  {}",
            format_program_identifier(&n.current_program_hash, program_names, verbose)
        );
    }
    if let Some(mv) = n.last_battery_mv {
        println!("    battery:  {mv} mV");
    }
    if let Some(rssi) = n.last_wake_rssi_dbm {
        println!("    RSSI:     {rssi} dBm");
    }
    if let Some(ms) = n.last_seen_ms {
        println!("    last seen: {}", format_epoch_ms(ms));
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
        "last_wake_rssi_dbm": n.last_wake_rssi_dbm,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_name_map_for_display_falls_back_to_empty_map() {
        let program_names = program_name_map_for_display(
            Err(tonic::Status::internal("program metadata unavailable")),
            false,
        );

        assert!(program_names.is_empty());
    }

    // ── Passphrase validation (T-0902) ──────────────────────────────────

    #[test]
    fn passphrase_short_chars_few_words_rejected() {
        // 10 chars, 2 words → both below threshold → rejected.
        let result = validate_passphrase("short pass");
        assert!(result.is_err());
    }

    #[test]
    fn passphrase_long_chars_passes() {
        // >= 20 chars, 3 words → passes (char threshold met).
        let result = validate_passphrase("a very long passphrase indeed here");
        assert!(result.is_ok());
    }

    #[test]
    fn passphrase_many_words_passes() {
        // 19 chars, 6 words → passes (word threshold met).
        let result = validate_passphrase("a b c d e f");
        assert!(result.is_ok());
    }

    #[test]
    fn passphrase_exactly_20_chars_passes() {
        // Exactly 20 chars, 1 word → passes (char threshold met).
        let result = validate_passphrase("12345678901234567890");
        assert!(result.is_ok());
    }

    #[test]
    fn passphrase_19_chars_5_words_rejected() {
        // 19 chars, 5 words → both below threshold → rejected.
        let result = validate_passphrase("abc def ghi jkl mno");
        assert!(result.is_err());
    }

    #[test]
    fn rotation_code_empty_rejected() {
        let result = validate_rotation_code("   ");
        assert!(result.is_err());
    }

    #[test]
    fn rotation_code_lowercase_is_normalized() {
        let result = validate_rotation_code("ab12cd").unwrap();
        assert_eq!(result, "AB12CD");
    }

    #[test]
    fn rotation_code_invalid_characters_rejected() {
        let result = validate_rotation_code("AB-12");
        assert!(result.is_err());
    }

    #[test]
    fn rotation_code_wrong_length_rejected() {
        let result = validate_rotation_code("ABC12");
        assert!(result.is_err());
    }

    #[test]
    fn rotation_code_non_ascii_rejected() {
        let err = validate_rotation_code("ß12").unwrap_err();
        assert_eq!(
            err,
            "rotation code must contain only ASCII letters and digits (lowercase is normalized)"
        );
    }

    // ── CBOR encoding ───────────────────────────────────────────────────

    #[test]
    fn cbor_plaintext_round_trip_structure() {
        let master_key = [0x42u8; 32];

        let plaintext = encode_rotation_plaintext(&master_key, "ABC123");

        // Must start with A2 (map of 2 entries).
        assert_eq!(plaintext[0], 0xA2);
        // Key 1 follows.
        assert_eq!(plaintext[1], 0x01);
        // Key 2 must be present.
        assert!(plaintext.contains(&0x02));
    }

    // ── CBOR uint encoding ──────────────────────────────────────────────

    #[test]
    fn cbor_uint_small() {
        let mut buf = Vec::new();
        cbor_encode_uint(&mut buf, 0);
        assert_eq!(buf, vec![0x00]);
        buf.clear();
        cbor_encode_uint(&mut buf, 23);
        assert_eq!(buf, vec![0x17]);
    }

    #[test]
    fn cbor_uint_one_byte() {
        let mut buf = Vec::new();
        cbor_encode_uint(&mut buf, 24);
        assert_eq!(buf, vec![0x18, 24]);
        buf.clear();
        cbor_encode_uint(&mut buf, 255);
        assert_eq!(buf, vec![0x18, 0xFF]);
    }

    #[test]
    fn cbor_uint_two_byte() {
        let mut buf = Vec::new();
        cbor_encode_uint(&mut buf, 256);
        assert_eq!(buf, vec![0x19, 0x01, 0x00]);
        buf.clear();
        cbor_encode_uint(&mut buf, 65536);
        assert_eq!(buf, vec![0x1A, 0x00, 0x01, 0x00, 0x00]);
    }
}
