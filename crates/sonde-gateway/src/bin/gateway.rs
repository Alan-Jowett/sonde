// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Local, Utc};
use clap::Parser;
#[cfg(windows)]
use clap::Subcommand;
use sonde_protocol::modem::{BUTTON_TYPE_LONG, BUTTON_TYPE_SHORT, DISPLAY_FRAME_BODY_SIZE};
use tokio::sync::RwLock;
use tracing::{error, info, warn};

use sonde_gateway::display_banner::{
    render_display_message, render_status_text_page, send_display_message,
    send_gateway_version_banner, ScrollableFramebuffer, STATUS_TEXT_COLUMNS,
};
use sonde_gateway::engine::{resolve_espnow_channel, Gateway, PendingCommand};
use sonde_gateway::handler::{load_handler_configs, HandlerRouter};
use sonde_gateway::key_provider::{EnvKeyProvider, FileKeyProvider, KeyProvider, KeyProviderError};
use sonde_gateway::modem::UsbEspNowTransport;
use sonde_gateway::registry::NodeRecord;
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

/// Maximum time to wait for graceful shutdown before force-exiting (GW-1400).
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const BUTTON_PAIRING_DURATION_S: u32 = 120;
const BUTTON_EXIT_REASON_DISPLAY_DURATION: Duration = Duration::from_secs(2);
const STATUS_PAGE_TIMEOUT: Duration = Duration::from_secs(60);
const NODE_STATUS_SCROLL_INTERVAL: Duration = Duration::from_millis(50);
const NODE_STATUS_SCROLL_STEP_PX: u32 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ButtonDisplayState {
    Generic,
    Passkey,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StatusPage {
    Channel,
    Nodes,
}

impl StatusPage {
    const ALL: [StatusPage; 2] = [StatusPage::Channel, StatusPage::Nodes];
}

#[derive(Debug, Default)]
struct StatusPageCycle {
    next_page_index: usize,
}

struct ActiveStatusPageScroll {
    stop_requested: Arc<AtomicBool>,
    handle: tokio::task::JoinHandle<()>,
}

type StatusPageScrollTask = Arc<tokio::sync::Mutex<Option<ActiveStatusPageScroll>>>;

enum RenderedStatusPage {
    Static(Box<[u8; DISPLAY_FRAME_BODY_SIZE]>),
    Scrollable(ScrollableFramebuffer),
}

impl RenderedStatusPage {
    fn initial_frame(&self) -> [u8; DISPLAY_FRAME_BODY_SIZE] {
        match self {
            Self::Static(frame) => **frame,
            Self::Scrollable(framebuffer) => framebuffer.visible_window(0),
        }
    }

    fn scrollable_frame(&self) -> Option<&ScrollableFramebuffer> {
        match self {
            Self::Scrollable(framebuffer) if framebuffer.is_scrollable() => Some(framebuffer),
            _ => None,
        }
    }
}

async fn update_display_message(transport: &Arc<UsbEspNowTransport>, lines: &[&str]) {
    if let Err(e) = send_display_message(transport, lines).await {
        warn!(error = %e, ?lines, "failed to update display");
    }
}

fn invalidate_display_restore(display_generation: &Arc<AtomicU64>) {
    display_generation.fetch_add(1, Ordering::SeqCst);
}

async fn reset_status_page_cycle(status_page_cycle: &Arc<tokio::sync::Mutex<StatusPageCycle>>) {
    status_page_cycle.lock().await.next_page_index = 0;
}

fn format_epoch_ms(ms: u64) -> String {
    let Ok(ms_i64) = i64::try_from(ms) else {
        return format!("<invalid timestamp: {ms}>");
    };

    DateTime::<Utc>::from_timestamp_millis(ms_i64)
        .map(|dt| dt.with_timezone(&Local).format("%c").to_string())
        .unwrap_or_else(|| format!("<invalid timestamp: {ms}>"))
}

fn format_system_time_for_display(timestamp: SystemTime) -> String {
    match timestamp.duration_since(UNIX_EPOCH) {
        Ok(duration) => {
            let ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
            format_epoch_ms(ms)
        }
        Err(_) => "<invalid timestamp>".to_string(),
    }
}

fn split_text_chunks(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return vec![String::new()];
    }

    let mut chunks = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let end = (start + width).min(chars.len());
        chunks.push(chars[start..end].iter().collect());
        start = end;
    }
    chunks
}

fn push_wrapped_text_line(lines: &mut Vec<String>, text: &str) {
    lines.extend(split_text_chunks(text, STATUS_TEXT_COLUMNS));
}

fn push_wrapped_property_value(lines: &mut Vec<String>, property: &str, value: &str) {
    push_wrapped_text_line(lines, property);

    let value_prefix = "- ";
    let continuation = " ".repeat(value_prefix.chars().count());
    let value_chunks = split_text_chunks(
        value,
        STATUS_TEXT_COLUMNS.saturating_sub(value_prefix.len()),
    );
    for (index, chunk) in value_chunks.into_iter().enumerate() {
        if index == 0 {
            lines.push(format!("{value_prefix}{chunk}"));
        } else {
            lines.push(format!("{continuation}{chunk}"));
        }
    }
}

fn build_node_status_lines(nodes: &[NodeRecord]) -> Vec<String> {
    if nodes.is_empty() {
        return vec!["No nodes registered.".to_string()];
    }

    let mut sorted_nodes: Vec<&NodeRecord> = nodes.iter().collect();
    sorted_nodes.sort_by(|left, right| left.node_id.cmp(&right.node_id));

    let mut lines = Vec::new();
    for (index, node) in sorted_nodes.into_iter().enumerate() {
        if index > 0 {
            lines.push(String::new());
        }
        push_wrapped_property_value(&mut lines, "node id", &node.node_id);
        if let Some(hash) = node.assigned_program_hash.as_ref() {
            push_wrapped_property_value(&mut lines, "assigned program", &hex::encode(hash));
        }
        if let Some(hash) = node.current_program_hash.as_ref() {
            push_wrapped_property_value(&mut lines, "current program", &hex::encode(hash));
        }
        if let Some(mv) = node.last_battery_mv {
            push_wrapped_property_value(&mut lines, "battery", &format!("{mv} mV"));
        }
        if let Some(last_seen) = node.last_seen {
            push_wrapped_property_value(
                &mut lines,
                "last seen",
                &format_system_time_for_display(last_seen),
            );
        }
        push_wrapped_property_value(
            &mut lines,
            "schedule",
            &format!("{}s", node.schedule_interval_s),
        );
    }

    lines
}

async fn render_status_page(
    storage: &Arc<dyn Storage>,
    default_channel: u8,
    page: StatusPage,
) -> RenderedStatusPage {
    match page {
        StatusPage::Channel => {
            let lines = match storage.get_config("espnow_channel").await {
                Ok(Some(channel)) => ["Channel".to_string(), channel],
                Ok(None) => ["Channel".to_string(), default_channel.to_string()],
                Err(e) => {
                    warn!(error = %e, "failed to load espnow_channel for status page");
                    ["Channel".to_string(), "Error".to_string()]
                }
            };
            let line_refs = [lines[0].as_str(), lines[1].as_str()];
            RenderedStatusPage::Static(Box::new(render_display_message(&line_refs)))
        }
        StatusPage::Nodes => match storage.list_nodes().await {
            Ok(nodes) => RenderedStatusPage::Scrollable(render_status_text_page(
                &build_node_status_lines(&nodes),
            )),
            Err(e) => {
                warn!(error = %e, "failed to load nodes for status page");
                RenderedStatusPage::Static(Box::new(render_display_message(&["Nodes", "Error"])))
            }
        },
    }
}

fn schedule_button_pairing_banner_restore(
    transport: &Arc<UsbEspNowTransport>,
    controller: &Arc<sonde_gateway::ble_pairing::BlePairingController>,
    display_generation: &Arc<AtomicU64>,
) {
    let generation = display_generation.fetch_add(1, Ordering::SeqCst) + 1;
    let transport: Weak<UsbEspNowTransport> = Arc::downgrade(transport);
    let controller = Arc::clone(controller);
    let display_generation = Arc::clone(display_generation);
    tokio::spawn(async move {
        tokio::time::sleep(BUTTON_EXIT_REASON_DISPLAY_DURATION).await;
        if display_generation.load(Ordering::SeqCst) != generation {
            return;
        }
        if controller.session_origin().await.is_some() {
            return;
        }
        let Some(transport) = transport.upgrade() else {
            return;
        };
        if let Err(e) = send_gateway_version_banner(&transport).await {
            warn!(error = %e, "failed to restore gateway version banner");
        }
    });
}

async fn cancel_status_page_scroll(scroll_task: &StatusPageScrollTask) {
    let active = scroll_task.lock().await.take();
    if let Some(active) = active {
        active.stop_requested.store(true, Ordering::SeqCst);
        let _ = active.handle.await;
    }
}

fn schedule_status_page_banner_restore(
    transport: &Arc<UsbEspNowTransport>,
    controller: &Arc<sonde_gateway::ble_pairing::BlePairingController>,
    display_generation: &Arc<AtomicU64>,
    status_page_cycle: &Arc<tokio::sync::Mutex<StatusPageCycle>>,
    scroll_task: &StatusPageScrollTask,
) -> u64 {
    let generation = display_generation.fetch_add(1, Ordering::SeqCst) + 1;
    let transport: Weak<UsbEspNowTransport> = Arc::downgrade(transport);
    let controller = Arc::clone(controller);
    let display_generation = Arc::clone(display_generation);
    let status_page_cycle = Arc::clone(status_page_cycle);
    let scroll_task = Arc::clone(scroll_task);
    tokio::spawn(async move {
        tokio::time::sleep(STATUS_PAGE_TIMEOUT).await;
        if display_generation.load(Ordering::SeqCst) != generation {
            return;
        }
        if controller.session_origin().await.is_some() {
            return;
        }
        display_generation.fetch_add(1, Ordering::SeqCst);
        cancel_status_page_scroll(&scroll_task).await;
        reset_status_page_cycle(&status_page_cycle).await;
        let Some(transport) = transport.upgrade() else {
            return;
        };
        if let Err(e) = send_gateway_version_banner(&transport).await {
            warn!(error = %e, "failed to restore gateway version banner");
        }
    });
    generation
}

async fn schedule_status_page_scroll(
    transport: &Arc<UsbEspNowTransport>,
    controller: &Arc<sonde_gateway::ble_pairing::BlePairingController>,
    display_generation: &Arc<AtomicU64>,
    generation: u64,
    framebuffer: &ScrollableFramebuffer,
    scroll_task: &StatusPageScrollTask,
) {
    if !framebuffer.is_scrollable() {
        return;
    }

    let transport: Weak<UsbEspNowTransport> = Arc::downgrade(transport);
    let controller = Arc::clone(controller);
    let display_generation = Arc::clone(display_generation);
    let framebuffer = framebuffer.clone();
    let stop_requested = Arc::new(AtomicBool::new(false));
    let stop_requested_for_task = Arc::clone(&stop_requested);
    let task = tokio::spawn(async move {
        let mut offset_y = 0;
        let mut ticker = tokio::time::interval(NODE_STATUS_SCROLL_INTERVAL);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            if stop_requested_for_task.load(Ordering::SeqCst) {
                return;
            }
            if display_generation.load(Ordering::SeqCst) != generation {
                return;
            }
            if controller.session_origin().await.is_some() {
                return;
            }
            let Some(transport) = transport.upgrade() else {
                return;
            };

            let scroll_end_offset = framebuffer.scroll_end_offset();
            offset_y = if offset_y >= scroll_end_offset {
                0
            } else {
                (offset_y + NODE_STATUS_SCROLL_STEP_PX).min(scroll_end_offset)
            };

            if let Err(e) = transport
                .send_display_frame(framebuffer.visible_window(offset_y))
                .await
            {
                warn!(error = %e, offset_y, "failed to scroll node status page");
                return;
            }
            if stop_requested_for_task.load(Ordering::SeqCst) {
                return;
            }
        }
    });
    *scroll_task.lock().await = Some(ActiveStatusPageScroll {
        stop_requested,
        handle: task,
    });
}

async fn open_button_pairing_session(
    transport: &Arc<UsbEspNowTransport>,
    controller: &Arc<sonde_gateway::ble_pairing::BlePairingController>,
    display_generation: &Arc<AtomicU64>,
    status_page_cycle: &Arc<tokio::sync::Mutex<StatusPageCycle>>,
    scroll_task: &StatusPageScrollTask,
    display_state: &mut ButtonDisplayState,
    window: &mut sonde_gateway::ble_pairing::RegistrationWindow,
) -> bool {
    use sonde_gateway::ble_pairing::PairingOrigin;

    if controller.session_origin().await.is_some() {
        return false;
    }
    if !controller
        .open_window(BUTTON_PAIRING_DURATION_S, PairingOrigin::Button)
        .await
    {
        return false;
    }
    cancel_status_page_scroll(scroll_task).await;
    if let Err(e) = transport.send_ble_enable().await {
        controller.close_window().await;
        error!("BLE_ENABLE send error for button pairing: {e}");
        return false;
    }
    invalidate_display_restore(display_generation);
    reset_status_page_cycle(status_page_cycle).await;
    window.open(BUTTON_PAIRING_DURATION_S);
    *display_state = ButtonDisplayState::Generic;
    info!("button-initiated BLE pairing opened");
    update_display_message(transport, &["Pairing"]).await;
    true
}

async fn close_button_pairing_session(
    transport: &Arc<UsbEspNowTransport>,
    controller: &Arc<sonde_gateway::ble_pairing::BlePairingController>,
    display_generation: &Arc<AtomicU64>,
    status_page_cycle: &Arc<tokio::sync::Mutex<StatusPageCycle>>,
    display_state: &mut ButtonDisplayState,
    window: &mut sonde_gateway::ble_pairing::RegistrationWindow,
    status_lines: &[&str],
) {
    controller.close_window().await;
    window.close();
    *display_state = ButtonDisplayState::Generic;
    if let Err(e) = transport.send_ble_disable().await {
        error!("BLE_DISABLE send error: {e}");
    }
    reset_status_page_cycle(status_page_cycle).await;
    update_display_message(transport, status_lines).await;
    schedule_button_pairing_banner_restore(transport, controller, display_generation);
}

async fn handle_button_short_event(
    transport: &Arc<UsbEspNowTransport>,
    controller: &Arc<sonde_gateway::ble_pairing::BlePairingController>,
    display_generation: &Arc<AtomicU64>,
    status_page_cycle: &Arc<tokio::sync::Mutex<StatusPageCycle>>,
    display_state: &mut ButtonDisplayState,
    window: &mut sonde_gateway::ble_pairing::RegistrationWindow,
) -> bool {
    if controller.session_origin().await != Some(sonde_gateway::ble_pairing::PairingOrigin::Button)
    {
        return false;
    }
    close_button_pairing_session(
        transport,
        controller,
        display_generation,
        status_page_cycle,
        display_state,
        window,
        &["Cancelled"],
    )
    .await;
    true
}

async fn handle_button_pairing_timeout(
    transport: &Arc<UsbEspNowTransport>,
    controller: &Arc<sonde_gateway::ble_pairing::BlePairingController>,
    display_generation: &Arc<AtomicU64>,
    status_page_cycle: &Arc<tokio::sync::Mutex<StatusPageCycle>>,
    display_state: &mut ButtonDisplayState,
    window: &mut sonde_gateway::ble_pairing::RegistrationWindow,
) {
    if controller.session_origin_raw().await
        != Some(sonde_gateway::ble_pairing::PairingOrigin::Button)
    {
        return;
    }
    info!("button-initiated BLE pairing timed out");
    close_button_pairing_session(
        transport,
        controller,
        display_generation,
        status_page_cycle,
        display_state,
        window,
        &["Timed out"],
    )
    .await;
}

async fn show_button_pairing_connected(
    transport: &Arc<UsbEspNowTransport>,
    controller: &Arc<sonde_gateway::ble_pairing::BlePairingController>,
    display_state: ButtonDisplayState,
) {
    if controller.session_origin().await == Some(sonde_gateway::ble_pairing::PairingOrigin::Button)
        && display_state != ButtonDisplayState::Passkey
    {
        update_display_message(transport, &["Phone connected"]).await;
    }
}

async fn confirm_button_pairing_passkey(
    transport: &Arc<UsbEspNowTransport>,
    controller: &Arc<sonde_gateway::ble_pairing::BlePairingController>,
    display_state: &mut ButtonDisplayState,
    passkey: u32,
) -> bool {
    if controller.session_origin().await != Some(sonde_gateway::ble_pairing::PairingOrigin::Button)
    {
        return false;
    }
    *display_state = ButtonDisplayState::Passkey;
    let passkey_text = format!("{passkey:06}");
    update_display_message(transport, &["Pin", &passkey_text]).await;
    if let Err(e) = transport.send_ble_pairing_confirm_reply(true).await {
        error!("BLE_PAIRING_CONFIRM_REPLY send error: {e}");
        return false;
    }
    true
}

async fn complete_button_pairing_success(
    transport: &Arc<UsbEspNowTransport>,
    controller: &Arc<sonde_gateway::ble_pairing::BlePairingController>,
    display_generation: &Arc<AtomicU64>,
    status_page_cycle: &Arc<tokio::sync::Mutex<StatusPageCycle>>,
    display_state: &mut ButtonDisplayState,
    window: &mut sonde_gateway::ble_pairing::RegistrationWindow,
) {
    update_display_message(transport, &["Provisioned"]).await;
    controller.close_window().await;
    window.close();
    *display_state = ButtonDisplayState::Generic;
    if let Err(e) = transport.send_ble_disable().await {
        error!("BLE_DISABLE send error after phone registration: {e}");
    }
    reset_status_page_cycle(status_page_cycle).await;
    update_display_message(transport, &["Done"]).await;
    schedule_button_pairing_banner_restore(transport, controller, display_generation);
}

async fn handle_idle_button_short_event(
    transport: &Arc<UsbEspNowTransport>,
    controller: &Arc<sonde_gateway::ble_pairing::BlePairingController>,
    storage: &Arc<dyn Storage>,
    default_channel: u8,
    display_generation: &Arc<AtomicU64>,
    status_page_cycle: &Arc<tokio::sync::Mutex<StatusPageCycle>>,
    scroll_task: &StatusPageScrollTask,
) -> bool {
    if controller.session_origin().await.is_some() {
        return false;
    }

    cancel_status_page_scroll(scroll_task).await;
    invalidate_display_restore(display_generation);
    let page = {
        let mut cycle = status_page_cycle.lock().await;
        let page = StatusPage::ALL[cycle.next_page_index % StatusPage::ALL.len()];
        cycle.next_page_index = (cycle.next_page_index + 1) % StatusPage::ALL.len();
        page
    };
    let rendered_page = render_status_page(storage, default_channel, page).await;
    let initial_frame = rendered_page.initial_frame();
    let initial_send_ok = match transport.send_display_frame(initial_frame).await {
        Ok(()) => true,
        Err(e) => {
            warn!(error = %e, ?page, "failed to update status page");
            false
        }
    };
    let generation = schedule_status_page_banner_restore(
        transport,
        controller,
        display_generation,
        status_page_cycle,
        scroll_task,
    );
    if initial_send_ok {
        if let Some(framebuffer) = rendered_page.scrollable_frame() {
            schedule_status_page_scroll(
                transport,
                controller,
                display_generation,
                generation,
                framebuffer,
                scroll_task,
            )
            .await;
        }
    }
    true
}

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

/// Default tracing filter for service mode (shared between init and reload).
#[cfg(all(windows, debug_assertions))]
const SERVICE_DEFAULT_FILTER: &str = "sonde_gateway=info";
#[cfg(all(windows, not(debug_assertions)))]
const SERVICE_DEFAULT_FILTER: &str = "sonde_gateway=warn";

/// Reload handle for the file-sink log filter, set by `init_service_logging`
/// and consumed by the service control handler on `ParamChange`.
#[cfg(windows)]
type ReloadHandle =
    tracing_subscriber::reload::Handle<tracing_subscriber::EnvFilter, tracing_subscriber::Registry>;
#[cfg(windows)]
static LOG_RELOAD_HANDLE: std::sync::OnceLock<ReloadHandle> = std::sync::OnceLock::new();

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
    /// Retrieve the key from the Linux D-Bus Secret Service keyring (requires `keyring` feature).
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
#[command(name = "sonde-gateway", version = concat!(env!("CARGO_PKG_VERSION"), " (", env!("SONDE_GIT_COMMIT"), ")"), about)]
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
    /// - `secret-service` — Retrieve from the Linux D-Bus Secret Service keyring (Linux with `keyring` feature).
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

    /// RSSI threshold (dBm) at or above which signal quality is "good".
    #[arg(long, default_value_t = -60)]
    rssi_good_threshold: i8,

    /// RSSI threshold (dBm) at or below which signal quality is "bad".
    #[arg(long, default_value_t = -75)]
    rssi_bad_threshold: i8,

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
            #[cfg(all(target_os = "linux", feature = "keyring"))]
            {
                use sonde_gateway::key_provider::SecretServiceKeyProvider;
                Ok(Box::new(SecretServiceKeyProvider::new(
                    cli.key_label.clone(),
                )))
            }
            #[cfg(not(all(target_os = "linux", feature = "keyring")))]
            {
                Err(KeyProviderError::NotAvailable(
                    "secret-service backend is only available on Linux with the `keyring` feature"
                        .into(),
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
    info!(
        version = concat!(env!("CARGO_PKG_VERSION"), " (", env!("SONDE_GIT_COMMIT"), ")"),
        db = %cli.db,
        port = %cli.port,
        channel = cli.channel,
        "starting sonde-gateway"
    );
    let mut shutdown = shutdown;

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
    let storage: Arc<SqliteStorage> = Arc::new(SqliteStorage::open(&cli.db, master_key)?);
    info!("storage opened: {}", cli.db);

    // 2b. Seed or load persisted ESP-NOW channel (GW-0808).
    //     If the database already has a channel, use it (ignoring --channel).
    //     Otherwise, seed the database with the CLI --channel value.
    let persisted_channel: u8 = resolve_espnow_channel(&*storage, cli.channel)
        .await
        .map_err(|e| format!("ESP-NOW channel resolution failed: {e}"))?;
    info!(
        persisted_channel,
        cli_channel = cli.channel,
        "ESP-NOW channel resolved (GW-0808)"
    );

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

    // 5. Gateway engine — always build HandlerRouter from DB (GW-1407).
    //    If --handler-config is provided, bootstrap YAML into DB first (GW-1405).
    if let Some(config_path) = &cli.handler_config {
        let configs = load_handler_configs(config_path).map_err(|e| {
            error!("failed to load handler config: {e}");
            e
        })?;
        info!(
            path = %config_path.display(),
            count = configs.len(),
            "loaded handler config — bootstrapping into database"
        );
        // Bootstrap: import each config into DB (GW-1405 §19.6).
        for cfg in &configs {
            for matcher in &cfg.matchers {
                use sonde_gateway::handler::ProgramMatcher;
                let program_hash = match matcher {
                    ProgramMatcher::Any => "*".to_string(),
                    ProgramMatcher::Hash(bytes) => {
                        bytes.iter().map(|b| format!("{b:02x}")).collect()
                    }
                };
                let record = sonde_gateway::storage::HandlerRecord {
                    program_hash,
                    command: cfg.command.clone(),
                    args: cfg.args.clone(),
                    working_dir: cfg.working_dir.clone(),
                    reply_timeout_ms: cfg.reply_timeout.map(|d| d.as_millis() as u64),
                };
                match storage.add_handler(&record).await {
                    Ok(true) => {
                        info!(program_hash = %record.program_hash, "bootstrapped handler into DB")
                    }
                    Ok(false) => {} // duplicate, DB takes precedence
                    Err(e) => {
                        warn!(program_hash = %record.program_hash, error = %e, "failed to bootstrap handler")
                    }
                }
            }
        }
    }

    // Always load handlers from DB and build the router (GW-1407).
    let handler_configs_from_db: Vec<sonde_gateway::handler::HandlerConfig> = storage
        .list_handlers()
        .await?
        .into_iter()
        .filter_map(sonde_gateway::admin::handler_record_to_config)
        .collect();
    info!(
        count = handler_configs_from_db.len(),
        "loaded handler configs from database"
    );
    let handler_router = Arc::new(tokio::sync::RwLock::new(HandlerRouter::new(
        handler_configs_from_db.clone(),
    )));

    // Create gateway engine with the shared handler router (D-485, GW-1407).
    let gateway = Arc::new({
        let mut gw = Gateway::new_with_pending(
            storage.clone(),
            pending_commands.clone(),
            session_manager.clone(),
            handler_router.clone(),
        );
        gw.set_rssi_thresholds(cli.rssi_good_threshold, cli.rssi_bad_threshold);
        gw
    });

    // 6–9. Modem transport + processing loops with reconnection (GW-1103).
    //
    // If the serial port disconnects (e.g. modem reset / USB-CDC unplug),
    // the reader task exits and the frame/BLE loops break.  Instead of
    // exiting the gateway process, we log the error, wait with backoff,
    // and rebuild the transport from scratch.
    let mut backoff = Duration::from_secs(1);
    const MAX_BACKOFF: Duration = Duration::from_secs(30);

    loop {
        // 6. Open serial port and create modem transport
        let serial_port = match async {
            let port = serial2_tokio::SerialPort::open(&cli.port, cli.baud_rate)?;
            let mut drain_buf = [0u8; 4096];
            loop {
                match tokio::time::timeout(Duration::from_millis(500), port.read(&mut drain_buf))
                    .await
                {
                    Ok(Ok(n)) if n > 0 => {
                        info!("drained {n} bytes of boot garbage from serial port");
                    }
                    _ => break,
                }
            }
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(port)
        }
        .await
        {
            Ok(port) => port,
            Err(e) => {
                error!(
                    operation = "serial port open",
                    port = %cli.port,
                    error = %e,
                    guidance = "check that the port exists, is not in use by another \
                                process, and that the current user has permission to \
                                access it",
                    "failed to open serial port"
                );
                info!("retrying in {}s…", backoff.as_secs());
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(MAX_BACKOFF);
                continue;
            }
        };

        // GW-1301: log serial connection.
        info!("modem serial connected");

        // GW-0808: read the current channel from the database on each
        // reconnect iteration so that `SetModemChannel` changes survive
        // modem restarts.
        let channel_for_transport = match storage.get_config("espnow_channel").await {
            Ok(Some(v)) => v.parse::<u8>().unwrap_or(persisted_channel),
            _ => persisted_channel,
        };

        let transport = match UsbEspNowTransport::new(serial_port, channel_for_transport).await {
            Ok(t) => Arc::new(t),
            Err(e) => {
                error!(
                    operation = "modem startup handshake",
                    port = %cli.port,
                    channel = channel_for_transport,
                    error = %e,
                    guidance = "check modem firmware and serial connection; \
                                the gateway will retry automatically",
                    "modem startup failed"
                );
                info!("retrying in {}s…", backoff.as_secs());
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(MAX_BACKOFF);
                continue;
            }
        };
        info!(channel = channel_for_transport, "modem transport ready");
        let warm_reboot_notify = transport.warm_reboot_notify();
        let warm_reboot_flag = transport.warm_reboot_flag();
        if let Err(e) = send_gateway_version_banner(&transport).await {
            error!(
                error = %e,
                version = env!("CARGO_PKG_VERSION"),
                guidance = "reliable display transfer failed; gateway will reconnect to recover modem transport state",
                "failed to send gateway version banner to modem display"
            );
            let immediate_reconnect = warm_reboot_flag.load(std::sync::atomic::Ordering::Acquire);
            transport.abort_reader_and_wait().await;
            drop(transport);
            if immediate_reconnect {
                info!(
                    "modem warm reboot detected during banner transfer — reconnecting immediately"
                );
                continue;
            }
            info!("retrying in {}s…", backoff.as_secs());
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(MAX_BACKOFF);
            continue;
        }
        backoff = Duration::from_secs(1); // reset on success

        // Re-create the admin service and spawn a fresh gRPC server on each
        // reconnect iteration to bind to the new transport reference.
        let ble_controller = Arc::new(sonde_gateway::ble_pairing::BlePairingController::new());
        let admin_service = AdminService::new(
            storage.clone(),
            pending_commands.clone(),
            session_manager.clone(),
        )
        .with_ble(Arc::clone(&ble_controller), Arc::clone(&transport))
        .with_handler_configs(handler_configs_from_db.clone())
        .with_handler_router(handler_router.clone());
        let admin_socket = cli.admin_socket.clone();

        let mut grpc_handle = tokio::spawn(async move {
            if let Err(e) = sonde_gateway::admin::serve_admin(admin_service, &admin_socket).await {
                error!("gRPC server error: {}", e);
            }
        });

        // 8. Main frame-processing loop
        info!("entering frame processing loop");
        let transport_ref = transport.clone();
        let gateway_ref = gateway.clone();

        let mut frame_loop = tokio::spawn(async move {
            loop {
                match transport_ref.recv_with_rssi().await {
                    Ok((raw_frame, peer_addr, rssi)) => {
                        if let Some(response) = gateway_ref
                            .process_frame_with_rssi(&raw_frame, peer_addr.clone(), Some(rssi))
                            .await
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
        // GW-0808: read channel from the database for each BLE pairing request
        // rather than capturing the CLI startup value.
        let ble_channel = channel_for_transport;
        let ble_ctrl = Arc::clone(&ble_controller);
        let button_display_generation = Arc::new(AtomicU64::new(0));
        let status_page_cycle = Arc::new(tokio::sync::Mutex::new(StatusPageCycle::default()));
        let status_page_scroll_task: StatusPageScrollTask = Arc::new(tokio::sync::Mutex::new(None));
        let mut ble_loop = tokio::spawn(async move {
            use sonde_gateway::ble_pairing::{handle_ble_recv, PairingOrigin};
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
            let button_timeout = tokio::time::sleep(Duration::from_secs(24 * 60 * 60));
            tokio::pin!(button_timeout);
            let mut button_timeout_armed = false;
            let mut button_display_state = ButtonDisplayState::Generic;

            loop {
                // Sync the local window state from the controller on each iteration.
                let controller_origin = ble_ctrl.session_origin().await;
                let controller_open = controller_origin.is_some();
                if controller_open && !window.is_open() {
                    window.open(3600);
                    if controller_origin == Some(PairingOrigin::Admin) {
                        cancel_status_page_scroll(&status_page_scroll_task).await;
                        invalidate_display_restore(&button_display_generation);
                        reset_status_page_cycle(&status_page_cycle).await;
                        if let Err(e) = send_gateway_version_banner(&ble_transport).await {
                            warn!(
                                error = %e,
                                "failed to restore gateway version banner for admin BLE pairing"
                            );
                        }
                    }
                } else if !controller_open && window.is_open() {
                    window.close();
                    button_timeout_armed = false;
                    button_display_state = ButtonDisplayState::Generic;
                }

                let recv_ble_event = ble_transport.recv_ble_event();
                tokio::pin!(recv_ble_event);

                let event = tokio::select! {
                    biased;
                    _ = &mut button_timeout, if button_timeout_armed => {
                        button_timeout_armed = false;
                        handle_button_pairing_timeout(
                            &ble_transport,
                            &ble_ctrl,
                            &button_display_generation,
                            &status_page_cycle,
                            &mut button_display_state,
                            &mut window,
                        )
                        .await;
                        continue;
                    }
                    event = &mut recv_ble_event => event,
                };

                match event {
                    Some(BleEvent::Recv(br)) => {
                        // GW-0808: read the current channel from the database
                        // so BLE pairing always returns the latest persisted
                        // channel, even if SetModemChannel was called after
                        // the BLE loop started.
                        let current_channel = match ble_storage.get_config("espnow_channel").await {
                            Ok(Some(v)) => v.parse::<u8>().unwrap_or(ble_channel),
                            _ => ble_channel,
                        };
                        if let Some(response) = handle_ble_recv(
                            &br.ble_data,
                            &identity,
                            &ble_storage,
                            &mut window,
                            current_channel,
                            Some(&ble_ctrl),
                        )
                        .await
                        {
                            let sent =
                                if let Err(e) = ble_transport.send_ble_indicate(&response).await {
                                    error!("BLE_INDICATE send error: {e}");
                                    false
                                } else {
                                    true
                                };

                            if sent
                                && ble_ctrl.session_origin_raw().await
                                    == Some(PairingOrigin::Button)
                                && ble_ctrl.take_successful_registration().await
                            {
                                complete_button_pairing_success(
                                    &ble_transport,
                                    &ble_ctrl,
                                    &button_display_generation,
                                    &status_page_cycle,
                                    &mut button_display_state,
                                    &mut window,
                                )
                                .await;
                                button_timeout_armed = false;
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
                        show_button_pairing_connected(
                            &ble_transport,
                            &ble_ctrl,
                            button_display_state,
                        )
                        .await;
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
                        ble_ctrl.broadcast_event(
                            sonde_gateway::ble_pairing::BlePairingEventKind::PasskeyRequest {
                                passkey: pc.passkey,
                            },
                        );
                        let accept = if ble_ctrl.session_origin().await
                            == Some(PairingOrigin::Button)
                        {
                            info!(
                                passkey = pc.passkey,
                                "BLE Numeric Comparison passkey — auto-accepting button pairing"
                            );
                            confirm_button_pairing_passkey(
                                &ble_transport,
                                &ble_ctrl,
                                &mut button_display_state,
                                pc.passkey,
                            )
                            .await
                        } else {
                            info!(
                                passkey = pc.passkey,
                                "BLE Numeric Comparison passkey — awaiting operator confirmation"
                            );
                            let (tx, rx) = tokio::sync::oneshot::channel();
                            ble_ctrl.set_passkey_responder(tx).await;

                            match tokio::time::timeout(std::time::Duration::from_secs(30), rx).await
                            {
                                Ok(Ok(v)) => v,
                                _ => {
                                    warn!("passkey confirmation timed out — rejecting");
                                    false
                                }
                            }
                        };

                        if ble_ctrl.session_origin().await == Some(PairingOrigin::Button) {
                            continue;
                        }

                        if let Err(e) = ble_transport.send_ble_pairing_confirm_reply(accept).await {
                            error!("BLE_PAIRING_CONFIRM_REPLY send error: {e}");
                        }
                    }
                    Some(BleEvent::Button(button)) => match button.button_type {
                        BUTTON_TYPE_LONG => {
                            if !open_button_pairing_session(
                                &ble_transport,
                                &ble_ctrl,
                                &button_display_generation,
                                &status_page_cycle,
                                &status_page_scroll_task,
                                &mut button_display_state,
                                &mut window,
                            )
                            .await
                            {
                                debug!("ignoring BUTTON_LONG while BLE pairing session is active");
                                continue;
                            }
                            button_timeout.as_mut().reset(
                                tokio::time::Instant::now()
                                    + Duration::from_secs(BUTTON_PAIRING_DURATION_S as u64),
                            );
                            button_timeout_armed = true;
                        }
                        BUTTON_TYPE_SHORT => match ble_ctrl.session_origin().await {
                            Some(PairingOrigin::Button) => {
                                info!("button-initiated BLE pairing cancelled");
                                handle_button_short_event(
                                    &ble_transport,
                                    &ble_ctrl,
                                    &button_display_generation,
                                    &status_page_cycle,
                                    &mut button_display_state,
                                    &mut window,
                                )
                                .await;
                                button_timeout_armed = false;
                            }
                            Some(PairingOrigin::Admin) => {
                                debug!("ignoring BUTTON_SHORT during admin-initiated BLE pairing");
                            }
                            None => {
                                let _ = handle_idle_button_short_event(
                                    &ble_transport,
                                    &ble_ctrl,
                                    &ble_storage,
                                    ble_channel,
                                    &button_display_generation,
                                    &status_page_cycle,
                                    &status_page_scroll_task,
                                )
                                .await;
                            }
                        },
                        other => {
                            debug!(button_type = other, "ignoring unknown button event");
                        }
                    },
                    None => {
                        debug!("BLE event channel closed");
                        break;
                    }
                }
            }
        });

        // 8b. Modem health monitor (GW-1102).
        let health_cancel = tokio_util::sync::CancellationToken::new();
        let mut health_handle = sonde_gateway::modem::spawn_health_monitor(
            Arc::downgrade(&transport),
            Duration::from_secs(30),
            health_cancel.clone(),
            sonde_gateway::modem::DEFAULT_MAX_HEALTH_POLL_FAILURES,
        );

        // 9. Wait for shutdown signal, or for any subsystem to exit unexpectedly.
        tokio::select! {
            _ = &mut shutdown => {
                info!("shutdown signal received, stopping gateway");
                // Abort all subsystem tasks so the tokio runtime does not
                // block on orphaned futures during teardown (GW-1400).
                ble_controller.cancel_and_wait().await;
                health_cancel.cancel();
                frame_loop.abort();
                ble_loop.abort();
                grpc_handle.abort();
                if !frame_loop.is_finished() {
                    let _ = frame_loop.await;
                }
                if !ble_loop.is_finished() {
                    let _ = ble_loop.await;
                }
                if !grpc_handle.is_finished() {
                    let _ = grpc_handle.await;
                }
                if !health_handle.is_finished() {
                    let _ = health_handle.await;
                }
                transport.abort_reader_and_wait().await;
                break; // exit the outer reconnect loop
            }
            _ = &mut frame_loop => {
                error!("frame processing loop exited — modem likely disconnected");
            }
            _ = &mut ble_loop => {
                error!("BLE event loop exited — modem likely disconnected");
            }
            _ = &mut grpc_handle => {
                error!("gRPC server exited unexpectedly");
                // Abort all subsystem tasks so the tokio runtime does not
                // block on orphaned futures during teardown (GW-1400).
                ble_controller.cancel_and_wait().await;
                health_cancel.cancel();
                frame_loop.abort();
                ble_loop.abort();
                if !frame_loop.is_finished() {
                    let _ = frame_loop.await;
                }
                if !ble_loop.is_finished() {
                    let _ = ble_loop.await;
                }
                if !health_handle.is_finished() {
                    let _ = health_handle.await;
                }
                transport.abort_reader_and_wait().await;
                break; // gRPC failure is not recoverable
            }
            result = &mut health_handle => {
                let reconnect = result.unwrap_or(false);
                if reconnect {
                    error!("health monitor: sustained poll failures — triggering modem reconnect");
                } else {
                    error!("health monitor exited — modem likely disconnected");
                }
            }
            // Wake up when the reader task signals a modem warm reboot.
            // The warm_reboot_flag acts as a latch so the event is not lost
            // if this arm does not win the select! poll (GW-1103 AC7).
            _ = warm_reboot_notify.notified() => {}
        }

        // GW-1103 AC7-9: warm reboot recovery — re-run full startup immediately.
        if warm_reboot_flag.load(std::sync::atomic::Ordering::Acquire) {
            info!("modem warm reboot detected — reconnecting immediately");
            // Cancel the BLE pairing session before dropping the transport so
            // the event-forwarding task releases its Arc<UsbEspNowTransport>.
            ble_controller.cancel_and_wait().await;
            health_cancel.cancel();
            frame_loop.abort();
            ble_loop.abort();
            grpc_handle.abort();
            // Guard each await with is_finished(): if a handle's output was already
            // consumed by the select! arm above, awaiting it again would hang
            // (poll returns Pending indefinitely once the output is taken).
            if !frame_loop.is_finished() {
                let _ = frame_loop.await;
            }
            if !ble_loop.is_finished() {
                let _ = ble_loop.await;
            }
            if !grpc_handle.is_finished() {
                let _ = grpc_handle.await;
            }
            if !health_handle.is_finished() {
                let _ = health_handle.await;
            }
            // Explicitly await the reader task so the serial port read half is
            // released before the next open() call (GW-1103 AC8).
            transport.abort_reader_and_wait().await;
            drop(transport);
            // Skip the backoff sleep — reconnect immediately (GW-1103 AC8).
            continue;
        }

        // Normal disconnect: abort all remaining tasks before reconnecting so
        // the old gRPC server releases its UDS/named-pipe socket and its
        // Arc<UsbEspNowTransport> clone, preventing bind failures and transport
        // leaks on the next reconnect iteration (GW-1103, GW-1301).
        ble_controller.cancel_and_wait().await;
        health_cancel.cancel();
        frame_loop.abort();
        ble_loop.abort();
        grpc_handle.abort();
        if !frame_loop.is_finished() {
            let _ = frame_loop.await;
        }
        if !ble_loop.is_finished() {
            let _ = ble_loop.await;
        }
        if !grpc_handle.is_finished() {
            let _ = grpc_handle.await;
        }
        if !health_handle.is_finished() {
            let _ = health_handle.await;
        }

        // GW-1301: log modem disconnecting before reconnect attempt.
        info!("modem disconnecting");

        // Transport disconnected — retry after backoff (GW-1103, GW-1301).
        info!(
            backoff_s = backoff.as_secs(),
            "modem disconnected — reconnecting"
        );
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(MAX_BACKOFF);
    } // end of reconnect loop

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
            // GW-1306 AC4: runtime log-level reload without restart.
            ServiceControl::ParamChange => {
                if let Some(handle) = LOG_RELOAD_HANDLE.get() {
                    let new_filter = tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| SERVICE_DEFAULT_FILTER.into());
                    match handle.reload(new_filter) {
                        Ok(()) => tracing::info!("log filter reloaded from RUST_LOG"),
                        Err(e) => tracing::error!(error = %e, "failed to reload log filter"),
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
        controls_accepted: ServiceControlAccept::STOP | ServiceControlAccept::PARAM_CHANGE,
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
        Ok(rt) => {
            let code = match rt.block_on(run_gateway(cli, shutdown_rx)) {
                Ok(()) => 0u32,
                Err(e) => {
                    error!("gateway exited with error: {e}");
                    1
                }
            };
            // GW-1400: start force-exit watchdog before runtime teardown.
            spawn_shutdown_watchdog();
            code
        }
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

    // GW-1306 AC4/AC5: reloadable log filter + graceful file failure.
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        Ok(file) => {
            let initial_filter =
                EnvFilter::try_from_default_env().unwrap_or_else(|_| SERVICE_DEFAULT_FILTER.into());
            let (filter, reload_handle) = tracing_subscriber::reload::Layer::new(initial_filter);
            let _ = LOG_RELOAD_HANDLE.set(reload_handle);

            let fmt_layer = tracing_subscriber::fmt::layer()
                .with_writer(std::sync::Mutex::new(file))
                .with_ansi(false)
                .with_filter(filter);

            let etw_layer = tracing_etw::LayerBuilder::new("sonde-gateway")
                .build()
                .map_err(|e| format!("failed to initialise ETW provider: {e}"))?;

            tracing_subscriber::registry()
                .with(fmt_layer)
                .with(etw_layer)
                .init();
        }
        Err(e) => {
            // ETW layer is created separately in each branch because the
            // subscriber type composition differs (with vs without the fmt
            // layer). Rust's type system requires distinct `.init()` calls.
            let etw_layer = tracing_etw::LayerBuilder::new("sonde-gateway")
                .build()
                .map_err(|e| format!("failed to initialise ETW provider: {e}"))?;

            tracing_subscriber::registry().with(etw_layer).init();

            tracing::error!(
                operation = "open log file",
                path = %log_path.display(),
                error = %e,
                guidance = "check directory permissions; the gateway will continue without file logging",
            );
        }
    }

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

/// Spawn a watchdog that force-exits the process after [`SHUTDOWN_TIMEOUT`] if
/// graceful shutdown stalls (GW-1400).  The watchdog runs on a separate OS
/// thread so that it survives tokio runtime teardown.
fn spawn_shutdown_watchdog() {
    std::thread::spawn(move || {
        std::thread::sleep(SHUTDOWN_TIMEOUT);
        warn!(
            timeout_secs = SHUTDOWN_TIMEOUT.as_secs(),
            "graceful shutdown did not complete in time — forcing exit (GW-1400)"
        );
        std::process::exit(0);
    });
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;
    use tokio::io::{duplex, AsyncReadExt, AsyncWriteExt, DuplexStream};

    use sonde_gateway::ble_pairing::{BlePairingController, PairingOrigin, RegistrationWindow};
    use sonde_gateway::display_banner::{render_display_message, render_gateway_version_banner};
    use sonde_gateway::registry::NodeRecord;
    use sonde_gateway::storage::{InMemoryStorage, Storage};
    use sonde_protocol::modem::{
        encode_modem_frame, DisplayFrameAck, FrameDecoder, ModemMessage, ModemReady,
        DISPLAY_FRAME_BODY_SIZE, DISPLAY_FRAME_CHUNK_COUNT, DISPLAY_FRAME_CHUNK_SIZE,
    };

    async fn read_next_message(
        stream: &mut DuplexStream,
        decoder: &mut FrameDecoder,
        buf: &mut [u8],
    ) -> ModemMessage {
        loop {
            match decoder.decode() {
                Ok(Some(msg)) => return msg,
                Ok(None) => {}
                Err(e) => panic!("decode error: {e}"),
            }
            let n = stream.read(buf).await.expect("read failed");
            assert!(n > 0, "stream closed unexpectedly");
            decoder.push(&buf[..n]);
        }
    }

    async fn do_startup_handshake(server: &mut DuplexStream, expected_channel: u8) {
        let mut decoder = FrameDecoder::new();
        let mut buf = [0u8; 256];

        let msg = read_next_message(server, &mut decoder, &mut buf).await;
        assert!(matches!(msg, ModemMessage::Reset));

        server
            .write_all(
                &encode_modem_frame(&ModemMessage::ModemReady(ModemReady {
                    firmware_version: [1, 0, 0, 0],
                    mac_address: [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF],
                }))
                .unwrap(),
            )
            .await
            .unwrap();

        let msg = read_next_message(server, &mut decoder, &mut buf).await;
        let requested_channel = match msg {
            ModemMessage::SetChannel(ch) => ch,
            other => panic!("expected SetChannel, got {other:?}"),
        };
        assert_eq!(requested_channel, expected_channel);

        server
            .write_all(
                &encode_modem_frame(&ModemMessage::SetChannelAck(requested_channel)).unwrap(),
            )
            .await
            .unwrap();
    }

    async fn create_transport_and_server(channel: u8) -> (Arc<UsbEspNowTransport>, DuplexStream) {
        let (client, mut server) = duplex(4096);
        let transport_handle =
            tokio::spawn(async move { UsbEspNowTransport::new(client, channel).await.unwrap() });
        do_startup_handshake(&mut server, channel).await;
        let transport = Arc::new(transport_handle.await.unwrap());
        (transport, server)
    }

    async fn assert_no_stream_data_while_time_paused(
        server: &mut DuplexStream,
        buf: &mut [u8],
        duration: Duration,
        message: &str,
    ) {
        let no_data = tokio::time::timeout(duration, server.read(buf));
        tokio::pin!(no_data);
        tokio::time::advance(duration).await;
        assert!(no_data.await.is_err(), "{message}");
    }

    async fn receive_display_transfer(
        server: &mut DuplexStream,
        decoder: &mut FrameDecoder,
        buf: &mut [u8],
    ) -> [u8; DISPLAY_FRAME_BODY_SIZE] {
        let mut framebuffer = [0u8; DISPLAY_FRAME_BODY_SIZE];

        let begin = read_next_message(server, decoder, buf).await;
        let transfer_id = match begin {
            ModemMessage::DisplayFrameBegin(begin) => begin.transfer_id,
            other => panic!("expected DisplayFrameBegin, got {other:?}"),
        };
        server
            .write_all(
                &encode_modem_frame(&ModemMessage::DisplayFrameAck(DisplayFrameAck {
                    transfer_id,
                    next_chunk_index: 0,
                }))
                .unwrap(),
            )
            .await
            .unwrap();

        for expected_chunk_index in 0..DISPLAY_FRAME_CHUNK_COUNT {
            let msg = read_next_message(server, decoder, buf).await;
            match msg {
                ModemMessage::DisplayFrameChunk(chunk) => {
                    assert_eq!(chunk.transfer_id, transfer_id);
                    assert_eq!(chunk.chunk_index, expected_chunk_index);
                    let start = usize::from(expected_chunk_index) * DISPLAY_FRAME_CHUNK_SIZE;
                    let end = start + DISPLAY_FRAME_CHUNK_SIZE;
                    framebuffer[start..end].copy_from_slice(&chunk.chunk_data);
                }
                other => panic!("expected DisplayFrameChunk, got {other:?}"),
            }
            server
                .write_all(
                    &encode_modem_frame(&ModemMessage::DisplayFrameAck(DisplayFrameAck {
                        transfer_id,
                        next_chunk_index: expected_chunk_index + 1,
                    }))
                    .unwrap(),
                )
                .await
                .unwrap();
        }

        framebuffer
    }

    fn make_rich_node(node_id: &str, key_hint: u16, fill: u8, last_seen_s: u64) -> NodeRecord {
        let mut node = NodeRecord::new(node_id.to_string(), key_hint, [fill; 32]);
        node.assigned_program_hash = Some(vec![fill; 32]);
        node.current_program_hash = Some(vec![fill.saturating_add(1); 32]);
        node.last_battery_mv = Some(3200 + u32::from(fill));
        node.last_seen = Some(UNIX_EPOCH + Duration::from_secs(last_seen_s));
        node.schedule_interval_s = 60 + u32::from(fill);
        node
    }

    #[test]
    fn node_status_lines_sort_nodes_and_omit_absent_fields() {
        let node_a = NodeRecord::new("a".to_string(), 1, [0x11; 32]);
        let node_b = make_rich_node("b", 2, 0x22, 1_700_000_000);

        let lines = build_node_status_lines(&[node_b.clone(), node_a.clone()]);
        let a_index = lines
            .iter()
            .position(|line| line == "- a")
            .expect("node a header missing");
        let b_index = lines
            .iter()
            .position(|line| line == "- b")
            .expect("node b header missing");
        assert!(a_index < b_index, "nodes must be sorted by node_id");
        assert!(
            lines
                .windows(2)
                .any(|window| window[0] == "node id" && window[1] == "- a"),
            "node id should render as a property/value pair"
        );
        assert!(
            lines.iter().all(|line| line != "key hint"),
            "key hint should not be shown on the display page"
        );
        assert!(
            lines.iter().all(|line| line != "- 1" && line != "- 2"),
            "key hint values should not be shown on the display page"
        );
        assert_eq!(
            lines
                .iter()
                .filter(|line| *line == "assigned program")
                .count(),
            1,
            "assigned program hash should be omitted when absent"
        );
        assert_eq!(
            lines
                .iter()
                .filter(|line| *line == "current program")
                .count(),
            1,
            "current program hash should be omitted when absent"
        );
        assert_eq!(
            lines.iter().filter(|line| *line == "battery").count(),
            1,
            "battery should be omitted when absent"
        );
        assert_eq!(
            lines.iter().filter(|line| *line == "last seen").count(),
            1,
            "last seen should be omitted when absent"
        );
        assert_eq!(
            lines.iter().filter(|line| *line == "schedule").count(),
            2,
            "schedule should be shown for each node"
        );
        assert!(
            lines
                .iter()
                .all(|line| line.chars().count() <= STATUS_TEXT_COLUMNS),
            "all rendered lines must fit within the status text width"
        );
    }

    #[test]
    fn empty_node_status_lines_show_empty_registry_message() {
        assert_eq!(
            build_node_status_lines(&[] as &[NodeRecord]),
            vec!["No nodes registered.".to_string()]
        );
    }

    async fn open_button_pairing_for_test(
        transport: Arc<UsbEspNowTransport>,
        controller: Arc<BlePairingController>,
        display_generation: Arc<AtomicU64>,
        status_page_cycle: Arc<tokio::sync::Mutex<StatusPageCycle>>,
        server: &mut DuplexStream,
        decoder: &mut FrameDecoder,
        buf: &mut [u8],
    ) -> RegistrationWindow {
        let status_page_scroll_task: StatusPageScrollTask = Arc::new(tokio::sync::Mutex::new(None));
        let task = tokio::spawn({
            let transport = Arc::clone(&transport);
            let controller = Arc::clone(&controller);
            let display_generation = Arc::clone(&display_generation);
            let status_page_cycle = Arc::clone(&status_page_cycle);
            let status_page_scroll_task = Arc::clone(&status_page_scroll_task);
            async move {
                let mut window = RegistrationWindow::new();
                let mut display_state = ButtonDisplayState::Generic;
                let opened = open_button_pairing_session(
                    &transport,
                    &controller,
                    &display_generation,
                    &status_page_cycle,
                    &status_page_scroll_task,
                    &mut display_state,
                    &mut window,
                )
                .await;
                (opened, window)
            }
        });

        let msg = read_next_message(server, decoder, buf).await;
        assert!(matches!(msg, ModemMessage::BleEnable));
        let framebuffer = receive_display_transfer(server, decoder, buf).await;
        assert_eq!(framebuffer, render_display_message(&["Pairing"]));

        let (opened, window) = task.await.unwrap();
        assert!(opened);
        assert_eq!(
            controller.session_origin().await,
            Some(PairingOrigin::Button)
        );
        window
    }

    #[tokio::test]
    async fn button_long_opens_pairing_and_updates_display() {
        let (transport, mut server) = create_transport_and_server(6).await;
        let controller = Arc::new(BlePairingController::new());
        let display_generation = Arc::new(AtomicU64::new(0));
        let status_page_cycle = Arc::new(tokio::sync::Mutex::new(StatusPageCycle::default()));
        let mut decoder = FrameDecoder::new();
        let mut buf = [0u8; 2048];

        let _window = open_button_pairing_for_test(
            Arc::clone(&transport),
            Arc::clone(&controller),
            Arc::clone(&display_generation),
            Arc::clone(&status_page_cycle),
            &mut server,
            &mut decoder,
            &mut buf,
        )
        .await;
    }

    #[tokio::test]
    async fn second_button_long_is_ignored_while_active() {
        let (transport, mut server) = create_transport_and_server(6).await;
        let controller = Arc::new(BlePairingController::new());
        let display_generation = Arc::new(AtomicU64::new(0));
        let status_page_cycle = Arc::new(tokio::sync::Mutex::new(StatusPageCycle::default()));
        let status_page_scroll_task: StatusPageScrollTask = Arc::new(tokio::sync::Mutex::new(None));
        let mut decoder = FrameDecoder::new();
        let mut buf = [0u8; 2048];

        let mut window = open_button_pairing_for_test(
            Arc::clone(&transport),
            Arc::clone(&controller),
            Arc::clone(&display_generation),
            Arc::clone(&status_page_cycle),
            &mut server,
            &mut decoder,
            &mut buf,
        )
        .await;

        let mut display_state = ButtonDisplayState::Generic;
        assert!(
            !open_button_pairing_session(
                &transport,
                &controller,
                &display_generation,
                &status_page_cycle,
                &status_page_scroll_task,
                &mut display_state,
                &mut window,
            )
            .await
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(200), server.read(&mut buf))
                .await
                .is_err(),
            "ignored long press must not emit modem traffic"
        );
    }

    #[tokio::test]
    async fn button_short_cancels_button_pairing_but_not_admin_pairing() {
        let (transport, mut server) = create_transport_and_server(6).await;
        let controller = Arc::new(BlePairingController::new());
        let display_generation = Arc::new(AtomicU64::new(0));
        let status_page_cycle = Arc::new(tokio::sync::Mutex::new(StatusPageCycle::default()));
        let mut decoder = FrameDecoder::new();
        let mut buf = [0u8; 2048];

        let window = open_button_pairing_for_test(
            Arc::clone(&transport),
            Arc::clone(&controller),
            Arc::clone(&display_generation),
            Arc::clone(&status_page_cycle),
            &mut server,
            &mut decoder,
            &mut buf,
        )
        .await;

        tokio::time::pause();
        let task = tokio::spawn({
            let transport = Arc::clone(&transport);
            let controller = Arc::clone(&controller);
            let display_generation = Arc::clone(&display_generation);
            let status_page_cycle = Arc::clone(&status_page_cycle);
            async move {
                let mut window = window;
                let mut display_state = ButtonDisplayState::Generic;
                let cancelled = handle_button_short_event(
                    &transport,
                    &controller,
                    &display_generation,
                    &status_page_cycle,
                    &mut display_state,
                    &mut window,
                )
                .await;
                (cancelled, window)
            }
        });

        let msg = read_next_message(&mut server, &mut decoder, &mut buf).await;
        assert!(matches!(msg, ModemMessage::BleDisable));
        let framebuffer = receive_display_transfer(&mut server, &mut decoder, &mut buf).await;
        assert_eq!(framebuffer, render_display_message(&["Cancelled"]));
        let (cancelled, mut window) = task.await.unwrap();
        assert!(cancelled);
        assert_eq!(controller.session_origin().await, None);
        tokio::time::advance(BUTTON_EXIT_REASON_DISPLAY_DURATION + Duration::from_millis(100))
            .await;
        let framebuffer = receive_display_transfer(&mut server, &mut decoder, &mut buf).await;
        assert_eq!(
            framebuffer,
            render_gateway_version_banner(env!("CARGO_PKG_VERSION"))
        );

        assert!(controller.open_window(120, PairingOrigin::Admin).await);
        let mut display_state = ButtonDisplayState::Generic;
        assert!(
            !handle_button_short_event(
                &transport,
                &controller,
                &display_generation,
                &status_page_cycle,
                &mut display_state,
                &mut window,
            )
            .await
        );
        assert_eq!(
            controller.session_origin().await,
            Some(PairingOrigin::Admin)
        );
    }

    #[tokio::test]
    async fn button_timeout_closes_pairing_and_shows_timed_out() {
        let (transport, mut server) = create_transport_and_server(6).await;
        let controller = Arc::new(BlePairingController::new());
        let display_generation = Arc::new(AtomicU64::new(0));
        let status_page_cycle = Arc::new(tokio::sync::Mutex::new(StatusPageCycle::default()));
        let mut decoder = FrameDecoder::new();
        let mut buf = [0u8; 2048];

        let window = open_button_pairing_for_test(
            Arc::clone(&transport),
            Arc::clone(&controller),
            Arc::clone(&display_generation),
            Arc::clone(&status_page_cycle),
            &mut server,
            &mut decoder,
            &mut buf,
        )
        .await;

        tokio::time::pause();
        let task = tokio::spawn({
            let transport = Arc::clone(&transport);
            let controller = Arc::clone(&controller);
            let display_generation = Arc::clone(&display_generation);
            let status_page_cycle = Arc::clone(&status_page_cycle);
            async move {
                let mut window = window;
                let mut display_state = ButtonDisplayState::Generic;
                close_button_pairing_session(
                    &transport,
                    &controller,
                    &display_generation,
                    &status_page_cycle,
                    &mut display_state,
                    &mut window,
                    &["Timed out"],
                )
                .await;
                window
            }
        });

        let msg = read_next_message(&mut server, &mut decoder, &mut buf).await;
        assert!(matches!(msg, ModemMessage::BleDisable));
        let framebuffer = receive_display_transfer(&mut server, &mut decoder, &mut buf).await;
        assert_eq!(framebuffer, render_display_message(&["Timed out"]));
        let _window = task.await.unwrap();
        tokio::time::advance(BUTTON_EXIT_REASON_DISPLAY_DURATION + Duration::from_millis(100))
            .await;
        let framebuffer = receive_display_transfer(&mut server, &mut decoder, &mut buf).await;
        assert_eq!(
            framebuffer,
            render_gateway_version_banner(env!("CARGO_PKG_VERSION"))
        );
    }

    #[tokio::test]
    async fn button_timeout_cleanup_still_runs_after_controller_deadline_expires() {
        let (transport, mut server) = create_transport_and_server(6).await;
        let controller = Arc::new(BlePairingController::new());
        let display_generation = Arc::new(AtomicU64::new(0));
        let status_page_cycle = Arc::new(tokio::sync::Mutex::new(StatusPageCycle::default()));
        let mut decoder = FrameDecoder::new();
        let mut buf = [0u8; 2048];

        assert!(controller.open_window(0, PairingOrigin::Button).await);
        let task = tokio::spawn({
            let transport = Arc::clone(&transport);
            let controller = Arc::clone(&controller);
            let display_generation = Arc::clone(&display_generation);
            let status_page_cycle = Arc::clone(&status_page_cycle);
            async move {
                let mut window = RegistrationWindow::new();
                let mut display_state = ButtonDisplayState::Generic;
                window.open(BUTTON_PAIRING_DURATION_S);
                handle_button_pairing_timeout(
                    &transport,
                    &controller,
                    &display_generation,
                    &status_page_cycle,
                    &mut display_state,
                    &mut window,
                )
                .await;
                window
            }
        });

        let msg = read_next_message(&mut server, &mut decoder, &mut buf).await;
        assert!(matches!(msg, ModemMessage::BleDisable));
        let framebuffer = receive_display_transfer(&mut server, &mut decoder, &mut buf).await;
        assert_eq!(framebuffer, render_display_message(&["Timed out"]));
        let _window = task.await.unwrap();
        assert_eq!(controller.session_origin_raw().await, None);
    }

    #[tokio::test]
    async fn button_pairing_connected_and_passkey_are_rendered_and_auto_confirmed() {
        let (transport, mut server) = create_transport_and_server(6).await;
        let controller = Arc::new(BlePairingController::new());
        let display_generation = Arc::new(AtomicU64::new(0));
        let status_page_cycle = Arc::new(tokio::sync::Mutex::new(StatusPageCycle::default()));
        let mut decoder = FrameDecoder::new();
        let mut buf = [0u8; 2048];

        let _window = open_button_pairing_for_test(
            Arc::clone(&transport),
            Arc::clone(&controller),
            Arc::clone(&display_generation),
            Arc::clone(&status_page_cycle),
            &mut server,
            &mut decoder,
            &mut buf,
        )
        .await;

        let connected_task = tokio::spawn({
            let transport = Arc::clone(&transport);
            let controller = Arc::clone(&controller);
            async move {
                show_button_pairing_connected(&transport, &controller, ButtonDisplayState::Generic)
                    .await;
            }
        });
        let framebuffer = receive_display_transfer(&mut server, &mut decoder, &mut buf).await;
        assert_eq!(framebuffer, render_display_message(&["Phone connected"]));
        connected_task.await.unwrap();

        let confirm_task = tokio::spawn({
            let transport = Arc::clone(&transport);
            let controller = Arc::clone(&controller);
            async move {
                let mut display_state = ButtonDisplayState::Generic;
                confirm_button_pairing_passkey(&transport, &controller, &mut display_state, 123456)
                    .await
            }
        });
        let framebuffer = receive_display_transfer(&mut server, &mut decoder, &mut buf).await;
        assert_eq!(framebuffer, render_display_message(&["Pin", "123456"]));
        let msg = read_next_message(&mut server, &mut decoder, &mut buf).await;
        match msg {
            ModemMessage::BlePairingConfirmReply(reply) => assert!(reply.accept),
            other => panic!("expected BLE pairing confirm reply, got {other:?}"),
        }
        let suppressed_connected = tokio::spawn({
            let transport = Arc::clone(&transport);
            let controller = Arc::clone(&controller);
            async move {
                show_button_pairing_connected(&transport, &controller, ButtonDisplayState::Passkey)
                    .await;
            }
        });
        suppressed_connected.await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(200), server.read(&mut buf))
                .await
                .is_err(),
            "connected updates must not overwrite the passkey screen"
        );

        assert!(confirm_task.await.unwrap());
    }

    #[tokio::test]
    async fn successful_button_pairing_shows_provisioned_then_done_and_disables_ble() {
        let (transport, mut server) = create_transport_and_server(6).await;
        let controller = Arc::new(BlePairingController::new());
        let display_generation = Arc::new(AtomicU64::new(0));
        let status_page_cycle = Arc::new(tokio::sync::Mutex::new(StatusPageCycle::default()));
        let mut decoder = FrameDecoder::new();
        let mut buf = [0u8; 2048];

        let window = open_button_pairing_for_test(
            Arc::clone(&transport),
            Arc::clone(&controller),
            Arc::clone(&display_generation),
            Arc::clone(&status_page_cycle),
            &mut server,
            &mut decoder,
            &mut buf,
        )
        .await;

        controller.mark_phone_registered().await;
        tokio::time::pause();
        let task = tokio::spawn({
            let transport = Arc::clone(&transport);
            let controller = Arc::clone(&controller);
            let display_generation = Arc::clone(&display_generation);
            let status_page_cycle = Arc::clone(&status_page_cycle);
            async move {
                let mut window = window;
                let mut display_state = ButtonDisplayState::Passkey;
                complete_button_pairing_success(
                    &transport,
                    &controller,
                    &display_generation,
                    &status_page_cycle,
                    &mut display_state,
                    &mut window,
                )
                .await;
                window
            }
        });

        let framebuffer = receive_display_transfer(&mut server, &mut decoder, &mut buf).await;
        assert_eq!(framebuffer, render_display_message(&["Provisioned"]));
        let msg = read_next_message(&mut server, &mut decoder, &mut buf).await;
        assert!(matches!(msg, ModemMessage::BleDisable));
        let framebuffer = receive_display_transfer(&mut server, &mut decoder, &mut buf).await;
        assert_eq!(framebuffer, render_display_message(&["Done"]));
        let _window = task.await.unwrap();
        assert_eq!(controller.session_origin().await, None);
        tokio::time::advance(BUTTON_EXIT_REASON_DISPLAY_DURATION + Duration::from_millis(100))
            .await;
        let framebuffer = receive_display_transfer(&mut server, &mut decoder, &mut buf).await;
        assert_eq!(
            framebuffer,
            render_gateway_version_banner(env!("CARGO_PKG_VERSION"))
        );
    }

    #[tokio::test]
    async fn idle_button_short_cycles_status_pages_and_restores_banner() {
        let (transport, mut server) = create_transport_and_server(6).await;
        let controller = Arc::new(BlePairingController::new());
        let display_generation = Arc::new(AtomicU64::new(0));
        let status_page_cycle = Arc::new(tokio::sync::Mutex::new(StatusPageCycle::default()));
        let status_page_scroll_task: StatusPageScrollTask = Arc::new(tokio::sync::Mutex::new(None));
        let storage = Arc::new(InMemoryStorage::new());
        storage.set_config("espnow_channel", "11").await.unwrap();
        let storage: Arc<dyn Storage> = storage;
        let mut decoder = FrameDecoder::new();
        let mut buf = [0u8; 2048];

        tokio::time::pause();
        let first_press = tokio::spawn({
            let transport = Arc::clone(&transport);
            let controller = Arc::clone(&controller);
            let storage = Arc::clone(&storage);
            let display_generation = Arc::clone(&display_generation);
            let status_page_cycle = Arc::clone(&status_page_cycle);
            let status_page_scroll_task = Arc::clone(&status_page_scroll_task);
            async move {
                handle_idle_button_short_event(
                    &transport,
                    &controller,
                    &storage,
                    6,
                    &display_generation,
                    &status_page_cycle,
                    &status_page_scroll_task,
                )
                .await
            }
        });
        let framebuffer = receive_display_transfer(&mut server, &mut decoder, &mut buf).await;
        assert_eq!(framebuffer, render_display_message(&["Channel", "11"]));
        assert!(first_press.await.unwrap());
        assert_no_stream_data_while_time_paused(
            &mut server,
            &mut buf,
            Duration::from_millis(50),
            "short press must not emit BLE control messages",
        )
        .await;

        tokio::time::advance(Duration::from_secs(30)).await;
        let second_press = tokio::spawn({
            let transport = Arc::clone(&transport);
            let controller = Arc::clone(&controller);
            let storage = Arc::clone(&storage);
            let display_generation = Arc::clone(&display_generation);
            let status_page_cycle = Arc::clone(&status_page_cycle);
            let status_page_scroll_task = Arc::clone(&status_page_scroll_task);
            async move {
                handle_idle_button_short_event(
                    &transport,
                    &controller,
                    &storage,
                    6,
                    &display_generation,
                    &status_page_cycle,
                    &status_page_scroll_task,
                )
                .await
            }
        });
        let framebuffer = receive_display_transfer(&mut server, &mut decoder, &mut buf).await;
        let expected_nodes_page =
            render_status_text_page(&build_node_status_lines(&[] as &[NodeRecord]));
        assert_eq!(framebuffer, expected_nodes_page.visible_window(0));
        assert!(second_press.await.unwrap());
        assert_no_stream_data_while_time_paused(
            &mut server,
            &mut buf,
            Duration::from_millis(50),
            "short press must not emit BLE control messages",
        )
        .await;

        tokio::time::advance(STATUS_PAGE_TIMEOUT + Duration::from_millis(100)).await;
        let framebuffer = receive_display_transfer(&mut server, &mut decoder, &mut buf).await;
        assert_eq!(
            framebuffer,
            render_gateway_version_banner(env!("CARGO_PKG_VERSION"))
        );
        assert_eq!(status_page_cycle.lock().await.next_page_index, 0);
    }

    #[tokio::test]
    async fn idle_button_short_scrolls_nodes_page_and_wraps_to_top() {
        let (transport, mut server) = create_transport_and_server(6).await;
        let controller = Arc::new(BlePairingController::new());
        let display_generation = Arc::new(AtomicU64::new(0));
        let status_page_cycle = Arc::new(tokio::sync::Mutex::new(StatusPageCycle::default()));
        let status_page_scroll_task: StatusPageScrollTask = Arc::new(tokio::sync::Mutex::new(None));
        let storage = Arc::new(InMemoryStorage::new());
        storage.set_config("espnow_channel", "11").await.unwrap();
        let node_a = make_rich_node("node-a", 0x1001, 0x41, 1_700_000_000);
        let node_b = make_rich_node("node-b", 0x1002, 0x52, 1_700_000_060);
        storage.upsert_node(&node_a).await.unwrap();
        storage.upsert_node(&node_b).await.unwrap();
        let storage: Arc<dyn Storage> = storage;
        let mut decoder = FrameDecoder::new();
        let mut buf = [0u8; 2048];

        let expected_page =
            render_status_text_page(&build_node_status_lines(&[node_a.clone(), node_b.clone()]));
        assert!(expected_page.is_scrollable(), "rich node page must scroll");

        tokio::time::pause();
        for expected_frame in [
            render_display_message(&["Channel", "11"]),
            expected_page.visible_window(0),
        ] {
            let press = tokio::spawn({
                let transport = Arc::clone(&transport);
                let controller = Arc::clone(&controller);
                let storage = Arc::clone(&storage);
                let display_generation = Arc::clone(&display_generation);
                let status_page_cycle = Arc::clone(&status_page_cycle);
                let status_page_scroll_task = Arc::clone(&status_page_scroll_task);
                async move {
                    handle_idle_button_short_event(
                        &transport,
                        &controller,
                        &storage,
                        6,
                        &display_generation,
                        &status_page_cycle,
                        &status_page_scroll_task,
                    )
                    .await
                }
            });
            let framebuffer = receive_display_transfer(&mut server, &mut decoder, &mut buf).await;
            assert_eq!(framebuffer, expected_frame);
            assert!(press.await.unwrap());
        }

        let mut expected_offset = 0;
        while expected_offset < expected_page.scroll_end_offset() {
            tokio::time::advance(NODE_STATUS_SCROLL_INTERVAL).await;
            expected_offset = (expected_offset + NODE_STATUS_SCROLL_STEP_PX)
                .min(expected_page.scroll_end_offset());
            let framebuffer = receive_display_transfer(&mut server, &mut decoder, &mut buf).await;
            assert_eq!(framebuffer, expected_page.visible_window(expected_offset));
        }

        tokio::time::advance(NODE_STATUS_SCROLL_INTERVAL).await;
        let framebuffer = receive_display_transfer(&mut server, &mut decoder, &mut buf).await;
        assert_eq!(framebuffer, expected_page.visible_window(0));
    }

    #[tokio::test]
    async fn reentering_nodes_page_restarts_scroll_from_top() {
        let (transport, mut server) = create_transport_and_server(6).await;
        let controller = Arc::new(BlePairingController::new());
        let display_generation = Arc::new(AtomicU64::new(0));
        let status_page_cycle = Arc::new(tokio::sync::Mutex::new(StatusPageCycle::default()));
        let status_page_scroll_task: StatusPageScrollTask = Arc::new(tokio::sync::Mutex::new(None));
        let storage = Arc::new(InMemoryStorage::new());
        storage.set_config("espnow_channel", "11").await.unwrap();
        let node_a = make_rich_node("node-a", 0x1001, 0x41, 1_700_000_000);
        let node_b = make_rich_node("node-b", 0x1002, 0x52, 1_700_000_060);
        storage.upsert_node(&node_a).await.unwrap();
        storage.upsert_node(&node_b).await.unwrap();
        let storage: Arc<dyn Storage> = storage;
        let mut decoder = FrameDecoder::new();
        let mut buf = [0u8; 2048];

        let expected_nodes_page =
            render_status_text_page(&build_node_status_lines(&[node_a.clone(), node_b.clone()]));

        tokio::time::pause();
        for _ in 0..2 {
            let press = tokio::spawn({
                let transport = Arc::clone(&transport);
                let controller = Arc::clone(&controller);
                let storage = Arc::clone(&storage);
                let display_generation = Arc::clone(&display_generation);
                let status_page_cycle = Arc::clone(&status_page_cycle);
                let status_page_scroll_task = Arc::clone(&status_page_scroll_task);
                async move {
                    handle_idle_button_short_event(
                        &transport,
                        &controller,
                        &storage,
                        6,
                        &display_generation,
                        &status_page_cycle,
                        &status_page_scroll_task,
                    )
                    .await
                }
            });
            let _ = receive_display_transfer(&mut server, &mut decoder, &mut buf).await;
            assert!(press.await.unwrap());
        }

        tokio::time::advance(NODE_STATUS_SCROLL_INTERVAL).await;
        let framebuffer = receive_display_transfer(&mut server, &mut decoder, &mut buf).await;
        assert_eq!(
            framebuffer,
            expected_nodes_page.visible_window(NODE_STATUS_SCROLL_STEP_PX)
        );

        for expected_frame in [
            render_display_message(&["Channel", "11"]),
            expected_nodes_page.visible_window(0),
        ] {
            let press = tokio::spawn({
                let transport = Arc::clone(&transport);
                let controller = Arc::clone(&controller);
                let storage = Arc::clone(&storage);
                let display_generation = Arc::clone(&display_generation);
                let status_page_cycle = Arc::clone(&status_page_cycle);
                let status_page_scroll_task = Arc::clone(&status_page_scroll_task);
                async move {
                    handle_idle_button_short_event(
                        &transport,
                        &controller,
                        &storage,
                        6,
                        &display_generation,
                        &status_page_cycle,
                        &status_page_scroll_task,
                    )
                    .await
                }
            });
            let framebuffer = receive_display_transfer(&mut server, &mut decoder, &mut buf).await;
            assert_eq!(framebuffer, expected_frame);
            assert!(press.await.unwrap());
        }
    }

    #[tokio::test]
    async fn empty_nodes_page_is_static() {
        let (transport, mut server) = create_transport_and_server(6).await;
        let controller = Arc::new(BlePairingController::new());
        let display_generation = Arc::new(AtomicU64::new(0));
        let status_page_cycle = Arc::new(tokio::sync::Mutex::new(StatusPageCycle::default()));
        let status_page_scroll_task: StatusPageScrollTask = Arc::new(tokio::sync::Mutex::new(None));
        let storage = Arc::new(InMemoryStorage::new());
        storage.set_config("espnow_channel", "11").await.unwrap();
        let storage: Arc<dyn Storage> = storage;
        let mut decoder = FrameDecoder::new();
        let mut buf = [0u8; 2048];

        let expected_page = render_status_text_page(&build_node_status_lines(&[] as &[NodeRecord]));
        assert!(
            !expected_page.is_scrollable(),
            "empty state should be static"
        );

        tokio::time::pause();
        for expected_frame in [
            render_display_message(&["Channel", "11"]),
            expected_page.visible_window(0),
        ] {
            let press = tokio::spawn({
                let transport = Arc::clone(&transport);
                let controller = Arc::clone(&controller);
                let storage = Arc::clone(&storage);
                let display_generation = Arc::clone(&display_generation);
                let status_page_cycle = Arc::clone(&status_page_cycle);
                let status_page_scroll_task = Arc::clone(&status_page_scroll_task);
                async move {
                    handle_idle_button_short_event(
                        &transport,
                        &controller,
                        &storage,
                        6,
                        &display_generation,
                        &status_page_cycle,
                        &status_page_scroll_task,
                    )
                    .await
                }
            });
            let framebuffer = receive_display_transfer(&mut server, &mut decoder, &mut buf).await;
            assert_eq!(framebuffer, expected_frame);
            assert!(press.await.unwrap());
        }

        tokio::time::advance(Duration::from_millis(120)).await;
        assert_no_stream_data_while_time_paused(
            &mut server,
            &mut buf,
            Duration::from_millis(200),
            "static node page must not emit autonomous scroll updates",
        )
        .await;
    }

    #[tokio::test]
    async fn status_page_timeout_does_not_restore_banner_during_admin_pairing() {
        let (transport, mut server) = create_transport_and_server(6).await;
        let controller = Arc::new(BlePairingController::new());
        let display_generation = Arc::new(AtomicU64::new(0));
        let status_page_cycle = Arc::new(tokio::sync::Mutex::new(StatusPageCycle::default()));
        let status_page_scroll_task: StatusPageScrollTask = Arc::new(tokio::sync::Mutex::new(None));
        let storage = Arc::new(InMemoryStorage::new());
        storage.set_config("espnow_channel", "11").await.unwrap();
        let storage: Arc<dyn Storage> = storage;
        let mut decoder = FrameDecoder::new();
        let mut buf = [0u8; 2048];

        tokio::time::pause();
        let short_press = tokio::spawn({
            let transport = Arc::clone(&transport);
            let controller = Arc::clone(&controller);
            let storage = Arc::clone(&storage);
            let display_generation = Arc::clone(&display_generation);
            let status_page_cycle = Arc::clone(&status_page_cycle);
            let status_page_scroll_task = Arc::clone(&status_page_scroll_task);
            async move {
                handle_idle_button_short_event(
                    &transport,
                    &controller,
                    &storage,
                    6,
                    &display_generation,
                    &status_page_cycle,
                    &status_page_scroll_task,
                )
                .await
            }
        });
        let framebuffer = receive_display_transfer(&mut server, &mut decoder, &mut buf).await;
        assert_eq!(framebuffer, render_display_message(&["Channel", "11"]));
        assert!(short_press.await.unwrap());

        assert!(controller.open_window(120, PairingOrigin::Admin).await);
        tokio::time::advance(STATUS_PAGE_TIMEOUT + Duration::from_millis(100)).await;
        assert_no_stream_data_while_time_paused(
            &mut server,
            &mut buf,
            Duration::from_millis(200),
            "status-page timeout must not restore the banner during admin pairing",
        )
        .await;
        assert_eq!(status_page_cycle.lock().await.next_page_index, 1);
    }

    #[tokio::test]
    async fn status_page_timeout_cancels_active_scroll_before_restoring_banner() {
        let (transport, mut server) = create_transport_and_server(6).await;
        let controller = Arc::new(BlePairingController::new());
        let display_generation = Arc::new(AtomicU64::new(0));
        let status_page_cycle = Arc::new(tokio::sync::Mutex::new(StatusPageCycle::default()));
        let status_page_scroll_task: StatusPageScrollTask = Arc::new(tokio::sync::Mutex::new(None));
        let mut decoder = FrameDecoder::new();
        let mut buf = [0u8; 2048];

        tokio::time::pause();
        let stop_requested = Arc::new(AtomicBool::new(false));
        let stop_requested_for_thread = Arc::clone(&stop_requested);
        let (dummy_scroll_stopped_tx, dummy_scroll_stopped_rx) = tokio::sync::oneshot::channel();
        let dummy_scroll_watcher = std::thread::spawn(move || {
            while !stop_requested_for_thread.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
            let _ = dummy_scroll_stopped_tx.send(());
        });
        let dummy_scroll = tokio::spawn(async move {
            let _ = dummy_scroll_stopped_rx.await;
        });
        *status_page_scroll_task.lock().await = Some(ActiveStatusPageScroll {
            stop_requested,
            handle: dummy_scroll,
        });

        let generation = schedule_status_page_banner_restore(
            &transport,
            &controller,
            &display_generation,
            &status_page_cycle,
            &status_page_scroll_task,
        );
        assert_eq!(display_generation.load(Ordering::SeqCst), generation);

        tokio::time::advance(STATUS_PAGE_TIMEOUT + Duration::from_millis(100)).await;
        let framebuffer = receive_display_transfer(&mut server, &mut decoder, &mut buf).await;
        assert_eq!(
            framebuffer,
            render_gateway_version_banner(env!("CARGO_PKG_VERSION"))
        );
        assert!(
            status_page_scroll_task.lock().await.is_none(),
            "idle restore must clear the active scroll task"
        );
        assert_eq!(status_page_cycle.lock().await.next_page_index, 0);
        dummy_scroll_watcher.join().unwrap();
    }
}

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

    #[cfg(debug_assertions)]
    const DEFAULT_FILTER: &str = "sonde_gateway=info";
    #[cfg(not(debug_assertions))]
    const DEFAULT_FILTER: &str = "sonde_gateway=warn";

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| DEFAULT_FILTER.into()),
        )
        .init();

    // Drive ctrl-c into a oneshot so run_gateway has a uniform shutdown interface.
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            let _ = shutdown_tx.send(());
        }
    });

    let result = run_gateway(&cli, shutdown_rx).await;

    // GW-1400: start the force-exit watchdog after run_gateway returns.
    // If tokio runtime teardown (Drop impls, pending I/O) hangs, the
    // watchdog will force-exit the process after SHUTDOWN_TIMEOUT.
    spawn_shutdown_watchdog();

    result
}
