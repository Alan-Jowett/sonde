// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! ESP32-S3 radio modem firmware.
//!
//! Bridges USB-CDC serial and ESP-NOW radio, relaying opaque frames
//! with MAC address and RSSI metadata. The modem is protocol-unaware:
//! it does not perform HMAC verification, CBOR parsing, or session management.
//!
//! Platform-independent modules (`bridge`, `peer_table`, `status`) compile
//! and test on any host. ESP-IDF modules (`espnow`, `usb_cdc`, `ble`) require
//! the `esp` feature and the Xtensa toolchain. The firmware entry point
//! is in `src/bin/modem.rs`.

// MD-0505: `quiet` and `verbose` are mutually exclusive log-level features.
#[cfg(all(feature = "quiet", feature = "verbose"))]
compile_error!(
    "features `quiet` and `verbose` are mutually exclusive; \
     use `--features esp,verbose --no-default-features` for verbose builds"
);

pub mod bridge;
pub mod peer_table;
pub mod status;

#[cfg(feature = "esp")]
pub mod ble;
#[cfg(feature = "esp")]
pub mod display;
#[cfg(feature = "esp")]
pub mod espnow;
#[cfg(feature = "esp")]
pub mod usb_cdc;
