// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Weak};

use tokio::sync::RwLock;
use tonic::Status;
use tracing::warn;

use crate::ble_pairing::BlePairingController;
use crate::display_banner::{send_display_message, send_gateway_version_banner};
use crate::display_control::{
    cancel_status_page_scroll, claim_display_generation, reset_status_page_cycle,
    try_claim_display_restore, StatusPageCycle, StatusPageScrollTask, STATUS_PAGE_TIMEOUT,
};
use crate::modem::UsbEspNowTransport;

#[derive(Clone)]
pub struct ActiveDisplayState {
    transport: Arc<UsbEspNowTransport>,
    controller: Arc<BlePairingController>,
    display_generation: Arc<AtomicU64>,
    status_page_cycle: Arc<tokio::sync::Mutex<StatusPageCycle>>,
    status_page_scroll_task: StatusPageScrollTask,
}

impl ActiveDisplayState {
    pub fn new(
        transport: Arc<UsbEspNowTransport>,
        controller: Arc<BlePairingController>,
        display_generation: Arc<AtomicU64>,
        status_page_cycle: Arc<tokio::sync::Mutex<StatusPageCycle>>,
        status_page_scroll_task: StatusPageScrollTask,
    ) -> Self {
        Self {
            transport,
            controller,
            display_generation,
            status_page_cycle,
            status_page_scroll_task,
        }
    }
}

#[derive(Clone, Default)]
pub struct DisplayStateHandle {
    inner: Arc<RwLock<Option<ActiveDisplayState>>>,
}

impl DisplayStateHandle {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn set(&self, state: ActiveDisplayState) {
        *self.inner.write().await = Some(state);
    }

    pub async fn clear(&self) {
        *self.inner.write().await = None;
    }

    pub async fn get(&self) -> Option<ActiveDisplayState> {
        self.inner.read().await.clone()
    }
}

fn schedule_display_restore(state: &ActiveDisplayState, generation: u64) {
    let transport: Weak<UsbEspNowTransport> = Arc::downgrade(&state.transport);
    let controller = Arc::clone(&state.controller);
    let display_generation = Arc::clone(&state.display_generation);
    let status_page_cycle = Arc::clone(&state.status_page_cycle);
    let status_page_scroll_task = Arc::clone(&state.status_page_scroll_task);

    tokio::spawn(async move {
        tokio::time::sleep(STATUS_PAGE_TIMEOUT).await;
        if controller.session_origin().await.is_some() {
            return;
        }
        if !try_claim_display_restore(display_generation.as_ref(), generation) {
            return;
        }
        cancel_status_page_scroll(&status_page_scroll_task).await;
        reset_status_page_cycle(&status_page_cycle).await;
        let Some(transport) = transport.upgrade() else {
            return;
        };
        if let Err(e) = send_gateway_version_banner(&transport).await {
            warn!(error = %e, "failed to restore gateway version banner after transient display");
        }
    });
}

async fn restore_display_failure(state: &ActiveDisplayState, generation: u64) {
    if !try_claim_display_restore(state.display_generation.as_ref(), generation) {
        return;
    }
    if let Err(e) = send_gateway_version_banner(&state.transport).await {
        warn!(
            error = %e,
            "failed to restore gateway version banner after transient display error"
        );
    }
}

pub async fn show_modem_display_message(
    state: &ActiveDisplayState,
    lines: &[String],
) -> Result<(), Status> {
    if state.controller.session_origin().await.is_some() {
        return Err(Status::failed_precondition(
            "BLE pairing session owns the modem display",
        ));
    }

    if lines.is_empty() || lines.len() > 4 {
        return Err(Status::invalid_argument(
            "lines must contain between 1 and 4 entries",
        ));
    }

    cancel_status_page_scroll(&state.status_page_scroll_task).await;
    let generation = claim_display_generation(&state.display_generation);
    reset_status_page_cycle(&state.status_page_cycle).await;

    let line_refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    if let Err(e) = send_display_message(&state.transport, &line_refs).await {
        restore_display_failure(state, generation).await;
        return Err(Status::internal(format!(
            "show modem display message failed: {e}"
        )));
    }

    schedule_display_restore(state, generation);
    Ok(())
}
