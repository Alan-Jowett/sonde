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
    use esp_idf_hal::peripherals::Peripherals;
    use esp_idf_svc::eventloop::EspSystemEventLoop;
    use esp_idf_svc::log::EspLogger;
    use esp_idf_svc::nvs::EspDefaultNvsPartition;
    use log::info;

    use sonde_node::crypto::{EspRng, SoftwareHmac, SoftwareSha256};
    use sonde_node::esp_hal::{EspBatteryReader, EspClock, EspHal};
    use sonde_node::esp_sleep::EspSleepController;
    use sonde_node::esp_storage::NvsStorage;
    use sonde_node::esp_transport::EspNowTransport;
    use sonde_node::map_storage::MapStorage;
    use sonde_node::rbpf_adapter::RbpfInterpreter;
    use sonde_node::traits::SleepController;
    use sonde_node::wake_cycle::{run_wake_cycle, WakeCycleOutcome};

    // Link ESP-IDF patches and initialize logging.
    esp_idf_svc::sys::link_patches();
    EspLogger::initialize_default();

    info!("sonde-node booting");
    info!("firmware ABI version: {}", sonde_node::FIRMWARE_ABI_VERSION);

    // --- Initialize platform ---
    let peripherals = Peripherals::take().expect("failed to take peripherals");
    let sysloop = EspSystemEventLoop::take().expect("failed to take event loop");
    let nvs_partition = EspDefaultNvsPartition::take().expect("failed to take NVS");

    let hmac = SoftwareHmac;
    let sha = SoftwareSha256;
    let mut rng = EspRng;
    let clock = EspClock;
    let mut hal = EspHal::new();
    let battery = EspBatteryReader;
    let mut sleep_ctrl = EspSleepController;

    let mut storage =
        NvsStorage::new(nvs_partition.clone()).expect("failed to initialize NVS storage");

    let mut transport = EspNowTransport::new(peripherals.modem, sysloop, nvs_partition)
        .expect("failed to initialize ESP-NOW transport");

    let mut interpreter = RbpfInterpreter::new();

    // Map storage: 4 KB budget (fits in ESP32-C3 RTC SRAM)
    let mut map_storage = MapStorage::new(4096);

    info!("sonde-node ready");

    // --- Wake cycle ---
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

    // --- Handle outcome ---
    match outcome {
        WakeCycleOutcome::Sleep { seconds } => {
            info!("entering deep sleep for {} seconds", seconds);
            sleep_ctrl.enter_deep_sleep(seconds);
        }
        WakeCycleOutcome::Reboot => {
            info!("rebooting");
            sleep_ctrl.reboot();
        }
        WakeCycleOutcome::Unpaired => {
            info!("node is unpaired — sleeping indefinitely");
            // Sleep for max duration (unpaired nodes wait for USB pairing)
            sleep_ctrl.enter_deep_sleep(u32::MAX);
        }
    }
}
