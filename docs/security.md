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
- A **factory reset** erases the key partition, returning the node to an unpaired state (see §2.6).
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

A factory reset returns a node to its initial unpaired state. The reset erases:

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
4. For `WAKE` messages: accept; create an active session for this node (replacing any previous session). For post-WAKE messages: verify the node has an active session and the sequence number matches the expected next value. If no active session or wrong sequence → silently discard.
5. Advance the session's expected sequence number.
6. Decode CBOR payload. If malformed → log, discard.
7. Process message.

See [protocol.md §7.2](protocol.md#72--verification-procedure-gateway-inbound) for the normative procedure.

### 3.4  Node verification (inbound from gateway)

1. Compute HMAC over header + payload using the node's own key. If mismatch → discard.
2. Verify that the echoed value in the response header (nonce for WAKE, sequence number for post-WAKE messages) matches the value sent in the corresponding request header. If mismatch → discard.
3. Decode CBOR payload and process.

See [protocol.md §7.3](protocol.md#73--verification-procedure-node-inbound) for the normative procedure.

### 3.5  No encryption

Authentication provides **integrity** (tamper detection) but **not confidentiality** (secrecy). Traffic on the air can be observed by an attacker. Applications that require confidentiality must encrypt data within the CBOR payload before calling `send()` or `send_recv()`.

---

## 4  Replay protection

### 4.1  Overview

Replay protection uses **session-scoped sequence numbers** tied to the WAKE nonce:

- **WAKE messages** include a random 64-bit nonce generated by the node. The nonce identifies the session.
- **The gateway responds** with a randomly chosen starting sequence number in its COMMAND response (authenticated by HMAC).
- **All subsequent messages in the wake cycle** use incrementing sequence numbers starting from that value. The gateway rejects any message whose sequence number does not match the expected next value for the active session.

No persistent replay-protection state is required on either the node or the gateway. The gateway tracks only active sessions in memory.

### 4.2  WAKE nonce (session identifier)

Each `WAKE` message includes a 64-bit **nonce** generated by the hardware RNG. The nonce is included in the fixed binary header and is covered by the HMAC. It serves as the **session identifier** — the gateway uses it to associate subsequent messages with this wake cycle.

| Property | Value |
|---|---|
| Nonce size | 64 bits |
| Generation | Hardware RNG, generated fresh for each WAKE message |
| Purpose | Identifies the session; binds the gateway's COMMAND response to this wake event |
| Persistence | Not stored across deep sleep |

### 4.3  Gateway-assigned sequence numbers (node → gateway)

On receiving a valid `WAKE`, the gateway creates an **active session** identified by the node's PSK and nonce. It assigns a **random starting sequence number** and includes it in the `COMMAND` response. The node uses this value for its next outbound message and increments it for each subsequent message in the wake cycle.

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
| WAKE with valid HMAC | Accept; create active session with new random starting sequence |
| Post-WAKE message matching an active session with expected seq | Accept; advance expected seq |
| Post-WAKE message with wrong seq or no matching active session | Silently discard. Log internally. |

| Scenario | Node behavior |
|---|---|
| Echoed value does not match sent value | Discard response. |
| Echoed value matches | Accept response. |

### 4.7  Why replaying captured traffic fails

An attacker who captures an entire wake session (WAKE + all post-WAKE messages) cannot replay it:

1. **Replayed WAKE** — the gateway creates a new active session with a *different* random starting sequence number. The attacker can observe this value in the (unencrypted) COMMAND response, but cannot generate valid follow-up messages without the PSK to compute correct HMACs.
2. **Replayed post-WAKE messages** — these carry the *original* session's sequence numbers, which do not match the new session's expected sequence. The gateway rejects them.
3. **Replayed post-WAKE without replaying WAKE** — there is no active session with the original nonce. The gateway rejects them.

The sequence number is not a secret — it is an anti-replay counter. The HMAC proves authenticity; the sequence number ensures each authenticated message is accepted exactly once within its session.

### 4.8  WAKE replay risk analysis

WAKE messages themselves can be replayed. This is low risk because:

- **The attacker cannot forge follow-up messages.** The gateway's COMMAND reply assigns a new starting sequence, but the attacker cannot use it without the PSK to compute valid HMACs.
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
| Message integrity | HMAC-SHA256 per frame | Not encrypted; content visible to observers |
| Node authentication | Per-node PSK + HMAC | Key compromise requires factory reset + re-pair |
| Replay protection | Session-scoped sequence numbers (nonce + random starting seq) | WAKE messages are replayable (low risk — see §4.8); no persistent state required |
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
