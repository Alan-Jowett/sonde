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
    use std::sync::Arc;

    use esp_idf_hal::prelude::Peripherals;
    use esp_idf_svc::bt::{BleEnabled, BtDriver};
    use esp_idf_svc::eventloop::EspSystemEventLoop;
    use esp_idf_svc::log::EspLogger;
    use esp_idf_svc::nvs::EspDefaultNvsPartition;
    use log::{error, info};

    use sonde_modem::ble::EspBleDriver;
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
    )
    .unwrap_or_else(|e| {
        error!("failed to initialize USB-CDC: {:?}", e);
        panic!("fatal: USB-CDC init failed");
    });

    // Share the USB connected flag with the ESP-NOW receive callback
    // so it can discard frames when USB is disconnected (MD-0301).
    let usb_connected = usb.connected();

    // Initialize BLE GATT server before WiFi/ESP-NOW so the modem peripheral
    // is consumed once at the ESP-IDF coexistence layer level.  On ESP32-S3,
    // WiFi and BLE share the radio via ESP-IDF's coexistence management.
    // After BtDriver takes the modem, we use unsafe Modem::new() for WiFi —
    // this is the standard coexistence pattern in esp-idf-hal.
    let bt = Arc::new(
        BtDriver::<BleEnabled>::new(peripherals.modem, None).unwrap_or_else(|e| {
            error!("failed to initialize Bluetooth driver: {:?}", e);
            panic!("fatal: Bluetooth init failed");
        }),
    );
    let ble = EspBleDriver::new(bt).unwrap_or_else(|e| {
        error!("failed to initialize BLE GATT server: {:?}", e);
        panic!("fatal: BLE GATT init failed");
    });
    info!("BLE GATT server initialized (advertising off by default — MD-0412)");

    // WiFi+ESP-NOW: use a second Modem token because the ESP32-S3 hardware
    // supports simultaneous WiFi and BLE via coexistence.
    //
    // Safety: `peripherals.modem` was consumed by BtDriver above.  Creating
    // a second Modem token is safe on ESP32-S3 because BT and WiFi share the
    // same radio hardware under ESP-IDF coexistence management.
    let wifi_modem = unsafe { esp_idf_hal::modem::Modem::new() };
    let espnow = sonde_modem::espnow::EspNowDriver::new(
        wifi_modem,
        sysloop,
        nvs,
        &counters,
        usb_connected,
    )
    .unwrap_or_else(|e| {
        error!("failed to initialize ESP-NOW: {:?}", e);
        panic!("fatal: ESP-NOW init failed");
    });

    let mut bridge = Bridge::with_ble(usb, espnow, ble, counters);

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
