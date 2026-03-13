<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# USB Pairing Protocol Specification

> **Document status:** Draft
> **Scope:** Wire-level protocol between the `sonde-admin` CLI tool and a USB-attached sonde node for key provisioning and factory reset.
> **Audience:** Implementers building the `sonde-admin` USB pairing commands or the node firmware pairing handler.
> **Related:** [modem-protocol.md](modem-protocol.md), [security.md](security.md), [node-requirements.md](node-requirements.md), [gateway-design.md § 13](gateway-design.md)

---

This document specifies the serial protocol used between the `sonde-admin` CLI tool and a sonde node connected via USB for key provisioning (pairing) and factory reset. The protocol is independent of the radio modem — pairing is a direct host-to-node USB-CDC connection.

---

## 1  Overview

### 1.1  Design principles

1. **Physical security** — USB access implies authorization. No authentication on the pairing channel.
2. **Atomic pairing** — key provisioning writes key_hint + PSK + magic bytes atomically. If interrupted, the key partition remains in its previous state. Factory reset erases partitions sequentially; a partial reset leaves the node unpaired (safe — re-pair to recover).
3. **Shared framing** — reuses the modem protocol's length-prefixed binary format and the `sonde-protocol` codec.
4. **Forward compatibility** — unknown message types are silently discarded.
5. **Simple state machine** — the node is either paired or unpaired. No multi-step negotiation.

---

## 2  Serial framing

### 2.1  Frame envelope

The protocol reuses the same length-prefixed binary framing as the [modem protocol](modem-protocol.md):

```
┌───────────┬──────────┬──────────────────────────────────┐
│ LEN (2B)  │ TYPE(1B) │ BODY (0..511 bytes)              │
│ BE u16    │          │                                  │
└───────────┴──────────┴──────────────────────────────────┘
```

| Field | Size | Description |
|-------|------|-------------|
| `LEN` | 2 bytes, big-endian u16 | Length of TYPE + BODY (min 1, max 512) |
| `TYPE` | 1 byte | Message type discriminator |
| `BODY` | 0–511 bytes | Type-specific payload |

No CRC — USB-CDC provides transport-layer integrity.

The `FrameDecoder` and `encode_modem_frame` from `sonde-protocol` are reused directly. Pairing messages use the `ModemMessage::Unknown { msg_type, body }` variant for encoding and decoding — the frame codec handles any type value transparently.

### 2.2  Receiver behavior

Both sides use `sonde_protocol::modem::FrameDecoder`. The decoder reports `EmptyFrame` for `LEN` = 0 and `FrameTooLarge` for `LEN` > 512. These are non-fatal — the decoder clears its buffer and is ready for the next frame. Receivers should:

- `EmptyFrame` → log and continue reading.
- `FrameTooLarge` → log the error. Host: close and re-open port, wait for `PAIRING_READY`. Node: continue reading (decoder buffer already cleared).
- Unknown `TYPE` → silently discard (forward compatibility).

### 2.3  Synchronization recovery

If the host receives garbled data (e.g., from a partial connection or power glitch), it should drain the serial buffer, close and re-open the port, and wait for a fresh `PAIRING_READY`. The node re-sends `PAIRING_READY` on USB-CDC reconnection.

---

## 3  Message types

Message type values are allocated from reserved ranges in the [modem protocol](modem-protocol.md) § 8.3:

```
0x10–0x1F: Host → Node (pairing commands)
0x90–0x9F: Node → Host (pairing responses)
```

### 3.1  Host → Node

| Type | Name | Body |
|------|------|------|
| 0x10 | `PAIR_REQUEST` | key_hint (2B BE) + psk (32B) = 34 bytes |
| 0x11 | `RESET_REQUEST` | Empty (0 bytes) |
| 0x12 | `IDENTITY_REQUEST` | Empty (0 bytes) |

### 3.2  Node → Host

| Type | Name | Body |
|------|------|------|
| 0x90 | `PAIR_ACK` | status (1B) |
| 0x91 | `RESET_ACK` | status (1B) |
| 0x92 | `IDENTITY_RESPONSE` | status (1B) + conditional fields |
| 0x9F | `PAIRING_READY` | firmware_version (4B BE) |

---

## 4  Message definitions

### 4.1  PAIR_REQUEST (Host → Node)

Provisions a PSK on the node. The host generates the key_hint and PSK; the node stores them in its key partition.

| Offset | Size | Field | Description |
|--------|------|-------|-------------|
| 0 | 2 | key_hint | Big-endian u16 |
| 2 | 32 | psk | 256-bit pre-shared key |

**Total body: 34 bytes.** The node MUST reject the request if the body is not exactly 34 bytes.

### 4.2  PAIR_ACK (Node → Host)

Response to `PAIR_REQUEST`.

| Offset | Size | Field | Values |
|--------|------|-------|--------|
| 0 | 1 | status | 0x00 = success, 0x01 = already paired, 0x02 = write error |

### 4.3  RESET_REQUEST (Host → Node)

Triggers a factory reset: erases PSK, programs, map data, and schedule. Body is empty.

### 4.4  RESET_ACK (Node → Host)

Response to `RESET_REQUEST`.

| Offset | Size | Field | Values |
|--------|------|-------|--------|
| 0 | 1 | status | 0x00 = success, 0x02 = erase error |

### 4.5  IDENTITY_REQUEST (Host → Node)

Queries the node's current pairing state. Body is empty.

### 4.6  IDENTITY_RESPONSE (Node → Host)

Response to `IDENTITY_REQUEST`.

| Offset | Size | Field | Values |
|--------|------|-------|--------|
| 0 | 1 | status | 0x00 = paired, 0x01 = unpaired |
| 1 | 2 | key_hint | Big-endian u16 (present only if status = 0x00) |

**Body: 1 byte (unpaired) or 3 bytes (paired).**

### 4.7  PAIRING_READY (Node → Host)

Sent by the node when it enters pairing mode. Analogous to `MODEM_READY` in the modem protocol.

| Offset | Size | Field | Description |
|--------|------|-------|-------------|
| 0 | 4 | firmware_version | Big-endian u32 |

---

## 5  Message flows

### 5.1  Startup

When the host opens the serial port, the node sends `PAIRING_READY`. The host waits for this before sending any commands.

```
Host                                Node
  │                                   │
  │── [open serial port] ────────────►│
  │                                   │
  │            ◄── PAIRING_READY ──   │
  │                                   │
  │  ════ ready for commands ════     │
```

### 5.2  Pairing

```
Host                                Node
  │                                   │
  │            ◄── PAIRING_READY ──   │
  │                                   │
  │── IDENTITY_REQUEST ──────────►    │  (check current state)
  │            ◄── IDENTITY_RESPONSE  │
  │                                   │
  │── PAIR_REQUEST(key_hint, psk) ►   │  (provision key)
  │            ◄── PAIR_ACK(0x00) ──  │  (success)
  │                                   │
  │  (node reboots into normal mode)  │
```

### 5.3  Factory reset

```
Host                                Node
  │                                   │
  │            ◄── PAIRING_READY ──   │
  │                                   │
  │── IDENTITY_REQUEST ──────────►    │  (get key_hint for gateway deregistration)
  │            ◄── IDENTITY_RESPONSE  │
  │                                   │
  │── RESET_REQUEST ─────────────►    │  (erase all persistent state)
  │            ◄── RESET_ACK(0x00) ── │  (success)
  │                                   │
  │  (node reboots into pairing mode) │
```

### 5.4  Error recovery

If any command fails (ACK status ≠ 0x00 or timeout), the host should close the serial port and report the error. No retry is attempted — the user must re-run the command.

```
Host                                Node
  │                                   │
  │── PAIR_REQUEST ──────────────►    │
  │            ◄── PAIR_ACK(0x01) ──  │  (already paired)
  │                                   │
  │  (host closes port, reports error)│
```

---

## 6  Error handling

### 6.1  Invalid frames

| Condition | Receiver behavior |
|-----------|-------------------|
| `len` = 0 | Silently discard. |
| `len` > 512 | Framing error. Drain buffer, wait for `PAIRING_READY`. |
| Unknown `type` | Silently discard (forward compatibility). |
| `PAIR_REQUEST` body ≠ 34 bytes | Node silently discards. |

### 6.2  Missing responses

If the host does not receive the expected ACK within the timeout (§7), the operation fails. The host should close the port and report an error. Unlike the modem protocol, there is no automatic retry or `RESET`-based recovery — pairing is a one-shot human-initiated operation.

### 6.3  USB disconnection

- **Node side:** Retains current state (paired or unpaired). Re-enters pairing mode and sends `PAIRING_READY` on next USB connection.
- **Host side:** Detects serial port closure, aborts the in-progress operation. If pairing had succeeded on the node but the gRPC registration was not yet attempted, the user must factory-reset the node and re-pair.

### 6.4  Unsolicited messages

The only unsolicited message is `PAIRING_READY`, which the node may re-send on USB reconnection. The host should accept the first `PAIRING_READY` and ignore subsequent ones within a session.

---

## 7  Timing

| Event | Timeout | Action on timeout |
|-------|---------|-------------------|
| `PAIRING_READY` after port open | 5 seconds | Abort with error ("node not responding — is it in pairing mode?"). |
| `PAIR_ACK` after `PAIR_REQUEST` | 5 seconds | Abort with error ("pairing failed — no response from node"). |
| `RESET_ACK` after `RESET_REQUEST` | 5 seconds | Abort with error ("factory reset failed — no response from node"). |
| `IDENTITY_RESPONSE` after `IDENTITY_REQUEST` | 2 seconds | Abort with error ("identity query failed"). |

All timeouts are from the moment the request frame is fully written to the serial port.

---

## 8  Protocol evolution

### 8.1  Forward compatibility

Both sides MUST silently discard frames with unrecognized `type` values. This allows the host tool and node firmware to be upgraded independently.

### 8.2  Version detection

The `firmware_version` field in `PAIRING_READY` allows the host to detect the node firmware version and adjust behavior if needed (e.g., skip `IDENTITY_REQUEST` if the firmware predates that message type).

### 8.3  Reserved type ranges

| Range | Purpose |
|-------|---------|
| 0x10 – 0x12 | Pairing commands (PAIR_REQUEST, RESET_REQUEST, IDENTITY_REQUEST) |
| 0x13 – 0x1F | Reserved for future host → node pairing commands |
| 0x90 – 0x92 | Pairing responses (PAIR_ACK, RESET_ACK, IDENTITY_RESPONSE) |
| 0x93 – 0x9E | Reserved for future node → host pairing messages |
| 0x9F | PAIRING_READY |

These ranges are a subset of the modem protocol's reserved ranges (0x10–0x7F, 0x90–0xFF) and do not conflict with core modem message types (0x01–0x0F, 0x81–0x8F).

---

## 9  Security considerations

- **Physical access required** — USB pairing requires physical access to the node. The USB cable is the secure channel.
- **PSK never sent over radio** — the PSK is only transmitted over USB, never over ESP-NOW.
- **No authentication on USB channel** — physical access to the USB port implies authorization.
- **key_hint is not secret** — it is a lookup optimization transmitted in the clear in WAKE messages.
- **Atomic writes** — the node's `pair()` method writes key_hint + PSK + magic bytes atomically. If interrupted, the key partition remains in its previous state (unpaired or previously paired).

---

## 10  Host-side operational flows

These flows describe the complete end-to-end operations performed by `sonde-admin`, including both the USB serial protocol and the gateway gRPC calls.

### 10.1  Pairing flow (`sonde-admin node pair`)

The `node_id` is an admin-assigned identifier supplied by the user as a CLI argument (e.g., `sonde-admin node pair --usb COM3 --node-id greenhouse-01`). It is not stored on or generated by the node.

1. Open serial port at 115200 baud (8N1).
2. Wait for `PAIRING_READY` (timeout 5s).
3. Send `IDENTITY_REQUEST`. If node is already paired, abort with error.
4. Generate 256-bit PSK from OS CSPRNG.
5. Compute key_hint: lower 16 bits of SHA-256(PSK).
6. Send `PAIR_REQUEST(key_hint, psk)`.
7. Wait for `PAIR_ACK` (timeout 5s). If status ≠ 0x00, abort.
8. Call `RegisterNode` gRPC on the gateway with node_id, key_hint, and PSK.
9. If gRPC fails:
   - Send `RESET_REQUEST` to roll back the node.
   - Wait for `RESET_ACK` (timeout 5s).
   - Abort with error.
10. Print node_id, key_hint, and confirmation.

### 10.2  Factory reset flow (`sonde-admin node reset`)

The `node_id` is supplied by the user as a CLI argument (e.g., `sonde-admin node reset --usb COM3 --node-id greenhouse-01`). The gateway's `RemoveNode` RPC takes `node_id`, not `key_hint`.

1. Open serial port at 115200 baud (8N1).
2. Wait for `PAIRING_READY` (timeout 5s).
3. Send `IDENTITY_REQUEST` to confirm the node is currently paired.
4. Send `RESET_REQUEST`.
5. Wait for `RESET_ACK` (timeout 5s). If status ≠ 0x00, abort.
6. If node was paired (identity status = 0x00) and `node_id` was provided:
   - Call `RemoveNode(node_id)` gRPC on the gateway.
7. Print confirmation.

---

## 11  Node firmware behavior

The node enters pairing mode under one of two conditions:

1. **Boot with no PSK** — magic bytes absent in key partition → pairing mode immediately.
2. **USB-CDC connection detected during boot** — firmware checks for USB before starting the wake cycle.

In pairing mode the node:
- Does NOT start the radio (ESP-NOW is not initialized).
- Listens on USB-CDC for pairing commands.
- Sends `PAIRING_READY` on connection.
- Processes `PAIR_REQUEST`, `RESET_REQUEST`, and `IDENTITY_REQUEST`.
- After successful pairing, reboots into normal operation.
- After successful factory reset, reboots into pairing mode (unpaired).

---

## 12  Constants

| Constant | Value | Description |
|----------|-------|-------------|
| Baud rate | 115200 | Serial port speed (8N1) |
| `PAIR_REQUEST` type | 0x10 | Host → Node |
| `RESET_REQUEST` type | 0x11 | Host → Node |
| `IDENTITY_REQUEST` type | 0x12 | Host → Node |
| `PAIR_ACK` type | 0x90 | Node → Host |
| `RESET_ACK` type | 0x91 | Node → Host |
| `IDENTITY_RESPONSE` type | 0x92 | Node → Host |
| `PAIRING_READY` type | 0x9F | Node → Host |
| `PAIR_REQUEST` body size | 34 bytes | 2B key_hint + 32B PSK |
| Status: success | 0x00 | |
| Status: already paired | 0x01 | In `PAIR_ACK` |
| Status: unpaired | 0x01 | In `IDENTITY_RESPONSE` |
| Status: write/erase error | 0x02 | In `PAIR_ACK` / `RESET_ACK` |
| PSK size | 32 bytes | 256-bit pre-shared key |
| key_hint size | 2 bytes | Big-endian u16 |
