<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Gateway Companion API

> **Document status:** Draft  
> **Scope:** The local gRPC integration API between `sonde-gateway` and companion processes that subscribe to gateway events and issue node-targeted commands.  
> **Audience:** Developers building sidecar or bridge processes alongside the gateway.  
> **Related:** [gateway-requirements.md](gateway-requirements.md), [gateway-design.md](gateway-design.md), [gateway-api.md](gateway-api.md)

---

## 1  Overview

The companion API is a **separate integration surface** from both:

1. **`GatewayAdmin`** — the operator-facing local admin API.
2. **`gateway-api.md`** — the handler stdin/stdout CBOR data-plane API.

A companion process uses this API when it needs to:

- subscribe to live gateway events such as node check-ins and payload arrivals, and
- issue node-targeted commands through the gateway's existing control path.

Example use case: a bridge process authenticates to Azure, forwards `node_checkin` and `node_payload` events to a Service Bus queue, reads cloud-issued commands, and relays them to the gateway.

The companion API is **informational and control-only**. It does not replace the handler data-plane API and does not provide a reply channel for node payloads.

---

## 2  Transport and lifecycle

### 2.1  Transport

The gateway exposes the companion API over **local-only gRPC** on a dedicated endpoint:

- **Unix/macOS:** Unix domain socket, default `/var/run/sonde/companion.sock`
- **Windows:** named pipe, default `\\.\pipe\sonde-companion`

No TCP listener is exposed in v1.

The companion API uses a dedicated protobuf service and companion-specific message types so it can evolve independently from `GatewayAdmin`.

### 2.2  Event stream lifecycle

Companion clients subscribe with a server-streaming RPC:

1. Client connects to the companion socket.
2. Client calls `StreamEvents`.
3. Gateway emits future `node_checkin` and `node_payload` events on the stream.
4. If the client disconnects, the stream ends immediately.
5. If the client falls behind the gateway's bounded event buffer, the gateway terminates the stream with `RESOURCE_EXHAUSTED`.

The stream is **live best-effort only**:

- events produced before subscription are not replayed;
- reconnecting clients receive only newly produced events; and
- durability, replay, and downstream queuing are the companion process's responsibility.

---

## 3  Protobuf contract

### 3.1  Service definition

```protobuf
service GatewayCompanion {
    rpc StreamEvents(CompanionStreamEventsRequest) returns (stream CompanionEvent);

    rpc ListNodes(CompanionListNodesRequest) returns (CompanionListNodesResponse);
    rpc GetNode(CompanionGetNodeRequest) returns (CompanionNodeInfo);
    rpc AssignProgram(CompanionAssignProgramRequest) returns (CompanionEmpty);
    rpc SetSchedule(CompanionSetScheduleRequest) returns (CompanionEmpty);
    rpc QueueReboot(CompanionQueueRebootRequest) returns (CompanionEmpty);
    rpc QueueEphemeral(CompanionQueueEphemeralRequest) returns (CompanionEmpty);
    rpc GetNodeStatus(CompanionGetNodeStatusRequest) returns (CompanionNodeStatus);
}
```

The companion service intentionally omits operator-only workflows such as node registration/removal, program ingestion/removal, state export/import, modem control, BLE pairing, and handler configuration.

### 3.2  `node_checkin`

```protobuf
message CompanionEvent {
    oneof event {
        CompanionNodeCheckIn node_checkin = 1;
        CompanionNodePayload node_payload = 2;
    }
}

message CompanionNodeCheckIn {
    string node_id = 1;
    bytes current_program_hash = 2;
    optional bytes assigned_program_hash = 3;
    uint32 battery_mv = 4;
    uint32 firmware_abi_version = 5;
    string firmware_version = 6;
    uint64 timestamp_ms = 7;
}
```

The gateway emits `node_checkin` after it accepts an authenticated `WAKE` and updates the node's latest-known state.

### 3.3  `node_payload`

```protobuf
enum CompanionPayloadOrigin {
    COMPANION_PAYLOAD_ORIGIN_UNSPECIFIED = 0;
    COMPANION_PAYLOAD_ORIGIN_APP_DATA = 1;
    COMPANION_PAYLOAD_ORIGIN_WAKE_BLOB = 2;
}

message CompanionNodePayload {
    string node_id = 1;
    bytes program_hash = 2;
    bytes payload = 3;
    uint64 timestamp_ms = 4;
    CompanionPayloadOrigin payload_origin = 5;
}
```

The gateway emits `node_payload` for:

- `APP_DATA { blob }`, with `payload_origin = APP_DATA`
- `WAKE { blob }`, with `payload_origin = WAKE_BLOB`

For a `WAKE` carrying a blob, the gateway emits `node_checkin` before the corresponding `node_payload`.

### 3.4  Command and query RPCs

```protobuf
message CompanionEmpty {}
message CompanionStreamEventsRequest {}
message CompanionListNodesRequest {}
message CompanionListNodesResponse { repeated CompanionNodeInfo nodes = 1; }
message CompanionGetNodeRequest { string node_id = 1; }
message CompanionAssignProgramRequest { string node_id = 1; bytes program_hash = 2; }
message CompanionSetScheduleRequest { string node_id = 1; uint32 interval_s = 2; }
message CompanionQueueRebootRequest { string node_id = 1; }
message CompanionQueueEphemeralRequest { string node_id = 1; bytes program_hash = 2; }
message CompanionGetNodeStatusRequest { string node_id = 1; }

message CompanionNodeInfo {
    string node_id = 1;
    uint32 key_hint = 2;
    optional bytes assigned_program_hash = 3;
    optional bytes current_program_hash = 4;
    optional uint32 last_battery_mv = 5;
    optional uint32 last_firmware_abi_version = 6;
    optional uint64 last_seen_ms = 7;
    optional uint32 schedule_interval_s = 8;
}

message CompanionNodeStatus {
    string node_id = 1;
    bytes current_program_hash = 2;
    optional uint32 battery_mv = 3;
    optional uint32 firmware_abi_version = 4;
    optional uint64 last_seen_ms = 5;
    bool has_active_session = 6;
}
```

The companion API's command RPCs reuse the gateway's existing command semantics:

- `AssignProgram` changes the assigned resident program.
- `SetSchedule` queues `UPDATE_SCHEDULE` for the next `WAKE`.
- `QueueReboot` queues `REBOOT` for the next `WAKE`.
- `QueueEphemeral` queues `RUN_EPHEMERAL` for the next `WAKE`.

These operations act on the same gateway state as the corresponding admin operations; the companion API does not define a separate command pipeline.

---

## 4  Behavioral notes

1. `node_payload` is informational only. Companion clients do not reply to payload events; node replies continue to use the handler flow defined in [gateway-api.md](gateway-api.md).
2. Message ordering is guaranteed only within a single live stream as produced by the gateway runtime. There is no replay cursor, durable offset, or exactly-once delivery guarantee.
3. Companion clients are expected to provide their own persistence and retry behavior when forwarding events to external systems.
