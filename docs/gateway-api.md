# Application API

> **Document status:** Draft  
> **Scope:** The data-plane API between a Sonde gateway and developer applications.  
> **Audience:** Developers building applications on the Sonde platform.  
> **Related:** [gateway-requirements.md](gateway-requirements.md), [protocol.md](protocol.md)

---

## 1  Overview

A Sonde application consists of two parts:

1. **A BPF program** — runs on the node, reads sensors, and calls `send()` to emit data.
2. **An application handler** — runs alongside the gateway, receives that data, and replies.

The gateway handles everything in between: protocol, authentication, program distribution, scheduling, node management. The developer never interacts with nodes, keys, or the radio protocol.

```
┌─────────────────────────────────────────────────────────────────┐
│                      Developer writes                           │
│                                                                 │
│  ┌──────────────────┐              ┌──────────────────────┐     │
│  │  BPF program     │              │  Application handler │     │
│  │  (sensors, logic)│              │  (data processing)   │     │
│  └────────┬─────────┘              └──────────┬───────────┘     │
│           │                                   │                 │
└───────────┼───────────────────────────────────┼─────────────────┘
            │                                   │
    ┌───────▼───────┐   radio   ┌───────────────▼───────────┐
    │  Sonde Node   │◄─────────►│  Sonde Gateway            │
    │  (firmware)   │           │  (protocol, auth, admin)   │
    └───────────────┘           └───────────────────────────┘
```

### What the developer sees

The developer thinks in terms of **programs and data**, not nodes and protocols:

- *"My soil moisture program sent me a reading — process it and reply with updated thresholds."*
- *"My temperature alert program fired — log it and notify me."*

### What the developer does NOT deal with

- Node provisioning, keys, authentication
- Program distribution, chunked transfer
- Scheduling, retries, timeouts
- Protocol framing, CBOR encoding
- Battery monitoring, firmware versions

These are the gateway's responsibility, managed by operations staff (see [gateway-requirements.md](gateway-requirements.md)).

---

## 2  Transport

The gateway invokes the application handler for each data exchange. Two models are supported:

### 2.1  Process handler (simple)

The gateway executes a configured command for each `APP_DATA`, passing data via stdin/stdout.

```
Gateway                          Handler process
  │                                  │
  │──── stdin: request (CBOR) ──────►│
  │                                  │  (process data)
  │◄──── stdout: response (CBOR) ───│
  │                                  │
  │  [process exits]                 │
```

- One process per APP_DATA message.
- Stateless — each invocation is independent.
- Simplest integration: a Python script, a shell command, a compiled binary.

**⚠ OPEN:** Is per-invocation process spawning too expensive for high-frequency data? Should there be a persistent-process option too?

### 2.2  Long-running handler (streaming)

The gateway connects to a long-running handler process over a local socket (Unix domain socket or named pipe). Messages are length-prefixed CBOR, full-duplex.

```
┌──────────────────────────────────┐
│  Length (4 bytes, big-endian)    │
│  CBOR payload (Length bytes)    │
└──────────────────────────────────┘
```

- Handler stays running — no startup overhead per message.
- Can maintain state across invocations.
- Gateway reconnects if the handler restarts.

**⚠ OPEN:** Should the gateway connect to the handler, or should the handler connect to the gateway? Handler-connects-to-gateway is simpler for deployment (gateway is always running first).

---

## 3  Message types

The application API has only **4 message types** — two in each direction.

### 3.1  Gateway → Handler

| msg_type | Name | Description |
|---|---|---|
| `0x01` | `DATA` | A BPF program sent data. Handler must reply. |
| `0x02` | `EVENT` | Informational lifecycle event. No reply needed. |

### 3.2  Handler → Gateway

| msg_type | Name | Description |
|---|---|---|
| `0x81` | `DATA_REPLY` | Response to a `DATA` message. |
| `0x82` | `LOG` | Optional: handler wants to log a message through the gateway. |

---

## 4  Message definitions

### 4.1  DATA (Gateway → Handler)

Sent when a node's BPF program calls `send()`. The handler **must** reply with a `DATA_REPLY`.

| Field | CBOR key | Type | Description |
|---|---|---|---|
| `msg_type` | 1 | uint | `0x01` |
| `request_id` | 2 | uint | Correlation ID. Echo this in the reply. |
| `node_id` | 3 | tstr | Stable, opaque identifier for the node (assigned by gateway admin). |
| `program_hash` | 4 | bstr | Hash of the BPF program that sent this data. |
| `data` | 5 | bstr | The opaque blob from the BPF program's `send()` call. |
| `timestamp` | 6 | uint | Unix timestamp of reception (seconds). |

**Key design decisions:**

- `node_id` is an **opaque string** assigned by the gateway admin (e.g., `"greenhouse-sensor-3"`). The developer never sees HMAC keys or key_hints.
- `program_hash` tells the handler which BPF program produced this data, so it knows how to decode the blob.
- `data` is opaque to the gateway — the BPF program and handler define their own schema.

---

### 4.2  DATA_REPLY (Handler → Gateway)

Response to a `DATA` message. The gateway delivers the reply blob to the node via `APP_DATA_REPLY`.

| Field | CBOR key | Type | Description |
|---|---|---|---|
| `msg_type` | 1 | uint | `0x81` |
| `request_id` | 2 | uint | Must match the `DATA` message's `request_id`. |
| `data` | 3 | bstr | Opaque reply blob for the BPF program. Zero-length for acknowledgement only. |

**Timeout:** If the handler does not reply within a configurable timeout (default: 5 seconds), the gateway sends an `APP_DATA_REPLY` with a zero-length blob to the node.

---

### 4.3  EVENT (Gateway → Handler)

Informational notification about node lifecycle. No reply required.

| Field | CBOR key | Type | Description |
|---|---|---|---|
| `msg_type` | 1 | uint | `0x02` |
| `node_id` | 3 | tstr | Opaque node identifier. |
| `event_type` | 4 | tstr | Event name (see below). |
| `details` | 5 | map | Event-specific key-value data. |
| `timestamp` | 6 | uint | Unix timestamp. |

#### Event types

| Event | Description | Details |
|---|---|---|
| `"node_online"` | Node completed a wake cycle. | `battery_mv`, `firmware_abi_version` |
| `"program_updated"` | Node installed a new program. | `program_hash` |
| `"node_timeout"` | Node has not woken within expected interval. | `last_seen`, `expected_interval_s` |

---

### 4.4  LOG (Handler → Gateway)

Optional: the handler can emit log messages through the gateway's logging system.

| Field | CBOR key | Type | Description |
|---|---|---|---|
| `msg_type` | 1 | uint | `0x82` |
| `level` | 2 | tstr | `"debug"`, `"info"`, `"warn"`, `"error"` |
| `message` | 3 | tstr | Log message text. |

---

## 5  Process handler protocol

For the simple process-per-message model (§2.1):

1. Gateway spawns the configured handler command.
2. Gateway writes a single `DATA` message (CBOR) to stdin, then closes stdin.
3. Handler reads stdin, processes the data, writes a single `DATA_REPLY` (CBOR) to stdout, and exits.
4. Gateway reads stdout and sends the reply blob to the node.

Exit code 0 = success. Non-zero = the gateway logs the error and sends a zero-length reply.

**Example (Python):**

```python
import sys, cbor2

# Read DATA from stdin
request = cbor2.load(sys.stdin.buffer)

# Process the sensor data
sensor_data = request[5]  # data field
# ... application logic ...

# Write DATA_REPLY to stdout
cbor2.dump({1: 0x81, 2: request[2], 3: reply_blob}, sys.stdout.buffer)
```

---

## 6  Configuration

The gateway administrator configures which handler to invoke. This is an ops/admin concern, not a developer concern.

```yaml
# Example gateway configuration (format TBD)
application:
  handler: "/usr/local/bin/my-soil-app"
  mode: "process"          # or "streaming"
  timeout_s: 5
  socket: "/var/run/sonde/app.sock"  # for streaming mode
```

**⚠ OPEN:** Should a single gateway support multiple handlers (one per program_hash)? This would allow different applications for different BPF programs on the same gateway.

---

## 7  Open questions

| ID | Section | Question |
|---|---|---|
| A-1 | §2.1 | Is per-invocation process spawning too expensive for high-frequency data? |
| A-2 | §2.2 | Handler-connects-to-gateway or gateway-connects-to-handler? |
| A-3 | §6 | Multiple handlers per gateway (routed by program_hash)? |
