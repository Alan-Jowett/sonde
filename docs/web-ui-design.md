<!-- SPDX-License-Identifier: MIT -->
<!-- Copyright (c) 2026 sonde contributors -->
# Sonde Web UI — Design

> **Document status:** Draft  
> **Scope:** Architecture and implementation design for the Sonde Web UI.  
> **Audience:** Implementers (human or LLM agent) building the web UI and supporting Azure infrastructure.  
> **Related:** [gateway-design.md](gateway-design.md), [gateway-validation.md](gateway-validation.md)

---

## 1. Overview

Static SPA hosted on Azure Static Web Apps (free tier). Vanilla HTML/JS/CSS with zero build step. Communicates directly with Azure Storage Tables via REST API using MSAL.js bearer tokens. Program ingestion is delegated to an HTTP-triggered Azure Function that runs Prevail verification server-side.

---

## 2. Component Architecture

```
Browser (SPA)
├── Dashboard (read actualstate table)
├── Desired State (read/write desiredstate table)
├── Program Upload (POST ELF to ProgramIngest function)
└── Program List (read programs + programroute tables)
     │
     │ MSAL.js Bearer Token
     ▼
Azure Storage Tables + ProgramIngest Azure Function
```

---

## 3. File Structure

```
deploy/web-ui/
  index.html                  — single-page app shell
  app.js                      — application logic (MSAL, table queries, UI rendering)
  style.css                   — minimal styling
  staticwebapp.config.json    — Azure Static Web Apps routing config
```

---

## 4. Node Dashboard (WEB-0100)

- Queries `actualstate` Azure Table via REST API.
- Groups entities by `PartitionKey` (one per node), displays most recent row (smallest `RowKey` due to reverse-timestamp ordering).
- Cross-references `desiredstate` table to compute divergence indicators.
- Auto-refresh every 30s by default (configurable).
- Columns: Node ID, Battery (mV), Firmware, ABI Version, Schedule (s), Current Program Hash, Assigned Program Hash, Last Seen, Status (aligned/diverged).

---

## 5. Desired State Management (WEB-0200)

- Form: Node ID (text), Schedule Interval (number, seconds), Program Hash (dropdown from `programs` table).
- On submit:
  - `PartitionKey`: `"n:" + SHA-256(node_id).hex()` using `SubtleCrypto`
  - `RowKey`: reverse-timestamp format `{(u64::MAX - timestamp_ms):016x}:{(u64::MAX - sequence):016x}:{random_nonce:016x}`
  - `timestamp_ms` stored as `Edm.Int64`
  - `POST`s entity to `desiredstate` table via Azure Tables REST API

---

## 6. Program Ingest (WEB-0300)

### 6.1 Azure Function: ProgramIngest

- HTTP trigger, `POST`, route `api/programs/ingest`
- Request: `multipart/form-data` with fields: `elf` (binary), `source_filename` (string), `abi_version` (integer), `verification_profile` (`"resident"|"ephemeral"`, default `"resident"`)
- Processing (reuses `sonde-gateway` `ProgramLibrary`):
  1. Parse multipart body
  2. Call `ProgramLibrary::ingest_elf(elf_bytes, profile)`
  3. Normalize `source_filename` via `normalize_display_filename()`
  4. Store in `programs` Azure Table
  5. Return JSON: `{"program_hash": "hex", "size": N, "abi_version": N, "source_filename": "name"}`
  6. On failure: return JSON error with Prevail diagnostics
- `programs` table schema: `PartitionKey="program"`, `RowKey=hex(program_hash)`, `source_filename`, `abi_version` (`Edm.Int32`), `cbor_image` (base64), `size_bytes` (`Edm.Int32`), `verification_profile`, `created_at` (`Edm.DateTime`)

### 6.2 Inline Program Image in DESIRED_STATE (WEB-0309, WEB-0310)

When the handler publishes a `DESIRED_STATE` message due to program divergence, it fetches the CBOR program image from the `programs` table and embeds it at CBOR key 5 (`assigned_program_image`, `bstr`). The companion forwards this opaque payload to the gateway, which ingests the inline image into its local `ProgramLibrary`.

> **Note**: The gateway's `DESIRED_STATE` handler (`connector.rs`) does not yet
> read key 5. A follow-up change to `gateway-companion-api.md` and
> `connector.rs` is required to parse and ingest the inline program image.

---

## 7. Program List and Routes (WEB-0400)

- Queries `programs` table (`PartitionKey eq 'program'`), displays table with hash, filename, `abi_version`, size, upload time.
- Program route management: queries `programroute` table, allows insert/update of `handler_queue` for a given program hash.

---

## 8. Authentication (WEB-0500)

- MSAL.js 2.x with authorization code flow + PKCE.
- Scopes: `https://storage.azure.com/.default` (Table/Queue access).
- Token caching in browser session storage.
- Silent token renewal; redirect to login on expiry.

> **Note**: The SPA acquires tokens scoped to `https://storage.azure.com/.default`
> for Azure Table operations. The ProgramIngest function endpoint is not yet
> implemented in the custom handler. When it is, a separate token with the
> function's API scope and appropriate auth configuration will be required.

---

## 9. Infrastructure (WEB-0600)

### 9.1 Static Web App

Azure Static Web App (free tier) provisioned via `static-web-app.bicep`.
After Bicep deployment, deploy the SPA using the deployment script.

Prerequisites: `az` CLI (logged in), `jq`, and `npm`/`npx` (for the SWA CLI).

```bash
./deploy/web-ui/deploy.sh [RESOURCE_GROUP]
```

The script:
1. Discovers the SWA, function app, and storage account from the resource group
2. Generates `config.json` with MSAL client ID, tenant ID, storage account, and function app name
3. Registers the SWA hostname as a SPA redirect URI on the Entra app registration
4. Adds Azure Storage API permission (`user_impersonation`) to the Entra app registration
5. Deploys the web-ui content to the Static Web App using the SWA CLI

> **Note:** Steps 3–4 mutate the Entra app registration associated with the Azure companion.

After deployment, grant users the `Storage Table Data Contributor` role on the storage account.

### 9.2 Modified Bicep Modules

- `storage.bicep`: add `programs` table.
- `function-rbac.bicep`: add Storage Table Data Contributor on `programs` table.
- `stack.bicep`: wire new modules.
- `main.bicep`: add outputs.

### 9.3 Function App Changes

- New `function.json` for `ProgramIngest` HTTP trigger (stub —
  `authLevel: function`). The custom handler does not yet route or
  process HTTP-triggered requests; this trigger definition is
  scaffolding for the follow-up implementation.
- The HTTP ingest handler implementation in `main.rs` is planned. The
  current handler routes all POSTs through `extract_trigger_payload` /
  `handle_payload`. A dedicated route for `/api/programs/ingest` with
  multipart parsing will be added in a follow-up.
