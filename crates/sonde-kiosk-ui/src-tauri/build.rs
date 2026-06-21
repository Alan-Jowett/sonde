// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

fn main() {
    // Android 15+ requires 16KB page-aligned ELF load segments.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("android") {
        println!("cargo:rustc-link-arg=-z");
        println!("cargo:rustc-link-arg=max-page-size=16384");
    }
    tauri_build::build();
}
