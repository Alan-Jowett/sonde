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
- Request: `application/json` with fields:
  - `elf` (string, required): Base64-encoded ELF binary
  - `source_filename` (string, optional): Original filename for display
  - `abi_version` (integer, optional): ABI version the program targets
  - `verification_profile` (string, optional): `"resident"` (default) or `"ephemeral"`
- Processing (reuses `sonde-gateway` `ProgramLibrary`):
  1. Parse JSON body from the Azure Functions HTTP trigger envelope (`Data.req.Body`)
  2. Base64-decode the `elf` field; reject if missing, empty, or invalid base64
  3. Reject requests where decoded ELF exceeds 1 MB (defense-in-depth before verification)
  4. Parse `verification_profile`; default to `Resident` if absent
  5. Call `ProgramLibrary::ingest_elf(elf_bytes, profile)` — this verifies with Prevail, extracts bytecode + maps, encodes deterministic CBOR, and computes SHA-256
  6. Normalize `source_filename` via `normalize_display_filename()`
  7. Store in `programs` Azure Table via `HandlerStore::store_program_image()`
  8. Return JSON: `{"program_hash": "hex", "size": N, "abi_version": N, "source_filename": "name"}`
  9. On failure: return JSON error with diagnostics
- The uploaded ELF is verified and transformed; the persisted and delivered artifact is the deterministic CBOR program image, not the raw ELF
- `programs` table schema: `PartitionKey="program"`, `RowKey=hex(program_hash)`, `source_filename`, `abi_version` (`Edm.Int32`), `cbor_image` (base64-encoded CBOR program image), `size_bytes` (`Edm.Int32`, CBOR image byte length), `verification_profile`, `created_at` (ISO 8601 UTC string)
- Idempotent: re-ingesting the same ELF produces the same program hash; all metadata fields (`source_filename`, `abi_version`, `verification_profile`, `created_at`) are overwritten on re-ingest (last-writer-wins)
- Error responses use HTTP status codes: 400 (malformed JSON, missing `elf` field, invalid base64), 413 (ELF exceeds size limit), 422 (Prevail verification failure, invalid ELF), 500 (storage/internal error)

#### 6.1.1 Custom Handler Routing

The Azure Functions custom handler (`main.rs`) routes `/ProgramIngest` to a
dedicated `handle_program_ingest` handler, separate from the catch-all
`/{*path}` route used for queue-triggered connector messages. The handler:

1. Extracts the HTTP trigger envelope from the Azure Functions invocation request
2. Reads `Data.req.Body` (JSON string)
3. Parses the JSON body to extract `elf`, `source_filename`, `abi_version`, `verification_profile`
4. Processes the program through `ProgramLibrary`
5. Returns an Azure Functions HTTP output binding response:
   ```json
   {
     "Outputs": {
       "res": {
         "statusCode": 200,
         "headers": {"Content-Type": "application/json"},
         "body": "{\"program_hash\":\"hex\",\"size\":N,\"abi_version\":N,\"source_filename\":\"name\"}"
       }
     }
   }
   ```

#### 6.1.2 HandlerStore Expansion

The `HandlerStore` trait gains:

```rust
async fn store_program_image(&self, row: &ProgramImageRow) -> Result<(), HandlerError>;
```

`ProgramImageRow` contains: `program_hash` (`Vec<u8>`), `cbor_image` (`Vec<u8>`),
`source_filename` (`Option<String>`), `abi_version` (`Option<u32>`), `size_bytes` (`u32`),
`verification_profile` (`String`), `created_at` (`String`, ISO 8601 UTC).

`AzureTablesStore` implements this as an upsert to the `programs` table, encoding
`cbor_image` as base64 and `program_hash` as hex for the `RowKey`.

### 6.2 Inline Program Image in DESIRED_STATE (WEB-0309, WEB-0310)

When the handler publishes a `DESIRED_STATE` message due to program divergence, it fetches the CBOR program image from the `programs` table and embeds it at CBOR key 5 (`assigned_program_image`, `bstr`). The companion forwards this opaque payload to the gateway, which ingests the inline image into its local `ProgramLibrary`.

> **Note**: The gateway's `DESIRED_STATE` handler (`connector.rs`) does not yet
> read key 5. Key 5 (`assigned_program_image`) is now documented in
> `gateway-companion-api.md` §3.2.2. A follow-up change to `connector.rs` is
> required to parse and ingest the inline program image.

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
> for Azure Table operations. The `ProgramIngest` function endpoint uses
> `authLevel: "function"` for API key authentication. Browser-based SPA
> access to this endpoint requires a follow-up to configure Entra/EasyAuth
> token-based authentication with the function's API scope and appropriate
> redirect URI configuration.

---

## 9. Infrastructure (WEB-0600)

### 9.1 Static Web App

Azure Static Web App (free tier) provisioned via `static-web-app.bicep`.
SPA content is deployed automatically during `sonde-azure-companion bootstrap`
(see AZC-0410). The bootstrap flow generates `config.json`, deploys the SPA
content to the Static Web App, registers the SWA hostname as a redirect URI on
the Entra app, and adds the Azure Storage API permission.

For standalone (non-bootstrap) deployment, use the deployment script:

Prerequisites: `az` CLI (logged in), `jq`, and `npm`/`npx` (for the SWA CLI).

```bash
./deploy/web-ui/deploy.sh <COMPANION_CLIENT_ID> [RESOURCE_GROUP]
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

- `ProgramIngest/function.json` defines the HTTP trigger (`authLevel: function`,
  route `programs/ingest`).
- `main.rs` routes `/ProgramIngest` to a dedicated handler that parses the
  Azure Functions HTTP trigger envelope and delegates to
  `AzureHandler::handle_program_ingest()`. The catch-all `/{*path}` route
  continues to handle queue-triggered connector messages.
- `lib.rs` implements `handle_program_ingest()` which reuses
  `ProgramLibrary::ingest_elf()` from `sonde-gateway` for Prevail verification,
  CBOR encoding, and SHA-256 hashing.
- The HTTP trigger response uses the Azure Functions `res` output binding
  envelope format with `statusCode`, `headers`, and `body` fields.

> **Note**: The `ProgramIngest` endpoint currently uses `authLevel: "function"`
> for Azure Functions-level API key authentication. Browser-based SPA access
> requires a follow-up to configure Entra/EasyAuth token-based authentication
> with the function's API scope.
