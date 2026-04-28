<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Azure Companion Design Specification

> **Document status:** Draft
> **Scope:** Internal design for the current Azure companion slice: container
> packaging, bootstrap scripts, gateway admin/connector integration, and Azure
> device-code login.
> **Audience:** Implementers building the new Azure companion crate and its deployment artifacts.
> **Related:** [azure-companion-requirements.md](azure-companion-requirements.md), [gateway-companion-api.md](gateway-companion-api.md), [gateway-design.md](gateway-design.md)

---

## 1  Overview

The Azure companion is a new Rust workspace crate that runs in its own Docker
container and talks to `sonde-gateway` over two local gateway-facing surfaces:
the admin gRPC API for bootstrap-only operator-visible actions and the local
framed connector API for long-running runtime traffic. This slice does not yet
provision Azure resources or bridge data into Azure services. Its responsibility
is limited to:

1. starting in a dedicated container,
2. preparing a mounted persistent state volume for later provisioning slices,
3. obtaining Azure authentication via device-code login,
4. showing a short prompt plus the device code on the modem display through the
   gateway admin API, and
5. starting a minimal long-running connector process after bootstrap completes.

Terraform provisioning, managed-identity creation, gateway configuration
generation, and Azure-side message forwarding are deferred to later issues.

---

## 2  Repository layout

> **Requirements:** AZC-0100, AZC-0102

The implementation adds the following artifacts:

| Artifact | Purpose |
|----------|---------|
| `crates/sonde-azure-companion/` | New Rust crate containing the Azure companion binary. |
| `.github/docker/Dockerfile.azure-companion` | Dockerfile for the dedicated Azure companion image. |
| `deploy/azure-companion/bootstrap.sh` | Host/container bootstrap script that prepares the mounted state volume and runs the Rust-owned device-auth bootstrap before normal runtime starts. |
| `deploy/azure-companion/entrypoint.sh` | In-container entrypoint that orchestrates bootstrap before starting the long-running process. |

The long-running binary is named `sonde-azure-companion`.

---

## 3  Runtime architecture

> **Requirements:** AZC-0100, AZC-0101, AZC-0300

### 3.1  Process model

The container runs a small shell entrypoint that performs bootstrap orchestration
and then execs the Rust binary. The split is intentional:

1. **Shell script** handles environment preparation, filesystem setup, and
   container-vs-host orchestration.
2. **Rust binary** owns gateway admin gRPC communication for bootstrap,
   connector-socket runtime communication, Microsoft device flow, and the future
   Azure-integration runtime.

This keeps the gateway-facing logic and the Azure device-code flow in typed
Rust, which removes the Azure CLI dependency and allows an Alpine runtime
image.

### 3.2  Mounted inputs

The container expects two mounted host resources:

| Mount | Purpose |
|-------|---------|
| State volume | Persistent storage reserved for later managed-identity bootstrap output and other local provisioning artifacts. |
| Gateway admin socket | Local IPC path used by bootstrap to call `GatewayAdmin` RPCs such as `ShowModemDisplayMessage`. |
| Gateway connector socket | Local framed IPC path used by the long-running runtime after bootstrap succeeds. |

The current slice prepares the state volume but does not persist Azure access
tokens there. The image itself is replaceable.

---

## 4  Bootstrap flow

> **Requirements:** AZC-0101, AZC-0102, AZC-0200, AZC-0201, AZC-0202, AZC-0203, AZC-0204

### 4.1  Startup decision

At container start, `entrypoint.sh` delegates to `bootstrap.sh`. The in-container
bootstrap path creates the mounted state directory and then invokes the Rust
binary's `bootstrap-auth` mode before starting the normal long-running process.
This slice does not inspect the state directory to skip login on restart.

### 4.2  Device-code login sequence

The first-run bootstrap sequence is:

1. Ensure the mounted state directory exists and is writable.
2. Invoke `sonde-azure-companion bootstrap-auth`.
3. Inside Rust, construct a Microsoft device-flow client from explicit
   environment-provided client ID and scopes.
4. Request a device code from Microsoft's device authorization endpoint.
5. Log the verification URI to stdout/stderr for operator visibility.
6. Call the gateway admin `ShowModemDisplayMessage` RPC with a short prompt plus
   the exact device code.
7. Poll the token endpoint until the operator completes device auth or the flow
   fails.
8. Discard the short-lived token and exec the long-running `run` mode.

The full Azure verification URL remains in stdout/stderr logs; the modem display
shows only the short prompt plus the device code.

### 4.3  Display failure handling

If step 6 fails because the gateway rejects the display request or no modem
transport is available, the bootstrap flow exits immediately with a non-zero
status. It does not continue to a console-only fallback. This preserves the
headless operator workflow required by the discovery review.

---

## 5  Rust binary interface

> **Requirements:** AZC-0100, AZC-0102, AZC-0201, AZC-0202, AZC-0300, AZC-0301

The initial `sonde-azure-companion` binary exposes three modes:

1. **`run`** — default long-running runtime mode. In this initial slice it
   establishes gateway connector connectivity and remains ready for later Azure
   integration work.
2. **`bootstrap-auth`** — performs Microsoft OAuth device flow in Rust, logs the
   verification URI, requests the modem display update, waits for operator
   completion, and discards the resulting token.
3. **`display-message`** — helper mode used by the bootstrap logic to call the
   gateway admin `ShowModemDisplayMessage` RPC with 1 to 4 lines of text.

The helper modes keep gateway IPC and OAuth error handling out of shell-script
string munging and ensure the same Rust client stack is used during bootstrap
and later runtime work.

`bootstrap-auth` requires the caller to provide the Azure device-flow client ID
and scopes explicitly through environment variables or CLI flags. This slice
does not guess Azure application defaults.

---

## 6  Gateway integration

> **Requirements:** AZC-0202, AZC-0203, AZC-0300

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

---

### 6.3  Runtime connector client

After bootstrap succeeds, the `run` mode connects to the gateway connector
socket and keeps a single long-lived connector session open. The runtime treats
the framed connector API as its normal control-plane integration surface and
does not depend on a separate companion runtime socket.

---

## 7  Long-running process behavior in this slice

> **Requirements:** AZC-0102, AZC-0301

After bootstrap succeeds, the `run` mode starts and validates gateway connector
connectivity. This first slice does not yet forward events to Azure services or
pull cloud-issued commands. Those behaviors are deferred to the later Terraform
and cloud-integration issues, but the long-running process already uses the
connector surface rather than a legacy companion runtime API.

