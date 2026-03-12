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
    let usb = sonde_modem::usb_cdc::UsbCdcDriver::new(
        peripherals.usb_serial,
        peripherals.pins.gpio19,
        peripherals.pins.gpio20,
    );

    // Share the USB connected flag with the ESP-NOW receive callback
    // so it can discard frames when USB is disconnected (MD-0301).
    let usb_connected = usb.connected();
    let espnow = sonde_modem::espnow::EspNowDriver::new(
        peripherals.modem,
        sysloop,
        nvs,
        &counters,
        usb_connected,
    );

    let mut bridge = Bridge::new(usb, espnow, counters);

    // Initialize the task watchdog with a 10-second timeout (MD-0302).
    unsafe {
        let wdt_config = esp_idf_sys::esp_task_wdt_config_t {
            timeout_ms: 10_000,
            idle_core_mask: 0,
            trigger_panic: true,
        };
        esp_idf_sys::esp!(esp_idf_sys::esp_task_wdt_reconfigure(&wdt_config))
            .expect("failed to configure watchdog");
        esp_idf_sys::esp!(esp_idf_sys::esp_task_wdt_add(
            esp_idf_sys::xTaskGetCurrentTaskHandle()
        ))
        .expect("failed to add task to watchdog");
    }

    // Retry MODEM_READY for up to 2 seconds to handle slow USB
    // enumeration (MD-0104).
    {
        let start = std::time::Instant::now();
        loop {
            bridge.send_modem_ready();
            if bridge.is_usb_connected() || start.elapsed().as_millis() >= 2000 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    info!("entering main loop");

    loop {
        bridge.poll();

        // Feed the watchdog each iteration (MD-0302).
        unsafe {
            esp_idf_sys::esp_task_wdt_reset();
        }

        // Yield briefly to avoid pegging the CPU and starving
        // lower-priority ESP-IDF tasks.
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}
