<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Azure Companion Design Specification

> **Document status:** Draft
> **Scope:** Internal design for the Azure companion container, bootstrap-state
> detection, bootstrap trigger behavior, and the Service Bus AMQP runtime bridge.
> The internal Azure provisioning workflow that creates the runtime certificate
> and broker resources is outside this document's scope.
> **Audience:** Implementers building the Azure companion crate and its
> deployment artifacts.
> **Related:** [azure-companion-requirements.md](azure-companion-requirements.md),
> [gateway-companion-api.md](gateway-companion-api.md),
> [gateway-design.md](gateway-design.md)

---

## 1  Overview

The Azure companion is a Rust workspace crate that runs in its own container and
talks to `sonde-gateway` over two local gateway-facing surfaces:

1. the admin gRPC API for bootstrap-only operator-visible actions, and
2. the local framed connector API for long-running runtime traffic.

The Azure companion now has two distinct responsibilities:

1. detect whether bootstrap has already completed,
2. invoke bootstrap when the required local provisioning artifacts are missing,
3. use the gateway admin API to display the device code during bootstrap, and
4. when bootstrap-complete state exists, bridge the gateway connector session to
   Azure Service Bus over AMQP.

The gateway-facing connector contract remains cloud-agnostic. Azure-specific
logic is confined to the Azure companion.

---

## 2  Repository layout

> **Requirements:** AZC-0100, AZC-0102

The implementation adds or updates the following artifacts:

| Artifact | Purpose |
|----------|---------|
| `crates/sonde-azure-companion/` | Rust crate containing the Azure companion binary. |
| `.github/docker/Dockerfile.azure-companion` | Dockerfile for the dedicated Azure companion image. |
| `deploy/azure-companion/bootstrap.sh` | Host/container bootstrap script that prepares the mounted state volume, evaluates bootstrap-complete state, and starts either bootstrap or runtime. |
| `deploy/azure-companion/entrypoint.sh` | In-container entrypoint that orchestrates bootstrap-state detection before starting the Rust binary. |

The long-running binary is named `sonde-azure-companion`.

---

## 3  Runtime architecture

> **Requirements:** AZC-0100, AZC-0101, AZC-0102, AZC-0301, AZC-0302, AZC-0303, AZC-0304, AZC-0305

### 3.1  Process model

The container runs a small shell entrypoint that performs filesystem and startup
orchestration and then execs the Rust binary. The split is intentional:

1. **Shell script** handles environment preparation, state-directory setup, and
   bootstrap-state detection orchestration.
2. **Rust binary** owns gateway admin gRPC communication, connector-socket
   runtime communication, bootstrap device flow, and broker integration.

This keeps the gateway-facing logic and Azure-facing runtime in typed Rust while
still allowing a small Alpine-oriented container image.

### 3.2  Mounted and configured inputs

The container expects the following runtime inputs:

| Input | Purpose |
|-------|---------|
| State volume | Persistent storage for local provisioning artifacts such as the runtime certificate PEM, private-key PEM, and service-principal metadata file. |
| Gateway admin socket | Local IPC path used by bootstrap to call `GatewayAdmin` RPCs such as `ShowModemDisplayMessage`. |
| Gateway connector socket | Local framed IPC path used by the long-running runtime after bootstrap succeeds. |
| Service Bus namespace | Explicit runtime configuration for the Azure Service Bus namespace. |
| Upstream queue name | Explicit runtime configuration for the queue that carries gateway-originated connector messages. |
| Downstream queue name | Explicit runtime configuration for the queue that carries cloud-originated desired-state messages. |

Bootstrap-complete state is defined by the combination of:

1. the required local provisioning artifacts in the state volume, and
2. the required explicit queue configuration.

The current runtime artifact shape is a companion-owned `service-principal.json`
file containing the Entra tenant ID, client ID, PEM certificate path, and PEM
private-key path, plus the referenced certificate and key files in the mounted
state directory.

### 3.3  Bootstrap-state decision

Startup follows this decision:

1. Ensure the mounted state directory exists and is writable.
2. Check whether the required local provisioning artifacts exist.
3. Check whether the required Service Bus namespace and queue configuration are present.
4. If both are present, skip bootstrap and start `run`.
5. Otherwise, start `bootstrap-auth`.

The internal Azure provisioning workflow that consumes the bootstrap token and
produces the runtime artifacts is outside this document's scope. The Azure
companion only defines the startup decision and the interfaces around it.

---

## 4  Bootstrap flow

> **Requirements:** AZC-0200, AZC-0201, AZC-0202, AZC-0203, AZC-0204, AZC-0300

### 4.1  Bootstrap trigger

Bootstrap is entered only when bootstrap-complete state is absent. This differs
from the earlier draft, which always re-entered device-code login on restart.

### 4.2  Device-code login sequence

When bootstrap is required, the Azure companion performs this sequence:

1. Invoke `sonde-azure-companion bootstrap-auth`.
2. Inside Rust, construct a Microsoft device-flow client from explicit
   environment-provided client ID and scopes.
3. Request a device code from Microsoft's device authorization endpoint.
4. Log the verification URI to stdout/stderr for operator visibility.
5. Call the gateway admin `ShowModemDisplayMessage` RPC with a short prompt plus
   the exact device code.
6. Poll the token endpoint until the operator completes device auth or the flow
   fails.
7. Hand off to the out-of-scope provisioning workflow that creates the runtime
   certificate and broker resources.
8. Return success only after bootstrap-complete state has been established.

The modem display shows only the short prompt plus the device code; the full
verification URL remains in stdout/stderr logs.

### 4.3  Display failure handling

If the display update fails because the gateway rejects the transient display
request or no modem transport is available, bootstrap exits immediately with a
non-zero status. It does not continue to a console-only fallback.

---

## 5  Rust binary interface

> **Requirements:** AZC-0100, AZC-0102, AZC-0201, AZC-0202, AZC-0300, AZC-0301, AZC-0302, AZC-0304, AZC-0305

The `sonde-azure-companion` binary exposes three modes:

1. **`run`** — default long-running runtime mode. It connects to the gateway
   connector socket and bridges connector traffic to Azure Service Bus.
2. **`bootstrap-auth`** — performs Microsoft OAuth device flow in Rust, logs the
   verification URI, requests the modem display update, waits for operator
   completion, and then returns control to the bootstrap workflow.
3. **`display-message`** — helper mode used by bootstrap logic to call the
   gateway admin `ShowModemDisplayMessage` RPC with 1 to 4 lines of text.

The companion receives explicit runtime configuration for the Service Bus
namespace and queue names rather than inferring deployment-specific defaults.

---

## 6  Gateway integration

> **Requirements:** AZC-0202, AZC-0203, AZC-0300, AZC-0301

### 6.1  Bootstrap admin client

Bootstrap helper paths connect to the gateway admin socket. They use the
published `GatewayAdmin` contract for operator-visible bootstrap actions and do
not use the connector API for transient display requests.

### 6.2  Shared display path

The gateway-side `ShowModemDisplayMessage` admin RPC gives the Azure companion no
special display privileges:

1. BLE pairing still preempts transient display requests.
2. Line-count validation remains 1 to 4 lines.
3. The gateway retains rendering, display ownership, and banner-restore logic.
4. The Azure companion cannot issue raw modem commands or upload framebuffers.

### 6.3  Runtime connector client

After bootstrap succeeds, the `run` mode connects to the gateway connector
socket and keeps a single long-lived connector session open. The runtime treats
the framed connector API as its normal control-plane integration surface and
does not depend on a separate companion runtime socket.

---

## 7  Azure broker transport architecture

> **Requirements:** AZC-0302, AZC-0303, AZC-0304, AZC-0305, AZC-0306, AZC-0307, AZC-0308, AZC-0309

### 7.1  Transport abstraction boundary

The runtime is divided into two internal responsibilities:

1. **Gateway connector side** — reads and writes framed Sonde connector payloads
   on the local connector socket.
2. **Broker transport side** — publishes and receives opaque payload bytes on the
   external control-plane transport.

These responsibilities are separated by an internal transport abstraction
boundary so the gateway-facing logic does not depend directly on one Azure SDK
crate. `azservicebus` is the first required broker transport implementation.

### 7.2  Azure Service Bus runtime

The Azure Service Bus transport implementation uses AMQP to connect to:

1. one upstream queue for gateway-originated connector messages, and
2. one downstream queue for desired-state requests coming from the control plane.

All gateway-originated connector message types that travel upstream through the
connector session — including actual-state, app-data, and connector-health
messages — share the upstream queue. The downstream queue is reserved for
desired-state messages destined for the gateway.

### 7.3  Azure authentication

Normal runtime starts use the provisioned certificate and private-key material
from the bootstrap-complete state and authenticate to Azure as an Entra
application / service principal. Interactive device auth is bootstrap-only and
is not part of normal runtime operation.

### 7.4  Transparent message bodies

The Service Bus message body carries the raw Sonde connector payload bytes
unchanged. The Azure companion may attach minimal broker metadata in message
properties for diagnostics or routing hints, but the broker representation does
not replace the connector payload with an Azure-specific schema.

### 7.5  Downstream settlement

For downstream desired-state requests, the Azure companion settles the Service
Bus message as successful only after the raw connector payload has been written
successfully to the local connector socket. This design intentionally stops at
local handoff; the gateway connector protocol does not grow a separate
round-trip acknowledgement just for Azure.

### 7.6  Fault handling

Detected failures on either side of the bridge are surfaced rather than masked:

1. upstream publish failures,
2. downstream receive failures,
3. downstream settlement failures, and
4. local connector write failures.

The runtime may reconnect or exit, but it must not silently claim success after
a detected bridge failure.
