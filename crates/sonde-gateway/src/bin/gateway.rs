// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use tokio::sync::RwLock;
use tracing::{error, info};

use sonde_gateway::engine::{Gateway, PendingCommand};
use sonde_gateway::handler::{load_handler_configs, HandlerRouter};
use sonde_gateway::key_provider::{EnvKeyProvider, FileKeyProvider, KeyProvider, KeyProviderError};
use sonde_gateway::modem::UsbEspNowTransport;
use sonde_gateway::session::SessionManager;
use sonde_gateway::sqlite_storage::SqliteStorage;
use sonde_gateway::storage::Storage;
use sonde_gateway::transport::Transport;
use sonde_gateway::AdminService;
use zeroize::Zeroizing;

#[cfg(unix)]
const DEFAULT_ADMIN_SOCKET: &str = "/var/run/sonde/admin.sock";
#[cfg(windows)]
const DEFAULT_ADMIN_SOCKET: &str = r"\\.\pipe\sonde-admin";

/// Key provider backend selection.
#[derive(clap::ValueEnum, Clone, Debug, Default)]
enum KeyProviderKind {
    /// Read a 64-hex-char key from the file given by `--master-key-file` (default).
    #[default]
    File,
    /// Read a 64-hex-char key from the `SONDE_MASTER_KEY` environment variable.
    Env,
    /// Decrypt a DPAPI-protected blob file given by `--master-key-file` (Windows only).
    Dpapi,
    /// Retrieve the key from the Linux D-Bus Secret Service keyring (Linux only).
    SecretService,
}

/// Sonde gateway — manages sensor nodes over ESP-NOW radio.
#[derive(Parser, Debug)]
#[command(name = "sonde-gateway", version, about)]
struct Cli {
    /// Path to the SQLite database file.
    #[arg(long, default_value = "sonde.db")]
    db: String,

    /// Serial port for the ESP-NOW modem (e.g., /dev/ttyUSB0 or COM3).
    #[arg(long)]
    port: String,

    /// ESP-NOW radio channel (1–14).
    #[arg(long, default_value_t = 1)]
    channel: u8,

    /// gRPC admin API socket path (UDS on Linux/macOS, named pipe on Windows).
    #[arg(long, default_value = DEFAULT_ADMIN_SOCKET)]
    admin_socket: String,

    /// Session timeout in seconds.
    #[arg(long, default_value_t = 30)]
    session_timeout: u64,

    /// Serial port baud rate.
    #[arg(long, default_value_t = 115_200)]
    baud_rate: u32,

    /// Path to a YAML handler configuration file.
    ///
    /// When provided, APP_DATA frames are routed to external handler processes
    /// as defined in the file. See gateway-design.md §9 for the format.
    #[arg(long)]
    handler_config: Option<PathBuf>,

    /// Key provider backend for loading the 32-byte master key (GW-0601a).
    ///
    /// - `file`           — Read 64 hex chars from `--master-key-file` (default).
    /// - `env`            — Read 64 hex chars from `SONDE_MASTER_KEY` env var.
    /// - `dpapi`          — Decrypt a DPAPI-protected blob at `--master-key-file` (Windows only).
    /// - `secret-service` — Retrieve from the Linux D-Bus Secret Service keyring (Linux only).
    #[arg(long, default_value = "file", value_enum)]
    key_provider: KeyProviderKind,

    /// Path to the master key file (hex or DPAPI blob, depending on `--key-provider`).
    ///
    /// Required for `--key-provider file` and `--key-provider dpapi`.
    /// For `--key-provider env`, falls back to the `SONDE_MASTER_KEY` environment variable.
    #[arg(long)]
    master_key_file: Option<PathBuf>,

    /// Label used to identify the master key in the Secret Service keyring.
    ///
    /// Only used when `--key-provider secret-service` is selected.
    /// Defaults to `"sonde-gateway-master-key"`.
    #[arg(long, default_value = "sonde-gateway-master-key")]
    key_label: String,
}

/// Build the appropriate [`KeyProvider`] from the CLI arguments.
///
/// Returns an error if the selected backend is not available on the current
/// platform or if required arguments are missing.
fn build_key_provider(cli: &Cli) -> Result<Box<dyn KeyProvider>, Box<dyn std::error::Error>> {
    match cli.key_provider {
        KeyProviderKind::File => {
            let path = cli.master_key_file.clone().ok_or(
                "master key required: provide --master-key-file when using --key-provider file",
            )?;
            Ok(Box::new(FileKeyProvider::new(path)))
        }
        KeyProviderKind::Env => Ok(Box::new(EnvKeyProvider::default())),
        KeyProviderKind::Dpapi => {
            #[cfg(windows)]
            {
                use sonde_gateway::key_provider::DpapiKeyProvider;
                let path = cli.master_key_file.clone().ok_or(
                    "DPAPI blob path required: provide --master-key-file when using --key-provider dpapi",
                )?;
                Ok(Box::new(DpapiKeyProvider::new(path)))
            }
            #[cfg(not(windows))]
            {
                Err(KeyProviderError::NotAvailable(
                    "dpapi backend is only available on Windows".into(),
                )
                .into())
            }
        }
        KeyProviderKind::SecretService => {
            #[cfg(target_os = "linux")]
            {
                use sonde_gateway::key_provider::SecretServiceKeyProvider;
                Ok(Box::new(SecretServiceKeyProvider::new(
                    cli.key_label.clone(),
                )))
            }
            #[cfg(not(target_os = "linux"))]
            {
                Err(KeyProviderError::NotAvailable(
                    "secret-service backend is only available on Linux".into(),
                )
                .into())
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "sonde_gateway=info".into()),
        )
        .init();

    let cli = Cli::parse();

    info!(db = %cli.db, port = %cli.port, channel = cli.channel, "starting sonde-gateway");

    // 1. Load master key for at-rest PSK encryption (GW-0601a).
    //    Build the appropriate KeyProvider from CLI arguments, then invoke it.
    let provider = build_key_provider(&cli)?;
    let master_key = Zeroizing::new({
        let k = provider.load_master_key()?;
        *k
    });
    info!(provider = ?cli.key_provider, "master key loaded");

    // 2. Open persistent storage
    let storage = Arc::new(SqliteStorage::open(&cli.db, master_key)?);
    info!("storage opened: {}", cli.db);

    // 2a. Load or generate gateway identity (GW-1200, GW-1201)
    {
        use sonde_gateway::gateway_identity::GatewayIdentity;

        fn hex_short(bytes: &[u8]) -> String {
            bytes.iter().map(|b| format!("{b:02x}")).collect()
        }

        let existing = storage.load_gateway_identity().await?;
        match existing {
            Some(identity) => {
                info!(
                    gateway_id = %hex_short(identity.gateway_id()),
                    public_key_prefix = %hex_short(&identity.public_key()[..8]),
                    "gateway identity loaded"
                );
            }
            None => {
                let identity = GatewayIdentity::generate()
                    .map_err(|e| format!("failed to generate gateway identity: {e}"))?;
                storage.store_gateway_identity(&identity).await?;
                info!(
                    gateway_id = %hex_short(identity.gateway_id()),
                    public_key_prefix = %hex_short(&identity.public_key()[..8]),
                    "gateway identity generated and stored"
                );
            }
        }
    }

    // 3. Session manager
    let session_manager = Arc::new(SessionManager::new(Duration::from_secs(
        cli.session_timeout,
    )));

    // 4. Shared pending-command queue (admin → engine)
    let pending_commands: Arc<RwLock<HashMap<String, Vec<PendingCommand>>>> =
        Arc::new(RwLock::new(HashMap::new()));

    // 4. Gateway engine — wire up handler router when a config file was given
    let gateway = if let Some(config_path) = &cli.handler_config {
        let configs = load_handler_configs(config_path).map_err(|e| {
            error!("failed to load handler config: {e}");
            e
        })?;
        info!(
            path = %config_path.display(),
            count = configs.len(),
            "loaded handler config"
        );
        let router = Arc::new(HandlerRouter::new(configs));
        Arc::new(Gateway::new_with_handler(
            storage.clone(),
            Duration::from_secs(cli.session_timeout),
            router,
        ))
    } else {
        Arc::new(Gateway::new_with_pending(
            storage.clone(),
            pending_commands.clone(),
            session_manager.clone(),
        ))
    };

    // 6. Open serial port and create modem transport
    let serial_port =
        tokio_serial::SerialStream::open(&tokio_serial::new(&cli.port, cli.baud_rate))?;
    let transport = Arc::new(UsbEspNowTransport::new(serial_port, cli.channel).await?);
    info!(channel = cli.channel, "modem transport ready");

    // 7. Start gRPC admin server
    let ble_controller = Arc::new(sonde_gateway::ble_pairing::BlePairingController::new());
    let admin_service = AdminService::new(storage.clone(), pending_commands, session_manager)
        .with_ble(Arc::clone(&ble_controller), Arc::clone(&transport));
    let admin_socket = cli.admin_socket.clone();

    let grpc_handle = tokio::spawn(async move {
        if let Err(e) = sonde_gateway::admin::serve_admin(admin_service, &admin_socket).await {
            error!("gRPC server error: {}", e);
        }
    });

    // 8. Main frame-processing loop
    info!("entering frame processing loop");
    let transport_ref = transport.clone();

    let frame_loop = tokio::spawn(async move {
        loop {
            match transport_ref.recv().await {
                Ok((raw_frame, peer_addr)) => {
                    if let Some(response) =
                        gateway.process_frame(&raw_frame, peer_addr.clone()).await
                    {
                        if let Err(e) = transport_ref.send(&response, &peer_addr).await {
                            error!("send error: {}", e);
                        }
                    }
                }
                Err(e) => {
                    error!("recv error: {e}");
                    break;
                }
            }
        }
    });

    // 8a. BLE event processing loop (Phase 1 phone pairing via modem relay).
    let ble_transport = transport.clone();
    let ble_storage: Arc<dyn Storage> = storage.clone();
    let ble_channel = cli.channel;
    let ble_ctrl = Arc::clone(&ble_controller);
    let ble_loop = tokio::spawn(async move {
        use sonde_gateway::ble_pairing::handle_ble_recv;
        use sonde_gateway::modem::BleEvent;
        use tracing::{debug, warn};

        // Load gateway identity for BLE pairing operations.
        let identity = match ble_storage.load_gateway_identity().await {
            Ok(Some(id)) => id,
            Ok(None) => {
                error!("BLE loop: no gateway identity — cannot handle BLE pairing");
                return;
            }
            Err(e) => {
                error!("BLE loop: failed to load gateway identity: {e}");
                return;
            }
        };

        // Use the shared registration window via the controller.
        let mut window = sonde_gateway::ble_pairing::RegistrationWindow::new();

        loop {
            // Sync the local window state from the controller on each iteration.
            // The controller may have opened/closed it via admin RPCs.
            let controller_open = ble_ctrl.is_window_open().await;
            if controller_open && !window.is_open() {
                // Controller opened the window — sync to local state.
                // We don't know the exact deadline, but the controller
                // handles auto-close, so just open with a long duration.
                window.open(3600);
            } else if !controller_open && window.is_open() {
                window.close();
            }

            match ble_transport.recv_ble_event().await {
                Some(BleEvent::Recv(br)) => {
                    if let Some(response) = handle_ble_recv(
                        &br.ble_data,
                        &identity,
                        &ble_storage,
                        &mut window,
                        ble_channel,
                        Some(&ble_ctrl),
                    )
                    .await
                    {
                        if let Err(e) = ble_transport.send_ble_indicate(&response).await {
                            error!("BLE_INDICATE send error: {e}");
                        }
                    }
                }
                Some(BleEvent::Connected(bc)) => {
                    info!(
                        peer = ?bc.peer_addr,
                        mtu = bc.mtu,
                        "BLE phone connected"
                    );
                    ble_ctrl.broadcast_event(
                        sonde_gateway::ble_pairing::BlePairingEventKind::PhoneConnected {
                            peer_addr: bc.peer_addr,
                            mtu: bc.mtu,
                        },
                    );
                }
                Some(BleEvent::Disconnected(bd)) => {
                    info!(
                        peer = ?bd.peer_addr,
                        reason = bd.reason,
                        "BLE phone disconnected"
                    );
                    ble_ctrl.broadcast_event(
                        sonde_gateway::ble_pairing::BlePairingEventKind::PhoneDisconnected {
                            peer_addr: bd.peer_addr,
                        },
                    );
                }
                Some(BleEvent::PairingConfirm(pc)) => {
                    info!(
                        passkey = pc.passkey,
                        "BLE Numeric Comparison passkey — awaiting operator confirmation"
                    );
                    // Broadcast to admin CLI streams.
                    ble_ctrl.broadcast_event(
                        sonde_gateway::ble_pairing::BlePairingEventKind::PasskeyRequest {
                            passkey: pc.passkey,
                        },
                    );
                    // Forward to admin CLI via the controller. If no admin
                    // client is listening, wait up to 30s then auto-reject.
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    ble_ctrl.set_passkey_responder(tx).await;

                    let accept =
                        match tokio::time::timeout(std::time::Duration::from_secs(30), rx).await {
                            Ok(Ok(v)) => v,
                            _ => {
                                warn!("passkey confirmation timed out — rejecting");
                                false
                            }
                        };

                    if let Err(e) = ble_transport.send_ble_pairing_confirm_reply(accept).await {
                        error!("BLE_PAIRING_CONFIRM_REPLY send error: {e}");
                    }
                }
                None => {
                    debug!("BLE event channel closed");
                    break;
                }
            }
        }
    });

    // 9. Wait for shutdown
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("received ctrl-c, shutting down");
        }
        _ = frame_loop => {
            error!("frame processing loop exited");
        }
        _ = ble_loop => {
            error!("BLE event loop exited");
        }
        _ = grpc_handle => {
            error!("gRPC server exited");
        }
    }

    info!("gateway stopped");
    Ok(())
}
