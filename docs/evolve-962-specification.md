<!-- SPDX-License-Identifier: MIT
     Copyright (c) 2026 sonde contributors -->
# Specification Patch: PSK Key Escrow Redesign

> **Issue:** #962
> **Status:** Draft — Phase 2 specification changes
> **Supersedes:** Escrow sections of evolve-887-specification.md (§§20.1–20.12,
> §§2.2–2.4, §§3.1, §§4.1, §§5.1, §§6.1, §§7 T-2000–T-2009, §§8).
> Also supersedes: `gateway-companion-api.md` §3.2 and §3.3 ACTUAL_STATE /
> DESIRED_STATE `entity_id` rules (empty string for gateway);
> `gateway-requirements.md` GW-0811 (`entity_id` ignored for gateway-scoped
> state).
> **Scope:** Simplify escrow architecture — eliminate imperative connector
> messages, unify gateway identity, add rotation-code authentication, treat
> gateway as first-class ACTUAL_STATE/DESIRED_STATE entity.
> **Traceability:** Redesigns GW-2000–GW-2013, AZH-0600–AZH-0605,
> ADMIN-0900–ADMIN-0902.

---

## 1  Motivation

The original evolve-887 escrow spec introduced four imperative connector
messages (KEY_ESCROW_PUBKEY, KEY_ESCROW_REQUEST, KEY_ESCROW_RESPONSE,
MASTER_KEY_INSTALL), a 5-state lifecycle machine, a separate EscrowBlob
CBOR format, integer key-version counters, a recovery queue with TTLs and
rate limiting, and a separate `gatewayescrow` Azure Table.

This redesign simplifies the architecture based on two principles:

1. **Declarative convergence** — the connector is high-latency and periodic.
   All state changes use the existing ACTUAL_STATE/DESIRED_STATE model.
   No imperative commands.
2. **Gateway as first-class entity** — the gateway reports and receives
   state the same way nodes do.

---

## 2  Design Changes — Gateway

### 2.1  Unified gateway identity (replaces §20.2)

The separate escrow X25519 keypair (`escrow_keypair` table, `load_or_generate_keypair()`)
is removed. The existing `GatewayIdentity` serves both purposes:

- **Ed25519**: BLE pairing challenge-response signing (unchanged).
- **X25519**: Key exchange for master key rotation (via existing `to_x25519()`
  conversion). This supersedes the retired GW-1202; the conversion is
  re-scoped under this specification as an escrow requirement.

The `GatewayIdentity` seed is already encrypted at rest with the master key
(AES-256-GCM, AAD = `b"sonde-gateway-identity" || gateway_id` — 22-byte
ASCII prefix concatenated with the raw 16-byte `gateway_id`). No additional
storage is needed.

**Removed artifacts:**
- `escrow_keypair` table
- `EscrowKeypair` type / `load_or_generate_keypair()` function
- `escrow_key_epoch` field (identity is stable; epoch is unnecessary when
  the public key itself is the identifier)

### 2.2  Master key identification (replaces §20.5, §20.6)

Each PSK record carries an opaque master key identifier and a monotonic epoch:

```sql
-- Migration: add master_key_id and master_key_epoch to nodes
ALTER TABLE nodes ADD COLUMN master_key_id BLOB;
ALTER TABLE nodes ADD COLUMN master_key_epoch INTEGER NOT NULL DEFAULT 0;

-- Migration: add master_key_id and master_key_epoch to phone_psks
ALTER TABLE phone_psks ADD COLUMN master_key_id BLOB;
ALTER TABLE phone_psks ADD COLUMN master_key_epoch INTEGER NOT NULL DEFAULT 0;
```

| Field | Type | Description |
|-------|------|-------------|
| `master_key_id` | BLOB (16 bytes) | Random opaque identifier, generated at key creation. NOT a hash of the key — avoids creating an offline passphrase verifier. |
| `master_key_epoch` | INTEGER | Monotonic counter, incremented on each rotation. Prevents rollback. |

On first startup (before any rotation), the gateway generates a random
`master_key_id` and sets `master_key_epoch = 1`. These are stored in
`gateway_config` as `master_key_id` (hex) and `master_key_epoch` (integer string).

Migration backfills existing records with the current key's id and epoch.

**Removed artifacts:**
- `EscrowState` enum (disabled/bootstrapping/ready/rotation_in_progress/degraded)
- `escrow_state` field on `Gateway` struct
- `load_escrow_state()` / `store_escrow_state()` functions
- `key_version` column concept (replaced by `master_key_id` + `master_key_epoch`)

### 2.3  Rotation code authentication (new)

The gateway generates a random single-use **rotation code** displayed on
the modem. Any master key rotation request must include this code inside
the X25519-encrypted envelope. This provides physical-presence authentication:
only someone who can see the modem display can authorize a rotation.

```sql
-- Stored in gateway_config
INSERT OR REPLACE INTO gateway_config (key, value) VALUES ('rotation_code', ?);
```

**Rotation code lifecycle:**
1. Gateway generates a random 6-character uppercase alphanumeric code
   (`[A-Z0-9]`, 36^6 ≈ 2.18 × 10^9 ≈ 31 bits of entropy) on first startup.
   The code is generated using CSPRNG (`getrandom::fill()`) with rejection
   sampling over the 36-symbol alphabet to avoid modulo bias.
2. Code is displayed on the modem alongside the BIP-39 fingerprint.
3. Admin reads the code from the modem display.
4. Admin includes the code in the rotation request (inside the encrypted payload).
   The admin CLI and SPA MUST normalize user input to uppercase before
   including it in the rotation payload.
5. Gateway verifies the code matches (case-sensitive after normalization).
6. On successful rotation, gateway generates a new code.
7. Code is never published to the cloud or connector.

### 2.4  Gateway ACTUAL_STATE (replaces §20.4, §20.5 status_details)

The gateway becomes a first-class entity in the ACTUAL_STATE model. Entity
kind is `"gateway"`, entity ID is `hex(gateway_id)` — lowercase hex encoding
of the raw 16-byte `gateway_id`, no `0x` prefix (e.g.,
`"a1b2c3d4e5f6a7b8a1b2c3d4e5f6a7b8"`).

**Note:** This supersedes `gateway-companion-api.md` §3.2 and §3.3
ACTUAL_STATE / DESIRED_STATE `entity_id` rules (empty string for gateway).
It also supersedes `gateway-requirements.md` GW-0811, which states that
gateway-scoped state ignores `entity_id`. With this spec, gateway
`entity_id` carries the hex-encoded `gateway_id` to distinguish multiple
gateways sharing one cloud deployment. Companion patches to both documents
must update these sections accordingly.

Gateway ACTUAL_STATE is emitted:
- On startup (after identity and master key are loaded).
- On connector reconnection (full state replay).
- Whenever gateway state changes (channel change, rotation complete, etc.).

| Field | CBOR key | Type | Description |
|-------|----------|------|-------------|
| `msg_type` | 1 | uint | `0x02` (ACTUAL_STATE) |
| `entity_kind` | 2 | tstr | `"gateway"` |
| `entity_id` | 3 | tstr | `hex(gateway_id)` |
| `timestamp_ms` | 9 | uint | Current time (Unix ms) |

**Common ACTUAL_STATE keys 4–8, 10–11:** For `entity_kind = "gateway"`,
keys 4 (`current_program_hash`), 5 (`assigned_program_hash`),
6 (`battery_mv`), 7 (`firmware_abi_version`), 8 (`firmware_version`),
10 (`status_details`), and 11 (`schedule_interval_s`) are node-specific
and MUST be omitted (not encoded as null). Encoders MUST NOT include
them; decoders MUST tolerate their absence. Key 9 (`timestamp_ms`) is
shared and required for all entity kinds.

| `channel` | 15 | uint | ESP-NOW channel |
| `master_key_id` | 16 | bstr (16 bytes) | Opaque master key identifier |
| `master_key_epoch` | 17 | uint | Monotonic master key epoch |
| `x25519_public_key` | 18 | bstr (32 bytes) | X25519 public key (derived from GatewayIdentity) |
| `fingerprint_words` | 19 | array of tstr | 6-word BIP-39 fingerprint |
| `missing_key_hints` | 20 | array of uint (0–65535) | Unknown key_hints (u16, one-shot, cleared after reporting) |
| `salt` | 21 | bstr (16 bytes)/null | KDF salt, if set |
| `kdf_params` | 22 | map/null | `{1: m_cost, 2: t_cost, 3: p_cost, 4: kdf_version}` |
| `gateway_version` | 23 | tstr | Gateway binary semver |
| `gateway_commit` | 24 | tstr | Gateway binary git commit |
| `modem_firmware_version` | 25 | tstr/null | Modem firmware semver (from startup handshake) |
| `modem_firmware_commit` | 26 | tstr/null | Modem firmware git commit |
| `rotation_in_progress` | 27 | bool | `true` if `pending_rotation` record exists |

**CBOR key allocation note:** Keys 15–27 are new gateway-specific fields.
Keys 1–11 retain their existing meanings for node/phone ACTUAL_STATE.
Keys 12–14 are redefined by this spec for escrow (see §2.7): key 12
becomes `encrypted_psk`, key 13 becomes `escrow_key_hint`, key 14
becomes `master_key_id` (replacing the previous `escrow_key_version`).

**Note on `master_key_id` key allocation:** Node/phone ACTUAL_STATE uses
CBOR key 14 for `master_key_id` (identifies which key encrypted that
entity's PSK). Gateway ACTUAL_STATE uses CBOR key 16 for `master_key_id`
(the gateway's current key). These are semantically the same identifier
but appear at different keys because keys 12–14 are the node/phone escrow
tuple (`encrypted_psk`, `escrow_key_hint`, `master_key_id`) while keys
15–27 are the gateway state block. Decoders dispatch on `entity_kind`
to select the correct key.

### 2.5  Gateway DESIRED_STATE (new)

The cloud drives gateway configuration changes via DESIRED_STATE with
`entity_kind = "gateway"`.

**Top-level envelope** (same as node DESIRED_STATE):

| Field | CBOR key | Type | Description |
|-------|----------|------|-------------|
| `msg_type` | 1 | uint | `0x01` (DESIRED_STATE) |
| `entity_kind` | 2 | tstr | `"gateway"` |
| `entity_id` | 3 | tstr | `hex(gateway_id)` |
| `desired_state` | 4 | map | Gateway desired state map (see below) |

**Inside `desired_state` map (key 4):**

| Field | CBOR key | Type | Description |
|-------|----------|------|-------------|
| `channel` | 15 | uint/null | Desired ESP-NOW channel |
| `salt` | 21 | bstr (16 bytes)/null | KDF salt from cloud |
| `kdf_params` | 22 | map/null | KDF parameters from cloud |
| `rotation_payload` | 28 | bstr/null | X25519-encrypted rotation payload (see §2.6) |
| `recovered_psks` | 29 | array/null | Recovered PSK records for missing nodes (see §2.8) |

**CBOR key alignment note:** Keys shared between ACTUAL_STATE and
DESIRED_STATE use the same key numbers (15 for `channel`, 21 for `salt`,
22 for `kdf_params`). DESIRED_STATE-only fields use keys 28–29 to avoid
collision with ACTUAL_STATE keys 23–27 (gateway/modem version fields and
`rotation_in_progress`).

**Convergence behavior:**
- **Channel:** If `channel` differs from current, gateway switches and
  reports updated ACTUAL_STATE.
- **Rotation payload:** If present and valid, gateway performs key rotation
  (see §2.6).
- **Recovered PSKs:** If present, gateway processes each record (see §2.8).
- **Salt/KDF params:** Gateway adopts from DESIRED_STATE if it has no
  local salt. Once the gateway has committed a local salt, it is
  immutable except via rotation payload delivery (§2.6). If both exist
  and differ, the gateway keeps its local salt.

### 2.6  Master key rotation via DESIRED_STATE (replaces §20.7)

#### 2.6.1  Rotation payload format

The `rotation_payload` field in DESIRED_STATE is a `bstr` with the
following binary layout:

```
┌──────────────────────────────────────────────────┐
│  version (1 byte) = 0x01                         │
│  sender_ephemeral_public (32 bytes)              │
│  nonce (12 bytes)                                │
│  ciphertext_and_tag (variable, ≥ 16 bytes)       │
└──────────────────────────────────────────────────┘
```

Total envelope: 45 bytes + ciphertext length.

**Plaintext layout** (CBOR map, encrypted inside `ciphertext_and_tag`):

| Field | CBOR key | Type | Description |
|-------|----------|------|-------------|
| `new_master_key` | 1 | bstr (32 bytes) | The new master key |
| `rotation_code` | 2 | tstr | 6-character rotation code from modem display |
| `new_master_key_id` | 3 | bstr (16 bytes) | Random opaque ID for the new key |
| `salt` | 4 | bstr (16 bytes)/null | KDF salt (included on first rotation) |
| `kdf_params` | 5 | map/null | `{1: m_cost, 2: t_cost, 3: p_cost, 4: kdf_version}` |

**Encryption parameters:**
- Shared secret: `X25519(gw_private_key, sender_ephemeral_public)`
- Derived key: `HKDF-SHA-256(shared_secret, hkdf_salt=b"sonde-rotation-v1",
  info=gateway_id_raw || current_master_key_epoch_be64)` where
  `hkdf_salt` is the 17-byte ASCII encoding of `"sonde-rotation-v1"`
  (no NUL terminator), `gateway_id_raw` is the raw 16-byte `gateway_id`
  (not hex-encoded), and `current_master_key_epoch_be64` is the 8-byte
  big-endian encoding of the gateway's current `master_key_epoch`.
  Note: the HKDF `hkdf_salt` is a fixed protocol constant, distinct from
  the Argon2id KDF `salt` used for passphrase derivation.
- Decryption: `AES-256-GCM-Open(derived_key, nonce, ciphertext_and_tag,
  aad=gateway_id_raw || current_master_key_epoch_be64)`

**Validation rules (gateway side):**
1. `version` must be `0x01`. Reject unknown versions.
2. `sender_ephemeral_public` must be 32 bytes and not a low-order point.
3. Payload length must be ≥ 45 + 16 bytes (minimum ciphertext with tag).
4. Decryption failure → for DESIRED_STATE ingress, log a warning and
   discard (no response channel). For gRPC `SubmitRotation`, return
   `accepted = false` with an error message.
5. `rotation_code` must match stored code → reject with warning if not.
6. `current_master_key_epoch` in AAD must match gateway's current epoch
   → reject stale/replayed payloads.
7. `new_master_key` must be 32 bytes. `new_master_key_id` must be 16 bytes.
8. **Rate limiting:** The gateway MUST rate-limit failed rotation attempts
   to at most 3 per 5-minute window per epoch. After exceeding the limit,
   further attempts are silently discarded (DESIRED_STATE) or rejected
   with a rate-limit error (gRPC) until the window expires. This prevents
   online brute-force of the 31-bit rotation code by an attacker who
   can submit DESIRED_STATE via compromised Azure credentials.

#### 2.6.2  Rotation execution

When the gateway receives a valid rotation payload, it:

1. **Decrypt:** Use `GatewayIdentity.to_x25519()` to derive X25519 private key.
   Decrypt the payload using X25519 + HKDF-SHA-256 + AES-256-GCM:
   - Shared secret: `X25519(gw_private, sender_ephemeral_public)`
   - Derived key: `HKDF-SHA-256(shared_secret, hkdf_salt=b"sonde-rotation-v1",
     info=gateway_id_raw || current_master_key_epoch_be64)`
   - Decrypt: `AES-256-GCM-Open(key, nonce, ciphertext,
     aad=gateway_id_raw || current_master_key_epoch_be64)`

2. **Verify rotation code:** The decrypted payload contains
   `{new_master_key, rotation_code, new_master_key_id, salt, kdf_params}`.
   Verify `rotation_code` matches the stored code. If not → reject, log
   warning.

3. **Verify epoch:** The `current_master_key_epoch_be64` encoded into the
   rotation payload's HKDF info and AES-GCM AAD must match the gateway's
   current epoch. If decryption succeeded, this is implicitly verified
   (AAD mismatch causes GCM authentication failure). As an explicit
   pre-persist check, the gateway computes `new_epoch = current_epoch + 1`
   and verifies it is strictly greater than the current epoch before
   writing the `pending_rotation` record in step 4.

4. **Prepare:** In a single database transaction:
   a. Write `pending_rotation` record:
   ```sql
   CREATE TABLE IF NOT EXISTS pending_rotation (
       id                 INTEGER PRIMARY KEY CHECK (id = 1),
       new_master_key_enc BLOB    NOT NULL,
       new_master_key_id  BLOB    NOT NULL,
       new_epoch          INTEGER NOT NULL,
       started_at         INTEGER NOT NULL,
       phase              TEXT    NOT NULL DEFAULT 'migrating_psks'
   );
   ```
   b. Purge all records from `pending_recovery` — these are encrypted with
      the old master key and cannot be decrypted after rotation completes.
      For nodes already known to the gateway (in the `nodes` table), their
      PSKs are re-encrypted during migration (step 5) and re-emitted via
      ACTUAL_STATE, updating the Azure blobs with the new `master_key_id`.
      For nodes that were mid-recovery (in `pending_recovery` but not yet
      promoted to `nodes`), their Azure blobs still carry the old
      `master_key_id`. These nodes will wake, trigger `missing_key_hints`,
      but the handler will not return their PSKs (master_key_id mismatch).
      **This is an accepted edge case:** nodes mid-recovery during rotation
      require manual reprovisioning. The window is narrow (recovery TTL
      is 24 hours; rotation is an infrequent operator action).

   The new master key is encrypted with the OLD master key for crash safety,
   using the same `encrypt_psk` pattern: `AES-256-GCM(old_master_key,
   random_nonce_12B, new_master_key_32B, aad=b"sonde-pending-rotation")`.
   The resulting `new_master_key_enc` blob is 60 bytes (12B nonce + 32B
   ciphertext + 16B GCM tag).
   Phase values: `migrating_psks` → `rewrapping_identity` → `committing`.
   `new_epoch` is derived as `current_master_key_epoch + 1`. The gateway
   rejects the rotation if `new_epoch` does not equal the epoch bound into
   the rotation payload AAD plus one (i.e., the payload was created for
   the gateway's current epoch).

5. **Migrate PSKs** (phase `migrating_psks`): For each record in `nodes`
   and `phone_psks` where `master_key_epoch < new_epoch`:
   - Decrypt PSK with old master key.
   - Re-encrypt PSK with new master key.
   - Update `master_key_id` and `master_key_epoch`.
   - Commit each record individually.
   After all records migrated, update phase to `rewrapping_identity`.

   **Dual-key frame processing during migration:** While migration is in
   progress, PSK records have mixed `master_key_epoch` values. During
   frame processing, the gateway looks up candidate PSKs by `key_hint`
   and decrypts each using the master key identified by that record's
   `master_key_epoch` — old key for unmigrated records, new key for
   migrated ones. Both keys are held in memory for the duration of
   the rotation.

6. **Rewrap identity seed** (phase `rewrapping_identity`): Re-encrypt
   `gateway_identity.encrypted_seed` under the new master key. Store the
   new encrypted seed in a **separate column** `encrypted_seed_new` (added
   via migration). The original `encrypted_seed` column (encrypted with
   the old key) is preserved until commit. Update phase to `committing`.

   ```sql
   -- Migration: add column for dual-key identity storage during rotation
   ALTER TABLE gateway_identity ADD COLUMN encrypted_seed_new BLOB;
   ```

7. **Commit** (phase `committing`): In a single DB transaction:
   - Copy `encrypted_seed_new` → `encrypted_seed` (promote the new-key
     version) and set `encrypted_seed_new = NULL`.
   - Store new `master_key_id` and `master_key_epoch` in `gateway_config`.
   - Store salt and KDF params from the rotation payload if non-null.
     A `null` value means "leave unchanged" — the gateway preserves its
     existing salt/KDF params.
   - Generate a new rotation code.
   - Delete `pending_rotation`.

   After the DB transaction commits successfully, activate the new master
   key in memory (`SqliteStorage` swaps its in-memory key reference).
   The in-memory swap is NOT part of the DB transaction — it occurs only
   after commit succeeds. If the process crashes after DB commit but before
   in-memory swap, the next startup will load the new key from the
   committed `gateway_config`.

8. **Emit:** Report updated gateway ACTUAL_STATE with new `master_key_id`,
   `master_key_epoch`, and `rotation_in_progress = false`. Re-emit
   node ACTUAL_STATE with updated `encrypted_psk` and `master_key_id` for
   all nodes.

**Crash recovery:** On startup, if `pending_rotation` exists:
- Decrypt the pending new master key using the current (old) master key.
- Check phase:
  - `migrating_psks`: Resume PSK migration (step 5), then continue.
  - `rewrapping_identity`: Resume identity rewrap (step 6), then continue.
  - `committing`: Complete the final commit (step 7).
- **Key invariant:** During all pre-commit phases, the original
  `encrypted_seed` column remains encrypted with the old (current) master
  key. The `encrypted_seed_new` column holds the new-key version but is
  only promoted to `encrypted_seed` in the atomic commit transaction
  (step 7). Therefore `GatewayIdentity` is always loadable with the
  current master key at startup, regardless of which phase the crash
  occurred in.

### 2.7  Node ACTUAL_STATE escrow fields (replaces §20.3, §20.4)

Node-scoped ACTUAL_STATE gains three fields:

| Field | CBOR key | Type | Description |
|-------|----------|------|-------------|
| `encrypted_psk` | 12 | bstr (60 bytes)/null | Encrypted PSK blob (see format below) |
| `escrow_key_hint` | 13 | uint (0–65535)/null | `key_hint` (u16) for Azure recovery lookup |
| `master_key_id` | 14 | bstr (16 bytes)/null | Opaque master key ID that encrypted this PSK |

**Phone PSKs are NOT escrowed.** Phone PSKs are encrypted at rest with the
master key and are re-encrypted during key rotation (§2.6.2 step 5), but
they are never published to the cloud via ACTUAL_STATE. Phone escrow may
be added in a future specification — the CBOR key assignments and handler
schema are designed to accommodate `entity_kind = "phone"` without
protocol changes.

**`encrypted_psk` format** (60 bytes, identical to the DB storage format):

```
┌──────────────────────────────────────────────────┐
│  nonce (12 bytes)                                │
│  ciphertext (32 bytes) + GCM tag (16 bytes)      │
└──────────────────────────────────────────────────┘
```

- Encryption: `AES-256-GCM(master_key, nonce, plaintext_psk,
  aad=node_id_utf8)` where `node_id_utf8` is the UTF-8 encoding of the
  node's `node_id` string.
- The blob is opaque to the cloud — the handler stores and returns it
  without inspection or decryption.

Escrow fields are emitted:
- On node registration.
- After key rotation (all node records re-emitted with new `master_key_id`).
- On connector reconnection (full state replay).

### 2.8  Declarative node recovery (replaces §20.9)

When a frame arrives with an unknown `key_hint`:

1. **Rate limit:** At most 1 hint per `key_hint` per 60 seconds. Bounded
   dedup set (max 256 entries, LRU eviction).
2. **Report:** Add `key_hint` to `missing_key_hints` in the next gateway
   ACTUAL_STATE emission.
3. **Clear:** After reporting, clear the hint from the pending set. If the
   node is still around, it will wake again and re-trigger.
4. **No frame buffering.** The frame is discarded. The node's wake cycle
   is the natural retry mechanism.

**Note for Azure handler:** The handler SHOULD latch/enqueue recovery
work immediately upon observing `missing_key_hints` in a gateway
ACTUAL_STATE message. Subsequent ACTUAL_STATE messages (e.g., after
gateway reboot) may overwrite the hints before the handler has
materialized `recovered_psks`. The handler must not rely on hints
persisting across multiple ACTUAL_STATE emissions.

When the gateway receives `recovered_psks` in DESIRED_STATE:

**`recovered_psks` CBOR schema** (array of maps with integer keys):

```
recovered_psks = [recovered_psk_record, ...]

recovered_psk_record = {
    1: node_id      (tstr)           -- node identifier
    2: key_hint     (uint, 0..=65535) -- key_hint (u16)
    3: encrypted_psk (bstr, 60 bytes) -- encrypted PSK blob (§2.7 format)
    4: master_key_id (bstr, 16 bytes) -- opaque master key ID
}
```

1. For each recovered PSK record:
   a. Verify `master_key_id` matches the gateway's current master key ID.
      If not → skip (wrong key era, can't use this PSK).
   b. Insert into a **provisional recovery table** (not directly into `nodes`).
      PSKs remain encrypted at rest — decrypt only into zeroized memory
      during trial frame authentication:
      ```sql
      CREATE TABLE IF NOT EXISTS pending_recovery (
          key_hint         INTEGER NOT NULL,
          node_id          TEXT NOT NULL,
          encrypted_psk    BLOB NOT NULL,
          master_key_id    BLOB NOT NULL,
          master_key_epoch INTEGER NOT NULL,
          received_at      INTEGER NOT NULL,
          PRIMARY KEY (key_hint, node_id)
      );
      ```
      The `master_key_epoch` is set to the gateway's current
      `master_key_epoch` at insertion time (the `recovered_psk_record`
      does not carry an epoch — it is implicit from the `master_key_id`
      match verified in step 1a).
2. On next frame with matching `key_hint`:
   a. Look up candidates in `pending_recovery`.
   b. For each candidate, decrypt `encrypted_psk` with the gateway's
      master key into zeroized memory, then trial-decrypt the frame
      with the resulting plaintext PSK.
   c. On first success: promote the record to the `nodes` table using
      the normal `upsert_node` path (which re-encrypts with the current
      master key), delete from `pending_recovery`, process the frame.
   d. On failure: leave in `pending_recovery` (may be retried on next frame).
3. **Expiry:** Records in `pending_recovery` older than 24 hours are
   purged on startup and periodically.

**Removed artifacts:**
- `RecoveryQueue` struct with TTLs, rate limiting, request_id
- `KEY_ESCROW_REQUEST` / `KEY_ESCROW_RESPONSE` imperative messages

### 2.9  Startup sequence (replaces §2.4)

Insert after step 2 (Initialize storage backend):

> 2a. Check if `pending_rotation` exists. If so, resume rotation
>     (§2.6.2 crash recovery). For phase `migrating_psks`, recovery
>     proceeds without needing `GatewayIdentity` (only PSK records are
>     touched). For phases `rewrapping_identity` and `committing`,
>     recovery first loads `GatewayIdentity` using the current (old)
>     master key (which is safe — see §2.6.2 key invariant), then
>     completes the remaining steps.
> 2b. Load `GatewayIdentity`. Derive X25519 public key via `to_x25519()`.
>     (If already loaded during step 2a recovery, reuse the cached value.)
> 2c. Load `master_key_id` and `master_key_epoch` from `gateway_config`.
>     If absent (first start), generate random 16-byte `master_key_id`,
>     set `master_key_epoch = 1`, backfill all existing PSK records,
>     and persist.
> 2d. Load `rotation_code` from `gateway_config`. If absent, generate one.

Insert after step 9 (Start connector API server):

> 9a. Emit gateway ACTUAL_STATE (§2.4) with all current state.
> 9b. Compute BIP-39 fingerprint from X25519 public key.
> 9c. Register fingerprint + rotation code as modem display pages.

### 2.10  Fingerprint computation (unchanged from §20.10)

Uses SHA-256 of the X25519 public key (derived from `GatewayIdentity`),
extracts 6 BIP-39 words from 66 bits. Shared BIP-39 wordlist in
`sonde-protocol`.

### 2.11  Modem display pages (replaces §5.1)

Two display pages registered on the modem:

**Page 1 — Key Fingerprint** (128×64 OLED):
```
Row 1 (y=8):   word1  word2
Row 2 (y=28):  word3  word4
Row 3 (y=48):  word5  word6
```

**Page 2 — Rotation Code** (128×64 OLED):
```
Row 1 (y=16):  ROTATION CODE
Row 2 (y=40):  <6-char code>
```

Both pages are rendered by the gateway and sent to the modem via the
existing reliable display-transfer subprotocol (GW-1101b).

---

## 3  Design Changes — Connector API

### 3.1  Removed message types

The following imperative message types from evolve-887 are removed:

| `msg_type` | Name | Replacement |
|------------|------|-------------|
| `0x10` | `KEY_ESCROW_PUBKEY` | Gateway ACTUAL_STATE field `x25519_public_key` (CBOR key 18) |
| `0x11` | `KEY_ESCROW_REQUEST` | Gateway ACTUAL_STATE field `missing_key_hints` (CBOR key 20) |
| `0x12` | `KEY_ESCROW_RESPONSE` | Gateway DESIRED_STATE field `recovered_psks` (CBOR key 29) |
| `0x13` | `MASTER_KEY_INSTALL` | Gateway DESIRED_STATE field `rotation_payload` (CBOR key 28) |

The `ConnectorOutboundMessage::KeyEscrowPubkey` and
`ConnectorOutboundMessage::KeyEscrowRequest` variants are removed.
`ConnectorEventHub::emit_key_escrow_pubkey()` and
`emit_key_escrow_request()` methods are removed.

### 3.2  ACTUAL_STATE extension

Node ACTUAL_STATE gains CBOR keys 12, 13, 14 (as defined in §2.7).
Gateway ACTUAL_STATE gains CBOR keys 15–27 (as defined in §2.4).

### 3.3  DESIRED_STATE extension

Gateway DESIRED_STATE gains CBOR keys 15, 21, 22, 28, 29 (as defined in §2.5).

---

## 4  Design Changes — Azure Handler

### 4.1  Table schema (replaces §8.1)

No separate `gatewayescrow` table. All gateway state is stored in the
existing `ActualState` table with `entity_kind = "gateway"`.

**`ActualState` table extension:**

| Column | Type | Applicable entity_kind | Description |
|--------|------|----------------------|-------------|
| `encrypted_psk` | binary/null | node | Raw encrypted PSK blob |
| `master_key_id` | binary/null | node, gateway | Opaque master key ID |
| `key_hint` | int64/null | node | key_hint for recovery queries |
| `x25519_public_key` | binary/null | gateway | X25519 public key |
| `channel` | int64/null | gateway | ESP-NOW channel |
| `master_key_epoch` | int64/null | gateway | Monotonic master key epoch |
| `salt` | binary/null | gateway | KDF salt |
| `kdf_params_json` | string/null | gateway | JSON-encoded KDF params |
| `gateway_version` | string/null | gateway | Gateway binary semver |
| `gateway_commit` | string/null | gateway | Gateway binary git commit |
| `modem_firmware_version` | string/null | gateway | Modem firmware semver |
| `modem_firmware_commit` | string/null | gateway | Modem firmware git commit |
| `missing_key_hints` | string/null | gateway | JSON array of missing key_hints |
| `fingerprint_words` | string/null | gateway | JSON array of 6 BIP-39 words |
| `rotation_in_progress` | bool/null | gateway | `true` if rotation is in progress |

### 4.2  Message handling (replaces §8.2)

**ACTUAL_STATE with `entity_kind = "gateway"`:**
- Upsert gateway row in `ActualState`. Use `PartitionKey = "g:" +
  entity_id` (hex-encoded gateway_id) and `RowKey = "state"` (singleton
  upsert — gateway state is current, not historical).
- If `missing_key_hints` is non-empty, look up matching escrowed PSKs
  from node rows where `key_hint` matches and `master_key_id` matches
  the gateway's reported `master_key_id`.
- Include matching PSKs in the next gateway DESIRED_STATE as `recovered_psks`.

**ACTUAL_STATE with `entity_kind = "node"`:**
- Store `encrypted_psk`, `master_key_id`, and `key_hint` alongside other
  state in the `ActualState` row.

### 4.3  Salt management (replaces §8.3)

Salt is no longer managed separately. It arrives as part of gateway
ACTUAL_STATE and is stored in the gateway's row. The handler includes
it in gateway DESIRED_STATE for new gateways that have no local salt.

Salt conflict resolution: the handler stores whatever the gateway reports
(gateway is authoritative for its own salt). The handler includes its
stored salt in DESIRED_STATE only for gateways that report `salt = null`.

### 4.4  Rotation payload relay

The handler does not originate or modify rotation payloads. The SPA
constructs the payload (X25519-encrypted new master key + rotation code)
and the handler places it in the gateway DESIRED_STATE's
`rotation_payload` field.

After the gateway processes the rotation and reports a new
`master_key_epoch` in ACTUAL_STATE, the handler clears the
`rotation_payload` from DESIRED_STATE.

---

## 5  Design Changes — Admin CLI

### 5.1  `sonde-admin key rotate` (replaces §4.1)

```
sonde-admin key rotate [--gateway-url <url>]
```

Flow:
1. Fetch gateway ACTUAL_STATE via gRPC (local connection to gateway).
2. Extract `x25519_public_key`, `fingerprint_words`, `master_key_epoch`.
3. Display BIP-39 fingerprint. Prompt: "Verify this matches the modem
   display. Continue? [y/N]"
4. Prompt for rotation code (from modem display).
5. Prompt for passphrase (masked, minimum 20 characters or 6 words).
6. Fetch salt from gateway ACTUAL_STATE (or prompt to generate new salt
   for first rotation).
7. Derive master key using KDF params from gateway ACTUAL_STATE (or
   defaults `m=65536, t=3, p=1` for first rotation when no params exist):
   `Argon2id(passphrase, salt, kdf_params)`.
8. Generate random 16-byte `new_master_key_id`.
9. Build `RotationPayloadV1` (§2.6.1): generate ephemeral X25519 keypair,
   derive encryption key, encrypt `{new_master_key, rotation_code,
   new_master_key_id, salt, kdf_params}`.
10. Submit rotation payload to gateway via gRPC `SubmitRotation` method.
11. Poll gateway ACTUAL_STATE via gRPC until `master_key_epoch` increments
    or timeout.

### 5.1.1  gRPC rotation API

The gateway admin gRPC service exposes a `SubmitRotation` method:

```protobuf
// Accepts the same RotationPayloadV1 binary format used in DESIRED_STATE.
rpc SubmitRotation(SubmitRotationRequest) returns (SubmitRotationResponse);

message SubmitRotationRequest {
  bytes rotation_payload = 1;  // RotationPayloadV1 (§2.6.1)
}

message SubmitRotationResponse {
  bool accepted = 1;
  string error = 2;            // Human-readable error if rejected
}
```

The gateway processes the payload through the same rotation handler
used for DESIRED_STATE payloads. Both paths converge at §2.6.2.

Note: `sonde-admin` communicates directly with the gateway via gRPC. It
does not use Azure or the connector. This is a local-only operation.

Passphrase and all derived keys are `Zeroizing`-wrapped.

### 5.2  `sonde-admin key fingerprint` (replaces §4.2)

```
sonde-admin key fingerprint [--gateway-url <url>]
```

Fetches gateway ACTUAL_STATE via gRPC and displays the 6-word BIP-39
fingerprint. No key operations.

### 5.3  `sonde-admin key status` (replaces §4.3)

```
sonde-admin key status [--gateway-url <url>]
```

Displays: master_key_epoch, master_key_id (hex), rotation_in_progress,
salt status, KDF params.

---

## 6  Design Changes — Security Model

### 6.1  Key hierarchy (replaces §6.1)

```
passphrase + salt ──► Argon2id ──► master_key (32 bytes)
                                      │
                              ┌───────┴────────┐
                              ▼                ▼
                    Encrypt(PSK_node1)  Encrypt(PSK_node2) ...
                              │                │
                              ▼                ▼
                       Azure ACTUAL_STATE   (encrypted blobs)
```

**Master key delivery (via SPA or CLI):**

```
Admin ──► Argon2id(passphrase, salt) ──► new_master_key
            +                                   │
         rotation_code (from modem)      X25519(ephemeral, gw_pubkey)
                                                │
                                        HKDF + AES-256-GCM
                                                │
                                   ┌────────────┘
                                   ▼
                    {new_key, rotation_code, new_key_id, salt, kdf}
                                   │
                        ──► DESIRED_STATE ──► Gateway
                                                │
                                     X25519(gw_privkey, ephemeral)
                                                │
                                        HKDF + AES-256-GCM
                                                │
                                    verify rotation_code
                                    verify master_key_epoch
                                                │
                                                ▼
                                          install new key
```

### 6.2  Threat analysis (replaces §6.1 threat table)

| Threat | Mitigation | Residual risk |
|--------|------------|---------------|
| Azure compromise → PSK disclosure | PSKs encrypted with master key; key never in Azure | None (AES-256-GCM) |
| Azure compromise → forced key rotation | Rotation requires rotation_code from modem display; attacker cannot read physical display | Requires physical access to modem |
| Azure MITM on public key | 6-word BIP-39 fingerprint verified by admin on modem display (66-bit work factor) | Targeted collision requires ~2^66 operations |
| Offline passphrase brute-force | Argon2id (64 MiB memory-hard); master_key_id is opaque random (NOT a hash of key) — no offline verifier in cloud | Computationally infeasible without direct gateway access |
| key_hint amplification (radio→cloud) | Rate limiting: 1 hint per key_hint per 60 seconds; max 256 dedup entries (LRU) | Bounded amplification factor |
| Fake recovered PSKs from cloud | Provisional `pending_recovery` table; promoted only after successful frame authentication | Bogus records expire after 24h |
| Rotation crash → split-brain keys | Two-key window + pending_rotation record + per-record master_key_epoch | Auto-recoverable on restart |
| DESIRED_STATE replay (old rotation) | master_key_epoch bound into HKDF info and AES-256-GCM AAD; gateway rejects if epoch ≠ current | Stale rotations rejected |
| Gateway physical compromise | Exposes master key in memory → all PSKs | Accepted; HSM/enclave as future enhancement |
| Passphrase loss | Irrecoverable by design | Accepted; admin retention requirement |

---

## 7  Validation Changes — Gateway

### T-GW-2000  Master key identification — first startup

**Covers:** §2.2 (master_key_id, master_key_epoch)
**Method:** Unit test

**Steps:**
1. Create `SqliteStorage` with a master key.
2. Verify no `master_key_id` or `master_key_epoch` in `gateway_config`.
3. Run startup initialization (§2.9 step 2c).
4. Verify `master_key_id` is a random 16-byte value (non-zero).
5. Verify `master_key_epoch = 1`.
6. Verify all existing PSK records have `master_key_id` and
   `master_key_epoch` backfilled.
7. Restart — verify same values are loaded (not regenerated).

**Pass criteria:** Stable master_key_id across restarts; epoch = 1; all
records backfilled.

---

### T-GW-2001  Gateway ACTUAL_STATE publication

**Covers:** §2.4
**Method:** Integration test

**Steps:**
1. Start gateway with identity, master key, and registered nodes.
2. Connect a mock connector consumer.
3. Verify gateway ACTUAL_STATE is received with `entity_kind = "gateway"`.
4. Verify fields: `entity_id`, `channel`, `master_key_id`, `master_key_epoch`,
   `x25519_public_key`, `fingerprint_words`, `gateway_version`, `gateway_commit`.
5. Verify `fingerprint_words` matches independent computation from public key.
6. Verify `rotation_in_progress = false`.
7. Verify node ACTUAL_STATE includes `encrypted_psk` (key 12),
   `escrow_key_hint` (key 13), and `master_key_id` (key 14) for each
   registered node.

**Pass criteria:** Gateway ACTUAL_STATE emitted on startup with all required
fields.

---

### T-GW-2002  Gateway DESIRED_STATE channel change

**Covers:** §2.5
**Method:** Integration test

**Steps:**
1. Start gateway on channel 1.
2. Deliver gateway DESIRED_STATE with `channel = 5`.
3. Verify gateway switches to channel 5.
4. Verify updated ACTUAL_STATE reports `channel = 5`.

**Pass criteria:** Channel converges to desired value.

---

### T-GW-2003  Rotation code authentication

**Covers:** §2.3, §2.6
**Method:** Integration test

**Steps:**
1. Start gateway, read rotation code from `gateway_config`.
2. Submit rotation payload with correct rotation code — verify accepted.
3. Verify new rotation code generated (different from old).
4. Submit rotation payload with old (used) code — verify rejected.
5. Submit rotation payload with wrong code — verify rejected.
6. Verify rejection is logged as a warning.

**Pass criteria:** Only correct, unused rotation codes are accepted.

---

### T-GW-2004  Master key rotation — happy path

**Covers:** §2.6
**Method:** Integration test

**Steps:**
1. Start gateway with 3 registered nodes and 1 phone PSK.
2. Record old `master_key_id` and `master_key_epoch`.
3. Submit valid rotation payload via DESIRED_STATE.
4. Verify all PSK records updated with new `master_key_id` and
   `master_key_epoch = old_epoch + 1`.
5. Verify old master key no longer decrypts any PSK record.
6. Verify new master key decrypts all PSK records.
7. Verify updated gateway ACTUAL_STATE: new epoch, new id,
   `rotation_in_progress = false`.
8. Verify node ACTUAL_STATE re-emitted with new `encrypted_psk`
   and `master_key_id`.

**Pass criteria:** All PSKs migrated; old key unusable; state updated.

---

### T-GW-2004a  Rotation validation failures

**Covers:** §2.6
**Method:** Unit test

**Steps:**
1. Submit rotation with wrong `master_key_epoch` in AAD — verify rejected.
2. Submit rotation with wrong `rotation_code` — verify rejected.
3. Submit rotation with corrupted ciphertext — verify decryption failure.
4. Submit identical rotation twice (replay) — verify second rejected
   (epoch already incremented).

**Pass criteria:** All invalid rotations rejected.

---

### T-GW-2005  Crash-safe key rotation

**Covers:** §2.6 crash recovery
**Method:** Integration test

**Steps:**
1. Start gateway with 10 registered nodes.
2. Begin key rotation, simulate crash after 5 nodes migrated.
3. Restart gateway — verify `pending_rotation` detected.
4. Verify `rotation_in_progress = true` in ACTUAL_STATE.
5. Verify auto-resume migrates remaining 5 nodes.
6. Verify all 10 nodes have new `master_key_id` and `master_key_epoch`.
7. Verify `pending_rotation` deleted.
8. Verify `rotation_in_progress = false` in ACTUAL_STATE.

**Pass criteria:** Partial rotation is resumed and completed after crash.

---

### T-GW-2006  Declarative node recovery

**Covers:** §2.8
**Method:** Integration test

**Steps:**
1. Start gateway with an empty local registry.
2. Send a valid encrypted WAKE frame with unknown `key_hint`.
3. Verify `missing_key_hints` includes the key_hint in next ACTUAL_STATE.
4. Verify the hint is cleared from subsequent ACTUAL_STATE emissions.
5. Send same key_hint within 60 seconds — verify NOT re-reported (rate limit).
6. Deliver `recovered_psks` in DESIRED_STATE with matching key_hint
   and `master_key_id`.
7. Verify PSK stored in `pending_recovery` table.
8. Send another frame with same key_hint — verify frame is processed
   using the recovered PSK.
9. Verify node promoted from `pending_recovery` to `nodes` table.

**Pass criteria:** Full recovery cycle works; rate limiting enforced.

---

### T-GW-2006a  Provisional recovery — wrong PSK

**Covers:** §2.8
**Method:** Unit test

**Steps:**
1. Insert a bogus PSK into `pending_recovery`.
2. Send a frame with matching `key_hint`.
3. Verify trial-decryption fails.
4. Verify bogus record remains in `pending_recovery` (not promoted).
5. Verify the bogus record is purged after 24 hours.

**Pass criteria:** Bad PSKs do not pollute the nodes table.

---

### T-GW-2006b  Provisional recovery — mismatched master_key_id

**Covers:** §2.8
**Method:** Unit test

**Steps:**
1. Deliver `recovered_psks` with `master_key_id` that doesn't match
   the gateway's current key.
2. Verify the record is skipped (not inserted into `pending_recovery`).

**Pass criteria:** PSKs from a different key era are rejected.

---

### T-GW-2006c  Phone PSKs not escrowed

**Covers:** §2.7 (phone escrow exclusion)
**Method:** Integration test

**Steps:**
1. Start gateway with 2 registered nodes and 1 phone PSK.
2. Connect a mock connector consumer.
3. Verify node ACTUAL_STATE includes `encrypted_psk` (key 12),
   `escrow_key_hint` (key 13), and `master_key_id` (key 14) for each node.
4. Verify NO ACTUAL_STATE is emitted with `entity_kind = "phone"`.
5. Perform key rotation.
6. Verify node ACTUAL_STATE is re-emitted with updated escrow fields.
7. Verify phone PSK is re-encrypted with new key in local DB.
8. Verify still no phone ACTUAL_STATE emitted.

**Pass criteria:** Phone PSKs rotate locally but are never published to
the connector.

---

### T-GW-2007  Salt management

**Covers:** §2.5, §2.6
**Method:** Integration test

**Steps:**
1. Start gateway with no local salt — verify `salt = null` in ACTUAL_STATE.
2. Deliver DESIRED_STATE with salt — verify gateway adopts it.
3. Verify subsequent ACTUAL_STATE reports the adopted salt.
4. Deliver DESIRED_STATE with a different salt — verify gateway keeps
   its existing salt (local wins once set).
5. Perform rotation with salt in payload — verify salt updated.

**Pass criteria:** Salt adoption and immutability semantics correct.

---

### T-GW-2008  gRPC rotation path

**Covers:** §5.1.1
**Method:** Integration test

**Steps:**
1. Start gateway with identity and master key.
2. Read rotation code from `gateway_config`.
3. Build a valid `RotationPayloadV1` with correct rotation code.
4. Submit via gRPC `SubmitRotation`.
5. Verify rotation succeeds (new epoch in ACTUAL_STATE).
6. Submit same payload again — verify rejected (epoch already incremented).
7. Submit via DESIRED_STATE with another valid payload — verify both
   paths use the same rotation handler.

**Pass criteria:** gRPC and DESIRED_STATE rotation paths are equivalent.

---

### T-GW-2009  Crash recovery — all rotation phases

**Covers:** §2.6.2 crash recovery
**Method:** Integration test

**Steps:**
1. Crash during `migrating_psks` phase — verify PSK migration resumes.
2. Crash during `rewrapping_identity` phase — verify identity rewrap
   resumes, and identity is loadable with the old key on restart.
3. Crash during `committing` phase — the `encrypted_seed_new` column
   holds the new-key version but `encrypted_seed` still uses the old key.
   Crash recovery completes the atomic commit transaction (step 7), which
   promotes `encrypted_seed_new` → `encrypted_seed` and activates the
   new master key. Verify identity is loadable after recovery.
4. For each phase, verify the gateway identity is always loadable on
   restart (no key/identity mismatch).

**Pass criteria:** Crash at any phase boundary recovers correctly without
identity loading failure.

---

## 8  Validation Changes — Azure Handler

### T-AZH-2000  Gateway ACTUAL_STATE storage

**Covers:** §4.1, §4.2
**Method:** Integration test

**Steps:**
1. Send gateway ACTUAL_STATE with all fields populated.
2. Verify row created in `ActualState` table with `entity_kind = "gateway"`.
3. Verify all fields stored correctly.
4. Send updated ACTUAL_STATE — verify row updated (upsert).

**Pass criteria:** Gateway state stored and updated correctly.

---

### T-AZH-2001  Node PSK escrow storage

**Covers:** §4.1, §4.2
**Method:** Integration test

**Steps:**
1. Send node ACTUAL_STATE with `encrypted_psk`, `master_key_id`, `key_hint`.
2. Verify stored in `ActualState` row alongside other node state.
3. Query by `key_hint` — verify the record is findable.

**Pass criteria:** PSK escrow data stored and queryable.

---

### T-AZH-2002  Missing key_hint recovery

**Covers:** §4.2
**Method:** Integration test

**Steps:**
1. Store node ACTUAL_STATE with `encrypted_psk`, `master_key_id = X`,
   `key_hint = 42`.
2. Store gateway ACTUAL_STATE with `master_key_id = X`,
   `missing_key_hints = [42]`.
3. Verify handler constructs gateway DESIRED_STATE with `recovered_psks`
   containing the matching node's PSK.
4. Store gateway ACTUAL_STATE with `master_key_id = Y` (different key),
   `missing_key_hints = [42]`.
5. Verify handler does NOT include the PSK (key mismatch).

**Pass criteria:** Recovery PSKs matched by key_hint AND master_key_id.

---

### T-AZH-2003  Rotation payload relay

**Covers:** §4.4
**Method:** Integration test

**Steps:**
1. SPA submits rotation payload via handler API.
2. Verify handler includes it in gateway DESIRED_STATE.
3. Gateway reports new `master_key_epoch` in ACTUAL_STATE.
4. Verify handler clears `rotation_payload` from DESIRED_STATE.

**Pass criteria:** Rotation payload relayed and cleared after completion.

---

## 9  Design Changes — Web UI (SPA)

The SPA rotation flow is fully specified in the web-ui spec trifecta:

- **Requirements:** `web-ui-requirements.md` §15 Key Management (WEB-1000 series)
- **Design:** `web-ui-design.md` §13 Key Management
- **Validation:** `web-ui-validation.md` T-WEB-1000 series

The SPA performs master key rotation via the following flow:

1. Read gateway ACTUAL_STATE from Azure `actualstate` Table
   (`PartitionKey = "g:" + gateway_id_hex`, `RowKey = "state"`).
2. Compute BIP-39 fingerprint **locally** from the `x25519_public_key`
   field (SHA-256 → 66 bits → 6 BIP-39 words). The SPA MUST NOT use
   the `fingerprint_words` field stored in Azure — a compromised Azure
   could substitute a rogue public key with pre-matched words. Display
   the locally-computed fingerprint and prompt operator to verify against
   the modem display.
3. Prompt for rotation code (from modem display) and passphrase.
4. Derive new master key: `Argon2id(passphrase, salt, kdf_params)` using
   a WASM Argon2id implementation.
5. Construct `RotationPayloadV1` (§2.6.1) using browser-side cryptography:
   X25519 key exchange via `noble-curves`, HKDF-SHA-256 and AES-256-GCM
   via Web Crypto API.
6. Write `rotation_payload` into gateway DESIRED_STATE row in Azure
   `desiredstate` Table (`PartitionKey = "g:" + gateway_id_hex`).
7. Poll gateway ACTUAL_STATE for `master_key_epoch` increment.

The SPA authenticates via Entra ID (MSAL.js) and accesses Azure Storage
Tables directly via REST API — the same pattern used for node desired
state management.

---

## 10  Removed Artifacts Summary

The following artifacts from evolve-887-specification.md are superseded:

| Artifact | Replacement |
|----------|-------------|
| §20.1–20.12 (escrow design sections) | This document §2.1–2.11 |
| §2.2 Storage trait extension (escrow keypair methods) | Removed; use GatewayIdentity |
| §2.3 Connector API extension (msg_types 0x10–0x13) | §3.1 (removed), §3.2–3.3 (ACTUAL/DESIRED_STATE fields) |
| §2.4 Startup sequence extension | §2.9 |
| §3.1 GatewayEscrow table | §4.1 (merged into ActualState table) |
| §3.1 ActualPhoneState table | §4.1 (merged into ActualState table) |
| §8.2 KEY_ESCROW_REQUEST/RESPONSE handling | §4.2 (declarative via ACTUAL/DESIRED_STATE) |
| §8.3 Salt first-writer-wins | §4.3 |
| §4.1 Admin key rotate (Azure-based) | §5.1 (gRPC-based, rotation code) |
| §6.1 Security threat analysis | §6.2 |
| T-2000–T-2009 validation | §7 T-GW-2000–T-GW-2007, §8 T-AZH-2000–T-AZH-2003 |
| `EscrowState` enum | Removed (rotation tracked by `pending_rotation` table) |
| `EscrowBlob` struct | Removed (raw encrypted PSK + master_key_id) |
| `RecoveryQueue` struct | Removed (declarative via ACTUAL/DESIRED_STATE) |
| `EscrowKeypair` / `escrow_keypair` table | Removed (unified with GatewayIdentity) |
| `ConnectorOutboundMessage::KeyEscrowPubkey` | Removed |
| `ConnectorOutboundMessage::KeyEscrowRequest` | Removed |
| `ConnectorEventHub::emit_key_escrow_pubkey()` | Removed |
| `ConnectorEventHub::emit_key_escrow_request()` | Removed |
| `key_version` column concept | Replaced by `master_key_id` + `master_key_epoch` |
