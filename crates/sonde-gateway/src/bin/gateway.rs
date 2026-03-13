// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use tokio::sync::RwLock;
use tracing::{error, info};

use sonde_gateway::admin::pb::gateway_admin_server::GatewayAdminServer;
use sonde_gateway::engine::{Gateway, PendingCommand};
use sonde_gateway::modem::UsbEspNowTransport;
use sonde_gateway::session::SessionManager;
use sonde_gateway::sqlite_storage::SqliteStorage;
use sonde_gateway::transport::Transport;
use sonde_gateway::AdminService;

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

    /// gRPC admin API listen address.
    #[arg(long, default_value = "127.0.0.1:50051")]
    admin_addr: String,

    /// Session timeout in seconds.
    #[arg(long, default_value_t = 30)]
    session_timeout: u64,

    /// Serial port baud rate.
    #[arg(long, default_value_t = 115_200)]
    baud_rate: u32,
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

    // 1. Open persistent storage
    let storage = Arc::new(SqliteStorage::open(&cli.db)?);
    info!("storage opened: {}", cli.db);

    // 2. Session manager
    let session_manager = Arc::new(SessionManager::new(Duration::from_secs(
        cli.session_timeout,
    )));

    // 3. Shared pending-command queue (admin → engine)
    let pending_commands: Arc<RwLock<HashMap<String, Vec<PendingCommand>>>> =
        Arc::new(RwLock::new(HashMap::new()));

    // 4. Gateway engine
    let gateway = Arc::new(Gateway::new_with_pending(
        storage.clone(),
        pending_commands.clone(),
        session_manager.clone(),
    ));

    // 5. Open serial port and create modem transport
    let serial_port =
        tokio_serial::SerialStream::open(&tokio_serial::new(&cli.port, cli.baud_rate))?;
    let transport = Arc::new(UsbEspNowTransport::new(serial_port, cli.channel).await?);
    info!(channel = cli.channel, "modem transport ready");

    // 6. Start gRPC admin server
    let admin_service = AdminService::new(storage.clone(), pending_commands, session_manager);
    let admin_addr: std::net::SocketAddr = cli.admin_addr.parse()?;

    let grpc_handle = tokio::spawn(async move {
        info!(%admin_addr, "gRPC admin server listening");
        if let Err(e) = tonic::transport::Server::builder()
            .add_service(GatewayAdminServer::new(admin_service))
            .serve(admin_addr)
            .await
        {
            error!("gRPC server error: {}", e);
        }
    });

    // 7. Main frame-processing loop
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

    // 8. Wait for shutdown
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("received ctrl-c, shutting down");
        }
        _ = frame_loop => {
            error!("frame processing loop exited");
        }
        _ = grpc_handle => {
            error!("gRPC server exited");
        }
    }

    info!("gateway stopped");
    Ok(())
}
