<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Gateway Connector API

> **Document status:** Draft  
> **Scope:** The local framed integration API between `sonde-gateway` and a single connector process that bridges the gateway to an external control plane.  
> **Audience:** Developers building transport adapters and control planes for Sonde gateways.  
> **Related:** [gateway-requirements.md](gateway-requirements.md), [gateway-design.md](gateway-design.md), [gateway-api.md](gateway-api.md)
>
> **Note:** This document is titled `Gateway Connector API`, but the file remains named `gateway-companion-api.md` for compatibility with existing links and references.

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
| `entity_id` | 3 | tstr | Opaque identifier of the target entity. For gateway-scoped state, this identifies the gateway instance. |
| `desired_state` | 4 | map | Complete desired-state map for the target entity. |

### 3.3  `ACTUAL_STATE` (gateway → control plane)

When the gateway learns or changes actual state relevant to reconciliation, it
emits an upstream connector message. For nodes, this includes the state accepted
from authenticated `WAKE` traffic and the gateway's resulting latest-known node
state.

| Field | CBOR key | Type | Description |
|---|---|---|---|
| `msg_type` | 1 | uint | `0x02` |
| `entity_kind` | 2 | tstr | `"gateway"` or `"node"` |
| `entity_id` | 3 | tstr | Opaque identifier of the affected entity. |
| `current_program_hash` | 4 | bstr/null | Current node program hash when applicable. |
| `assigned_program_hash` | 5 | bstr/null | Gateway-assigned resident program hash when applicable. |
| `battery_mv` | 6 | uint/null | Latest node battery reading in millivolts when applicable. |
| `firmware_abi_version` | 7 | uint/null | Firmware ABI version when applicable. |
| `firmware_version` | 8 | tstr/null | Firmware version string when applicable. |
| `timestamp_ms` | 9 | uint | Reception timestamp in Unix milliseconds. |
| `status_details` | 10 | map | Additional gateway- or entity-scoped status fields relevant to reconciliation. |

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

| Field | CBOR key | Type | Description |
|---|---|---|---|
| `msg_type` | 1 | uint | `0x04` |
| `health_state` | 2 | tstr | Connector health classification such as `ok`, `degraded`, or `desynchronized`. |
| `timestamp_ms` | 3 | uint | Timestamp when the health condition was observed. |
| `details` | 4 | map | Additional operator-facing details about the detected condition. |

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
