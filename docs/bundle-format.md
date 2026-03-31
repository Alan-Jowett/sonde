<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Sonde App Bundle Format Specification

> **Document status:** Draft
> **Source:** Derived from [issue #491](https://github.com/Alan-Jowett/sonde/issues/491) and [gateway-requirements.md](gateway-requirements.md).
> **Scope:** Structure, manifest schema, and versioning of `.sondeapp` application bundles.

---

## 1  Overview

A **Sonde App Bundle** is a single, versioned artifact that contains everything
needed to deploy an application onto the Sonde platform.  It packages BPF
programs, handler executables, node targeting rules, and hardware profiles into
a self-contained archive that can be validated offline and deployed via the
`sonde-admin` CLI.

The bundle formalises the separation between the **Sonde platform** (firmware,
gateway, protocol, BPF VM) and **Sonde applications** (sensor logic, ingestion
pipeline, hardware definitions).

---

## 2  Definitions

| Term | Meaning |
|------|---------|
| **bundle** | A `.sondeapp` file — a gzipped tar archive containing a manifest and application artifacts. |
| **manifest** | The `app.yaml` file at the archive root that describes the bundle contents, targeting, and metadata. |
| **schema version** | An integer in the manifest that identifies the manifest format version. |
| **program** | A pre-compiled BPF ELF binary included in the bundle's `bpf/` directory. |
| **handler** | An external process (command + arguments) that receives `APP_DATA` from nodes via the gateway handler protocol. |
| **hardware profile** | A set of sensor descriptors (type, bus address, label) describing the physical sensors attached to a node. |
| **node target** | An entry in the manifest that maps a node name to a program assignment and optional hardware profile. |
| **deploy** | The act of ingesting programs, assigning them to nodes, and configuring handlers on a gateway from a bundle. |
| **undeploy** | The act of removing handlers, unassigning programs from nodes, and optionally deleting programs that were deployed by a bundle. |

---

## 3  Archive format

### 3.1  Container

A bundle is a **gzipped tar archive** (`.tgz`) with the file extension
`.sondeapp`.  The archive MUST contain the manifest at the path `app.yaml`
relative to the archive root.

Tools MUST accept files with either `.sondeapp` or `.tgz` extension.  The
canonical extension for distribution is `.sondeapp`.

### 3.2  Directory layout

The archive MUST follow this directory structure:

```
<archive-root>/
├── app.yaml              # REQUIRED — manifest
├── bpf/                  # REQUIRED if programs are defined
│   ├── <program-1>.elf   # Pre-compiled BPF ELF binary
│   └── <program-2>.elf
└── handler/              # REQUIRED if handlers are defined
    ├── <handler-entry>   # Handler executable or script
    └── ...               # Supporting files (libraries, configs)
```

**Rules:**

- `app.yaml` MUST be at the archive root (not inside a subdirectory).
- The `bpf/` directory MUST exist if the manifest references any programs.
- The `handler/` directory MUST exist if the manifest references any handlers
  with files that need to be extracted.
- All paths in the manifest are relative to the archive root.
- Symlinks MUST NOT be followed during extraction (security constraint).
- Path traversal (`../`) in archive entries MUST be rejected.

### 3.3  Naming convention

Bundle filenames SHOULD follow the pattern:

```
<app-name>-<version>.sondeapp
```

Example: `temperature-monitor-0.1.0.sondeapp`

The filename is informational; the authoritative name and version are in the
manifest.

---

## 4  Manifest schema

The manifest is a YAML file named `app.yaml` at the archive root.

### 4.1  Top-level fields

```yaml
# Required fields
schema_version: 1                    # Integer, manifest format version
name: "temperature-monitor"          # String, application name (1-64 chars, [a-z0-9-])
version: "0.1.0"                     # String, application version (semver)

# Optional fields
description: "BME280 temperature monitoring"  # String, human-readable description (max 256 chars)

# Required sections
programs: [...]                      # List of program definitions
nodes: [...]                         # List of node targets

# Optional sections
handlers: [...]                      # List of handler definitions
```

### 4.2  Field constraints

| Field | Type | Required | Constraints |
|-------|------|----------|-------------|
| `schema_version` | integer | yes | MUST be `1` for this specification version |
| `name` | string | yes | 1–64 characters, regex `[a-z0-9]([a-z0-9-]*[a-z0-9])?` (lowercase, hyphens, no leading/trailing hyphens) |
| `version` | string | yes | Semantic versioning (MAJOR.MINOR.PATCH), e.g., `"1.2.3"` |
| `description` | string | no | Max 256 characters, UTF-8 |
| `programs` | list | yes | At least one program entry |
| `nodes` | list | yes | At least one node target |
| `handlers` | list | no | Handler definitions; if absent, no handlers are configured |

### 4.3  Program entry

Each entry in the `programs` list defines a BPF program included in the bundle.

```yaml
programs:
  - name: "temp-reader"              # String, unique within bundle
    path: "bpf/temp_reader.elf"      # String, path to ELF binary relative to archive root
    profile: "resident"              # String, "resident" or "ephemeral"
```

| Field | Type | Required | Constraints |
|-------|------|----------|-------------|
| `name` | string | yes | 1–64 characters, unique within the `programs` list, regex `[a-z0-9]([a-z0-9_-]*[a-z0-9])?` |
| `path` | string | yes | Relative path to a `.elf` file in the archive; MUST exist in the archive |
| `profile` | string | yes | `"resident"` or `"ephemeral"` |

### 4.4  Handler entry

Each entry in the `handlers` list defines a handler process to be registered
with the gateway.

```yaml
handlers:
  - program: "temp-reader"           # String, references a program name from programs list
    command: "python3"               # String, executable command
    args: ["handler/ingest.py"]      # List of strings, command arguments (optional)
    working_dir: "handler/"          # String, working directory (optional)
    reply_timeout_ms: 5000           # Integer, per-handler reply timeout (optional)
```

| Field | Type | Required | Constraints |
|-------|------|----------|-------------|
| `program` | string | yes | MUST reference a `name` in the `programs` list, or `"*"` for catch-all |
| `command` | string | yes | Executable command (resolved in PATH or relative to extraction directory) |
| `args` | list of strings | no | Command-line arguments; defaults to empty list |
| `working_dir` | string | no | Working directory for handler; relative paths resolved from extraction directory |
| `reply_timeout_ms` | integer | no | Per-handler reply timeout in milliseconds; if absent, gateway default is used |

**Notes:**
- A handler entry with `program: "*"` defines a catch-all handler (matches any
  program hash not matched by a specific handler).
- At most one catch-all handler MAY exist per bundle.
- The `command` and `args` together form the handler process invocation, matching
  the existing `sonde-admin handler add` semantics.

### 4.5  Node target entry

Each entry in the `nodes` list targets a specific node for program deployment.

```yaml
nodes:
  - name: "sensor-1"                 # String, node name/ID
    program: "temp-reader"           # String, references a program name from programs list
    hardware:                        # Optional, hardware profile
      sensors:
        - type: "i2c"               # String: "i2c", "adc", "gpio", "spi"
          id: 118                    # Integer, bus address or channel number
          label: "BME280"            # String, human-readable label (optional)
      rf_channel: 6                  # Integer, ESP-NOW RF channel 1-13 (optional)
```

| Field | Type | Required | Constraints |
|-------|------|----------|-------------|
| `name` | string | yes | Node name/ID; MUST be unique within the `nodes` list |
| `program` | string | yes | MUST reference a `name` in the `programs` list |
| `hardware` | object | no | Hardware profile for the node |
| `hardware.sensors` | list | no | List of sensor descriptors |
| `hardware.sensors[].type` | string | yes (if sensor) | One of: `"i2c"`, `"adc"`, `"gpio"`, `"spi"` |
| `hardware.sensors[].id` | integer | yes (if sensor) | Bus address or channel number, 0–65535 |
| `hardware.sensors[].label` | string | no | Human-readable label, max 64 bytes UTF-8 |
| `hardware.rf_channel` | integer | no | ESP-NOW RF channel, 1–13 |

**Notes:**
- The `hardware` section is informational for V1.  It is included in the bundle
  so that future tools (e.g., the BLE pairing tool) can use it to configure
  nodes at pairing time without requiring a bundle format change.
- The `name` field corresponds to the `node_id` used in gateway `AssignProgram`
  and other admin API calls.
- Multiple nodes MAY reference the same program.

---

## 5  Schema versioning

### 5.1  Version semantics

The `schema_version` field is an integer that identifies the manifest format.
Parsers MUST reject manifests with a `schema_version` higher than they support.

| Schema version | Meaning |
|----------------|---------|
| `1` | Initial format defined by this specification |

### 5.2  Forward compatibility

Future schema versions MAY add new optional fields to existing sections.
Parsers for schema version N SHOULD ignore unknown fields in manifests with
`schema_version` ≤ N (forward-compatible reads within the same major version).

A new schema version is required when:
- A new required field is added
- The semantics of an existing field change
- A structural change to the manifest layout occurs

### 5.3  Backward compatibility

Tools supporting schema version N MUST also accept manifests with any
`schema_version` < N, applying defaults for fields introduced in later versions.

---

## 6  Validation rules

A bundle is **valid** if and only if all of the following hold:

### 6.1  Archive validation

1. The file is a valid gzipped tar archive.
2. No archive entry contains path traversal (`../`).
3. No archive entry is a symlink.
4. `app.yaml` exists at the archive root.

### 6.2  Manifest validation

1. `app.yaml` is valid YAML and parses successfully.
2. `schema_version` is present and is a supported integer.
3. `name` matches the regex `[a-z0-9]([a-z0-9-]*[a-z0-9])?` and is 1–64 characters.
4. `version` is a valid semver string.
5. `programs` is a non-empty list.
6. `nodes` is a non-empty list.
7. `description`, if present, is at most 256 characters.

### 6.3  Program validation

For each program entry:

1. `name` is unique within the `programs` list.
2. `path` is a relative path (no leading `/`, no `../`).
3. The file at `path` exists in the archive.
4. The file at `path` has a valid ELF magic number (`\x7fELF`).
5. `profile` is `"resident"` or `"ephemeral"`.
6. `name` matches the regex `[a-z0-9]([a-z0-9_-]*[a-z0-9])?` and is 1–64 characters.

### 6.4  Handler validation

For each handler entry:

1. `program` references a valid `name` in the `programs` list, or is `"*"`.
2. At most one handler has `program: "*"`.
3. `command` is a non-empty string.
4. `reply_timeout_ms`, if present, is a positive integer.

### 6.5  Node validation

For each node target:

1. `name` is unique within the `nodes` list.
2. `program` references a valid `name` in the `programs` list.
3. `hardware.sensors[].type`, if present, is one of `"i2c"`, `"adc"`, `"gpio"`, `"spi"`.
4. `hardware.sensors[].id`, if present, is a non-negative integer in the range 0–65535.
5. `hardware.rf_channel`, if present, is between 1 and 13 inclusive.
6. `hardware.sensors[].label`, if present, is at most 64 bytes UTF-8.

### 6.6  Cross-reference validation

1. Every program referenced by a `nodes[].program` entry exists in `programs`.
2. Every program referenced by a `handlers[].program` entry exists in `programs`
   (or is `"*"`).
3. Every program in `programs` is referenced by at least one `nodes` entry
   (warning, not error — a program may be included for future use).

---

## 7  Deploy semantics

Deploying a bundle to a gateway performs the following operations in order:

### 7.1  Deploy sequence

1. **Validate** — Run all validation rules (§6).  Abort on any error.
2. **Ingest programs** — For each program in `programs`:
   - Call `IngestProgram` with the ELF binary and verification profile.
   - If the program already exists (same content hash), skip ingestion.
   - Record the mapping: program name → program hash.
3. **Configure handlers** — For each handler in `handlers`:
   - Resolve the `program` name to its content hash.
   - Call `AddHandler` with the program hash, command, args, working directory,
     and reply timeout.
   - If the handler already exists with identical configuration, skip.
4. **Assign programs to nodes** — For each node in `nodes`:
   - Resolve the `program` name to its content hash.
   - Call `AssignProgram` with the node name and program hash.
   - If the node is already assigned the same program, skip.

### 7.2  Idempotency

Deploy MUST be idempotent: deploying the same bundle twice produces the same
end state.  Specifically:

- Programs with identical content hashes are not re-ingested.
- Handlers with identical configurations are not re-added.
- Node assignments that already match are not re-issued.

### 7.3  Error handling

If any step fails:

- The deploy command MUST report which step failed and why.
- Steps that completed successfully before the failure are NOT rolled back
  (partial deploy is the expected state on failure).
- The user can re-run deploy after fixing the issue; idempotency ensures
  already-completed steps are skipped.

### 7.4  Dry-run mode

The deploy command SHOULD support a `--dry-run` flag that:

- Validates the bundle.
- Resolves all program names to content hashes (by computing SHA-256 of the
  CBOR image that would result from ELF ingestion).
- Reports what actions would be taken without executing them.

---

## 8  Undeploy semantics

Undeploying a bundle reverses the effects of a deploy.

### 8.1  Undeploy sequence

1. **Parse manifest** — Extract the manifest from the bundle.
2. **Remove handlers** — For each handler in `handlers`:
   - Resolve the `program` name to its content hash (by ingesting the ELF
     to compute the hash, without storing).
   - Call `RemoveHandler` with the program hash.
   - If the handler does not exist, skip.
3. **Unassign programs from nodes** — For each node in `nodes`:
   - The undeploy command does NOT automatically unassign programs from nodes,
     because the node may have been reassigned to a different program since
     deploy.  The command SHOULD warn the user about nodes that are still
     assigned to bundle programs.
4. **Remove programs** — For each program in `programs`:
   - If `--remove-programs` flag is set, call `RemoveProgram` with the
     program hash.
   - If the program is still assigned to any node, skip and warn.

### 8.2  Safety

- Undeploy MUST NOT remove programs that are assigned to nodes unless the
  user explicitly requests it (`--force`).
- Undeploy MUST NOT affect nodes, programs, or handlers not defined in the
  bundle.

---

## 9  Complete manifest example

```yaml
schema_version: 1
name: "temperature-monitor"
version: "0.1.0"
description: "BME280 temperature monitoring application"

programs:
  - name: "temp-reader"
    path: "bpf/temp_reader.elf"
    profile: "resident"

handlers:
  - program: "temp-reader"
    command: "python3"
    args: ["handler/ingest.py"]
    working_dir: "handler/"
    reply_timeout_ms: 5000

nodes:
  - name: "greenhouse-1"
    program: "temp-reader"
    hardware:
      sensors:
        - type: "i2c"
          id: 118
          label: "BME280"
      rf_channel: 6

  - name: "greenhouse-2"
    program: "temp-reader"
    hardware:
      sensors:
        - type: "i2c"
          id: 118
          label: "BME280"
      rf_channel: 6
```

---

## Revision history

| Version | Date | Author | Changes |
|---------|------|--------|---------|
| 0.1 | 2026-03-31 | sonde contributors | Initial draft from issue #491 |
