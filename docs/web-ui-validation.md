<!-- SPDX-License-Identifier: MIT -->
<!-- Copyright (c) 2026 sonde contributors -->
# Sonde Web UI — Validation

> **Document status:** Draft
> **Scope:** Validation plan and traceable test matrix for the Sonde Web UI.
> **Audience:** Implementers (human or LLM agent) writing web UI, function, integration, and infrastructure tests.
> **Related:** [web-ui-requirements.md](web-ui-requirements.md), [web-ui-design.md](web-ui-design.md)

---

## 1  Overview

This document defines the validation plan for the Sonde Web UI SPA. Every test
case traces to a requirement in [web-ui-requirements.md](web-ui-requirements.md)
and verifies one or more acceptance criteria.

---

## 2  Scope of Validation

### In scope

- SPA front-end behavior (all tabs)
- Environment manager
- Authentication flows (MSAL.js)
- Program upload flow (SPA → ProgramIngest)
- Infrastructure provisioning (Bicep, CORS, EasyAuth, GitHub Pages)
- Cross-cutting concerns (escaping, MSAL hash routing, popup detection)

### Out of scope

- Azure handler function internals beyond ProgramIngest (covered by azure-handler-validation.md)
- Gateway, node, or modem behavior
- BPF compilation / Prevail verifier internals

---

## 3  Test Strategy

| Level | Scope | Tools |
|-------|-------|-------|
| Unit (JS) | Pure functions (RowKey, PartitionKey, escapeHtml) | Browser test runner or Node.js |
| Unit (Rust) | ProgramIngest handler logic | `cargo test` |
| Integration | Azure Table read/write, EasyAuth token validation | Live Azure environment |
| Manual/E2E | Full SPA workflows in browser | Manual testing against live environment |
| Infrastructure | Bicep deployment verification | Azure CLI / portal inspection |

---

## 4  Requirements Traceability Matrix

| Requirement | Test Cases |
|-------------|------------|
| WEB-0101 | T-WEB-0101, T-WEB-0101b, T-WEB-0101c, T-WEB-0727, T-WEB-0734 |
| WEB-0102 | T-WEB-0102, T-WEB-0102b, T-WEB-0102c |
| WEB-0103 | T-WEB-0103, T-WEB-0103b, T-WEB-0728, T-WEB-0734 |
| WEB-0104 | T-WEB-0104, T-WEB-0105, T-WEB-0106, T-WEB-0107, T-WEB-0108 |
| WEB-0105 | T-WEB-0109, T-WEB-0110, T-WEB-0111, T-WEB-0112, T-WEB-0113, T-WEB-0114, T-WEB-0115 |
| WEB-0201 | T-WEB-0201, T-WEB-0201b, T-WEB-0201c |
| WEB-0202 | T-WEB-0202, T-WEB-0202b, T-WEB-0202c |
| WEB-0203 | T-WEB-0203, T-WEB-0203b |
| WEB-0204 | T-WEB-0204, T-WEB-0204b |
| WEB-0205 | T-WEB-0205 |
| WEB-0206 | T-WEB-0206 |
| WEB-0207 | T-WEB-0207, T-WEB-0207b |
| WEB-0301 | T-WEB-0301, T-WEB-0301b |
| WEB-0302 | T-WEB-0302 |
| WEB-0303 | T-WEB-0303, T-WEB-0303b |
| WEB-0304 | T-WEB-0304 |
| WEB-0305 | T-WEB-0305, T-WEB-0305b |
| WEB-0306 | T-WEB-0306 |
| WEB-0307 | T-WEB-0307a, T-WEB-0307b |
| WEB-0308 | T-WEB-0308 |
| WEB-0309 | T-WEB-0309, T-WEB-0309b |
| WEB-0310 | T-WEB-0310, T-WEB-0310b, T-WEB-0310c, T-WEB-0310d |
| WEB-0401 | T-WEB-0401, T-WEB-0401b, T-WEB-0401c |
| WEB-0501 | T-WEB-0501, T-WEB-0501b, T-WEB-0501c |
| WEB-0502 | T-WEB-0502, T-WEB-0502b |
| WEB-0503 | T-WEB-0503 |
| WEB-0504 | T-WEB-0504, T-WEB-0504b |
| WEB-0505 | T-WEB-0505 |
| WEB-0506 | T-WEB-0506 |
| WEB-0507 | T-WEB-0507 |
| WEB-0508 | T-WEB-0508, T-WEB-0509 |
| WEB-0602 | T-WEB-0602 |
| WEB-0603 | T-WEB-0603 |
| WEB-0604 | T-WEB-0604 |
| WEB-0605 | T-WEB-0605 |
| WEB-0606 | T-WEB-0606, T-WEB-0606b, T-WEB-0606c, T-WEB-0606d, T-WEB-0606e |
| WEB-0607 | T-WEB-0607 |
| WEB-0700 | T-WEB-0701, T-WEB-0701b, T-WEB-0735 |
| WEB-0701 | T-WEB-0702, T-WEB-0703, T-WEB-0706, T-WEB-0713, T-WEB-0714, T-WEB-0715, T-WEB-0702b, T-WEB-0703b |
| WEB-0702 | T-WEB-0704, T-WEB-0705, T-WEB-0704b, T-WEB-0704c |
| WEB-0703 | T-WEB-0707, T-WEB-0708, T-WEB-0709, T-WEB-0710, T-WEB-0711, T-WEB-0712, T-WEB-0707b |
| WEB-0705 | T-WEB-0723, T-WEB-0724, T-WEB-0725, T-WEB-0726, T-WEB-0802b |
| WEB-0706 | T-WEB-0727, T-WEB-0728, T-WEB-0729, T-WEB-0730, T-WEB-0731, T-WEB-0732, T-WEB-0733, T-WEB-0734, T-WEB-0735, T-WEB-0736 |
| WEB-0704 | T-WEB-0716, T-WEB-0717, T-WEB-0718, T-WEB-0719, T-WEB-0720, T-WEB-0721, T-WEB-0722 |
| WEB-0800 | T-WEB-0801, T-WEB-0802, T-WEB-0802b, T-WEB-0803, T-WEB-0804, T-WEB-0805, T-WEB-0806 |
| WEB-0801 | T-WEB-0802 |
| WEB-0802 | T-WEB-0806, T-WEB-0807, T-WEB-0808, T-WEB-0809 |
| WEB-0803 | T-WEB-0801, T-WEB-0801b, T-WEB-0818 |
| WEB-0804 | T-WEB-0805, T-WEB-0805b, T-WEB-0805c |
| WEB-0807 | T-WEB-0810, T-WEB-0810b, T-WEB-0810c, T-WEB-0811, T-WEB-0812, T-WEB-0813, T-WEB-0814, T-WEB-0815, T-WEB-0815b, T-WEB-0819, T-WEB-0820, T-WEB-0821, T-WEB-0822 |
| WEB-0808 | T-WEB-0816, T-WEB-0816b, T-WEB-0816c, T-WEB-0817 |
| WEB-0805 | T-WEB-0804, T-WEB-0804b |
| WEB-0806 | T-WEB-0803, T-WEB-0803b, T-WEB-0803c, T-WEB-0803d, T-WEB-0803d2, T-WEB-0803e, T-WEB-0803f, T-WEB-0803g, T-WEB-0803h |
| WEB-0901 | T-WEB-0901, T-WEB-0901b, T-WEB-0901c |
| WEB-0902 | T-WEB-0902, T-WEB-0902b |
| WEB-1001 | T-WEB-1001, T-WEB-1001b, T-WEB-1001c, T-WEB-1001d |
| WEB-1002 | T-WEB-1002, T-WEB-1002b, T-WEB-1002c, T-WEB-1002d, T-WEB-1002e |
| WEB-1003 | T-WEB-1003, T-WEB-1003b |
| WEB-1004 | T-WEB-1004 |
| WEB-1005 | T-WEB-1005 |
| WEB-1006 | ~~T-WEB-1006, T-WEB-1006b~~ (retired — Issue #1092) |
| WEB-1007 | T-WEB-1007, T-WEB-1007b |
| WEB-1008 | T-WEB-1008 |
| WEB-1009 | T-WEB-1009, T-WEB-1009b, T-WEB-1009c, T-WEB-1009d, T-WEB-1009e, T-WEB-1009f, T-WEB-1009g, T-WEB-1009h, T-WEB-1009i, T-WEB-1009j, T-WEB-1009k |
| WEB-1100 | T-WEB-1100, T-WEB-1101, T-WEB-1102, T-WEB-1103, T-WEB-1104, T-WEB-1104b |
| WEB-1101 | T-WEB-1105, T-WEB-1106, T-WEB-1107, T-WEB-1108, T-WEB-1109, T-WEB-1109c |
| WEB-1102 | T-WEB-1110, T-WEB-1111, T-WEB-1111b, T-WEB-1112, T-WEB-1113, T-WEB-1114 |
| WEB-1103 | T-WEB-1115, T-WEB-1116, T-WEB-1117, T-WEB-1118, T-WEB-1118b |
| WEB-1104 | T-WEB-1119, T-WEB-1120, T-WEB-1120b, T-WEB-1120c, T-WEB-1120d, T-WEB-1120e, T-WEB-0733, T-WEB-0736 |
| WEB-1105 | T-WEB-1121, T-WEB-1122, T-WEB-1123, T-WEB-1124 |
| WEB-1106 | T-WEB-1125, T-WEB-1126, T-WEB-1127, T-WEB-1127b, T-WEB-1127c |
| WEB-1107 | T-WEB-1128, T-WEB-1129, T-WEB-1130, T-WEB-1130b, T-WEB-1130c |
| WEB-1108 | T-WEB-1131, T-WEB-1132 |
| WEB-1109 | T-WEB-1109a, T-WEB-1109b |
| WEB-1110 | T-WEB-1110a, T-WEB-1110b |
| WEB-CC-01 | T-WEB-CC-01, T-WEB-CC-01b |
| WEB-CC-02 | T-WEB-CC-02, T-WEB-CC-02b |
| WEB-CC-03 | T-WEB-CC-03, T-WEB-CC-03b |
| WEB-CC-04 | T-WEB-CC-04, T-WEB-CC-04b |

---

## 5  Test Cases

### 5.1  Node Dashboard

| Test ID | Requirement | Description | Method | Status |
|---|---|---|---|---|
| T-WEB-0101 | WEB-0101 | SPA renders node table from `actualstate` query | Manual/E2E | Planned |
| T-WEB-0101b | WEB-0101 | Nodes sorted alphabetically by `node_id` | Manual/E2E | Planned |
| T-WEB-0101c | WEB-0101 | Empty `actualstate` table displays "No node state found." | Manual/E2E | Planned |
| T-WEB-0102 | WEB-0102 | All ten required columns displayed | Manual/E2E | Planned |
| T-WEB-0102b | WEB-0102 | Program hashes truncated to 8-char hex with full-hash tooltip | Manual/E2E | Planned |
| T-WEB-0102c | WEB-0102 | "Last Seen" shows relative time (e.g., "5m ago") | Manual/E2E | Planned |
| T-WEB-0103 | WEB-0103 | Auto-refresh polls at configured interval (30s default) | Manual | Planned |
| T-WEB-0103b | WEB-0103 | Auto-refresh cancelled when navigating to a different tab | Manual | Planned |
| T-WEB-0104 | WEB-0104 | Divergence indicator shown when desired-state row exists and actual differs from desired | Manual/E2E | Planned |
| T-WEB-0105 | WEB-0104 | Unassigned program (desired row exists, program hash empty/missing) shows Diverged while node still reports a current program | Manual/E2E | Planned |
| T-WEB-0106 | WEB-0104 | Node with no desired-state row shows Aligned even if actual state reports a program | Manual/E2E | Planned |
| T-WEB-0109 | WEB-0105 | Dashboard exposes device-data export start/end controls, format selector, and Export action | Manual/E2E | Planned |
| T-WEB-0110 | WEB-0105 | JSONL device-data export writes one JSON object per line with the required historical actual-state fields and `null` for missing optional values | Manual/E2E | Planned |
| T-WEB-0111 | WEB-0105 | CSV device-data export writes the required header and empty fields for missing optional values | Manual/E2E | Planned |
| T-WEB-0112 | WEB-0105 | Device-data export includes multiple historical `actualstate` rows for the same node within the selected range, not just the dashboard's latest row | Manual/E2E | Planned |
| T-WEB-0113 | WEB-0105 | Device-data export follows Azure Table continuation tokens so ranges with more than 1000 rows per node export completely | Unit (JS) | Planned |
| T-WEB-0114 | WEB-0105 | Missing or inverted device-data export start/end values are rejected with operator-visible feedback and no file download | Manual | Planned |
| T-WEB-0115 | WEB-0105 | Device-data export includes matching rows from multiple known node partitions within the selected range and is unaffected by dashboard auto-refresh | Manual/E2E | Planned |

### 5.2  Desired State

| Test ID | Requirement | Description | Method | Status |
|---|---|---|---|---|
| T-WEB-0201 | WEB-0201 | Set desired schedule writes correct row with `Edm.Int32` type | Integration | Planned |
| T-WEB-0201b | WEB-0201 | Schedule input enforces min=1 and step=1 | Manual | Planned |
| T-WEB-0201c | WEB-0201 | Empty schedule field omits `desired_schedule_interval_s` from entity | Integration | Planned |
| T-WEB-0202 | WEB-0202 | Assign program hash writes correct row with lowercase hex | Integration | Planned |
| T-WEB-0202b | WEB-0202 | Program Hash field is a `<select>` dropdown populated from `programs` table | Manual/E2E | Planned |
| T-WEB-0202c | WEB-0202 | Selecting "No program target" omits `desired_assigned_program_hash` from entity | Integration | Planned |
| T-WEB-0203 | WEB-0203 | RowKey uses reverse-timestamp format (3 colon-separated 16-char hex segments) | Unit (JS) | Planned |
| T-WEB-0203b | WEB-0203 | First RowKey segment is bitwise complement of current `timestamp_ms` | Unit (JS) | Planned |
| T-WEB-0204 | WEB-0204 | PartitionKey = `n:{SHA-256(node_id)}` via SubtleCrypto | Unit (JS) | Planned |
| T-WEB-0204b | WEB-0204 | SHA-256 hash input is the UTF-8 encoding of `node_id` | Unit (JS) | Planned |
| T-WEB-0205 | WEB-0205 | `timestamp_ms` stored as `Edm.Int64` | Integration | Planned |
| T-WEB-0206 | WEB-0206 | Node ID field is a dropdown-only control populated from latest `actualstate` nodes; arbitrary node IDs cannot be entered or submitted | Manual/E2E | Planned |
| T-WEB-0207 | WEB-0207 | Selecting a node pre-populates Schedule and Program Hash (desired state preferred over actual state) | Manual/E2E | Planned |
| T-WEB-0207b | WEB-0207 | Pre-populated hash not in dropdown defaults to "No program target" | Manual/E2E | Planned |

### 5.3  Program Ingest

| Test ID | Requirement | Description | Method | Status |
|---|---|---|---|---|
| T-WEB-0301 | WEB-0301 | `ProgramIngest` accepts ELF + metadata via JSON POST | Unit (Rust) | Pass |
| T-WEB-0301b | WEB-0301 | Missing `elf` field returns HTTP 400 | Unit (Rust) | Pass |
| T-WEB-0302 | WEB-0302 | Prevail verification runs; invalid ELF rejected | Unit (Rust) | Pass |
| T-WEB-0303 | WEB-0303 | Program hash matches gateway computation | Unit (Rust) | Pass |
| T-WEB-0303b | WEB-0303 | Re-ingesting same ELF produces same hash (idempotent) | Unit (Rust) | Planned |
| T-WEB-0304 | WEB-0304 | Program stored in `programs` table with all fields | Integration | Planned |
| T-WEB-0305 | WEB-0305 | Success returns hash+metadata; failure returns diagnostics | Unit (Rust) | Pass |
| T-WEB-0305b | WEB-0305 | Failure returns JSON error with appropriate HTTP status code | Unit (Rust) | Planned |
| T-WEB-0306 | WEB-0306 | Oversized programs rejected (>1 MB → HTTP 413) | Unit (Rust) | Pass |
| T-WEB-0307a | WEB-0307 | Empty ELF rejected | Unit (Rust) | Pass |
| T-WEB-0307b | WEB-0307 | Multi-program ELF rejected | Unit (Rust) | Planned |
| T-WEB-0308 | WEB-0308 | `source_filename` normalized to basename | Unit (Rust) | Pass |
| T-WEB-0309 | WEB-0309 | `DESIRED_STATE` includes inline ELF on program divergence | Unit (Rust) | Planned |
| T-WEB-0309b | WEB-0309 | Gateway receives inline ELF and runs Prevail re-verification | Integration | Planned |
| T-WEB-0310 | WEB-0310 | `DESIRED_STATE` carries key 5 with ELF bytes and keys 6–8 with metadata | Unit (Rust) | Planned |
| T-WEB-0310b | WEB-0310 | Key 6 carries verification profile value | Unit (Rust) | Planned |
| T-WEB-0310c | WEB-0310 | Key 7 carries source filename | Unit (Rust) | Planned |
| T-WEB-0310d | WEB-0310 | Key 8 carries ABI version | Unit (Rust) | Planned |

### 5.4  Program List

| Test ID | Requirement | Description | Method | Status |
|---|---|---|---|---|
| T-WEB-0401 | WEB-0401 | SPA lists programs from `programs` table with correct columns | Manual/E2E | Planned |
| T-WEB-0401b | WEB-0401 | Programs sorted by `created_at` descending (newest first) | Manual/E2E | Planned |
| T-WEB-0401c | WEB-0401 | Empty programs table displays "No programs found." | Manual/E2E | Planned |

### 5.5  Authentication

| Test ID | Requirement | Description | Method | Status |
|---|---|---|---|---|
| T-WEB-0501 | WEB-0501 | MSAL.js login flow works (popup, PKCE) | Manual | Planned |
| T-WEB-0501b | WEB-0501 | Token cache uses `sessionStorage` | Manual | Planned |
| T-WEB-0501c | WEB-0501 | Active account tracked and displayed in header | Manual | Planned |
| T-WEB-0502 | WEB-0502 | Storage API calls include bearer token | Integration | Planned |
| T-WEB-0502b | WEB-0502 | `acquireTokenSilent` attempted first; falls back to `acquireTokenPopup` | Manual | Planned |
| T-WEB-0503 | WEB-0503 | `ProgramIngest` rejects unauthenticated requests (EasyAuth returns 401) | Integration | Planned |
| T-WEB-0504 | WEB-0504 | SPA acquires Function App-scoped token for `ProgramIngest` calls | Manual | Planned |
| T-WEB-0504b | WEB-0504 | Bearer token sent on `POST /api/programs/ingest` | Manual | Planned |
| T-WEB-0505 | WEB-0505 | `ProgramIngest` rejects Storage-scoped token (wrong audience) | Integration | Planned |
| T-WEB-0506 | WEB-0506 | `ProgramIngest` rejects expired bearer token | Integration | Planned |
| T-WEB-0507 | WEB-0507 | `ProgramIngest` accepts valid `api://<clientId>/user_impersonation` token | Integration | Planned |
| T-WEB-0508 | WEB-0508 | MSAL `redirectUri` correctly set for GitHub Pages and custom domain | Manual | Planned |

### 5.6  Infrastructure

| Test ID | Requirement | Description | Method | Status |
|---|---|---|---|---|
| T-WEB-0602 | WEB-0602 | Bicep provisions `programs` table | Infrastructure | Planned |
| T-WEB-0603 | WEB-0603 | `ProgramIngest` HTTP trigger deployed alongside `UpstreamConnector` | Infrastructure | Planned |
| T-WEB-0604 | WEB-0604 | CORS configured for GitHub Pages and `sondeplatform.com` origins | Infrastructure | Planned |
| T-WEB-0605 | WEB-0605 | Function identity has table contributor on `programs` | Infrastructure | Planned |
| T-WEB-0606 | WEB-0606 | EasyAuth configured on Function App with Entra ID provider | Infrastructure | Planned |
| T-WEB-0606b | WEB-0606 | `platform.enabled` is `true` | Infrastructure | Planned |
| T-WEB-0606c | WEB-0606 | `unauthenticatedClientAction` is `Return401` | Infrastructure | Planned |
| T-WEB-0606d | WEB-0606 | `azureActiveDirectory.enabled` is `true` | Infrastructure | Planned |
| T-WEB-0606e | WEB-0606 | `allowedAudiences` includes both `api://<clientId>` and bare `<clientId>` | Infrastructure | Planned |
| T-WEB-0607 | WEB-0607 | `ProgramIngest` `authLevel` is `anonymous` (auth delegated to EasyAuth) | Infrastructure | Planned |

### 5.7  Sensor Data

| Test ID | Requirement | Description | Method | Status |
|---|---|---|---|---|
| T-WEB-0701 | WEB-0700 | Sensor Data tab appears in tab bar and loads data from `sensordata` table | Manual/E2E | Planned |
| T-WEB-0701b | WEB-0700 | Loading indicator displayed while fetching sensor data | Manual/E2E | Planned |
| T-WEB-0702 | WEB-0701 | Time-series graph renders lines per (node, program, reading) | Manual/E2E | Planned |
| T-WEB-0702b | WEB-0701 | X-axis is time (from `timestamp_ms`); Y-axis is reading value | Manual/E2E | Planned |
| T-WEB-0703 | WEB-0701 | Time range selector filters data correctly | Manual/E2E | Planned |
| T-WEB-0703b | WEB-0701 | Default time range is 24h | Manual/E2E | Planned |
| T-WEB-0704 | WEB-0702 | Table view shows all sensor data columns | Manual/E2E | Planned |
| T-WEB-0704b | WEB-0702 | Sensor data table sorted by timestamp descending | Manual/E2E | Planned |
| T-WEB-0704c | WEB-0702 | Raw Payload truncated to 40 characters with full-value tooltip | Manual/E2E | Planned |
| T-WEB-0705 | WEB-0702 | SPA handles empty `decoded_readings` gracefully (shows "—") | Manual/E2E | Planned |
| T-WEB-0706 | WEB-0701 | SPA displays string-encoded int64 values (above `Number.MAX_SAFE_INTEGER`) correctly and renders in-range values as numbers | Manual | Planned |
| T-WEB-0707 | WEB-0703 | Series edit dialog opens when ✏️ button is clicked and pre-fills saved overrides | Manual | Planned |
| T-WEB-0707b | WEB-0703 | Series edit dialog has focus trapping and closes on Escape | Manual | Planned |
| T-WEB-0708 | WEB-0703 | Custom display name replaces default label in series picker, chart legend, and tooltip | Manual | Planned |
| T-WEB-0709 | WEB-0703 | Scale divisor transforms plotted values (e.g., 1000 converts 21500 → 21.5) | Manual | Planned |
| T-WEB-0710 | WEB-0703 | Unit suffix appears in tooltip values and Y-axis title when all series share the same suffix | Manual | Planned |
| T-WEB-0711 | WEB-0703 | Per-series display overrides persist with the active environment across page reloads | Manual | Planned |
| T-WEB-0712 | WEB-0703 | Reset to Default clears overrides and restores original label/scale | Manual | Planned |
| T-WEB-0716 | WEB-0704 | Sensor Data tab exposes export start/end controls, format selector, and Export action | Manual/E2E | Planned |
| T-WEB-0717 | WEB-0704 | JSONL export writes one JSON object per line with the required five fields and `decoded_readings: null` for empty rows | Manual/E2E | Planned |
| T-WEB-0718 | WEB-0704 | CSV export writes the required header and preserves the raw JSON string in `decoded_readings_json` | Manual/E2E | Planned |
| T-WEB-0719 | WEB-0704 | Export obeys the custom export range but ignores graph preset range, current view mode, and series selection | Manual/E2E | Planned |
| T-WEB-0720 | WEB-0704 | Export follows Azure Table continuation tokens so ranges with more than 1000 rows per node export completely | Unit (JS) | Pass |
| T-WEB-0721 | WEB-0704 | Missing or inverted export start/end values are rejected with operator-visible feedback and no file download | Manual | Planned |
| T-WEB-0722 | WEB-0704 | Export includes matching rows from multiple known node partitions within the selected range | Manual/E2E | Planned |
| T-WEB-0723 | WEB-0705 | Graph/table mode and preset Sensor Data time range persist with the environment across reload and export/import | Manual | Planned |
| T-WEB-0724 | WEB-0705 | Selected series are environment-scoped and unavailable saved series are ignored without error when rendering | Unit (JS) | Pass |
| T-WEB-0725 | WEB-0705 | Export/import preserves explicit empty `selectedSeries: []` while treating omitted `selectedSeries` as the default initial auto-selection behavior | Unit (JS) | Pass |
| T-WEB-0726 | WEB-0705 | Environment activation clears transient Sensor Data export state and auto-refresh instead of carrying it across environments | Unit (JS) | Pass |
| T-WEB-0727 | WEB-0706 | Dashboard, Desired State node discovery, Sensor Data, and Dashboards reuse cached `actualstate` rows within one session instead of repeating unchanged table fetches | Unit (JS) | Pass |
| T-WEB-0728 | WEB-0706 | When the requested range is already covered, refresh fetches only rows newer than the cached watermark and merges them without duplicates | Unit (JS) | Not Started |
| T-WEB-0729 | WEB-0706 | Expanding a requested time range fetches only the uncovered older interval and preserves already cached newer rows | Unit (JS) | Pass |
| T-WEB-0730 | WEB-0706 | A newly reporting node whose `actualstate` row arrives after the watermark is discovered by delta refresh and becomes visible to downstream views in the same session | Integration | Not Started |
| T-WEB-0731 | WEB-0706 | Session telemetry cache is not written to `localStorage` and is excluded from environment export/import data | Unit (JS) | Not Started |
| T-WEB-0732 | WEB-0706 | Switching environments clears or isolates session telemetry cache entries before the next environment renders | Unit (JS) | Pass |
| T-WEB-0733 | WEB-0706 | Multiple dashboard metrics with overlapping variable sources reuse one cached `sensordata` fetch result per partition for a render/time-range context | Unit (JS) | Pass |
| T-WEB-0734 | WEB-0706 | Dashboard cold-session node-state loads and identical dashboard auto-refresh `actualstate` reads in one environment share a single in-flight Azure Table request during hydration and during identical delta refreshes | Unit (JS) | Pass |
| T-WEB-0735 | WEB-0706 | Concurrent identical `sensordata` consumers for one partition/range/options share a single in-flight Azure Table request during cold-session fetch | Unit (JS) | Pass |
| T-WEB-0736 | WEB-0706 | Multiple dashboard metric consumers that request the same cold-session telemetry scope before the first fetch resolves observe one shared in-flight request and receive identical results | Unit (JS) | Pass |

### 5.8  Environment Manager

| Test ID | Requirement | Description | Method | Status |
|---|---|---|---|---|
| T-WEB-0801 | WEB-0803 | First load with no environments shows full-screen setup modal; main UI is inaccessible | Manual | Planned |
| T-WEB-0801b | WEB-0803 | Setup modal cannot be closed without adding an environment (no Close button) | Manual | Planned |
| T-WEB-0802 | WEB-0801 | Adding an environment persists all fields to `localStorage` under `sonde_environments` | Manual | Planned |
| T-WEB-0802b | WEB-0800 | First run after upgrade migrates legacy `sonde_series_overrides` into the active environment's Sensor Data preferences | Manual | Planned |
| T-WEB-0803 | WEB-0806 | Switching environment re-initializes MSAL, clears session, and refreshes active tab | Manual | Planned |
| T-WEB-0803b | WEB-0806 | Auto-refresh timer cleared on environment switch | Manual | Planned |
| T-WEB-0803c | WEB-0806 | `CONFIG` fields updated from selected environment | Manual | Planned |
| T-WEB-0803d | WEB-0806 | MSAL instance discarded on environment switch | Manual | Planned |
| T-WEB-0803d2 | WEB-0806 | New MSAL instance initialized with new environment credentials | Manual | Planned |
| T-WEB-0803e | WEB-0806 | Active MSAL account cleared on switch | Manual | Planned |
| T-WEB-0803f | WEB-0806 | Only MSAL-related `sessionStorage` keys cleared (other session data preserved) | Manual | Planned |
| T-WEB-0803g | WEB-0806 | Active tab re-rendered after switch | Manual | Planned |
| T-WEB-0803h | WEB-0806 | Switching environments loads that environment's saved Sensor Data preferences before the Sensor Data tab re-renders | Unit (JS) | Pass |
| T-WEB-0804 | WEB-0805 | Active environment name displayed in header bar | Manual | Planned |
| T-WEB-0804b | WEB-0805 | Environment indicator updates when environment is switched | Manual | Planned |
| T-WEB-0805 | WEB-0804 | Edit and delete operations on environments work correctly | Manual | Planned |
| T-WEB-0805b | WEB-0804 | Deleting active environment switches to next available or shows setup modal | Manual | Planned |
| T-WEB-0805c | WEB-0804 | Each environment row shows Use, Export, Edit, and Delete buttons | Manual | Planned |
| T-WEB-0806 | WEB-0802 | Environment fields validated (all required, duplicate name rejected) | Manual | Planned |

### 5.8b  Import / Export

| Test ID | Requirement | Description | Method | Status |
|---|---|---|---|---|
| T-WEB-0810 | WEB-0807 | Importing a valid `.json` file with all required connection fields adds the environment to the list | Manual | Planned |
| T-WEB-0810b | WEB-0807 | Importing a valid `sensorData` object restores view mode, preset time range, selected series, and per-series overrides | Manual | Planned |
| T-WEB-0810c | WEB-0807 | Importing a legacy environment file without `sensorData` succeeds and initializes default Sensor Data preferences | Manual | Planned |
| T-WEB-0811 | WEB-0807 | Importing a file with blank `name` prompts user for a name before saving | Manual | Planned |
| T-WEB-0812 | WEB-0807 | Importing a file with a name that conflicts with an existing environment offers overwrite or rename | Manual | Planned |
| T-WEB-0813 | WEB-0807 | Importing a file with `version` other than `1` (missing, zero, greater) is rejected with error | Manual | Planned |
| T-WEB-0814 | WEB-0807, WEB-0802 | Importing a file with invalid field values (bad GUID, wrong storage account format) is rejected | Manual | Planned |
| T-WEB-0815 | WEB-0807 | Overwriting the active environment via import triggers MSAL re-initialization | Manual | Planned |
| T-WEB-0815b | WEB-0807 | Import overwrite replaces, rather than merges, the destination environment's saved Sensor Data preferences | Manual | Planned |
| T-WEB-0816 | WEB-0808 | Export button downloads a `.json` file with `version: 1`, the five environment fields, and the `sensorData` object | Manual | Planned |
| T-WEB-0816b | WEB-0808 | Export of an environment with unsafe filename characters (slashes, colons) produces a sanitized filename | Manual | Planned |
| T-WEB-0816c | WEB-0808 | Export of an environment whose sanitized name is empty uses fallback `sonde-environment.json` | Manual | Planned |
| T-WEB-0817 | WEB-0807, WEB-0808 | Exported file round-trips through import: export → import into fresh browser → environment connection fields and Sensor Data preferences are preserved | Manual | Planned |
| T-WEB-0818 | WEB-0803 | First-load setup modal includes Import button and accepts environment file | Manual | Planned |
| T-WEB-0819 | WEB-0807 | Importing a non-JSON file (e.g., plain text, binary) is rejected with a descriptive error | Manual | Planned |
| T-WEB-0820 | WEB-0807 | Importing a JSON file with top-level array, null, or string is rejected | Manual | Planned |
| T-WEB-0821 | WEB-0807 | Importing a JSON file with missing required data fields is rejected | Manual | Planned |
| T-WEB-0822 | WEB-0807 | Extra JSON properties in the import file are silently ignored | Manual | Planned |

### 5.9  Deployment

| Test ID | Requirement | Description | Method | Status |
|---|---|---|---|---|
| T-WEB-0901 | WEB-0901 | GitHub Actions workflow deploys `deploy/web-ui/` to GitHub Pages on push to main | Infrastructure | Planned |
| T-WEB-0901b | WEB-0901 | Workflow uses `actions/upload-pages-artifact` and `actions/deploy-pages` | Infrastructure | Planned |
| T-WEB-0901c | WEB-0901 | Manual trigger (`workflow_dispatch`) is supported | Infrastructure | Planned |
| T-WEB-0902 | WEB-0902 | Bicep includes GitHub Pages and `sondeplatform.com` in CORS origins and SPA redirect URIs | Infrastructure | Planned |
| T-WEB-0902b | WEB-0902 | CORS origins parameterized via Bicep parameters | Infrastructure | Planned |
| T-WEB-0902c | WEB-0902 | Bootstrap script defaults custom domain origin to `https://sondeplatform.com` when `SONDE_AZURE_CUSTOM_DOMAIN_ORIGIN` is unset | Inspection | Planned |

### 5.10  Cross-Cutting

| Test ID | Requirement | Description | Method | Status |
|---|---|---|---|---|
| T-WEB-CC-01 | WEB-CC-01 | `deploy/web-ui/` contains only `.html`, `.js`, `.css`, `.json` — no build tools or `node_modules` | Inspection | Planned |
| T-WEB-CC-01b | WEB-CC-01 | External libraries (MSAL.js, Chart.js) loaded via CDN `<script>` tags | Inspection | Planned |
| T-WEB-CC-02 | WEB-CC-02 | All dynamic values in rendered HTML pass through `escapeHtml()` — no raw insertion of user/server data | Code review | Planned |
| T-WEB-CC-02b | WEB-CC-02 | `escapeHtml()` escapes all five characters: `escapeHtml('&<>"\'')` returns `'&amp;&lt;&gt;&quot;&#39;'` with no double-encoding (e.g., no `&amp;lt;`) | Unit (JS) | Planned |
| T-WEB-CC-03 | WEB-CC-03 | Non-auth hashes temporarily cleared before MSAL init; restored after | Manual | Planned |
| T-WEB-CC-03b | WEB-CC-03 | Auth hashes (containing `code=`, `error=`, `access_token=`) preserved for MSAL | Manual | Planned |
| T-WEB-CC-04 | WEB-CC-04 | SPA loaded inside MSAL popup does not call `init()` | Code review | Planned |
| T-WEB-CC-04b | WEB-CC-04 | Popup detection uses `window.opener && window.opener !== window` check | Code review | Planned |

### 5.11  Divergence Edge Cases

| Test ID | Requirement | Description | Method | Status |
|---|---|---|---|---|
| T-WEB-0107 | WEB-0104 | Desired schedule differs from observed schedule → shows "Diverged" | Manual/E2E | Planned |
| T-WEB-0108 | WEB-0104 | Schedule column tooltip shows both observed and desired values | Manual/E2E | Planned |

### 5.12  Graph Limits

| Test ID | Requirement | Description | Method | Status |
|---|---|---|---|---|
| T-WEB-0713 | WEB-0701 | Graph enforces 20-series maximum; excess series available via picker | Manual | Planned |
| T-WEB-0714 | WEB-0701 | Data downsampled to ≤500 points per series | Manual | Planned |
| T-WEB-0715 | WEB-0701 | Hover tooltip shows timestamp, series label, and value | Manual | Planned |

### 5.13  Environment Validation

| Test ID | Requirement | Description | Method | Status |
|---|---|---|---|---|
| T-WEB-0807 | WEB-0802 | Client ID and Tenant ID must be valid GUIDs | Manual | Planned |
| T-WEB-0808 | WEB-0802 | Storage Account validated as 3–24 lowercase alphanumeric | Manual | Planned |
| T-WEB-0809 | WEB-0802 | Function App Name validated as 2–60 alphanumeric with hyphens | Manual | Planned |

### 5.14  Redirect URI Edge Case

| Test ID | Requirement | Description | Method | Status |
|---|---|---|---|---|
| T-WEB-0509 | WEB-0508 | URL with filename (e.g., `/sonde/index.html`) produces redirect URI with directory path (`/sonde/`) | Manual | Planned |

### 5.15  Key Management

| Test ID | Requirement | Description | Method | Status |
|---|---|---|---|---|
| T-WEB-1001 | WEB-1001 | Gateway status card displays all required fields including convergence badge | Manual/E2E | Planned |
| T-WEB-1001b | WEB-1001 | Gateway row is not shown in the node table | Manual | Planned |
| T-WEB-1001c | WEB-1001 | Fingerprint computed locally from x25519_public_key, not read from stored fingerprint_words | Unit JS | Planned |
| T-WEB-1001d | WEB-1001 | Gateway status card shows convergence badge (Aligned or Diverged) | Manual/E2E | Planned |
| T-WEB-1002 | WEB-1002 | Inline rotation form opens within gateway card and validates inputs | Manual | Planned |
| T-WEB-1002b | WEB-1002 | Rotation code input is normalized to uppercase | Manual | Planned |
| T-WEB-1002c | WEB-1002 | Rotation is disabled on unsupported browsers | Manual | Planned |
| T-WEB-1002d | WEB-1002 | After submission, form shows success message and collapses (no inline polling) | Manual | Planned |
| T-WEB-1002e | WEB-1002 | Dashboard auto-refresh is paused while rotation form is expanded | Manual | Planned |
| T-WEB-1003 | WEB-1003 | Argon2id key derivation produces the correct output | Unit (JS) | Planned |
| T-WEB-1003b | WEB-1003 | KDF params from ACTUAL_STATE are used when present | Unit (JS) | Planned |
| T-WEB-1004 | WEB-1004 | `RotationPayloadV1` construction matches the specified binary format | Unit (JS) | Planned |
| T-WEB-1005 | WEB-1005 | DESIRED_STATE row is created with the correct gateway `PartitionKey` and includes `submitted_epoch` | Integration | Planned |
| T-WEB-1006 | WEB-1006 | ~~Successful rotation is detected via `master_key_epoch` polling~~ **Retired** (Issue #1092) | — | Retired |
| T-WEB-1006b | WEB-1006 | ~~Rotation timeout is handled gracefully~~ **Retired** (Issue #1092) | — | Retired |
| T-WEB-1007 | WEB-1007 | Gateway ACTUAL_STATE is read from the Azure Table using latest-row selection (not `RowKey='state'`) | Integration | Planned |
| T-WEB-1007b | WEB-1007 | Missing gateway row shows a `No gateway connected` message | Manual | Planned |
| T-WEB-1008 | WEB-1008 | No key material is written to browser storage | Manual | Planned |
| T-WEB-1009 | WEB-1009 | Gateway with no desired-state row shows "Aligned" | Manual/E2E | Planned |
| T-WEB-1009b | WEB-1009 | Gateway with pending `rotation_payload` (`actual.master_key_epoch <= desired.submitted_epoch` and `rotation_in_progress` false) shows "Diverged" | Manual/E2E | Planned |
| T-WEB-1009c | WEB-1009 | Gateway with `rotation_in_progress` true shows "Aligned" (rotation consumed) | Manual/E2E | Planned |
| T-WEB-1009d | WEB-1009 | Gateway with desired `channel` differing from actual shows "Diverged" | Manual/E2E | Planned |
| T-WEB-1009e | WEB-1009 | Gateway with desired `salt` when actual `salt` is absent shows "Diverged" | Manual/E2E | Planned |
| T-WEB-1009f | WEB-1009 | Gateway with desired `salt` when actual `salt` already exists shows "Aligned" (set-if-absent) | Manual/E2E | Planned |
| T-WEB-1009g | WEB-1009 | Gateway with desired `kdf_params` when actual is absent shows "Diverged" | Manual/E2E | Planned |
| T-WEB-1009h | WEB-1009 | Gateway with desired `kdf_params` when actual already exists shows "Aligned" (set-if-absent) | Manual/E2E | Planned |
| T-WEB-1009i | WEB-1009 | Badge uses same CSS classes as node convergence badge (`badge success`/`badge warning`) | Manual | Planned |
| T-WEB-1009j | WEB-1009 | Gateway with `master_key_epoch > submitted_epoch` shows "Aligned" (rotation consumed via epoch advance) | Manual/E2E | Planned |
| T-WEB-1009k | WEB-1009 | Matched channel, salt, kdf_params (desired == actual) shows "Aligned" | Manual/E2E | Planned |

---

## 6  Risk-Based Test Prioritization

| Risk | Impact | Likelihood | Priority | Related Tests |
|------|--------|------------|----------|---------------|
| Authentication failure blocks all functionality | High | Medium | P1 | T-WEB-0501–0509 |
| Program ingest rejects valid ELF or accepts invalid | High | Low | P1 | T-WEB-0301–0308 |
| Divergence indicator shows wrong status | Medium | Medium | P2 | T-WEB-0104–0108 |
| Gateway convergence badge shows wrong status | Medium | Medium | P2 | T-WEB-1009–1009k |
| Environment switching leaves stale tokens | Medium | Medium | P2 | T-WEB-0803 |
| Rotation key derivation timeout (Argon2id WASM slow on mobile) | Medium | Medium | P2 | T-WEB-1003 |
| XSS from unescaped user/server data | High | Low | P1 | T-WEB-CC-02 |
| Sensor data renders incorrect values | Medium | Low | P3 | T-WEB-0706, T-WEB-0709, T-WEB-0713–0715 |
| Dashboard expression injection vulnerability | High | Low | P1 | T-WEB-1111 |
| Dashboard computed metrics show incorrect values | Medium | Low | P2 | T-WEB-1118, T-WEB-1119, T-WEB-1120 |

---

## 7  Pass/Fail Criteria

- **Entry criteria:** SPA deployed to GitHub Pages; Azure environment provisioned via Bicep.
- **Exit criteria:** All P1 tests pass; no P2 tests fail with severity > minor.
- **Acceptance threshold:** 100% of Must-priority requirements have at least one passing test case.

---

### 5.11  Custom Dashboards (WEB-1100)

| Test ID | Requirement | Description | Method | Status |
|---|---|---|---|---|
| T-WEB-1100 | WEB-1100 | Dashboards tab is available in SPA navigation | Manual: Verify tab/section appears | Not Started |
| T-WEB-1101 | WEB-1100 | "+" button creates new dashboard with prompt for name | Manual: Click "+", enter name, verify dashboard created | Not Started |
| T-WEB-1102 | WEB-1100 | Dashboard tabs allow switching between dashboards | Manual: Create 2 dashboards, verify switching preserves state | Not Started |
| T-WEB-1103 | WEB-1100 | Dashboard can be renamed | Manual: Rename dashboard, verify name persisted | Not Started |
| T-WEB-1104 | WEB-1100 | Dashboard can be deleted with confirmation | Manual: Delete dashboard, verify confirmation prompt | Not Started |
| T-WEB-1104b | WEB-1100 | Empty dashboard prompts the operator to add a chart | Manual: Create dashboard with no charts, verify empty-state message | Not Started |
| T-WEB-1105 | WEB-1101 | Add variable binding with valid node ID, reading type, and variable name | Manual: Add variable, verify stored in localStorage | Not Started |
| T-WEB-1106 | WEB-1101 | Variable name validation rejects invalid JS identifiers | Unit: Test regex `/^[a-zA-Z_][a-zA-Z0-9_]*$/` with invalid inputs | Not Started |
| T-WEB-1107 | WEB-1101 | Variable name uniqueness enforced within dashboard | Manual: Add duplicate variable name, verify error | Not Started |
| T-WEB-1108 | WEB-1101 | Edit variable updates binding | Manual: Edit variable, verify change persisted | Not Started |
| T-WEB-1109 | WEB-1101 | Delete variable warns if used in metric expression | Manual: Delete variable used in metric, verify confirmation prompt | Not Started |
| T-WEB-1109c | WEB-1101 | Reserved function name rejected as variable name | Manual: Attempt to create variable named `sqrt`, verify error | Not Started |
| T-WEB-1110 | WEB-1102 | Expression editor validates syntax on blur/save | Unit: Test expr-eval parser with valid/invalid expressions | Not Started |
| T-WEB-1111 | WEB-1102 | Expression evaluator does NOT use eval() or Function() | Code review: Verify expr-eval library usage, no eval calls | Not Started |
| T-WEB-1111b | WEB-1102 | Malicious expression rejected or safely evaluated | Integration: Test `constructor.constructor('alert(1)')()`, verify no execution | Not Started |
| T-WEB-1112 | WEB-1102 | Syntax errors display inline error message | Manual: Enter `(x +`, verify error shown | Not Started |
| T-WEB-1113 | WEB-1102 | Undefined variable warning displayed | Manual: Enter expression with undefined variable, verify warning | Not Started |
| T-WEB-1114 | WEB-1102 | Supported operators and functions work correctly | Unit: Test `+`, `-`, `*`, `/`, `^`, `sqrt`, `log`, `abs`, etc. | Not Started |
| T-WEB-1115 | WEB-1103 | Add chart with a name to a dashboard | Manual: Create chart, verify card appears and persists | Not Started |
| T-WEB-1116 | WEB-1103 | Rename chart updates persisted chart name | Manual: Rename chart, reload, verify saved name | Not Started |
| T-WEB-1117 | WEB-1103 | Delete chart removes the chart and its metrics after confirmation | Manual: Delete populated chart, verify removal | Not Started |
| T-WEB-1118 | WEB-1103 | Add metric with display name and expression to a selected chart | Manual: Add metric from a chart card, verify it appears in that chart | Not Started |
| T-WEB-1118b | WEB-1103 | Edit metric can move it to a different chart | Manual: Reassign metric, verify dataset appears on new chart | Not Started |
| T-WEB-1119 | WEB-1104 | Expression `(x - 92000) / 10` with x=92500 produces 50 | Integration: Mock data, verify computed value | Not Started |
| T-WEB-1120 | WEB-1104 | Expression `sqrt(T * T + H * H)` with T=3, H=4 produces 5 | Integration: Mock data, verify computed value | Not Started |
| T-WEB-1120b | WEB-1104 | Time-series evaluation creates datasets with correct points on the assigned chart | Integration: Mock time-series data, verify chart datasets | Not Started |
| T-WEB-1120c | WEB-1104 | Multiple metrics assigned to the same chart render on one shared graph | Integration: Add two metrics to one chart, verify one canvas with two datasets | Not Started |
| T-WEB-1120d | WEB-1104 | Multiple charts in one dashboard render as separate graphs sharing one dashboard time range | Integration: Add two charts, verify two canvases and shared range controls | Not Started |
| T-WEB-1120e | WEB-1104 | Dashboard chart X-axis labels show date + time for time ranges longer than 24 hours and time-only labels for 24 hours or less | Integration: Mock 24h, 7d, and custom >24h ranges; inspect tick label formatter output | Not Started |
| T-WEB-1121 | WEB-1105 | Malformed expression prevents charting and shows error | Manual: Enter `(x + / 10`, verify chart not rendered, error shown | Not Started |
| T-WEB-1122 | WEB-1105 | Missing variable data skips timestamp (gap in chart) | Integration: Mock partial data, verify gaps | Not Started |
| T-WEB-1123 | WEB-1105 | Runtime error (e.g., log(-5)) skips point, logs to console | Integration: Mock negative value, verify point skipped | Not Started |
| T-WEB-1124 | WEB-1105 | Network error fetching data shows user-visible error | Integration: Mock network failure, verify error message | Not Started |
| T-WEB-1125 | WEB-1106 | Dashboard persisted in localStorage under environment | Unit: Add dashboard, verify `sonde_environments[env].dashboards` | Not Started |
| T-WEB-1126 | WEB-1106 | Switching environments loads that environment's dashboards | Integration: Create dashboards in env1, switch to env2, verify isolation | Not Started |
| T-WEB-1127 | WEB-1106 | Dashboard changes trigger saveEnvironments() | Unit: Mock saveEnvironments, verify called on add/edit/delete | Not Started |
| T-WEB-1127b | WEB-1106 | localStorage quota exceeded shows error, dashboard not lost | Unit: Mock QuotaExceededError, verify error message, dashboard in memory | Not Started |
| T-WEB-1127c | WEB-1106 | Legacy persisted dashboard with top-level metrics migrates to a default chart on load | Integration: Seed old schema in localStorage, load environment, verify one default chart contains the metrics | Not Started |
| T-WEB-1127d | WEB-1106 | Variables pane expand/collapse state persists across reload | Integration: Collapse the Variables pane, reload, verify it remains collapsed for that dashboard | Not Started |
| T-WEB-1127e | WEB-1106 | Per-chart Metrics pane expand/collapse state persists across reload | Integration: Collapse one chart's Metrics pane, reload, verify only that chart remains collapsed | Not Started |
| T-WEB-1127f | WEB-1106 | Dashboards without stored pane state default Variables and Metrics panes to expanded | Integration: Seed legacy dashboard without pane-state fields, load, verify Variables and all chart Metrics panes are expanded | Not Started |
| T-WEB-1128 | WEB-1107 | Environment export includes dashboards array in JSON | Integration: Export environment with dashboards, verify JSON structure | Not Started |
| T-WEB-1128b | WEB-1107 | Environment export preserves persisted pane states | Integration: Export dashboard with collapsed Variables pane and collapsed chart Metrics pane, verify both state fields appear in JSON | Not Started |
| T-WEB-1129 | WEB-1107 | Environment import restores dashboards | Integration: Import JSON with dashboards, verify restored | Not Started |
| T-WEB-1129b | WEB-1107 | Environment import restores persisted pane states | Integration: Import dashboard JSON with collapsed pane states, verify restored UI state after load | Not Started |
| T-WEB-1130 | WEB-1107 | Missing dashboards field defaults to empty array | Integration: Import JSON without dashboards field, verify default | Not Started |
| T-WEB-1130b | WEB-1107 | Import with undefined variable shows warning | Integration: Import metric with undefined variable, verify warning displayed | Not Started |
| T-WEB-1130c | WEB-1107 | Legacy dashboard import migrates top-level metrics into a default chart | Integration: Import old schema, verify one default chart containing the metrics | Not Started |
| T-WEB-1131 | WEB-1108 | Both Sensor Data and Dashboards tabs visible | Manual: Verify both tabs present in navigation | Not Started |
| T-WEB-1132 | WEB-1108 | Switching between tabs preserves independent state | Manual: Configure both tabs, switch, verify state preserved | Not Started |
| T-WEB-1109a | WEB-1109 | Operator precedence: `2 + 3 * 4` evaluates to `14` | Unit: Evaluate expression, verify result | Not Started |
| T-WEB-1109b | WEB-1109 | Parentheses override precedence: `(2 + 3) * 4` evaluates to `20` | Unit: Evaluate expression, verify result | Not Started |
| T-WEB-1110a | WEB-1110 | Creating 21st dashboard shows warning | Manual: Create 20 dashboards, attempt 21st, verify warning | Not Started |
| T-WEB-1110b | WEB-1110 | Creating 11th metric shows warning | Manual: Create 10 metrics, attempt 11th, verify warning | Not Started |
| T-WEB-1133 | WEB-1111 | Variables pane can be collapsed and expanded from its header | Manual: Toggle Variables pane twice, verify content hides and returns | Not Started |
| T-WEB-1134 | WEB-1111 | Each chart's Metrics pane can be collapsed independently | Manual: Collapse one chart's Metrics pane, verify another chart remains unchanged | Not Started |
| T-WEB-1135 | WEB-1111 | Collapsing a chart's Metrics pane keeps the rendered graph visible | Manual: Collapse Metrics pane on populated chart, verify canvas and legend remain visible | Not Started |
| T-WEB-1136 | WEB-1111 | Pane controls default to expanded when no saved state exists | Manual: Open a new dashboard, verify Variables and Metrics panes start expanded | Not Started |
| T-WEB-1137 | WEB-1111 | Pane controls expose expanded/collapsed state to keyboard and assistive technology | Manual: Keyboard-navigate to pane controls, toggle them, verify accessible state changes | Not Started |

---

## 8  Revision History

| Date | Author | Description |
|------|--------|-------------|
| 2026-06-18 | evolve skill | Added WEB-0706 validation coverage for in-flight request coalescing across actualstate hydration, sensordata cold fetches, and overlapping dashboard metric consumers. |
| 2026-06-18 | evolve skill | Added WEB-0706 session telemetry cache traceability and validation cases for reuse, delta refresh, uncovered historical backfill, environment isolation, and dashboard metric fetch deduplication. |
| 2026-06-17 | evolve skill | Added validation coverage for collapsible Variables and per-chart Metrics panes, including persistence, default state, and accessibility checks. |
| 2026-06-16 | evolve skill | Added §5.11 (Custom Dashboards) test cases T-WEB-1100 through T-WEB-1132. Updated traceability matrix for WEB-1100 series requirements. |
| 2026-05-29 | Issue #1092 | Added T-WEB-1009 through T-WEB-1009k for gateway convergence. Updated T-WEB-1001/1002 for badge and inline form. Retired T-WEB-1006/1006b. |
| 2026-05-19 | Spec extraction (automated) | Restructured with sections, traceability matrix, risk prioritization. Added references to web-ui-requirements.md. |
| 2026-05-19 | Trifecta remediation (#1012) | Added ~35 fine-grained test cases to cover all acceptance criteria individually. Updated traceability matrix. |
