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
    extract_http_trigger_body, extract_trigger_payload, format_ingest_response, AzureHandler,
    AzureTablesStore, HandlerError, RuntimeConfig, StorageQueuePublisher,
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
        .route("/ProgramIngest", post(program_ingest))
        .route("/", post(invoke))
        .route("/{*path}", post(invoke))
        .with_state(state);
    let listener = TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn invoke(State(state): State<AppState>, body: Bytes) -> Response {
    match handle_invocation(&state, &body).await {
        Ok(()) => (StatusCode::OK, Json(json!({}))).into_response(),
        Err(err) => {
            let msg = err.to_string();
            tracing::error!(error = msg, "handler invocation failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": msg })),
            )
                .into_response()
        }
    }
}

async fn handle_invocation(state: &AppState, body: &[u8]) -> Result<(), HandlerError> {
    let payload = extract_trigger_payload(body)?;
    state.handler.handle_payload(&payload).await
}

async fn program_ingest(State(state): State<AppState>, body: Bytes) -> Response {
    let json_body = match extract_http_trigger_body(&body) {
        Ok(body) => body,
        Err(err) => {
            let msg = err.to_string();
            tracing::error!(error = msg, "program ingest: failed to parse HTTP envelope");
            let resp = format_ingest_response(
                400,
                &serde_json::json!({ "error": msg }),
            );
            return (StatusCode::OK, Json(resp)).into_response();
        }
    };

    match state.handler.handle_program_ingest(&json_body).await {
        Ok(ingest_resp) => {
            let resp = format_ingest_response(
                200,
                &serde_json::to_value(&ingest_resp).unwrap_or_default(),
            );
            (StatusCode::OK, Json(resp)).into_response()
        }
        Err(ingest_err) => {
            let msg = ingest_err.message.clone();
            let status = ingest_err.status_code;
            tracing::error!(error = msg, http_status = status, "program ingest failed");
            let resp = format_ingest_response(
                status,
                &serde_json::json!({ "error": msg }),
            );
            (StatusCode::OK, Json(resp)).into_response()
        }
    }
}
