// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! ESP32-S3 radio modem firmware.
//!
//! Bridges USB-CDC serial and ESP-NOW radio, relaying opaque frames
//! with MAC address and RSSI metadata. The modem is protocol-unaware:
//! it does not perform HMAC verification, CBOR parsing, or session management.

mod bridge;
mod espnow;
mod peer_table;
mod status;
mod usb_cdc;

use esp_idf_hal::prelude::Peripherals;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::log::EspLogger;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use log::info;

use crate::bridge::Bridge;
use crate::espnow::EspNowDriver;
use crate::status::ModemCounters;
use crate::usb_cdc::UsbCdcDriver;

fn main() {
    // Link ESP-IDF patches and initialize logging.
    esp_idf_svc::sys::link_patches();
    EspLogger::initialize_default();

    info!("sonde-modem firmware starting");

    let peripherals = Peripherals::take().expect("failed to take peripherals");
    let sysloop = EspSystemEventLoop::take().expect("failed to take event loop");
    let nvs = EspDefaultNvsPartition::take().expect("failed to take NVS partition");

    let counters = ModemCounters::new();
    let usb = UsbCdcDriver::new();
    let espnow = EspNowDriver::new(peripherals.modem, sysloop, nvs, &counters);

    let mut bridge = Bridge::new(usb, espnow, counters);

    // Send MODEM_READY on boot.
    bridge.send_modem_ready();

    info!("entering main loop");

    loop {
        bridge.poll();
        // The ESP-NOW receive callback fires from the WiFi task and
        // enqueues RECV_FRAME messages into the bridge's TX buffer.
        // No explicit polling needed for inbound radio frames.
    }
}
