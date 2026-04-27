// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use std::process::Command;

#[test]
fn help_lists_top_level_subcommands() {
    let output = Command::new(env!("CARGO_BIN_EXE_sonde-admin"))
        .arg("--help")
        .output()
        .expect("failed to run sonde-admin");

    assert!(output.status.success(), "--help should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    for subcommand in [
        "node",
        "program",
        "schedule",
        "reboot",
        "ephemeral",
        "status",
        "state",
        "modem",
        "pairing",
        "handler",
    ] {
        assert!(
            stdout.contains(subcommand),
            "top-level help should list `{subcommand}`: {stdout}"
        );
    }
}

#[test]
fn node_help_lists_nested_subcommands() {
    let output = Command::new(env!("CARGO_BIN_EXE_sonde-admin"))
        .args(["node", "--help"])
        .output()
        .expect("failed to run sonde-admin");

    assert!(output.status.success(), "node --help should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    for subcommand in ["list", "get", "register", "remove", "factory-reset"] {
        assert!(
            stdout.contains(subcommand),
            "node help should list `{subcommand}`: {stdout}"
        );
    }
}

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
