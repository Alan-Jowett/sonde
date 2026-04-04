<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Bundle Tool Validation Specification

> **Document status:** Draft
> **Source:** Derived from [bundle-tool-requirements.md](bundle-tool-requirements.md) and [bundle-tool-design.md](bundle-tool-design.md).
> **Scope:** Test cases for the `sonde-bundle` crate (library and CLI).
> **Related:** [bundle-format.md](bundle-format.md), [gateway-validation.md](gateway-validation.md) §15

---

## 1  Test environment

### 1.1  Test infrastructure

Tests use:
- Temporary directories for bundle creation and extraction
- Minimal valid ELF binaries (4-byte `\x7fELF` + padding) for program validation
- In-memory tar/gzip generation for archive tests
- The `sonde-bundle` library API directly (unit/integration tests)
- The `sonde-bundle` CLI binary (CLI tests via `assert_cmd` or `Command`)

### 1.2  Test data helpers

A `test_helpers` module provides:
- `minimal_elf() -> Vec<u8>` — returns the smallest valid ELF header
- `valid_manifest() -> Manifest` — returns a minimal valid manifest
- `create_test_bundle(dir: &Path, manifest: &Manifest)` — writes `app.yaml` + dummy ELF files to a directory
- `build_test_archive(dir: &Path) -> Vec<u8>` — creates an in-memory `.tgz` from a directory

---

## 2  Manifest parsing tests

### T-SB-0100  Valid manifest parsing

**Traces to:** SB-0100

**Steps:**
1. Create a YAML string with all required fields (`schema_version`, `name`, `version`, `programs`, `nodes`) and all optional fields (`description`, `handlers` with `args`, `working_dir`, `reply_timeout_ms`, node `hardware` with sensors and `rf_channel`).
2. Parse using `Manifest::from_yaml()`.

**Expected:**
- Parse succeeds.
- All fields are populated with correct values.
- Optional fields are `Some(...)` with correct values.

---

### T-SB-0101  Manifest with missing optional fields

**Traces to:** SB-0100

**Steps:**
1. Create a YAML string with only required fields (no `description`, no `handlers`, no `hardware`).
2. Parse using `Manifest::from_yaml()`.

**Expected:**
- Parse succeeds.
- `description` is `None`.
- `handlers` is an empty list.
- Node `hardware` is `None`.

---

### T-SB-0102  Manifest with unknown fields

**Traces to:** SB-0100 (forward compatibility)

**Steps:**
1. Create a valid YAML manifest with an extra unknown field `future_feature: true`.
2. Parse using `Manifest::from_yaml()`.

**Expected:**
- Parse succeeds (unknown fields are ignored).

---

### T-SB-0103  Invalid YAML syntax

**Traces to:** SB-0100

**Steps:**
1. Provide a malformed YAML string (e.g., unmatched brackets).
2. Parse using `Manifest::from_yaml()`.

**Expected:**
- Parse fails with a descriptive error.

---

### T-SB-0104  Schema version validation — supported

**Traces to:** SB-0102

**Steps:**
1. Create a manifest with `schema_version: 1`.
2. Validate.

**Expected:**
- No schema version error.

---

### T-SB-0105  Schema version validation — unsupported

**Traces to:** SB-0102

**Steps:**
1. Create a manifest with `schema_version: 2`.
2. Validate.

**Expected:**
- Validation error: "unsupported schema version: 2 (maximum supported: 1)".

---

### T-SB-0106  Schema version validation — zero

**Traces to:** SB-0102

**Steps:**
1. Create a manifest with `schema_version: 0`.
2. Validate.

**Expected:**
- Validation error: schema version must be ≥ 1.

---

### T-SB-0107  Schema version validation — missing

**Traces to:** SB-0102

**Steps:**
1. Create a manifest without `schema_version`.
2. Parse.

**Expected:**
- Validation error: `schema_version` is required (missing).

---

## 3  Archive tests

### T-SB-0200  Valid archive extraction

**Traces to:** SB-0101

**Steps:**
1. Create a valid `.tgz` archive with `app.yaml`, `bpf/test.elf`, and `handler/run.sh`.
2. Extract using `extract_bundle()`.

**Expected:**
- Extraction succeeds.
- `app.yaml`, `bpf/test.elf`, and `handler/run.sh` exist in the target directory.
- Parsed manifest is returned.

---

### T-SB-0201  Archive with path traversal

**Traces to:** SB-0101

**Steps:**
1. Create a `.tgz` archive with an entry `../etc/passwd`.
2. Attempt to extract.

**Expected:**
- Extraction fails with `PathTraversal` error.
- No files are written to disk.

---

### T-SB-0202  Archive with symlink

**Traces to:** SB-0101

**Steps:**
1. Create a `.tgz` archive with a symlink entry.
2. Attempt to extract.

**Expected:**
- Extraction fails with `SymlinkNotAllowed` error.

---

### T-SB-0203  Non-gzip file rejection

**Traces to:** SB-0200

**Steps:**
1. Create a plain text file named `test.sondeapp`.
2. Call `validate_bundle()`.

**Expected:**
- Error: "invalid archive format".

---

### T-SB-0204  Archive missing app.yaml

**Traces to:** SB-0200

**Steps:**
1. Create a valid `.tgz` with files but no `app.yaml`.
2. Call `validate_bundle()`.

**Expected:**
- Error: "missing manifest".

---

## 4  Structural validation tests

### T-SB-0300  Missing required field — name

**Traces to:** SB-0200

**Steps:**
1. Create a manifest without `name`.
2. Validate.

**Expected:**
- Error: "missing required field: `name`".

---

### T-SB-0301  Invalid name — uppercase

**Traces to:** SB-0200

**Steps:**
1. Create a manifest with `name: "MyApp"`.
2. Validate.

**Expected:**
- Error: "name must match pattern `[a-z0-9]([a-z0-9-]*[a-z0-9])?`".

---

### T-SB-0302  Invalid name — leading hyphen

**Traces to:** SB-0200

**Steps:**
1. Create a manifest with `name: "-my-app"`.
2. Validate.

**Expected:**
- Error: name must not start with a hyphen.

---

### T-SB-0303  Invalid version — not semver

**Traces to:** SB-0200

**Steps:**
1. Create a manifest with `version: "1.0"`.
2. Validate.

**Expected:**
- Error: "version must be valid semver".

---

### T-SB-0304  Empty programs list

**Traces to:** SB-0200

**Steps:**
1. Create a manifest with `programs: []`.
2. Validate.

**Expected:**
- Error: "programs must not be empty".

---

### T-SB-0305  Empty nodes list

**Traces to:** SB-0200

**Steps:**
1. Create a manifest with `nodes: []`.
2. Validate.

**Expected:**
- Error: "nodes must not be empty".

---

### T-SB-0306  Description exceeds max length

**Traces to:** SB-0200

**Steps:**
1. Create a manifest with `description` set to a 257-character string.
2. Validate.

**Expected:**
- Error: "`description` must not exceed 256 characters".

---

## 5  Program validation tests

### T-SB-0400  Program file not found

**Traces to:** SB-0201

**Steps:**
1. Create a manifest referencing `bpf/missing.elf`.
2. Validate against a directory without that file.

**Expected:**
- Error: "program file not found: `bpf/missing.elf`".

---

### T-SB-0401  Invalid ELF magic

**Traces to:** SB-0201

**Steps:**
1. Create a file at `bpf/bad.elf` containing `hello world`.
2. Validate.

**Expected:**
- Error: "invalid ELF file: `bpf/bad.elf`".

---

### T-SB-0402  Duplicate program names

**Traces to:** SB-0201

**Steps:**
1. Create a manifest with two programs both named `"temp-reader"`.
2. Validate.

**Expected:**
- Error: "duplicate program name: `temp-reader`".

---

### T-SB-0403  Invalid profile value

**Traces to:** SB-0201

**Steps:**
1. Create a manifest with `profile: "persistent"` (invalid).
2. Parse/validate.

**Expected:**
- Error: profile must be `"resident"` or `"ephemeral"`.

---

### T-SB-0404  Invalid program name

**Traces to:** SB-0201

**Steps:**
1. Create a manifest with a program named `"UPPER_CASE"`.
2. Validate.

**Expected:**
- Error: program name must match pattern `[a-z0-9]([a-z0-9_-]*[a-z0-9])?`.

---

## 6  Handler validation tests

### T-SB-0500  Handler references unknown program

**Traces to:** SB-0202

**Steps:**
1. Create a manifest with a handler referencing `program: "nonexistent"`.
2. Validate.

**Expected:**
- Error: "handler references unknown program: `nonexistent`".

---

### T-SB-0501  Valid catch-all handler

**Traces to:** SB-0202

**Steps:**
1. Create a manifest with a handler with `program: "*"`.
2. Validate.

**Expected:**
- No error for the catch-all handler.

---

### T-SB-0502  Duplicate catch-all handlers

**Traces to:** SB-0202

**Steps:**
1. Create a manifest with two handlers having `program: "*"`.
2. Validate.

**Expected:**
- Error: "duplicate catch-all handler".

---

### T-SB-0503  Handler with empty command

**Traces to:** SB-0202

**Steps:**
1. Create a manifest with a handler with `command: ""`.
2. Validate.

**Expected:**
- Error: "handler command must not be empty".

---

### T-SB-0504  Handler with invalid reply_timeout_ms

**Traces to:** SB-0202

**Steps:**
1. Create a manifest with a handler with `reply_timeout_ms: 0`.
2. Validate.

**Expected:**
- Error: "`reply_timeout_ms` must be a positive integer".

---

## 7  Node validation tests

### T-SB-0600  Node references unknown program

**Traces to:** SB-0203

**Steps:**
1. Create a manifest with a node referencing `program: "nonexistent"`.
2. Validate.

**Expected:**
- Error: "node `<name>` references unknown program: `nonexistent`".

---

### T-SB-0601  Duplicate node names

**Traces to:** SB-0203

**Steps:**
1. Create a manifest with two nodes named `"sensor-1"`.
2. Validate.

**Expected:**
- Error: "duplicate node name: `sensor-1`".

---

### T-SB-0602  Invalid sensor type

**Traces to:** SB-0203

**Steps:**
1. Create a manifest with a sensor type `"uart"` (not in allowed set).
2. Parse/validate.

**Expected:**
- Error: sensor type must be one of `i2c`, `adc`, `gpio`, `spi`.

---

### T-SB-0603  RF channel out of range

**Traces to:** SB-0203

**Steps:**
1. Create a manifest with `rf_channel: 14`.
2. Validate.

**Expected:**
- Error: "`rf_channel` must be between 1 and 13".

---

### T-SB-0604  Sensor label exceeds max length

**Traces to:** SB-0203

**Steps:**
1. Create a manifest with a sensor label of 65 bytes.
2. Validate.

**Expected:**
- Error: "sensor label must not exceed 64 bytes".

---

## 8  Cross-reference validation tests

### T-SB-0700  Unreferenced program warning

**Traces to:** SB-0204

**Steps:**
1. Create a manifest with program `"temp-reader"` and program `"humidity-reader"`, but only nodes referencing `"temp-reader"`.
2. Validate.

**Expected:**
- Validation succeeds (no errors).
- Warning: "program `humidity-reader` is not referenced by any node".

---

## 9  Bundle creation tests

### T-SB-0800  Create valid bundle

**Traces to:** SB-0300

**Steps:**
1. Create a directory with `app.yaml`, `bpf/test.elf` (valid ELF), and `handler/run.sh`.
2. Call `create_bundle()`.

**Expected:**
- A `.sondeapp` file is created.
- The file is a valid gzipped tar.
- Extracting and re-validating succeeds.

---

### T-SB-0801  Create fails on invalid manifest

**Traces to:** SB-0301

**Steps:**
1. Create a directory with an invalid `app.yaml` (missing `name`).
2. Call `create_bundle()`.

**Expected:**
- No `.sondeapp` file is created.
- Error indicates the validation failure.

---

### T-SB-0802  Create excludes unreferenced files

**Traces to:** SB-0300

**Steps:**
1. Create a directory with `app.yaml`, `bpf/test.elf` (referenced), and `bpf/unreferenced.elf` (NOT referenced in manifest).
2. Call `create_bundle()`.
3. Extract the resulting archive and list its contents.

**Expected:**
- `bpf/test.elf` is present in the archive.
- `bpf/unreferenced.elf` is NOT present in the archive.

---

## 10  Inspection tests

### T-SB-0900  Inspect valid bundle

**Traces to:** SB-0400

**Steps:**
1. Create a valid `.sondeapp` bundle.
2. Call `inspect_bundle()`.

**Expected:**
- Returns `BundleInfo` with correct manifest fields.
- File list includes `app.yaml`, `bpf/test.elf`, and handler files.
- File sizes are accurate.

---

## 11  CLI tests

### T-SB-1000  CLI create — success

**Traces to:** SB-0500

**Steps:**
1. Create a valid source directory.
2. Run `sonde-bundle create <dir>`.

**Expected:**
- Exit code 0.
- Output includes the created file path and size.
- The created file is valid.

---

### T-SB-1001  CLI create — validation failure

**Traces to:** SB-0500

**Steps:**
1. Create a directory with invalid `app.yaml`.
2. Run `sonde-bundle create <dir>`.

**Expected:**
- Exit code non-zero.
- Stderr includes validation error message.
- No `.sondeapp` file created.

---

### T-SB-1002  CLI validate — valid bundle

**Traces to:** SB-0501

**Steps:**
1. Create a valid `.sondeapp`.
2. Run `sonde-bundle validate <path>`.

**Expected:**
- Exit code 0.

---

### T-SB-1003  CLI validate — invalid bundle

**Traces to:** SB-0501

**Steps:**
1. Create an invalid `.sondeapp` (e.g., missing ELF).
2. Run `sonde-bundle validate <path>`.

**Expected:**
- Exit code non-zero.
- Stderr includes validation errors.

---

### T-SB-1004  CLI validate — warnings only

**Traces to:** SB-0501

**Steps:**
1. Create a valid `.sondeapp` with an unreferenced program.
2. Run `sonde-bundle validate <path>`.

**Expected:**
- Exit code 0 (warnings do not fail validation).
- Stderr includes warning about unreferenced program.

---

### T-SB-1005  CLI inspect — text output

**Traces to:** SB-0502

**Steps:**
1. Create a valid `.sondeapp`.
2. Run `sonde-bundle inspect <path>`.

**Expected:**
- Output includes app name, version, programs, nodes, handlers.

---

### T-SB-1006  CLI inspect — JSON output

**Traces to:** SB-0502

**Steps:**
1. Create a valid `.sondeapp`.
2. Run `sonde-bundle inspect <path> --format json`.

**Expected:**
- Output is valid JSON.
- JSON contains manifest fields.

---

## 10  Distribution tests

### T-SB-1100  CI produces sonde-bundle artifact

**Traces to:** SB-0600

**Steps:**
1. Push a change to `crates/sonde-bundle/` on the `main` branch.
2. Wait for CI to complete.
3. List workflow artifacts for the completed run.

**Expected:**
- An artifact named `sonde-bundle-linux-x86_64` exists and contains a
  valid executable.

---

### T-SB-1101  Cross-platform artifacts

**Traces to:** SB-0601

**Steps:**
1. Trigger a CI run that includes the sonde-bundle build job.
2. List all artifacts for the run.

**Expected:**
- Artifacts exist for at least two platforms: `sonde-bundle-linux-x86_64`
  and `sonde-bundle-windows-x86_64`.
- A `sonde-bundle-macos-aarch64` artifact SHOULD also exist.
- Each contains a valid binary for the target platform.

---

### T-SB-1102  Template repo CMake build

**Traces to:** SB-0602

**Steps:**
1. Clone the `sonde-app-template` repository.
2. Install clang with BPF target support.
3. Run `cmake -B build -DCMAKE_TOOLCHAIN_FILE=cmake/bpf-toolchain.cmake`.
4. Run `cmake --build build`.

**Expected:**
- Build completes without errors.
- `build/bpf/my_sensor.o` exists and starts with `\x7fELF` magic.
- `build/compile_commands.json` exists.

---

### T-SB-1103  Template CI produces sondeapp

**Traces to:** SB-0603

**Steps:**
1. Push a commit to the template repo's `main` branch.
2. Wait for the `build.yml` workflow to complete.
3. List workflow artifacts.

**Expected:**
- A `.sondeapp` artifact exists.
- Downloading and running `sonde-bundle validate` on it succeeds (exit 0).

---

### T-SB-1104  Version pinning

**Traces to:** SB-0604

**Steps:**
1. In the template repo, set `.sonde-version` to a specific sonde CI run ID.
2. Push and wait for CI.
3. Check which sonde-bundle binary was downloaded.

**Expected:**
- The CI log shows downloading sonde-bundle from the pinned run, not latest.

---

### T-SB-1105  Local build and validate

**Traces to:** SB-0605

**Steps:**
1. Clone the template repo locally.
2. Install clang and sonde-bundle.
3. Run `cmake -B build -DCMAKE_TOOLCHAIN_FILE=cmake/bpf-toolchain.cmake && cmake --build build`.
4. Run `sonde-bundle create .`.
5. Run `sonde-bundle validate <output>.sondeapp`.

**Expected:**
- BPF programs compile without errors.
- `sonde-bundle create .` produces a `.sondeapp` archive.
- `sonde-bundle validate <output>.sondeapp` exits 0.

---

## Appendix A  Traceability matrix

| Requirement | Test case(s) |
|-------------|-------------|
| SB-0100 | T-SB-0100, T-SB-0101, T-SB-0102, T-SB-0103 |
| SB-0101 | T-SB-0200, T-SB-0201, T-SB-0202 |
| SB-0102 | T-SB-0104, T-SB-0105, T-SB-0106, T-SB-0107 |
| SB-0200 | T-SB-0203, T-SB-0204, T-SB-0300, T-SB-0301, T-SB-0302, T-SB-0303, T-SB-0304, T-SB-0305, T-SB-0306 |
| SB-0201 | T-SB-0400, T-SB-0401, T-SB-0402, T-SB-0403, T-SB-0404 |
| SB-0202 | T-SB-0500, T-SB-0501, T-SB-0502, T-SB-0503, T-SB-0504 |
| SB-0203 | T-SB-0600, T-SB-0601, T-SB-0602, T-SB-0603, T-SB-0604 |
| SB-0204 | T-SB-0700 |
| SB-0300 | T-SB-0800, T-SB-0802 |
| SB-0301 | T-SB-0801 |
| SB-0400 | T-SB-0900 |
| SB-0500 | T-SB-1000, T-SB-1001 |
| SB-0501 | T-SB-1002, T-SB-1003, T-SB-1004 |
| SB-0502 | T-SB-1005, T-SB-1006 |
| SB-0600 | T-SB-1100 |
| SB-0601 | T-SB-1101 |
| SB-0602 | T-SB-1102 |
| SB-0603 | T-SB-1103 |
| SB-0604 | T-SB-1104 |
| SB-0605 | T-SB-1105 |
