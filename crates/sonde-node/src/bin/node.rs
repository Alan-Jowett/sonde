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

#[cfg(any(feature = "esp", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BootMode {
    PreProvisioningTest,
    BlePairing,
    WakeCycle,
}

#[cfg(any(feature = "esp", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BootResetReason {
    DeepSleepWake,
    SoftwareReset,
    PowerOn,
    Brownout,
    Other,
}

#[cfg(any(feature = "esp", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BootAction {
    PreProvisioningTest,
    BlePairing,
    WakeCycle,
    BrownoutRecoverySleep { seconds: u32 },
}

#[cfg(any(feature = "esp", test))]
fn select_boot_mode(has_staged_test: bool, has_psk: bool) -> BootMode {
    if has_staged_test && !has_psk {
        BootMode::PreProvisioningTest
    } else if !has_psk {
        BootMode::BlePairing
    } else {
        BootMode::WakeCycle
    }
}

#[cfg(any(feature = "esp", test))]
fn boot_reason_label(reset_reason: BootResetReason) -> &'static str {
    if reset_reason == BootResetReason::DeepSleepWake {
        "deep_sleep_wake"
    } else if reset_reason == BootResetReason::SoftwareReset {
        "software_reset"
    } else if reset_reason == BootResetReason::PowerOn {
        "power_on"
    } else if reset_reason == BootResetReason::Brownout {
        "brownout"
    } else {
        "other_reset"
    }
}

#[cfg(feature = "esp")]
fn boot_reset_reason_from_esp(
    reset_reason: esp_idf_svc::sys::esp_reset_reason_t,
) -> BootResetReason {
    if reset_reason == esp_idf_svc::sys::esp_reset_reason_t_ESP_RST_DEEPSLEEP {
        BootResetReason::DeepSleepWake
    } else if reset_reason == esp_idf_svc::sys::esp_reset_reason_t_ESP_RST_SW {
        BootResetReason::SoftwareReset
    } else if reset_reason == esp_idf_svc::sys::esp_reset_reason_t_ESP_RST_POWERON {
        BootResetReason::PowerOn
    } else if reset_reason == esp_idf_svc::sys::esp_reset_reason_t_ESP_RST_BROWNOUT {
        BootResetReason::Brownout
    } else {
        BootResetReason::Other
    }
}

#[cfg(any(feature = "esp", test))]
fn should_enter_brownout_recovery(reset_reason: BootResetReason, boot_mode: BootMode) -> bool {
    reset_reason == BootResetReason::Brownout && boot_mode == BootMode::WakeCycle
}

#[cfg(any(feature = "esp", test))]
fn brownout_recovery_sleep_s(base_interval_s: u32) -> u32 {
    use sonde_node::sleep::{SleepManager, WakeReason};

    SleepManager::new(base_interval_s, WakeReason::Scheduled).effective_sleep_s()
}

#[cfg(any(feature = "esp", test))]
fn brownout_recovery_sleep_s_from_storage<S: sonde_node::traits::PlatformStorage>(
    storage: &mut S,
) -> u32 {
    let _ = storage.take_early_wake_flag();
    let (base_interval_s, _active_partition) = storage.read_schedule();
    brownout_recovery_sleep_s(base_interval_s)
}

#[cfg(any(feature = "esp", test))]
fn select_boot_action<S: sonde_node::traits::PlatformStorage>(
    reset_reason: BootResetReason,
    has_staged_test: bool,
    has_psk: bool,
    storage: &mut S,
) -> BootAction {
    let boot_mode = select_boot_mode(has_staged_test, has_psk);
    if should_enter_brownout_recovery(reset_reason, boot_mode) {
        BootAction::BrownoutRecoverySleep {
            seconds: brownout_recovery_sleep_s_from_storage(storage),
        }
    } else {
        match boot_mode {
            BootMode::PreProvisioningTest => BootAction::PreProvisioningTest,
            BootMode::BlePairing => BootAction::BlePairing,
            BootMode::WakeCycle => BootAction::WakeCycle,
        }
    }
}

#[cfg(any(feature = "esp", test))]
fn log_boot_reason(reset_reason: BootResetReason) {
    log::info!("boot_reason={} (ND-1000)", boot_reason_label(reset_reason));
}

#[cfg(any(feature = "esp", test))]
fn log_brownout_recovery_sleep(seconds: u32) {
    log::info!(
        "entering deep sleep duration_seconds={} reason=brownout_recovery (ND-1007)",
        seconds,
    );
}

#[cfg(feature = "esp")]
fn main() {
    use esp_idf_hal::gpio::{PinDriver, Pull};
    use esp_idf_hal::peripherals::Peripherals;
    use esp_idf_svc::eventloop::EspSystemEventLoop;
    use esp_idf_svc::log::EspLogger;
    use esp_idf_svc::nvs::EspDefaultNvsPartition;
    use log::{info, warn};

    use sonde_node::ble_pairing::execute_staged_test_command;
    use sonde_node::board_layout::stage_runtime_board_layout;
    use sonde_node::crypto::{EspRng, SoftwareSha256};
    use sonde_node::esp_ble_pairing::run_ble_pairing_mode;
    use sonde_node::esp_hal::{EspClock, EspHal};
    use sonde_node::esp_sleep::EspSleepController;
    use sonde_node::esp_storage::NvsStorage;
    use sonde_node::esp_transport::EspNowTransport;
    use sonde_node::hal::Hal;
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

    warn!("sonde-node booting (commit {})", env!("SONDE_GIT_COMMIT"));
    warn!("firmware ABI version: {}", sonde_node::FIRMWARE_ABI_VERSION);

    // Log boot reason (ND-1000).
    let reset_reason = boot_reset_reason_from_esp(unsafe { esp_idf_svc::sys::esp_reset_reason() });
    log_boot_reason(reset_reason);

    // Register the main task with the ESP-IDF task watchdog (ND-0919).
    // The watchdog timeout (CONFIG_ESP_TASK_WDT_TIMEOUT_S=20) covers the
    // entire wake cycle. No periodic feeding is needed because the node
    // runs a single wake cycle and then sleeps; if the cycle completes
    // normally, we deregister before sleeping. If it hangs, the watchdog
    // triggers a panic/reset.
    unsafe {
        let wdt_config = esp_idf_svc::sys::esp_task_wdt_config_t {
            timeout_ms: 20_000,
            idle_core_mask: 0,
            trigger_panic: true,
        };
        esp_idf_svc::sys::esp!(esp_idf_svc::sys::esp_task_wdt_reconfigure(&wdt_config))
            .expect("failed to configure watchdog");
        esp_idf_svc::sys::esp!(esp_idf_svc::sys::esp_task_wdt_add(
            esp_idf_svc::sys::xTaskGetCurrentTaskHandle()
        ))
        .expect("failed to add task to watchdog");
    }
    info!("task watchdog registered (20 s timeout, ND-0919)");

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
    //   1. Pairing button held ≥ 500 ms AND PSK present → factory reset + BLE
    //   2. Staged pre-provisioning test command (unpaired only) → test mode
    //   3. No PSK → BLE pairing mode
    //   4. Brownout on a paired boot → recovery sleep without ESP-NOW activity
    //   5. PSK stored, reg_complete NOT set → PEER_REQUEST mode (WAKE cycle variant)
    //   6. PSK stored, reg_complete set → normal WAKE cycle
    // ---------------------------------------------------------------------------

    // Pairing button is GPIO 9 on the ESP32-C3 DevKitM-1 (active LOW).
    // We sample it for 500 ms immediately after boot.  If the pin is
    // held LOW for the entire sampling window, button_held = true, which
    // triggers an immediate factory reset (ND-0917).
    let button_held = {
        // GPIO 9 is the BOOT button on most ESP32-C3 boards.
        // Configure as input with internal pull-up (active LOW).
        let button_pin = peripherals.pins.gpio9;
        let button = PinDriver::input(button_pin, Pull::Up)
            .expect("failed to configure pairing button GPIO");

        const SAMPLE_INTERVAL_MS: u32 = 10;
        const SAMPLE_COUNT: u32 = 500 / SAMPLE_INTERVAL_MS; // 50 samples over 500 ms
        let mut held = true;
        for _ in 0..SAMPLE_COUNT {
            if button.is_high() {
                held = false;
                break; // not pressed — no point sampling further
            }
            // Busy-wait 10 ms between samples
            unsafe {
                esp_idf_svc::sys::vTaskDelay(
                    (SAMPLE_INTERVAL_MS * esp_idf_svc::sys::CONFIG_FREERTOS_HZ) / 1000,
                );
            }
        }
        held
    };

    let mut has_psk = storage.read_key().is_some();

    // (1) Boot button held on a paired node → immediate factory reset (ND-0917).
    // Erases PSK, programs, maps, schedule, channel, BLE artifacts, and
    // staged test state. After reset, the node is unpaired and enters BLE
    // pairing mode. If the reset fails, enter deep sleep (fail-closed).
    if button_held && has_psk {
        warn!("pairing button held ≥ 500 ms on paired node — performing factory reset (ND-0917)");
        let mut ks = sonde_node::key_store::KeyStore::new(&mut storage);
        match ks.factory_reset(&mut map_storage) {
            Ok(()) => {
                warn!("factory reset complete — node is now unpaired");
                has_psk = false;
            }
            Err(e) => {
                warn!(
                    "factory reset failed: {} — entering deep sleep (fail-closed)",
                    e
                );
                sleep_ctrl.enter_deep_sleep(60);
            }
        }
    } else if button_held {
        info!("pairing button held ≥ 500 ms on unpaired node — no reset needed");
    }

    let mut has_staged_test = storage.read_staged_test_command().is_some();
    if has_staged_test && has_psk {
        warn!("ignoring stale staged pre-provisioning test command on paired node");
        if let Err(err) = storage.clear_staged_test_command() {
            warn!(
                "failed to clear stale staged pre-provisioning test command: {}",
                err
            );
        } else {
            has_staged_test = false;
        }
    }

    match select_boot_action(reset_reason, has_staged_test, has_psk, &mut storage) {
        BootAction::BrownoutRecoverySleep { seconds } => {
            warn!(
                "brownout reset detected — skipping ESP-NOW activity for recovery sleep (ND-0900a)"
            );
            log_brownout_recovery_sleep(seconds);
            sleep_ctrl.enter_deep_sleep(seconds);
        }
        BootAction::PreProvisioningTest => {
            let staged_command = storage
                .read_staged_test_command()
                .expect("boot mode selected pre-provisioning test without staged command");
            info!(
                "entering pre-provisioning test mode (test_type={}, rf_channel={:?})",
                staged_command.test_type, staged_command.rf_channel
            );

            let test_channel = staged_command
                .rf_channel
                .or_else(|| storage.read_channel())
                .unwrap_or(1);
            let clock = EspClock;

            match EspNowTransport::new(peripherals.modem, sysloop, nvs_partition, test_channel) {
                Ok(mut transport) => {
                    match execute_staged_test_command(&mut storage, &mut transport, &clock) {
                        Ok(Some(result)) => {
                            info!(
                                "pre-provisioning test completed: status=0x{:02x} attempts={} elapsed_ms={}",
                                result.status, result.attempt_count, result.elapsed_ms
                            );
                        }
                        Ok(None) => {
                            warn!("pre-provisioning test mode entered without a staged command");
                        }
                        Err(err) => {
                            warn!("pre-provisioning test execution failed: {}", err);
                        }
                    }
                }
                Err(err) => {
                    warn!(
                        "failed to initialize ESP-NOW for pre-provisioning test mode: {}",
                        err
                    );
                    let _ = storage.write_test_result(&sonde_protocol::TestResult {
                        status: sonde_protocol::TEST_RESULT_EXECUTION_ERROR,
                        test_type: Some(staged_command.test_type),
                        reply_frame: None,
                        reply_rssi_dbm: None,
                        attempt_count: 0,
                        elapsed_ms: 0,
                    });
                    let _ = storage.clear_staged_test_command();
                }
            }

            info!("pre-provisioning test mode finished — rebooting to BLE pairing mode");
            sleep_ctrl.reboot();
        }
        BootAction::BlePairing => {
            info!("entering BLE pairing mode (no PSK={})", !has_psk);

            let pairing_board_layout = storage
                .read_board_layout()
                .unwrap_or(sonde_protocol::BoardLayout::SONDE_SENSOR_NODE_REV_A);
            let mut pairing_hal = EspHal::new(pairing_board_layout);
            pairing_hal.enter_active_gpio_state();

            match run_ble_pairing_mode(&mut storage) {
                Ok(()) => {
                    info!("BLE pairing mode exited — rebooting");
                    pairing_hal.prepare_for_sleep();
                    sleep_ctrl.reboot();
                }
                Err(e) => {
                    // BLE pairing mode failed to initialize or run. Enter deep
                    // sleep to conserve battery until the operator retries.
                    warn!("BLE pairing mode failed: {} — entering deep sleep", e);
                    pairing_hal.prepare_for_sleep();
                    sleep_ctrl.enter_deep_sleep(60);
                }
            }
        }
        BootAction::WakeCycle => {}
    }

    // (3) + (4) PSK is present. reg_complete flag determines whether we
    //     send PEER_REQUEST (flag absent/cleared) or run a normal WAKE cycle
    //     (flag set).  Both paths use the same wake cycle engine — the engine
    //     will check reg_complete internally via the storage trait.

    // --- Node is paired — initialize radio and run wake cycle ---
    let sha = SoftwareSha256;
    let aead = sonde_node::node_aead::NodeAead;
    let mut rng = EspRng;
    let clock = EspClock;
    let board_layout = storage
        .read_board_layout()
        .unwrap_or(sonde_protocol::BoardLayout::LEGACY_COMPAT);
    stage_runtime_board_layout(&board_layout);
    let mut hal = EspHal::new(board_layout);

    // Read the stored WiFi channel (falls back to channel 1 if not yet set).
    let channel = storage.read_channel().unwrap_or(1);

    warn!("ESP-NOW channel={} (ND-1016)", channel);

    let mut transport = EspNowTransport::new(peripherals.modem, sysloop, nvs_partition, channel)
        .expect("failed to initialize ESP-NOW transport");

    let mut interpreter = SondeBpfInterpreter::new();
    let mut async_queue = sonde_node::async_queue::AsyncQueue::from_rtc();

    info!("sonde-node ready");

    let outcome = run_wake_cycle(
        &mut transport,
        &mut storage,
        &mut hal,
        &mut rng,
        &clock,
        &board_layout,
        &mut interpreter,
        &mut map_storage,
        &sha,
        &aead,
        &mut async_queue,
    );

    // Deregister from the task watchdog before sleeping (ND-0919).
    // The wake cycle completed normally — no need for watchdog protection
    // during the sleep/reboot path.
    unsafe {
        let _ =
            esp_idf_svc::sys::esp_task_wdt_delete(esp_idf_svc::sys::xTaskGetCurrentTaskHandle());
    }

    match outcome {
        WakeCycleOutcome::Sleep { seconds } => {
            hal.prepare_for_sleep();
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

#[cfg(test)]
mod tests {
    use super::{
        boot_reason_label, brownout_recovery_sleep_s, log_boot_reason, log_brownout_recovery_sleep,
        select_boot_action, select_boot_mode, should_enter_brownout_recovery, BootAction, BootMode,
        BootResetReason,
    };
    #[cfg(debug_assertions)]
    use log::{Level, Log, Metadata, Record};
    use sonde_node::error::NodeResult;
    use sonde_node::traits::PlatformStorage;
    #[cfg(debug_assertions)]
    use std::sync::{Mutex, Once};

    #[cfg(debug_assertions)]
    struct TestLogger;

    #[cfg(debug_assertions)]
    static TEST_LOG_RECORDS: Mutex<Vec<(Level, String)>> = Mutex::new(Vec::new());

    #[cfg(debug_assertions)]
    impl Log for TestLogger {
        fn enabled(&self, _metadata: &Metadata) -> bool {
            true
        }

        fn log(&self, record: &Record) {
            TEST_LOG_RECORDS
                .lock()
                .unwrap()
                .push((record.level(), format!("{}", record.args())));
        }

        fn flush(&self) {}
    }

    #[cfg(debug_assertions)]
    static TEST_LOGGER: TestLogger = TestLogger;

    #[cfg(debug_assertions)]
    fn init_test_logger() {
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            let _ = log::set_logger(&TEST_LOGGER);
            log::set_max_level(log::LevelFilter::Trace);
        });
    }

    #[cfg(debug_assertions)]
    fn drain_log_records() -> Vec<(Level, String)> {
        std::mem::take(&mut *TEST_LOG_RECORDS.lock().unwrap())
    }

    #[derive(Default)]
    struct TestStorage {
        base_interval_s: u32,
        active_partition: u8,
        early_wake_flag: bool,
    }

    impl PlatformStorage for TestStorage {
        fn read_key(&self) -> Option<(u16, [u8; 32])> {
            None
        }

        fn write_key(&mut self, _key_hint: u16, _psk: &[u8; 32]) -> NodeResult<()> {
            Ok(())
        }

        fn erase_key(&mut self) -> NodeResult<()> {
            Ok(())
        }

        fn read_schedule(&self) -> (u32, u8) {
            (self.base_interval_s, self.active_partition)
        }

        fn write_schedule_interval(&mut self, interval_s: u32) -> NodeResult<()> {
            self.base_interval_s = interval_s;
            Ok(())
        }

        fn write_active_partition(&mut self, partition: u8) -> NodeResult<()> {
            self.active_partition = partition;
            Ok(())
        }

        fn reset_schedule(&mut self) -> NodeResult<()> {
            self.base_interval_s = 0;
            self.active_partition = 0;
            Ok(())
        }

        fn read_program(&self, _partition: u8) -> Option<Vec<u8>> {
            None
        }

        fn write_program(&mut self, _partition: u8, _image: &[u8]) -> NodeResult<()> {
            Ok(())
        }

        fn erase_program(&mut self, _partition: u8) -> NodeResult<()> {
            Ok(())
        }

        fn take_early_wake_flag(&mut self) -> bool {
            let flag = self.early_wake_flag;
            self.early_wake_flag = false;
            flag
        }

        fn set_early_wake_flag(&mut self) -> NodeResult<()> {
            self.early_wake_flag = true;
            Ok(())
        }
    }

    #[test]
    fn staged_test_takes_priority_when_unpaired() {
        // Staged test command + unpaired → pre-provisioning test mode
        assert_eq!(select_boot_mode(true, false), BootMode::PreProvisioningTest);
    }

    #[test]
    fn staged_test_ignored_when_paired() {
        // Staged test command + paired → WakeCycle (stale test is cleared
        // in the boot path before select_boot_mode is called)
        assert_eq!(select_boot_mode(true, true), BootMode::WakeCycle);
    }

    #[test]
    fn ble_pairing_selected_when_unpaired() {
        // No PSK, no staged test → BLE pairing
        assert_eq!(select_boot_mode(false, false), BootMode::BlePairing);
    }

    #[test]
    fn wake_cycle_selected_for_paired_node() {
        // PSK present, no staged test → normal WAKE cycle
        assert_eq!(select_boot_mode(false, true), BootMode::WakeCycle);
    }

    #[test]
    fn boot_reason_logs_brownout_label() {
        assert_eq!(boot_reason_label(BootResetReason::Brownout), "brownout");
        assert_eq!(boot_reason_label(BootResetReason::Other), "other_reset");
    }

    #[test]
    fn brownout_recovery_only_applies_to_paired_wake_cycle_boots() {
        assert!(should_enter_brownout_recovery(
            BootResetReason::Brownout,
            BootMode::WakeCycle,
        ));
        assert!(!should_enter_brownout_recovery(
            BootResetReason::Brownout,
            BootMode::BlePairing,
        ));
        assert!(!should_enter_brownout_recovery(
            BootResetReason::Brownout,
            BootMode::PreProvisioningTest,
        ));
        assert!(!should_enter_brownout_recovery(
            BootResetReason::PowerOn,
            BootMode::WakeCycle,
        ));
    }

    #[test]
    fn brownout_recovery_uses_base_interval() {
        assert_eq!(brownout_recovery_sleep_s(300), 300);
    }

    #[test]
    fn brownout_recovery_clamps_to_minimum_sleep_interval() {
        assert_eq!(brownout_recovery_sleep_s(0), 1);
    }

    #[test]
    fn brownout_boot_action_skips_paired_wake_cycle_boots_and_uses_base_interval() {
        let mut storage = TestStorage {
            base_interval_s: 300,
            active_partition: 1,
            early_wake_flag: true,
        };

        assert_eq!(
            select_boot_action(BootResetReason::Brownout, false, true, &mut storage),
            BootAction::BrownoutRecoverySleep { seconds: 300 }
        );
        assert!(!storage.early_wake_flag);
    }

    #[test]
    fn brownout_boot_action_clamps_to_minimum_sleep_interval() {
        let mut storage = TestStorage {
            base_interval_s: 0,
            active_partition: 1,
            early_wake_flag: true,
        };

        assert_eq!(
            select_boot_action(BootResetReason::Brownout, false, true, &mut storage),
            BootAction::BrownoutRecoverySleep { seconds: 1 }
        );
        assert!(!storage.early_wake_flag);
    }

    #[cfg(debug_assertions)]
    #[test]
    fn brownout_boot_reason_log_is_emitted() {
        init_test_logger();
        drain_log_records();

        log_boot_reason(BootResetReason::Brownout);

        let records = drain_log_records();
        assert!(records.iter().any(|(level, msg)| {
            *level == log::Level::Info && msg.contains("boot_reason=brownout")
        }));
    }

    #[cfg(debug_assertions)]
    #[test]
    fn brownout_recovery_sleep_log_is_emitted() {
        init_test_logger();
        drain_log_records();

        log_brownout_recovery_sleep(300);

        let records = drain_log_records();
        assert!(records.iter().any(|(level, msg)| {
            *level == log::Level::Info
                && msg.contains("entering deep sleep")
                && msg.contains("duration_seconds=300")
                && msg.contains("reason=brownout_recovery")
        }));
    }

    // Note: button_held + PSK → factory reset is handled in the boot path
    // before select_boot_mode is called (has_psk becomes false after reset).
    // The select_boot_mode function no longer knows about button state.
}
