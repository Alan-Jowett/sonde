<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Azure Companion Requirements Specification

> **Document status:** Draft
> **Source:** [issue #771](https://github.com/Alan-Jowett/sonde/issues/771) and companion-bootstrap discovery review.
> **Scope:** This document covers only the initial Azure companion slice: a new Rust companion app in its own Docker container plus bootstrap scripts for Azure device-code login. Terraform, managed-identity creation, gateway configuration generation, and handler wiring are out of scope for this document.
> **Related:** [gateway-companion-api.md](gateway-companion-api.md), [gateway-requirements.md](gateway-requirements.md), [gateway-design.md](gateway-design.md)

---

## 1  Definitions

| Term | Definition |
|------|------------|
| **Azure companion** | The new Rust process that runs in its own container and integrates with `sonde-gateway` through the local companion API. |
| **Auth state volume** | A mounted persistent directory used to hold Azure authentication state across container restarts. |
| **Bootstrap login** | The initial Azure device-code login flow performed when the auth state volume does not yet contain usable authentication state. |
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
the Azure companion binary, Azure CLI tooling required for device-code login,
and the bootstrap scripts needed to initialize authentication state.

**Acceptance criteria:**

1. Building the Azure companion Dockerfile produces an image that starts the Azure companion container without requiring the gateway image.
2. The image contains the Azure companion binary.
3. The image contains Azure CLI tooling sufficient to perform `az login --use-device-code`.
4. The image contains the bootstrap scripts used by the initial login flow.

---

### AZC-0101  Persistent auth state volume

**Priority:** Must
**Source:** Discovery review

**Description:**
The Azure companion container MUST use a mounted persistent auth state volume so
Azure authentication state survives container restarts. The image itself MUST
remain stateless with respect to bootstrap progress and acquired credentials.

**Acceptance criteria:**

1. When the container is started with a mounted auth state volume, Azure authentication state is written into that mounted directory rather than remaining only in the image filesystem.
2. Restarting the container with the same mounted auth state volume preserves previously acquired authentication state.
3. Starting the container with a fresh empty auth state volume is treated as a first-run bootstrap case.

---

### AZC-0102  Bootstrap entrypoint scripts

**Priority:** Must
**Source:** [issue #771](https://github.com/Alan-Jowett/sonde/issues/771), discovery review

**Description:**
The repository MUST provide bootstrap scripts that prepare the auth state
volume, run the first-login flow when needed, and then start the long-running
Azure companion process inside its dedicated container.

**Acceptance criteria:**

1. A provided bootstrap script can start the Azure companion container with the expected auth state volume and gateway companion socket bindings.
2. The bootstrap path initializes the auth state volume before invoking the login logic.
3. After bootstrap prerequisites are satisfied, the scripts start the Azure companion process without requiring manual in-container steps.

---

## 4  Azure device-code login bootstrap

### AZC-0200  Empty-state bootstrap detection

**Priority:** Must
**Source:** Discovery review

**Description:**
When the mounted auth state volume does not contain usable Azure authentication
state, the bootstrap flow MUST treat startup as a first-run bootstrap and enter
Azure device-code login.

**Acceptance criteria:**

1. Starting the container with an empty auth state volume enters the bootstrap login path.
2. Starting the container with usable persisted authentication state does not enter the bootstrap login path.
3. The bootstrap decision is based on the mounted auth state volume, not on transient in-memory state.

---

### AZC-0201  Azure device-code login

**Priority:** Must
**Source:** [issue #771](https://github.com/Alan-Jowett/sonde/issues/771)

**Description:**
The bootstrap flow MUST obtain Azure authentication through Azure device-code
login. The initial implementation MUST use `az login --use-device-code`.

**Acceptance criteria:**

1. First-run bootstrap invokes Azure device-code login without requiring a local browser on the gateway host.
2. The login flow waits for successful operator completion before reporting bootstrap success.
3. If Azure device-code login fails, bootstrap exits with a non-zero status and does not report success.

---

### AZC-0202  Device code display via gateway companion API

**Priority:** Must
**Source:** Discovery review, [gateway-companion-api.md](gateway-companion-api.md)

**Description:**
When Azure device-code login produces a device code, the bootstrap flow MUST
request a modem display update through the gateway companion API. The displayed
message MUST include a short prompt and the exact device code. The Azure
companion MUST NOT attempt raw modem control or bypass the gateway's display
ownership rules.

**Acceptance criteria:**

1. During first-run bootstrap, the Azure companion calls the gateway companion `ShowModemDisplayMessage` RPC with text that includes both a short prompt and the exact device code.
2. The displayed device code matches the value produced by Azure device-code login without modification.
3. The Azure companion uses the gateway companion socket, not the admin socket, for this display request.
4. The bootstrap flow does not invoke raw modem serial commands or direct framebuffer upload.

---

### AZC-0203  Display failure aborts bootstrap

**Priority:** Must
**Source:** Discovery review

**Description:**
If the Azure companion cannot display the device code on the modem through the
gateway companion API, bootstrap MUST fail closed. It MUST surface the failure
to the operator and require a retry after the display becomes available.

**Acceptance criteria:**

1. If the gateway returns `FAILED_PRECONDITION` because BLE pairing owns the display, bootstrap exits with a non-zero status.
2. If the gateway returns `UNAVAILABLE` because no modem transport is configured, bootstrap exits with a non-zero status.
3. Bootstrap does not silently continue with a console-only fallback when the modem display request fails.

---

### AZC-0204  Persisted authentication reuse

**Priority:** Must
**Source:** Discovery review

**Description:**
After a successful bootstrap login, the Azure companion MUST reuse the persisted
authentication state from the mounted auth state volume on later starts instead
of requiring device-code login again.

**Acceptance criteria:**

1. After a successful first-run bootstrap, restarting the container with the same auth state volume skips Azure device-code login.
2. On a restart that skips device-code login, the bootstrap flow does not request a modem display update for a new device code.
3. If persisted authentication state is missing or unusable, the next start re-enters the bootstrap login path.

---

## 5  Gateway integration

### AZC-0300  Companion-socket integration

**Priority:** Must
**Source:** Discovery review, [gateway-companion-api.md](gateway-companion-api.md) §2

**Description:**
The Azure companion MUST integrate with the gateway through the local companion
socket exposed by the `GatewayCompanion` service. It MUST NOT depend on the
gateway admin socket for its normal runtime integration.

**Acceptance criteria:**

1. The Azure companion can connect to the configured companion socket when the gateway is running.
2. The Azure companion uses the companion socket for the modem display request defined by AZC-0202.
3. Normal Azure companion startup does not require access to the gateway admin socket.

