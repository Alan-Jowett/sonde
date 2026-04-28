<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Azure Companion Requirements Specification

> **Document status:** Draft
> **Source:** [issue #771](https://github.com/Alan-Jowett/sonde/issues/771),
> connector redesign discovery review, and Azure Service Bus discovery review.
> **Scope:** This document covers the Azure companion container, bootstrap-state
> detection, bootstrap-trigger behavior, and the long-running Azure Service Bus
> runtime bridge between `sonde-gateway` and an external Azure control plane.
> The internal Azure provisioning workflow that creates the runtime certificate,
> private key,
> Entra application/service principal, and Service Bus resources is out of scope
> for this document.
> **Related:** [gateway-companion-api.md](gateway-companion-api.md),
> [gateway-requirements.md](gateway-requirements.md),
> [gateway-design.md](gateway-design.md)

---

## 1  Definitions

| Term | Definition |
|------|------------|
| **Azure companion** | The Rust process that runs in its own container and integrates with `sonde-gateway` through the local admin API for bootstrap-only operator-visible actions and the local connector API for long-running runtime traffic. |
| **State volume** | A mounted persistent directory reserved for Azure companion bootstrap output and other local provisioning artifacts. |
| **Provisioning artifacts** | The local certificate PEM, private-key PEM, and related companion-owned state that indicate Azure bootstrap has already completed. |
| **Queue configuration** | The Azure Service Bus namespace and the names of the upstream and downstream queues, supplied explicitly to the companion through configuration rather than hard-coded in the image. |
| **Bootstrap-complete state** | The condition where the required provisioning artifacts exist and the required queue configuration is present, allowing the companion to skip bootstrap and start runtime directly. |
| **Transparent connector payload** | A Service Bus message body that carries the raw Sonde connector payload bytes unchanged. |

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
**Source:** [issue #771](https://github.com/Alan-Jowett/sonde/issues/771), Azure Service Bus discovery review

**Description:**
The repository MUST build a dedicated Docker container image for the Azure
companion. The image MUST be separate from the gateway image and MUST contain
the Azure companion binary plus the bootstrap scripts needed to initialize the
mounted state volume, decide whether bootstrap is required, and start the
long-running runtime bridge. The image MUST remain suitable for Alpine Linux
deployment.

**Acceptance criteria:**

1. Building the Azure companion Dockerfile produces an image that starts the Azure companion container without requiring the gateway image.
2. The image contains the Azure companion binary.
3. The image is based on Alpine Linux.
4. The image does not require Azure CLI for normal runtime connectivity to Service Bus.
5. The image contains the bootstrap scripts used to decide between bootstrap and normal runtime startup.

---

### AZC-0101  Persistent state volume

**Priority:** Must
**Source:** Discovery review

**Description:**
The Azure companion container MUST use a mounted persistent state volume so the
bootstrap workflow can leave behind the local provisioning artifacts needed by
later runtime starts. The image itself MUST remain stateless. The companion MUST
NOT treat short-lived Azure access tokens as persisted runtime state.

**Acceptance criteria:**

1. The bootstrap scripts create and use the mounted state directory rather than relying on image-local writable paths.
2. The state volume is the companion-owned location used to detect whether bootstrap has already completed.
3. The runtime can determine bootstrap-complete state from the presence of the required provisioning artifacts plus required queue configuration.
4. The current design does not require persisted Azure access tokens in the state volume for normal runtime starts.

---

### AZC-0102  Bootstrap entrypoint scripts

**Priority:** Must
**Source:** [issue #771](https://github.com/Alan-Jowett/sonde/issues/771), discovery review

**Description:**
The repository MUST provide bootstrap scripts that prepare the mounted state
volume, decide whether bootstrap is required, and then start either the
bootstrap workflow or the long-running Azure companion runtime inside its
dedicated container.

**Acceptance criteria:**

1. A provided bootstrap script can start the Azure companion container with the expected state volume plus the required local gateway socket bindings.
2. The bootstrap path initializes the state volume before evaluating bootstrap-complete state.
3. If bootstrap-complete state is absent, the scripts invoke the bootstrap workflow before starting the long-running runtime.
4. If bootstrap-complete state is present, the scripts skip bootstrap and start the long-running runtime without requiring manual in-container steps.
5. If bootstrap is invoked, the scripts do not start the long-running runtime until bootstrap-complete state has been established.

---

## 4  Bootstrap-state detection and bootstrap trigger behavior

### AZC-0200  Startup decision based on bootstrap-complete state

**Priority:** Must
**Source:** Azure Service Bus discovery review

**Description:**
At startup, the Azure companion MUST determine whether bootstrap has already
completed. Bootstrap-complete state requires both the local provisioning
artifacts and the required queue configuration. If either is missing, the
companion MUST enter the bootstrap workflow instead of normal runtime mode.

**Acceptance criteria:**

1. Starting the container without the required provisioning artifacts enters the bootstrap workflow.
2. Starting the container without the required queue configuration enters the bootstrap workflow.
3. Starting the container with both the required provisioning artifacts and required queue configuration skips bootstrap and starts runtime directly.

---

### AZC-0201  Device-code bootstrap entry

**Priority:** Must
**Source:** [issue #771](https://github.com/Alan-Jowett/sonde/issues/771), discovery review

**Description:**
When bootstrap is required, the Azure companion MUST begin bootstrap by
obtaining Azure authentication through Azure device-code login using an
in-process Rust device-flow client rather than shelling out to Azure CLI. The
Azure-side provisioning workflow that consumes that bootstrap authentication is
outside this document's scope.

**Acceptance criteria:**

1. Missing bootstrap-complete state causes the companion bootstrap path to invoke Azure device-code login without requiring a local browser on the gateway host.
2. The login flow waits for successful operator completion before reporting bootstrap success.
3. If Azure device-code login fails, bootstrap exits with a non-zero status and does not report success.
4. Successful device-code login alone is not treated as bootstrap completion; the bootstrap path reports success only after bootstrap-complete state has been established.

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

1. During bootstrap, the Azure companion calls the admin `ShowModemDisplayMessage` RPC with text that includes both a short prompt and the exact device code.
2. The displayed device code matches the value produced by Azure device-code login without modification.
3. The Azure companion uses the gateway admin socket, not the connector socket, for this display request.
4. The bootstrap flow does not invoke raw modem serial commands or direct framebuffer upload.

---

### AZC-0203  Display failure aborts bootstrap

**Priority:** Must
**Source:** Discovery review

**Description:**
If the Azure companion cannot display the device code on the modem through the
gateway admin API, bootstrap MUST fail closed. It MUST surface the failure to
the operator and require a retry after the display becomes available.

**Acceptance criteria:**

1. If the gateway returns `FAILED_PRECONDITION` because BLE pairing owns the display, bootstrap exits with a non-zero status.
2. If the gateway returns `UNAVAILABLE` because no modem transport is configured, bootstrap exits with a non-zero status.
3. Bootstrap does not silently continue with a console-only fallback when the modem display request fails.

---

### AZC-0204  Bootstrapped state reuse

**Priority:** Must
**Source:** Azure Service Bus discovery review

**Description:**
If bootstrap-complete state is present at startup, the Azure companion MUST
reuse that state and skip device-code bootstrap. It MUST NOT re-enter device
login merely because the container restarted.

**Acceptance criteria:**

1. Restarting the container with the required provisioning artifacts and queue configuration skips device-code login.
2. Runtime startup after a restart does not require operator-visible bootstrap interaction when bootstrap-complete state is present.
3. Removing either the required provisioning artifacts or queue configuration causes the next start to re-enter bootstrap.

---

## 5  Gateway and Azure runtime integration

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

---

### AZC-0302  Explicit Azure queue configuration

**Priority:** Must
**Source:** Azure Service Bus discovery review

**Description:**
The Azure companion MUST require explicit configuration for the Azure Service
Bus namespace plus the names of exactly two queues: one upstream queue for
gateway-originated connector messages and one downstream queue for cloud-issued
desired-state messages. These values MUST NOT be hard-coded in the container
image.

**Acceptance criteria:**

1. Runtime startup requires explicit namespace configuration.
2. Runtime startup requires explicit configuration for one upstream queue and one downstream queue.
3. The upstream and downstream queues are independently configurable.
4. The image does not hard-code environment-specific queue names or namespace values.

---

### AZC-0303  Pluggable broker transport boundary

**Priority:** Must
**Source:** Azure Service Bus discovery review, GW-0814

**Description:**
The Azure companion runtime MUST isolate its broker-specific integration behind
a pluggable transport boundary so the gateway-facing connector logic does not
depend on one specific Azure SDK crate. `azservicebus` is the first required
transport implementation for this document, but the design MUST keep the Azure
transport swappable.

**Acceptance criteria:**

1. The runtime design separates gateway-connector logic from broker-specific AMQP operations.
2. Replacing the Azure Service Bus transport implementation does not require changing the gateway-facing connector protocol.
3. `azservicebus` is supported as the initial concrete Azure transport implementation.

---

### AZC-0304  Azure Service Bus AMQP transport

**Priority:** Must
**Source:** Azure Service Bus discovery review

**Description:**
The Azure companion runtime MUST bridge the local connector session to Azure
Service Bus over AMQP. The runtime MUST publish gateway-originated connector
messages to the configured upstream queue and consume cloud-issued desired-state
messages from the configured downstream queue.

**Acceptance criteria:**

1. Gateway-originated connector messages are forwarded to the configured upstream queue.
2. Cloud-issued desired-state messages are received from the configured downstream queue and forwarded toward the local gateway connector socket.
3. The runtime uses the same long-lived local connector session for both upstream and downstream connector traffic.

---

### AZC-0305  Certificate-authenticated Azure runtime

**Priority:** Must
**Source:** Azure Service Bus discovery review

**Description:**
The Azure companion runtime MUST authenticate to Azure using an Entra
application/service principal with client-certificate authentication. The
required RBAC roles are assumed to be preconfigured outside this document.

**Acceptance criteria:**

1. Normal runtime starts use the provisioned certificate and private-key material rather than device-code login.
2. Runtime startup fails closed if the required certificate or private-key material is absent or unusable.
3. The runtime design does not require interactive Azure authentication after bootstrap-complete state exists.

---

### AZC-0306  Transparent upstream connector payloads

**Priority:** Must
**Source:** Azure Service Bus discovery review, GW-0814

**Description:**
When forwarding gateway-originated connector traffic to the upstream queue, the
Azure companion MUST carry the raw Sonde connector payload bytes unchanged in
the Service Bus message body. The companion MAY attach minimal broker metadata
in message properties, but it MUST NOT translate the connector payload into an
Azure-specific schema.

**Acceptance criteria:**

1. The Service Bus message body for upstream traffic contains the raw Sonde connector payload bytes.
2. The Azure companion does not rewrite connector payload fields into an Azure-specific typed schema before enqueueing them.
3. Any broker properties used by the companion are supplementary metadata rather than a replacement for the raw connector payload body.

---

### AZC-0307  Transparent downstream desired-state payloads

**Priority:** Must
**Source:** Azure Service Bus discovery review, GW-0811

**Description:**
When consuming desired-state requests from the downstream queue, the Azure
companion MUST interpret the Service Bus message body as a raw Sonde connector
payload and forward those bytes unchanged to the local gateway connector socket.
It MUST NOT require the Azure companion to decode and re-encode the desired
state into a different transport schema.

**Acceptance criteria:**

1. The Azure companion forwards downstream desired-state message bodies to the local gateway connector socket as raw connector payload bytes.
2. The Azure companion does not require Azure-specific payload translation to deliver desired state to the gateway.
3. The downstream queue is reserved for desired-state requests rather than upstream state or app-data traffic.

---

### AZC-0308  Downstream settlement after local handoff

**Priority:** Must
**Source:** Azure Service Bus discovery review

**Description:**
The Azure companion MUST treat a downstream Service Bus message as successfully
processed only after the raw connector payload has been written successfully to
the local gateway connector socket. This success criterion covers local handoff
to the gateway socket only; it does not imply a separate gateway reconciliation
acknowledgement path.

**Acceptance criteria:**

1. The Azure companion does not settle a downstream desired-state message as successful before the raw payload has been written to the local connector socket.
2. If the Azure companion cannot write the payload to the local connector socket, it does not report that Service Bus message as successfully processed.
3. The requirement does not invent a new synchronous acknowledgement path inside the gateway connector protocol.

---

### AZC-0309  Detected transport loss observability

**Priority:** Must
**Source:** GW-0815, Azure Service Bus discovery review

**Description:**
The Azure companion MUST NOT silently mask detected failures at the broker or
local connector boundary. If publish, receive, settlement, or local connector
handoff fails, the runtime MUST surface the condition to operators through
logging, process status, or both.

**Acceptance criteria:**

1. Detected upstream publish failures are surfaced through logging, process status, or both.
2. Detected downstream receive, settlement, or local connector write failures are surfaced through logging, process status, or both.
3. The runtime design does not silently claim success after a detected broker or local connector failure.
