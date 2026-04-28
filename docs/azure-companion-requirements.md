<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Azure Companion Requirements Specification

> **Document status:** Draft
> **Source:** [issue #771](https://github.com/Alan-Jowett/sonde/issues/771) and companion-bootstrap discovery review.
> **Scope:** This document covers the current Azure companion slice: a new Rust
> connector app in its own Docker container plus bootstrap scripts for Azure
> device-code login. Terraform, managed-identity creation, gateway configuration
> generation, and Azure-side message handling beyond local gateway integration
> are out of scope for this document.
> **Related:** [gateway-companion-api.md](gateway-companion-api.md), [gateway-requirements.md](gateway-requirements.md), [gateway-design.md](gateway-design.md)

---

## 1  Definitions

| Term | Definition |
|------|------------|
| **Azure companion** | The new Rust process that runs in its own container and integrates with `sonde-gateway` through the local admin API for bootstrap and the local connector API for long-running runtime traffic. |
| **State volume** | A mounted persistent directory reserved for Azure companion bootstrap output such as local credentials, managed-identity identifiers, and related provisioning artifacts once later slices implement them. |
| **Bootstrap login** | The Azure device-code login flow performed by the current slice before the long-running runtime process starts. |
| **Device code prompt** | The short operator-facing text shown on the modem display during bootstrap, consisting of a prompt plus the Azure device code. |

---

## 2  Requirement format

Each requirement uses the following fields:

- **ID** — Unique identifier (`AZC-XXXX`).
- **Title** — Short name.
- **Description** — What the Azure companion must do.
- **Acceptance criteria** — Observable, testable conditions that confirm the requirement is met.
- **Priority** — MoSCoW: **Must**, **Should**, **May**.
- **Source** — Issue, gateway specification, or reviewed discovery output that motivates the requirement.

---

## 3  Container packaging and bootstrap entrypoints

### AZC-0100  Dedicated companion container image

**Priority:** Must
**Source:** [issue #771](https://github.com/Alan-Jowett/sonde/issues/771)

**Description:**
The repository MUST build a dedicated Docker container image for the Azure
companion. The image MUST be separate from the gateway image and MUST contain
the Azure companion binary, the Rust-owned device-auth implementation required
for device-code login, and the bootstrap scripts needed to initialize the
mounted state volume.

**Acceptance criteria:**

1. Building the Azure companion Dockerfile produces an image that starts the Azure companion container without requiring the gateway image.
2. The image contains the Azure companion binary.
3. The image is based on Alpine Linux and does not require Azure CLI tooling to perform device-code login.
4. The image contains the bootstrap scripts used by the initial login flow.

---

### AZC-0101  Persistent state volume

**Priority:** Must
**Source:** Discovery review

**Description:**
The Azure companion container MUST use a mounted persistent state volume so
later slices can store local provisioning output there. The image itself MUST
remain stateless. The current slice MUST prepare and mount that directory but
MUST NOT persist Azure access tokens there.

**Acceptance criteria:**

1. The bootstrap scripts create and use the mounted state directory rather than relying on image-local writable paths.
2. The container image does not depend on a baked-in token cache or Azure CLI profile directory.
3. The current slice does not write persisted Azure access tokens into the mounted state volume.

---

### AZC-0102  Bootstrap entrypoint scripts

**Priority:** Must
**Source:** [issue #771](https://github.com/Alan-Jowett/sonde/issues/771), discovery review

**Description:**
The repository MUST provide bootstrap scripts that prepare the mounted state
volume, run the Rust-owned device-auth flow, and then start the long-running
Azure companion process inside its dedicated container.

**Acceptance criteria:**

1. A provided bootstrap script can start the Azure companion container with the expected state volume plus the required local gateway socket bindings.
2. The bootstrap path initializes the state volume before invoking the login logic.
3. After bootstrap prerequisites are satisfied, the scripts start the Azure companion process without requiring manual in-container steps.

---

## 4  Azure device-code login bootstrap

### AZC-0200  Current-slice bootstrap always performs device auth

**Priority:** Must
**Source:** Discovery review

**Description:**
Until later slices implement persistent provisioning output, the bootstrap flow
MUST perform Azure device-code login on every start. The current slice MUST NOT
treat any existing volume contents as reusable login state.

**Acceptance criteria:**

1. Starting the container with an empty state volume enters the bootstrap login path.
2. Starting the container with a previously used state volume still enters the bootstrap login path.
3. The current slice does not inspect Azure CLI token caches or similar login-state artifacts to skip device auth.

---

### AZC-0201  Azure device-code login

**Priority:** Must
**Source:** [issue #771](https://github.com/Alan-Jowett/sonde/issues/771)

**Description:**
The bootstrap flow MUST obtain Azure authentication through Azure device-code
login. The initial implementation MUST use an in-process Rust OAuth device-flow
client rather than shelling out to Azure CLI.

**Acceptance criteria:**

1. First-run bootstrap invokes Azure device-code login without requiring a local browser on the gateway host.
2. The login flow waits for successful operator completion before reporting bootstrap success.
3. If Azure device-code login fails, bootstrap exits with a non-zero status and does not report success.

---

### AZC-0202  Device code display via gateway admin API

**Priority:** Must
**Source:** Discovery review, GW-0809

**Description:**
When Azure device-code login produces a device code, the Rust bootstrap flow MUST
request a modem display update through the gateway admin API. The displayed
message MUST include a short prompt and the exact device code. The Azure
companion MUST NOT attempt raw modem control or bypass the gateway's display
ownership rules.

**Acceptance criteria:**

1. During first-run bootstrap, the Azure companion calls the admin `ShowModemDisplayMessage` RPC with text that includes both a short prompt and the exact device code.
2. The displayed device code matches the value produced by Azure device-code login without modification.
3. The Azure companion uses the gateway admin socket, not the connector socket, for this display request.
4. The bootstrap flow does not invoke raw modem serial commands or direct framebuffer upload.

---

### AZC-0203  Display failure aborts bootstrap

**Priority:** Must
**Source:** Discovery review

**Description:**
If the Azure companion cannot display the device code on the modem through the
gateway admin API, bootstrap MUST fail closed. It MUST surface the failure
to the operator and require a retry after the display becomes available.

**Acceptance criteria:**

1. If the gateway returns `FAILED_PRECONDITION` because BLE pairing owns the display, bootstrap exits with a non-zero status.
2. If the gateway returns `UNAVAILABLE` because no modem transport is configured, bootstrap exits with a non-zero status.
3. Bootstrap does not silently continue with a console-only fallback when the modem display request fails.

---

### AZC-0204  Persisted login reuse deferred

**Priority:** Must
**Source:** Discovery review

**Description:**
Because this slice does not yet provision Azure resources or persist the
managed-identity bootstrap artifacts that later slices will rely on, it MUST
NOT treat the mounted state volume as reusable login state. Every start MUST
repeat device-code login until the provisioning slice defines the persisted
state format.

**Acceptance criteria:**

1. Restarting the container with the same state volume still performs device-code login.
2. Each restart that performs device-code login issues a fresh modem display request for the new device code.
3. No current-slice behavior depends on token caches or other persisted login state.

---

## 5  Gateway integration

### AZC-0300  Bootstrap admin-socket integration

**Priority:** Must
**Source:** Discovery review, GW-0809

**Description:**
The Azure companion bootstrap flow MUST integrate with the gateway through the
local admin socket exposed by `GatewayAdmin` for operator-visible bootstrap
actions such as transient modem display. Bootstrap MUST NOT route those actions
through the connector API.

**Acceptance criteria:**

1. The Azure companion bootstrap flow can connect to the configured admin socket when the gateway is running.
2. The Azure companion uses the admin socket for the modem display request defined by AZC-0202.
3. Bootstrap display behavior does not require a separate companion-runtime socket.

---

### AZC-0301  Runtime connector-socket integration

**Priority:** Must
**Source:** Gateway connector redesign, GW-0810 through GW-0815

**Description:**
After bootstrap succeeds, the long-running Azure companion runtime MUST connect
to the gateway through the local connector socket and treat that framed
connector API as its normal runtime integration surface. Runtime control-plane
traffic MUST NOT depend on `GatewayAdmin`.

**Acceptance criteria:**

1. The long-running Azure companion runtime can connect to the configured connector socket when the gateway is running.
2. Runtime startup does not require access to a legacy companion socket.
3. The long-running runtime treats the connector API, not `GatewayAdmin`, as its normal control-plane integration path.

