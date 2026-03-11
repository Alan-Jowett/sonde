// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Modem counters and status tracking.
//!
//! Maintains `tx_count`, `rx_count`, `tx_fail_count`, and `uptime_s`.
//! All counters (including uptime) reset to zero on boot and on RESET.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// Thread-safe modem counters, shareable between the main loop and
/// the ESP-NOW receive/send callbacks.
pub struct ModemCounters {
    tx_count: AtomicU32,
    rx_count: AtomicU32,
    tx_fail_count: AtomicU32,
    /// Microsecond timestamp of the last reset (from `Instant`).
    /// We store elapsed micros at reset time so `uptime_s` reflects
    /// time since last RESET, not since boot.
    reset_epoch_us: AtomicU64,
    boot_time: Instant,
}

impl ModemCounters {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            tx_count: AtomicU32::new(0),
            rx_count: AtomicU32::new(0),
            tx_fail_count: AtomicU32::new(0),
            reset_epoch_us: AtomicU64::new(0),
            boot_time: Instant::now(),
        })
    }

    pub fn inc_tx(&self) {
        self.tx_count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_rx(&self) {
        self.rx_count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_tx_fail(&self) {
        self.tx_fail_count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn tx_count(&self) -> u32 {
        self.tx_count.load(Ordering::Relaxed)
    }

    pub fn rx_count(&self) -> u32 {
        self.rx_count.load(Ordering::Relaxed)
    }

    pub fn tx_fail_count(&self) -> u32 {
        self.tx_fail_count.load(Ordering::Relaxed)
    }

    /// Returns seconds since last boot or RESET.
    pub fn uptime_s(&self) -> u32 {
        let total_us = self.boot_time.elapsed().as_micros() as u64;
        let epoch_us = self.reset_epoch_us.load(Ordering::Relaxed);
        ((total_us - epoch_us) / 1_000_000) as u32
    }

    /// Reset all counters to zero and restart uptime (called on RESET command).
    pub fn reset(&self) {
        self.tx_count.store(0, Ordering::Relaxed);
        self.rx_count.store(0, Ordering::Relaxed);
        self.tx_fail_count.store(0, Ordering::Relaxed);
        let now_us = self.boot_time.elapsed().as_micros() as u64;
        self.reset_epoch_us.store(now_us, Ordering::Relaxed);
    }
}
