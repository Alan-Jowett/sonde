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
    use esp_idf_hal::peripherals::Peripherals;
    use esp_idf_svc::eventloop::EspSystemEventLoop;
    use esp_idf_svc::log::EspLogger;
    use esp_idf_svc::nvs::EspDefaultNvsPartition;
    use log::info;

    use sonde_node::crypto::{EspRng, SoftwareHmac, SoftwareSha256};
    use sonde_node::esp_hal::{EspBatteryReader, EspClock, EspHal};
    use sonde_node::esp_pairing_serial::EspUsbSerialJtag;
    use sonde_node::esp_sleep::EspSleepController;
    use sonde_node::esp_storage::NvsStorage;
    use sonde_node::esp_transport::EspNowTransport;
    use sonde_node::map_storage::MapStorage;
    use sonde_node::pairing::run_pairing_mode;
    use sonde_node::sonde_bpf_adapter::SondeBpfInterpreter;
    use sonde_node::traits::{PlatformStorage, SleepController};
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

    let mut sleep_ctrl = EspSleepController;

    let mut storage =
        NvsStorage::new(nvs_partition.clone()).expect("failed to initialize NVS storage");

    // Map storage: 4 KB budget (fits in ESP32-C3 RTC SRAM)
    let mut map_storage = MapStorage::new(4096);

    // --- Check pairing status BEFORE initializing the radio ---
    // Per pairing-protocol.md §11: when unpaired, the node enters
    // pairing mode on USB Serial/JTAG without starting ESP-NOW.
    if storage.read_key().is_none() {
        info!("node is unpaired — entering pairing mode");
        match EspUsbSerialJtag::new() {
            Ok(mut usb_serial) => {
                run_pairing_mode(&mut usb_serial, &mut storage, &mut map_storage);
                drop(usb_serial);
                info!("pairing mode exited (USB disconnect) — rebooting");
            }
            Err(e) => {
                info!("failed to initialize USB serial: {:?} — rebooting", e);
            }
        }
        sleep_ctrl.reboot();
    }

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
            info!("entering deep sleep for {} seconds", seconds);
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
