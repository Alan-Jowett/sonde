<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Azure Companion Requirements Specification

> **Document status:** Draft
> **Source:** [issue #771](https://github.com/Alan-Jowett/sonde/issues/771),
> connector redesign discovery review, and Azure Storage Queue discovery review.
> **Scope:** This document covers the Azure companion deployment surfaces
> (Linux runtime container and Windows native service), the dedicated Azure
> bootstrap container image, bootstrap-state detection, bootstrap-trigger
> behavior, the integrated Azure provisioning orchestration (certificate
> generation, bootstrap-image execution via Docker API, and runtime artifact
> creation), and the long-running Azure Storage Queue runtime bridge between
> `sonde-gateway` and an external Azure control plane.
> The Bicep module definitions themselves are specified in
> [azure-provisioning-requirements.md](azure-provisioning-requirements.md).
> **Related:** [azure-provisioning-requirements.md](azure-provisioning-requirements.md),
> [gateway-companion-api.md](gateway-companion-api.md),
> [gateway-requirements.md](gateway-requirements.md),
> [gateway-design.md](gateway-design.md)

---

## 1  Definitions

| Term | Definition |
|------|------------|
| **Azure companion** | The Rust process that integrates with `sonde-gateway` through the local admin API for bootstrap-only operator-visible actions and the local connector API for long-running runtime traffic. On Linux it is deployed in its own runtime container; on Windows it may be deployed as a native service. It also acts as the launcher/orchestrator for explicit bootstrap operations. |
| **Bootstrap image** | The dedicated `sonde-azure-bootstrap` container image that contains the Azure CLI, bundled Bicep files, and bootstrap script used to provision Azure resources for the companion. |
| **State volume** | A mounted persistent directory reserved for Azure companion bootstrap output and other local provisioning artifacts. |
| **State directory** | The platform-owned persistent directory that stores Azure companion provisioning artifacts. On Linux this is provided by the mounted state volume; on Windows the default location is `%ProgramData%\sonde-azure-companion`. |
| **Provisioning artifacts** | The local certificate PEM, private-key PEM, and related companion-owned state that indicate Azure bootstrap has already completed. |
| **Queue configuration** | The Azure Storage Queue endpoint and the names of the upstream and downstream queues, supplied to the companion through either environment variables or a persisted configuration file (`storage-queues.json`) written by bootstrap. Both sources are valid; persisted configuration is the primary source after bootstrap, and environment variables may override it. |
| **Bootstrap-complete state** | The condition where the required provisioning artifacts exist and the required queue configuration is present (from either source), allowing the companion to skip bootstrap and start runtime directly. |
| **Transparent connector payload** | A Storage Queue message body that carries the raw Sonde connector payload bytes unchanged. |

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

## 3  Deployment packaging and bootstrap entrypoints

### AZC-0100  Dedicated companion runtime container image

**Priority:** Must
**Source:** [issue #771](https://github.com/Alan-Jowett/sonde/issues/771), Azure Storage Queue discovery review

**Description:**
The repository MUST build a dedicated Docker container image for the Azure
companion runtime. The image MUST be separate from the gateway image and MUST
contain the Azure companion binary plus the scripts needed to initialize the
mounted state volume, decide whether bootstrap is required, launch the
dedicated bootstrap image when necessary, and start the long-running runtime
bridge. The runtime image MUST remain suitable for Alpine Linux deployment.

**Acceptance criteria:**

1. Building the Azure companion runtime Dockerfile produces an image that starts the Azure companion runtime container without requiring the gateway image.
2. The runtime image contains the Azure companion binary.
3. The runtime image is based on Alpine Linux.
4. The runtime image does not require Azure CLI for normal runtime connectivity to Storage Queue.
5. The runtime image contains the startup/orchestration scripts used to decide between bootstrap and normal runtime startup.
6. The runtime image does not need to bundle the Bicep deployment files or Azure CLI tooling.

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
bootstrap workflow by launching the dedicated bootstrap image or the
long-running Azure companion runtime inside its dedicated runtime container.

**Acceptance criteria:**

1. A provided bootstrap script can start the Azure companion container with the expected state volume plus the required local gateway socket bindings.
2. The bootstrap path initializes the state volume before evaluating bootstrap-complete state.
3. If bootstrap-complete state is absent, the scripts invoke the bootstrap workflow before starting the long-running runtime.
4. If bootstrap-complete state is present, the scripts skip bootstrap and start the long-running runtime without requiring manual in-container steps.
5. If bootstrap is invoked, the scripts do not start the long-running runtime until bootstrap-complete state has been established.

---

### AZC-0103  Windows native service deployment

**Priority:** Must
**Source:** [issue #837](https://github.com/Alan-Jowett/sonde/issues/837), gateway Windows service model

**Description:**
On Windows, the repository MUST support deploying `sonde-azure-companion` as a
native SCM-managed service rather than requiring the Linux-oriented container
wrapper. The native service MUST use the companion's Windows defaults for the
gateway admin pipe, gateway connector pipe, and persistent state directory
unless the operator overrides them explicitly.

**Acceptance criteria:**

1. `sonde-azure-companion` can be registered as a Windows service that starts the real native binary, not the Linux container bootstrap wrapper.
2. The registered service uses `SERVICE_AUTO_START`.
3. By default, the Windows service uses `\\.\pipe\sonde-admin` for the admin pipe, `\\.\pipe\sonde-connector` for the connector pipe, and `%ProgramData%\sonde-azure-companion` for persistent state.
4. Starting and stopping the service through SCM reaches the same runtime bridge behavior as invoking the native binary directly.

---

### AZC-0104  Optional Windows MSI companion component

**Priority:** Must
**Source:** [issue #837](https://github.com/Alan-Jowett/sonde/issues/837)

**Description:**
The Windows MSI installer MUST offer `sonde-azure-companion` as an optional
component instead of always installing it with the gateway. The UI MUST expose
this component as a checkbox that defaults to off. If the operator selects the
component, the installer MUST install the companion binary and register the
Windows service. The installer MUST NOT require an Azure-specific settings page
or collect Azure runtime configuration during installation.

**Acceptance criteria:**

1. The MSI presents an Azure companion component choice whose default state is unchecked.
2. When the component is not selected, the installer does not register a `sonde-azure-companion` service.
3. When the component is selected, the installer installs the companion binary and registers the companion service.
4. The installer does not prompt for Azure queue endpoint, queue, tenant, subscription, certificate, or other Azure-specific settings.
5. Silent or unattended installation can select the component through an MSI feature/property rather than requiring interactive UI.

---

### AZC-0105  Windows CLI service-management fallback

**Priority:** Must
**Source:** [issue #837](https://github.com/Alan-Jowett/sonde/issues/837), gateway Windows service model

**Description:**
The `sonde-azure-companion` binary MUST provide Windows CLI fallback commands
to install and uninstall the companion service for scripted, repair, and
headless scenarios. Re-running install with updated arguments MUST behave as an
idempotent service configuration update. Uninstall MUST stop and delete the
service registration without deleting persisted provisioning artifacts.

**Acceptance criteria:**

1. `sonde-azure-companion install` registers or updates the Windows service and exits successfully when run with Administrator privileges.
2. Re-running `sonde-azure-companion install` updates the existing service configuration rather than failing with "service exists".
3. `sonde-azure-companion uninstall` stops and removes the service registration.
4. Running `sonde-azure-companion uninstall` when no service is registered exits successfully with an informational message.
5. The install and uninstall commands require elevated privileges and fail with a clear message when run unprivileged.
6. Installing or uninstalling the service does not delete `%ProgramData%\sonde-azure-companion` or its provisioning artifacts.

---

### AZC-0106  Dedicated versioned bootstrap container image

**Priority:** Must
**Source:** Discovery review

**Description:**
The repository MUST build a dedicated `sonde-azure-bootstrap` container image
for Azure provisioning. This image MUST be separate from the runtime companion
image and MUST contain the Azure CLI, the bundled Bicep deployment files, and
the bootstrap script needed to execute device-code login plus Bicep deployment.
By default, `sonde-azure-companion` MUST launch the bootstrap image reference
`ghcr.io/alan-jowett/sonde-azure-bootstrap:<matching companion version>`.
Development and test workflows MAY override that default image reference
explicitly.

**Acceptance criteria:**

1. Building the bootstrap Dockerfile produces a `sonde-azure-bootstrap` image that can execute the bootstrap script without relying on files copied from the runtime image.
2. The bootstrap image contains the Azure CLI tooling needed for `az login --use-device-code` and `az deployment`.
3. The bootstrap image contains the bundled Bicep deployment files from `deploy/bicep/`.
4. The default bootstrap image reference used by `sonde-azure-companion` is a version tag that matches the companion release version.
5. If the matching bootstrap image tag is unavailable, bootstrap fails with a clear error rather than silently selecting a different version.
6. Development or test workflows can override the default bootstrap image reference explicitly.

---

## 4  Bootstrap-state detection and bootstrap trigger behavior

### AZC-0200  Startup decision based on bootstrap-complete state

**Priority:** Must
**Source:** Azure Storage Queue discovery review

**Description:**
At startup, the Linux runtime container entrypoint for the Azure companion MUST
determine whether bootstrap has already completed. Bootstrap-complete state
requires both the local provisioning artifacts and the required queue
configuration. If either is missing, the Linux runtime container entrypoint
MUST enter the bootstrap workflow by launching the dedicated bootstrap image
instead of normal runtime mode.

**Acceptance criteria:**

1. Starting the container without the required provisioning artifacts enters the bootstrap workflow.
2. Starting the container without the required queue configuration enters the bootstrap workflow.
3. Starting the container with both the required provisioning artifacts and required queue configuration skips bootstrap and starts runtime directly.

---

### AZC-0201  Unified bootstrap subcommand

**Priority:** Must
**Source:** [issue #771](https://github.com/Alan-Jowett/sonde/issues/771), [issue #849](https://github.com/Alan-Jowett/sonde/issues/849), discovery review

**Description:**
When bootstrap is required, the Azure companion MUST provide a single unified
`bootstrap` subcommand that performs the entire provisioning lifecycle:
certificate generation, device-code authentication inside the dedicated
`sonde-azure-bootstrap` image, Azure deployment through the Docker API,
deployment of the bundled Azure handler package into the provisioned Function
App, deployment of the Web UI SPA content to the provisioned Static Web App,
Entra app configuration (SPA redirect URI and Storage API permission), and
local runtime artifact creation. The earlier `bootstrap-auth`
subcommand is retired and replaced by this unified command. By default, the
Rust companion launches the `sonde-azure-bootstrap:<matching companion
version>` image tag; the image contains the Azure CLI, bundled Bicep files,
bundled Azure handler package, and bootstrap script.
The Rust companion monitors the bootstrap image output to extract the device
code and display it on the modem. The same explicit `bootstrap` subcommand MUST
be supported by the Windows native binary when invoked manually by an operator;
Windows service startup remains a separate fail-closed path governed by
AZC-0205.

**Acceptance criteria:**

1. Missing bootstrap-complete state causes the companion bootstrap path to invoke the unified `bootstrap` subcommand.
2. The `bootstrap` subcommand performs device-code login, certificate generation, Bicep deployment, Azure handler package deployment/activation, and runtime artifact creation in sequence.
3. If any phase of the unified bootstrap fails, the subcommand exits with a non-zero status and does not report success.
4. Successful device-code login alone is not treated as bootstrap completion; the bootstrap path reports success only after bootstrap-complete state has been established.
5. The earlier `bootstrap-auth` subcommand name is no longer accepted.
6. Invoking `sonde-azure-companion bootstrap` from the Windows native binary performs the same end-to-end provisioning lifecycle as the Linux bootstrap path, subject to the same Docker and Azure prerequisites.
7. By default, the unified bootstrap path launches the version-tagged `sonde-azure-bootstrap` image that matches the companion release version.

---

### AZC-0202  Device code display via gateway admin API

**Priority:** Must
**Source:** Discovery review, GW-0809

**Description:**
When the dedicated bootstrap image's `az login --use-device-code` step produces
a device code in its output, the Rust bootstrap flow MUST extract the code and
request a modem display update through the gateway admin API. The displayed
message MUST include a short prompt and the exact device code. The Azure
companion MUST NOT attempt raw modem control or bypass the gateway's display
ownership rules. Because the bootstrap image tag is version-matched to the
companion release, the output format is controlled by the shipped bootstrap
artifact for that release.

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
**Source:** Azure Storage Queue discovery review

**Description:**
If bootstrap-complete state is present at startup, the Azure companion MUST
reuse that state and skip device-code bootstrap. It MUST NOT re-enter device
login merely because the container restarted.

**Acceptance criteria:**

1. Restarting the container with the required provisioning artifacts and queue configuration skips device-code login.
2. Runtime startup after a restart does not require operator-visible bootstrap interaction when bootstrap-complete state is present.
3. Removing either the required provisioning artifacts or queue configuration causes the next start to re-enter bootstrap.

---

### AZC-0205  Windows service startup fails closed before bootstrap

**Priority:** Must
**Source:** [issue #837](https://github.com/Alan-Jowett/sonde/issues/837)

**Description:**
When the Windows native service starts without bootstrap-complete state, it
MUST fail closed with a clear operator-visible diagnostic instead of entering
the bootstrap-image launch flow automatically. Windows operators are
expected to provision or bootstrap the companion explicitly before the service
can start successfully in steady state.

**Acceptance criteria:**

1. Starting the Windows service without the required provisioning artifacts exits with a non-zero service status and surfaces a clear diagnostic identifying the missing state.
2. Starting the Windows service without the required queue configuration exits with a non-zero service status and surfaces a clear diagnostic identifying the missing configuration.
3. In the missing-state cases above, the Windows service does not attempt to pull or run the bootstrap image automatically.
4. Once bootstrap-complete state is present, restarting the Windows service starts the runtime bridge normally without requiring installer changes.

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
**Source:** Azure Storage Queue discovery review

**Description:**
The Azure companion MUST require explicit configuration for the Azure Storage
Queue endpoint plus the names of exactly two queues: one upstream queue for
gateway-originated connector messages and one downstream queue for cloud-issued
desired-state messages. These values MUST NOT be hard-coded in the container
image. Configuration MAY be supplied through environment variables or through a
persisted configuration file (`storage-queues.json`) written by the bootstrap
workflow. Environment variables, if set, override persisted file values.

**Acceptance criteria:**

1. Runtime startup requires queue endpoint configuration from either environment variables or a persisted configuration file.
2. Runtime startup requires configuration for one upstream queue and one downstream queue from either source.
3. The upstream and downstream queues are independently configurable.
4. The image does not hard-code environment-specific queue names or endpoint values.
5. Environment variables override values from the persisted configuration file when both are present.

---

### AZC-0303  Pluggable broker transport boundary

**Priority:** Must
**Source:** Azure Storage Queue discovery review, GW-0814

**Description:**
The Azure companion runtime MUST isolate its broker-specific integration behind
a pluggable transport boundary so the gateway-facing connector logic does not
depend on one specific Azure SDK crate. `reqwest` is the first required
transport implementation for this document, but the design MUST keep the Azure
transport swappable.

**Acceptance criteria:**

1. The runtime design separates gateway-connector logic from broker-specific HTTP operations.
2. Replacing the Azure Storage Queue transport implementation does not require changing the gateway-facing connector protocol.
3. `reqwest` is supported as the initial concrete Azure transport implementation.

---

### AZC-0304  Azure Storage Queue HTTP transport

**Priority:** Must
**Source:** Azure Storage Queue discovery review

**Description:**
The Azure companion runtime MUST bridge the local connector session to Azure
Storage Queues over HTTP REST. The runtime MUST publish gateway-originated connector
messages to the configured upstream queue and consume cloud-issued desired-state
messages from the configured downstream queue.

**Acceptance criteria:**

1. Gateway-originated connector messages are forwarded to the configured upstream queue.
2. Cloud-issued desired-state messages are received from the configured downstream queue and forwarded toward the local gateway connector socket.
3. The runtime uses the same long-lived local connector session for both upstream and downstream connector traffic.

---

### AZC-0305  Certificate-authenticated Azure runtime

**Priority:** Must
**Source:** Azure Storage Queue discovery review

**Description:**
The Azure companion runtime MUST authenticate to Azure using an Entra
application/service principal with client-certificate authentication. The
required RBAC roles are assumed to be preconfigured outside this document.

**Acceptance criteria:**

1. Normal runtime starts use the provisioned certificate and private-key material rather than device-code login.
2. Runtime startup fails closed if the required certificate or private-key material is absent or unusable.
3. The runtime design does not require interactive Azure authentication after bootstrap-complete state exists.
4. The OAuth token endpoint used for client-assertion authentication is derived from the `login_endpoint` field in `service-principal.json`, normalized by stripping any trailing slash before constructing the full URL. When the field is absent (pre-existing deployments), the runtime defaults to `https://login.microsoftonline.com`. When the field is present but empty or whitespace-only, startup fails with a configuration error.

---

### AZC-0306  Transparent upstream connector payloads

**Priority:** Must
**Source:** Azure Storage Queue discovery review, GW-0814

**Description:**
When forwarding gateway-originated connector traffic to the upstream queue, the
Azure companion MUST carry the raw Sonde connector payload bytes unchanged in
the Storage Queue message body. The companion MAY attach minimal broker metadata
in message properties, but it MUST NOT translate the connector payload into an
Azure-specific schema.

**Acceptance criteria:**

1. The Storage Queue message body for upstream traffic contains the raw Sonde connector payload bytes.
2. The Azure companion does not rewrite connector payload fields into an Azure-specific typed schema before enqueueing them.
3. Any broker properties used by the companion are supplementary metadata rather than a replacement for the raw connector payload body.

---

### AZC-0307  Transparent downstream desired-state payloads

**Priority:** Must
**Source:** Azure Storage Queue discovery review, GW-0811

**Description:**
When consuming desired-state requests from the downstream queue, the Azure
companion MUST interpret the Storage Queue message body as a raw Sonde connector
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
**Source:** Azure Storage Queue discovery review

**Description:**
The Azure companion MUST treat a downstream Storage Queue message as successfully
processed only after the raw connector payload has been written successfully to
the local gateway connector socket. This success criterion covers local handoff
to the gateway socket only; it does not imply a separate gateway reconciliation
acknowledgement path.

**Acceptance criteria:**

1. The Azure companion does not settle a downstream desired-state message as successful before the raw payload has been written to the local connector socket.
2. If the Azure companion cannot write the payload to the local connector socket, it does not report that Storage Queue message as successfully processed.
3. The requirement does not invent a new synchronous acknowledgement path inside the gateway connector protocol.

---

### AZC-0309  Detected transport loss observability

**Priority:** Must
**Source:** GW-0815, Azure Storage Queue discovery review

**Description:**
The Azure companion MUST NOT silently mask detected failures at the broker or
local connector boundary. If publish, receive, settlement, or local connector
handoff fails, the runtime MUST surface the condition to operators through
logging, process status, or both.

**Acceptance criteria:**

1. Detected upstream publish failures are surfaced through logging, process status, or both.
2. Detected downstream receive, settlement, or local connector write failures are surfaced through logging, process status, or both.
3. The runtime design does not silently claim success after a detected broker or local connector failure.

---

### AZC-0310  Live Azure CI bridge validation against a connector harness

**Priority:** Must
**Source:** CI validation discovery review

**Description:**
The repository MUST support an on-demand live Azure validation workflow that
runs the real `sonde-azure-companion` runtime against a local connector test
harness and the disposable Azure Storage Queue resources created for that run. The
workflow validates the Azure companion bridge at the connector boundary and MUST
NOT require a full `sonde-gateway` process for this live cloud test.

**Acceptance criteria:**

1. The live Azure validation workflow starts the real `sonde-azure-companion` runtime.
2. The runtime connects to a local connector harness rather than a full `sonde-gateway` process.
3. The runtime uses the disposable Azure Storage Queue endpoint and queue names provisioned for that workflow run.
4. The live validation covers both upstream publish and downstream desired-state delivery.
5. The workflow remains on-demand rather than part of routine PR CI.

---

### AZC-0311  Live Azure CI verifies concrete Storage Queue handoff semantics

**Priority:** Must
**Source:** CI validation discovery review, AZC-0304, AZC-0308, AZC-0309

**Description:**
The live Azure validation workflow MUST verify the bridge semantics that matter
at the cloud boundary: transparent upstream payload publication, transparent
downstream desired-state delivery, and downstream settlement only after the
local connector harness accepts the payload.

**Acceptance criteria:**

1. At least one representative upstream connector payload is published through the real Azure Storage Queue transport during the live workflow.
2. At least one representative downstream desired-state payload is received from Azure Storage Queue and written unchanged to the local connector harness.
3. The workflow demonstrates that downstream settlement occurs only after successful local connector handoff.
4. Detected Azure transport or local connector handoff failures are surfaced by the workflow rather than silently ignored.

---

## 6  Bootstrap provisioning orchestration

### AZC-0400  Self-signed certificate generation

**Priority:** Must
**Source:** [issue #771](https://github.com/Alan-Jowett/sonde/issues/771), [issue #849](https://github.com/Alan-Jowett/sonde/issues/849), discovery review

**Description:**
The unified `bootstrap` subcommand MUST generate a self-signed ECDSA P-256
X.509 certificate and corresponding private key in Rust for use as the Entra
application credential. The certificate and private key MUST be written to the
mounted state volume as PEM files.

**Acceptance criteria:**

1. Bootstrap generates an ECDSA P-256 key pair and self-signed X.509 certificate without requiring external tooling.
2. The generated certificate has a default validity period of 2 years from the time of generation.
3. The certificate PEM and private-key PEM are written to the mounted state volume.
4. The certificate's DER-encoded public material can be base64-encoded and passed to the Bicep deployment's `companionCertificateBase64` parameter.
5. Re-running bootstrap regenerates the certificate and private key.
6. On Unix platforms, the private-key PEM file MUST be written with owner-only read permissions (mode 0600).
7. On Windows, the private-key PEM file MUST be protected with a restricted ACL that permits read access to the bootstrap operator and the configured Windows service identity for steady-state runtime, while denying broad access to generic principals such as `Everyone` and `Users`.

---

### AZC-0401  Bootstrap image execution via Docker API

**Priority:** Must
**Source:** [issue #771](https://github.com/Alan-Jowett/sonde/issues/771), discovery review

**Description:**
The unified `bootstrap` subcommand MUST execute the Azure provisioning flow by
using the Bollard Rust Docker SDK to run the dedicated
`sonde-azure-bootstrap` image. The bootstrap MUST NOT shell out to the Docker
CLI binary; it MUST use the Docker Engine API directly through the Bollard
crate.

**Acceptance criteria:**

1. Bootstrap uses the Bollard crate to create and run the `sonde-azure-bootstrap` container.
2. Bootstrap does not invoke the `docker` CLI binary or shell out to any Docker command.
3. By default, bootstrap uses the `sonde-azure-bootstrap` image tag that matches the companion release version.
4. Bootstrap passes only dynamic deployment inputs such as the generated certificate into the bootstrap container before running Azure deployment logic, and does not depend on host-side Bicep files.
5. Bootstrap authenticates by running `az login --use-device-code` inside the bootstrap container and parsing the emitted device code for modem display.
6. Bootstrap captures the Azure deployment JSON outputs from the container's stdout.
7. Bootstrap cleans up the bootstrap container after completion, regardless of success or failure.

---

### AZC-0402  Bundled Azure provisioning assets in bootstrap image

**Priority:** Must
**Source:** [issue #771](https://github.com/Alan-Jowett/sonde/issues/771), discovery review

**Description:**
The dedicated `sonde-azure-bootstrap` image MUST bundle the Azure provisioning
assets needed for bootstrap at build time so the bootstrap workflow does not
rely on runtime-image internals or host-side Bicep files. The same image MUST
also bundle the prebuilt Azure handler package needed to make the Function App
runnable during bootstrap.

**Acceptance criteria:**

1. The bootstrap Dockerfile copies `deploy/bicep/` into the bootstrap image at a fixed path.
2. The bundled provisioning assets include the top-level `main.bicep`, all module files, and `bicepconfig.json`.
3. The bootstrap image includes the script that runs `az login --use-device-code` followed by Azure deployment.
4. Bootstrap references the bundled path inside the bootstrap image rather than copying Bicep files out of the runtime companion image.
5. The bootstrap image bundles the prebuilt `sonde-azure-handler` package that bootstrap deploys into the Function App.

---

### AZC-0403  Runtime artifact creation from Bicep outputs

**Priority:** Must
**Source:** [issue #771](https://github.com/Alan-Jowett/sonde/issues/771), AZP-0203

**Description:**
After successful Bicep deployment, the unified `bootstrap` subcommand MUST
create the local runtime artifacts expected by `sonde-azure-companion run`:
`service-principal.json` containing the Entra tenant ID, client ID, and
relative paths to the certificate and private-key PEM files already written to
the state volume.

**Acceptance criteria:**

1. Bootstrap writes `service-principal.json` to the mounted state volume after successful Bicep deployment.
2. The `service-principal.json` contains `tenant_id`, `client_id`, `certificate_path`, `private_key_path`, and `login_endpoint` fields.
3. The `tenant_id`, `client_id`, and `login_endpoint` values are extracted from the Bicep deployment outputs. The `login_endpoint` is sourced from `environment().authentication.loginEndpoint` exposed through the `companionBootstrapValues` output.
4. The `certificate_path` and `private_key_path` values are relative paths within the state volume pointing to the generated PEM files.
5. The resulting state satisfies the `check-runtime-ready` validation without manual intervention.

---

### AZC-0404  Storage Queue configuration from Bicep outputs

**Priority:** Must
**Source:** [issue #771](https://github.com/Alan-Jowett/sonde/issues/771), AZC-0302

**Description:**
After successful Bicep deployment, the unified `bootstrap` subcommand MUST
persist or surface the Storage Queue endpoint and queue names so the companion
runtime can use them without requiring the operator to manually extract and
supply them.

**Acceptance criteria:**

1. Bootstrap extracts the Storage Queue endpoint, upstream queue name, and downstream queue name from the Bicep deployment outputs.
2. Bootstrap writes or persists these values so they are available at the next container startup.
3. The runtime can read the persisted queue configuration without requiring the operator to re-enter it manually.

### AZC-0405  Bootstrap progress display

**Priority:** Must
**Source:** [issue #771](https://github.com/Alan-Jowett/sonde/issues/771), AZC-0202

**Description:**
The unified `bootstrap` subcommand MUST report progress for each major phase
on both the modem display (via the gateway admin API) and stderr. The phases
include authentication, certificate generation, Azure deployment, and
completion.

**Acceptance criteria:**

1. Each major bootstrap phase updates the modem display with a short status message via the admin `ShowModemDisplayMessage` RPC.
2. Each major bootstrap phase logs a status message to stderr.
3. The authentication phase displays the device code as defined by AZC-0202.
4. Bootstrap completion displays a success message on the modem display.
5. Bootstrap failure displays an error indication on the modem display before exiting.

---

### AZC-0406  Docker socket access

**Priority:** Must
**Source:** [issue #771](https://github.com/Alan-Jowett/sonde/issues/771), discovery review

**Description:**
The Azure companion runtime container MUST have access to the Docker Engine API
so the Bollard crate can create and manage the dedicated bootstrap container
during bootstrap.
The Docker socket MUST be bind-mounted from the host into the companion
container.

**Acceptance criteria:**

1. The bootstrap entrypoint script bind-mounts the Docker socket from the host into the companion container.
2. Bollard auto-detects the Docker socket path inside the container.
3. If the Docker socket is not accessible, bootstrap fails with a clear error message identifying the missing socket.

---

### AZC-0407  Idempotent re-bootstrap

**Priority:** Must
**Source:** [issue #771](https://github.com/Alan-Jowett/sonde/issues/771) AC-2, AZP-0300

**Description:**
Re-running the unified `bootstrap` subcommand on an already-provisioned system
MUST be safe. Bootstrap MUST re-generate the certificate, re-run the Bicep
deployment, and re-write the runtime artifacts. The Bicep deployment is
inherently idempotent; certificate regeneration updates the Entra application
credential. Bootstrap MUST use a staging approach so that a failed re-bootstrap
does not corrupt the existing working state.

**Acceptance criteria:**

1. Re-running bootstrap on a system with existing provisioning artifacts succeeds without errors.
2. Re-bootstrap regenerates the certificate and private key.
3. Re-bootstrap re-runs the Bicep deployment with the new certificate.
4. Re-bootstrap updates `service-principal.json` with any changed values.
5. The runtime can start successfully after re-bootstrap.
6. If re-bootstrap fails at any phase after authentication, the previous working state remains intact.
7. No staging artifacts remain in the state volume after a failed re-bootstrap.

---

### AZC-0408  Azure subscription selection

**Priority:** Should
**Source:** Discovery review

**Description:**
The unified `bootstrap` subcommand SHOULD allow the operator to specify which
Azure subscription to target for the Bicep deployment. If not specified, the
deployment SHOULD use the default subscription from the device-login session.

**Acceptance criteria:**

1. Bootstrap accepts an optional Azure subscription ID via environment variable.
2. If specified, the Bicep deployment targets that subscription.
3. If not specified, the Bicep deployment uses the device-login session's default subscription.

---

### AZC-0409  Bootstrap deploys and activates the bundled Azure handler package

**Priority:** Must
**Source:** USER-REQUEST: implement "Function code deployment" in azure function, AZP-0105

**Description:**
After successful Azure deployment, the unified `bootstrap` subcommand MUST
deploy the bundled prebuilt `sonde-azure-handler` package into the provisioned
Azure Function App and wait until Azure reports the package active. Bootstrap
does not succeed if the Function App shell exists but no functions are loaded.

**Acceptance criteria:**

1. Bootstrap deploys the bundled `sonde-azure-handler` package without requiring a manual operator upload step.
2. The deployed package matches the companion/bootstrap release rather than an ad hoc package built on the operator host during bootstrap.
3. Bootstrap waits until Azure reports that at least one function is loaded in the provisioned Function App before reporting success.
4. If package deployment or activation fails, bootstrap exits non-zero and does not report success-shaped completion.

---

### AZC-0410  Bootstrap deploys SPA content and configures Entra app for Web UI

**Priority:** Must
**Source:** USER-REQUEST: single bootstrap command that installs all Azure parts

**Description:**
After successful Function App handler deployment and activation, the unified
`bootstrap` subcommand MUST deploy the bundled Web UI SPA content to the
provisioned Static Web App and configure the Entra app registration for
browser-based access. This eliminates the separate `deploy/web-ui/deploy.sh`
manual step, making `sonde-azure-companion bootstrap` the single command that
provisions all Azure surfaces.

The bootstrap image MUST bundle the SPA static assets (`index.html`, `app.js`,
`style.css`, `staticwebapp.config.json`) and generate `config.json` from the
Bicep deployment outputs at deploy time.

The bootstrap script MUST:
1. Extract `staticWebAppName`, `staticWebAppHostname`, `companionClientId`,
   `companionTenantId`, `storageAccountName`, and `functionAppName` from the
   Bicep deployment outputs.
2. Generate `config.json` with the MSAL client ID, tenant ID, storage account
   name, and function app name.
3. Deploy the SPA content (including the generated `config.json`) to the Static
   Web App using the ARM zipdeploy REST API (`az rest`) and the provisioned
   storage account as a temporary staging area.
4. Register the SWA hostname (`https://<hostname>`) as a SPA redirect URI on
   the Entra app registration, merging with any existing redirect URIs.
5. Add the Azure Storage `user_impersonation` API permission to the Entra app
   registration if not already present.
6. Expose `api://<clientId>/user_impersonation` as an API scope on the Entra app
   registration (required for EasyAuth token validation on the Function App).
   If the scope already exists, this step is a no-op.

The bootstrap image deploys SPA content to the Static Web App using the ARM
REST API (`Microsoft.Web/staticSites/{name}/zipdeploy`) via `az rest`. The
content is zipped using Python (bundled with `az` CLI), uploaded as a temporary
blob to the provisioned storage account, and referenced by SAS URL in the
zipdeploy request. This approach requires no Node.js, npm, or
architecture-specific binaries, enabling native arm64 execution.

**Acceptance criteria:**

1. After `sonde-azure-companion bootstrap` completes, the Static Web App serves the SPA content at its default hostname without any additional manual deployment step.
2. The generated `config.json` contains correct MSAL client ID, authority URL, storage account name, and function app name values matching the Bicep deployment outputs.
3. The Entra app registration includes the SWA hostname as a SPA redirect URI after bootstrap.
4. The Entra app registration includes the Azure Storage `user_impersonation` API permission after bootstrap.
5. Existing SPA redirect URIs on the Entra app are preserved (not overwritten) when adding the SWA hostname.
6. Re-running bootstrap updates the SPA content and Entra configuration idempotently.
7. If SPA deployment fails, bootstrap exits non-zero and does not report success.
8. The Entra app registration exposes `api://<clientId>/user_impersonation` as an API scope after bootstrap.
