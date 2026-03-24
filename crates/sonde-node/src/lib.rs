// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

pub mod ble_pairing;
pub mod bpf_dispatch;
pub mod bpf_helpers;
pub mod bpf_runtime;
pub mod crypto;
pub mod error;
#[cfg(feature = "esp")]
pub mod esp_ble_pairing;
#[cfg(feature = "esp")]
pub mod esp_hal;
#[cfg(feature = "esp")]
pub mod esp_sleep;
#[cfg(feature = "esp")]
pub mod esp_storage;
#[cfg(feature = "esp")]
pub mod esp_transport;
pub mod hal;
pub mod key_store;
pub mod map_storage;
pub mod peer_request;
pub mod program_store;
pub mod sleep;
pub mod sonde_bpf_adapter;
pub mod traits;
pub mod wake_cycle;

/// Firmware ABI version. Bumped when the helper API changes.
pub const FIRMWARE_ABI_VERSION: u32 = 1;

/// Shared log-capture utility for tests (ND-1006, ND-1010).
#[cfg(test)]
pub(crate) mod test_log_capture {
    use log::{Level, Log, Metadata, Record};
    use std::sync::Mutex;

    struct TestLogger;

    static LOG_RECORDS: Mutex<Vec<(Level, String)>> = Mutex::new(Vec::new());

    impl Log for TestLogger {
        fn enabled(&self, _metadata: &Metadata) -> bool {
            true
        }
        fn log(&self, record: &Record) {
            if let Ok(mut records) = LOG_RECORDS.lock() {
                records.push((record.level(), format!("{}", record.args())));
            }
        }
        fn flush(&self) {}
    }

    static TEST_LOGGER: TestLogger = TestLogger;

    pub fn init() {
        let _ = log::set_logger(&TEST_LOGGER);
        log::set_max_level(log::LevelFilter::Trace);
    }

    /// Drain all captured records, returning them and clearing the buffer.
    pub fn drain_log_records() -> Vec<(Level, String)> {
        let mut records = LOG_RECORDS.lock().unwrap();
        records.drain(..).collect()
    }
}
