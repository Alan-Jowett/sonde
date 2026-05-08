// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::json;
use sonde_azure_handler::{
    extract_trigger_payload, AzureHandler, AzureTablesStore, HandlerError, RuntimeConfig,
    StorageQueuePublisher,
};
use tokio::net::TcpListener;
use tracing_subscriber::prelude::*;

#[derive(Clone)]
struct AppState {
    handler: Arc<AzureHandler<AzureTablesStore, StorageQueuePublisher>>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_ansi(false)
                .with_target(true)
                .with_level(true),
        )
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "sonde_azure_handler=info".into()),
        )
        .init();

    let config = RuntimeConfig::from_env()?;
    let store = Arc::new(AzureTablesStore::new(&config)?);
    let publisher = Arc::new(StorageQueuePublisher::new(
        config.storage_queue_endpoint.clone(),
    )?);
    let state = AppState {
        handler: Arc::new(AzureHandler::new(
            store,
            publisher,
            config.downstream_queue.clone(),
        )),
    };

    let port = std::env::var("FUNCTIONS_CUSTOMHANDLER_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(3000);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let app = Router::new()
        .route("/", post(invoke))
        .route("/*path", post(invoke))
        .with_state(state);
    let listener = TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn invoke(State(state): State<AppState>, body: Bytes) -> Response {
    match handle_invocation(&state, &body).await {
        Ok(()) => (StatusCode::OK, Json(json!({}))).into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": err.to_string() })),
        )
            .into_response(),
    }
}

async fn handle_invocation(state: &AppState, body: &[u8]) -> Result<(), HandlerError> {
    let payload = extract_trigger_payload(body)?;
    state.handler.handle_payload(&payload).await
}
