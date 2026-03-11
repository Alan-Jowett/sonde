// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use crate::error::NodeResult;

/// Radio transport for sending and receiving frames.
pub trait Transport {
    /// Send a raw frame to the gateway.
    fn send(&mut self, frame: &[u8]) -> NodeResult<()>;

    /// Wait for a frame from the gateway with the given timeout.
    /// Returns `Ok(Some(data))` if a frame arrives within the timeout,
    /// `Ok(None)` if the timeout expires, or `Err` on transport failure.
    fn recv(&mut self, timeout_ms: u32) -> NodeResult<Option<Vec<u8>>>;
}

/// Hardware random number generator.
pub trait Rng {
    /// Generate a 64-bit random value using the hardware RNG.
    fn random_u64(&mut self) -> u64;
}

/// Monotonic clock for measuring elapsed time within a wake cycle.
pub trait Clock {
    /// Milliseconds elapsed since boot (or since the clock was started).
    fn elapsed_ms(&self) -> u64;

    /// Busy-wait for the specified number of milliseconds.
    /// Used for retry backoff. Implementations on real hardware should
    /// use a platform timer; test mocks can be a no-op.
    fn delay_ms(&self, ms: u32);
}

/// Deep sleep controller.
pub trait SleepController {
    /// Enter deep sleep for the specified number of seconds.
    /// This function does not return under normal operation.
    fn enter_deep_sleep(&mut self, seconds: u32) -> !;

    /// Restart the firmware. Does not return.
    fn reboot(&mut self) -> !;
}

/// Persistent storage for key partition, schedule, and program partitions.
pub trait PlatformStorage {
    // --- Key partition ---

    /// Read the PSK and key_hint from the key partition.
    /// Returns `None` if the node is unpaired (no magic bytes).
    fn read_key(&self) -> Option<(u16, [u8; 32])>;

    /// Write a PSK and key_hint to the key partition (USB pairing).
    /// Returns an error if a key is already present.
    fn write_key(&mut self, key_hint: u16, psk: &[u8; 32]) -> NodeResult<()>;

    /// Erase the key partition (factory reset).
    fn erase_key(&mut self) -> NodeResult<()>;

    // --- Schedule partition ---

    /// Read the base wake interval in seconds and the active program partition flag.
    /// Returns (interval_s, active_partition: 0 or 1).
    fn read_schedule(&self) -> (u32, u8);

    /// Write the base wake interval.
    fn write_schedule_interval(&mut self, interval_s: u32) -> NodeResult<()>;

    /// Write the active program partition flag (0 or 1).
    fn write_active_partition(&mut self, partition: u8) -> NodeResult<()>;

    // --- Program partitions ---

    /// Read a program partition (0 = A, 1 = B). Returns the raw CBOR image bytes
    /// or `None` if the partition is empty/erased.
    fn read_program(&self, partition: u8) -> Option<Vec<u8>>;

    /// Write a program image to the specified partition.
    fn write_program(&mut self, partition: u8, image: &[u8]) -> NodeResult<()>;

    /// Erase a program partition.
    fn erase_program(&mut self, partition: u8) -> NodeResult<()>;

    // --- Wake reason flags (stored in RTC-persistent area) ---

    /// Read and clear the "early wake requested" flag.
    fn take_early_wake_flag(&mut self) -> bool;

    /// Set the "early wake requested" flag (persists through deep sleep).
    fn set_early_wake_flag(&mut self) -> NodeResult<()>;
}
