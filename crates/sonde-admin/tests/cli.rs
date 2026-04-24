// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use std::process::Command;

#[test]
fn modem_display_rejects_more_than_four_lines() {
    let output = Command::new(env!("CARGO_BIN_EXE_sonde-admin"))
        .args(["modem", "display", "one", "two", "three", "four", "five"])
        .output()
        .expect("failed to run sonde-admin");

    assert!(
        !output.status.success(),
        "clap should reject more than four display lines"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("five")
            || stderr.contains("too many")
            || stderr.contains("unexpected argument"),
        "stderr should mention the rejected extra argument: {stderr}"
    );
}
