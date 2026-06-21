// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    sonde_kiosk_ui_backend::run();
}
