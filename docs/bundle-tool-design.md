<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Bundle Tool Design Specification

> **Document status:** Draft
> **Source:** Derived from [bundle-tool-requirements.md](bundle-tool-requirements.md) and [bundle-format.md](bundle-format.md).
> **Scope:** Architecture and module design of the `sonde-bundle` crate.
> **Related:** [bundle-format.md](bundle-format.md), [gateway-design.md](gateway-design.md) §20

---

## 1  Overview

The `sonde-bundle` crate provides a library and CLI for creating, validating,
and inspecting Sonde App Bundles (`.sondeapp`).  It is a pure Rust crate with
no platform-specific dependencies.

**Crate type:** Library (`lib`) + binary (`sonde-bundle`)

**Dependencies:**
- `serde` / `serde_yaml_ng` — YAML manifest parsing
- `flate2` — gzip compression/decompression
- `tar` — tar archive creation/extraction
- `clap` — CLI argument parsing (binary only)
- `semver` — semver parsing and validation

The crate does NOT depend on `sonde-protocol`, `sonde-gateway`, or any
network/gRPC libraries.  It is a standalone tool for bundle manipulation.

---

## 2  Module architecture

```
sonde-bundle/
├── src/
│   ├── main.rs         # CLI entry point (SB-0500–SB-0502)
│   ├── lib.rs          # Public API re-exports
│   ├── manifest.rs     # Manifest types and YAML parsing (SB-0100)
│   ├── archive.rs      # Archive creation, extraction, and inspection (SB-0101, SB-0300, SB-0400)
│   ├── validate.rs     # Validation logic (SB-0200–SB-0204)
│   └── error.rs        # Error types
├── Cargo.toml
```

### 2.1  Module responsibilities

| Module | Responsibility | Requirements |
|--------|---------------|--------------|
| `manifest` | Parse, serialize, and represent `app.yaml` content | SB-0100, SB-0102 |
| `archive` | Create, extract, and inspect `.sondeapp` (`.tgz`) archives with security enforcement | SB-0101, SB-0300, SB-0301, SB-0400 |
| `validate` | Run all validation rules against a parsed bundle | SB-0200–SB-0204 |
| `error` | Unified error type for all operations | All |
| `main` | CLI entry point with `create`, `validate`, `inspect` subcommands | SB-0500–SB-0502 |

---

## 3  Data types

### 3.1  Manifest

```rust
/// Parsed app.yaml manifest (SB-0100).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub schema_version: u32,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: Option<String>,
    pub programs: Vec<ProgramEntry>,
    pub nodes: Vec<NodeTarget>,
    #[serde(default)]
    pub handlers: Vec<HandlerEntry>,
}

/// A BPF program included in the bundle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgramEntry {
    pub name: String,
    pub path: String,
    pub profile: VerificationProfile,
}

/// Verification profile for a BPF program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VerificationProfile {
    Resident,
    Ephemeral,
}

/// A handler process definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandlerEntry {
    pub program: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub working_dir: Option<String>,
    pub reply_timeout_ms: Option<u32>,
}

/// A node target with optional hardware profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeTarget {
    pub name: String,
    pub program: String,
    #[serde(default)]
    pub hardware: Option<HardwareProfile>,
}

/// Hardware profile describing physical sensors on a node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareProfile {
    #[serde(default)]
    pub sensors: Vec<SensorDescriptor>,
    pub rf_channel: Option<u8>,
}

/// A sensor attached to a node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensorDescriptor {
    #[serde(rename = "type")]
    pub sensor_type: SensorType,
    pub id: u16,
    pub label: Option<String>,
}

/// Sensor bus type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SensorType {
    I2c,
    Adc,
    Gpio,
    Spi,
}
```

### 3.2  Validation result

```rust
/// Result of validating a bundle.
#[derive(Debug)]
pub struct ValidationResult {
    pub errors: Vec<ValidationError>,
    pub warnings: Vec<ValidationWarning>,
}

impl ValidationResult {
    pub fn is_valid(&self) -> bool {
        self.errors.is_empty()
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
```

### 3.3  Bundle info

```rust
/// Structured information about a bundle (SB-0400).
#[derive(Debug)]
pub struct BundleInfo {
    pub manifest: Manifest,
    pub files: Vec<BundleFile>,
    pub archive_size: u64,
}

/// A file entry in the bundle archive.
#[derive(Debug)]
pub struct BundleFile {
    pub path: String,
    pub size: u64,
}
```

---

## 4  Archive module

### 4.1  Extraction (SB-0101)

```rust
/// Extract a .sondeapp archive to a target directory.
///
/// Security: rejects path traversal and symlinks.
pub fn extract_bundle(
    bundle_path: &Path,
    target_dir: &Path,
) -> Result<Manifest, BundleError>;
```

**Algorithm:**
1. Open the file as `GzDecoder` → `tar::Archive`.
2. Iterate entries.  For each entry:
   a. Check path for `..` components → reject with `PathTraversal` error.
   b. Check entry type for symlinks → reject with `SymlinkNotAllowed` error.
   c. Extract to `target_dir / entry_path`.
3. Parse `target_dir / app.yaml` as `Manifest`.
4. Return the parsed manifest.

### 4.2  Creation (SB-0300)

```rust
/// Create a .sondeapp archive from a source directory.
///
/// Validates the source before creating the archive (SB-0301).
pub fn create_bundle(
    source_dir: &Path,
    output_path: &Path,
) -> Result<BundleInfo, BundleError>;
```

**Algorithm:**
1. Parse `source_dir / app.yaml` as `Manifest`.
2. Run `validate_manifest()` on the parsed manifest against `source_dir`.
3. If validation fails, return the errors (no archive created — SB-0301).
4. Create `GzEncoder` → `tar::Builder`.
5. Add `app.yaml` first.
6. For each program entry: add the ELF file at the specified path.
7. For each handler entry: add all files under the handler's working directory.
8. Finish the archive.
9. Return `BundleInfo`.

### 4.3  In-memory reading (SB-0400)

```rust
/// Read bundle metadata without extracting to disk.
pub fn inspect_bundle(
    bundle_path: &Path,
) -> Result<BundleInfo, BundleError>;
```

**Algorithm:**
1. Open as `GzDecoder` → `tar::Archive`.
2. Iterate entries, collecting file paths and sizes.
3. Read `app.yaml` entry into memory and parse as `Manifest`.
4. Return `BundleInfo` with manifest, file list, and archive size.

---

## 5  Validation module

### 5.1  Validation pipeline (SB-0200–SB-0204)

```rust
/// Validate a bundle archive.
pub fn validate_bundle(
    bundle_path: &Path,
) -> Result<ValidationResult, BundleError>;

/// Validate a manifest against a source directory (used during creation).
pub fn validate_manifest(
    manifest: &Manifest,
    source_dir: &Path,
) -> ValidationResult;
```

The validation pipeline runs all rules from [bundle-format.md](bundle-format.md) §6 in order:

1. **Archive validation** (SB-0200): gzip/tar validity, no path traversal, no symlinks.
2. **Manifest field validation** (SB-0200): required fields, regex constraints, semver.
3. **Program validation** (SB-0201): unique names, file existence, ELF magic, valid profile.
4. **Handler validation** (SB-0202): program references, catch-all uniqueness, non-empty command.
5. **Node validation** (SB-0203): program references, unique names, sensor type/channel ranges.
6. **Cross-reference validation** (SB-0204): unreferenced programs → warning.

All rules are evaluated even if earlier rules fail — the result contains ALL
errors and warnings, not just the first.

### 5.2  Name validation

The `name` field is validated against the regex pattern:
`^[a-z0-9]([a-z0-9-]*[a-z0-9])?$` (1–64 characters).  This is checked without
a regex crate dependency using character iteration.

### 5.3  Semver validation

The `version` field is validated using the `semver` crate.  Pre-release and
build metadata are accepted (e.g., `1.0.0-alpha.1+build.42`).

---

## 6  Error handling

```rust
/// Errors from bundle operations.
#[derive(Debug)]
pub enum BundleError {
    /// I/O error reading or writing files.
    Io(std::io::Error),
    /// YAML parse error.
    Yaml(serde_yaml::Error),
    /// Archive is not valid gzip/tar.
    InvalidArchive(String),
    /// Path traversal detected in archive entry.
    PathTraversal(String),
    /// Symlink detected in archive entry.
    SymlinkNotAllowed(String),
    /// Manifest is missing from archive.
    MissingManifest,
    /// Bundle validation failed (contains all errors).
    ValidationFailed(ValidationResult),
}

impl std::fmt::Display for BundleError { ... }
impl std::error::Error for BundleError { ... }
```

---

## 7  CLI design

### 7.1  Command structure

```
sonde-bundle <COMMAND>

Commands:
  create    Create a .sondeapp bundle from a directory
  validate  Validate a .sondeapp bundle
  inspect   Show bundle contents and metadata
  help      Print help
```

### 7.2  `create` subcommand (SB-0500)

```
sonde-bundle create <SOURCE_DIR> [--output <PATH>]

Arguments:
  <SOURCE_DIR>  Directory containing app.yaml and bundle files

Options:
  --output <PATH>  Output path (default: <name>-<version>.sondeapp)
```

**Behavior:**
1. Parse `app.yaml` from `SOURCE_DIR`.
2. Validate manifest + files.
3. Create `.sondeapp` archive.
4. Print: `Created <output-path> (<size> bytes)`.

### 7.3  `validate` subcommand (SB-0501)

```
sonde-bundle validate <BUNDLE_PATH>

Arguments:
  <BUNDLE_PATH>  Path to .sondeapp file
```

**Behavior:**
1. Run all validation rules.
2. Print errors to stderr, warnings to stderr with `warning:` prefix.
3. Exit 0 if valid, non-zero if errors.

### 7.4  `inspect` subcommand (SB-0502)

```
sonde-bundle inspect <BUNDLE_PATH> [--format <FORMAT>]

Arguments:
  <BUNDLE_PATH>  Path to .sondeapp file

Options:
  --format <FORMAT>  Output format: text (default) or json
```

**Behavior:**
1. Read manifest + file list without extracting.
2. Print structured information:
   - App name, version, description
   - Programs (name, path, profile, ELF size)
   - Handlers (program, command, args)
   - Nodes (name, program, hardware)
3. If `--format json`, output as JSON.

---

## 8  Concurrency model

The `sonde-bundle` crate is entirely synchronous.  No async runtime is required.
All I/O is blocking.  This keeps the crate simple and suitable for use in CI
pipelines, build scripts, and as a dependency without pulling in tokio.

---

## Revision history

| Version | Date | Author | Changes |
|---------|------|--------|---------|
| 0.1 | 2026-03-31 | sonde contributors | Initial draft |
