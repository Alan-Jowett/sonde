<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Bundle Tool Requirements Specification

> **Document status:** Draft
> **Source:** Derived from [issue #491](https://github.com/Alan-Jowett/sonde/issues/491) and [bundle-format.md](bundle-format.md).
> **Scope:** This document covers the `sonde-bundle` crate — a library and CLI tool for creating, validating, and inspecting Sonde App Bundles (`.sondeapp`).
> **Related:** [bundle-format.md](bundle-format.md), [gateway-requirements.md](gateway-requirements.md), [gateway-api.md](gateway-api.md)

---

## 1  Definitions

| Term | Definition |
|------|---------|
| **Bundle** | A `.sondeapp` file — a gzipped tar archive containing a manifest and application artifacts. See [bundle-format.md](bundle-format.md). |
| **Manifest** | The `app.yaml` file at the archive root. |
| **Schema version** | Integer in the manifest identifying the manifest format version. |

---

## 2  Requirement format

Each requirement uses the following fields:

- **ID** — Unique identifier (`SB-XXXX`).
- **Title** — Short name.
- **Description** — What the tool must do.
- **Acceptance criteria** — Observable, testable conditions that confirm the requirement is met.
- **Priority** — MoSCoW: **Must**, **Should**, **May**.
- **Source** — Upstream document or issue that motivates the requirement.

---

## 3  Bundle parsing

### SB-0100  Manifest parsing

**Priority:** Must
**Source:** [bundle-format.md](bundle-format.md) §4

**Description:**
The library MUST parse `app.yaml` from a `.sondeapp` archive and produce a
structured `Manifest` value containing all fields defined in [bundle-format.md](bundle-format.md) §4.

**Acceptance criteria:**

1. A valid `app.yaml` with all required fields is parsed without error.
2. Optional fields (`description`, `handlers`, `hardware`) are populated when present and default to `None`/empty when absent.
3. Unknown fields are ignored without error (forward compatibility per [bundle-format.md](bundle-format.md) §5.2).
4. Invalid YAML (syntax errors) produces a descriptive error indicating the parse failure location.

---

### SB-0101  Archive extraction

**Priority:** Must
**Source:** [bundle-format.md](bundle-format.md) §3

**Description:**
The library MUST extract files from a `.sondeapp` (gzipped tar) archive,
enforcing the security constraints in [bundle-format.md](bundle-format.md) §3.2.

**Acceptance criteria:**

1. Valid `.tgz` and `.sondeapp` files are accepted.
2. Archive entries containing path traversal (`../`) are rejected with an error before any file is written.
3. Symlinks in the archive are rejected with an error before any file is written.
4. `app.yaml` is extracted from the archive root.
5. `bpf/` and `handler/` directories are extracted when present.

---

### SB-0102  Schema version handling

**Priority:** Must
**Source:** [bundle-format.md](bundle-format.md) §5

**Description:**
The library MUST check the `schema_version` field and reject manifests with
unsupported versions.

**Acceptance criteria:**

1. `schema_version: 1` is accepted.
2. `schema_version: 2` (or any value > 1) is rejected with an error message indicating the maximum supported version.
3. `schema_version: 0` or negative values are rejected.
4. Missing `schema_version` is rejected.

---

## 4  Bundle validation

### SB-0200  Structural validation

**Priority:** Must
**Source:** [bundle-format.md](bundle-format.md) §6.1–6.2

**Description:**
The library MUST validate that the archive is a valid gzipped tar, contains
`app.yaml` at the root, and that all required manifest fields are present and
well-formed.

**Acceptance criteria:**

1. A non-gzip file is rejected with "invalid archive format".
2. A gzip file without `app.yaml` is rejected with "missing manifest".
3. A manifest missing `name` is rejected with a field-specific error.
4. A manifest with `name` violating the regex constraint (`[a-z0-9]([a-z0-9-]*[a-z0-9])?`) is rejected.
5. A manifest missing `version` is rejected.
6. An invalid semver `version` is rejected.
7. A manifest with an empty `programs` list is rejected.
8. A manifest with an empty `nodes` list is rejected.
9. A manifest with `description` exceeding 256 characters is rejected.

---

### SB-0201  Program reference validation

**Priority:** Must
**Source:** [bundle-format.md](bundle-format.md) §6.3

**Description:**
The library MUST validate that every program entry references an existing ELF
file in the archive and that program names are unique.

**Acceptance criteria:**

1. A program with a `path` that does not exist in the archive is rejected with "program file not found: `<path>`".
2. A program whose file does not start with `\x7fELF` magic is rejected with "invalid ELF file: `<path>`".
3. Duplicate program names are rejected.
4. A program with invalid `profile` (not `"resident"` or `"ephemeral"`) is rejected.
5. A program with a `name` violating the regex constraint (`[a-z0-9]([a-z0-9_-]*[a-z0-9])?`) is rejected.

---

### SB-0202  Handler reference validation

**Priority:** Must
**Source:** [bundle-format.md](bundle-format.md) §6.4

**Description:**
The library MUST validate that handler entries reference valid program names
and that at most one catch-all handler exists.

**Acceptance criteria:**

1. A handler referencing a program name not in the `programs` list is rejected.
2. A handler with `program: "*"` is accepted.
3. Two handlers with `program: "*"` are rejected with "duplicate catch-all handler".
4. A handler with an empty `command` is rejected.
5. A handler with `reply_timeout_ms: 0` or negative is rejected.

---

### SB-0203  Node reference validation

**Priority:** Must
**Source:** [bundle-format.md](bundle-format.md) §6.5

**Description:**
The library MUST validate that node entries reference valid program names and
that node names are unique.

**Acceptance criteria:**

1. A node referencing a program name not in the `programs` list is rejected.
2. Duplicate node names are rejected.
3. `hardware.sensors[].type` values outside `{"i2c", "adc", "gpio", "spi"}` are rejected.
4. `hardware.rf_channel` values outside 1–13 are rejected.
5. `hardware.sensors[].label` values exceeding 64 bytes UTF-8 are rejected.

---

### SB-0204  Cross-reference validation

**Priority:** Should
**Source:** [bundle-format.md](bundle-format.md) §6.6

**Description:**
The library SHOULD warn (not error) when a program is defined in `programs`
but not referenced by any node.

**Acceptance criteria:**

1. A program not referenced by any node produces a warning, not an error.
2. A program referenced by at least one node produces no warning.
3. Validation still succeeds (returns OK) when only warnings are present.

---

## 5  Bundle creation

### SB-0300  Create bundle from directory

**Priority:** Must
**Source:** [issue #491](https://github.com/Alan-Jowett/sonde/issues/491)

**Description:**
The library MUST create a `.sondeapp` archive from a directory containing
`app.yaml` and the referenced program/handler files.

**Acceptance criteria:**

1. Given a directory with a valid `app.yaml`, `bpf/` with ELF files, and `handler/` with handler files, `create_bundle()` produces a valid `.sondeapp` file.
2. The resulting archive passes all validation rules (SB-0200–SB-0204).
3. The manifest is at the archive root (not nested in a subdirectory).
4. Files not referenced by the manifest are NOT included in the archive.

---

### SB-0301  Validate on create

**Priority:** Must
**Source:** [bundle-format.md](bundle-format.md) §6

**Description:**
Bundle creation MUST run all validation rules before writing the archive.
An invalid directory layout MUST NOT produce an archive.

**Acceptance criteria:**

1. If validation fails, no `.sondeapp` file is created.
2. The error message matches the validation error from SB-0200–SB-0204.

---

## 6  Bundle inspection

### SB-0400  Inspect bundle contents

**Priority:** Should
**Source:** [issue #491](https://github.com/Alan-Jowett/sonde/issues/491)

**Description:**
The library SHOULD provide a function to inspect a bundle and return structured
information about its contents without extracting files to disk.

**Acceptance criteria:**

1. `inspect_bundle()` returns the parsed manifest, list of files in the archive, and total archive size.
2. Program files are identified with their sizes.
3. Handler files are identified with their sizes.

---

## 7  CLI tool

### SB-0500  `sonde-bundle create` command

**Priority:** Must
**Source:** [issue #491](https://github.com/Alan-Jowett/sonde/issues/491)

**Description:**
The `sonde-bundle` CLI MUST provide a `create` subcommand that builds a
`.sondeapp` archive from a source directory.

**Acceptance criteria:**

1. `sonde-bundle create <source-dir> [--output <path>]` creates a `.sondeapp` file.
2. If `--output` is not specified, the output file is `<name>-<version>.sondeapp` in the current directory.
3. On validation failure, the command exits with a non-zero exit code and prints the validation errors to stderr.
4. On success, the command prints the output path and bundle size to stdout.

---

### SB-0501  `sonde-bundle validate` command

**Priority:** Must
**Source:** [issue #491](https://github.com/Alan-Jowett/sonde/issues/491)

**Description:**
The `sonde-bundle` CLI MUST provide a `validate` subcommand that checks a
`.sondeapp` file against all validation rules without deploying.

**Acceptance criteria:**

1. `sonde-bundle validate <bundle-path>` exits with code 0 on a valid bundle.
2. On an invalid bundle, exits with non-zero code and prints all validation errors to stderr.
3. Warnings (e.g., unreferenced programs) are printed to stderr but do not cause a non-zero exit.

---

### SB-0502  `sonde-bundle inspect` command

**Priority:** Should
**Source:** [issue #491](https://github.com/Alan-Jowett/sonde/issues/491)

**Description:**
The `sonde-bundle` CLI SHOULD provide an `inspect` subcommand that displays
bundle contents and manifest in a human-readable format.

**Acceptance criteria:**

1. `sonde-bundle inspect <bundle-path>` prints the manifest fields (name, version, programs, nodes, handlers).
2. Program ELF sizes are displayed.
3. Output is human-readable by default; `--format json` produces JSON output.

---

## 8  Distribution

### SB-0600  CI binary artifacts

**Priority:** Must
**Source:** [issue #632](https://github.com/Alan-Jowett/sonde/issues/632)

**Description:**
The sonde CI workflow MUST build the `sonde-bundle` CLI binary and upload it
as a workflow artifact on every push to `main`.

**Acceptance criteria:**

1. A push to `main` that changes `crates/sonde-bundle/**` or `Cargo.lock` triggers a CI job that builds `sonde-bundle` in release mode.
2. The built binary is uploaded as a GitHub Actions artifact with a platform-identifying name (e.g., `sonde-bundle-linux-x86_64`).
3. The artifact is downloadable via `gh run download` or the Actions UI.

---

### SB-0601  Cross-platform builds

**Priority:** Must
**Source:** [issue #632](https://github.com/Alan-Jowett/sonde/issues/632)

**Description:**
The CI MUST produce `sonde-bundle` binaries for Linux x86_64 and Windows
x86_64.  macOS aarch64 SHOULD be provided when CI runner cost is acceptable.

**Acceptance criteria:**

1. Each CI run produces at least two artifacts: `sonde-bundle-linux-x86_64` and `sonde-bundle-windows-x86_64`.
2. A `sonde-bundle-macos-aarch64` artifact SHOULD also be produced.
3. Each artifact contains the `sonde-bundle` binary (or `.exe` on Windows).
4. Each binary is statically linked or self-contained (no runtime dependencies beyond the OS).

> **Note:** macOS aarch64 runners cost ~10× more CI minutes than Linux.
> The macOS build MAY be deferred or run only on tagged releases to control costs.

---

### SB-0602  Template repo structure

**Priority:** Must
**Source:** [issue #632](https://github.com/Alan-Jowett/sonde/issues/632)

**Description:**
A GitHub template repository (`sonde-app-template`) MUST provide a
ready-to-use project scaffold for sonde app developers. It MUST contain:
a CMake-based BPF build system with a BPF toolchain file, the `sonde_helpers.h`
header, an example BPF program, an example Python handler, an `app.yaml`
manifest, and a README.

**Acceptance criteria:**

1. The repository is marked as a GitHub template (users can click "Use this template").
2. The repo contains at minimum: `CMakeLists.txt`, `cmake/bpf-toolchain.cmake`, `bpf/include/sonde_helpers.h`, `bpf/my_sensor.c`, `handler/handler.py`, `app.yaml`, `README.md`, `.github/workflows/build.yml`.
3. `cmake -B build -DCMAKE_TOOLCHAIN_FILE=cmake/bpf-toolchain.cmake && cmake --build build` compiles `bpf/my_sensor.c` to a BPF ELF object file.
4. `cmake --build build` generates `compile_commands.json` for IDE integration.
5. `sonde_helpers.h` includes a comment indicating the sonde ABI version it targets.

---

### SB-0603  Template CI workflow

**Priority:** Must
**Source:** [issue #632](https://github.com/Alan-Jowett/sonde/issues/632)

**Description:**
The template repository MUST include a GitHub Actions workflow that:
downloads a `sonde-bundle` binary, compiles BPF programs with CMake,
runs `sonde-bundle create`, and uploads the resulting `.sondeapp` as an
artifact.

**Acceptance criteria:**

1. On push to `main`, the CI workflow runs to completion and produces a `.sondeapp` artifact.
2. The workflow downloads `sonde-bundle` from the sonde repo using `gh run download --repo alan-jowett/sonde`, authenticating with a token that has `actions:read` access to that repository (e.g., a PAT or GitHub App token; the default `GITHUB_TOKEN` is scoped to the current repo only).
3. BPF programs are compiled with `cmake -B build -DCMAKE_TOOLCHAIN_FILE=cmake/bpf-toolchain.cmake && cmake --build build`.
4. `sonde-bundle create .` packages the compiled programs and handler into a `.sondeapp`.  The `app.yaml` `path:` entries point to `build/bpf/<name>.o`.
5. The `.sondeapp` artifact is uploaded and downloadable from the Actions UI.
6. If `sonde-bundle create` fails validation, the workflow fails with a non-zero exit code.

---

### SB-0604  Version pinning

**Priority:** Must
**Source:** [issue #632](https://github.com/Alan-Jowett/sonde/issues/632)

**Description:**
The template repository MUST allow the developer to control which version of
`sonde-bundle` the CI workflow downloads, via a version variable.

**Acceptance criteria:**

1. A variable (e.g., `SONDE_VERSION` in the workflow file or a `.sonde-version` file) controls which CI run or release tag to download `sonde-bundle` from.
2. Changing the variable and pushing triggers a build with the new version.
3. The README documents how to update the version.

---

### SB-0605  Local build and validate

**Priority:** Should
**Source:** [issue #632](https://github.com/Alan-Jowett/sonde/issues/632)

**Description:**
The template repository SHOULD include instructions and tooling for a local
development loop: compile BPF programs with CMake, create a `.sondeapp`
bundle from the project directory, then validate that bundle with
`sonde-bundle validate`.

**Acceptance criteria:**

1. The README includes a "Local Development" section with step-by-step commands.
2. `cmake -B build -DCMAKE_TOOLCHAIN_FILE=cmake/bpf-toolchain.cmake && cmake --build build` compiles BPF programs locally.
3. The documented local workflow includes creating a `.sondeapp` archive (e.g., `sonde-bundle create .`) and validating the produced archive with `sonde-bundle validate <output>.sondeapp`.
4. If `sonde-bundle` is not installed, the README explains how to obtain it (download from CI artifacts or `cargo install`).

---

## Appendix A  Requirement index

| ID | Title | Priority |
|----|-------|----------|
| SB-0100 | Manifest parsing | Must |
| SB-0101 | Archive extraction | Must |
| SB-0102 | Schema version handling | Must |
| SB-0200 | Structural validation | Must |
| SB-0201 | Program reference validation | Must |
| SB-0202 | Handler reference validation | Must |
| SB-0203 | Node reference validation | Must |
| SB-0204 | Cross-reference validation | Should |
| SB-0300 | Create bundle from directory | Must |
| SB-0301 | Validate on create | Must |
| SB-0400 | Inspect bundle contents | Should |
| SB-0500 | `sonde-bundle create` command | Must |
| SB-0501 | `sonde-bundle validate` command | Must |
| SB-0502 | `sonde-bundle inspect` command | Should |
| SB-0600 | CI binary artifacts | Must |
| SB-0601 | Cross-platform builds | Must |
| SB-0602 | Template repo structure | Must |
| SB-0603 | Template CI workflow | Must |
| SB-0604 | Version pinning | Must |
| SB-0605 | Local build and validate | Should |
