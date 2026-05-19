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

- Azure handler function internals (covered by azure-handler-validation.md)
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
| WEB-0101 | T-WEB-0101 |
| WEB-0102 | T-WEB-0102 |
| WEB-0103 | T-WEB-0103 |
| WEB-0104 | T-WEB-0104, T-WEB-0105, T-WEB-0106, T-WEB-0107, T-WEB-0108 |
| WEB-0201 | T-WEB-0201 |
| WEB-0202 | T-WEB-0202 |
| WEB-0203 | T-WEB-0203 |
| WEB-0204 | T-WEB-0204 |
| WEB-0205 | T-WEB-0205 |
| WEB-0206 | T-WEB-0206 |
| WEB-0207 | T-WEB-0207 |
| WEB-0301 | T-WEB-0301 |
| WEB-0302 | T-WEB-0302 |
| WEB-0303 | T-WEB-0303 |
| WEB-0304 | T-WEB-0304 |
| WEB-0305 | T-WEB-0305 |
| WEB-0306 | T-WEB-0306 |
| WEB-0307 | T-WEB-0307a, T-WEB-0307b |
| WEB-0308 | T-WEB-0308 |
| WEB-0309 | T-WEB-0309 |
| WEB-0310 | T-WEB-0310 |
| WEB-0401 | T-WEB-0401 |
| WEB-0501 | T-WEB-0501 |
| WEB-0502 | T-WEB-0502 |
| WEB-0503 | T-WEB-0503 |
| WEB-0504 | T-WEB-0504 |
| WEB-0505 | T-WEB-0505 |
| WEB-0506 | T-WEB-0506 |
| WEB-0507 | T-WEB-0507 |
| WEB-0508 | T-WEB-0508, T-WEB-0509 |
| WEB-0602 | T-WEB-0602 |
| WEB-0603 | T-WEB-0603 |
| WEB-0604 | T-WEB-0604 |
| WEB-0605 | T-WEB-0605 |
| WEB-0606 | T-WEB-0606 |
| WEB-0607 | T-WEB-0607 |
| WEB-0700 | T-WEB-0701 |
| WEB-0701 | T-WEB-0702, T-WEB-0703, T-WEB-0706, T-WEB-0713, T-WEB-0714, T-WEB-0715 |
| WEB-0702 | T-WEB-0704, T-WEB-0705 |
| WEB-0703 | T-WEB-0707, T-WEB-0708, T-WEB-0709, T-WEB-0710, T-WEB-0711, T-WEB-0712 |
| WEB-0800 | — (covered by WEB-0801 through WEB-0806 tests) |
| WEB-0801 | T-WEB-0802 |
| WEB-0802 | T-WEB-0806, T-WEB-0807, T-WEB-0808, T-WEB-0809 |
| WEB-0803 | T-WEB-0801 |
| WEB-0804 | T-WEB-0805 |
| WEB-0805 | T-WEB-0804 |
| WEB-0806 | T-WEB-0803 |
| WEB-0901 | T-WEB-0901 |
| WEB-0902 | T-WEB-0902 |
| WEB-CC-01 | T-WEB-CC-01 |
| WEB-CC-02 | T-WEB-CC-02 |
| WEB-CC-03 | T-WEB-CC-03 |
| WEB-CC-04 | T-WEB-CC-04 |

---

## 5  Test Cases

### 5.1  Node Dashboard

| Test ID | Requirement | Description | Method | Status |
|---|---|---|---|---|
| T-WEB-0101 | WEB-0101 | SPA renders node table from `actualstate` query | Manual/E2E | Planned |
| T-WEB-0102 | WEB-0102 | All nine required columns displayed | Manual/E2E | Planned |
| T-WEB-0103 | WEB-0103 | Auto-refresh polls at configured interval (30s default) | Manual | Planned |
| T-WEB-0104 | WEB-0104 | Divergence indicator shown when desired-state row exists and actual differs from desired | Manual/E2E | Planned |
| T-WEB-0105 | WEB-0104 | Unassigned program (desired row exists, program hash empty/missing) shows Diverged while node still reports a current program | Manual/E2E | Planned |
| T-WEB-0106 | WEB-0104 | Node with no desired-state row shows Aligned even if actual state reports a program | Manual/E2E | Planned |

### 5.2  Desired State

| Test ID | Requirement | Description | Method | Status |
|---|---|---|---|---|
| T-WEB-0201 | WEB-0201 | Set desired schedule writes correct row with `Edm.Int32` type | Integration | Planned |
| T-WEB-0202 | WEB-0202 | Assign program hash writes correct row with lowercase hex | Integration | Planned |
| T-WEB-0203 | WEB-0203 | RowKey uses reverse-timestamp format (3 colon-separated 16-char hex segments) | Unit (JS) | Planned |
| T-WEB-0204 | WEB-0204 | PartitionKey = `n:{SHA-256(node_id)}` via SubtleCrypto | Unit (JS) | Planned |
| T-WEB-0205 | WEB-0205 | `timestamp_ms` stored as `Edm.Int64` | Integration | Planned |
| T-WEB-0206 | WEB-0206 | Node ID field is a dropdown-only control populated from latest `actualstate` nodes; arbitrary node IDs cannot be entered or submitted | Manual/E2E | Planned |
| T-WEB-0207 | WEB-0207 | Selecting a node pre-populates Schedule and Program Hash (desired state preferred over actual state) | Manual/E2E | Planned |

### 5.3  Program Ingest

| Test ID | Requirement | Description | Method | Status |
|---|---|---|---|---|
| T-WEB-0301 | WEB-0301 | `ProgramIngest` accepts ELF + metadata via JSON POST | Unit (Rust) | Pass |
| T-WEB-0302 | WEB-0302 | Prevail verification runs; invalid ELF rejected | Unit (Rust) | Pass |
| T-WEB-0303 | WEB-0303 | Program hash matches gateway computation | Unit (Rust) | Pass |
| T-WEB-0304 | WEB-0304 | Program stored in `programs` table with all fields | Integration | Planned |
| T-WEB-0305 | WEB-0305 | Success returns hash+metadata; failure returns diagnostics | Unit (Rust) | Pass |
| T-WEB-0306 | WEB-0306 | Oversized programs rejected (>1 MB → HTTP 413) | Unit (Rust) | Pass |
| T-WEB-0307a | WEB-0307 | Empty ELF rejected | Unit (Rust) | Pass |
| T-WEB-0307b | WEB-0307 | Multi-program ELF rejected | Unit (Rust) | Planned |
| T-WEB-0308 | WEB-0308 | `source_filename` normalized to basename | Unit (Rust) | Pass |
| T-WEB-0309 | WEB-0309 | `DESIRED_STATE` includes inline ELF on program divergence | Unit (Rust) | Planned |
| T-WEB-0310 | WEB-0310 | `DESIRED_STATE` carries key 5 with ELF bytes and keys 6–8 with metadata | Unit (Rust) | Planned |

### 5.4  Program List

| Test ID | Requirement | Description | Method | Status |
|---|---|---|---|---|
| T-WEB-0401 | WEB-0401 | SPA lists programs from `programs` table with correct columns | Manual/E2E | Planned |

### 5.5  Authentication

| Test ID | Requirement | Description | Method | Status |
|---|---|---|---|---|
| T-WEB-0501 | WEB-0501 | MSAL.js login flow works (popup, PKCE) | Manual | Planned |
| T-WEB-0502 | WEB-0502 | Storage API calls include bearer token | Integration | Planned |
| T-WEB-0503 | WEB-0503 | `ProgramIngest` rejects unauthenticated requests (EasyAuth returns 401) | Integration | Planned |
| T-WEB-0504 | WEB-0504 | SPA acquires Function App-scoped token for `ProgramIngest` calls | Manual | Planned |
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
| T-WEB-0607 | WEB-0607 | `ProgramIngest` `authLevel` is `anonymous` (auth delegated to EasyAuth) | Infrastructure | Planned |

### 5.7  Sensor Data

| Test ID | Requirement | Description | Method | Status |
|---|---|---|---|---|
| T-WEB-0701 | WEB-0700 | Sensor Data tab appears in tab bar and loads data from `sensordata` table | Manual/E2E | Planned |
| T-WEB-0702 | WEB-0701 | Time-series graph renders lines per (node, program, reading) | Manual/E2E | Planned |
| T-WEB-0703 | WEB-0701 | Time range selector filters data correctly | Manual/E2E | Planned |
| T-WEB-0704 | WEB-0702 | Table view shows all sensor data columns | Manual/E2E | Planned |
| T-WEB-0705 | WEB-0702 | SPA handles empty `decoded_readings` gracefully (shows "—") | Manual/E2E | Planned |
| T-WEB-0706 | WEB-0701 | SPA displays string-encoded int64 values (above `Number.MAX_SAFE_INTEGER`) correctly and renders in-range values as numbers | Manual | Planned |
| T-WEB-0707 | WEB-0703 | Series edit dialog opens when ✏️ button is clicked and pre-fills saved overrides | Manual | Planned |
| T-WEB-0708 | WEB-0703 | Custom display name replaces default label in series picker, chart legend, and tooltip | Manual | Planned |
| T-WEB-0709 | WEB-0703 | Scale divisor transforms plotted values (e.g., 1000 converts 21500 → 21.5) | Manual | Planned |
| T-WEB-0710 | WEB-0703 | Unit suffix appears in tooltip values and Y-axis title when all series share the same suffix | Manual | Planned |
| T-WEB-0711 | WEB-0703 | Overrides persist across page reloads via `localStorage` | Manual | Planned |
| T-WEB-0712 | WEB-0703 | Reset to Default clears overrides and restores original label/scale | Manual | Planned |

### 5.8  Environment Manager

| Test ID | Requirement | Description | Method | Status |
|---|---|---|---|---|
| T-WEB-0801 | WEB-0803 | First load with no environments shows full-screen setup modal; main UI is inaccessible | Manual | Planned |
| T-WEB-0802 | WEB-0801 | Adding an environment persists all fields to `localStorage` under `sonde_environments` | Manual | Planned |
| T-WEB-0803 | WEB-0806 | Switching environment re-initializes MSAL, clears session, and refreshes active tab | Manual | Planned |
| T-WEB-0804 | WEB-0805 | Active environment name displayed in header bar | Manual | Planned |
| T-WEB-0805 | WEB-0804 | Edit and delete operations on environments work correctly | Manual | Planned |
| T-WEB-0806 | WEB-0802 | Environment fields validated (all required, duplicate name rejected) | Manual | Planned |

### 5.9  Deployment

| Test ID | Requirement | Description | Method | Status |
|---|---|---|---|---|
| T-WEB-0901 | WEB-0901 | GitHub Actions workflow deploys `deploy/web-ui/` to GitHub Pages on push to main | Infrastructure | Planned |
| T-WEB-0902 | WEB-0902 | Bicep includes GitHub Pages and `sondeplatform.com` in CORS origins and SPA redirect URIs | Infrastructure | Planned |

### 5.10  Cross-Cutting

| Test ID | Requirement | Description | Method | Status |
|---|---|---|---|---|
| T-WEB-CC-01 | WEB-CC-01 | `deploy/web-ui/` contains only `.html`, `.js`, `.css`, `.json` — no build tools or `node_modules` | Inspection | Planned |
| T-WEB-CC-02 | WEB-CC-02 | All dynamic values in rendered HTML pass through `escapeHtml()` — no raw insertion of user/server data | Code review | Planned |
| T-WEB-CC-03 | WEB-CC-03 | Non-auth hashes temporarily cleared before MSAL init; restored after | Manual | Planned |
| T-WEB-CC-04 | WEB-CC-04 | SPA loaded inside MSAL popup does not call `init()` | Code review | Planned |

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

---

## 6  Risk-Based Test Prioritization

| Risk | Impact | Likelihood | Priority | Related Tests |
|------|--------|------------|----------|---------------|
| Authentication failure blocks all functionality | High | Medium | P1 | T-WEB-0501–0509 |
| Program ingest rejects valid ELF or accepts invalid | High | Low | P1 | T-WEB-0301–0308 |
| Divergence indicator shows wrong status | Medium | Medium | P2 | T-WEB-0104–0108 |
| Environment switching leaves stale tokens | Medium | Medium | P2 | T-WEB-0803 |
| XSS from unescaped user/server data | High | Low | P1 | T-WEB-CC-02 |
| Sensor data renders incorrect values | Medium | Low | P3 | T-WEB-0706, T-WEB-0709, T-WEB-0713–0715 |

---

## 7  Pass/Fail Criteria

- **Entry criteria:** SPA deployed to GitHub Pages; Azure environment provisioned via Bicep.
- **Exit criteria:** All P1 tests pass; no P2 tests fail with severity > minor.
- **Acceptance threshold:** 100% of Must-priority requirements have at least one passing test case.

---

## 8  Revision History

| Date | Author | Description |
|------|--------|-------------|
| 2026-05-19 | Spec extraction (automated) | Restructured with sections, traceability matrix, risk prioritization. Added references to web-ui-requirements.md. |
