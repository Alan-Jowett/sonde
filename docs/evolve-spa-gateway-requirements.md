<!-- SPDX-License-Identifier: MIT -->
<!-- Copyright (c) 2026 sonde contributors -->
# Requirements Patch: SPA Gateway Configuration

> **Status:** Draft
> **Scope:** New web UI requirements for gateway configuration features
> currently available only in sonde-admin CLI.
> **Traceability:** USER-REQUEST: "add support to the SPA for configuring
> the gateway — program remove, reboot, ephemeral, modem set-channel,
> modem scan, modem status, key fingerprint, key status, key rotate"

---

## Change Manifest

| ID | Type | Summary |
|---|---|---|
| WEB-1000 | NEW | Gateway Settings tab |
| WEB-1001 | NEW | Modem status display |
| WEB-1002 | NEW | Modem channel set |
| WEB-1003 | NEW | Modem channel scan |
| WEB-1100 | NEW | Key escrow section in Gateway tab |
| WEB-1101 | NEW | Key fingerprint display |
| WEB-1102 | NEW | Key/escrow status display |
| WEB-1103 | NEW | Key rotation wizard |
| WEB-1200 | NEW | Program remove |
| WEB-1300 | NEW | Node reboot action |
| WEB-1301 | NEW | Ephemeral program dispatch |
| AZH-0700 | NEW | Program remove endpoint |
| AZH-0701 | NEW | Admin command relay endpoint |
| AZH-0702 | NEW | Key rotation relay endpoint |
| AZH-0703 | NEW | Modem status in gateway-scoped ACTUAL_STATE |

---

## New Requirements — Web UI (WEB-1000 series)

### WEB-1000  Gateway Settings tab

**Priority:** Must
**Source:** USER-REQUEST

**Description:**
The SPA MUST add a new top-level tab "Gateway" in the navigation bar. This
tab hosts modem status/controls and key escrow information. It follows the
same authentication gate as other tabs (requires MSAL sign-in).

**Acceptance criteria:**

1. "Gateway" appears as a tab in the navigation bar after "Sensor Data".
2. Tab requires authentication (same as Dashboard, Desired State, Programs).
3. Tab contains two sections: "Modem" and "Key Escrow".

---

### WEB-1001  Modem status display

**Priority:** Must
**Source:** USER-REQUEST, mirrors `sonde-admin modem status`

**Description:**
The Gateway tab MUST display the modem's current status: connected/disconnected,
current WiFi channel, and MAC address. This data is read from
the gateway-scoped row in the `actualstate` Azure Table
(PartitionKey=`"gw:status"`, latest row by reverse-timestamp RowKey).

The gateway publishes modem status in gateway-scoped `ACTUAL_STATE` via
`status_details` sub-keys (see AZH-0703). The Azure handler stores these
as columns in the `actualstate` table.

**Acceptance criteria:**

1. Modem status section displays: connection state, WiFi channel, MAC address.
2. Data is read from the `actualstate` table, gateway-scoped partition
   (`PartitionKey = "gw:status"`).
3. Auto-refreshes with the same interval as the Dashboard.
4. Gracefully shows "No modem data" when no gateway-scoped row exists.

---

### WEB-1002  Modem channel set

**Priority:** Must
**Source:** USER-REQUEST, mirrors `sonde-admin modem set-channel`

**Description:**
The Gateway tab MUST provide a control to set the modem's WiFi channel. The
SPA sends a `POST /api/admin/command` request to the Azure Function handler
with command type `set_channel`. The handler constructs an `ADMIN_COMMAND`
connector message (msg_type `0x20`) and enqueues it on the downstream queue.

**Acceptance criteria:**

1. Channel selector shows valid WiFi channels (1–14).
2. Submit sends `POST /api/admin/command` with
   `{"command": "set_channel", "params": {"channel": N}}`.
3. Success shows "Channel change requested" confirmation.
4. Failure shows error message.
5. Uses Function App-scoped bearer token (same as program ingest).
6. Updated channel appears in modem status on next auto-refresh.

---

### WEB-1003  Modem channel scan

**Priority:** Should
**Source:** USER-REQUEST, mirrors `sonde-admin modem scan`

**Description:**
The Gateway tab SHOULD provide a button to trigger a WiFi channel scan. The
SPA sends a `POST /api/admin/command` request with command type `scan_channels`.
Since scan is asynchronous, the SPA shows a "scan requested" confirmation.
Scan results are published by the gateway in gateway-scoped `ACTUAL_STATE`
and appear on the next auto-refresh.

**Acceptance criteria:**

1. "Scan Channels" button sends `POST /api/admin/command` with
   `{"command": "scan_channels"}`.
2. Success shows "Scan requested" confirmation.
3. Scan results (channel + RSSI per AP) display in a table when available
   in the gateway-scoped `actualstate` row.
4. Uses Function App-scoped bearer token.

---

### WEB-1100  Key escrow section

**Priority:** Must
**Source:** USER-REQUEST

**Description:**
The Gateway tab MUST include a "Key Escrow" section displaying fingerprint,
status, and a rotation control. Data is read from the `gatewayescrow` and
`actualstate` Azure Tables.

**Acceptance criteria:**

1. Key Escrow section is visible in the Gateway tab.
2. Section contains fingerprint display, status summary, and rotation button.

---

### WEB-1101  Key fingerprint display

**Priority:** Must
**Source:** USER-REQUEST, mirrors `sonde-admin key fingerprint`

**Description:**
The Key Escrow section MUST display the gateway's recovery public key
fingerprint as a 6-word BIP-39 mnemonic. The public key is read from the
`gatewayescrow` table (PartitionKey=`"gateway"`, RowKey=`"pubkey"`,
column `public_key` base64-encoded 32 bytes).

The fingerprint is computed client-side using the exact algorithm from
the escrow specification (evolve-887-specification.md §20.10):

1. `hash = SHA-256(public_key_bytes)` via Web Crypto `SubtleCrypto.digest()`.
2. Pack `hash[0..9]` (9 bytes) into a `BigInt` or equivalent 72+ bit integer.
3. Extract six 11-bit indices from the most-significant 66 bits:
   `index[i] = (bits >> (72 - 11 - 11*i)) & 0x7FF` for i in 0..6.
4. Look up each index in the BIP-39 English wordlist (2048 entries).

**Acceptance criteria:**

1. Fingerprint displays as six words (e.g., "abandon ability able …").
2. SHA-256 uses Web Crypto `SubtleCrypto.digest()`.
3. BIP-39 English wordlist is embedded in the SPA JS source.
4. Produces identical output to `sonde-admin key fingerprint` and the
   gateway's modem OLED display for the same public key.
5. Shows "No recovery key published" when no pubkey row exists.

---

### WEB-1102  Key/escrow status display

**Priority:** Must
**Source:** USER-REQUEST

**Description:**
The Key Escrow section MUST display the gateway's escrow lifecycle state,
current key version, and KDF parameters. This is a subset of the admin
CLI's `key status` output — the SPA omits escrowed PSK counts (which
require scanning the full `actualstate` table and are better suited to
CLI batch queries).

Data sources:

- Escrow state and key version: `gatewayescrow` table,
  PartitionKey=`"gateway"`, RowKey=`"state"`:
  - Column `escrow_state` (string). Per evolve-887, valid values are:
    `disabled`, `bootstrapping`, `ready`, `rotation_in_progress`,
    `degraded`.
  - Column `escrow_key_version` (int64).
- KDF parameters: `gatewayescrow` table, PartitionKey=`"gateway"`,
  RowKey=`"salt"`:
  - Column `kdf_params_json` (JSON string containing `m_cost`, `t_cost`,
    `p_cost`, `kdf_version`).

**Acceptance criteria:**

1. Displays escrow state with appropriate badge:
   - `ready` → green/success badge
   - `bootstrapping`, `rotation_in_progress` → yellow/warning badge
   - `degraded` → red/error badge
   - `disabled` → grey/muted badge
   - missing → "Unknown" with grey badge
2. Displays current key version (uint) or "—" if absent.
3. Displays KDF parameters (Argon2id m_cost, t_cost, p_cost) from salt row,
   or "No KDF salt configured" if salt row is absent.
4. Shows warning banner when escrow state is not `ready`.

---

### WEB-1103  Key rotation wizard

**Priority:** Must
**Source:** USER-REQUEST, mirrors `sonde-admin key rotate`

**Description:**
The Key Escrow section MUST provide a "Rotate Key" button that opens a modal
wizard for master key rotation. The wizard:

1. Reads the gateway's recovery public key from `gatewayescrow` table
   (PartitionKey=`"gateway"`, RowKey=`"pubkey"`).
2. Reads the KDF salt and parameters from `gatewayescrow` table
   (PartitionKey=`"gateway"`, RowKey=`"salt"`).
3. Displays the key fingerprint for out-of-band verification.
4. Prompts the operator for a passphrase (with confirmation field).
5. Derives the master key using Argon2id with parameters from the salt row:
   - Output length: 32 bytes
   - Salt: from `gatewayescrow` salt row (base64-decoded, 16 bytes)
   - m_cost, t_cost, p_cost: from `kdf_params_json`
   - Uses CDN-hosted `argon2-browser` (or equivalent) WASM library.
6. Generates an ephemeral X25519 keypair using CDN-hosted `@noble/curves`
   (or equivalent).
7. Encrypts the master key per evolve-887 §20.7:
   - `shared_secret = X25519(ephemeral_private_key, gateway_public_key)`
   - `key = HKDF-SHA-256(shared_secret, salt="sonde-escrow-v1",
      info=target_key_epoch_be || operation_id)`
   - `ciphertext, tag = AES-256-GCM(key, nonce, master_key,
      aad=operation_id || target_key_epoch_be)`
   - HKDF uses Web Crypto; AES-256-GCM uses Web Crypto.
8. Sends the encrypted payload to `POST /api/keys/rotate` (see AZH-0702).
9. Shows success/failure result.

**Acceptance criteria:**

1. Wizard validates passphrase length (minimum 12 characters).
2. Passphrase confirmation must match.
3. Argon2id derivation runs in-browser; spinner shown during computation.
4. X25519 key exchange uses `@noble/curves/ed25519` (x25519 export) or
   equivalent constant-time library.
5. HKDF-SHA-256 and AES-256-GCM use Web Crypto API (`SubtleCrypto`).
6. CDN scripts MUST use Subresource Integrity (SRI) hashes.
7. Wizard is disabled when no recovery public key or salt exists.
8. On success, wizard closes and status refreshes.
9. On failure, wizard shows error and does not close.
10. Key material (passphrase, derived key, shared secret) is never persisted
    to `localStorage`, `sessionStorage`, or cookies. JS cannot guarantee
    memory zeroization, but the SPA MUST null references after use.
11. `operation_id`: 16 random bytes via `crypto.getRandomValues()`.
12. `rotation_counter`: read from `gatewayescrow` table
    (PartitionKey=`"gateway"`, RowKey=`"state"`, column
    `escrow_key_version`). Use `escrow_key_version + 1` as the
    rotation counter. If no state row exists, use `1`.
13. `expiry_ms`: `Date.now() + 300_000` (5-minute expiry).

---

## New Requirements — Web UI (WEB-1200 series)

### WEB-1200  Program remove

**Priority:** Must
**Source:** USER-REQUEST, mirrors `sonde-admin program remove`

**Description:**
The Programs tab MUST provide a delete button for each program in the
programs table. Clicking delete sends `DELETE /api/programs/{hash}` to the
Azure Function handler, which removes the program from the `programs` table.

If the program is currently assigned to one or more nodes (referenced in
`desiredstate` table rows), the delete button SHOULD show a warning but
MUST NOT block deletion. The operator is responsible for reassigning nodes.

**Acceptance criteria:**

1. Each program row has a delete button (🗑️ icon or "Delete" text).
2. Delete requires confirmation (browser `confirm()` dialog).
3. On success, program list refreshes and shows confirmation message.
4. On failure, error message is displayed.
5. Uses Function App-scoped bearer token.

---

## New Requirements — Web UI (WEB-1300 series)

### WEB-1300  Node reboot action

**Priority:** Must
**Source:** USER-REQUEST, mirrors `sonde-admin reboot`

**Description:**
The Dashboard tab MUST provide a "Reboot" action for each node. Clicking
sends `POST /api/admin/command` with command type `reboot_node` and the
target `node_id`. The handler constructs an `ADMIN_COMMAND` connector
message and enqueues it on the downstream queue.

**Acceptance criteria:**

1. Each node row has a "Reboot" button or action menu entry.
2. Reboot requires confirmation dialog.
3. On success, shows "Reboot queued" message.
4. On failure, shows error message.
5. Uses Function App-scoped bearer token.

---

### WEB-1301  Ephemeral program dispatch

**Priority:** Must
**Source:** USER-REQUEST, mirrors `sonde-admin ephemeral`

**Description:**
The Desired State tab MUST be extended with an optional "Ephemeral Program"
field. When set, the desired-state row includes
`desired_ephemeral_program_hash`. The existing Azure handler desired-state
reconciliation path translates this to CBOR key 3
(`ephemeral_program_hash`) in the node-scoped `DESIRED_STATE` connector
message.

Note: The Azure companion/handler already translates `desiredstate` table
rows into CBOR `DESIRED_STATE` messages for assigned programs (keys 1, 5–8).
The ephemeral field (key 3) MUST be added to this translation path if not
already present.

Ephemeral dispatch follows one-shot semantics: the gateway clears the
ephemeral program hash from its local desired state after successfully
queuing the program for execution. Subsequent `DESIRED_STATE` messages
without `ephemeral_program_hash` (key 3) do not re-trigger the ephemeral
run.

**Acceptance criteria:**

1. Desired State form gains an optional "Ephemeral Program" dropdown,
   filtered to programs with `verification_profile = "ephemeral"`.
2. When selected, submitting writes a desired-state row with
   `desired_ephemeral_program_hash` set to the selected program hash.
3. On success, shows confirmation message.
4. The Azure handler includes CBOR key 3 in `DESIRED_STATE` when the
   `desiredstate` row contains `desired_ephemeral_program_hash`.
5. On failure, shows error message.

---

## New Requirements — Azure Handler (AZH-0700 series)

### AZH-0700  Program remove endpoint

**Priority:** Must
**Source:** WEB-1200

**Description:**
The Azure Function handler MUST expose a `DELETE /api/programs/{hash}`
HTTP endpoint that removes a program from the `programs` Azure Table.

**Acceptance criteria:**

1. Validates `hash` is a 64-character lowercase hex string.
2. Deletes row with `PartitionKey="program"`, `RowKey=hash` from the
   `programs` table.
3. Returns 200 on success with `{"deleted": true, "program_hash": "..."}`.
4. Returns 404 if program not found, 400 if hash is invalid.
5. Protected by EasyAuth (same as `ProgramIngest`).

---

### AZH-0701  Admin command relay endpoint

**Priority:** Must
**Source:** WEB-1002, WEB-1003, WEB-1300

**Description:**
The Azure Function handler MUST expose a `POST /api/admin/command` HTTP
endpoint that accepts imperative admin commands and relays them to the
gateway via a new `ADMIN_COMMAND` connector message (msg_type `0x20`).

This uses a new connector message type rather than `DESIRED_STATE` because
these are one-shot imperative commands (reboot, scan) that do not fit the
declarative complete-replacement semantics of `DESIRED_STATE`. Each command
carries an `operation_id` for idempotency and correlation, following the
same pattern as `MASTER_KEY_INSTALL` (msg_type `0x13`).

**Request body:**

```json
{
  "command": "set_channel" | "scan_channels" | "reboot_node",
  "params": { ... }
}
```

**Command parameters:**

| Command | Params | Description |
|---|---|---|
| `set_channel` | `{"channel": N}` (1–14) | Set modem WiFi channel |
| `scan_channels` | `{}` (none) | Trigger WiFi channel scan |
| `reboot_node` | `{"node_id": "..."}` | Queue reboot for a node |

**`ADMIN_COMMAND` connector message (msg_type `0x20`):**

| Field | CBOR key | Type | Description |
|---|---|---|---|
| `msg_type` | 1 | uint | `0x20` |
| `command` | 2 | tstr | Command name |
| `params` | 3 | map | Command-specific parameters (CBOR) |
| `operation_id` | 4 | bstr (16 bytes) | Unique operation ID |
| `created_at` | 5 | uint | Creation timestamp (Unix ms) |
| `expiry_ms` | 6 | uint | Expiry timestamp (Unix ms) |

**Acceptance criteria:**

1. Validates `command` is one of the known command types.
2. Validates `params` per command type (e.g., channel in 1–14).
3. Generates `operation_id` (16 random bytes) and `expiry_ms`
   (`created_at + 300_000`).
4. Constructs the CBOR `ADMIN_COMMAND` message.
5. Enqueues on the downstream connector queue.
6. Returns 202 (Accepted) with `{"operation_id": "hex", "command": "..."}`.
7. Returns 400 on validation failure with error details.
8. Protected by EasyAuth.

---

### AZH-0702  Key rotation relay endpoint

**Priority:** Must
**Source:** WEB-1103

**Description:**
The Azure Function handler MUST expose a `POST /api/keys/rotate`
HTTP endpoint that accepts a pre-encrypted `MASTER_KEY_INSTALL` payload
from the SPA and relays it to the gateway via the connector downstream queue.
The handler MUST NOT decrypt or inspect the encrypted master key.

**Request body (JSON):**

```json
{
  "target_key_epoch": <uint>,
  "sender_public_key": "<base64, 32 bytes>",
  "encrypted_master_key": "<base64>",
  "nonce": "<base64, 12 bytes>",
  "tag": "<base64, 16 bytes>",
  "operation_id": "<base64, 16 bytes>",
  "rotation_counter": <uint>,
  "expiry_ms": <uint>
}
```

**Acceptance criteria:**

1. Validates all required fields are present with correct types and lengths.
2. Base64-decodes binary fields; validates byte lengths.
3. Constructs a `MASTER_KEY_INSTALL` connector message (msg_type `0x13`)
   per the schema in evolve-887 §3.9.
4. Enqueues on the downstream connector queue.
5. Returns 202 (Accepted) with `{"operation_id": "hex"}`.
6. Returns 400 on validation failure.
7. Protected by EasyAuth.

---

### AZH-0703  Modem status in gateway-scoped ACTUAL_STATE

**Priority:** Must
**Source:** WEB-1001

**Description:**
The Azure handler MUST store modem status fields from gateway-scoped
`ACTUAL_STATE` messages. The gateway publishes modem status in
`status_details` sub-keys. The handler stores these as columns in the
`actualstate` table under the gateway-scoped partition
(PartitionKey=`"gw:status"`).

**New `status_details` fields (gateway-scoped ACTUAL_STATE):**

| Field | CBOR key (in `status_details`) | Type | Description |
|---|---|---|---|
| `modem_connected` | 10 | bool | Modem USB-CDC connection state |
| `modem_channel` | 11 | uint/null | Current WiFi channel |
| `modem_mac` | 12 | tstr/null | Modem MAC address |
| `scan_results` | 13 | array/null | Array of `{channel: uint, rssi: int, ssid: tstr}` |
| `scan_timestamp` | 14 | uint/null | Unix ms of last scan |

Note: These use `status_details` CBOR keys 10+ to avoid collision with
escrow keys 1–4 defined in evolve-887.

**Acceptance criteria:**

1. Handler extracts modem fields from `status_details` in gateway-scoped
   `ACTUAL_STATE`.
2. Stores them as columns in `actualstate` table with
   PartitionKey=`"gw:status"`.
3. SPA can read these columns to display modem status.
4. Missing fields are stored as null/empty.

---

## Companion API Extensions

### New connector message type: `ADMIN_COMMAND` (msg_type `0x20`)

Imperative commands from the admin SPA use a dedicated message type rather
than `DESIRED_STATE`. This avoids semantic mismatch — `DESIRED_STATE` uses
complete-replacement semantics unsuitable for one-shot operations (reboot,
scan). It also avoids CBOR key collisions with escrow fields already
defined in gateway `DESIRED_STATE` (keys 1–2 per evolve-887).

The gateway MUST process `ADMIN_COMMAND` messages by dispatching to the
appropriate handler based on `command`:

| Command | Gateway action |
|---|---|
| `set_channel` | Calls modem `SetChannel` over USB-CDC |
| `scan_channels` | Calls modem `ScanChannels` over USB-CDC; publishes results in next `ACTUAL_STATE` `status_details` |
| `reboot_node` | Queues reboot for the specified node (same as gRPC `QueueReboot`) |

The gateway MUST validate `expiry_ms` and reject expired commands. The
gateway MUST deduplicate by `operation_id` (same pattern as
`MASTER_KEY_INSTALL`).

### Gateway-scoped `ACTUAL_STATE` `status_details` extension

New sub-keys for modem status (keys 10–14) are added alongside the
existing escrow sub-keys (keys 1–4 from evolve-887). See AZH-0703 for
the schema.

### Ephemeral program in desired-state translation

The Azure handler's desired-state-to-CBOR translation MUST include
`desired_ephemeral_program_hash` → CBOR key 3 when the field is present
in the `desiredstate` table row.

---

## Invariant Impact Assessment

1. **No existing behavior changes.** All new requirements are additive —
   existing Dashboard, Desired State, Programs, and Sensor Data tabs are
   unaffected.
2. **Authentication model preserved.** New endpoints use the same EasyAuth
   + MSAL bearer token pattern as `ProgramIngest`.
3. **No DESIRED_STATE schema changes.** Imperative commands use the new
   `ADMIN_COMMAND` message type (0x20) to avoid collisions with escrow
   keys 1–2 in gateway `DESIRED_STATE` and to respect the declarative
   complete-replacement semantics of `DESIRED_STATE`.
4. **Connector message forward compatibility.** The gateway already ignores
   unknown `msg_type` values. Old gateways will silently discard
   `ADMIN_COMMAND` messages until upgraded.
5. **No new tables.** All data is read from or written to existing tables
   (`actualstate`, `desiredstate`, `programs`, `gatewayescrow`).
6. **CDN dependencies.** Key rotation introduces two new CDN dependencies
   (`argon2-browser` WASM, `@noble/curves` X25519). These follow the
   existing zero-build SPA convention. All CDN scripts MUST use
   Subresource Integrity (SRI) hashes.
7. **JS secret lifetime.** JavaScript cannot guarantee memory zeroization.
   The SPA nulls key material references after use but cannot fully erase
   memory. This is an accepted limitation documented in the requirements.
