<!-- SPDX-License-Identifier: MIT -->
<!-- Copyright (c) 2026 sonde contributors -->
# Specification Patch: SPA Gateway Configuration

> **Status:** Draft
> **Scope:** Design and validation changes propagated from the requirements
> patch (WEB-1000–WEB-1301, AZH-0700–AZH-0703).
> **Traceability:** Every section traces to one or more requirement IDs.

---

## Change Manifest — Affected Documents

| Document | Section(s) | Change type |
|---|---|---|
| `web-ui-design.md` | NEW §12, §13, §14 | New sections |
| `web-ui-design.md` | §2, §7, §8, §11.2 | Extended |
| `web-ui-validation.md` | New tests T-WEB-1001–T-WEB-1301 | New entries |
| `gateway-companion-api.md` | §3.3.1, new §3.6 | Extended + new section |
| `gateway-design.md` | New §(TBD) | New section for ADMIN_COMMAND |

---

## 1. Web UI Design Changes (`web-ui-design.md`)

### 1.1 Component Architecture update (§2)

> **Requirements:** WEB-1000

Updated architecture diagram:

```
Browser (SPA)
├── Dashboard (read actualstate table; reboot action)
├── Desired State (read/write desiredstate table; ephemeral dispatch)
├── Program Upload (POST ELF to ProgramIngest function)
├── Program List (read programs table; delete action)
├── Sensor Data (read SensorData table, time-series graph)
├── Gateway (read actualstate gw:status + gatewayescrow; admin commands)
└── Environment Manager (localStorage config)
     │
     │ MSAL.js Bearer Token
     ▼
Azure Storage Tables + Azure Function HTTP Endpoints
```

### 1.2 Program List extension (§7)

> **Requirements:** WEB-1200

The program list table gains a "Delete" column. Each row shows a 🗑️
button that, on click:

1. Shows `confirm('Delete program {truncHash}?')`.
2. On confirm, calls `deleteProgramFromFunction(hash)`.
3. `deleteProgramFromFunction` acquires a Function App-scoped bearer token
   (same `getFunctionToken()` used by program ingest) and sends:
   ```
   DELETE https://<functionAppName>.azurewebsites.net/api/programs/<hash>
   Authorization: Bearer <token>
   ```
4. On 200, shows success toast and re-renders program list.
5. On error, shows error toast with `parseErrorPayload()`.

Before showing the confirm dialog, the SPA checks the latest
`desiredstate` rows for references to the program hash. If any node's
`desired_assigned_program_hash` matches, the confirm dialog includes a
warning: "⚠ This program is assigned to {N} node(s). Deleting it will
not unassign them."

### 1.3 Environment data model extension (§11.2)

> **Requirements:** WEB-1000 (Gateway tab needs escrow table name)

No schema change needed. The `gatewayescrow` table name is added as a
hardcoded CONFIG constant alongside the existing table names:

```js
const CONFIG = {
  // ... existing fields ...
  gatewayEscrowTable: 'gatewayescrow',
};
```

The escrow table is in the same storage account as the other tables.

### 1.4 Authentication extension (§8)

> **Requirements:** WEB-1002, WEB-1003, WEB-1200, WEB-1300, WEB-1103

All new Azure Function endpoints (`/api/programs/{hash}`,
`/api/admin/command`, `/api/keys/rotate`) use the same Function App-scoped
bearer token (`api://<clientId>/user_impersonation`) already used for
program ingest. No new MSAL scopes or auth configuration needed.

---

## 2. NEW §12 — Gateway Tab (`web-ui-design.md`)

> **Requirements:** WEB-1000, WEB-1001, WEB-1002, WEB-1003, WEB-1100,
> WEB-1101, WEB-1102, WEB-1103

### 12.1 Overview

The Gateway tab provides two panels:
- **Modem** — status display, channel selector, scan trigger
- **Key Escrow** — fingerprint, lifecycle status, rotation wizard

Data sources:
- `actualstate` table, `PartitionKey = "gw:status"` — modem status and
  escrow state from gateway-scoped `ACTUAL_STATE`.
- `gatewayescrow` table, `PartitionKey = "gateway"` — recovery public
  key (RowKey `"pubkey"`) and KDF salt (RowKey `"salt"`).

### 12.2 Modem Status Panel (WEB-1001)

Renders a status card with:

| Label | Source column | Display |
|---|---|---|
| Connection | `modem_connected` | "Connected" (green) / "Disconnected" (red) |
| WiFi Channel | `modem_channel` | Number or "—" |
| MAC Address | `modem_mac` | String or "—" |

When no `gw:status` row exists, shows "No modem data available."

Auto-refreshes using the same `setAutoRefresh()` mechanism as the
Dashboard (WEB-0103).

### 12.3 Channel Set Control (WEB-1002)

Below the modem status card:

```html
<form id="channel-form" class="form-grid">
  <label>WiFi Channel
    <select name="channel">
      <option value="" disabled selected>Select channel…</option>
      <!-- options 1–14 -->
    </select>
  </label>
  <button type="submit" class="primary">Set Channel</button>
</form>
```

On submit:
1. Acquire Function App token via `getFunctionToken()`.
2. `POST https://<functionAppName>.azurewebsites.net/api/admin/command`
   with body `{"command": "set_channel", "params": {"channel": N}}`.
3. On 202, show success toast "Channel change requested".
4. On error, show error toast.

### 12.4 Channel Scan Control (WEB-1003)

Below the channel form, a "Scan Channels" button:

On click:
1. Acquire Function App token.
2. `POST /api/admin/command` with body `{"command": "scan_channels"}`.
3. On 202, show toast "Scan requested — results will appear on refresh".

When `scan_results` is present in the `gw:status` row, render a table:

| Channel | RSSI | SSID |
|---|---|---|
| 1 | -45 | MyNetwork |
| 6 | -72 | OtherNetwork |

`scan_results` is stored as a JSON string column. Parse with
`JSON.parse()` and handle parse errors gracefully.

`scan_timestamp` is rendered as relative time next to the scan results
heading.

### 12.5 Key Escrow Panel (WEB-1100)

The Key Escrow panel contains three sub-sections stacked vertically:
fingerprint, status, and rotation.

### 12.6 Fingerprint Display (WEB-1101)

Reads the pubkey from `gatewayescrow` table:
- `PartitionKey = "gateway"`, `RowKey = "pubkey"`
- Column `public_key` is base64-encoded (32 bytes).

Computation (matching evolve-887 §20.10):

```js
async function computeFingerprint(publicKeyBase64) {
  const pubkeyBytes = base64ToBytes(publicKeyBase64);
  const hashBuffer = await crypto.subtle.digest('SHA-256', pubkeyBytes);
  const hash = new Uint8Array(hashBuffer);
  // Pack bytes 0–8 into a BigInt (72 bits)
  let bits = 0n;
  for (let i = 0; i < 9; i++) {
    bits = (bits << 8n) | BigInt(hash[i]);
  }
  const words = [];
  for (let i = 0; i < 6; i++) {
    const shift = 72n - 11n - 11n * BigInt(i);
    const index = Number((bits >> shift) & 0x7FFn);
    words.push(BIP39_ENGLISH[index]);
  }
  return words;
}
```

The BIP-39 English wordlist (2048 entries) is embedded as a JS array
constant in `app.js`. The list is the standard BIP-0039 English wordlist.

Display: six words in a monospace `<code>` element, separated by spaces.
Example: `abandon ability able about above absent`.

Key epoch and creation timestamp are displayed below the fingerprint as
informational metadata.

When no pubkey row exists: "No recovery key published" in muted text.

### 12.7 Escrow Status Display (WEB-1102)

Data sources:
- Escrow state and key version: `gatewayescrow` table,
  `PartitionKey = "gateway"`, `RowKey = "state"`:
  - Column `escrow_state` (string).
  - Column `escrow_key_version` (int64).
- KDF params: `gatewayescrow` table, `PartitionKey = "gateway"`,
  `RowKey = "salt"`, column `kdf_params_json` (JSON string).

**Canonical `gatewayescrow` table schema** (from handler entity structs):

| Row | Column | Type | Description |
|---|---|---|---|
| `pubkey` | `public_key` | string (base64) | 32-byte X25519 public key |
| `pubkey` | `key_epoch` | int64 | Monotonic epoch |
| `pubkey` | `created_at` | int64 | Unix ms |
| `state` | `escrow_state` | string/null | Lifecycle state |
| `state` | `escrow_key_version` | int64/null | Master key version |
| `salt` | `salt` | string (base64) | 16-byte KDF salt |
| `salt` | `kdf_params_json` | string/null | JSON: `{"m_cost":N,"t_cost":N,"p_cost":N,"kdf_version":N}` |
| `salt` | `created_at` | int64 | Unix ms |

All rows use `PartitionKey = "gateway"`.

Render:

| Label | Value | Badge |
|---|---|---|
| Escrow State | `ready` | `<span class="badge success">ready</span>` |
| Key Version | `3` | — |
| KDF | Argon2id (m=65536, t=3, p=1) | — |

Badge mapping:
- `ready` → `success` (green)
- `bootstrapping`, `rotation_in_progress` → `warning` (yellow)
- `degraded` → `error` (red)
- `disabled` → `muted` (grey)
- missing/unknown → `muted` with text "Unknown"

When KDF salt row is absent: "No KDF salt configured" in muted text.

Warning banner: when escrow state is not `ready`, display a yellow banner
above the panel: "⚠ Key escrow is not ready. Recovery may not be possible."

### 12.8 Key Rotation Wizard (WEB-1103)

A modal dialog opened by clicking "Rotate Key" button. The button is
disabled when no pubkey row or salt row exists (tooltip explains why).

**CDN Dependencies:**

```html
<script src="https://cdn.jsdelivr.net/npm/argon2-browser@1.18.0/dist/argon2-bundled.min.js"
        integrity="sha384-<SRI_HASH>"
        crossorigin="anonymous"></script>
<script src="https://cdn.jsdelivr.net/npm/@noble/curves@1.8.1/ed25519.js"
        integrity="sha384-<SRI_HASH>"
        crossorigin="anonymous"></script>
```

SRI hashes MUST be computed at integration time and pinned in
`index.html`. Versions MUST be pinned (no `@latest` or version ranges).

**Wizard steps:**

**Step 1 — Verify fingerprint:**
- Display the 6-word fingerprint (computed per §12.6).
- Display key epoch.
- Instruction text: "Verify this fingerprint matches the display on your
  gateway's OLED screen."
- Checkbox: "I have verified the fingerprint" (must be checked to
  proceed).
- Buttons: [Cancel] [Next →]

**Step 2 — Enter passphrase:**
- Password input: "Passphrase" (min 12 chars, validated on blur).
- Password input: "Confirm passphrase" (must match).
- Error text appears below if too short or mismatched.
- Buttons: [← Back] [Rotate Key]

**Step 3 — Processing (non-interactive):**
- Spinner with status text cycling through:
  - "Deriving key with Argon2id…" (may take 2–10 seconds)
  - "Generating ephemeral keypair…"
  - "Encrypting master key…"
  - "Sending to gateway…"
- No user interaction; buttons disabled.

**Step 4 — Result:**
- On success: "✓ Key rotation initiated. The gateway will process the
  rotation on the next cycle." [Close]
- On failure: "✗ Key rotation failed: {error}" [Close] [Retry]

**Cryptographic operations:**

```js
async function rotateKey(passphrase, pubkeyBase64, saltBase64, kdfParams, keyEpoch) {
  // 1. Decode inputs
  const pubkey = base64ToBytes(pubkeyBase64);     // 32 bytes
  const salt = base64ToBytes(saltBase64);           // 16 bytes

  // 2. Derive master key via Argon2id
  const masterKey = await argon2.hash({
    pass: passphrase,
    salt: salt,
    type: argon2.ArgonType.Argon2id,
    mem: kdfParams.m_cost,      // e.g. 65536
    time: kdfParams.t_cost,     // e.g. 3
    parallelism: kdfParams.p_cost, // e.g. 1
    hashLen: 32,
  });
  // masterKey.hash is Uint8Array(32)

  // 3. Generate ephemeral X25519 keypair
  const ephemeralPrivate = crypto.getRandomValues(new Uint8Array(32));
  const ephemeralPublic = x25519.scalarMultBase(ephemeralPrivate);

  // 4. X25519 shared secret
  const sharedSecret = x25519.scalarMult(ephemeralPrivate, pubkey);

  // 5. Generate operation_id
  const operationId = crypto.getRandomValues(new Uint8Array(16));

  // 6. HKDF-SHA-256 to derive encryption key
  const targetEpochBe = new Uint8Array(new BigUint64Array([BigInt(keyEpoch)]).buffer);
  // Reverse for big-endian
  targetEpochBe.reverse();
  const info = concatBytes(targetEpochBe, operationId);
  const hkdfKey = await hkdfSha256(sharedSecret, 'sonde-escrow-v1', info, 32);

  // 7. AES-256-GCM encrypt
  const nonce = crypto.getRandomValues(new Uint8Array(12));
  const aad = concatBytes(operationId, targetEpochBe);
  const importedKey = await crypto.subtle.importKey(
    'raw', hkdfKey, 'AES-GCM', false, ['encrypt']);
  const ciphertext = await crypto.subtle.encrypt(
    { name: 'AES-GCM', iv: nonce, additionalData: aad, tagLength: 128 },
    importedKey, masterKey.hash);
  // Web Crypto appends tag to ciphertext; split them
  const ctBytes = new Uint8Array(ciphertext);
  const encryptedMasterKey = ctBytes.slice(0, ctBytes.length - 16);
  const tag = ctBytes.slice(ctBytes.length - 16);

  // 8. Clean up references (best-effort; JS cannot guarantee zeroization)
  masterKey.hash.fill(0);
  ephemeralPrivate.fill(0);
  sharedSecret.fill(0);

  // 9. Send to handler
  const payload = {
    target_key_epoch: keyEpoch,
    sender_public_key: bytesToBase64(ephemeralPublic),
    encrypted_master_key: bytesToBase64(encryptedMasterKey),
    nonce: bytesToBase64(nonce),
    tag: bytesToBase64(tag),
    operation_id: bytesToBase64(operationId),
    rotation_counter: stateRow ? (stateRow.escrow_key_version + 1) : 1,
    expiry_ms: Date.now() + 300_000,
  };

  const token = await getFunctionToken();
  const response = await fetch(
    `https://${CONFIG.functionAppName}.azurewebsites.net/api/keys/rotate`,
    { method: 'POST', headers: {
        Authorization: `Bearer ${token}`,
        'Content-Type': 'application/json',
      }, body: JSON.stringify(payload) });

  if (!response.ok) throw new Error(await response.text());
  return await response.json();
}
```

**HKDF-SHA-256 helper** uses Web Crypto:

```js
async function hkdfSha256(ikm, salt, info, length) {
  const saltBytes = typeof salt === 'string'
    ? new TextEncoder().encode(salt) : salt;
  const baseKey = await crypto.subtle.importKey(
    'raw', ikm, 'HKDF', false, ['deriveBits']);
  const bits = await crypto.subtle.deriveBits(
    { name: 'HKDF', hash: 'SHA-256', salt: saltBytes, info: info },
    baseKey, length * 8);
  return new Uint8Array(bits);
}
```

---

## 3. NEW §13 — Dashboard Node Actions (`web-ui-design.md`)

> **Requirements:** WEB-1300

### 13.1 Reboot Button

Each node row in the Dashboard table gains an "Actions" column with a
"Reboot" button. On click:

1. `confirm('Reboot node {nodeId}?')`.
2. On confirm, acquire Function App token.
3. `POST /api/admin/command` with body:
   `{"command": "reboot_node", "params": {"node_id": "<nodeId>"}}`.
4. On 202, show toast "Reboot queued for {nodeId}".
5. On error, show error toast.

The Dashboard table header gains a new column: `<th>Actions</th>`.

---

## 4. NEW §14 — Ephemeral Program Dispatch (`web-ui-design.md`)

> **Requirements:** WEB-1301

### 14.1 Desired State Form Extension

The Desired State form (§5) gains an optional "Ephemeral Program"
dropdown below the existing "Program Hash" dropdown:

```html
<label>Ephemeral Program (optional)
  <select name="ephemeralProgramHash">
    <option value="">None</option>
    <!-- filtered to verification_profile === 'ephemeral' -->
  </select>
</label>
```

When a program is selected, the submitted desired-state entity includes:

```js
entity.desired_ephemeral_program_hash = selectedHash.toLowerCase();
```

The field is omitted (not set to empty string) when "None" is selected.

---

## 5. Companion API Changes (`gateway-companion-api.md`)

### 5.1 New message type: `ADMIN_COMMAND` (§3.6)

> **Requirements:** AZH-0701

Added to the msg_type table:

| `msg_type` | Name | Direction |
|---|---|---|
| `0x20` | `ADMIN_COMMAND` | Control plane → gateway |

`ADMIN_COMMAND` carries imperative one-shot commands that do not fit the
declarative complete-replacement semantics of `DESIRED_STATE`. Each
command carries an `operation_id` for idempotency and an `expiry_ms` for
staleness rejection, following the pattern established by
`MASTER_KEY_INSTALL` (msg_type `0x13`).

| Field | CBOR key | Type | Description |
|---|---|---|---|
| `msg_type` | 1 | uint | `0x20` |
| `command` | 2 | tstr | Command name: `"set_channel"`, `"scan_channels"`, `"reboot_node"` |
| `params` | 3 | map | Command parameters (integer-keyed CBOR map) |
| `operation_id` | 4 | bstr (16 bytes) | Unique operation ID for idempotency |
| `created_at` | 5 | uint | Creation timestamp (Unix ms) |
| `expiry_ms` | 6 | uint | Expiry timestamp (Unix ms); gateway rejects if `now > expiry_ms` |

**Command parameter schemas:**

`set_channel`:

| Field | CBOR key | Type | Description |
|---|---|---|---|
| `channel` | 1 | uint | WiFi channel (1–14) |

`scan_channels`: empty map `{}`.

`reboot_node`:

| Field | CBOR key | Type | Description |
|---|---|---|---|
| `node_id` | 1 | tstr | Target node identifier |

**Behavioral note update (§4):** The existing note 4 ("Admin/operator
workflows remain on `GatewayAdmin`") is amended to note that
`ADMIN_COMMAND` provides a limited remote-admin surface for operations
that must be accessible from the cloud (channel management, remote
reboot). The local gRPC `GatewayAdmin` remains authoritative for the
full command set.

### 5.2 `status_details` extension (§3.3.1)

> **Requirements:** AZH-0703

New fields in the `status_details` map for gateway-scoped `ACTUAL_STATE`.
These use keys 10+ to avoid collision with escrow keys 1–4 defined in
evolve-887.

| Field | CBOR key | Type | Description |
|---|---|---|---|
| `modem_connected` | 10 | bool | Modem USB-CDC connection state |
| `modem_channel` | 11 | uint/null | Current WiFi channel |
| `modem_mac` | 12 | tstr/null | Modem MAC address (colon-separated hex) |
| `scan_results` | 13 | array/null | Array of maps: `{1: channel(uint), 2: rssi(int), 3: ssid(tstr)}` |
| `scan_timestamp` | 14 | uint/null | Unix ms of last completed scan |

### 5.3 Ephemeral program in desired-state translation

> **Requirements:** WEB-1301

The Azure handler's desired-state reconciliation (which translates
`desiredstate` table rows into CBOR `DESIRED_STATE` messages) MUST
include `desired_ephemeral_program_hash` → CBOR key 3 when the column
is present and non-empty in the `desiredstate` table row.

---

## 6. Gateway Design Changes (`gateway-design.md`)

### 6.1 ADMIN_COMMAND processing

> **Requirements:** AZH-0701 (gateway-side handling)

The gateway's `ConnectorService` gains a new match arm for msg_type
`0x20` (`ADMIN_COMMAND`):

1. **Validate expiry**: reject if `now > expiry_ms`. Log warning.
2. **Dedup by operation_id**: reject if operation_id was already processed
   (same dedup table used for `MASTER_KEY_INSTALL`).
3. **Dispatch by command**:
   - `set_channel` → call `modem.set_channel(params.channel)` via
     USB-CDC. On success, emit gateway-scoped `ACTUAL_STATE` with
     updated `modem_channel` in `status_details`.
   - `scan_channels` → call `modem.scan()` via USB-CDC. On completion,
     emit gateway-scoped `ACTUAL_STATE` with `scan_results` and
     `scan_timestamp` in `status_details`.
   - `reboot_node` → call `session_manager.queue_reboot(node_id)`.
     Same behavior as the existing gRPC `QueueReboot`.
4. **Unknown commands**: log warning and discard (silent, per security
   design).

### 6.2 Gateway-scoped ACTUAL_STATE modem fields

The gateway MUST periodically (and on change) emit gateway-scoped
`ACTUAL_STATE` with modem status in `status_details`:

- On modem connect/disconnect: emit with `modem_connected` updated.
- On channel change: emit with `modem_channel` updated.
- On scan completion: emit with `scan_results` and `scan_timestamp`.
- On startup: emit current modem status.

---

## 7. Azure Handler Design Changes

### 7.1 New HTTP routes

> **Requirements:** AZH-0700, AZH-0701, AZH-0702

The `main.rs` axum router gains:

```rust
.route("/ProgramRemove/{hash}", delete(program_remove))
.route("/AdminCommand", post(admin_command))
.route("/KeyRotate", post(key_rotate))
```

Note: Azure Functions custom handler routing uses capitalized path
segments matching the `function.json` bindings. The SPA calls
`/api/programs/{hash}`, `/api/admin/command`, and `/api/keys/rotate`
which Azure Functions maps to the above internal routes.

Each handler:
1. Extracts the HTTP trigger envelope (`extract_http_trigger_body`).
2. Validates the request.
3. Performs the operation (table delete, queue enqueue).
4. Returns an Azure Functions HTTP output binding response.

### 7.2 `handle_program_remove` (AZH-0700)

```rust
pub async fn handle_program_remove(
    &self,
    program_hash: &str,
) -> Result<(), ProgramRemoveError>
```

1. Validate `program_hash` is 64 hex chars.
2. Delete entity `PartitionKey="program"`, `RowKey=program_hash` from
   the `programs` table.
3. Return `Ok(())` on success, `NotFound` on 404, `BadRequest` on
   invalid hash.

### 7.3 `handle_admin_command` (AZH-0701)

```rust
pub async fn handle_admin_command(
    &self,
    body: &serde_json::Value,
) -> Result<AdminCommandResponse, AdminCommandError>
```

1. Parse `command` and `params` from JSON body.
2. Validate command name and params (channel range, node_id presence).
3. Generate `operation_id` (16 random bytes), `created_at`, `expiry_ms`.
4. Encode CBOR `ADMIN_COMMAND` message (msg_type `0x20`).
5. Publish to downstream queue via existing `DownstreamPublisher`.
6. Return `AdminCommandResponse { operation_id }`.

### 7.4 `handle_key_rotate` (AZH-0702)

```rust
pub async fn handle_key_rotate(
    &self,
    body: &serde_json::Value,
) -> Result<KeyRotateResponse, KeyRotateError>
```

1. Parse and validate all `MASTER_KEY_INSTALL` fields from JSON.
2. Base64-decode binary fields; validate byte lengths.
3. Encode CBOR `MASTER_KEY_INSTALL` message (msg_type `0x13`) per
   evolve-887 §3.9.
4. Publish to downstream queue.
5. Return `KeyRotateResponse { operation_id }`.

### 7.5 Gateway-scoped ACTUAL_STATE storage (AZH-0703)

When the handler receives a gateway-scoped `ACTUAL_STATE` (entity_kind
`"gateway"`), it stores/upserts a row in the `actualstate` table:

- `PartitionKey = "gw:status"`
- `RowKey` = reverse-timestamp format (same as node rows)
- Columns from `status_details`:
  - `escrow_state` (string) — key 1
  - `escrow_key_version` (int64) — key 2
  - `modem_connected` (boolean) — key 10
  - `modem_channel` (int32) — key 11
  - `modem_mac` (string) — key 12
  - `scan_results` (string, JSON-encoded array) — key 13
  - `scan_timestamp` (int64) — key 14

### 7.6 Azure Functions bindings

New `function.json` files for the three new HTTP triggers:

**ProgramRemove/function.json:**
```json
{
  "bindings": [
    {
      "authLevel": "anonymous",
      "type": "httpTrigger",
      "direction": "in",
      "name": "req",
      "methods": ["delete"],
      "route": "programs/{hash}"
    },
    { "type": "http", "direction": "out", "name": "$return" }
  ]
}
```

**AdminCommand/function.json:**
```json
{
  "bindings": [
    {
      "authLevel": "anonymous",
      "type": "httpTrigger",
      "direction": "in",
      "name": "req",
      "methods": ["post"],
      "route": "admin/command"
    },
    { "type": "http", "direction": "out", "name": "$return" }
  ]
}
```

**KeyRotate/function.json:**
```json
{
  "bindings": [
    {
      "authLevel": "anonymous",
      "type": "httpTrigger",
      "direction": "in",
      "name": "req",
      "methods": ["post"],
      "route": "keys/rotate"
    },
    { "type": "http", "direction": "out", "name": "$return" }
  ]
}
```

All use `authLevel: "anonymous"` — authentication is delegated to
EasyAuth (§9.4 in web-ui-design.md).

---

## 8. Validation Changes (`web-ui-validation.md`)

### New test entries

| Test ID | Requirement | Description | Method | Status |
|---|---|---|---|---|
| T-WEB-1001 | WEB-1000 | Gateway tab appears in nav bar and requires authentication | Manual | Planned |
| T-WEB-1002 | WEB-1001 | Modem status displays connection, channel, MAC from `gw:status` row | Manual | Planned |
| T-WEB-1003 | WEB-1001 | Modem status shows "No modem data" when no `gw:status` row exists | Manual | Planned |
| T-WEB-1004 | WEB-1002 | Set channel sends `POST /api/admin/command` with correct body | Manual | Planned |
| T-WEB-1005 | WEB-1002 | Channel selector validates range 1–14 | Manual | Planned |
| T-WEB-1006 | WEB-1003 | Scan button sends `POST /api/admin/command` scan_channels | Manual | Planned |
| T-WEB-1007 | WEB-1003 | Scan results render in table when available | Manual | Planned |
| T-WEB-1101 | WEB-1101 | Fingerprint displays 6 BIP-39 words matching admin CLI output | Manual | Planned |
| T-WEB-1102 | WEB-1101 | Fingerprint shows "No recovery key" when pubkey row absent | Manual | Planned |
| T-WEB-1103 | WEB-1102 | Escrow status displays all 5 lifecycle states with correct badges | Manual | Planned |
| T-WEB-1104 | WEB-1102 | Warning banner shown when escrow state is not `ready` | Manual | Planned |
| T-WEB-1105 | WEB-1102 | KDF params display from salt row; "No KDF salt" when absent | Manual | Planned |
| T-WEB-1106 | WEB-1103 | Rotation wizard disabled when pubkey or salt missing | Manual | Planned |
| T-WEB-1107 | WEB-1103 | Wizard validates passphrase length ≥ 12 and confirmation match | Manual | Planned |
| T-WEB-1108 | WEB-1103 | Wizard performs Argon2id + X25519 + AES-256-GCM and sends to handler | Integration | Planned |
| T-WEB-1109 | WEB-1103 | Wizard shows spinner during Argon2id derivation | Manual | Planned |
| T-WEB-1110 | WEB-1103 | Success closes wizard and refreshes status | Manual | Planned |
| T-WEB-1111 | WEB-1103 | Failure shows error and does not close wizard | Manual | Planned |
| T-WEB-1112 | WEB-1103 | CDN scripts use SRI hashes | Inspection | Planned |
| T-WEB-1201 | WEB-1200 | Program delete button present on each row | Manual | Planned |
| T-WEB-1202 | WEB-1200 | Delete confirmation dialog shown | Manual | Planned |
| T-WEB-1203 | WEB-1200 | Successful delete refreshes list and shows toast | Manual | Planned |
| T-WEB-1204 | WEB-1200 | Failed delete shows error toast | Manual | Planned |
| T-WEB-1301 | WEB-1300 | Reboot button sends admin command and shows confirmation | Manual | Planned |
| T-WEB-1302 | WEB-1300 | Reboot confirmation dialog shown before sending | Manual | Planned |
| T-WEB-1303 | WEB-1301 | Ephemeral dropdown shows only ephemeral-profiled programs | Manual | Planned |
| T-WEB-1304 | WEB-1301 | Ephemeral submission writes `desired_ephemeral_program_hash` to table | Integration | Planned |

### Azure handler test entries

| Test ID | Requirement | Description | Method | Status |
|---|---|---|---|---|
| T-AZH-0701 | AZH-0700 | `DELETE /api/programs/{hash}` removes row; 404 on missing | Unit (Rust) | Planned |
| T-AZH-0702 | AZH-0700 | Invalid hash (non-hex, wrong length) returns 400 | Unit (Rust) | Planned |
| T-AZH-0703 | AZH-0701 | `POST /api/admin/command` set_channel enqueues ADMIN_COMMAND | Unit (Rust) | Planned |
| T-AZH-0704 | AZH-0701 | Invalid channel (0, 15, negative) returns 400 | Unit (Rust) | Planned |
| T-AZH-0705 | AZH-0701 | `POST /api/admin/command` reboot_node enqueues with node_id | Unit (Rust) | Planned |
| T-AZH-0706 | AZH-0701 | Unknown command type returns 400 | Unit (Rust) | Planned |
| T-AZH-0707 | AZH-0702 | `POST /api/keys/rotate` constructs MASTER_KEY_INSTALL msg | Unit (Rust) | Planned |
| T-AZH-0708 | AZH-0702 | Invalid/missing fields in rotate body return 400 | Unit (Rust) | Planned |
| T-AZH-0709 | AZH-0703 | Gateway-scoped ACTUAL_STATE stored with modem columns | Unit (Rust) | Planned |
| T-AZH-0710 | AZH-0703 | Missing status_details fields stored as null | Unit (Rust) | Planned |

### Gateway validation entries (ADMIN_COMMAND processing)

| Test ID | Requirement | Description | Method | Status |
|---|---|---|---|---|
| T-GW-2100 | AZH-0701 | Gateway processes `set_channel` ADMIN_COMMAND and dispatches to modem | Integration | Planned |
| T-GW-2101 | AZH-0701 | Gateway rejects expired ADMIN_COMMAND (`now > expiry_ms`) | Unit (Rust) | Planned |
| T-GW-2102 | AZH-0701 | Gateway deduplicates ADMIN_COMMAND by `operation_id` | Unit (Rust) | Planned |
| T-GW-2103 | AZH-0701 | Gateway processes `scan_channels` and emits scan results in ACTUAL_STATE | Integration | Planned |
| T-GW-2104 | AZH-0701 | Gateway processes `reboot_node` and queues reboot | Unit (Rust) | Planned |
| T-GW-2105 | AZH-0701 | Gateway discards unknown command type silently | Unit (Rust) | Planned |
| T-GW-2106 | AZH-0703 | Gateway emits modem status in gateway-scoped ACTUAL_STATE status_details | Integration | Planned |
| T-AZH-0711 | AZH-0701 | `POST /api/admin/command` scan_channels enqueues ADMIN_COMMAND | Unit (Rust) | Planned |
| T-AZH-0712 | WEB-1301 | Handler includes CBOR key 3 in DESIRED_STATE when `desired_ephemeral_program_hash` present | Unit (Rust) | Planned |
| T-WEB-1205 | WEB-1200 | Delete warning shown when program is referenced by desired-state rows | Manual | Planned |
| T-WEB-1113 | WEB-1103 | Key material not persisted to localStorage/sessionStorage/cookies | Inspection | Planned |
