// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

pub mod bpf_dispatch;
pub mod bpf_helpers;
pub mod bpf_runtime;
pub mod error;
pub mod hal;
pub mod key_store;
pub mod map_storage;
pub mod program_store;
#[cfg(feature = "rbpf")]
pub mod rbpf_adapter;
pub mod sleep;
pub mod traits;
pub mod wake_cycle;

/// Firmware ABI version. Bumped when the helper API changes.
pub const FIRMWARE_ABI_VERSION: u32 = 1;
