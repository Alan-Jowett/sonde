<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Security Model

> **Document status:** Draft  
> **Scope:** The complete security model for the Sonde platform: trust assumptions, key provisioning, authentication, replay protection, and failure modes.  
> **Audience:** Implementers, auditors, and operators of the Sonde platform.  
> **Related:** [protocol.md](protocol.md) (wire protocol), [gateway-requirements.md](gateway-requirements.md) (gateway requirements), [node-requirements.md](node-requirements.md) (node requirements)

---

## 1  Threat and trust model

### 1.1  Trust boundaries

| Entity | Trust level | Notes |
|---|---|---|
| **Gateway** | Trusted | The gateway is the root of trust for the entire deployment. |
| **Nodes** | Trusted (for their own data) | Each node is authenticated via its unique pre-shared key. |
| **Radio transport (ESP-NOW)** | Untrusted | All traffic on the air interface is considered adversarial. |
| **Other nodes** | Untrusted | Nodes do not trust or authenticate messages from other nodes. |
| **Phone (pairing agent)** | Delegated trust | Authorized by the gateway via a phone PSK.  Can provision nodes on the gateway's behalf.  Trust is revocable. |
| **BLE transport** | Untrusted | BLE LESC provides transport encryption, but the protocol does not rely on it for security guarantees (see [ble-pairing-protocol.md §9.2](ble-pairing-protocol.md#92--ble-link-security)). |

### 1.2  Assumptions

- The gateway is operated in a controlled environment and its key store is protected.
- A node's identity is bound to its embedded key; physical access to a node is equivalent to key compromise.
- The gateway has no persistent outbound connection to nodes — communication is always node-initiated.
- A phone's pairing authority derives from its phone PSK, issued by the gateway.  The phone is trusted to generate node PSKs and submit registration requests, but the gateway independently validates every request.
- Phone PSK compromise is equivalent to unauthorized pairing authority.  The gateway operator can revoke a phone PSK to immediately terminate its authority.

### 1.3  Out-of-scope threats

- **Application-layer confidentiality** — the radio protocol provides payload encryption (AES-256-GCM), but the gateway decrypts and forwards plaintext to handler processes. End-to-end encryption between the BPF program and the handler is out of scope.
- **Gateway compromise** — if the gateway is compromised, all node keys and program assignments are exposed.
- **Radio jamming / denial-of-service** — physical-layer interference is not addressed by this protocol.

---

## 2  Key provisioning and storage

### 2.1  Per-node symmetric keys

Each node is provisioned with a unique 256-bit pre-shared key (PSK) at manufacturing or deployment time. The key is used for AES-256-GCM authenticated encryption of all frames exchanged between the node and the gateway.

- Keys are **symmetric** — the same key is used by both the node and the gateway.
- Keys are **unique per node** — no two nodes share a key.
- Keys are **rotatable** via factory reset and re-pairing (see §2.6).

### 2.2  Key storage on the node

On the reference hardware (ESP32-C3/S3) the node's PSK is stored in a **dedicated flash partition**. This means:

- The key is **software-accessible** — firmware can read the raw key bytes to perform AES-256-GCM operations.
- A **factory reset** erases the key partition, returning the node to an unpaired state (see §2.6).
- Flash storage does not provide the same hardware isolation as eFuse-based approaches; a compromised firmware image can read the key. Mitigations include secure boot and flash encryption where available.

### 2.3  Key storage on the gateway

The gateway stores the per-node key database persistently. The key database maps `key_hint` values to one or more 256-bit keys (see [protocol.md §3.1.1](protocol.md#311--key_hint-semantics) for `key_hint` semantics). Protecting this database is an operational requirement:

- The key store SHOULD be encrypted at rest.
- Exporting the key store SHOULD require explicit operator authorization (see [gateway-requirements.md GW-1001](gateway-requirements.md)).

#### 2.3.1  Master key providers (GW-0601b)

The gateway abstracts master-key loading behind a `KeyProvider` trait.  The
backend is selected with `--key-provider` at startup.  Four backends are
available:

| Backend | `--key-provider` value | Platform | Description |
|---------|------------------------|----------|-------------|
| `FileKeyProvider` | `file` *(default)* | All | Read 64 hex characters from `--master-key-file`. |
| `EnvKeyProvider` | `env` | All | Read 64 hex characters from the `SONDE_MASTER_KEY` environment variable. |
| `DpapiKeyProvider` | `dpapi` | Windows only | Decrypt a DPAPI-protected blob at `--master-key-file`. The blob is tied to the Windows user or machine account — it cannot be decrypted on another machine or by a different user. |
| `SecretServiceKeyProvider` | `secret-service` | Linux only | Retrieve the key from the D-Bus Secret Service (GNOME Keyring, KWallet, or any compatible implementation), identified by `--key-label`. |

**Deployment guidance:**

*File backend (all platforms — default):*
```
# Generate a 32-byte key
openssl rand -hex 32 > /etc/sonde/master.hex
chmod 600 /etc/sonde/master.hex
sonde-gateway --key-provider file --master-key-file /etc/sonde/master.hex ...
```
Protect the key file with file-system permissions and, if available, encrypt
the containing volume.

*DPAPI backend (Windows):*

The DPAPI backend reads a binary blob file produced by `protect_with_dpapi()`
(in the `sonde_gateway::key_provider` library).  The blob can only be
decrypted by the **same Windows user or machine account** that created it —
running as a different user, or copying the blob to another machine, causes a
decryption failure at startup.  For service deployments, use a dedicated
service account and create the blob as that account.

```powershell
# Provision once (run as the gateway service account):
# 1. Place a hex key at master.hex (protect this file before this step)
# 2. Use a small Rust snippet or a provisioning utility built from
#    sonde_gateway::key_provider::protect_with_dpapi() to create the blob:
#
#    protect_with_dpapi(&key_bytes, Path::new(r"C:\ProgramData\sonde\master.dpapi"))

# Run the gateway:
sonde-gateway --key-provider dpapi --master-key-file C:\ProgramData\sonde\master.dpapi ...
```

*Secret Service backend (Linux):*

```bash
# Provision once (stores the key in the OS keyring):
# Use store_in_secret_service() from sonde_gateway::key_provider,
# or the following Python snippet as a reference:
#
# import secretstorage, secrets
# conn = secretstorage.dbus_init()
# coll = secretstorage.get_default_collection(conn)
# coll.unlock()
# key = bytes.fromhex(open('/etc/sonde/master.hex').read().strip())
# coll.create_item('sonde-gateway-master-key',
#   {'service': 'sonde-gateway', 'account': 'sonde-gateway-master-key'},
#   key, replace=True)

# Run the gateway:
sonde-gateway --key-provider secret-service --key-label sonde-gateway-master-key ...
```
For headless servers without an interactive session, run
`gnome-keyring-daemon --daemonize --unlock` at service start, or use
`systemd-creds` as an alternative.

### 2.4  Key provisioning

#### 2.4.1  BLE pairing (field deployment)

A mobile phone app acts as a delegated pairing agent:

1. The phone generates a unique 256-bit PSK for the node.
2. The phone builds a pairing request containing the node PSK and AEAD-encrypts it with its phone PSK (§2.7).
3. The phone provisions the node over BLE: sends the node PSK, RF channel, and the encrypted pairing request.
4. The node stores the PSK and relays the encrypted pairing request to the gateway over ESP-NOW.
5. The gateway decrypts the pairing request with the phone PSK, extracts the node PSK, decrypts the ESP-NOW frame, and registers the node.

The node PSK is transmitted in plaintext over the BLE link (protected by BLE LESC transport encryption).  `NODE_PROVISION` sends the `node_psk`, `phone_psk`, `phone_key_hint`, and the `encrypted_payload` over BLE, so a BLE MITM attacker who defeats Just Works pairing captures all the material needed to craft a valid `PEER_REQUEST` (the outer frame requires `phone_psk` for AES-256-GCM encryption, and the `encrypted_payload` is pre-built by the phone).  The primary mitigation is using a MITM-resistant BLE pairing method (Passkey Entry or Numeric Comparison), which prevents interception entirely.  Secondary mitigations include the one-time-use nature of the encrypted payload (the gateway rejects duplicate `node_id` registrations with a different PSK) and the PairingRequest timestamp tolerance (±86400 s).  Note: the 120 s registration window applies to Phase 1 phone registration (`REGISTER_PHONE`), not to Phase 2 node registration.  Just Works is acceptable only for low-threat environments where physical proximity provides adequate assurance.

See [ble-pairing-protocol.md](ble-pairing-protocol.md) for the full BLE wire protocol.

Re-pairing is possible: a factory-reset node (see §2.6) can be paired again via BLE, receiving a new key and a new identity.

### 2.5  Key compromise

#### 2.5.1  Node PSK compromise

If a node's key is compromised (e.g., through firmware exploit or physical flash extraction):

- The compromise is **limited to that node** — other nodes are unaffected.
- The gateway SHOULD remove the compromised node's key from the registry immediately.
- The node can be **factory-reset** (see §2.6) to erase the compromised key and all persistent state.
- After factory reset, the node is re-paired with a fresh key, effectively giving it a new identity.

#### 2.5.2  Phone PSK compromise

If a phone's PSK is compromised (e.g., stolen device, malware):

- The attacker gains the ability to **forge pairing requests** — they can register rogue nodes on the gateway.
- The compromise does **not** affect existing nodes — each node has its own independent PSK.
- The gateway operator MUST **revoke** the compromised phone PSK immediately.  After revocation, the gateway rejects all future pairing requests authenticated with that PSK.
- Nodes registered by the compromised phone before revocation remain valid unless individually removed by the operator.

### 2.6  Factory reset

A factory reset returns a node to its initial unpaired state. The reset erases:

- The **pre-shared key** — the node loses its identity.
- All **persistent map data** — application state is wiped.
- The **resident BPF program** — the node has no program to execute.

After a factory reset, the node is inert: it cannot authenticate with any gateway and will not execute application logic. To return it to service, the operator must re-pair it via BLE (§2.4.1), which provisions a new key and registers the node in the gateway's key database.

On BLE-equipped nodes, holding the pairing button during boot also triggers a factory reset before accepting a new provision (see [ble-pairing-protocol.md §8.2.1](ble-pairing-protocol.md#821--factory-reset-via-ble)).

Factory reset is the standard remediation path for key compromise, decommissioning, and transferring a node to a different deployment.

### 2.7  Phone PSK provisioning and trust delegation

The BLE pairing protocol ([ble-pairing-protocol.md](ble-pairing-protocol.md)) introduces delegated pairing authority via phone PSKs.

#### 2.7.1  Phone PSK lifecycle

1. **Issuance.** The phone generates a unique 256-bit phone PSK using its OS CSPRNG and sends it to the gateway in the `REGISTER_PHONE` BLE message during phone-to-gateway pairing.  The PSK is transmitted over a BLE LESC (Numeric Comparison) link, which provides transport-layer confidentiality and mutual authentication (see [ble-pairing-protocol.md §5.4](ble-pairing-protocol.md#54--register_phone-0x02)).
2. **Storage.** The gateway stores the phone PSK alongside a human-readable label, issuance timestamp, and active/revoked status.  The phone stores the phone PSK in the app's secure storage.
3. **Usage.** The phone uses its PSK for AES-256-GCM encryption of pairing request payloads and for frame-level AEAD on PEER_REQUEST frames.  The gateway authenticates pairing requests by successfully decrypting them.
4. **Revocation.** The gateway operator can revoke a phone PSK at any time.  Revoked PSKs are retained in the database (for audit) but all future pairing requests signed with them are rejected.

#### 2.7.2  Trust properties

| Property | Guarantee |
|----------|-----------|
| **Authorization** | Only phones holding a valid (non-revoked) PSK can register nodes. |
| **Isolation** | Each phone has a unique PSK.  One phone cannot forge requests as another phone. |
| **Auditability** | The gateway records which phone PSK was used to register each node (stored as a `registered_by` association in the node database — schema to be defined in a future gateway design PR). |
| **Revocability** | Revoking a phone PSK immediately disables that phone's pairing authority. |
| **No gateway key exposure** | The phone PSK is a symmetric pairing credential — it does not grant access to the node key database or any other gateway state. |

#### 2.7.3  Gateway Ed25519 keypair — RETIRED

> **RETIRED (issue #495).** The gateway no longer holds an Ed25519 keypair. Gateway identity, challenge–response signing (REQUEST_GW_INFO / GW_INFO_RESPONSE), and ECDH key agreement are eliminated. Phone registration uses phone-generated PSKs sent over BLE LESC. Pairing request payloads are encrypted with `phone_psk` (AES-256-GCM) instead of ECDH. Authority derives from possession of the node key database — there is no persistent cryptographic identity.

---

## 3  Authentication and integrity

### 3.1  Frame structure

Every frame exchanged between a node and the gateway has the following layout:

A frame is three contiguous regions: the fixed 11-byte binary Header (`key_hint` 2 bytes, `msg_type` 1 byte, `nonce` 8 bytes); the variable-length AES-256-GCM ciphertext (encrypted CBOR-encoded Payload); and a trailing 16-byte GCM authentication tag. The header is passed as Additional Authenticated Data (AAD) — it is authenticated but not encrypted.

```
┌──────────────────────────────────────────────────────────────────┐
│  Header (AAD)          │  Ciphertext              │  GCM tag     │
│  (key_hint, msg_type,  │  (AES-256-GCM encrypted  │  (16 bytes)  │
│   nonce)               │   CBOR payload)          │              │
└──────────────────────────────────────────────────────────────────┘
```

**GCM nonce construction (12 bytes):**

```
gcm_nonce = SHA-256(psk)[0..3] ‖ msg_type ‖ frame_nonce
```

Including the `msg_type` byte in the nonce ensures that request/response pairs sharing the same `frame_nonce` (e.g., WAKE 0x01 / COMMAND 0x81) produce distinct GCM nonces, preventing nonce reuse across directions. The 3-byte PSK-derived prefix makes cross-key nonce collisions extremely unlikely, but does not provide absolute cross-key uniqueness.

The AEAD construction covers the full header (AAD) and encrypts + authenticates the payload:

```
ciphertext, tag = AES-256-GCM-Seal(key = psk, nonce = gcm_nonce, aad = header, plaintext = payload)
```

The frame on the wire is: `header ‖ ciphertext ‖ tag[16]`.

See [protocol.md §3](protocol.md#3--frame-format) for the detailed wire specification.

### 3.2  What is authenticated

- **Direction** — the `msg_type` high-bit distinguishes node→gateway from gateway→node frames, and this field is covered by the AEAD (as part of the AAD).
- **Nonce** — the nonce is part of the fixed binary header and is covered by the AEAD, preventing an attacker from substituting a different nonce to defeat replay protection.
- **Payload** — all CBOR payload bytes are encrypted and authenticated by AES-256-GCM; payload modification or observation is prevented.

### 3.3  Gateway verification (inbound from node)

1. Extract `key_hint` from the fixed header.
2. Look up candidate `node_psk`(s) by `key_hint`. If no candidates → silently discard.
3. For each candidate `node_psk`, attempt AES-256-GCM-Open over header + ciphertext; if decryption succeeds, accept and bind the frame to that node. If none succeed → silently discard.
4. For `WAKE` messages: accept; create an active session for this node (replacing any previous session). For post-WAKE messages: verify the node has an active session and the sequence number matches the expected next value. If no active session or wrong sequence → silently discard.
5. Advance the session's expected sequence number.
6. Decode CBOR payload. If malformed → log, discard.
7. Process message.

See [protocol.md §7.2](protocol.md#72--verification-procedure-gateway-inbound) for the normative procedure.

### 3.4  Node verification (inbound from gateway)

1. Attempt AES-256-GCM-Open over header + ciphertext using the node's own key. If decryption fails → discard.
2. Verify that the echoed value in the response header (nonce for WAKE, sequence number for post-WAKE messages) matches the value sent in the corresponding request header. If mismatch → discard.
3. Decode CBOR payload and process.

See [protocol.md §7.3](protocol.md#73--verification-procedure-node-inbound) for the normative procedure.

### 3.5  Encryption and confidentiality

AES-256-GCM provides both **integrity** (tamper detection) and **confidentiality** (payload secrecy). Traffic on the air cannot be observed or modified by an attacker without the PSK. The header (key_hint, msg_type, nonce) is visible but authenticated; the CBOR payload is encrypted.

---

## 4  Replay protection

### 4.1  Overview

Replay protection uses **session-scoped sequence numbers** tied to the WAKE nonce:

- **WAKE messages** include a random 64-bit nonce generated by the node. The nonce identifies the session.
- **The gateway responds** with a randomly chosen starting sequence number in its COMMAND response (authenticated by AEAD).
- **All subsequent messages in the wake cycle** use incrementing sequence numbers starting from that value. The gateway rejects any message whose sequence number does not match the expected next value for the active session.

No persistent replay-protection state is required on either the node or the gateway. The gateway tracks only active sessions in memory.

### 4.2  WAKE nonce (session identifier)

Each `WAKE` message includes a 64-bit **nonce** generated by the hardware RNG. The nonce is included in the fixed binary header and is covered by the AEAD. It serves as the **session identifier** — the gateway uses it to associate subsequent messages with this wake cycle.

| Property | Value |
|---|---|
| Nonce size | 64 bits |
| Generation | Hardware RNG, generated fresh for each WAKE message |
| Purpose | Identifies the session; binds the gateway's COMMAND response to this wake event |
| Persistence | Not stored across deep sleep |

### 4.3  Gateway-assigned sequence numbers (node → gateway)

On receiving a valid `WAKE`, the gateway creates an **active session** for this node (replacing any previous session). It assigns a **random starting sequence number** and includes it in the `COMMAND` response. The node uses this value for its next outbound message and increments it for each subsequent message in the wake cycle.

| Parameter | Value |
|---|---|
| Counter size | 64 bits |
| Assignment | Gateway picks a random starting sequence number and includes it in the COMMAND response |
| Node behavior | Use the assigned starting sequence for the first post-WAKE message, increment for each subsequent message |
| Gateway acceptance rule | Track expected next sequence per active session; reject any message that does not match |
| Persistence (gateway) | None — active sessions are tracked in memory only; discarded when the session ends or times out |
| Persistence (node) | None — the gateway tells the node where to start each wake cycle |

### 4.4  Session lifecycle

1. **Session created** — gateway receives a valid WAKE, creates (or replaces) an active session for this node, assigns a random starting sequence number.
2. **Session active** — gateway accepts post-WAKE messages from this node only if a session is active and the message carries the expected next sequence number.
3. **Session ended** — the node sleeps (no further messages arrive). The gateway discards the session after a timeout.

### 4.5  Node verification of gateway responses (gateway → node)

The gateway echoes the node's nonce (for WAKE) or sequence number (for subsequent messages) in the response header. The node verifies that the echoed value matches the value it sent. This prevents:

- A replayed old gateway response from being accepted.
- A response intended for one message from being applied to a different request.

### 4.6  Behavior on stale or replayed messages

| Scenario | Gateway behavior |
|---|---|
| WAKE with valid GCM tag | Accept; create active session with new random starting sequence |
| Post-WAKE message matching an active session with expected seq | Accept; advance expected seq |
| Post-WAKE message with wrong seq or no matching active session | Silently discard. Log internally. |

| Scenario | Node behavior |
|---|---|
| Echoed value does not match sent value | Discard response. |
| Echoed value matches | Accept response. |

### 4.7  Why replaying captured traffic fails

An attacker who captures an entire wake session (WAKE + all post-WAKE messages) cannot replay it:

1. **Replayed WAKE** — the gateway creates a new active session with a *different* random starting sequence number. The attacker can observe the header of the COMMAND response, but cannot decrypt it or generate valid follow-up messages without the PSK.
2. **Replayed post-WAKE messages** — these carry the *original* session's sequence numbers, which do not match the new session's expected sequence. The gateway rejects them.
3. **Replayed post-WAKE without replaying WAKE** — there is no active session with the original nonce. The gateway rejects them.

The sequence number is not a secret — it is an anti-replay counter. The AEAD proves authenticity; the sequence number ensures each authenticated message is accepted exactly once within its session.

### 4.8  WAKE replay risk analysis

WAKE messages themselves can be replayed. This is low risk because:

- **The attacker cannot forge follow-up messages.** The gateway's COMMAND reply assigns a new starting sequence, but the attacker cannot use it without the PSK to produce valid AES-256-GCM ciphertexts.
- **No state corruption.** The gateway creates a new active session that will time out with no further messages. The real node's next wake creates its own independent session.
- **Operational noise only.** The gateway may log a false wake event or attempt a program update to a node that is not listening. Both are harmless and self-correcting (the gateway times out the session).

### 4.9  Deep sleep

Neither the node nor the gateway stores replay-protection state across deep sleep:

1. The node generates a fresh random nonce for its WAKE message.
2. The gateway creates a new active session and assigns a random starting sequence number.
3. The node uses that sequence number for all subsequent messages in the cycle.
4. The node sleeps — no state to persist. The gateway discards the session.

---

## 5  Identity binding

### 5.1  Key is identity

A node's identity is its pre-shared key, not its `key_hint` or any network address. A message is accepted as originating from a specific node if and only if AEAD decryption succeeds with that node's PSK.

- `key_hint` is a **lookup optimization** to avoid trying every key in the database. It is not an authenticator.
- Two nodes with colliding `key_hint` values are disambiguated by AEAD decryption: the gateway tries all candidate keys for the hint and accepts the first successful decryption.

### 5.2  Node ID binding

A node's logical ID (used in the node registry) is permanently bound to its PSK at provisioning time. The gateway rejects any message that does not match a registered PSK. There is no mechanism for a node to claim a different identity.

### 5.3  Program integrity binding

BPF programs are identified by their **content hash** (SHA-256 of the program bytes). The node reports its resident program hash in every `WAKE` message. The gateway:

1. Compares the reported hash against the intended program for that node.
2. Issues `UPDATE_PROGRAM` if the hash does not match.
3. Verifies the newly transferred program against the expected hash before accepting it (via `PROGRAM_ACK`).

This ensures that only gateway-approved programs run on nodes and prevents a corrupted or tampered program from persisting undetected.

---

## 6  Failure modes

The protocol handles all authentication and integrity failures by **silent discard**. No error response is sent to the sender. This prevents information leakage that could assist an attacker.

| Condition | Gateway behavior | Node behavior |
|---|---|---|
| No key matches `key_hint` | Silently discard. Log internally. | N/A |
| Invalid GCM tag | Silently discard. Log internally. | Discard frame. |
| Stale/replayed sequence number | Silently discard. Log internally. | N/A |
| Nonce mismatch in response | N/A | Discard frame. Retry with backoff. |
| Malformed CBOR (post-auth) | Silently discard. Log internally. | Discard frame. |
| Program hash mismatch | Issue `UPDATE_PROGRAM`. | N/A |
| Program transfer hash mismatch | Reject `PROGRAM_ACK`. Log. | Retry transfer. |
| Key compromise | Remove node from registry. Factory-reset and re-pair node. | N/A |

See [protocol.md §8](protocol.md#8--error-handling) for the full error-handling table.

---

## 7  Gateway failover

### 7.1  Gateway identity

The gateway has no persistent cryptographic identity and no keypair. Its authority over nodes derives entirely from possession of the node key database. Any gateway instance loaded with the same key database and program assignments can serve nodes transparently.

### 7.2  Key material sharing

For failover to work correctly, the replacement gateway must have:

1. The complete node key database (all `key_hint` → PSK mappings).
2. The current program assignments and schedules for all nodes.
3. The full BPF program library (so it can serve `GET_CHUNK` requests).

Key material MUST be transferred through a physically secured channel (e.g., encrypted export/import via USB media — see [GW-1001](gateway-requirements.md)). Transmitting keys over the air or through unprotected storage is prohibited.

### 7.3  Replay-protection state

Replay-protection state does **not** need to be synchronized between gateway instances. Active sessions are in-memory only and naturally short-lived. When a replacement gateway comes online:

- Its active session table is empty.
- Nodes that were mid-session will time out and re-WAKE on their next cycle, creating new sessions on the replacement gateway.
- Previously captured traffic cannot be replayed because replayed WAKE messages trigger new sessions with different starting sequence numbers, and old post-WAKE messages carry the wrong sequence values.

### 7.4  Program hash consistency

All gateway instances in a failover group MUST serve identical programs for any given program hash. A node that receives a program with a hash that does not match the expected value will reject it and retry, cycling until it receives the correct bytes.

---

## 8  Summary of security properties

| Property | Mechanism | Limitation |
|---|---|---|
| Message integrity | AES-256-GCM per frame (auth + encryption) | Header visible to observers; payload encrypted |
| Node authentication | Per-node PSK + AEAD | Key compromise requires factory reset + re-pair |
| Replay protection | Session-scoped sequence numbers (nonce + random starting seq) | WAKE messages are replayable (low risk — see §4.8); no persistent state required |
| Program integrity | Content hash (`PROGRAM_ACK`) | Gateway key store must be protected |
| Key storage | Dedicated flash partition | Software-accessible; mitigate with secure boot / flash encryption |
| Key provisioning | BLE pairing via phone | Requires authorized phone |
| Delegated pairing | Phone PSK + AEAD-based authorization of pairing requests | Phone PSK compromise allows rogue registration until revoked |
| Pairing payload confidentiality | `phone_psk`-based AES-256-GCM | Phone PSK compromise exposes pairing payloads |
| Identity binding | PSK = node identity | Factory reset + re-pair to revoke / replace identity |

---

## 9  Design tradeoffs

The security model makes deliberate tradeoffs between security and usability. This section documents the alternatives considered and the rationale for each choice.

### 9.1  Key storage: eFuse vs flash

| | eFuse (hardware fuse) | Flash partition |
|---|---|---|
| **Key accessibility** | Hardware-inaccessible after provisioning; only the AES peripheral can use the key | Software-accessible; firmware can read the raw key bytes |
| **Resistance to firmware compromise** | Compromised firmware cannot extract the key | Compromised firmware can read the key |
| **Factory reset** | Not possible — key is permanently fused | Supported — key partition can be erased |
| **Key rotation** | Not possible — node must be physically replaced on compromise | Supported — factory reset + re-pair provisions a new key |

**Chosen: flash.** The ability to factory-reset and re-pair nodes is more valuable in practice than hardware key isolation. Flash-based keys can be mitigated with secure boot (prevents unauthorized firmware from running) and flash encryption (prevents physical flash readout). eFuse storage remains a valid hardening option for deployments that do not require field re-provisioning.

### 9.2  Key provisioning: factory-set vs on-site pairing

| | Factory-set (at manufacturing) | On-site pairing |
|---|---|---|
| **Provisioning environment** | Controlled manufacturing facility | Field / deployment site |
| **Operator skill required** | Provisioning is part of the manufacturing pipeline; end user does not handle keys | Operator must have physical access and a provisioning tool |
| **Flexibility** | Key is fixed at manufacturing; cannot adapt to deployment-specific gateways | Node can be paired to any gateway at deployment time |
| **Factory reset** | Re-provisioning requires return to factory or a secure key-injection station | Re-provisioning is performed on-site with the same pairing tool |

**Chosen: on-site pairing.** Factory-set keys are the most secure option (provisioning happens in a controlled environment, keys never traverse a field channel), but on-site pairing enables simpler logistics: nodes ship as blank devices and are paired to the target gateway during installation. This is essential for deployments where the operator and the manufacturer are different parties, or where nodes may be redeployed across gateways.

### 9.3  Pairing channel: BLE vs over-the-air

| | BLE-mediated pairing (via phone) | Over-the-air (ESP-NOW) pairing |
|---|---|---|
| **Channel security** | BLE LESC encrypted transport; MITM possible with Just Works | Broadcast radio; vulnerable to eavesdropping and MITM |
| **MITM resistance** | BLE MITM can intercept node PSK, but cannot forge the `phone_psk`-encrypted pairing payload | Requires a key-agreement protocol with out-of-band verification |
| **Convenience** | Phone app over BLE — no cable required | Fully wireless — no physical contact |
| **Complexity** | Moderate: phone PSK trust delegation, AES-256-GCM encryption | Complex: secure key-agreement over untrusted radio |
| **Field usability** | Good — phone app, no special hardware | Good — but security concerns outweigh convenience |

**Chosen: BLE.**

BLE-mediated pairing via a phone app provides the best tradeoff between security and field usability.  The phone acts as a delegated agent whose authority is revocable (phone PSK revocation).  However, `NODE_PROVISION` transmits both the `node_psk` and the `encrypted_payload` over the BLE link, so a BLE MITM attacker who defeats Just Works pairing captures sufficient material to craft a valid `PEER_REQUEST` and race the legitimate node.  This constitutes a node PSK compromise.  The `node_id` uniqueness check and timestamp tolerance (±86400 s) limit replay but do not prevent a race.  The primary mitigation is using a MITM-resistant BLE pairing method (Passkey Entry or Numeric Comparison).  Just Works is acceptable for low-threat environments where physical proximity provides adequate assurance.

> **Note:** USB-mediated pairing was originally considered for bench testing and development. It was removed because the ESP32 ROM retains UART resources in a way that prevents reliable serial communication during pairing — a hardware limitation that cannot be worked around in firmware. USB/UART remains available for firmware flashing and debug console output.

Direct ESP-NOW pairing (without BLE intermediary) was considered and rejected — it would require a secure key-agreement protocol over the untrusted radio, adding complexity and a new attack surface.  The BLE intermediary isolates the key exchange from the operational radio channel.
