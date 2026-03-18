// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Standalone fake GATT peripheral for BLE loopback integration testing.
//!
//! Usage:
//! ```sh
//! cargo run -p sonde-e2e --bin fake_gatt_peripheral
//! # or with a custom bind address:
//! cargo run -p sonde-e2e --bin fake_gatt_peripheral -- --bind 127.0.0.1:19555
//! ```

use sonde_e2e::fake_peripheral::{self, FakePeripheralConfig};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("sonde_e2e=debug,sonde_gateway=debug")
        .init();

    let bind_addr = std::env::args()
        .skip_while(|a| a != "--bind")
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:19555".into());

    let config = FakePeripheralConfig {
        bind_addr,
        ..Default::default()
    };

    let peripheral = fake_peripheral::start(config)
        .await
        .expect("failed to start fake GATT peripheral");

    println!("Fake GATT peripheral listening on {}", peripheral.addr());
    println!("Press Ctrl+C to stop.");

    tokio::signal::ctrl_c()
        .await
        .expect("failed to listen for Ctrl+C");

    peripheral.cancel();
}
