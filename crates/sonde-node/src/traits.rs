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

    /// Busy-wait for the specified number of microseconds.
    /// Default implementation rounds up to milliseconds; platform
    /// implementations should use a hardware timer for µs precision.
    fn delay_us(&self, us: u32) {
        if us > 0 {
            self.delay_ms((us.saturating_add(999)) / 1000);
        }
    }
}

/// Deep sleep controller.
pub trait SleepController {
    /// Enter deep sleep for the specified number of seconds.
    /// This function does not return under normal operation.
    fn enter_deep_sleep(&mut self, seconds: u32) -> !;

    /// Restart the firmware. Does not return.
    fn reboot(&mut self) -> !;
}

/// Serial transport used during USB pairing mode.
///
/// Abstracts the USB serial port so that `run_pairing_mode` can be
/// tested with a mock implementation. The ESP32-C3 implementation
/// uses the USB Serial/JTAG peripheral; other platforms may use
/// UART or USB-CDC.
pub trait PairingSerial {
    /// Read bytes into `buf`. Returns the number of bytes read.
    ///
    /// Blocks for up to `timeout_ms` milliseconds. Returns `Ok(0)` on
    /// timeout with no data. Returns `Err` on disconnect or I/O error
    /// (the pairing loop treats any error as a disconnect signal).
    fn read(&mut self, buf: &mut [u8], timeout_ms: u32) -> NodeResult<usize>;

    /// Write `data` to the serial port. Returns `Err` on I/O error or
    /// if the data could not be delivered after retries (disconnect).
    fn write(&mut self, data: &[u8]) -> NodeResult<()>;
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

    /// Reset the schedule to default values (default interval, partition 0).
    /// Called during factory reset.
    fn reset_schedule(&mut self) -> NodeResult<()>;

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

    // --- WiFi channel ---

    /// Read the stored WiFi channel (1–13). Returns `None` if not set.
    fn read_channel(&self) -> Option<u8> {
        None
    }

    /// Persist the WiFi channel (1–13) to storage.
    fn write_channel(&mut self, _channel: u8) -> NodeResult<()> {
        Ok(())
    }

    // --- BLE pairing artifacts (ND-0916) ---

    /// Read the encrypted peer payload stored during BLE provisioning.
    /// Returns `None` if no payload is stored (node not yet BLE-provisioned).
    fn read_peer_payload(&self) -> Option<Vec<u8>> {
        None
    }

    /// Check whether a peer payload is stored without reading/copying it.
    ///
    /// Default implementation delegates to `read_peer_payload().is_some()`.
    /// Platform-specific implementations can override this to avoid heap
    /// allocation (e.g., NVS key existence check).
    fn has_peer_payload(&self) -> bool {
        self.read_peer_payload().is_some()
    }

    /// Persist the encrypted peer payload received in NODE_PROVISION.
    fn write_peer_payload(&mut self, _payload: &[u8]) -> NodeResult<()> {
        Ok(())
    }

    /// Erase the encrypted peer payload from storage.
    /// Called after the first successful WAKE/COMMAND exchange (ND-0914)
    /// and during factory reset (ND-0917).
    fn erase_peer_payload(&mut self) -> NodeResult<()> {
        Ok(())
    }

    /// Read the registration-complete flag.
    /// Returns `true` if the node has been acknowledged by the gateway
    /// (PEER_ACK received and validated).
    fn read_reg_complete(&self) -> bool {
        false
    }

    /// Persist the registration-complete flag.
    /// Set to `true` on valid PEER_ACK (ND-0913); cleared to `false`
    /// on NODE_PROVISION (ND-0906).
    fn write_reg_complete(&mut self, _complete: bool) -> NodeResult<()> {
        Ok(())
    }
}
