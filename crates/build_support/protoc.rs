// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

pub fn explicit_protoc_path() -> Option<std::ffi::OsString> {
    std::env::var_os("PROTOC").filter(|value| !value.is_empty())
}

pub fn has_usable_protoc_on_path() -> bool {
    std::process::Command::new("protoc")
        .arg("--version")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| {
            let version = String::from_utf8_lossy(&output.stdout);
            version
                .split_whitespace()
                .nth(1)
                .and_then(|raw| raw.split('.').next())
                .and_then(|major| major.parse::<u32>().ok())
        })
        .map(|major| major >= 3)
        .unwrap_or(false)
}
