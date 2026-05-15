<!-- SPDX-License-Identifier: MIT
     Copyright (c) 2026 sonde contributors -->
# Specification Patch: PSK Key Escrow for Gateway Replaceability

> **Issue:** #887
> **Status:** Draft — Phase 2 specification changes
> **Scope:** Design and validation changes propagated from the requirements
> patch (GW-2000–GW-2011, AZH-0600–AZH-0605, ADMIN-0900–ADMIN-0902).
> **Traceability:** Every section traces to one or more requirement IDs.

---

## 1  Impact Analysis

### 1.1  Affected design documents

| Document | Affected sections | Change type |
|----------|-------------------|-------------|
| `gateway-design.md` | §10 Storage trait, §10a Master key provider, §13A Connector API, §15 Startup sequence, NEW §20 | Extend + new section |
| `gateway-companion-api.md` | §3.1 Message types, §3.3 ACTUAL_STATE, NEW §3.6–3.9 | Extend + new sections |
| `azure-handler-design.md` | §4 Table schemas, §5 Reconciliation, NEW §8 | Extend + new section |
| `admin-design.md` | NEW §11 | New section |
| `modem-design.md` | §9a Display output | Extend |
| `security.md` | §2.3 Key storage | Extend |

### 1.2  Affected validation documents

| Document | Change type |
|----------|-------------|
| `gateway-validation.md` | New test series T-2000–T-2011 |
| `azure-handler-validation.md` | New test series T-AZH-0600–T-AZH-0605 |
| `admin-validation.md` | New test series T-0900–T-0902 |

---

## 2  Design Changes — Gateway

### 2.1  New section: §20 PSK Key Escrow (gateway-design.md)

> **Requirements:** GW-2000–GW-2011

#### 20.1  Overview

The escrow subsystem enables gateway replaceability by securely escrowing
encrypted PSKs to Azure via the connector API. The design introduces four
new capabilities:

1. **Recovery keypair** — X25519 keypair for secure master-key delivery.
2. **Escrow blob emission** — encrypted PSKs sent upstream in ACTUAL_STATE.
3. **Key rotation** — admin-initiated master key replacement with crash-safe
   record migration.
4. **Auto-heal recovery** — on-demand PSK retrieval for unknown nodes.

#### 20.2  Recovery keypair (GW-2000, GW-2001)

On first startup, the gateway generates an X25519 keypair:

```rust
pub struct RecoveryKeypair {
    /// X25519 secret key (32 bytes), zeroized on drop.
    secret: Zeroizing<[u8; 32]>,
    /// X25519 public key (32 bytes).
    public: [u8; 32],
    /// Monotonic epoch, incremented on each regeneration.
    epoch: u64,
}
```

The keypair is persisted in the gateway database:

```sql
CREATE TABLE IF NOT EXISTS escrow_keypair (
    id          INTEGER PRIMARY KEY CHECK (id = 1),
    secret_enc  BLOB NOT NULL,     -- private key, AES-256-GCM encrypted with master key
    public_key  BLOB NOT NULL,     -- raw 32-byte X25519 public key
    epoch       INTEGER NOT NULL,  -- monotonic key epoch
    created_at  INTEGER NOT NULL   -- Unix milliseconds
);
```

The private key is encrypted at rest with the current master key
(same `encrypt_psk` pattern used for node PSKs). On startup, if the
`escrow_keypair` table is empty, a new keypair is generated. If decryption
fails (master key mismatch after recovery), a new keypair is generated
and the epoch is incremented.

The public key is published to the connector via `KEY_ESCROW_PUBKEY`
(msg_type `0x10`) on every startup and whenever the keypair changes.

#### 20.3  Escrow blob format (GW-2002)

Each encrypted PSK is wrapped in a canonical escrow envelope:

```rust
pub struct EscrowBlob {
    pub escrow_version: u8,         // schema version (1)
    pub key_version: u64,           // master key version
    pub subject_kind: SubjectKind,  // Node or Phone
    pub subject_id: String,         // node_id or phone_id
    pub key_hint: u16,              // PSK key_hint
    pub nonce: [u8; 12],            // AES-256-GCM nonce
    pub ciphertext: [u8; 32],       // encrypted PSK
    pub tag: [u8; 16],              // GCM authentication tag
}

pub enum SubjectKind {
    Node,
    Phone,
}
```

CBOR encoding uses integer keys (1–8) as defined in GW-2002. The AAD for
AES-256-GCM encryption is the CBOR-encoded map of fields 1–5 (escrow_version,
key_version, subject_kind, subject_id, key_hint). This binds the ciphertext
to the subject identity, preventing blob swap attacks.

Total CBOR-encoded size: ≤ 120 bytes typical.

#### 20.4  Escrow emission in ACTUAL_STATE (GW-2003)

A new CBOR key `12` (`encrypted_psk_escrow`) is added to node-scoped
ACTUAL_STATE payloads. The value is a bstr containing the CBOR-encoded
`EscrowBlob`.

For phone PSKs, the gateway emits ACTUAL_STATE messages with
`entity_kind = "phone"` and `entity_id = phone_id`. The phone ACTUAL_STATE
payload contains only `encrypted_psk_escrow` (key 12) and `timestamp_ms`
(key 9).

Escrow blobs are emitted:
- On node/phone registration (initial escrow).
- After key rotation (re-encrypted blobs for all subjects).
- On connector reconnection (full state replay).

When escrow state is `disabled`, the field is `null`.

#### 20.5  Escrow lifecycle state machine (GW-2004)

```
                 ┌──────────┐
     startup ──► │ disabled │
                 └────┬─────┘
                      │ first MASTER_KEY_INSTALL
                      ▼
                 ┌──────────────┐
                 │ bootstrapping│
                 └────┬─────────┘
                      │ all PSKs re-encrypted + emitted
                      ▼
                 ┌──────────┐
           ┌───► │  ready   │ ◄───┐
           │     └────┬─────┘     │
           │          │ new MASTER_KEY_INSTALL
           │          ▼           │
           │     ┌────────────────┴──┐
           │     │ rotation_in_progress │
           │     └────┬──────────────┘
           │          │ all PSKs migrated
           │          │
           │     ┌────▼─────┐
           └─────┤  ready   │
                 └──────────┘

  On crash during bootstrapping or rotation:
                 ┌──────────┐
     restart ──► │ degraded │ ──► auto-resume ──► ready
                 └──────────┘
```

Persisted in `gateway_config` as key `escrow_state`.

The escrow state is reported in gateway-scoped ACTUAL_STATE via a new
`status_details` sub-key (CBOR key 1 within the `status_details` map):

| Field | CBOR key (in `status_details`) | Type | Description |
|-------|-------------------------------|------|-------------|
| `escrow_state` | 1 | tstr | One of: `"disabled"`, `"bootstrapping"`, `"ready"`, `"rotation_in_progress"`, `"degraded"` |
| `escrow_key_version` | 2 | uint/null | Current master key version, or null if disabled |

#### 20.6  Key version tracking (GW-2005)

A new column `key_version INTEGER NOT NULL DEFAULT 0` is added to the
`nodes` and `phone_psks` tables via migration:

```sql
-- Migration: add key_version to nodes
ALTER TABLE nodes ADD COLUMN key_version INTEGER NOT NULL DEFAULT 0;
-- Migration: add key_version to phone_psks
ALTER TABLE phone_psks ADD COLUMN key_version INTEGER NOT NULL DEFAULT 0;
```

The current key version is stored in `gateway_config` as key
`escrow_key_version`. Initial value is `0` (pre-escrow state).

#### 20.7  Master key rotation (GW-2006, GW-2007)

On receiving a `MASTER_KEY_INSTALL` (msg_type `0x13`), the gateway:

1. **Validate**: Check `target_key_epoch` matches current keypair epoch.
   Check `operation_id` has not been seen before (dedup table). Check
   `expiry_ms` has not passed. Reject and log if any check fails.

2. **Decrypt**: Use X25519 + HKDF-SHA-256 + AES-256-GCM to decrypt the
   master key payload:
   - Perform X25519 DH: `shared_secret = X25519(private_key, sender_public_key)`
   - Derive encryption key: `key = HKDF-SHA-256(shared_secret, salt="sonde-escrow-v1", info=target_key_epoch || operation_id)`
   - Decrypt: `new_master_key = AES-256-GCM-Open(key, nonce, ciphertext, aad=operation_id || target_key_epoch)`

3. **Prepare**: Write `pending_rotation` record to database:
   ```sql
   INSERT OR REPLACE INTO pending_rotation (id, new_master_key_enc, new_key_version, operation_id, started_at)
   VALUES (1, ?, ?, ?, ?);
   ```
   The new master key is encrypted with the OLD master key for crash safety.

4. **Migrate**: For each PSK record (nodes + phone_psks) where
   `key_version < new_key_version`:
   - Decrypt PSK with old master key.
   - Re-encrypt PSK with new master key.
   - Update record with new ciphertext and new `key_version`.
   - Commit each record individually.

5. **Commit**: Update master key in KeyProvider storage. Remove
   `pending_rotation` record. Set `escrow_key_version` in `gateway_config`.
   Update escrow state to `ready`.

6. **Emit**: Re-emit ACTUAL_STATE with new escrow blobs for all subjects.

7. **Cleanup**: Zero old master key from memory.

**Crash recovery (GW-2007):** On startup, if `pending_rotation` exists:
- Decrypt the pending new master key using the current (old) master key.
- Resume migration from step 4, processing only records where
  `key_version < new_key_version`.
- The two-key window ensures both old and new PSKs can be decrypted
  during migration.

**Deduplication table:**
```sql
CREATE TABLE IF NOT EXISTS escrow_operations (
    operation_id BLOB PRIMARY KEY,
    processed_at INTEGER NOT NULL
);
```

#### 20.8  Salt management (GW-2008)

The KDF salt record is stored in `gateway_config`:

| Config key | Value | Description |
|------------|-------|-------------|
| `escrow_salt` | hex-encoded 16 bytes | Argon2id salt |
| `escrow_argon2_m_cost` | uint string | Memory cost in KiB (default: 65536) |
| `escrow_argon2_t_cost` | uint string | Time cost / passes (default: 3) |
| `escrow_argon2_p_cost` | uint string | Parallelism (default: 1) |
| `escrow_kdf_version` | uint string | Schema version (default: 1) |
| `escrow_salt_created_at` | uint string | Unix milliseconds |

Published via gateway-scoped ACTUAL_STATE in `status_details`.
Adopted from Azure (first-writer-wins) during connector session setup.

#### 20.9  Unknown node recovery (GW-2009, GW-2010)

When a frame arrives with an unknown `key_hint` and escrow state is `ready`:

1. Check rate limiter: at most 1 request per `key_hint` per 60 seconds.
2. Buffer the raw frame bytes in a bounded recovery queue (max 64 entries,
   30-second TTL per entry).
3. Emit `KEY_ESCROW_REQUEST` (msg_type `0x11`) to the connector.
4. On receiving `KEY_ESCROW_RESPONSE` (msg_type `0x12`):
   a. Look up buffered frame by `request_id`.
   b. For each candidate blob (max 16):
      - Decrypt escrow blob with master key → PSK.
      - Attempt trial-decryption of buffered frame with candidate PSK.
   c. On first successful decryption:
      - Register the node/phone locally (upsert).
      - Process the frame normally.
      - Emit ACTUAL_STATE with escrow blob.
   d. If no candidate succeeds, discard frame with warning log.
5. If response arrives after TTL expiry, discard with warning log.

```rust
struct RecoveryQueue {
    entries: HashMap<[u8; 16], RecoveryEntry>, // request_id → entry
    hint_rate: HashMap<u16, Instant>,           // key_hint → last request time
}

struct RecoveryEntry {
    key_hint: u16,
    raw_frame: Vec<u8>,
    peer_address: [u8; 6],
    created_at: Instant,
}
```

#### 20.10  Fingerprint computation (GW-2011)

The BIP-39 wordlist fingerprint is computed as:

```rust
fn compute_fingerprint(public_key: &[u8; 32]) -> [&'static str; 6] {
    let hash = sha256(public_key);
    let mut words = [""; 6];
    // Extract 66 bits (6 × 11-bit indices) from hash bytes 0..9
    let bits = u128::from_be_bytes([
        hash[0], hash[1], hash[2], hash[3],
        hash[4], hash[5], hash[6], hash[7],
        hash[8], 0, 0, 0, 0, 0, 0, 0,
    ]);
    for i in 0..6 {
        let index = ((bits >> (128 - 11 - 11 * i)) & 0x7FF) as usize;
        words[i] = BIP39_ENGLISH[index];
    }
    words
}
```

The BIP-39 English wordlist (2048 entries) is embedded as a compile-time
constant in `sonde-protocol` so both gateway and admin/SPA share the same
list. The wordlist is the standard list from
[BIP-0039](https://github.com/bitcoin/bips/blob/master/bip-0039/english.txt).

The gateway renders the fingerprint as a display page using the existing
reliable display-transfer subprotocol (GW-1101b page navigation). Layout:
three rows of two words each, centered on the 128×64 OLED.

### 2.2  Storage trait extension (gateway-design.md §10)

The `Storage` trait gains escrow-related methods:

```rust
// Escrow keypair (GW-2000)
async fn get_escrow_keypair(&self) -> Result<Option<EscrowKeypairRecord>>;
async fn store_escrow_keypair(&self, record: &EscrowKeypairRecord) -> Result<()>;

// Escrow operations dedup (GW-2006)
async fn is_operation_processed(&self, operation_id: &[u8]) -> Result<bool>;
async fn record_operation(&self, operation_id: &[u8]) -> Result<()>;

// Pending rotation (GW-2007)
async fn get_pending_rotation(&self) -> Result<Option<PendingRotationRecord>>;
async fn store_pending_rotation(&self, record: &PendingRotationRecord) -> Result<()>;
async fn delete_pending_rotation(&self) -> Result<()>;
```

The existing `upsert_node` and `upsert_phone_psk` methods are unchanged;
they already handle `key_version` via the column added by migration.

### 2.3  Connector API extension (gateway-companion-api.md §3)

Four new message types are added to the connector API:

| `msg_type` | Name | Direction | Description |
|------------|------|-----------|-------------|
| `0x10` | `KEY_ESCROW_PUBKEY` | Gateway → control plane | Recovery public key publication |
| `0x11` | `KEY_ESCROW_REQUEST` | Gateway → control plane | Request escrowed PSK(s) for a key_hint |
| `0x12` | `KEY_ESCROW_RESPONSE` | Control plane → gateway | Escrowed PSK candidate(s) |
| `0x13` | `MASTER_KEY_INSTALL` | Control plane → gateway | Encrypted new master key |

These use imperative/transactional semantics (operation IDs, expiry,
idempotency) rather than the replacement semantics of DESIRED_STATE/
ACTUAL_STATE.

**§3.6  `KEY_ESCROW_PUBKEY` (gateway → control plane)**

| Field | CBOR key | Type | Description |
|-------|----------|------|-------------|
| `msg_type` | 1 | uint | `0x10` |
| `public_key` | 2 | bstr (32 bytes) | X25519 public key |
| `key_epoch` | 3 | uint | Monotonic key epoch |
| `created_at` | 4 | uint | Creation timestamp (Unix ms) |
| `fingerprint_words` | 5 | array of tstr | 6-word BIP-39 fingerprint (informational) |

**§3.7  `KEY_ESCROW_REQUEST` (gateway → control plane)**

| Field | CBOR key | Type | Description |
|-------|----------|------|-------------|
| `msg_type` | 1 | uint | `0x11` |
| `key_hint` | 2 | uint | Key hint from undecryptable frame |
| `request_id` | 3 | bstr (16 bytes) | Unique request ID |

**§3.8  `KEY_ESCROW_RESPONSE` (control plane → gateway)**

| Field | CBOR key | Type | Description |
|-------|----------|------|-------------|
| `msg_type` | 1 | uint | `0x12` |
| `request_id` | 2 | bstr (16 bytes) | Correlates to request |
| `candidates` | 3 | array of bstr | Array of CBOR-encoded EscrowBlob (max 16) |
| `key_hint` | 4 | uint | Echo of requested key_hint |

**§3.9  `MASTER_KEY_INSTALL` (control plane → gateway)**

| Field | CBOR key | Type | Description |
|-------|----------|------|-------------|
| `msg_type` | 1 | uint | `0x13` |
| `target_key_epoch` | 2 | uint | Must match gateway's current public key epoch |
| `sender_public_key` | 3 | bstr (32 bytes) | Admin's ephemeral X25519 public key |
| `encrypted_master_key` | 4 | bstr | AES-256-GCM ciphertext of the new master key |
| `nonce` | 5 | bstr (12 bytes) | AES-256-GCM nonce |
| `tag` | 6 | bstr (16 bytes) | AES-256-GCM authentication tag |
| `operation_id` | 7 | bstr (16 bytes) | Unique operation ID for idempotency |
| `rotation_counter` | 8 | uint | Monotonic rotation counter |
| `expiry_ms` | 9 | uint | Message expiry (Unix ms) |

**ACTUAL_STATE extension (§3.3):**

New field in node-scoped ACTUAL_STATE:

| Field | CBOR key | Type | Description |
|-------|----------|------|-------------|
| `encrypted_psk_escrow` | 12 | bstr/null | CBOR-encoded EscrowBlob |

New `entity_kind = "phone"` for phone PSK escrow.

New fields in gateway-scoped ACTUAL_STATE `status_details`:

| Field | CBOR key (in `status_details`) | Type | Description |
|-------|-------------------------------|------|-------------|
| `escrow_state` | 1 | tstr | Lifecycle state |
| `escrow_key_version` | 2 | uint/null | Current key version |
| `escrow_salt` | 3 | bstr/null | KDF salt (16 bytes) |
| `escrow_kdf_params` | 4 | map/null | `{1: m_cost, 2: t_cost, 3: p_cost, 4: kdf_version}` |

### 2.4  Startup sequence extension (gateway-design.md §15)

Insert after step 2 (Initialize storage backend):

> 2a. Load or generate escrow keypair (GW-2000). If `pending_rotation`
>     exists, resume key rotation (GW-2007).

Insert after step 9 (Start connector API server):

> 9a. Emit `KEY_ESCROW_PUBKEY` on connector (GW-2001).
> 9b. Emit gateway-scoped ACTUAL_STATE with escrow state and salt (GW-2004, GW-2008).

Add fingerprint page to modem display page list (after GW-1101a banner):

> 5a. If escrow keypair exists, register fingerprint as a navigable display
>     page (GW-2011).

#### 20.11  Replacement gateway bootstrap (GW-2012)

A replacement gateway with an empty database follows this sequence:

1. Normal startup (§15 steps 1–5): generate random master key, initialize
   storage, generate escrow keypair.
2. Escrow state = `disabled`.
3. Publish `KEY_ESCROW_PUBKEY` to connector.
4. Display fingerprint on modem.
5. Fetch salt from Azure via connector (adopt if available).
6. **Wait for admin**: The gateway operates normally (answering registered
   nodes) but silently discards unknown `key_hint` frames per GW-1002.
   It does NOT emit `KEY_ESCROW_REQUEST`.
7. Admin performs `sonde-admin key rotate` → `MASTER_KEY_INSTALL` arrives.
8. Gateway processes rotation (GW-2006) — re-encrypts any local PSKs (none
   for a fresh gateway), installs the passphrase-derived master key.
9. Escrow state transitions to `ready`.
10. From this point, unknown `key_hint` frames trigger recovery (GW-2009).

#### 20.12  Connector key-management security (GW-2013)

Key-management messages on the connector are secured as follows:

| Message | Authentication mechanism |
|---------|------------------------|
| `MASTER_KEY_INSTALL` | X25519 key agreement binds to gateway's pubkey. `target_key_epoch` prevents cross-gateway replay. `operation_id` prevents same-gateway replay. `expiry_ms` prevents delayed delivery. |
| `KEY_ESCROW_RESPONSE` | Each escrow blob is individually AES-256-GCM authenticated via AAD binding (GW-2002). Tampered/swapped blobs fail decryption. |
| `KEY_ESCROW_REQUEST` | Gateway-originated only. Control plane cannot trigger. |
| `KEY_ESCROW_PUBKEY` | Informational. Admin verifies via out-of-band fingerprint on modem. |

**Rotation step fix:** Step 5 of §20.7 re-encrypts the recovery private
key (`escrow_keypair.secret_enc`) under the new master key. The
`pending_rotation` record tracks whether private-key rewrapping has
completed (`privkey_rewrapped BOOLEAN DEFAULT FALSE`). On crash recovery,
if `privkey_rewrapped = FALSE`, the private key is re-encrypted before
migration resumes.

---

## 3  Design Changes — Azure Handler

### 3.1  New section: §8 PSK Key Escrow (azure-handler-design.md)

> **Requirements:** AZH-0600–AZH-0605

#### 8.1  Table schema extensions

**GatewayEscrow table** (new):

| Column | Type | Description |
|--------|------|-------------|
| PartitionKey | string | Gateway identifier (fixed "gateway") |
| RowKey | string | `"pubkey"` or `"salt"` |
| PublicKey | binary | X25519 public key (32 bytes) |
| KeyEpoch | int64 | Monotonic epoch |
| Salt | binary | KDF salt (16 bytes) |
| KdfParams | string | JSON: `{"m_cost":65536,"t_cost":3,"p_cost":1,"version":1}` |
| CreatedAt | int64 | Unix milliseconds |

**ActualNodeState table extension** (existing table):

| Column | Type | Description |
|--------|------|-------------|
| EncryptedPskEscrow | binary/null | Opaque escrow blob from gateway |
| EscrowKeyVersion | int64/null | Key version of the escrow blob |

**ActualPhoneState table** (new):

| Column | Type | Description |
|--------|------|-------------|
| PartitionKey | string | Gateway identifier |
| RowKey | string | Phone ID |
| EncryptedPskEscrow | binary | Opaque escrow blob |
| EscrowKeyVersion | int64 | Key version |
| TimestampMs | int64 | Last update timestamp |

#### 8.2  Message handling

**`KEY_ESCROW_PUBKEY` (upstream, msg_type `0x10`):**
- Upsert `GatewayEscrow` row with RowKey `"pubkey"`.
- Only update if incoming `key_epoch` ≥ stored epoch (monotonic guard).

**`ACTUAL_STATE` with `encrypted_psk_escrow` (upstream):**
- For `entity_kind = "node"`: store blob in `ActualNodeState` row.
- For `entity_kind = "phone"`: store blob in `ActualPhoneState` row.
- The handler MUST NOT decrypt or inspect blob contents.

**`KEY_ESCROW_REQUEST` (upstream, msg_type `0x11`):**
- Query `ActualNodeState` and `ActualPhoneState` for rows matching
  the requested `key_hint` (stored as a column alongside the escrow blob).
- Return matching blobs as `KEY_ESCROW_RESPONSE` (msg_type `0x12`) via
  the downstream queue.
- Cap at 16 candidates.

**`MASTER_KEY_INSTALL` relay (downstream):**
- The handler does not originate `MASTER_KEY_INSTALL` messages. These are
  authored by the admin SPA and placed directly into the downstream queue
  (or relayed through a dedicated admin API endpoint on the handler).
- The handler relays them verbatim without decryption.

#### 8.3  Salt first-writer-wins (AZH-0603)

On receiving a gateway ACTUAL_STATE with `escrow_salt`:
- If no `GatewayEscrow` row with RowKey `"salt"` exists, create it.
- If a row exists, ignore the incoming salt (first-writer-wins).
- On subsequent gateway-scoped DESIRED_STATE emissions (e.g., after
  reconnect), include the stored salt so the gateway can adopt it.

#### 8.4  Key hint index

To efficiently serve `KEY_ESCROW_REQUEST`, the handler stores `key_hint`
as an indexed column in both `ActualNodeState` and `ActualPhoneState`.
The column is populated from the `key_hint` field within the escrow blob
metadata (CBOR key 5 of the EscrowBlob). Since the handler must not
decrypt the blob, the `key_hint` is also sent as a top-level field in
the ACTUAL_STATE message alongside `encrypted_psk_escrow`.

---

## 4  Design Changes — Admin CLI

### 4.1  New section: §11 Key Management Commands (admin-design.md)

> **Requirements:** ADMIN-0900–ADMIN-0902

#### 11.1  `sonde-admin key rotate`

Subcommand for master key rotation:

```
sonde-admin key rotate [--gateway-url <url>] [--azure-endpoint <url>]
```

Flow:
1. Fetch salt from Azure (or prompt to generate new salt for first rotation).
2. Fetch gateway's recovery public key from Azure.
3. Compute and display 6-word BIP-39 fingerprint.
4. Prompt: "Verify this fingerprint matches the modem display. Continue? [y/N]"
5. Prompt for passphrase (masked, minimum 20 characters or 6 words).
6. Derive master key: `Argon2id(passphrase, salt, m=65536, t=3, p=1)`.
7. Generate ephemeral X25519 keypair for key encapsulation.
8. Compute shared secret: `X25519(ephemeral_secret, gateway_public_key)`.
9. Derive encryption key: `HKDF-SHA-256(shared_secret, "sonde-escrow-v1", ...)`.
10. Encrypt master key with derived encryption key.
11. Build `MASTER_KEY_INSTALL` message with operation_id, rotation_counter,
    expiry (5 minutes from now).
12. Send via Azure downstream queue (or direct gRPC if local).
13. Poll escrow status until `ready` or timeout.

Passphrase and all derived keys are `Zeroizing`-wrapped.

#### 11.2  `sonde-admin key fingerprint`

Display-only command:

```
sonde-admin key fingerprint [--azure-endpoint <url>]
```

Fetches the gateway's public key from Azure and displays the 6-word
BIP-39 fingerprint. No confirmation or key operations.

#### 11.3  `sonde-admin key status`

```
sonde-admin key status [--gateway-url <url>] [--azure-endpoint <url>]
```

Displays escrow state, key version, escrowed PSK counts, and KDF params.

---

## 5  Design Changes — Modem

### 5.1  Fingerprint display page (modem-design.md §9a extension)

No modem firmware changes are required. The modem is a passive display
sink (MD-0703). The gateway renders the fingerprint page using the
existing reliable display-transfer subprotocol.

The gateway adds a "Key Fingerprint" page to the display page rotation
(GW-1101b). Layout for 128×64 OLED at 6px-wide font (~21 chars/row):

```
Row 1 (y=8):   word1  word2
Row 2 (y=28):  word3  word4
Row 3 (y=48):  word5  word6
```

Words are center-aligned per row. The page title "KEY FINGERPRINT" is
rendered in smaller font at the top if space permits, or the words
fill the screen.

---

## 6  Design Changes — Security Model

### 6.1  New subsection: security.md §2.3.2 PSK key escrow

**Key hierarchy:**

```
passphrase + salt ──► Argon2id ──► master_key (32 bytes)
                                      │
                              ┌───────┴────────┐
                              ▼                ▼
                    Encrypt(PSK_node1)  Encrypt(PSK_node2) ...
                              │                │
                              ▼                ▼
                       Azure Storage    Azure Storage
                     (encrypted blobs)  (encrypted blobs)
```

**Master key delivery:**

```
Admin ──► Argon2id(passphrase, salt) ──► master_key
                                              │
                          X25519(ephemeral, gw_pubkey)
                                  │
                          HKDF + AES-256-GCM
                                  │
                                  ▼
                      encrypted_master_key ──► Azure ──► Gateway
                                                              │
                                                   X25519(gw_privkey, ephemeral)
                                                              │
                                                      HKDF + AES-256-GCM
                                                              │
                                                              ▼
                                                        master_key
```

**Threat analysis:**

| Threat | Mitigation | Residual risk |
|--------|------------|---------------|
| Azure compromise → PSK disclosure | PSKs encrypted with master key; master key never in Azure | None (AES-256-GCM) |
| Azure compromise → master key disclosure | Master key encrypted with gateway's public key; private key on gateway | None (X25519 + AES-256-GCM) |
| Azure MITM on public key | 6-word BIP-39 fingerprint verified by admin on modem display (66-bit work factor) | Targeted collision requires ~2^66 operations |
| Offline passphrase brute-force | Argon2id (64 MiB memory-hard) + ≥77-bit entropy | Computationally infeasible |
| key_hint amplification (radio→cloud) | Rate limiting: 1 request/key_hint/60s; max 16 candidates | Bounded amplification factor |
| Rotation crash → split-brain keys | Two-key window + key_version tracking + auto-resume | Temporary degraded state, auto-recoverable |
| Gateway physical compromise | Exposes master key in memory → all PSKs | Accepted; HSM/enclave as future enhancement |
| Passphrase loss | Irrecoverable by design | Accepted; admin retention requirement |

---

## 7  Validation Changes — Gateway

### New test series: T-2000 (PSK Key Escrow)

#### T-2000  Recovery keypair generation

**Covers:** GW-2000
**Method:** Unit test

**Steps:**
1. Create `SqliteStorage` with a master key.
2. Verify no keypair exists initially.
3. Call keypair generation.
4. Verify keypair is persisted and retrievable.
5. Restart with same master key — verify same keypair is loaded (not regenerated).
6. Restart with different master key — verify new keypair is generated with incremented epoch.

**Pass criteria:** Keypair persists across restarts with same master key; regenerated with different master key.

---

#### T-2001  Public key publication via connector

**Covers:** GW-2001
**Method:** Integration test

**Steps:**
1. Start gateway with escrow keypair.
2. Connect a mock connector consumer.
3. Verify `KEY_ESCROW_PUBKEY` (msg_type `0x10`) is received.
4. Verify it contains public_key, key_epoch, created_at, fingerprint_words.
5. Verify fingerprint_words matches independent computation.

**Pass criteria:** Public key message emitted on startup with correct fields.

---

#### T-2002  Escrow blob format — round-trip

**Covers:** GW-2002
**Method:** Unit test

**Steps:**
1. Create an EscrowBlob for a node PSK with known values.
2. Encrypt with a master key.
3. Verify CBOR-encoded size ≤ 150 bytes.
4. Decrypt with same master key — verify PSK matches.
5. Decrypt with wrong master key — verify authentication failure.
6. Tamper with subject_id in AAD — verify decryption failure.
7. Swap blob between two different node_ids — verify decryption failure.

**Pass criteria:** Round-trip succeeds; wrong key, tampered AAD, and swapped blobs all fail.

---

#### T-2003  Escrow blob in ACTUAL_STATE

**Covers:** GW-2003
**Method:** Integration test

**Steps:**
1. Register a node with escrow enabled (key_version > 0).
2. Verify ACTUAL_STATE contains `encrypted_psk_escrow` (CBOR key 12).
3. Verify the blob is a valid EscrowBlob with correct subject_id and key_hint.
4. Register a phone PSK — verify phone-scoped ACTUAL_STATE emitted.
5. With escrow disabled (key_version = 0), verify field is null.

**Pass criteria:** Escrow blobs present when enabled, null when disabled.

---

#### T-2004  Escrow lifecycle state machine

**Covers:** GW-2004
**Method:** Unit test + integration test

**Steps:**
1. Fresh gateway — verify state is `disabled`.
2. Perform first `MASTER_KEY_INSTALL` — verify state transitions through
   `bootstrapping` → `ready`.
3. Perform another rotation — verify `rotation_in_progress` → `ready`.
4. Simulate crash during rotation — verify state is `degraded` on restart.
5. Verify auto-resume completes and state becomes `ready`.

**Pass criteria:** All state transitions match the state machine diagram.

---

#### T-2005  Key version tracking

**Covers:** GW-2005
**Method:** Unit test

**Steps:**
1. Register a node — verify `key_version = 0`.
2. Perform key rotation — verify all nodes updated to `key_version = 1`.
3. Perform another rotation — verify `key_version = 2`.
4. Verify `key_version` is monotonically increasing (never decremented).

**Pass criteria:** Key versions increment correctly.

---

#### T-2006  Master key rotation — happy path

**Covers:** GW-2006
**Method:** Integration test

**Steps:**
1. Start gateway with 3 registered nodes and 1 phone PSK.
2. Send valid `MASTER_KEY_INSTALL` message.
3. Verify all PSK records are re-encrypted with new key version.
4. Verify old master key no longer decrypts any PSK record.
5. Verify new master key decrypts all PSK records.
6. Verify escrow blobs re-emitted via ACTUAL_STATE.
7. Verify escrow state is `ready`.

**Pass criteria:** All PSKs migrated; old key unusable; new blobs emitted.

---

#### T-2006a  Master key rotation — validation failures

**Covers:** GW-2006
**Method:** Unit test

**Steps:**
1. Send `MASTER_KEY_INSTALL` with wrong `target_key_epoch` — verify rejection.
2. Send `MASTER_KEY_INSTALL` with duplicate `operation_id` — verify rejection.
3. Send `MASTER_KEY_INSTALL` with expired `expiry_ms` — verify rejection.

**Pass criteria:** All three messages rejected with appropriate error logs.

---

#### T-2007  Crash-safe key rotation

**Covers:** GW-2007
**Method:** Integration test

**Steps:**
1. Start gateway with 10 registered nodes.
2. Begin key rotation, simulate crash after 5 nodes migrated.
3. Restart gateway — verify `pending_rotation` detected.
4. Verify escrow state is `degraded`.
5. Verify auto-resume migrates remaining 5 nodes.
6. Verify state transitions to `ready`.
7. Verify all 10 nodes have new `key_version`.

**Pass criteria:** Partial rotation is resumed and completed after crash.

---

#### T-2008  Salt management — first-writer-wins

**Covers:** GW-2008
**Method:** Integration test

**Steps:**
1. Start gateway with no local salt and no Azure salt — verify salt generated.
2. Restart gateway with Azure salt available but no local salt — verify
   Azure salt adopted.
3. Verify local and Azure salts match after adoption.
4. Start another gateway with a different local salt — verify warning logged
   and local salt used.

**Pass criteria:** First-writer-wins semantics upheld; conflict detected.

---

#### T-2009  Unknown node recovery request

**Covers:** GW-2009
**Method:** Integration test

**Steps:**
1. Start gateway with escrow `ready`, empty local registry.
2. Send a valid encrypted WAKE frame from a previously-escrowed node.
3. Verify `KEY_ESCROW_REQUEST` emitted with correct `key_hint`.
4. Send the same key_hint within 60 seconds — verify request is NOT re-emitted
   (rate limit).
5. Wait 60 seconds, send again — verify request IS re-emitted.
6. With escrow `disabled`, send unknown frame — verify silent discard only.

**Pass criteria:** Rate-limited recovery requests when enabled; silent discard when disabled.

---

#### T-2010  PSK recovery from escrowed blob

**Covers:** GW-2010
**Method:** Integration test

**Steps:**
1. Start gateway with escrow `ready`, empty registry, known master key.
2. Send WAKE frame from node whose PSK is escrowed.
3. Gateway emits `KEY_ESCROW_REQUEST`.
4. Respond with `KEY_ESCROW_RESPONSE` containing the correct escrow blob.
5. Verify node is registered locally.
6. Verify WAKE frame is processed (COMMAND response sent).
7. Verify ACTUAL_STATE emitted with escrow blob.

**Pass criteria:** Node recovered and operational after escrow response.

---

#### T-2010a  PSK recovery — multiple candidates with collision

**Covers:** GW-2010
**Method:** Unit test

**Steps:**
1. Create two nodes with colliding key_hints (different PSKs).
2. Send recovery response with both escrow blobs.
3. Verify the correct PSK is identified by trial-decryption.

**Pass criteria:** Correct node registered despite key_hint collision.

---

#### T-2010b  PSK recovery — expired buffer

**Covers:** GW-2010
**Method:** Unit test

**Steps:**
1. Send WAKE from unknown node.
2. Wait 31 seconds (past 30s TTL).
3. Send recovery response.
4. Verify frame is discarded with warning log.

**Pass criteria:** Expired recovery entries are cleaned up.

---

#### T-2011  Fingerprint computation determinism

**Covers:** GW-2011
**Method:** Unit test

**Steps:**
1. Compute fingerprint for a known public key.
2. Verify result matches expected 6 words.
3. Verify same public key always produces same words.
4. Verify different public key produces different words.

**Pass criteria:** Deterministic, reproducible fingerprint computation.

---

#### T-2012  Replacement gateway bootstrap — end-to-end

**Covers:** GW-2012
**Method:** Integration test

**Steps:**
1. Start a replacement gateway with empty DB.
2. Verify escrow state is `disabled`.
3. Verify fingerprint displayed on modem.
4. Send WAKE from a node whose PSK is escrowed in Azure.
5. Verify frame is silently discarded (no `KEY_ESCROW_REQUEST` emitted).
6. Perform `MASTER_KEY_INSTALL` with the correct passphrase-derived master key.
7. Verify escrow state transitions to `ready`.
8. Send same WAKE again.
9. Verify `KEY_ESCROW_REQUEST` emitted.
10. Respond with escrowed blob.
11. Verify node is recovered and COMMAND response sent.

**Pass criteria:** Full replacement bootstrap succeeds; recovery blocked until key installed.

---

#### T-2013  Connector key-management security

**Covers:** GW-2013
**Method:** Unit test + integration test

**Steps:**
1. Send `MASTER_KEY_INSTALL` encrypted with a different gateway's public key
   — verify decryption failure.
2. Send `MASTER_KEY_INSTALL` with replayed `operation_id` — verify rejection.
3. Send `KEY_ESCROW_RESPONSE` with a tampered escrow blob — verify blob
   discarded, other valid candidates still processed.
4. Send unsolicited `KEY_ESCROW_RESPONSE` with no matching request — verify
   discarded.
5. Verify recovery private key is re-encrypted after rotation (restart
   gateway with new master key, verify keypair loads successfully).

**Pass criteria:** All security checks enforced; private key survives rotation.

---

## 8  Validation Changes — Azure Handler

#### T-AZH-0600  Escrow blob storage from ACTUAL_STATE

**Covers:** AZH-0600
**Method:** Integration test

**Steps:**
1. Send ACTUAL_STATE with `encrypted_psk_escrow` for a node.
2. Verify blob stored in ActualNodeState table.
3. Send ACTUAL_STATE for a phone — verify stored in ActualPhoneState table.
4. Send updated blob (new key_version) — verify previous blob overwritten.

**Pass criteria:** Blobs stored and updated correctly.

---

#### T-AZH-0601  Recovery serving by key_hint

**Covers:** AZH-0601
**Method:** Integration test

**Steps:**
1. Store escrow blobs for 3 nodes (2 with same key_hint, 1 different).
2. Send `KEY_ESCROW_REQUEST` for the colliding key_hint.
3. Verify response contains exactly 2 candidate blobs.
4. Send request for the unique key_hint — verify 1 candidate.
5. Send request for unknown key_hint — verify empty candidate list.

**Pass criteria:** Correct candidate sets returned.

---

#### T-AZH-0602  Gateway public key storage

**Covers:** AZH-0602
**Method:** Integration test

**Steps:**
1. Send `KEY_ESCROW_PUBKEY` with epoch 1.
2. Verify stored in GatewayEscrow table.
3. Send `KEY_ESCROW_PUBKEY` with epoch 2 — verify updated.
4. Send `KEY_ESCROW_PUBKEY` with epoch 1 (stale) — verify NOT updated.

**Pass criteria:** Monotonic epoch guard enforced.

---

#### T-AZH-0603  Salt first-writer-wins

**Covers:** AZH-0603
**Method:** Integration test

**Steps:**
1. Send ACTUAL_STATE with salt — verify stored.
2. Send ACTUAL_STATE with different salt — verify NOT overwritten.
3. Verify original salt returned to gateway.

**Pass criteria:** First salt preserved; subsequent writes ignored.

---

#### T-AZH-0604  MASTER_KEY_INSTALL relay

**Covers:** AZH-0604
**Method:** Integration test

**Steps:**
1. Place `MASTER_KEY_INSTALL` message in downstream queue.
2. Verify message relayed to gateway verbatim.
3. Verify handler did not decrypt or modify the payload.

**Pass criteria:** Opaque relay without inspection.

---

#### T-AZH-0605  Escrow state observability

**Covers:** AZH-0605
**Method:** Integration test

**Steps:**
1. Store gateway-scoped ACTUAL_STATE with escrow_state = "ready".
2. Query escrow state — verify "ready" returned.
3. Update to "rotation_in_progress" — verify updated.

**Pass criteria:** Escrow state queryable.

---

## 9  Validation Changes — Admin CLI

#### T-0900  Key rotation — happy path

**Covers:** ADMIN-0900
**Method:** Integration test (mock Azure + mock gateway)

**Steps:**
1. Run `sonde-admin key rotate` with mock Azure returning salt + public key.
2. Verify fingerprint displayed.
3. Confirm fingerprint, enter valid passphrase.
4. Verify `MASTER_KEY_INSTALL` message sent to downstream queue.
5. Verify passphrase zeroed from memory after use.

**Pass criteria:** Rotation message sent correctly.

---

#### T-0900a  Key rotation — fingerprint rejection

**Covers:** ADMIN-0900
**Method:** Unit test

**Steps:**
1. Run key rotate, deny fingerprint confirmation.
2. Verify no `MASTER_KEY_INSTALL` message sent.
3. Verify command exits with error.

**Pass criteria:** Aborted rotation sends nothing.

---

#### T-0900b  Key rotation — weak passphrase rejection

**Covers:** ADMIN-0900
**Method:** Unit test

**Steps:**
1. Run key rotate with passphrase "abc" (too short).
2. Verify rejection with entropy warning.

**Pass criteria:** Weak passphrases rejected.

---

#### T-0901  Fingerprint display

**Covers:** ADMIN-0901
**Method:** Unit test

**Steps:**
1. Run `sonde-admin key fingerprint` with known public key.
2. Verify 6-word fingerprint displayed.
3. Verify matches gateway-side computation for same key.

**Pass criteria:** Consistent fingerprint across admin and gateway.

---

#### T-0902  Escrow status display

**Covers:** ADMIN-0902
**Method:** Integration test

**Steps:**
1. Run `sonde-admin key status` with mock Azure returning escrow state.
2. Verify state, key version, and PSK counts displayed.

**Pass criteria:** Status information displayed correctly.

---

## 10  Invariant Check

| Existing invariant | Preserved? | Notes |
|--------------------|------------|-------|
| PSKs never in plaintext outside gateway memory | ✅ | Escrow blobs are AES-256-GCM encrypted |
| Connector messages are CBOR with integer keys | ✅ | New message types follow same convention |
| Unknown keys ignored by receivers | ✅ | New ACTUAL_STATE key 12 ignored by old receivers |
| DESIRED_STATE is complete replacement | ✅ | New escrow messages use separate msg_types |
| Storage trait is async | ✅ | New methods follow same pattern |
| Master key via KeyProvider | ✅ | Rotation updates the key provider store; trait unchanged |
| Modem is passive display sink | ✅ | Fingerprint rendered by gateway, not modem |
| Silent discard for unknown nodes (GW-1002) | ✅ | Recovery is additive; failed recovery still discards silently |

---

## 11  Completeness Check

| Requirement | Design section | Validation test(s) |
|-------------|---------------|-------------------|
| GW-2000 | §20.2 | T-2000 |
| GW-2001 | §20.2, §2.3 | T-2001 |
| GW-2002 | §20.3 | T-2002 |
| GW-2003 | §20.4, §2.3 | T-2003 |
| GW-2004 | §20.5 | T-2004 |
| GW-2005 | §20.6 | T-2005 |
| GW-2006 | §20.7 | T-2006, T-2006a |
| GW-2007 | §20.7 | T-2007 |
| GW-2008 | §20.8 | T-2008 |
| GW-2009 | §20.9 | T-2009 |
| GW-2010 | §20.9 | T-2010, T-2010a, T-2010b |
| GW-2011 | §20.10 | T-2011 |
| GW-2012 | §20.11 | T-2012 |
| GW-2013 | §20.12 | T-2013 |
| AZH-0600 | §8.1, §8.2 | T-AZH-0600 |
| AZH-0601 | §8.2, §8.4 | T-AZH-0601 |
| AZH-0602 | §8.2 | T-AZH-0602 |
| AZH-0603 | §8.3 | T-AZH-0603 |
| AZH-0604 | §8.2 | T-AZH-0604 |
| AZH-0605 | §8.2 | T-AZH-0605 |
| ADMIN-0900 | §11.1 | T-0900, T-0900a, T-0900b |
| ADMIN-0901 | §11.2 | T-0901 |
| ADMIN-0902 | §11.3 | T-0902 |

All requirements have design sections and validation tests. ✅

---

## 12  Conflict Detection

No contradictions found between:
- New escrow message types and existing DESIRED_STATE/ACTUAL_STATE semantics.
- New Storage trait methods and existing trait contract.
- New ACTUAL_STATE fields and existing field numbering (key 12 is unused).
- Escrow lifecycle states and existing gateway startup/shutdown sequences.
- Fingerprint display page and existing modem display page navigation.
