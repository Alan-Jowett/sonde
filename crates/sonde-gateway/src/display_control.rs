// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Time a terminal button-pairing status screen remains visible before restore.
pub const BUTTON_EXIT_REASON_DISPLAY_DURATION: Duration = Duration::from_secs(2);
/// Idle timeout for gateway-owned temporary/status display screens.
pub const STATUS_PAGE_TIMEOUT: Duration = Duration::from_secs(60);
/// Scroll cadence for oversized node status pages.
pub const NODE_STATUS_SCROLL_INTERVAL: Duration = Duration::from_millis(50);
/// Vertical scroll increment per tick for oversized node status pages.
pub const NODE_STATUS_SCROLL_STEP_PX: u32 = 3;

#[derive(Debug, Default)]
pub struct StatusPageCycle {
    pub next_page_index: usize,
}

pub struct ActiveStatusPageScroll {
    pub stop_requested: Arc<AtomicBool>,
    pub handle: tokio::task::JoinHandle<()>,
}

pub type StatusPageScrollTask = Arc<tokio::sync::Mutex<Option<ActiveStatusPageScroll>>>;

pub fn claim_display_generation(display_generation: &Arc<AtomicU64>) -> u64 {
    display_generation.fetch_add(1, Ordering::SeqCst) + 1
}

pub fn invalidate_display_restore(display_generation: &Arc<AtomicU64>) {
    let _ = claim_display_generation(display_generation);
}

pub fn try_claim_display_restore(display_generation: &AtomicU64, generation: u64) -> bool {
    display_generation
        .compare_exchange(
            generation,
            generation.saturating_add(1),
            Ordering::SeqCst,
            Ordering::SeqCst,
        )
        .is_ok()
}

pub async fn reset_status_page_cycle(status_page_cycle: &Arc<tokio::sync::Mutex<StatusPageCycle>>) {
    status_page_cycle.lock().await.next_page_index = 0;
}

pub async fn cancel_status_page_scroll(scroll_task: &StatusPageScrollTask) {
    let active = scroll_task.lock().await.take();
    if let Some(active) = active {
        active.stop_requested.store(true, Ordering::SeqCst);
        let _ = active.handle.await;
    }
}
