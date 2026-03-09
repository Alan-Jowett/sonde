# Gateway Application API

> **Document status:** Draft  
> **Scope:** API surface exposed by the Sonde gateway to external applications.  
> **Audience:** Developers building applications on the Sonde platform.  
> **Related:** [gateway-requirements.md](gateway-requirements.md), [protocol.md](protocol.md)

---

## 1  Overview

The Sonde gateway is a platform service. It handles node protocol, authentication, program distribution, and scheduling. **Application logic runs in a separate process** that communicates with the gateway through a well-defined API.

```
┌──────────────────────────────────────────────────┐
│                  Developer ships                 │
│                                                  │
│  ┌──────────────┐        ┌────────────────────┐  │
│  │  BPF ELF     │        │  Gateway App       │  │
│  │  (node-side) │        │  (any language)    │  │
│  └──────┬───────┘        └────────┬───────────┘  │
│         │                         │              │
└─────────┼─────────────────────────┼──────────────┘
          │                         │
  ┌───────▼───────┐        ┌────────▼───────────┐
  │  Sonde Node   │◄──────►│  Sonde Gateway     │
  │  (firmware)   │ radio  │  (service/daemon)   │
  └───────────────┘        └────────────────────┘
```

This separation means:
- Gateway and application are developed and deployed independently.
- Applications can be written in any language.
- Multiple applications can connect to the same gateway simultaneously.
- The gateway can be upgraded without changing applications (and vice versa).

---

## 2  Transport

The gateway exposes a local API over a **Unix domain socket** (Linux/macOS) or **named pipe** (Windows).

| Parameter | Value |
|---|---|
| Default socket path | `/var/run/sonde/gateway.sock` |
| Default named pipe | `\\.\pipe\sonde-gateway` |
| Wire format | Length-prefixed CBOR messages |
| Direction | Bidirectional (full-duplex) |

### 2.1  Framing

Each message on the socket is framed as:

```
┌──────────────────────────────────┐
│  Length (4 bytes, big-endian)    │
│  CBOR payload (Length bytes)    │
└──────────────────────────────────┘
```

The length field does not include itself.

### 2.2  Message envelope

Every message (both directions) uses the same CBOR envelope:

| Field | CBOR key | Type | Description |
|---|---|---|---|
| `msg_type` | 1 | uint | Message type discriminator. |
| `request_id` | 2 | uint | Caller-assigned ID for request/response correlation. |
| `payload` | 3 | map | Message-specific fields. |
| `error` | 4 | tstr (optional) | Error description (responses only). Absent on success. |

**⚠ OPEN:** Should the transport be a Unix socket with CBOR, or would gRPC / HTTP be more appropriate? Unix socket + CBOR is the lightest option and matches the CBOR-native protocol. gRPC adds a protobuf dependency but provides codegen and streaming for free.

---

## 3  Message types

### 3.1  Application → Gateway

| msg_type | Name | Description |
|---|---|---|
| `0x01` | `SUBSCRIBE` | Register to receive APP_DATA from specific nodes. |
| `0x02` | `UNSUBSCRIBE` | Stop receiving APP_DATA for specific nodes. |
| `0x03` | `ASSIGN_PROGRAM` | Assign a BPF ELF to one or more nodes. |
| `0x04` | `SET_SCHEDULE` | Set the base wake interval for a node. |
| `0x05` | `QUERY_NODES` | Query the node registry. |
| `0x06` | `QUERY_NODE` | Get detailed status for a single node. |
| `0x07` | `RUN_EPHEMERAL` | Send an ephemeral program to a node. |
| `0x08` | `REBOOT_NODE` | Issue a REBOOT command to a node on next wake. |
| `0x09` | `ADD_NODE` | Register a new node (key_hint + key). |
| `0x0A` | `REMOVE_NODE` | Deregister a node. |

### 3.2  Gateway → Application

| msg_type | Name | Description |
|---|---|---|
| `0x81` | `RESPONSE` | Response to any application request. |
| `0x82` | `APP_DATA_EVENT` | Notification: APP_DATA received from a node. Application must reply. |
| `0x83` | `NODE_EVENT` | Notification: node state change (wake, program update, battery alert). |

---

## 4  Message definitions

### 4.1  SUBSCRIBE

Register to receive `APP_DATA_EVENT` notifications for specific nodes (or all nodes).

**Request payload:**

| Field | Type | Required | Description |
|---|---|---|---|
| `node_filter` | array of bstr | No | List of node keys (HMAC keys) to subscribe to. If omitted, subscribe to all nodes. |

**Response payload:** Empty on success.

An application can subscribe multiple times with different filters; subscriptions are additive. If multiple applications are connected, all subscribers receive the event, but **only one** must respond (see §4.8).

---

### 4.2  UNSUBSCRIBE

**Request payload:**

| Field | Type | Required | Description |
|---|---|---|---|
| `node_filter` | array of bstr | No | List of node keys to unsubscribe from. If omitted, unsubscribe from all. |

**Response payload:** Empty on success.

---

### 4.3  ASSIGN_PROGRAM

Assign a BPF ELF binary as the resident program for one or more nodes. The gateway verifies the program (Prevail) and distributes it on each node's next wake.

**Request payload:**

| Field | Type | Required | Description |
|---|---|---|---|
| `program` | bstr | Yes | Complete BPF ELF binary. |
| `nodes` | array of bstr | Yes | List of node keys to assign the program to. |

**Response payload:**

| Field | Type | Description |
|---|---|---|
| `program_hash` | bstr | Hash of the accepted program. |
| `verification_result` | tstr | `"pass"` or verification error details. |

The gateway rejects the request if the program fails Prevail verification.

---

### 4.4  SET_SCHEDULE

Set the base wake interval for a node.

**Request payload:**

| Field | Type | Required | Description |
|---|---|---|---|
| `node` | bstr | Yes | Node key. |
| `interval_s` | uint | Yes | Base wake interval in seconds. |

**Response payload:** Empty on success.

---

### 4.5  QUERY_NODES

List all registered nodes with summary status.

**Request payload:** Empty (or optional filters TBD).

**Response payload:**

| Field | Type | Description |
|---|---|---|
| `nodes` | array of map | List of node summaries. |

Each node summary:

| Field | Type | Description |
|---|---|---|
| `key_hint` | uint | Node's key_hint value. |
| `program_hash` | bstr | Hash of the assigned program. |
| `node_program_hash` | bstr | Hash of the program currently on the node (from last WAKE). |
| `interval_s` | uint | Current base wake interval. |
| `last_wake` | uint | Unix timestamp of last WAKE. |
| `battery_mv` | uint | Last reported battery voltage. |
| `firmware_abi_version` | uint | Last reported ABI version. |

---

### 4.6  QUERY_NODE

Get detailed status for a single node.

**Request payload:**

| Field | Type | Required | Description |
|---|---|---|---|
| `node` | bstr | Yes | Node key. |

**Response payload:** Same fields as a single node summary (§4.5), plus:

| Field | Type | Description |
|---|---|---|
| `pending_command` | tstr | Next command queued for this node (if any). |
| `transfer_progress` | map | Chunked transfer state (if in progress). |

---

### 4.7  RUN_EPHEMERAL

Queue an ephemeral BPF program for execution on a node's next wake.

**Request payload:**

| Field | Type | Required | Description |
|---|---|---|---|
| `program` | bstr | Yes | Complete ephemeral BPF ELF binary. |
| `node` | bstr | Yes | Node key. |

**Response payload:**

| Field | Type | Description |
|---|---|---|
| `program_hash` | bstr | Hash of the accepted program. |
| `verification_result` | tstr | `"pass"` or verification error details. |

The gateway verifies the program against the ephemeral profile before accepting.

---

### 4.8  APP_DATA_EVENT

Sent by the gateway when a node sends `APP_DATA`. The application **must** reply with the blob to include in `APP_DATA_REPLY`.

**Event payload:**

| Field | Type | Description |
|---|---|---|
| `node` | bstr | Node key (identifies which node sent the data). |
| `key_hint` | uint | Node's key_hint value. |
| `blob` | bstr | The opaque APP_DATA blob from the BPF program. |
| `timestamp` | uint | Unix timestamp of reception. |

**Required response payload:**

| Field | Type | Required | Description |
|---|---|---|---|
| `reply_blob` | bstr | Yes | Opaque blob to send back in `APP_DATA_REPLY`. Use zero-length for acknowledgement only. |

**Timeout:** If the application does not respond within a configurable timeout, the gateway sends an `APP_DATA_REPLY` with a zero-length blob.

**⚠ OPEN:** When multiple applications are subscribed, how is the responder selected? Options:
1. First subscriber wins (others receive the event but don't respond).
2. Primary/secondary designation at subscribe time.
3. Only one subscriber per node is allowed.

---

### 4.9  REBOOT_NODE

Queue a REBOOT command for a node on its next wake.

**Request payload:**

| Field | Type | Required | Description |
|---|---|---|---|
| `node` | bstr | Yes | Node key. |

**Response payload:** Empty on success.

---

### 4.10  ADD_NODE

Register a new node with the gateway.

**Request payload:**

| Field | Type | Required | Description |
|---|---|---|---|
| `key_hint` | uint | Yes | 16-bit key_hint value for the node. |
| `key` | bstr | Yes | 256-bit pre-shared HMAC key. |
| `interval_s` | uint | No | Initial base wake interval (default: gateway-configured). |

**Response payload:** Empty on success. Error if key_hint + key combination conflicts.

---

### 4.11  REMOVE_NODE

Deregister a node.

**Request payload:**

| Field | Type | Required | Description |
|---|---|---|---|
| `node` | bstr | Yes | Node key. |

**Response payload:** Empty on success.

---

### 4.12  NODE_EVENT

Asynchronous notification of node lifecycle events. Informational — no response required.

**Event payload:**

| Field | Type | Description |
|---|---|---|
| `node` | bstr | Node key. |
| `key_hint` | uint | Node's key_hint value. |
| `event_type` | tstr | One of: `"wake"`, `"program_updated"`, `"program_ack"`, `"ephemeral_complete"`, `"battery_low"`, `"timeout"`. |
| `details` | map | Event-specific data (e.g., `battery_mv`, `program_hash`, `firmware_abi_version`). |
| `timestamp` | uint | Unix timestamp. |

---

## 5  Node identity in the API

In the node-gateway radio protocol, nodes are identified by `key_hint` (a lookup hint) and authenticated by HMAC. In the application API, nodes are identified by their **HMAC key** (as a byte string). This is the unambiguous, collision-free identifier.

The `key_hint` is included in events and query results for informational purposes, but the `node` field (the key) is the primary identifier for all API operations.

**⚠ OPEN:** Exposing raw HMAC keys to applications raises a security question. Should the API use an opaque node ID (e.g., a hash of the key) instead, keeping the actual key internal to the gateway?

---

## 6  Error handling

All requests receive a `RESPONSE` message. On error, the `error` field contains a human-readable description.

| Error | Description |
|---|---|
| `"unknown_node"` | The specified node key is not in the registry. |
| `"verification_failed"` | BPF program failed Prevail verification. |
| `"program_too_large"` | Program exceeds size limit. |
| `"node_busy"` | A command is already queued for this node. |
| `"invalid_request"` | Malformed request or missing required fields. |
| `"timeout"` | Operation timed out. |

---

## 7  Concurrency model

- Multiple applications can connect simultaneously.
- Each connection is independent (separate subscriptions, separate request IDs).
- The gateway serializes per-node state — two applications cannot queue conflicting commands for the same node.
- `APP_DATA_EVENT` delivery and response handling must be resolved when multiple subscribers exist (see §4.8 open question).

---

## 8  Lifecycle

1. **Application connects** to the gateway socket.
2. **Application subscribes** to nodes of interest.
3. **Gateway sends events** (APP_DATA_EVENT, NODE_EVENT) as nodes wake and communicate.
4. **Application sends commands** (ASSIGN_PROGRAM, SET_SCHEDULE, etc.) as needed.
5. **Application disconnects** — all subscriptions for that connection are removed. Queued commands remain.

---

## 9  Open questions

| ID | Section | Question |
|---|---|---|
| A-1 | §2 | Unix socket + CBOR vs. gRPC vs. HTTP? |
| A-2 | §4.8 | Multi-subscriber APP_DATA_EVENT response ownership? |
| A-3 | §5 | Expose raw HMAC keys or opaque node IDs to applications? |
