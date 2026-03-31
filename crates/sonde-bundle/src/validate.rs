// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Validation logic for Sonde App Bundles.

use crate::manifest::{Manifest, SensorType, VerificationProfile};
use std::collections::HashSet;
use std::io::Read;
use std::path::{Component, Path};

/// Result of validating a bundle.
#[derive(Debug)]
pub struct ValidationResult {
    pub errors: Vec<ValidationError>,
    pub warnings: Vec<ValidationWarning>,
}

impl ValidationResult {
    /// Returns true if the bundle is valid (no errors).
    pub fn is_valid(&self) -> bool {
        self.errors.is_empty()
    }

    fn new() -> Self {
        Self {
            errors: Vec::new(),
            warnings: Vec::new(),
        }
    }
}

/// A validation error (bundle is invalid).
#[derive(Debug)]
pub struct ValidationError {
    pub rule: &'static str,
    pub message: String,
}

/// A validation warning (bundle is valid but has concerns).
#[derive(Debug)]
pub struct ValidationWarning {
    pub rule: &'static str,
    pub message: String,
}

/// Validate a name against the app name regex: `[a-z0-9]([a-z0-9-]*[a-z0-9])?`
fn is_valid_app_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 64 {
        return false;
    }
    let chars: Vec<char> = name.chars().collect();
    // First char: [a-z0-9]
    if !chars[0].is_ascii_lowercase() && !chars[0].is_ascii_digit() {
        return false;
    }
    if chars.len() == 1 {
        return true;
    }
    // Last char: [a-z0-9]
    if !chars[chars.len() - 1].is_ascii_lowercase() && !chars[chars.len() - 1].is_ascii_digit() {
        return false;
    }
    // Middle chars: [a-z0-9-]
    for &c in &chars[1..chars.len() - 1] {
        if !c.is_ascii_lowercase() && !c.is_ascii_digit() && c != '-' {
            return false;
        }
    }
    true
}

/// Validate a name against the program name regex: `[a-z0-9]([a-z0-9_-]*[a-z0-9])?`
fn is_valid_program_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 64 {
        return false;
    }
    let chars: Vec<char> = name.chars().collect();
    if !chars[0].is_ascii_lowercase() && !chars[0].is_ascii_digit() {
        return false;
    }
    if chars.len() == 1 {
        return true;
    }
    if !chars[chars.len() - 1].is_ascii_lowercase() && !chars[chars.len() - 1].is_ascii_digit() {
        return false;
    }
    for &c in &chars[1..chars.len() - 1] {
        if !c.is_ascii_lowercase() && !c.is_ascii_digit() && c != '-' && c != '_' {
            return false;
        }
    }
    true
}

/// Check if a path is safe (relative, no parent-dir or prefix components).
pub(crate) fn is_path_unsafe(path: &Path) -> bool {
    if path.is_absolute() {
        return true;
    }
    for component in path.components() {
        match component {
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return true,
            _ => {}
        }
    }
    let raw = path.as_os_str().as_encoded_bytes();
    if !raw.is_empty() && (raw[0] == b'/' || raw[0] == b'\\') {
        return true;
    }
    // Reject Windows drive letters (e.g., "C:\foo") on any platform
    if raw.len() >= 2 && raw[1] == b':' && raw[0].is_ascii_alphabetic() {
        return true;
    }
    false
}

fn has_path_traversal(path: &str) -> bool {
    is_path_unsafe(Path::new(path))
}

/// Validate a manifest against a source directory.
pub fn validate_manifest(manifest: &Manifest, source_dir: &Path) -> ValidationResult {
    let mut result = ValidationResult::new();

    // §6.2 Manifest validation
    if manifest.schema_version == 0 {
        result.errors.push(ValidationError {
            rule: "schema_version",
            message: "schema_version must be >= 1".to_string(),
        });
    } else if manifest.schema_version > 1 {
        result.errors.push(ValidationError {
            rule: "schema_version",
            message: format!(
                "unsupported schema version: {} (maximum supported: 1)",
                manifest.schema_version
            ),
        });
    }

    if !is_valid_app_name(&manifest.name) {
        result.errors.push(ValidationError {
            rule: "name",
            message: format!(
                "name must match pattern [a-z0-9]([a-z0-9-]*[a-z0-9])? and be 1-64 characters, got: `{}`",
                manifest.name
            ),
        });
    }

    if semver::Version::parse(&manifest.version).is_err() {
        result.errors.push(ValidationError {
            rule: "version",
            message: format!("version must be valid semver, got: `{}`", manifest.version),
        });
    }

    if manifest.programs.is_empty() {
        result.errors.push(ValidationError {
            rule: "programs",
            message: "programs must not be empty".to_string(),
        });
    }

    if manifest.nodes.is_empty() {
        result.errors.push(ValidationError {
            rule: "nodes",
            message: "nodes must not be empty".to_string(),
        });
    }

    if let Some(ref desc) = manifest.description {
        if desc.chars().count() > 256 {
            result.errors.push(ValidationError {
                rule: "description",
                message: "description must not exceed 256 characters".to_string(),
            });
        }
    }

    // §6.3 Program validation
    let mut program_names = HashSet::new();
    for prog in &manifest.programs {
        if !program_names.insert(&prog.name) {
            result.errors.push(ValidationError {
                rule: "program.name",
                message: format!("duplicate program name: `{}`", prog.name),
            });
        }

        if !is_valid_program_name(&prog.name) {
            result.errors.push(ValidationError {
                rule: "program.name",
                message: format!(
                    "program name must match pattern [a-z0-9]([a-z0-9_-]*[a-z0-9])?, got: `{}`",
                    prog.name
                ),
            });
        }

        // Validate verification profile
        if let VerificationProfile::Unknown(ref s) = prog.profile {
            result.errors.push(ValidationError {
                rule: "program.profile",
                message: format!(
                    "unknown verification profile `{}`, expected `resident` or `ephemeral`",
                    s
                ),
            });
        }

        // Check path safety
        if prog.path.is_empty() {
            result.errors.push(ValidationError {
                rule: "program.path",
                message: format!("program `{}` path must not be empty", prog.name),
            });
        } else if has_path_traversal(&prog.path) {
            result.errors.push(ValidationError {
                rule: "program.path",
                message: format!("program path must be relative with no ..: `{}`", prog.path),
            });
        } else {
            // Check file exists
            let file_path = source_dir.join(&prog.path);
            if !file_path.exists() {
                result.errors.push(ValidationError {
                    rule: "program.path",
                    message: format!("program file not found: `{}`", prog.path),
                });
            } else {
                // Check ELF magic (read only the first 4 bytes)
                match std::fs::File::open(&file_path) {
                    Ok(mut f) => {
                        let mut magic = [0u8; 4];
                        match f.read_exact(&mut magic) {
                            Ok(()) => {
                                if &magic != b"\x7fELF" {
                                    result.errors.push(ValidationError {
                                        rule: "program.elf",
                                        message: format!("invalid ELF file: `{}`", prog.path),
                                    });
                                }
                            }
                            Err(_) => {
                                result.errors.push(ValidationError {
                                    rule: "program.elf",
                                    message: format!(
                                        "invalid ELF file (too small): `{}`",
                                        prog.path
                                    ),
                                });
                            }
                        }
                    }
                    Err(e) => {
                        result.errors.push(ValidationError {
                            rule: "program.path",
                            message: format!("cannot read program file `{}`: {}", prog.path, e),
                        });
                    }
                }
            }
        }
    }

    // §6.4 Handler validation
    let mut catch_all_count = 0;
    for handler in &manifest.handlers {
        if handler.program == "*" {
            catch_all_count += 1;
            if catch_all_count > 1 {
                result.errors.push(ValidationError {
                    rule: "handler.program",
                    message: "duplicate catch-all handler".to_string(),
                });
            }
        } else if !program_names.contains(&handler.program) {
            result.errors.push(ValidationError {
                rule: "handler.program",
                message: format!("handler references unknown program: `{}`", handler.program),
            });
        }

        if handler.command.is_empty() {
            result.errors.push(ValidationError {
                rule: "handler.command",
                message: "handler command must not be empty".to_string(),
            });
        }

        if let Some(timeout) = handler.reply_timeout_ms {
            if timeout == 0 {
                result.errors.push(ValidationError {
                    rule: "handler.reply_timeout_ms",
                    message: "reply_timeout_ms must be a positive integer".to_string(),
                });
            }
        }

        if let Some(ref wd) = handler.working_dir {
            if has_path_traversal(wd) {
                result.errors.push(ValidationError {
                    rule: "handler.working_dir",
                    message: format!(
                        "working_dir must be relative with no path traversal: `{}`",
                        wd
                    ),
                });
            } else {
                let wd_path = source_dir.join(wd);
                if !wd_path.exists() {
                    result.errors.push(ValidationError {
                        rule: "handler.working_dir",
                        message: format!("working directory not found: `{}`", wd),
                    });
                } else if !wd_path.is_dir() {
                    result.errors.push(ValidationError {
                        rule: "handler.working_dir",
                        message: format!("working_dir must be a directory: `{}`", wd),
                    });
                }
            }
        }
    }

    // §6.5 Node validation
    let mut node_names = HashSet::new();
    let mut referenced_programs = HashSet::new();
    for node in &manifest.nodes {
        if !node_names.insert(&node.name) {
            result.errors.push(ValidationError {
                rule: "node.name",
                message: format!("duplicate node name: `{}`", node.name),
            });
        }

        if node.name.is_empty() {
            result.errors.push(ValidationError {
                rule: "node.name",
                message: "node name must not be empty".to_string(),
            });
        }

        if !program_names.contains(&node.program) {
            result.errors.push(ValidationError {
                rule: "node.program",
                message: format!(
                    "node `{}` references unknown program: `{}`",
                    node.name, node.program
                ),
            });
        }
        referenced_programs.insert(&node.program);

        if let Some(ref hw) = node.hardware {
            if let Some(ch) = hw.rf_channel {
                if !(1..=13).contains(&ch) {
                    result.errors.push(ValidationError {
                        rule: "node.hardware.rf_channel",
                        message: format!("rf_channel must be between 1 and 13, got: {}", ch),
                    });
                }
            }
            for sensor in &hw.sensors {
                if let SensorType::Unknown(ref s) = sensor.sensor_type {
                    result.errors.push(ValidationError {
                        rule: "node.hardware.sensors.type",
                        message: format!(
                            "unknown sensor type `{}`, expected one of: i2c, adc, gpio, spi",
                            s
                        ),
                    });
                }
                if let Some(ref label) = sensor.label {
                    if label.len() > 64 {
                        result.errors.push(ValidationError {
                            rule: "node.hardware.sensors.label",
                            message: "sensor label must not exceed 64 bytes UTF-8".to_string(),
                        });
                    }
                }
            }
        }
    }

    // §6.6 Cross-reference validation (warnings)
    for prog in &manifest.programs {
        if !referenced_programs.contains(&prog.name) {
            result.warnings.push(ValidationWarning {
                rule: "cross-reference",
                message: format!("program `{}` is not referenced by any node", prog.name),
            });
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::*;

    fn minimal_manifest() -> Manifest {
        Manifest {
            schema_version: 1,
            name: "test-app".to_string(),
            version: "0.1.0".to_string(),
            description: None,
            programs: vec![ProgramEntry {
                name: "test-prog".to_string(),
                path: "bpf/test.elf".to_string(),
                profile: VerificationProfile::Resident,
            }],
            nodes: vec![NodeTarget {
                name: "node-1".to_string(),
                program: "test-prog".to_string(),
                hardware: None,
            }],
            handlers: vec![],
        }
    }

    fn setup_test_dir(dir: &Path) {
        let bpf_dir = dir.join("bpf");
        std::fs::create_dir_all(&bpf_dir).unwrap();
        // Minimal ELF: magic + padding
        let mut elf = vec![0x7f, b'E', b'L', b'F'];
        elf.extend_from_slice(&[0u8; 12]);
        std::fs::write(bpf_dir.join("test.elf"), &elf).unwrap();
    }

    #[test]
    fn test_valid_manifest() {
        let dir = tempfile::tempdir().unwrap();
        setup_test_dir(dir.path());
        let m = minimal_manifest();
        let r = validate_manifest(&m, dir.path());
        assert!(r.is_valid(), "expected valid: {:?}", r.errors);
    }

    #[test]
    fn test_unsupported_schema_version() {
        let dir = tempfile::tempdir().unwrap();
        setup_test_dir(dir.path());
        let mut m = minimal_manifest();
        m.schema_version = 2;
        let r = validate_manifest(&m, dir.path());
        assert!(!r.is_valid());
        assert!(r
            .errors
            .iter()
            .any(|e| e.message.contains("unsupported schema version")));
    }

    #[test]
    fn test_schema_version_zero() {
        let dir = tempfile::tempdir().unwrap();
        setup_test_dir(dir.path());
        let mut m = minimal_manifest();
        m.schema_version = 0;
        let r = validate_manifest(&m, dir.path());
        assert!(!r.is_valid());
        assert!(r.errors.iter().any(|e| e.message.contains(">= 1")));
    }

    #[test]
    fn test_invalid_app_name_uppercase() {
        let dir = tempfile::tempdir().unwrap();
        setup_test_dir(dir.path());
        let mut m = minimal_manifest();
        m.name = "MyApp".to_string();
        let r = validate_manifest(&m, dir.path());
        assert!(!r.is_valid());
        assert!(r.errors.iter().any(|e| e.rule == "name"));
    }

    #[test]
    fn test_invalid_app_name_leading_hyphen() {
        let dir = tempfile::tempdir().unwrap();
        setup_test_dir(dir.path());
        let mut m = minimal_manifest();
        m.name = "-my-app".to_string();
        let r = validate_manifest(&m, dir.path());
        assert!(!r.is_valid());
        assert!(r.errors.iter().any(|e| e.rule == "name"));
    }

    #[test]
    fn test_single_char_app_name() {
        let dir = tempfile::tempdir().unwrap();
        setup_test_dir(dir.path());
        let mut m = minimal_manifest();
        m.name = "a".to_string();
        let r = validate_manifest(&m, dir.path());
        assert!(
            r.is_valid(),
            "single-char name should be valid: {:?}",
            r.errors
        );
    }

    #[test]
    fn test_invalid_semver() {
        let dir = tempfile::tempdir().unwrap();
        setup_test_dir(dir.path());
        let mut m = minimal_manifest();
        m.version = "1.0".to_string();
        let r = validate_manifest(&m, dir.path());
        assert!(!r.is_valid());
        assert!(r.errors.iter().any(|e| e.message.contains("semver")));
    }

    #[test]
    fn test_empty_programs() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = minimal_manifest();
        m.programs.clear();
        let r = validate_manifest(&m, dir.path());
        assert!(!r.is_valid());
        assert!(r
            .errors
            .iter()
            .any(|e| e.message.contains("programs must not be empty")));
    }

    #[test]
    fn test_empty_nodes() {
        let dir = tempfile::tempdir().unwrap();
        setup_test_dir(dir.path());
        let mut m = minimal_manifest();
        m.nodes.clear();
        let r = validate_manifest(&m, dir.path());
        assert!(!r.is_valid());
        assert!(r
            .errors
            .iter()
            .any(|e| e.message.contains("nodes must not be empty")));
    }

    #[test]
    fn test_description_too_long() {
        let dir = tempfile::tempdir().unwrap();
        setup_test_dir(dir.path());
        let mut m = minimal_manifest();
        m.description = Some("x".repeat(257));
        let r = validate_manifest(&m, dir.path());
        assert!(!r.is_valid());
        assert!(r.errors.iter().any(|e| e.message.contains("256")));
    }

    #[test]
    fn test_program_file_not_found() {
        let dir = tempfile::tempdir().unwrap();
        // No files created
        let m = minimal_manifest();
        let r = validate_manifest(&m, dir.path());
        assert!(!r.is_valid());
        assert!(r
            .errors
            .iter()
            .any(|e| e.message.contains("program file not found")));
    }

    #[test]
    fn test_invalid_elf_magic() {
        let dir = tempfile::tempdir().unwrap();
        let bpf_dir = dir.path().join("bpf");
        std::fs::create_dir_all(&bpf_dir).unwrap();
        std::fs::write(bpf_dir.join("test.elf"), b"hello world").unwrap();
        let m = minimal_manifest();
        let r = validate_manifest(&m, dir.path());
        assert!(!r.is_valid());
        assert!(r.errors.iter().any(|e| e.message.contains("invalid ELF")));
    }

    #[test]
    fn test_duplicate_program_names() {
        let dir = tempfile::tempdir().unwrap();
        setup_test_dir(dir.path());
        let mut m = minimal_manifest();
        m.programs.push(ProgramEntry {
            name: "test-prog".to_string(),
            path: "bpf/test.elf".to_string(),
            profile: VerificationProfile::Resident,
        });
        let r = validate_manifest(&m, dir.path());
        assert!(!r.is_valid());
        assert!(r
            .errors
            .iter()
            .any(|e| e.message.contains("duplicate program name")));
    }

    #[test]
    fn test_invalid_program_name() {
        let dir = tempfile::tempdir().unwrap();
        setup_test_dir(dir.path());
        let mut m = minimal_manifest();
        m.programs[0].name = "UPPER_CASE".to_string();
        m.nodes[0].program = "UPPER_CASE".to_string();
        let r = validate_manifest(&m, dir.path());
        assert!(!r.is_valid());
        assert!(r
            .errors
            .iter()
            .any(|e| e.rule == "program.name" && e.message.contains("pattern")));
    }

    #[test]
    fn test_handler_unknown_program() {
        let dir = tempfile::tempdir().unwrap();
        setup_test_dir(dir.path());
        let mut m = minimal_manifest();
        m.handlers.push(HandlerEntry {
            program: "nonexistent".to_string(),
            command: "python3".to_string(),
            args: vec![],
            working_dir: None,
            reply_timeout_ms: None,
        });
        let r = validate_manifest(&m, dir.path());
        assert!(!r.is_valid());
        assert!(r
            .errors
            .iter()
            .any(|e| e.message.contains("unknown program")));
    }

    #[test]
    fn test_handler_catch_all() {
        let dir = tempfile::tempdir().unwrap();
        setup_test_dir(dir.path());
        let mut m = minimal_manifest();
        m.handlers.push(HandlerEntry {
            program: "*".to_string(),
            command: "python3".to_string(),
            args: vec!["handler.py".to_string()],
            working_dir: None,
            reply_timeout_ms: None,
        });
        let r = validate_manifest(&m, dir.path());
        assert!(
            r.is_valid(),
            "catch-all handler should be valid: {:?}",
            r.errors
        );
    }

    #[test]
    fn test_duplicate_catch_all() {
        let dir = tempfile::tempdir().unwrap();
        setup_test_dir(dir.path());
        let mut m = minimal_manifest();
        for _ in 0..2 {
            m.handlers.push(HandlerEntry {
                program: "*".to_string(),
                command: "python3".to_string(),
                args: vec![],
                working_dir: None,
                reply_timeout_ms: None,
            });
        }
        let r = validate_manifest(&m, dir.path());
        assert!(!r.is_valid());
        assert!(r
            .errors
            .iter()
            .any(|e| e.message.contains("duplicate catch-all")));
    }

    #[test]
    fn test_handler_empty_command() {
        let dir = tempfile::tempdir().unwrap();
        setup_test_dir(dir.path());
        let mut m = minimal_manifest();
        m.handlers.push(HandlerEntry {
            program: "test-prog".to_string(),
            command: "".to_string(),
            args: vec![],
            working_dir: None,
            reply_timeout_ms: None,
        });
        let r = validate_manifest(&m, dir.path());
        assert!(!r.is_valid());
        assert!(r
            .errors
            .iter()
            .any(|e| e.message.contains("must not be empty")));
    }

    #[test]
    fn test_handler_zero_timeout() {
        let dir = tempfile::tempdir().unwrap();
        setup_test_dir(dir.path());
        let mut m = minimal_manifest();
        m.handlers.push(HandlerEntry {
            program: "test-prog".to_string(),
            command: "python3".to_string(),
            args: vec![],
            working_dir: None,
            reply_timeout_ms: Some(0),
        });
        let r = validate_manifest(&m, dir.path());
        assert!(!r.is_valid());
        assert!(r
            .errors
            .iter()
            .any(|e| e.message.contains("positive integer")));
    }

    #[test]
    fn test_node_unknown_program() {
        let dir = tempfile::tempdir().unwrap();
        setup_test_dir(dir.path());
        let mut m = minimal_manifest();
        m.nodes.push(NodeTarget {
            name: "node-2".to_string(),
            program: "nonexistent".to_string(),
            hardware: None,
        });
        let r = validate_manifest(&m, dir.path());
        assert!(!r.is_valid());
        assert!(r
            .errors
            .iter()
            .any(|e| e.message.contains("unknown program")));
    }

    #[test]
    fn test_duplicate_node_names() {
        let dir = tempfile::tempdir().unwrap();
        setup_test_dir(dir.path());
        let mut m = minimal_manifest();
        m.nodes.push(NodeTarget {
            name: "node-1".to_string(),
            program: "test-prog".to_string(),
            hardware: None,
        });
        let r = validate_manifest(&m, dir.path());
        assert!(!r.is_valid());
        assert!(r
            .errors
            .iter()
            .any(|e| e.message.contains("duplicate node name")));
    }

    #[test]
    fn test_rf_channel_out_of_range() {
        let dir = tempfile::tempdir().unwrap();
        setup_test_dir(dir.path());
        let mut m = minimal_manifest();
        m.nodes[0].hardware = Some(HardwareProfile {
            sensors: vec![],
            rf_channel: Some(14),
        });
        let r = validate_manifest(&m, dir.path());
        assert!(!r.is_valid());
        assert!(r.errors.iter().any(|e| e.message.contains("rf_channel")));
    }

    #[test]
    fn test_sensor_label_too_long() {
        let dir = tempfile::tempdir().unwrap();
        setup_test_dir(dir.path());
        let mut m = minimal_manifest();
        m.nodes[0].hardware = Some(HardwareProfile {
            sensors: vec![SensorDescriptor {
                sensor_type: SensorType::I2c,
                id: 0x76,
                label: Some("x".repeat(65)),
            }],
            rf_channel: None,
        });
        let r = validate_manifest(&m, dir.path());
        assert!(!r.is_valid());
        assert!(r.errors.iter().any(|e| e.message.contains("64 bytes")));
    }

    #[test]
    fn test_unreferenced_program_warning() {
        let dir = tempfile::tempdir().unwrap();
        setup_test_dir(dir.path());
        // Create a second ELF
        std::fs::write(
            dir.path().join("bpf").join("extra.elf"),
            [0x7f, b'E', b'L', b'F', 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        )
        .unwrap();
        let mut m = minimal_manifest();
        m.programs.push(ProgramEntry {
            name: "extra-prog".to_string(),
            path: "bpf/extra.elf".to_string(),
            profile: VerificationProfile::Resident,
        });
        let r = validate_manifest(&m, dir.path());
        assert!(
            r.is_valid(),
            "warnings should not fail validation: {:?}",
            r.errors
        );
        assert!(r.warnings.iter().any(|w| w.message.contains("extra-prog")));
    }

    #[test]
    fn test_dotdot_in_filename_not_path_component() {
        let dir = tempfile::tempdir().unwrap();
        let bpf_dir = dir.path().join("bpf");
        std::fs::create_dir_all(&bpf_dir).unwrap();
        let mut elf = vec![0x7f, b'E', b'L', b'F'];
        elf.extend_from_slice(&[0u8; 12]);
        std::fs::write(bpf_dir.join("my..file.elf"), &elf).unwrap();
        let mut m = minimal_manifest();
        m.programs[0].path = "bpf/my..file.elf".to_string();
        let r = validate_manifest(&m, dir.path());
        assert!(
            r.is_valid(),
            "dotdot in filename (not path component) should be valid: {:?}",
            r.errors
        );
    }

    #[test]
    fn test_empty_node_name() {
        let dir = tempfile::tempdir().unwrap();
        setup_test_dir(dir.path());
        let mut m = minimal_manifest();
        m.nodes[0].name = "".to_string();
        let r = validate_manifest(&m, dir.path());
        assert!(!r.is_valid());
        assert!(r
            .errors
            .iter()
            .any(|e| e.message.contains("must not be empty")));
    }

    #[test]
    fn test_multiple_handlers_same_program_allowed() {
        let dir = tempfile::tempdir().unwrap();
        setup_test_dir(dir.path());
        let mut m = minimal_manifest();
        for _ in 0..2 {
            m.handlers.push(HandlerEntry {
                program: "test-prog".to_string(),
                command: "python3".to_string(),
                args: vec![],
                working_dir: None,
                reply_timeout_ms: None,
            });
        }
        let r = validate_manifest(&m, dir.path());
        assert!(
            r.is_valid(),
            "multiple handlers for the same program should be allowed: {:?}",
            r.errors
        );
    }

    #[test]
    fn test_handler_working_dir_not_found() {
        let dir = tempfile::tempdir().unwrap();
        setup_test_dir(dir.path());
        let mut m = minimal_manifest();
        m.handlers.push(HandlerEntry {
            program: "test-prog".to_string(),
            command: "python3".to_string(),
            args: vec![],
            working_dir: Some("handler/nonexistent".to_string()),
            reply_timeout_ms: None,
        });
        let r = validate_manifest(&m, dir.path());
        assert!(!r.is_valid());
        assert!(r
            .errors
            .iter()
            .any(|e| e.message.contains("working directory not found")));
    }

    #[test]
    fn test_handler_working_dir_is_file() {
        let dir = tempfile::tempdir().unwrap();
        setup_test_dir(dir.path());
        // Create a file where working_dir expects a directory
        std::fs::write(dir.path().join("not-a-dir"), b"data").unwrap();
        let mut m = minimal_manifest();
        m.handlers.push(HandlerEntry {
            program: "test-prog".to_string(),
            command: "python3".to_string(),
            args: vec![],
            working_dir: Some("not-a-dir".to_string()),
            reply_timeout_ms: None,
        });
        let r = validate_manifest(&m, dir.path());
        assert!(!r.is_valid());
        assert!(r
            .errors
            .iter()
            .any(|e| e.message.contains("must be a directory")));
    }

    #[test]
    fn test_handler_working_dir_valid() {
        let dir = tempfile::tempdir().unwrap();
        setup_test_dir(dir.path());
        std::fs::create_dir_all(dir.path().join("handler")).unwrap();
        let mut m = minimal_manifest();
        m.handlers.push(HandlerEntry {
            program: "test-prog".to_string(),
            command: "python3".to_string(),
            args: vec![],
            working_dir: Some("handler".to_string()),
            reply_timeout_ms: None,
        });
        let r = validate_manifest(&m, dir.path());
        assert!(
            r.is_valid(),
            "valid working_dir should pass: {:?}",
            r.errors
        );
    }
}
