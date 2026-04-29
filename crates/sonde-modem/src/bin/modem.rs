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
    use esp_idf_hal::peripherals::Peripherals;
    use esp_idf_svc::eventloop::EspSystemEventLoop;
    use esp_idf_svc::log::EspLogger;
    use esp_idf_svc::nvs::EspDefaultNvsPartition;
    use log::{error, info};

    use sonde_modem::ble::EspBleDriver;
    use sonde_modem::bridge::Bridge;
    use sonde_modem::display::{EspSsd1306Display, ModemDisplay};
    use sonde_modem::status::ModemCounters;

    // Link ESP-IDF patches and initialize logging.
    esp_idf_svc::sys::link_patches();
    EspLogger::initialize_default();

    // Build-type–aware runtime log level (MD-0505).
    // In debug builds or with the `verbose` feature, default to INFO.
    // In release builds without `verbose`, default to WARN.
    #[cfg(any(debug_assertions, feature = "verbose"))]
    log::set_max_level(log::LevelFilter::Info);
    #[cfg(not(any(debug_assertions, feature = "verbose")))]
    log::set_max_level(log::LevelFilter::Warn);

    info!(
        "sonde-modem firmware starting (commit {})",
        env!("SONDE_GIT_COMMIT")
    );

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

    // Initialize BLE GATT server (MD-0400).
    // esp32-nimble manages the NimBLE stack internally; no Modem peripheral
    // token is needed here. WiFi/ESP-NOW and NimBLE share the radio via
    // ESP-IDF coexistence management (CONFIG_ESP32_WIFI_BT_COEX).
    let ble = EspBleDriver::new();
    info!("BLE GATT server initialized (advertising off by default — MD-0412)");

    let display = match EspSsd1306Display::new() {
        Ok(display) => {
            info!("SSD1306 display initialized on I2C0 (addr=0x3C, SDA=GPIO5, SCL=GPIO6)");
            ModemDisplay::new(display)
        }
        Err(e) => {
            error!(
                "failed to initialize SSD1306 display: {}; continuing with display disabled",
                e
            );
            ModemDisplay::disabled()
        }
    };

    let espnow = sonde_modem::espnow::EspNowDriver::new(
        peripherals.modem,
        sysloop,
        nvs,
        &counters,
        usb_connected,
    )
    .unwrap_or_else(|e| {
        error!("failed to initialize ESP-NOW: {:?}", e);
        panic!("fatal: ESP-NOW init failed");
    });

    use sonde_modem::bridge::ButtonScanner;

    // Configure GPIO2 (XIAO D1 / 1-Wire data line) as input with internal
    // pull-up for button detection (MD-0600).
    unsafe {
        esp_idf_sys::gpio_reset_pin(esp_idf_sys::gpio_num_t_GPIO_NUM_2);
        esp_idf_sys::esp!(esp_idf_sys::gpio_set_direction(
            esp_idf_sys::gpio_num_t_GPIO_NUM_2,
            esp_idf_sys::gpio_mode_t_GPIO_MODE_INPUT,
        ))
        .expect("failed to configure GPIO2 direction");
        esp_idf_sys::esp!(esp_idf_sys::gpio_set_pull_mode(
            esp_idf_sys::gpio_num_t_GPIO_NUM_2,
            esp_idf_sys::gpio_pull_mode_t_GPIO_PULLUP_ONLY,
        ))
        .expect("failed to configure GPIO2 pull-up");
    }
    info!("GPIO2 configured as button input (active-low, pull-up — MD-0600)");

    // Configure GPIO3 (XIAO D2) to latch low before enabling output, so it is
    // driven low at boot without a transient high pulse.
    unsafe {
        esp_idf_sys::gpio_reset_pin(esp_idf_sys::gpio_num_t_GPIO_NUM_3);
        esp_idf_sys::esp!(esp_idf_sys::gpio_set_level(
            esp_idf_sys::gpio_num_t_GPIO_NUM_3,
            0,
        ))
        .expect("failed to set GPIO3 low");
        esp_idf_sys::esp!(esp_idf_sys::gpio_set_direction(
            esp_idf_sys::gpio_num_t_GPIO_NUM_3,
            esp_idf_sys::gpio_mode_t_GPIO_MODE_OUTPUT,
        ))
        .expect("failed to configure GPIO3 direction");
    }
    info!("GPIO3 (XIAO D2) configured as output, low");

    let button_scanner = ButtonScanner::new(|| {
        // Active-low: GPIO reads 0 when pressed, 1 when idle.
        // Return true when pressed.
        unsafe { esp_idf_sys::gpio_get_level(esp_idf_sys::gpio_num_t_GPIO_NUM_2) == 0 }
    });

    let mut bridge =
        Bridge::with_ble_button_and_display(usb, espnow, ble, button_scanner, display, counters);

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
