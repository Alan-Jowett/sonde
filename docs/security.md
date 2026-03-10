# Security Model

> **Document status:** Draft  
> **Scope:** The complete security model for the Sonde platform: trust assumptions, key provisioning, authentication, replay protection, and failure modes.  
> **Audience:** Implementers, auditors, and operators of the Sonde platform.  
> **Related:** [protocol.md](protocol.md) (wire protocol), [gateway-requirements.md](gateway-requirements.md) (gateway requirements)

---

## 1  Threat and trust model

### 1.1  Trust boundaries

| Entity | Trust level | Notes |
|---|---|---|
| **Gateway** | Trusted | The gateway is the root of trust for the entire deployment. |
| **Nodes** | Trusted (for their own data) | Each node is authenticated via its unique pre-shared key. |
| **Radio transport (ESP-NOW)** | Untrusted | All traffic on the air interface is considered adversarial. |
| **Other nodes** | Untrusted | Nodes do not trust or authenticate messages from other nodes. |

### 1.2  Assumptions

- The gateway is operated in a controlled environment and its key store is protected.
- A node's identity is bound to its embedded key; physical access to a node is equivalent to key compromise.
- The gateway has no persistent outbound connection to nodes — communication is always node-initiated.

### 1.3  Out-of-scope threats

- **Confidentiality** — data is authenticated but not encrypted. An attacker can observe message contents on the air. Confidentiality requirements must be addressed at the application layer.
- **Gateway compromise** — if the gateway is compromised, all node keys and program assignments are exposed.
- **Radio jamming / denial-of-service** — physical-layer interference is not addressed by this protocol.

---

## 2  Key provisioning and storage

### 2.1  Per-node symmetric keys

Each node is provisioned with a unique 256-bit pre-shared key (PSK) at manufacturing or deployment time. The key is used for HMAC-SHA256 authentication of all frames exchanged between the node and the gateway.

- Keys are **symmetric** — the same key is used by both the node and the gateway.
- Keys are **unique per node** — no two nodes share a key.
- Keys are **rotatable** via factory reset and re-pairing (see §2.6).

### 2.2  Key storage on the node

On the reference hardware (ESP32-C3/S3) the node's PSK is stored in a **dedicated flash partition**. This means:

- The key is **software-accessible** — firmware can read the raw key bytes to compute HMACs.
- A **factory reset** erases the key partition, returning the node to an un-paired state (see §2.6).
- Flash storage does not provide the same hardware isolation as eFuse-based approaches; a compromised firmware image can read the key. Mitigations include secure boot and flash encryption where available.

### 2.3  Key storage on the gateway

The gateway stores the per-node key database persistently. The key database maps `key_hint` values to one or more 256-bit keys (see [protocol.md §3.1.1](protocol.md#311--key_hint-semantics) for `key_hint` semantics). Protecting this database is an operational requirement:

- The key store SHOULD be encrypted at rest.
- Exporting the key store SHOULD require explicit operator authorization (see [gateway-requirements.md GW-1001](gateway-requirements.md)).

### 2.4  Key provisioning (USB pairing)

Keys are provisioned through a **USB-mediated pairing** process:

1. The node is connected to a provisioning host (gateway or dedicated tool) via USB.
2. The host generates a unique 256-bit PSK for the node.
3. The key is written to the node's flash key partition and registered in the gateway's key database in a single atomic operation.
4. The node is disconnected and deployed.

There is no over-the-air key exchange or negotiation. The USB connection provides a physically controlled channel for key material transfer.

Re-pairing is possible: a factory-reset node (see §2.6) can be paired again, receiving a new key and a new identity.

### 2.5  Key compromise

If a node's key is compromised (e.g., through firmware exploit or physical flash extraction):

- The compromise is **limited to that node** — other nodes are unaffected.
- The gateway SHOULD remove the compromised node's key from the registry immediately.
- The node can be **factory-reset** (see §2.6) to erase the compromised key and all persistent state.
- After factory reset, the node is re-paired via USB with a fresh key, effectively giving it a new identity.

### 2.6  Factory reset

A factory reset returns a node to its initial un-paired state. The reset erases:

- The **pre-shared key** — the node loses its identity.
- All **persistent map data** — application state is wiped.
- The **resident BPF program** — the node has no program to execute.

After a factory reset, the node is inert: it cannot authenticate with any gateway and will not execute application logic. To return it to service, the operator must re-pair it via USB (§2.4), which provisions a new key and registers the node in the gateway's key database.

Factory reset is the standard remediation path for key compromise, decommissioning, and transferring a node to a different deployment.

---

## 3  Authentication and integrity

### 3.1  Frame structure

Every frame exchanged between a node and the gateway has the following layout:

```
┌─────────────────────────────────────────────────────────────┐
│  Header               │  Payload               │  HMAC      │
│  (key_hint, msg_type, │  (CBOR-encoded         │  (32 bytes)│
│   nonce)              │   message body)        │            │
└─────────────────────────────────────────────────────────────┘
│◄──────── HMAC input (header + payload) ────────►│
```

The authentication tag covers the full header and payload:

```
hmac = HMAC-SHA256(key = node_psk, message = header || payload)
```

The frame on the wire is: `header || payload || hmac`.

See [protocol.md §3](protocol.md#3--frame-format) for the detailed wire specification.

### 3.2  What is authenticated

- **Direction** — the `msg_type` high-bit distinguishes node→gateway from gateway→node frames, and this field is covered by the HMAC.
- **Nonce** — the nonce is part of the fixed binary header and is covered by the HMAC, preventing an attacker from substituting a different nonce to defeat replay protection.
- **Payload** — all CBOR payload bytes are covered by the HMAC; payload modification is detectable.

### 3.3  Gateway verification (inbound from node)

1. Extract `key_hint` from the fixed header.
2. Look up candidate `node_psk`(s) by `key_hint`. If no candidates → silently discard.
3. For each candidate `node_psk`, compute HMAC over header + payload; if any HMAC matches, accept and bind the frame to that node. If none match → silently discard.
4. For `WAKE` messages: accept (WAKE replay is tolerated — see §4.6). For post-WAKE messages: extract sequence number from header; if `seq ≤ last_accepted_seq` → silently discard.
5. Update `last_accepted_seq` for the node.
6. Decode CBOR payload. If malformed → log, discard.
7. Process message.

See [protocol.md §7.2](protocol.md#72--verification-procedure-gateway-inbound) for the normative procedure.

### 3.4  Node verification (inbound from gateway)

1. Compute HMAC over header + payload using the node's own key. If mismatch → discard.
2. Verify that the `nonce` in the response header matches the nonce the node sent in its request header. If mismatch → discard.
3. Decode CBOR payload and process.

See [protocol.md §7.3](protocol.md#73--verification-procedure-node-inbound) for the normative procedure.

### 3.5  No encryption

Authentication provides **integrity** (tamper detection) but **not confidentiality** (secrecy). Traffic on the air can be observed by an attacker. Applications that require confidentiality must encrypt data within the CBOR payload before calling `send()` or `send_recv()`.

---

## 4  Replay protection

### 4.1  Overview

Replay protection uses a two-tier scheme:

- **WAKE messages** use a random nonce. The WAKE message itself is not replay-protected (see §4.6 for risk analysis). The gateway uses the WAKE nonce to bind its response to that specific wake event.
- **All subsequent messages in a wake cycle** use a gateway-assigned sequence number that the node increments for each outbound frame. The gateway rejects any message with a sequence number ≤ the last accepted value for that node.

This design eliminates the need for the node to store any replay-protection state across deep sleep.

### 4.2  WAKE nonce

Each `WAKE` message includes a 64-bit **nonce** generated by the hardware RNG. The nonce is included in the fixed binary header and is covered by the HMAC.

| Property | Value |
|---|---|
| Nonce size | 64 bits |
| Generation | Hardware RNG, generated fresh for each WAKE message |
| Purpose | Binds the gateway's COMMAND response to this specific wake event |
| Persistence | Not stored across deep sleep |

### 4.3  Gateway-assigned sequence numbers (node → gateway)

The gateway maintains a **per-node monotonic counter** (`last_accepted_seq`). On receiving a valid `WAKE`, the gateway includes a **starting sequence number** in its `COMMAND` response. The node uses this value for its next outbound message and increments it for each subsequent message in the wake cycle.

| Parameter | Value |
|---|---|
| Counter size | 64 bits |
| Assignment | Gateway assigns `starting_seq = last_accepted_seq + 1` in the COMMAND response |
| Node behavior | Use `starting_seq` for the first post-WAKE message, increment for each subsequent message |
| Gateway acceptance rule | Reject any message with `seq ≤ last_accepted_seq`; update `last_accepted_seq` on accept |
| Persistence (gateway) | `last_accepted_seq` MUST be persisted durably per node |
| Persistence (node) | None required — the gateway tells the node where to start each wake cycle |

This eliminates the bounded-window replay vulnerability: once a message is accepted, its sequence number can never be reused regardless of how many subsequent messages are sent.

### 4.4  Node verification of gateway responses (gateway → node)

The gateway echoes the node's nonce (for WAKE) or sequence number (for subsequent messages) in the response header. The node verifies that the echoed value matches the value it sent. This prevents:

- A replayed old gateway response from being accepted.
- A response intended for one message from being applied to a different request.

### 4.5  Behavior on stale or replayed messages

| Scenario | Gateway behavior |
|---|---|
| WAKE with valid HMAC | Accept; assign new starting sequence number |
| Post-WAKE message with `seq ≤ last_accepted_seq` | Silently discard. Log internally. |
| Post-WAKE message with `seq > last_accepted_seq` | Accept; update `last_accepted_seq`. |

| Scenario | Node behavior |
|---|---|
| Echoed value does not match sent value | Discard response. |
| Echoed value matches | Accept response. |

### 4.6  WAKE replay risk analysis

WAKE messages are not protected by the sequence number scheme. A replayed WAKE is low risk because:

- **The attacker cannot use the response.** The gateway's COMMAND reply is authenticated with the node's PSK; without the key, the attacker cannot read or forge a follow-up.
- **No state corruption.** The gateway assigns a new (higher) starting sequence number for the replayed session. Since the real node is not awake, no messages arrive for that session. When the real node wakes later, it receives an even higher starting sequence — no conflict.
- **Operational noise only.** The gateway may log a false wake event or attempt a program update to a node that is not listening. Both are harmless and self-correcting (the gateway times out).
- **No replay amplification.** Previously captured post-WAKE messages carry sequence numbers below `last_accepted_seq` and are rejected even if a WAKE is replayed, because the gateway's counter only moves forward.

### 4.7  Sequence numbers and deep sleep

Nodes do **not** store sequence state across deep sleep. On each wake cycle:

1. The node generates a fresh random nonce for its WAKE message.
2. The gateway responds with the next available starting sequence number.
3. The node uses that sequence number for all subsequent messages in the cycle.
4. The node sleeps — no sequence state to persist.

---

## 5  Identity binding

### 5.1  Key is identity

A node's identity is its pre-shared key, not its `key_hint` or any network address. A message is accepted as originating from a specific node if and only if it passes HMAC verification with that node's PSK.

- `key_hint` is a **lookup optimization** to avoid trying every key in the database. It is not an authenticator.
- Two nodes with colliding `key_hint` values are disambiguated by HMAC verification: the gateway tries all candidate keys for the hint and accepts the first match.

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
| Invalid HMAC | Silently discard. Log internally. | Discard frame. |
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

The gateway has no persistent cryptographic identity. Its authority over nodes derives entirely from possession of the node key database. Any gateway instance loaded with the same key database and program assignments can serve nodes transparently.

### 7.2  Key material sharing

For failover to work correctly, the replacement gateway must have:

1. The complete node key database (all `key_hint` → PSK mappings).
2. The current program assignments and schedules for all nodes.
3. The full BPF program library (so it can serve `GET_CHUNK` requests).

Key material MUST be transferred through a physically secured channel (e.g., encrypted export/import via USB media — see [GW-1001](gateway-requirements.md)). Transmitting keys over the air or through unprotected storage is prohibited.

### 7.3  Sequence number synchronization

The per-node `last_accepted_seq` counter MUST be persisted durably by the gateway and synchronized across gateway instances in a failover group. A replacement gateway MUST restore the counter state before processing node traffic.

This is simpler than synchronizing a sliding window: only a single 64-bit integer per node needs to be persisted. The replacement gateway loads the counter values from durable storage (or from the exported state — see [GW-1001](gateway-requirements.md)) and resumes operation. Previously accepted messages are permanently non-replayable because their sequence numbers are below the stored counter.

### 7.4  Program hash consistency

All gateway instances in a failover group MUST serve identical programs for any given program hash. A node that receives a program with a hash that does not match the expected value will reject it and retry, cycling until it receives the correct bytes.

---

## 8  Summary of security properties

| Property | Mechanism | Limitation |
|---|---|---|
| Message integrity | HMAC-SHA256 per frame | Not encrypted; content visible to observers |
| Node authentication | Per-node PSK + HMAC | Key compromise requires factory reset + re-pair |
| Replay protection | Gateway-assigned monotonic sequence numbers | WAKE messages are not sequence-protected (low risk — see §4.6) |
| Program integrity | Content hash (`PROGRAM_ACK`) | Gateway key store must be protected |
| Key storage | Dedicated flash partition | Software-accessible; mitigate with secure boot / flash encryption |
| Key provisioning | USB-mediated pairing | Requires physical USB access |
| Identity binding | PSK = node identity | Factory reset + re-pair to revoke / replace identity |

---

## 9  Design tradeoffs

The security model makes deliberate tradeoffs between security and usability. This section documents the alternatives considered and the rationale for each choice.

### 9.1  Key storage: eFuse vs flash

| | eFuse (hardware fuse) | Flash partition |
|---|---|---|
| **Key accessibility** | Hardware-inaccessible after provisioning; only the HMAC peripheral can use the key | Software-accessible; firmware can read the raw key bytes |
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

### 9.3  Pairing channel: USB vs over-the-air

| | USB-mediated pairing | Over-the-air (OTA) pairing |
|---|---|---|
| **Channel security** | Physical point-to-point connection; no eavesdropping or interception | Radio channel is broadcast; vulnerable to eavesdropping and man-in-the-middle (MITM) attacks |
| **MITM resistance** | Inherent — an attacker must physically intercept the USB cable | Requires a key-agreement protocol (e.g., Diffie–Hellman with out-of-band verification) to resist MITM |
| **Convenience** | Operator must physically connect each node | Nodes could be paired at range without physical contact |
| **Complexity** | Simple: generate key, write to both ends | Requires a secure pairing protocol, trust-on-first-use policy, or out-of-band verification step |

**Chosen: USB.** USB-mediated pairing eliminates the MITM attack surface entirely — the key is transferred over a physically controlled channel. OTA pairing would require a secure key-agreement protocol over the untrusted ESP-NOW radio, adding protocol complexity and a new class of attacks. Since nodes must be physically installed anyway, requiring a USB connection during that process is an acceptable cost.
