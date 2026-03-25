// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! ESP32-C3 node firmware entry point.
//!
//! This binary is only built with the `esp` feature enabled.

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
    use esp_idf_hal::gpio::{PinDriver, Pull};
    use esp_idf_hal::peripherals::Peripherals;
    use esp_idf_svc::eventloop::EspSystemEventLoop;
    use esp_idf_svc::log::EspLogger;
    use esp_idf_svc::nvs::EspDefaultNvsPartition;
    use log::{info, warn};

    use sonde_node::crypto::{EspRng, SoftwareHmac, SoftwareSha256};
    use sonde_node::esp_ble_pairing::run_ble_pairing_mode;
    use sonde_node::esp_hal::{EspBatteryReader, EspClock, EspHal};
    use sonde_node::esp_sleep::EspSleepController;
    use sonde_node::esp_storage::NvsStorage;
    use sonde_node::esp_transport::EspNowTransport;
    use sonde_node::map_storage::{MapStorage, MAP_BUDGET};
    use sonde_node::sonde_bpf_adapter::SondeBpfInterpreter;
    use sonde_node::traits::{PlatformStorage, SleepController};
    use sonde_node::wake_cycle::{run_wake_cycle, WakeCycleOutcome};

    // Link ESP-IDF patches and initialize logging.
    esp_idf_svc::sys::link_patches();
    EspLogger::initialize_default();

    // Build-type–aware runtime log level (ND-1012).
    // In debug builds or with the `verbose` feature, default to INFO.
    // In release builds without `verbose`, default to WARN.
    #[cfg(any(debug_assertions, feature = "verbose"))]
    log::set_max_level(log::LevelFilter::Info);
    #[cfg(not(any(debug_assertions, feature = "verbose")))]
    log::set_max_level(log::LevelFilter::Warn);

    info!("sonde-node booting (commit {})", env!("SONDE_GIT_COMMIT"));
    info!("firmware ABI version: {}", sonde_node::FIRMWARE_ABI_VERSION);

    // Log boot reason (ND-1000).
    let reset_reason = unsafe { esp_idf_svc::sys::esp_reset_reason() };
    let boot_reason = if reset_reason == esp_idf_svc::sys::esp_reset_reason_t_ESP_RST_DEEPSLEEP {
        "deep_sleep_wake"
    } else {
        "power_on"
    };
    info!("boot_reason={} (ND-1000)", boot_reason);

    // --- Initialize platform ---
    let peripherals = Peripherals::take().expect("failed to take peripherals");
    let sysloop = EspSystemEventLoop::take().expect("failed to take event loop");
    let nvs_partition = EspDefaultNvsPartition::take().expect("failed to take NVS");

    let mut sleep_ctrl = EspSleepController;

    let mut storage =
        NvsStorage::new(nvs_partition.clone()).expect("failed to initialize NVS storage");

    // Map storage: backed by MAP_BACKING in RTC slow SRAM so that map data
    // survives deep sleep (ND-0603). MAP_BUDGET is ~6 KB on ESP32-C3.
    //
    // Try to restore from the RTC layout record written by the previous wake
    // cycle. If the record is absent (cold boot) or invalid, fall back to an
    // empty MapStorage so the wake-cycle engine's normal allocate-on-mismatch
    // path handles initialisation.
    let mut map_storage =
        MapStorage::from_rtc(MAP_BUDGET).unwrap_or_else(|| MapStorage::new(MAP_BUDGET));

    // ---------------------------------------------------------------------------
    // Boot priority (ND-0900)
    //
    // Check in order:
    //   1. No PSK OR pairing button held ≥ 500 ms → BLE pairing mode
    //   2. PSK stored, reg_complete NOT set → PEER_REQUEST mode (WAKE cycle variant)
    //   3. PSK stored, reg_complete set → normal WAKE cycle
    // ---------------------------------------------------------------------------

    // (1) No PSK, or pairing button held ≥ 500 ms → BLE pairing mode.
    //
    // Pairing button is GPIO 9 on the ESP32-C3 DevKitM-1 (active LOW).
    // We sample it for 500 ms immediately after boot.  If the pin is
    // held LOW for the entire sampling window, button_held = true, which
    // triggers a factory reset before accepting new BLE credentials (ND-0917).
    let button_held = {
        // GPIO 9 is the BOOT button on most ESP32-C3 boards.
        // Configure as input with internal pull-up (active LOW).
        let button_pin = peripherals.pins.gpio9;
        let button = PinDriver::input(button_pin, Pull::Up)
            .expect("failed to configure pairing button GPIO");

        const SAMPLE_INTERVAL_MS: u32 = 10;
        const SAMPLE_COUNT: u32 = 500 / SAMPLE_INTERVAL_MS; // 50 samples over 500 ms
        let mut held_count: u32 = 0;
        for _ in 0..SAMPLE_COUNT {
            if button.is_low() {
                held_count += 1;
            }
            // Busy-wait 10 ms between samples
            unsafe {
                esp_idf_svc::sys::vTaskDelay(
                    (SAMPLE_INTERVAL_MS * esp_idf_svc::sys::CONFIG_FREERTOS_HZ) / 1000,
                );
            }
        }
        let held = held_count == SAMPLE_COUNT;
        if held {
            info!("pairing button held ≥ 500 ms — will factory reset on BLE provision");
        }
        held
    };

    if storage.read_key().is_none() || button_held {
        info!(
            "entering BLE pairing mode (no PSK={}, button_held={})",
            storage.read_key().is_none(),
            button_held
        );
        match run_ble_pairing_mode(&mut storage, &mut map_storage, button_held) {
            Ok(()) => {
                info!("BLE pairing mode exited — rebooting");
                sleep_ctrl.reboot();
            }
            Err(e) => {
                // BLE GATT server not yet implemented — deep sleep to conserve
                // battery until firmware is updated with BLE support.
                warn!("BLE pairing mode failed: {} — entering deep sleep", e);
                sleep_ctrl.enter_deep_sleep(60);
            }
        }
    }

    // (3) + (4) PSK is present. reg_complete flag determines whether we
    //     send PEER_REQUEST (flag absent/cleared) or run a normal WAKE cycle
    //     (flag set).  Both paths use the same wake cycle engine — the engine
    //     will check reg_complete internally via the storage trait.

    // --- Node is paired — initialize radio and run wake cycle ---
    let hmac = SoftwareHmac;
    let sha = SoftwareSha256;
    let mut rng = EspRng;
    let clock = EspClock;
    let mut hal = EspHal::new();
    let battery = EspBatteryReader;

    // Read the stored WiFi channel (falls back to channel 1 if not yet set).
    let channel = storage.read_channel().unwrap_or(1);

    let mut transport = EspNowTransport::new(peripherals.modem, sysloop, nvs_partition, channel)
        .expect("failed to initialize ESP-NOW transport");

    let mut interpreter = SondeBpfInterpreter::new();

    info!("sonde-node ready");

    let outcome = run_wake_cycle(
        &mut transport,
        &mut storage,
        &mut hal,
        &mut rng,
        &clock,
        &battery,
        &mut interpreter,
        &mut map_storage,
        &hmac,
        &sha,
    );

    match outcome {
        WakeCycleOutcome::Sleep { seconds } => {
            sleep_ctrl.enter_deep_sleep(seconds);
        }
        WakeCycleOutcome::Reboot => {
            info!("rebooting");
            sleep_ctrl.reboot();
        }
        WakeCycleOutcome::Unpaired => {
            // Should not happen — we checked read_key() above.
            // If storage was corrupted mid-cycle, reboot to re-enter pairing.
            info!("unexpected unpaired state — rebooting");
            sleep_ctrl.reboot();
        }
    }
}
