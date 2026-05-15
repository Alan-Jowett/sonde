<!-- SPDX-License-Identifier: MIT
     Copyright (c) 2026 sonde contributors -->
# Requirements Patch: PSK Key Escrow for Gateway Replaceability

> **Issue:** #887
> **Status:** Draft — Phase 1 requirements discovery
> **Scope:** New requirements for PSK key escrow across gateway, Azure handler,
> Azure companion, modem, admin CLI, and security model.
> **User intent:** Enable gateway replacement without re-pairing nodes by
> escrowing encrypted PSKs to Azure. Azure never holds plaintext secrets.

---

## Change Manifest

| Action | Component | REQ-IDs | Summary |
|--------|-----------|---------|---------|
| **New** | Gateway | GW-2000–GW-2013 | Asymmetric keypair, escrow lifecycle, key rotation, recovery, bootstrap |
| **New** | Azure Handler | AZH-0600–AZH-0605 | Escrow blob storage, recovery serving, salt/pubkey tables |
| **New** | Admin CLI | ADMIN-0900–ADMIN-0903 | Passphrase entry, KDF, fingerprint verification, key rotation |
| **New** | Connector API | — | New message types for escrow operations |
| **Modify** | Gateway | GW-0601a | Note: master key may now be passphrase-derived (admin-side) |
| **Modify** | Gateway | GW-1002 | Unknown nodes may now trigger recovery instead of silent discard |
| **Modify** | Security model | security.md §2 | Updated key hierarchy, escrow threat model |

---

## New Requirements — Gateway (GW-2000 series)

### GW-2000  Recovery keypair generation

**Priority:** Must
**Source:** Issue #887

**Description:**
The gateway MUST generate an asymmetric keypair at first startup for use in
secure master-key delivery. The keypair MUST be persisted locally (encrypted
at rest with the current master key) so that it survives restarts without
requiring the admin to re-verify the fingerprint. The private key MUST be
wrapped in `Zeroizing` and zeroed on drop.

The recommended construction is X25519 (Curve25519 Diffie-Hellman) combined
with HKDF-SHA-256 and AES-256-GCM for the key-encapsulation step (ECIES /
HPKE-style). The specific algorithm choice is deferred to the design phase.

**Acceptance criteria:**

1. On first startup, a keypair is generated using `getrandom::fill()` for
   randomness.
2. The public key is available for publication via the connector API.
3. The private key is persisted in the gateway's local storage, encrypted
   with the current master key.
4. On subsequent startups, the persisted keypair is loaded (not regenerated).
5. If the private key cannot be decrypted (master key mismatch), the gateway
   generates a new keypair and logs a warning.

---

### GW-2001  Recovery public key publication

**Priority:** Must
**Source:** Issue #887, GW-2000

**Description:**
The gateway MUST publish its recovery public key to the control plane via a
new `KEY_ESCROW_PUBKEY` connector message on startup and whenever the keypair
changes. The message includes the raw public key bytes, a monotonic key epoch
(incremented on each keypair generation), and a creation timestamp.

**Acceptance criteria:**

1. The gateway emits `KEY_ESCROW_PUBKEY` on every startup after keypair load.
2. The message contains: public key bytes, key epoch (uint), creation
   timestamp (uint, Unix milliseconds).
3. If the keypair is regenerated (new epoch), a new `KEY_ESCROW_PUBKEY` is
   emitted.
4. The key epoch is monotonically increasing and persisted across restarts.

---

### GW-2002  Escrow blob format and authenticated binding

**Priority:** Must
**Source:** Issue #887

**Description:**
The gateway MUST define a canonical escrow blob format for encrypted PSKs.
Each blob MUST be AES-256-GCM encrypted with the current master key, with
the following identity fields authenticated as AAD (Additional Authenticated
Data):

- `escrow_version` (uint) — schema version for forward compatibility
- `key_version` (uint) — which master key version encrypted this blob
- `subject_kind` (text) — `"node"` or `"phone"`
- `subject_id` (text) — node ID or phone ID
- `key_hint` (uint16) — the PSK's key_hint value

The blob body (encrypted) contains the raw 32-byte PSK. The nonce MUST be
unique per encryption (generated via `getrandom::fill()`).

The wire format is CBOR-encoded:

| Field | CBOR key | Type | Description |
|-------|----------|------|-------------|
| `escrow_version` | 1 | uint | Schema version (initially `1`) |
| `key_version` | 2 | uint | Master key version that encrypted this blob |
| `subject_kind` | 3 | tstr | `"node"` or `"phone"` |
| `subject_id` | 4 | tstr | Node ID or phone ID |
| `key_hint` | 5 | uint | PSK key_hint value |
| `nonce` | 6 | bstr (12 bytes) | AES-256-GCM nonce |
| `ciphertext` | 7 | bstr (32 bytes) | Encrypted PSK |
| `tag` | 8 | bstr (16 bytes) | AES-256-GCM authentication tag |

**Acceptance criteria:**

1. Escrow blobs can be produced and consumed by the gateway.
2. Decryption with the wrong master key fails with an authentication error.
3. Tampering with any AAD field (subject_kind, subject_id, key_hint,
   key_version, escrow_version) causes decryption failure.
4. Swapping two escrow blobs between different nodes causes decryption
   failure (because AAD differs).
5. The blob is ≤ 150 bytes CBOR-encoded.

---

### GW-2003  Escrow blob emission in ACTUAL_STATE

**Priority:** Must
**Source:** Issue #887, GW-2002

**Description:**
The gateway MUST include the encrypted escrow blob for each node and phone
PSK in `ACTUAL_STATE` connector messages. A new CBOR key is added to the
node-scoped `ACTUAL_STATE` payload:

| Field | CBOR key | Type | Description |
|-------|----------|------|-------------|
| `encrypted_psk_escrow` | 12 | bstr/null | CBOR-encoded escrow blob (GW-2002 format). `null` when escrow is disabled or not yet bootstrapped. |

For phone PSKs, the gateway MUST emit phone-scoped `ACTUAL_STATE` messages
(new `entity_kind = "phone"`) with the same `encrypted_psk_escrow` field.

**Acceptance criteria:**

1. Every `ACTUAL_STATE` for a registered node includes `encrypted_psk_escrow`
   when escrow is enabled.
2. Phone PSKs are emitted as `entity_kind = "phone"` with escrow blobs.
3. When escrow is disabled (no passphrase-derived key rotation has occurred),
   the field is `null`.
4. After key rotation, all escrow blobs are re-emitted with the new
   `key_version`.

---

### GW-2004  Escrow lifecycle state machine

**Priority:** Must
**Source:** Issue #887

**Description:**
The gateway MUST track and expose its escrow readiness via an explicit
lifecycle state:

| State | Description |
|-------|-------------|
| `disabled` | Gateway is using a random master key. No passphrase-derived key rotation has occurred. Escrow blobs are not emitted. |
| `bootstrapping` | Key rotation is in progress. Some PSKs may still be encrypted with the old key version. |
| `ready` | All node and phone PSKs are encrypted with the current passphrase-derived master key and have been emitted to the connector. |
| `rotation_in_progress` | A new key rotation is underway. |
| `degraded` | A rotation was interrupted (crash/restart). Some PSKs are encrypted with a different key version than the current master key. The gateway MUST resume rotation on startup. |

The escrow state MUST be persisted and reported in gateway-scoped
`ACTUAL_STATE` messages.

**Acceptance criteria:**

1. A fresh gateway starts in `disabled` state.
2. After a successful key rotation, the state transitions to `ready`.
3. During rotation, the state is `bootstrapping` or `rotation_in_progress`.
4. If the gateway restarts during rotation, it detects `degraded` state and
   resumes rotation automatically.
5. The escrow state is visible to operators via admin API and connector.

---

### GW-2005  Master key version tracking

**Priority:** Must
**Source:** Issue #887

**Description:**
The gateway MUST maintain a monotonically increasing `key_version` counter.
Each encrypted PSK record in local storage MUST be tagged with the
`key_version` of the master key used to encrypt it. The current
`key_version` MUST be persisted in the gateway's database.

**Acceptance criteria:**

1. Each encrypted PSK record in SQLite includes a `key_version` column.
2. The current `key_version` is stored in a gateway metadata table.
3. `key_version` is monotonically increasing (never decremented).
4. After key rotation, all PSK records are tagged with the new `key_version`.

---

### GW-2006  Key rotation via connector

**Priority:** Must
**Source:** Issue #887, GW-2000

**Description:**
The gateway MUST accept a `MASTER_KEY_INSTALL` connector message containing
a new master key encrypted with the gateway's recovery public key. On
receipt, the gateway:

1. Decrypts the payload with its private key to obtain the new master key.
2. Validates the message includes the correct `target_key_epoch` matching
   the gateway's current public key epoch.
3. Increments `key_version`.
4. Re-encrypts all local PSK records (node + phone) from the old master key
   to the new master key, transactionally.
5. Re-encrypts the recovery private key (`escrow_keypair.secret_enc`) under
   the new master key.
6. Updates the master key in the `KeyProvider` storage.
7. Emits updated escrow blobs via `ACTUAL_STATE` for all PSKs.
8. Transitions escrow state to `ready` (or `rotation_in_progress` → `ready`).
9. Zeroes the old master key from memory.

The `MASTER_KEY_INSTALL` message format:

| Field | CBOR key | Type | Description |
|-------|----------|------|-------------|
| `msg_type` | 1 | uint | New message type TBD |
| `target_key_epoch` | 2 | uint | Must match gateway's current public key epoch |
| `encrypted_master_key` | 3 | bstr | Master key encrypted with gateway's public key |
| `operation_id` | 4 | bstr | Unique operation ID for idempotency |
| `rotation_counter` | 5 | uint | Monotonic rotation counter for replay protection |
| `expiry_ms` | 6 | uint | Message expiry timestamp (Unix ms). Gateway rejects expired messages. |

**Acceptance criteria:**

1. Gateway successfully receives and processes a `MASTER_KEY_INSTALL` message.
2. All local PSKs are re-encrypted with the new master key.
3. The operation is atomic — a crash during re-encryption is recoverable
   (see GW-2008).
4. Messages with wrong `target_key_epoch` are rejected.
5. Duplicate `operation_id` values are rejected (idempotency).
6. Expired messages are rejected.
7. After successful rotation, all escrow blobs are re-emitted.

---

### GW-2007  Crash-safe key rotation

**Priority:** Must
**Source:** Issue #887, GW-2006

**Description:**
Key rotation MUST be crash-safe. The gateway MUST use the following
transactional approach:

1. **Prepare**: Write the new master key (encrypted with the old master key)
   and new `key_version` to a `pending_rotation` metadata record.
2. **Migrate**: For each PSK record, decrypt with old key, re-encrypt with
   new key, write with new `key_version`. Each record update is individually
   committed (the `key_version` tag on each record tracks progress).
3. **Commit**: Update the active master key, remove the `pending_rotation`
   record, update escrow state.

On startup, if a `pending_rotation` record exists, the gateway MUST resume
migration from where it left off (records with old `key_version` still need
migration).

During the migration window, the gateway MUST be able to decrypt PSKs
encrypted with either the old or new `key_version` (two-key window).

**Acceptance criteria:**

1. A crash at any point during rotation leaves the database in a consistent
   state.
2. On restart after a crash, the gateway detects the incomplete rotation and
   resumes automatically.
3. During migration, nodes can still authenticate (both old and new key
   versions are supported for decryption).
4. After migration completes, only the new key version is active.

---

### GW-2008  Salt management

**Priority:** Must
**Source:** Issue #887

**Description:**
The gateway MUST manage a KDF salt record containing:

- `salt` (16 bytes, random)
- `argon2id_m_cost` (uint, memory cost in KiB — default 65536 = 64 MiB)
- `argon2id_t_cost` (uint, time cost / passes — default 3)
- `argon2id_p_cost` (uint, parallelism — default 1)
- `kdf_version` (uint, schema version — initially 1)
- `created_at` (uint, Unix milliseconds)

The salt record is stored locally in the gateway database and published
to Azure via the connector. The strategy is **first-writer-wins**:

- If no salt exists locally or in Azure, the gateway generates one and
  publishes it.
- If Azure has a salt but the gateway does not, the gateway adopts Azure's
  salt.
- If both exist and differ, the gateway logs a warning and uses the local
  salt (local is authoritative for the running instance).

**Acceptance criteria:**

1. Salt is generated using `getrandom::fill()`.
2. Salt record is persisted in the gateway database.
3. Salt record is published via connector on startup and after generation.
4. Salt is adopted from Azure when local salt is absent.
5. Conflict detection logs a warning when local and Azure salts differ.

---

### GW-2009  Unknown node recovery request

**Priority:** Must
**Source:** Issue #887

**Description:**
When the gateway receives a frame from a `key_hint` for which no local PSK
exists, and escrow is in `ready` state, the gateway MUST emit a
`KEY_ESCROW_REQUEST` connector message requesting the escrowed PSK(s) for
that `key_hint`. The gateway MUST NOT emit recovery requests when escrow is
`disabled`.

Because `key_hint` is only 16 bits and collisions are possible, the recovery
response may contain multiple candidate PSKs. The gateway trial-decrypts the
original frame against each candidate.

**Rate limiting:** The gateway MUST rate-limit recovery requests per
`key_hint` to prevent radio-triggered amplification attacks. A reasonable
default is at most 1 request per `key_hint` per 60 seconds.

`KEY_ESCROW_REQUEST` message format:

| Field | CBOR key | Type | Description |
|-------|----------|------|-------------|
| `msg_type` | 1 | uint | New message type TBD |
| `key_hint` | 2 | uint | The key_hint from the undecryptable frame |
| `request_id` | 3 | bstr | Unique request ID for correlation |

**Acceptance criteria:**

1. An unknown `key_hint` in `ready` state triggers a `KEY_ESCROW_REQUEST`.
2. The same `key_hint` does not trigger more than 1 request per 60 seconds.
3. In `disabled` state, unknown `key_hint` values are silently discarded
   (existing GW-1002 behavior).
4. The request includes a unique `request_id` for response correlation.

---

### GW-2010  PSK recovery from escrowed blob

**Priority:** Must
**Source:** Issue #887, GW-2009

**Description:**
On receiving a `KEY_ESCROW_RESPONSE` connector message, the gateway MUST:

1. Match the `request_id` to a pending recovery request.
2. For each candidate escrow blob in the response, attempt to decrypt
   using the current master key.
3. For each successfully decrypted PSK, attempt trial-decryption of the
   buffered original frame.
4. If a PSK successfully decrypts the frame, register the node (or phone)
   locally and process the frame normally.
5. If no candidate succeeds, log a warning and discard the frame.

The gateway MUST buffer the original undecryptable frame for a bounded
time (default: 30 seconds) while awaiting the recovery response.

`KEY_ESCROW_RESPONSE` message format:

| Field | CBOR key | Type | Description |
|-------|----------|------|-------------|
| `msg_type` | 1 | uint | New message type TBD |
| `request_id` | 2 | bstr | Correlates to the `KEY_ESCROW_REQUEST` |
| `candidates` | 3 | array of bstr | Array of escrow blobs (GW-2002 format) |
| `key_hint` | 4 | uint | The requested key_hint |

**Acceptance criteria:**

1. A valid recovery response with a matching PSK results in node
   registration and frame processing.
2. Multiple candidates are tried in order; first successful match wins.
3. A maximum candidate count is enforced (e.g., 16) to bound trial
   decryption cost.
4. If no candidate matches, the frame is discarded with a warning log.
5. If the recovery response arrives after the frame buffer expires, it is
   discarded with a warning log.
6. Successfully recovered nodes emit `ACTUAL_STATE` with escrow blobs.

---

### GW-2011  Fingerprint display on modem

**Priority:** Must
**Source:** Issue #887

**Description:**
The gateway MUST display its recovery public key fingerprint on the modem's
OLED display as a navigable display page (consistent with GW-1101b
short-press page navigation). The fingerprint is computed as:

1. `hash = SHA-256(public_key_bytes)`
2. Take the first 66 bits of `hash` (bits 0–65).
3. Split into six 11-bit unsigned integers.
4. Map each to a word from the BIP-39 English wordlist (2048 entries).

The display shows the 6 words clearly on the 128×64 OLED (e.g., three rows
of two words each). The fingerprint page is always available when a recovery
keypair exists.

**Acceptance criteria:**

1. The fingerprint is deterministically computed from the public key.
2. The same public key always produces the same 6 words.
3. Both the gateway (modem display) and admin tool / SPA compute identical
   fingerprints for the same public key.
4. The fingerprint page is accessible via short-press navigation.
5. The fingerprint is clearly readable on the 128×64 display.
6. The security target (66-bit anti-MITM work factor) is documented.

---

### GW-2012  Replacement gateway bootstrap sequence

**Priority:** Must
**Source:** Issue #887

**Description:**
A replacement gateway (empty database, no local PSKs) MUST follow a defined
bootstrap sequence before it can recover escrowed PSKs:

1. Start with a fresh random master key (via `KeyProvider`).
2. Generate a new recovery keypair (GW-2000).
3. Publish the recovery public key via connector (GW-2001).
4. Display the fingerprint on the modem (GW-2011).
5. Fetch the KDF salt from Azure (first-writer-wins, GW-2008). If Azure has
   a salt, adopt it locally.
6. Wait for `MASTER_KEY_INSTALL` from the admin (GW-2006).
7. On successful master key installation, transition escrow state from
   `disabled` to `bootstrapping` → `ready`.
8. Begin answering `KEY_ESCROW_REQUEST` responses for unknown nodes
   (GW-2009, GW-2010).

The gateway MUST NOT attempt node recovery (GW-2009) while escrow state is
`disabled` — it MUST silently discard unknown frames per GW-1002 until the
admin completes key installation.

**Acceptance criteria:**

1. A replacement gateway with empty DB starts in `disabled` escrow state.
2. Fingerprint is displayed before admin performs key rotation.
3. After `MASTER_KEY_INSTALL`, the gateway can recover escrowed nodes.
4. Before `MASTER_KEY_INSTALL`, unknown frames are silently discarded.
5. The full replacement flow (bootstrap → key install → first node recovery)
   succeeds end-to-end.

---

### GW-2013  Connector key-management message security

**Priority:** Must
**Source:** Issue #887, security.md

**Description:**
Connector key-management messages (`MASTER_KEY_INSTALL`, `KEY_ESCROW_REQUEST`,
`KEY_ESCROW_RESPONSE`) cross a trust boundary between the gateway and the
control plane. The gateway MUST enforce the following security properties:

1. **`MASTER_KEY_INSTALL` authentication**: The message is cryptographically
   bound to the gateway's recovery public key via X25519 key agreement. Only
   a sender who knows the master key AND the gateway's public key can produce
   a valid message. The `target_key_epoch` prevents replaying messages to a
   different keypair generation. The `operation_id` prevents replay. The
   `expiry_ms` prevents delayed delivery.

2. **`KEY_ESCROW_RESPONSE` integrity**: Escrow blobs are individually
   authenticated via AES-256-GCM with AAD (GW-2002). The gateway
   trial-decrypts each candidate — invalid or tampered blobs fail
   authentication and are discarded. The response itself does not require
   additional authentication because each blob is self-authenticating.

3. **`KEY_ESCROW_REQUEST` is gateway-originated**: Only the gateway emits
   recovery requests. The control plane cannot trigger requests.

4. **Rate limiting**: Per-key_hint rate limiting (GW-2009) bounds
   amplification from radio-triggered requests.

**Acceptance criteria:**

1. A `MASTER_KEY_INSTALL` with a valid structure but encrypted with a
   different gateway's public key fails decryption.
2. A replayed `MASTER_KEY_INSTALL` (same `operation_id`) is rejected.
3. A tampered escrow blob in `KEY_ESCROW_RESPONSE` is discarded without
   affecting other candidates.
4. An unsolicited `KEY_ESCROW_RESPONSE` (no matching pending request) is
   discarded.

---

## New Requirements — Azure Handler (AZH-0600 series)

### AZH-0600  Encrypted PSK escrow blob storage

**Priority:** Must
**Source:** Issue #887, GW-2003

**Description:**
The Azure handler MUST store `encrypted_psk_escrow` blobs received in
`ACTUAL_STATE` messages in per-subject Azure Table rows. The table schema
extends the existing node state table:

- Node PSKs: stored in the existing node state row (new column).
- Phone PSKs: stored in phone-scoped rows (new `entity_kind = "phone"`).

The handler MUST NOT decrypt or inspect the escrow blob contents. The blob
is opaque ciphertext from the handler's perspective.

**Acceptance criteria:**

1. Escrow blobs from `ACTUAL_STATE` are persisted in Azure Table Storage.
2. Blobs are stored verbatim (no transformation or decryption).
3. Node and phone escrow blobs are stored in separate logical rows.
4. Blob updates (re-encryption after key rotation) overwrite the previous
   blob for the same subject.

---

### AZH-0601  Encrypted PSK recovery serving

**Priority:** Must
**Source:** Issue #887, GW-2009, GW-2010

**Description:**
On receiving a `KEY_ESCROW_REQUEST` from the gateway (via upstream queue),
the Azure handler MUST:

1. Look up all escrow blobs matching the requested `key_hint`.
2. Return them as a `KEY_ESCROW_RESPONSE` via the downstream desired-state
   queue.
3. Cap the candidate set at 16 blobs (fail-closed if more exist — log
   warning and return the first 16).

**Acceptance criteria:**

1. A `KEY_ESCROW_REQUEST` for a known `key_hint` returns matching blobs.
2. A `KEY_ESCROW_REQUEST` for an unknown `key_hint` returns an empty
   candidate list.
3. The candidate set is bounded at 16.
4. Responses are correlated via `request_id`.

---

### AZH-0602  Gateway recovery public key storage

**Priority:** Must
**Source:** Issue #887, GW-2001

**Description:**
The Azure handler MUST store the gateway's recovery public key received via
`KEY_ESCROW_PUBKEY` messages. The stored record includes public key bytes,
key epoch, and creation timestamp. This record is readable by the admin
SPA / CLI for fingerprint verification and master-key encryption.

**Acceptance criteria:**

1. Public key records are stored in Azure Table Storage.
2. Only the latest (highest epoch) public key is authoritative.
3. The record is queryable by the admin SPA / CLI.

---

### AZH-0603  KDF salt storage

**Priority:** Must
**Source:** Issue #887, GW-2008

**Description:**
The Azure handler MUST store the KDF salt record received from the gateway
via the connector. The record includes salt bytes, Argon2id parameters, KDF
version, and creation timestamp. The strategy is first-writer-wins:

- If no salt record exists in Azure, the handler stores the first one received.
- If a salt record already exists, the handler ignores subsequent writes
  (does not overwrite) and returns the existing salt in the next
  gateway-scoped `DESIRED_STATE` (or via a dedicated response).

**Acceptance criteria:**

1. The first salt record received is stored persistently.
2. Subsequent salt writes from the gateway do not overwrite existing salt.
3. The existing salt is returned to the gateway on request.

---

### AZH-0604  Master key install relay

**Priority:** Must
**Source:** Issue #887, GW-2006

**Description:**
The Azure handler MUST relay `MASTER_KEY_INSTALL` messages from the admin
SPA / CLI to the gateway via the downstream connector queue. The handler
MUST NOT decrypt or inspect the encrypted master key payload.

**Acceptance criteria:**

1. `MASTER_KEY_INSTALL` messages from the admin SPA are relayed verbatim.
2. The handler does not decrypt or modify the encrypted payload.
3. The message is delivered via the existing downstream queue mechanism.

---

### AZH-0605  Escrow state observability

**Priority:** Should
**Source:** Issue #887, GW-2004

**Description:**
The Azure handler SHOULD expose the gateway's escrow lifecycle state
(from gateway-scoped `ACTUAL_STATE`) to the admin SPA / CLI. This enables
operators to verify that escrow is `ready` before relying on it for
recovery.

**Acceptance criteria:**

1. The gateway's escrow state is stored in Azure and queryable.
2. The admin SPA / CLI can display the current escrow state.
3. A warning is surfaced when escrow is not `ready`.

---

## New Requirements — Admin CLI (ADMIN-0900 series)

### ADMIN-0900  Key rotation command

**Priority:** Must
**Source:** Issue #887

**Description:**
`sonde-admin key rotate` MUST prompt the admin for a passphrase, derive a
master key using Argon2id with the stored salt and parameters, encrypt the
master key with the gateway's verified recovery public key, and send a
`MASTER_KEY_INSTALL` message via the Azure handler (or directly via the
admin gRPC API if local).

For the first rotation (escrow bootstrap), the admin provides only the new
passphrase. For subsequent rotations, the admin provides only the new
passphrase (the gateway handles the old-to-new transition internally since
it already has the old key).

The command MUST:

1. Fetch the salt record from Azure (or generate if none exists).
2. Fetch the gateway's recovery public key from Azure.
3. Display the 6-word fingerprint and prompt the admin to verify it matches
   the modem display.
4. On confirmation, derive the master key and encrypt it.
5. Send `MASTER_KEY_INSTALL` to the gateway.
6. Wait for and display the rotation result.

**Acceptance criteria:**

1. The command prompts for a passphrase (masked input).
2. The passphrase is validated for minimum entropy (6 diceware words / 20+
   random characters).
3. The 6-word fingerprint is displayed for admin verification.
4. The command fails if the admin does not confirm the fingerprint.
5. The rotation result is displayed (success / failure with reason).
6. The passphrase and derived master key are zeroed from memory after use.

---

### ADMIN-0901  Fingerprint verification

**Priority:** Must
**Source:** Issue #887, GW-2011

**Description:**
`sonde-admin key fingerprint` MUST fetch the gateway's recovery public key
from Azure and display its 6-word BIP-39-wordlist fingerprint. This allows
the admin to verify the fingerprint independently of the rotation flow.

**Acceptance criteria:**

1. The command fetches the public key from Azure.
2. The 6-word fingerprint is computed identically to GW-2011.
3. The fingerprint is displayed in a clear, human-readable format.

---

### ADMIN-0902  Escrow status command

**Priority:** Should
**Source:** Issue #887, GW-2004

**Description:**
`sonde-admin key status` SHOULD display the current escrow lifecycle state,
key version, number of escrowed PSKs (node + phone), and KDF parameters.

**Acceptance criteria:**

1. Escrow state is displayed (disabled / bootstrapping / ready / rotation_in_progress / degraded).
2. Current key version is displayed.
3. Count of escrowed node and phone PSKs is displayed.

---

## Connector API Changes

### New message types

The following new connector message types are added:

| `msg_type` | Name | Direction | Description |
|------------|------|-----------|-------------|
| `0x10` | `KEY_ESCROW_PUBKEY` | Gateway → control plane | Gateway's recovery public key |
| `0x11` | `KEY_ESCROW_REQUEST` | Gateway → control plane | Request escrowed PSK(s) for a key_hint |
| `0x12` | `KEY_ESCROW_RESPONSE` | Control plane → gateway | Escrowed PSK candidate(s) |
| `0x13` | `MASTER_KEY_INSTALL` | Control plane → gateway | Encrypted new master key for rotation |

These are separate from `DESIRED_STATE`/`ACTUAL_STATE` because they have
imperative/transactional semantics (operation IDs, expiry, idempotency)
rather than replacement semantics.

The existing `ACTUAL_STATE` is extended with one new field
(`encrypted_psk_escrow`, CBOR key 12) using the existing declarative
replacement semantics.

---

## Modified Requirements

### GW-0601a (addendum)

The master key MAY be derived from a passphrase via Argon2id. This
derivation happens in the admin tool / SPA — the gateway only receives the
raw 32-byte master key. The `KeyProvider` trait and existing backends are
unchanged.

### GW-1002 (addendum)

When escrow is in `ready` state, unknown `key_hint` values MAY trigger a
recovery request (GW-2009) instead of immediate silent discard. The frame
is buffered for up to 30 seconds pending recovery. If recovery fails or
times out, the frame is silently discarded per the original requirement.

### security.md §2.3 (addendum)

New subsection: **2.3.2 PSK key escrow**

Describes the escrow key hierarchy, asymmetric key transport, recovery
flow, and escrow-specific threats:

- Azure compromise: cannot decrypt PSKs (encrypted with master key that
  Azure never holds).
- Public key substitution: mitigated by modem-displayed 6-word fingerprint
  verification (66-bit anti-MITM work factor).
- Offline passphrase brute-force: mitigated by Argon2id (64 MiB memory-hard)
  + minimum 77-bit entropy requirement.
- Key_hint amplification: mitigated by per-key_hint rate limiting and
  bounded candidate sets.

---

## Invariant Impact Assessment

| Invariant | Impact |
|-----------|--------|
| PSKs never leave gateway in plaintext | **Preserved** — escrow blobs are always AES-256-GCM encrypted with the master key |
| Azure never holds plaintext secrets | **Preserved** — master key derived from passphrase, never stored in Azure; PSKs encrypted |
| Silent-discard error model | **Modified** — unknown key_hints may now trigger recovery requests when escrow is enabled, but failed recovery still results in silent discard |
| Connector API backward compatibility | **Preserved** — new message types use previously unused `msg_type` values; new ACTUAL_STATE field is optional |
| KeyProvider trait contract | **Preserved** — passphrase derivation is admin-side; gateway still uses raw master keys |
| State export/import (GW-1001) | **Unaffected** — escrow is complementary to, not a replacement for, state export |
| Radio protocol | **Unaffected** — no changes to on-air frame format |

---

## Traceability

All new requirements trace to `USER-REQUEST: "Azure should be the long-term
store for PSKs for the gateway, so replacing the gateway doesn't break
pairing, but without disclosing the keys."`

The requirements were informed by adversarial analysis that identified:
- Key_hint collision risks → bounded candidate sets + rate limiting (GW-2009, GW-2010)
- Escrow blob swap attacks → authenticated context binding via AAD (GW-2002)
- DESIRED_STATE semantic conflict → new connector message types
- Crash-safety risks → explicit rotation state machine (GW-2004, GW-2007)
- Escrow readiness ambiguity → lifecycle state machine (GW-2004)
