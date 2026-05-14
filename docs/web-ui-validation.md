<!-- SPDX-License-Identifier: MIT -->
<!-- Copyright (c) 2026 sonde contributors -->
# Sonde Web UI — Validation

> **Document status:** Draft  
> **Scope:** Validation plan and traceable test matrix for the Sonde Web UI.  
> **Audience:** Implementers (human or LLM agent) writing web UI, function, integration, and infrastructure tests.  
> **Related:** [web-ui-design.md](web-ui-design.md)

---

## Test Matrix

| Test ID | Requirement | Description | Method | Status |
|---|---|---|---|---|
| T-WEB-0101 | WEB-0101 | SPA renders node table from `actualstate` query | Manual/E2E | Planned |
| T-WEB-0102 | WEB-0102 | All required columns displayed | Manual/E2E | Planned |
| T-WEB-0103 | WEB-0103 | Auto-refresh polls at configured interval | Manual | Planned |
| T-WEB-0104 | WEB-0104 | Divergence indicator shown when desired-state row exists and actual differs from desired | Manual/E2E | Planned |
| T-WEB-0105 | WEB-0104 | Unassigned program (desired row exists, program hash empty/missing) shows Diverged while node still reports a current program | Manual/E2E | Planned |
| T-WEB-0106 | WEB-0104 | Node with no desired-state row shows Aligned even if actual state reports a program | Manual/E2E | Planned |
| T-WEB-0201 | WEB-0201 | Set desired schedule writes correct row | Integration | Planned |
| T-WEB-0202 | WEB-0202 | Assign program hash writes correct row | Integration | Planned |
| T-WEB-0203 | WEB-0203 | RowKey uses reverse-timestamp format | Unit (JS) | Planned |
| T-WEB-0204 | WEB-0204 | PartitionKey = `n:{SHA-256(node_id)}` | Unit (JS) | Planned |
| T-WEB-0205 | WEB-0205 | `timestamp_ms` stored as `Edm.Int64` | Integration | Planned |
| T-WEB-0206 | WEB-0206 | Node ID field is a dropdown-only control populated from latest `actualstate` nodes; arbitrary node IDs cannot be entered or submitted | Manual/E2E | Planned |
| T-WEB-0207 | WEB-0207 | Selecting a node pre-populates Schedule and Program Hash (desired state preferred over actual state) | Manual/E2E | Planned |
| T-WEB-0301 | WEB-0301 | `ProgramIngest` accepts ELF + metadata via JSON POST | Unit (Rust) | Pass |
| T-WEB-0302 | WEB-0302 | Prevail verification runs; invalid ELF rejected | Unit (Rust) | Pass |
| T-WEB-0303 | WEB-0303 | Program hash matches gateway computation | Unit (Rust) | Pass |
| T-WEB-0304 | WEB-0304 | Program stored in `programs` table with all fields | Integration | Planned |
| T-WEB-0305 | WEB-0305 | Success returns hash+metadata; failure returns diagnostics | Unit (Rust) | Pass |
| T-WEB-0306 | WEB-0306 | Oversized programs rejected | Unit (Rust) | Pass |
| T-WEB-0307a | WEB-0307 | Empty ELF rejected | Unit (Rust) | Pass |
| T-WEB-0307b | WEB-0307 | Multi-program ELF rejected | Unit (Rust) | Planned |
| T-WEB-0308 | WEB-0308 | `source_filename` normalized to basename | Unit (Rust) | Pass |
| T-WEB-0309 | WEB-0309 | `DESIRED_STATE` includes inline ELF on program divergence | Unit (Rust) | Planned |
| T-WEB-0310 | WEB-0310 | `DESIRED_STATE` carries key 5 with ELF bytes and keys 6-8 with metadata | Unit (Rust) | Planned |
| T-WEB-0401 | WEB-0401 | SPA lists programs from `programs` table | Manual/E2E | Planned |
| T-WEB-0402 | WEB-0402 | SPA creates/edits `programroute` entries | Manual/E2E | Planned |
| T-WEB-0501 | WEB-0501 | MSAL.js login flow works | Manual | Planned |
| T-WEB-0502 | WEB-0502 | Storage API calls use bearer token | Integration | Planned |
| T-WEB-0503 | WEB-0503 | `ProgramIngest` rejects unauthenticated requests (EasyAuth returns 401) | Integration | Planned |
| T-WEB-0504 | WEB-0504 | SPA acquires Function App-scoped token for `ProgramIngest` calls | Manual | Planned |
| T-WEB-0505 | WEB-0505 | `ProgramIngest` rejects Storage-scoped token (wrong audience) | Integration | Planned |
| T-WEB-0506 | WEB-0506 | `ProgramIngest` rejects expired bearer token | Integration | Planned |
| T-WEB-0507 | WEB-0507 | `ProgramIngest` accepts valid `api://<clientId>/user_impersonation` token | Integration | Planned |
| T-WEB-0601 | WEB-0601 | Bicep provisions Static Web App | Infrastructure | Planned |
| T-WEB-0602 | WEB-0602 | Bicep provisions `programs` table | Infrastructure | Planned |
| T-WEB-0603 | WEB-0603 | `ProgramIngest` HTTP trigger deployed alongside `UpstreamConnector` | Infrastructure | Planned |
| T-WEB-0604 | WEB-0604 | CORS configured for SPA origin | Infrastructure | Planned |
| T-WEB-0605 | WEB-0605 | Function identity has table contributor on `programs` | Infrastructure | Planned |
| T-WEB-0606 | WEB-0606 | EasyAuth configured on Function App with Entra ID provider | Infrastructure | Planned |
| T-WEB-0607 | WEB-0607 | `ProgramIngest` `authLevel` is `anonymous` (auth delegated to EasyAuth) | Infrastructure | Planned |
