// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! ESP32 node firmware entry point.
//!
//! This binary is only built with the `esp` feature enabled and the
//! `xtensa-esp32-espidf` target.

#[cfg(not(feature = "esp"))]
fn main() {
    eprintln!("The node firmware binary requires the `esp` feature.");
    eprintln!(
        "Build with: cargo build -p sonde-node --bin node --features esp --target xtensa-esp32-espidf"
    );
    std::process::exit(1);
}

#[cfg(feature = "esp")]
fn main() {
    use esp_idf_svc::log::EspLogger;
    use log::info;

    // Link ESP-IDF patches and initialize logging.
    esp_idf_svc::sys::link_patches();
    EspLogger::initialize_default();

    info!("sonde-node booting");
    info!("firmware ABI version: {}", sonde_node::FIRMWARE_ABI_VERSION);
    info!("sonde-node ready");

    // Idle loop — the wake cycle engine will be invoked here once
    // the platform trait implementations are complete.
    loop {
        std::thread::sleep(std::time::Duration::from_millis(1000));
    }
}
