// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! CLI integration tests for `sonde-bundle`.

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::Path;

fn write_test_bundle_dir(dir: &Path) {
    let manifest = r#"
schema_version: 1
name: "test-app"
version: "0.1.0"
programs:
  - name: "test-prog"
    path: "bpf/test.elf"
    profile: "resident"
nodes:
  - name: "node-1"
    program: "test-prog"
"#;
    std::fs::write(dir.join("app.yaml"), manifest).unwrap();
    std::fs::create_dir_all(dir.join("bpf")).unwrap();
    let mut elf = vec![0x7f, b'E', b'L', b'F'];
    elf.extend_from_slice(&[0u8; 12]);
    std::fs::write(dir.join("bpf").join("test.elf"), &elf).unwrap();
}

#[test]
fn test_cli_create() {
    let src = tempfile::tempdir().unwrap();
    write_test_bundle_dir(src.path());
    let out = tempfile::tempdir().unwrap();
    let bundle_path = out.path().join("test.sondeapp");

    Command::cargo_bin("sonde-bundle")
        .unwrap()
        .args(["create", &src.path().to_string_lossy(), "--output"])
        .arg(&bundle_path)
        .assert()
        .success()
        .stdout(predicate::str::contains("Created"));
    assert!(bundle_path.exists());
}

#[test]
fn test_cli_validate_valid() {
    let src = tempfile::tempdir().unwrap();
    write_test_bundle_dir(src.path());
    let out = tempfile::tempdir().unwrap();
    let bundle_path = out.path().join("test.sondeapp");

    Command::cargo_bin("sonde-bundle")
        .unwrap()
        .args(["create", &src.path().to_string_lossy(), "--output"])
        .arg(&bundle_path)
        .assert()
        .success();

    Command::cargo_bin("sonde-bundle")
        .unwrap()
        .arg("validate")
        .arg(&bundle_path)
        .assert()
        .success()
        .stdout(predicate::str::contains("Bundle is valid"));
}

#[test]
fn test_cli_validate_invalid() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad.sondeapp");
    std::fs::write(&path, "not gzip").unwrap();

    Command::cargo_bin("sonde-bundle")
        .unwrap()
        .arg("validate")
        .arg(&path)
        .assert()
        .failure()
        .stderr(predicate::str::contains("error:"));
}

#[test]
fn test_cli_inspect_text() {
    let src = tempfile::tempdir().unwrap();
    write_test_bundle_dir(src.path());
    let out = tempfile::tempdir().unwrap();
    let bundle_path = out.path().join("test.sondeapp");

    Command::cargo_bin("sonde-bundle")
        .unwrap()
        .args(["create", &src.path().to_string_lossy(), "--output"])
        .arg(&bundle_path)
        .assert()
        .success();

    Command::cargo_bin("sonde-bundle")
        .unwrap()
        .args(["inspect", "--format", "text"])
        .arg(&bundle_path)
        .assert()
        .success()
        .stdout(predicate::str::contains("Bundle: test-app v0.1.0"));
}

#[test]
fn test_cli_inspect_json() {
    let src = tempfile::tempdir().unwrap();
    write_test_bundle_dir(src.path());
    let out = tempfile::tempdir().unwrap();
    let bundle_path = out.path().join("test.sondeapp");

    Command::cargo_bin("sonde-bundle")
        .unwrap()
        .args(["create", &src.path().to_string_lossy(), "--output"])
        .arg(&bundle_path)
        .assert()
        .success();

    Command::cargo_bin("sonde-bundle")
        .unwrap()
        .args(["inspect", "--format", "json"])
        .arg(&bundle_path)
        .assert()
        .success()
        .stdout(predicate::str::contains("\"name\": \"test-app\""));
}

#[test]
fn test_cli_inspect_invalid_format() {
    Command::cargo_bin("sonde-bundle")
        .unwrap()
        .args(["inspect", "--format", "xml", "dummy.sondeapp"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid value"));
}
