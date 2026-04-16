// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

// ND-1012: `quiet` and `verbose` are mutually exclusive log-level features.
#[cfg(all(feature = "quiet", feature = "verbose"))]
compile_error!(
    "features `quiet` and `verbose` are mutually exclusive; \
     use `--features esp,verbose --no-default-features` for verbose builds"
);

pub mod async_queue;
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
pub mod node_aead;
pub mod peer_request;
pub mod program_store;
pub mod sleep;
pub mod sonde_bpf_adapter;
pub mod traits;
pub mod wake_cycle;

/// Firmware ABI version. Bumped when the helper API changes.
pub const FIRMWARE_ABI_VERSION: u32 = 2;

/// Shared log-capture utility for tests (ND-1006, ND-1010).
///
/// Log records are captured per-thread to avoid cross-test interference
/// when tests run in parallel (the Rust default).
///
/// Only available in debug test builds — all callers are gated with
/// `#[cfg(debug_assertions)]` because the log levels they assert on are
/// stripped at compile time in release builds (ND-1012).
#[cfg(all(test, debug_assertions))]
pub(crate) mod test_log_capture {
    use log::{Level, Log, Metadata, Record};
    use std::collections::HashMap;
    use std::sync::{Mutex, Once};
    use std::thread::{self, ThreadId};

    struct TestLogger;

    type LogMap = HashMap<ThreadId, Vec<(Level, String)>>;
    static LOG_RECORDS: Mutex<Option<LogMap>> = Mutex::new(None);

    impl Log for TestLogger {
        fn enabled(&self, _metadata: &Metadata) -> bool {
            true
        }
        fn log(&self, record: &Record) {
            if let Ok(mut guard) = LOG_RECORDS.lock() {
                let map = guard.get_or_insert_with(HashMap::new);
                let thread_id = thread::current().id();
                map.entry(thread_id)
                    .or_default()
                    .push((record.level(), format!("{}", record.args())));
            }
        }
        fn flush(&self) {}
    }

    static TEST_LOGGER: TestLogger = TestLogger;
    static INIT: Once = Once::new();

    pub fn init() {
        INIT.call_once(|| {
            let _ = log::set_logger(&TEST_LOGGER);
            log::set_max_level(log::LevelFilter::Trace);
        });
    }

    /// Drain all captured records for the current thread, returning them
    /// and clearing the buffer for this thread only.
    pub fn drain_log_records() -> Vec<(Level, String)> {
        let thread_id = thread::current().id();
        let mut guard = LOG_RECORDS.lock().unwrap();
        guard
            .get_or_insert_with(HashMap::new)
            .remove(&thread_id)
            .unwrap_or_default()
    }
}
