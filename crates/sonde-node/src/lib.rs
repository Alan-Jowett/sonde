// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

pub mod bpf_dispatch;
pub mod bpf_helpers;
pub mod bpf_runtime;
pub mod crypto;
pub mod error;
#[cfg(feature = "esp")]
pub mod esp_sleep;
#[cfg(feature = "esp")]
pub mod esp_storage;
#[cfg(feature = "esp")]
pub mod esp_transport;
pub mod hal;
pub mod key_store;
pub mod map_storage;
pub mod program_store;
pub mod rbpf_adapter;
pub mod sleep;
pub mod traits;
pub mod wake_cycle;

/// Firmware ABI version. Bumped when the helper API changes.
pub const FIRMWARE_ABI_VERSION: u32 = 1;
