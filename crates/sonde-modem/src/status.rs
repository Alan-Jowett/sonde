// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Modem counters and status tracking.
//!
//! Maintains `tx_count`, `rx_count`, `tx_fail_count`, and `uptime_s`.
//! All counters (including uptime) reset to zero on boot and on RESET.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Thread-safe modem counters, shareable between the main loop and
/// the ESP-NOW receive/send callbacks.
pub struct ModemCounters {
    tx_count: AtomicU32,
    rx_count: AtomicU32,
    tx_fail_count: AtomicU32,
    /// Milliseconds elapsed since boot at the last reset. Protected by
    /// a Mutex because Xtensa lacks 64-bit atomics. Contention is
    /// negligible — only accessed on RESET and GET_STATUS.
    reset_epoch_ms: Mutex<u64>,
    boot_time: Instant,
}

impl ModemCounters {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            tx_count: AtomicU32::new(0),
            rx_count: AtomicU32::new(0),
            tx_fail_count: AtomicU32::new(0),
            reset_epoch_ms: Mutex::new(0),
            boot_time: Instant::now(),
        })
    }

    /// Create counters with a custom boot time (for testing).
    #[cfg(test)]
    fn new_with_boot_time(boot_time: Instant) -> Arc<Self> {
        Arc::new(Self {
            tx_count: AtomicU32::new(0),
            rx_count: AtomicU32::new(0),
            tx_fail_count: AtomicU32::new(0),
            reset_epoch_ms: Mutex::new(0),
            boot_time,
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
        let total_ms = self.boot_time.elapsed().as_millis() as u64;
        let epoch_ms = *self.reset_epoch_ms.lock().unwrap();
        (total_ms.saturating_sub(epoch_ms) / 1000) as u32
    }

    /// Reset all counters to zero and restart uptime (called on RESET command).
    pub fn reset(&self) {
        self.tx_count.store(0, Ordering::Relaxed);
        self.rx_count.store(0, Ordering::Relaxed);
        self.tx_fail_count.store(0, Ordering::Relaxed);
        let now_ms = self.boot_time.elapsed().as_millis() as u64;
        *self.reset_epoch_ms.lock().unwrap() = now_ms;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn initial_values_are_zero() {
        let c = ModemCounters::new();
        assert_eq!(c.tx_count(), 0);
        assert_eq!(c.rx_count(), 0);
        assert_eq!(c.tx_fail_count(), 0);
    }

    #[test]
    fn inc_tx_increments() {
        let c = ModemCounters::new();
        c.inc_tx();
        c.inc_tx();
        c.inc_tx();
        assert_eq!(c.tx_count(), 3);
    }

    #[test]
    fn inc_rx_increments() {
        let c = ModemCounters::new();
        c.inc_rx();
        c.inc_rx();
        assert_eq!(c.rx_count(), 2);
    }

    #[test]
    fn inc_tx_fail_increments() {
        let c = ModemCounters::new();
        c.inc_tx_fail();
        assert_eq!(c.tx_fail_count(), 1);
    }

    #[test]
    fn counters_are_independent() {
        let c = ModemCounters::new();
        c.inc_tx();
        c.inc_tx();
        c.inc_rx();
        c.inc_tx_fail();
        c.inc_tx_fail();
        c.inc_tx_fail();
        assert_eq!(c.tx_count(), 2);
        assert_eq!(c.rx_count(), 1);
        assert_eq!(c.tx_fail_count(), 3);
    }

    #[test]
    fn reset_zeroes_all_counters() {
        let c = ModemCounters::new();
        c.inc_tx();
        c.inc_rx();
        c.inc_tx_fail();
        c.reset();
        assert_eq!(c.tx_count(), 0);
        assert_eq!(c.rx_count(), 0);
        assert_eq!(c.tx_fail_count(), 0);
    }

    #[test]
    fn uptime_near_zero_at_boot() {
        let c = ModemCounters::new();
        assert_eq!(c.uptime_s(), 0);
    }

    #[test]
    fn uptime_reflects_elapsed_time() {
        // Backdate boot_time by 5 seconds to avoid wall-clock sleeping.
        let boot = Instant::now() - Duration::from_secs(5);
        let c = ModemCounters::new_with_boot_time(boot);
        let uptime = c.uptime_s();
        assert!((4..=6).contains(&uptime), "expected ~5s, got {}", uptime);
    }

    #[test]
    fn uptime_resets_on_reset() {
        // Backdate boot_time by 5 seconds so uptime starts > 0.
        let boot = Instant::now() - Duration::from_secs(5);
        let c = ModemCounters::new_with_boot_time(boot);
        assert!(c.uptime_s() >= 4);
        c.reset();
        assert_eq!(c.uptime_s(), 0);
    }

    #[test]
    fn counters_work_after_reset() {
        let c = ModemCounters::new();
        c.inc_tx();
        c.inc_rx();
        c.reset();
        c.inc_tx();
        c.inc_tx();
        c.inc_tx_fail();
        assert_eq!(c.tx_count(), 2);
        assert_eq!(c.rx_count(), 0);
        assert_eq!(c.tx_fail_count(), 1);
    }

    #[test]
    fn arc_shared_across_threads() {
        let c = ModemCounters::new();
        let c2 = Arc::clone(&c);
        let handle = thread::spawn(move || {
            c2.inc_tx();
            c2.inc_rx();
        });
        handle.join().unwrap();
        assert_eq!(c.tx_count(), 1);
        assert_eq!(c.rx_count(), 1);
    }
}
