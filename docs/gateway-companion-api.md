<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Gateway Connector API

> **Document status:** Draft  
> **Scope:** The local framed integration API between `sonde-gateway` and a single connector process that bridges the gateway to an external control plane.  
> **Audience:** Developers building transport adapters and control planes for Sonde gateways.  
> **Related:** [gateway-requirements.md](gateway-requirements.md), [gateway-design.md](gateway-design.md), [gateway-api.md](gateway-api.md)
>
> **Note:** This document now defines the **connector API**. The file remains
> named `gateway-companion-api.md` only for compatibility with existing links
> and references. The earlier companion-sidecar gRPC contract has been retired
> from the supported architecture. Bootstrap-oriented local clients use
> `GatewayAdmin` for limited operator-facing actions such as transient modem
> display; long-running control-plane/runtime traffic uses the connector model
> defined here.

---

## 1  Overview

The connector API is a **separate integration surface** from both:

1. **`GatewayAdmin`** — the operator-facing local admin API.
2. **`gateway-api.md`** — the handler stdin/stdout CBOR data-plane API.

A connector process uses this API when it needs to:

- receive upstream gateway and node state updates relevant to reconciliation,
- receive upstream node application payload data, and
- deliver complete desired-state messages from a control plane to the gateway.

The connector process is intentionally a **transport adapter**. It transports
connector messages between the gateway's local socket and some external,
asynchronous, store-and-forward control-plane transport. The gateway does not
encode Azure-specific, Service Bus-specific, or other cloud-vendor-specific
logic in this interface.

---

## 2  Transport and lifecycle

### 2.1  Local transport

The gateway exposes the connector API over **local-only IPC** on a dedicated
endpoint:

- **Unix/macOS:** Unix domain socket, default `/var/run/sonde/connector.sock`
- **Windows:** named pipe, default `\\.\pipe\sonde-connector`

No TCP listener is exposed in v1.

### 2.2  Framing

Each connector message is framed as:

```
┌──────────────────────────────────┐
│  Length (4 bytes, big-endian)    │
│  Message bytes (Length bytes)    │
└──────────────────────────────────┘
```

The message bytes contain a Sonde-defined connector protocol payload. The
connector adapter forwards these bytes unchanged; only the gateway and the
control plane interpret the payload schema.

The gateway MUST reject any connector frame whose length exceeds the configured
maximum connector message size. The default maximum is **1 MB** (1,048,576
bytes). This bound applies to the `Length` field's declared message bytes only;
it does not include the 4-byte length prefix itself. If the length prefix
exceeds the configured bound, or if the peer closes the connection before the
declared payload bytes are fully received, the gateway closes the connector
session.

### 2.3  Session model

The gateway accepts at most one active connector session at a time. The
connector session is long-lived and bidirectional:

1. Connector process connects to the local socket.
2. Gateway and connector exchange framed messages in both directions.
3. If the connector disconnects, the gateway stops delivering connector traffic
   until a new connector session is established.
4. If a second connector client attempts to connect while a session is active,
   the gateway rejects or closes the new connection without disrupting the
   active connector.

The external transport behind the connector is outside this document's scope.

---

## 3  Connector payload schema

The connector payload schema is organized around **desired state**, **actual
state**, **application data**, and **connector health**. All connector message
bytes are encoded as CBOR maps with integer keys.

### 3.1  Common encoding conventions

All connector messages use the following common fields:

| Field | CBOR key | Type | Description |
|---|---|---|---|
| `msg_type` | 1 | uint | Connector message type. |

Connector message type values are:

| `msg_type` | Name | Direction |
|---|---|---|
| `0x01` | `DESIRED_STATE` | Control plane → gateway |
| `0x02` | `ACTUAL_STATE` | Gateway → control plane |
| `0x03` | `APP_DATA` | Gateway → control plane |
| `0x04` | `CONNECTOR_HEALTH` | Gateway → control plane |

Timestamps carried by connector messages are encoded as Unix time in
milliseconds.

The following rules apply to all connector payloads:

- **Integer keys only for schema-defined maps.** Every CBOR map defined by this
  document uses unsigned integer keys, including nested maps.
- **Per-message keyspaces with explicit shared fields.** Field meanings are
  defined per schema-defined map or message type, not globally across the
  connector API. A key MAY be reused by a different message type with a
  different meaning unless this document explicitly defines it as a common
  field shared across message types. For connector messages, key `1` is the
  shared `msg_type` field.
- **Reserved ranges for extension.** Within each schema-defined map, keys
  `1`-`127` are available for fields defined by this specification and keys
  `128`-`255` are reserved for future Sonde-defined standard fields.
- **Unknown key handling.** Receivers MUST ignore unknown keys in any
  schema-defined map unless a field definition for that message states
  otherwise.
- **Explicit opaque payloads only.** If a field is intended to carry opaque CBOR
  whose internal structure is outside this document, the field definition must
  say so explicitly.

### 3.2  `DESIRED_STATE` (control plane → gateway)

Control-plane ingress carries **complete desired state** for exactly one
addressable entity per message:

- one **gateway** entity, or
- one **node** entity.

Each desired-state message replaces the previously stored desired state for the
target entity. The connector path does **not** expose imperative node command
RPCs such as "queue reboot now" or "assign program now." Those are internal
gateway reconciliation outcomes.

| Field | CBOR key | Type | Description |
|---|---|---|---|
| `msg_type` | 1 | uint | `0x01` |
| `entity_kind` | 2 | tstr | `"gateway"` or `"node"` |
| `entity_id` | 3 | tstr | Opaque identifier of the target entity. For `entity_kind = "node"`, this is the target node identifier. For `entity_kind = "gateway"`, this is the lowercase hex encoding of the raw 16-byte `gateway_id` (e.g., `"a1b2c3d4e5f6a7b8a1b2c3d4e5f6a7b8"`). |
| `desired_state` | 4 | map | Complete desired-state map for the target entity. The payload schema depends on `entity_kind`; see sections 3.2.1 and 3.2.2. Any map nested under `desired_state` also uses integer CBOR keys. |

The connector API models exactly one gateway entity per connector stream.
The `entity_id` field distinguishes multiple gateways sharing one cloud
deployment (e.g., during gateway replacement failover).

#### 3.2.1  `desired_state` payload for `entity_kind = "gateway"`

`desired_state` is a CBOR map with the following schema:

| Field | CBOR key | Type | Description |
|---|---|---|---|
| `channel` | 15 | uint/null | Desired ESP-NOW channel. `null` means no channel change requested. |
| `salt` | 21 | bstr (16 bytes)/null | KDF salt from cloud. Adopted by gateway only if it has no local salt. `null` means no salt provided. |
| `kdf_params` | 22 | map/null | KDF parameters `{1: m_cost, 2: t_cost, 3: p_cost, 4: kdf_version}`. `null` means no params provided. |
| `rotation_payload` | 28 | bstr/null | X25519-encrypted `RotationPayloadV1` for master key rotation. `null` means no rotation requested. See `evolve-962-specification.md` §2.6.1 for format. |
| `recovered_psks` | 29 | array/null | Array of recovered PSK records for missing nodes. Each record is a CBOR map: `{1: node_id (tstr), 2: key_hint (uint), 3: encrypted_psk (bstr), 4: master_key_id (bstr)}`. `null` means no recovery records. |

#### 3.2.2  `desired_state` payload for `entity_kind = "node"`

`desired_state` is a CBOR map with the following schema:

Each `DESIRED_STATE` message is a complete replacement of the previously known
desired state for that entity, not a patch. For the fields defined in this
draft, a missing key and a present key with value `null` have the same meaning:
the replacement desired state explicitly contains no desired value for that
field. Receivers MUST NOT interpret `null` as "leave the previous value
unchanged".

| Field | CBOR key | Type | Description |
|---|---|---|---|
| `assigned_program_hash` | 1 | bstr/null | Desired resident program hash. `null` means no resident program assignment is desired. |
| `schedule_interval_s` | 2 | uint/null | Desired node wake interval in seconds. `null` means no scheduled interval target is desired in this draft. |
| `ephemeral_program_hash` | 3 | bstr/null | Desired ephemeral program hash to queue when reconciliation determines one is needed. `null` means no ephemeral run is requested. |
| `assigned_program_elf` | 5 | bstr/null | Raw ELF binary for the assigned program. Present when the control plane embeds the program so the gateway can verify and ingest it into its local `ProgramLibrary` without a separate fetch. The gateway MUST run full Prevail verification (`ProgramLibrary::ingest_elf`) on the ELF before accepting it. Absent or `null` means no inline program is provided; the gateway must already have the program locally. |
| `assigned_program_verification_profile` | 6 | text/null | Verification profile for the inline ELF: `"resident"` (default if absent) or `"ephemeral"`. Only meaningful when key 5 is present. |
| `assigned_program_source_filename` | 7 | text/null | Display filename for the inline program. Optional metadata stored alongside the ingested program. |
| `assigned_program_abi_version` | 8 | uint/null | ABI version the inline program targets. Optional metadata stored alongside the ingested program. |

Fields not yet defined by this draft remain reserved for future desired-state
extension. Receivers MUST ignore unknown integer keys.

### 3.3  `ACTUAL_STATE` (gateway → control plane)

When the gateway learns or changes actual state relevant to reconciliation, it
emits an upstream connector message. For nodes, this includes the state accepted
from authenticated `WAKE` traffic and the gateway's resulting latest-known node
state.

| Field | CBOR key | Type | Description |
|---|---|---|---|
| `msg_type` | 1 | uint | `0x02` |
| `entity_kind` | 2 | tstr | `"gateway"` or `"node"` |
| `entity_id` | 3 | tstr | Opaque identifier of the affected entity. For `entity_kind = "node"`, this is the affected node identifier. For `entity_kind = "gateway"`, this is the lowercase hex encoding of the raw 16-byte `gateway_id`. |
| `current_program_hash` | 4 | bstr/null | Current node program hash when applicable. |
| `assigned_program_hash` | 5 | bstr/null | Gateway-assigned resident program hash when applicable. |
| `battery_mv` | 6 | uint/null | Latest node battery reading in millivolts when applicable. |
| `firmware_abi_version` | 7 | uint/null | Firmware ABI version when applicable. |
| `firmware_version` | 8 | tstr/null | Firmware version string when applicable. |
| `timestamp_ms` | 9 | uint | Reception timestamp in Unix milliseconds. For node-scoped `ACTUAL_STATE`, this is the node's last check-in time as observed by the gateway. |
| `status_details` | 10 | map | Additional gateway- or entity-scoped status fields relevant to reconciliation. See section 3.3.1. |
| `schedule_interval_s` | 11 | uint/null | Latest node wake interval in seconds when applicable. |
| `encrypted_psk` | 12 | bstr (60 bytes)/null | Encrypted PSK blob for node escrow. Node-scoped only; omit for gateway. |
| `escrow_key_hint` | 13 | uint (0–65535)/null | `key_hint` (u16) for Azure recovery lookup. Node-scoped only. |
| `master_key_id` | 14 | bstr (16 bytes)/null | Opaque master key ID that encrypted this node's PSK. Node-scoped only. |
| `channel` | 15 | uint/null | ESP-NOW channel. Gateway-scoped only. |
| `master_key_id` | 16 | bstr (16 bytes)/null | Gateway's current master key ID. Gateway-scoped only. |
| `master_key_epoch` | 17 | uint/null | Monotonic master key epoch. Gateway-scoped only. |
| `x25519_public_key` | 18 | bstr (32 bytes)/null | X25519 public key derived from GatewayIdentity. Gateway-scoped only. |
| `fingerprint_words` | 19 | array of tstr/null | 6-word BIP-39 fingerprint. Gateway-scoped only. |
| `missing_key_hints` | 20 | array of uint/null | Unknown key_hints (one-shot, cleared after reporting). Gateway-scoped only. |
| `salt` | 21 | bstr (16 bytes)/null | KDF salt. Gateway-scoped only. |
| `kdf_params` | 22 | map/null | `{1: m_cost, 2: t_cost, 3: p_cost, 4: kdf_version}`. Gateway-scoped only. |
| `gateway_version` | 23 | tstr/null | Gateway binary semver. Gateway-scoped only. |
| `gateway_commit` | 24 | tstr/null | Gateway binary git commit. Gateway-scoped only. |
| `modem_firmware_version` | 25 | tstr/null | Modem firmware semver. Gateway-scoped only. |
| `modem_firmware_commit` | 26 | tstr/null | Modem firmware git commit. Gateway-scoped only. |
| `rotation_in_progress` | 27 | bool/null | `true` if rotation is in progress. Gateway-scoped only. |
| `wake_rssi_dbm` | 28 | int/null | Modem-measured receive RSSI (dBm) of the node's WAKE frame. Node-scoped only; null when the transport does not supply RSSI metadata. |

**Key scoping:** Keys 4–8, 10–11, 28 are node-specific and MUST be omitted for
`entity_kind = "gateway"`. Keys 12–14 are node escrow fields. Keys 15–27
are gateway-specific and MUST be omitted for `entity_kind = "node"`. Key 9
(`timestamp_ms`) is shared and required for all entity kinds. Decoders
dispatch on `entity_kind` to interpret the correct keys.

#### 3.3.1  `status_details` payload

`status_details` is a CBOR map with the following schema:

| Field | CBOR key | Type | Description |
|---|---|---|---|
| *(no fields currently defined)* | — | — | Senders SHOULD encode an empty map (`{}`) when no additional status details are available. Receivers MUST accept an empty map and MUST ignore unknown integer keys for forward compatibility. |

### 3.4  `APP_DATA` (gateway → control plane)

When the gateway accepts node-originated application payload data, it emits an
upstream connector message containing:

- `node_id`
- `program_hash`
- the opaque application payload bytes
- a reception timestamp encoded as Unix time in milliseconds
- an origin discriminator (`app_data` or `wake_blob`)

This path is informational only. The connector API does not provide a reply
channel for node `send_recv()` responses; those continue to flow through the
handler API.

| Field | CBOR key | Type | Description |
|---|---|---|---|
| `msg_type` | 1 | uint | `0x03` |
| `node_id` | 2 | tstr | Opaque node identifier assigned by the gateway. |
| `program_hash` | 3 | bstr | Hash of the program that produced the payload. |
| `payload` | 4 | bstr | Opaque application payload bytes. |
| `timestamp_ms` | 5 | uint | Reception timestamp in Unix milliseconds. |
| `payload_origin` | 6 | tstr | `"app_data"` or `"wake_blob"` |

### 3.5  `CONNECTOR_HEALTH` (gateway → control plane)

The connector model is intended to be lossless under normal circumstances.
Detectable connector-delivery failure or desynchronization must be surfaced to
operators. The exact external transport retry policy is outside the gateway
core, but the connector/gateway boundary must not silently mask detected loss.

For this section, **stale state** means any control-plane-visible state whose
current value can no longer be assumed to match the gateway's authoritative view
because one or more connector messages may have been lost, duplicated, or
applied out of order after a detected connector fault.

When `health_state` is not `ok`, operators and connector implementations must
treat the following as potentially stale until the condition is cleared and the
relevant state is re-read from an authoritative gateway surface:

- **Desired state reflected by the control plane** — the external system may be
  showing a requested configuration that the gateway did not accept or only
  partially applied.
- **Node status / inventory / last-known observations** — the control plane may
  be missing newer `ACTUAL_STATE` updates or may still be showing superseded
  values.
- **Application-data visibility** — the control plane may be missing `APP_DATA`
  messages, may receive them out of order, or may be unable to determine
  whether a payload was already forwarded before the detected fault.
- **Pending commands or reconciliation progress** — any in-flight action derived
  from desired state must be considered uncertain until re-confirmed.

Operator guidance tied to `health_state`:

- `ok`: normal operation. No special handling is required.
- `degraded`: delivery is impaired but not known to be unrecoverable. Treat
  newly received state as advisory and verify affected changes through an
  authoritative gateway surface before concluding reconciliation succeeded.
- `desynchronized`: the connector/control-plane view is unreliable until
  resynchronized. Rebuild the external view from authoritative gateway state
  before resuming normal automation.

`health_state` is an enumerated text field. Senders MUST encode exactly one of
`ok`, `degraded`, or `desynchronized`. Receivers that encounter any other
`health_state` string MUST treat the condition as equivalent to
`desynchronized` for safety and SHOULD surface the unrecognized value for
diagnostics.

| Field | CBOR key | Type | Description |
|---|---|---|---|
| `msg_type` | 1 | uint | `0x04` |
| `health_state` | 2 | tstr | Connector health classification. MUST be one of `ok`, `degraded`, or `desynchronized`; receivers MUST treat unknown values as `desynchronized` for safety. |
| `timestamp_ms` | 3 | uint | Timestamp when the health condition was observed. |
| `details` | 4 | map | Additional operator-facing details about the detected condition, including stale-state scope and suggested remediation when applicable. See section 3.5.1. |

#### 3.5.1  `details` payload

`details` is a CBOR map with the following schema:

| Field | CBOR key | Type | Description |
|---|---|---|---|
| `failure_mode` | 1 | tstr | Short identifier for the detected connector fault. |
| `stale_scope` | 2 | array | Array of text labels naming the potentially stale state domains, such as `desired_state`, `actual_state`, `app_data`, or `reconciliation_progress`. |
| `remediation` | 3 | tstr | Suggested operator action or recovery guidance when known. |

Receivers MUST ignore unknown integer keys in this map.

---

## 4  Behavioral notes

1. The connector protocol is **cloud-agnostic**. Any external broker or control
   plane may be used as long as it can carry the framed connector messages.
2. The gateway remains the reconciler. The control plane sends desired state;
   the gateway determines which node-facing commands are required to converge.
3. Upstream application data is **informational only**; it does not replace the
   existing handler data-plane contract.
4. Admin/operator workflows remain on `GatewayAdmin`; the connector API is not a
   second admin surface.
5. Bootstrap-only local clients use `GatewayAdmin` rather than the connector
   path when they need operator-visible actions such as transient display.
