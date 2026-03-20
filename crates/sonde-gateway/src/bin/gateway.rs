// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
#[cfg(windows)]
use clap::Subcommand;
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

// ── Windows NT service constants ─────────────────────────────────────────────
#[cfg(windows)]
const SERVICE_NAME: &str = "sonde-gateway";
#[cfg(windows)]
const SERVICE_DISPLAY_NAME: &str = "Sonde Gateway";
#[cfg(windows)]
const SERVICE_DESCRIPTION: &str = "Manages sensor nodes over ESP-NOW radio.";

// Static storage for parsed CLI args, shared between main() and the service
// entry point which runs on a separate thread dispatched by the SCM.
#[cfg(windows)]
static SERVICE_CLI: std::sync::OnceLock<Cli> = std::sync::OnceLock::new();

// ── Windows NT service entry point ───────────────────────────────────────────
// Must be defined at module scope (outside any function) per the macro contract.
#[cfg(windows)]
windows_service::define_windows_service!(ffi_service_main, service_entry);

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

/// Optional subcommands for Windows NT service management.
///
/// Without a subcommand the gateway runs as a normal console application.
#[cfg(windows)]
#[derive(Subcommand, Debug, Clone)]
enum ServiceCommand {
    /// Install sonde-gateway as a Windows NT service (requires Administrator).
    ///
    /// The service is registered to start automatically at boot. All gateway
    /// options supplied here (--port, --db, …) are embedded in the service
    /// registration and used on every subsequent start.
    Install,
    /// Remove the sonde-gateway Windows NT service registration (requires Administrator).
    ///
    /// If the service is running it is stopped first. The service entry is then
    /// permanently deleted from the Service Control Manager database.
    Uninstall,
}

/// Sonde gateway — manages sensor nodes over ESP-NOW radio.
#[derive(Parser, Debug, Clone)]
#[command(name = "sonde-gateway", version, about)]
struct Cli {
    /// Service management subcommand (Windows only).
    ///
    /// `install`   — Register as a Windows NT service (auto-start on boot).
    /// `uninstall` — Remove the service registration.
    ///
    /// Without a subcommand the gateway runs as a console application.
    #[cfg(windows)]
    #[command(subcommand)]
    command: Option<ServiceCommand>,

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

    /// Auto-generate the master key on first run if it does not already exist.
    ///
    /// When set, the gateway will generate a cryptographically random 32-byte
    /// master key via `getrandom::fill()` and write it to the configured
    /// backend (`--key-provider`) if no key is present.  If a key already
    /// exists it is loaded unchanged — no data loss, no overwrite.
    ///
    /// A warning is logged when a new key is generated so operators are aware.
    ///
    /// Not supported with `--key-provider env` (environment variables are
    /// read-only from the process).
    #[arg(long, default_value_t = false)]
    generate_master_key: bool,

    /// Run as a Windows NT service.
    ///
    /// This flag is set automatically by `sonde-gateway install` in the service
    /// registration and is passed by the Windows Service Control Manager when
    /// starting the service. Do not set this flag manually.
    #[cfg(windows)]
    #[arg(long, hide = true)]
    service: bool,

    /// Path to the log file used when running as a Windows NT service.
    ///
    /// Defaults to `<db-path>.log` (e.g., `sonde.log` when `--db sonde.db`).
    /// Has no effect in console mode.
    #[cfg(windows)]
    #[arg(long)]
    log_file: Option<PathBuf>,
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

/// Core gateway run loop.
///
/// Starts all subsystems (storage, transport, gRPC admin server, BLE loop, frame
/// processing loop) and runs until `shutdown` resolves or any subsystem exits.
async fn run_gateway(
    cli: &Cli,
    shutdown: tokio::sync::oneshot::Receiver<()>,
) -> Result<(), Box<dyn std::error::Error>> {
    info!(db = %cli.db, port = %cli.port, channel = cli.channel, "starting sonde-gateway");

    // 1. Load master key for at-rest PSK encryption (GW-0601a).
    //    Build the appropriate KeyProvider from CLI arguments, then invoke it.
    let provider = build_key_provider(cli)?;
    let master_key = Zeroizing::new(if cli.generate_master_key {
        let k = provider.generate_or_load_master_key()?;
        *k
    } else {
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

    // 5. Gateway engine — wire up handler router when a config file was given
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
    //
    // Drain any boot log garbage from the USB-CDC buffer before starting
    // the modem protocol. ESP32-S3 ROM and early IDF init may send text
    // to the USB endpoint before the console is routed to UART.
    let serial_port = {
        let port = serial2_tokio::SerialPort::open(&cli.port, cli.baud_rate)?;
        let mut drain_buf = [0u8; 4096];
        loop {
            match tokio::time::timeout(Duration::from_millis(500), port.read(&mut drain_buf)).await
            {
                Ok(Ok(n)) if n > 0 => {
                    info!("drained {n} bytes of boot garbage from serial port");
                }
                _ => break,
            }
        }
        port
    };
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
            let controller_open = ble_ctrl.is_window_open().await;
            if controller_open && !window.is_open() {
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
                    ble_ctrl.broadcast_event(
                        sonde_gateway::ble_pairing::BlePairingEventKind::PasskeyRequest {
                            passkey: pc.passkey,
                        },
                    );
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

    // 9. Wait for shutdown signal, or for any subsystem to exit unexpectedly.
    tokio::select! {
        _ = shutdown => {
            info!("shutdown signal received, stopping gateway");
        }
        _ = frame_loop => {
            error!("frame processing loop exited unexpectedly");
        }
        _ = ble_loop => {
            error!("BLE event loop exited unexpectedly");
        }
        _ = grpc_handle => {
            error!("gRPC server exited unexpectedly");
        }
    }

    info!("gateway stopped");
    Ok(())
}

// ── Windows NT service implementation ────────────────────────────────────────

/// Service entry point called by the Windows Service Control Manager.
///
/// Runs on a dedicated thread created by `service_dispatcher::start()`.
/// The CLI args are retrieved from [`SERVICE_CLI`] which is populated by
/// `main()` before calling `service_dispatcher::start()`.
#[cfg(windows)]
fn service_entry(_arguments: Vec<std::ffi::OsString>) {
    use std::sync::{Arc, Mutex};
    use windows_service::service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
    };
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};

    const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;

    let cli = match SERVICE_CLI.get() {
        Some(c) => c,
        None => {
            // This should never happen: main() stores the CLI before dispatching.
            return;
        }
    };

    // Channel used to signal the async gateway to shut down cleanly.
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    // Wrap in Arc<Mutex<Option<…>>> so the Fn closure can send exactly once.
    let shutdown_tx = Arc::new(Mutex::new(Some(shutdown_tx)));

    let event_handler = move |control_event| -> ServiceControlHandlerResult {
        match control_event {
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            ServiceControl::Stop | ServiceControl::Shutdown => {
                if let Ok(mut guard) = shutdown_tx.lock() {
                    if let Some(tx) = guard.take() {
                        let _ = tx.send(());
                    }
                }
                ServiceControlHandlerResult::NoError
            }
            _ => ServiceControlHandlerResult::NotImplemented,
        }
    };

    let status_handle = match service_control_handler::register(SERVICE_NAME, event_handler) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("sonde-gateway: failed to register service control handler: {e}");
            return;
        }
    };

    // Report that the service is now running.
    let running_status = ServiceStatus {
        service_type: SERVICE_TYPE,
        current_state: ServiceState::Running,
        controls_accepted: ServiceControlAccept::STOP,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    };
    if let Err(e) = status_handle.set_service_status(running_status) {
        eprintln!("sonde-gateway: failed to report Running status: {e}");
        return;
    }

    // Run the gateway on a fresh tokio runtime.
    let exit_code = match tokio::runtime::Runtime::new() {
        Ok(rt) => match rt.block_on(run_gateway(cli, shutdown_rx)) {
            Ok(()) => 0u32,
            Err(e) => {
                error!("gateway exited with error: {e}");
                1
            }
        },
        Err(e) => {
            eprintln!("sonde-gateway: failed to create tokio runtime: {e}");
            1
        }
    };

    // Report that the service has stopped.
    let stopped_status = ServiceStatus {
        service_type: SERVICE_TYPE,
        current_state: ServiceState::Stopped,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code: ServiceExitCode::Win32(exit_code),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    };
    let _ = status_handle.set_service_status(stopped_status);
}

/// Initialise tracing to write to a log file **and** emit ETW events (used in
/// service mode where there is no console).
///
/// The file layer honours `RUST_LOG`; the ETW layer receives all events so that
/// ETW-side filtering (via `logman` / `tracelog`) controls verbosity
/// independently.
///
/// The log file path defaults to `<db-path>.log` when `--log-file` is not set.
#[cfg(windows)]
fn init_service_logging(cli: &Cli) -> Result<(), Box<dyn std::error::Error>> {
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::EnvFilter;

    let log_path = cli.log_file.clone().unwrap_or_else(|| {
        let mut p = PathBuf::from(&cli.db);
        p.set_extension("log");
        p
    });

    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| format!("cannot open log file {}: {e}", log_path.display()))?;

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::sync::Mutex::new(file))
        .with_ansi(false)
        .with_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| "sonde_gateway=info".into()),
        );

    let etw_layer = tracing_etw::LayerBuilder::new("sonde-gateway")
        .build()
        .map_err(|e| format!("failed to initialise ETW provider: {e}"))?;

    tracing_subscriber::registry()
        .with(fmt_layer)
        .with(etw_layer)
        .init();

    Ok(())
}

/// Install sonde-gateway as an auto-start Windows NT service.
///
/// The current executable is registered under the name `sonde-gateway`.
/// All gateway options passed on the command line are embedded in the service
/// registration so they are used on every subsequent start by the SCM.
#[cfg(windows)]
fn install_service(_cli: &Cli) -> Result<(), Box<dyn std::error::Error>> {
    use std::ffi::OsString;
    use windows_service::service::{
        ServiceAccess, ServiceErrorControl, ServiceStartType, ServiceType,
    };
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )?;

    let exe_path = std::env::current_exe()?;

    // Reconstruct launch arguments from the original command line, replacing
    // the `install` subcommand with the `--service` flag so the SCM invokes
    // the binary in service mode on each start.
    let launch_args: Vec<OsString> = std::iter::once(OsString::from("--service"))
        .chain(
            std::env::args_os()
                .skip(1) // skip binary path
                .filter(|a| a.to_str() != Some("install")),
        )
        .collect();

    let service_info = windows_service::service::ServiceInfo {
        name: OsString::from(SERVICE_NAME),
        display_name: OsString::from(SERVICE_DISPLAY_NAME),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: exe_path,
        launch_arguments: launch_args,
        dependencies: vec![],
        account_name: None, // run as LocalSystem
        account_password: None,
    };

    let service = manager.create_service(&service_info, ServiceAccess::CHANGE_CONFIG)?;
    service.set_description(SERVICE_DESCRIPTION)?;

    println!("sonde-gateway service installed successfully.");
    println!("Start with: sc start {SERVICE_NAME}");
    Ok(())
}

/// Uninstall the sonde-gateway Windows NT service.
///
/// Stops the service if it is currently running, then removes it from the
/// Service Control Manager database.
#[cfg(windows)]
fn uninstall_service() -> Result<(), Box<dyn std::error::Error>> {
    use std::time::{Duration, Instant};
    use windows_service::service::{ServiceAccess, ServiceState};
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    use windows_sys::Win32::Foundation::ERROR_SERVICE_DOES_NOT_EXIST;

    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;

    let service = manager.open_service(
        SERVICE_NAME,
        ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE,
    )?;

    // Mark the service for deletion. It won't actually be removed until all
    // open handles are closed and the service is stopped.
    service.delete()?;

    if service.query_status()?.current_state != ServiceState::Stopped {
        service.stop()?;
        println!("Stopping sonde-gateway service…");
    }

    // Close our handle so the SCM can remove the entry.
    drop(service);

    // Poll until the service disappears from the SCM database (≤5 s).
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match manager.open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS) {
            Err(windows_service::Error::Winapi(e))
                if e.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST as i32) =>
            {
                println!("sonde-gateway service uninstalled successfully.");
                return Ok(());
            }
            _ => std::thread::sleep(Duration::from_millis(500)),
        }
    }

    println!("sonde-gateway service is marked for deletion.");
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    // ── Windows NT service dispatch ──────────────────────────────────────────
    #[cfg(windows)]
    {
        match &cli.command {
            Some(ServiceCommand::Install) => return install_service(&cli),
            Some(ServiceCommand::Uninstall) => return uninstall_service(),
            None => {}
        }

        if cli.service {
            // Running as a Windows NT service (invoked by the SCM).
            // Initialise file-based logging first (no console available).
            init_service_logging(&cli)?;
            SERVICE_CLI
                .set(cli)
                .expect("SERVICE_CLI already set — duplicate service dispatch?");
            windows_service::service_dispatcher::start(SERVICE_NAME, ffi_service_main)?;
            return Ok(());
        }
    }

    // ── Console mode (default on all platforms) ──────────────────────────────
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "sonde_gateway=info".into()),
        )
        .init();

    // Drive ctrl-c into a oneshot so run_gateway has a uniform shutdown interface.
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            let _ = shutdown_tx.send(());
        }
    });

    run_gateway(&cli, shutdown_rx).await
}
