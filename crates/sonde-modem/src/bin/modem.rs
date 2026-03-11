// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! ESP32-S3 radio modem firmware entry point.
//!
//! This binary is only built with the `esp` feature enabled and the
//! `xtensa-esp32s3-espidf` target.

#[cfg(not(feature = "esp"))]
fn main() {
    eprintln!("The modem firmware binary requires the `esp` feature.");
    eprintln!(
        "Build with: cargo build -p sonde-modem --features esp --target xtensa-esp32s3-espidf"
    );
    std::process::exit(1);
}

#[cfg(feature = "esp")]
fn main() {
    use esp_idf_hal::prelude::Peripherals;
    use esp_idf_svc::eventloop::EspSystemEventLoop;
    use esp_idf_svc::log::EspLogger;
    use esp_idf_svc::nvs::EspDefaultNvsPartition;
    use log::info;

    use sonde_modem::bridge::Bridge;
    use sonde_modem::status::ModemCounters;

    // Link ESP-IDF patches and initialize logging.
    esp_idf_svc::sys::link_patches();
    EspLogger::initialize_default();

    info!("sonde-modem firmware starting");

    let peripherals = Peripherals::take().expect("failed to take peripherals");
    let sysloop = EspSystemEventLoop::take().expect("failed to take event loop");
    let nvs = EspDefaultNvsPartition::take().expect("failed to take NVS partition");

    let counters = ModemCounters::new();
    let usb = sonde_modem::usb_cdc::UsbCdcDriver::new();
    let espnow = sonde_modem::espnow::EspNowDriver::new(peripherals.modem, sysloop, nvs, &counters);

    let mut bridge = Bridge::new(usb, espnow, counters);

    // Send MODEM_READY on boot.
    bridge.send_modem_ready();

    info!("entering main loop");

    loop {
        bridge.poll();

        // Yield briefly to avoid pegging the CPU and starving
        // lower-priority ESP-IDF tasks.
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}
