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

    // Platform crypto (available on all targets).
    let _hmac = sonde_node::crypto::SoftwareHmac;
    let _sha256 = sonde_node::crypto::SoftwareSha256;

    // Platform trait implementations (ESP-only):
    //   let rng = sonde_node::crypto::EspRng;
    //   let transport = sonde_node::esp_transport::EspNowTransport::new()?;
    //   let storage = sonde_node::esp_storage::NvsStorage::new()?;
    //   let sleep_ctrl = sonde_node::esp_sleep::EspSleepController;
    //
    // Wire these into WakeCycleEngine once the stubs are implemented.

    // Idle loop — replaced by wake-cycle engine once platform
    // implementations are complete.
    loop {
        std::thread::sleep(std::time::Duration::from_millis(1000));
    }
}
