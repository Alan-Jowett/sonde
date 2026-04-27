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

## 3  Logical message model

The connector payload schema is organized around **desired state**, **actual
state**, **application data**, and **connector health**.

### 3.1  Control plane → gateway

Control-plane ingress carries **complete desired state** for exactly one
addressable entity per message:

- one **gateway** entity, or
- one **node** entity.

Each desired-state message replaces the previously stored desired state for the
target entity. The connector path does **not** expose imperative node command
RPCs such as "queue reboot now" or "assign program now." Those are internal
gateway reconciliation outcomes.

### 3.2  Gateway → control plane actual-state updates

When the gateway learns or changes actual state relevant to reconciliation, it
emits an upstream connector message. For nodes, this includes the state accepted
from authenticated `WAKE` traffic and the gateway's resulting latest-known node
state, including reception timestamps encoded as Unix time in milliseconds.

### 3.3  Gateway → control plane application-data updates

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

### 3.4  Connector health and loss signaling

The connector model is intended to be lossless under normal circumstances.
Detectable connector-delivery failure or desynchronization must be surfaced to
operators. The exact external transport retry policy is outside the gateway
core, but the connector/gateway boundary must not silently mask detected loss.

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
