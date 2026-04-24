<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Azure Companion Design Specification

> **Document status:** Draft
> **Scope:** Internal design for the initial Azure companion slice: container packaging, bootstrap scripts, gateway companion integration, and Azure device-code login.
> **Audience:** Implementers building the new Azure companion crate and its deployment artifacts.
> **Related:** [azure-companion-requirements.md](azure-companion-requirements.md), [gateway-companion-api.md](gateway-companion-api.md), [gateway-design.md](gateway-design.md)

---

## 1  Overview

The Azure companion is a new Rust workspace crate that runs in its own Docker
container and talks to `sonde-gateway` over the local companion gRPC API. This
initial slice does not yet provision Azure resources or bridge data into Azure
services. Its responsibility is limited to:

1. starting in a dedicated container,
2. detecting first-run bootstrap from a mounted auth state volume,
3. obtaining Azure authentication via device-code login,
4. showing a short prompt plus the device code on the modem display through the
   gateway companion API, and
5. starting a minimal long-running companion process after bootstrap completes.

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
| `deploy/azure-companion/bootstrap.sh` | Host/container bootstrap script that prepares the mounted auth state volume and runs first-login bootstrap when needed. |
| `deploy/azure-companion/entrypoint.sh` | In-container entrypoint that orchestrates bootstrap before starting the long-running process. |

The long-running binary is named `sonde-azure-companion`.

---

## 3  Runtime architecture

> **Requirements:** AZC-0100, AZC-0101, AZC-0300

### 3.1  Process model

The container runs a small shell entrypoint that performs bootstrap orchestration
and then execs the Rust binary. The split is intentional:

1. **Shell script** handles environment preparation, filesystem setup, and Azure
   CLI invocation.
2. **Rust binary** owns gateway companion gRPC communication and the future
   Azure-integration runtime.

This keeps the gateway-facing logic in typed Rust while still using the Azure
CLI directly for the initial device-code login flow required by issue #771.

### 3.2  Mounted inputs

The container expects two mounted host resources:

| Mount | Purpose |
|-------|---------|
| Auth state volume | Persistent storage for Azure CLI authentication state. |
| Gateway companion socket | Local IPC path used to call `GatewayCompanion` RPCs, including `ShowModemDisplayMessage`. |

The auth state volume is the only persistent state required by this initial
slice. The image itself is replaceable.

---

## 4  Bootstrap flow

> **Requirements:** AZC-0101, AZC-0102, AZC-0200, AZC-0201, AZC-0202, AZC-0203, AZC-0204

### 4.1  Startup decision

At container start, `entrypoint.sh` checks the mounted auth state volume for
usable Azure authentication state.

- If usable state is present, the script skips device-code login and starts the
  long-running Rust process.
- If usable state is absent, the script runs `bootstrap.sh` to perform first-run
  login.

### 4.2  Device-code login sequence

The first-run bootstrap sequence is:

1. Ensure the auth state directory exists and is writable.
2. Invoke Azure CLI device-code login with `az login --use-device-code`.
3. Capture the device-code output emitted by Azure CLI.
4. Extract the device code from that output.
5. Call `sonde-azure-companion display-message <prompt> <code>` so the Rust
   binary connects to the gateway companion socket and issues
   `ShowModemDisplayMessage`.
6. Wait for Azure CLI to complete successfully.
7. Leave the resulting Azure authentication state in the mounted auth state
   volume.
8. Start the long-running Rust process.

The full Azure verification URL remains in stdout/stderr logs; the modem display
shows only the short prompt plus the device code.

### 4.3  Display failure handling

If step 5 fails because the gateway rejects the display request or no modem
transport is available, the bootstrap flow exits immediately with a non-zero
status. It does not continue to a console-only fallback. This preserves the
headless operator workflow required by the discovery review.

---

## 5  Rust binary interface

> **Requirements:** AZC-0100, AZC-0102, AZC-0202, AZC-0300

The initial `sonde-azure-companion` binary exposes two modes:

1. **`run`** — default long-running companion mode. In this initial slice it
   establishes gateway companion connectivity and remains ready for later Azure
   integration work.
2. **`display-message`** — helper mode used by the bootstrap scripts to call the
   gateway companion `ShowModemDisplayMessage` RPC with 1 to 4 lines of text.

The helper mode keeps gateway IPC out of shell-script string munging and ensures
the same Rust client stack is used during bootstrap and later runtime work.

---

## 6  Gateway integration

> **Requirements:** AZC-0202, AZC-0203, AZC-0300

### 6.1  Companion client

The Rust binary connects only to the gateway companion socket. It does not use
the admin socket. The gRPC client uses the published `GatewayCompanion` contract
and companion-specific message types.

### 6.2  Shared display path

The gateway-side `ShowModemDisplayMessage` companion RPC reuses the same display
helper as the existing admin RPC. The Azure companion therefore gains no special
display privileges:

1. BLE pairing still preempts transient display requests.
2. Line-count validation remains 1 to 4 lines.
3. The gateway retains rendering, display ownership, and banner-restore logic.
4. The Azure companion cannot issue raw modem commands or upload framebuffers.

---

## 7  Long-running process behavior in this slice

> **Requirements:** AZC-0102, AZC-0300

After bootstrap succeeds, the `run` mode starts and validates gateway companion
connectivity. This first slice does not yet forward events to Azure services or
pull cloud-issued commands. Those behaviors are deferred to the later Terraform
and cloud-integration issues.

