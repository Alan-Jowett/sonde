<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Azure Companion Design Specification

> **Document status:** Draft
> **Scope:** Internal design for the Azure companion deployment surfaces
> (Linux runtime container and Windows native service), the dedicated bootstrap
> image, bootstrap-state detection, bootstrap trigger behavior, integrated
> provisioning orchestration (certificate generation, bootstrap-image execution
> via Docker API, and runtime artifact creation), and the Storage Queue HTTP REST
> runtime bridge.
> The Bicep module definitions themselves are specified in
> [azure-provisioning-design.md](azure-provisioning-design.md).
> **Audience:** Implementers building the Azure companion crate and its
> deployment artifacts.
> **Related:** [azure-provisioning-design.md](azure-provisioning-design.md),
> [azure-companion-requirements.md](azure-companion-requirements.md),
> [gateway-companion-api.md](gateway-companion-api.md),
> [gateway-design.md](gateway-design.md)

---

## 1  Overview

The Azure companion is a Rust workspace crate that talks to
`sonde-gateway` over two local gateway-facing surfaces:

1. the admin gRPC API for bootstrap-only operator-visible actions, and
2. the local framed connector API for long-running runtime traffic.

The Azure companion now has two distinct responsibilities:

1. detect whether bootstrap has already completed,
2. invoke the dedicated bootstrap image on the Linux runtime-container path when the
   required local provisioning artifacts are missing,
3. use the gateway admin API to display the device code during bootstrap,
4. orchestrate the full provisioning lifecycle (certificate generation,
   bootstrap-image execution via the Docker API, and runtime artifact
   creation), and
5. when bootstrap-complete state exists, bridge the gateway connector session to
   Azure Storage Queue over HTTP REST.

The gateway-facing connector contract remains cloud-agnostic. Azure-specific
logic is confined to the Azure companion.

---

## 2  Repository layout

> **Requirements:** AZC-0100, AZC-0102, AZC-0103, AZC-0104, AZC-0106

The implementation adds or updates the following artifacts:

| Artifact | Purpose |
|----------|---------|
| `crates/sonde-azure-companion/` | Rust crate containing the Azure companion binary. |
| `.github/docker/Dockerfile.azure-companion` | Dockerfile for the dedicated Azure companion runtime image. |
| `.github/docker/Dockerfile.azure-bootstrap` | Dockerfile for the dedicated `sonde-azure-bootstrap` provisioning image. |
| `deploy/azure-companion/bootstrap.sh` | Host/container runtime orchestration script that prepares the mounted state volume, evaluates bootstrap-complete state, launches the bootstrap image when needed, and otherwise starts runtime. |
| `deploy/azure-companion/entrypoint.sh` | In-container runtime entrypoint that orchestrates bootstrap-state detection before starting the Rust binary. |
| `deploy/azure-bootstrap/bootstrap.sh` | In-bootstrap-image script that runs device-code login plus Azure deployment using bundled provisioning assets. |
| `installer/windows/sonde.wxs` | Windows MSI definition that exposes the Azure companion as an optional installer feature and can register the companion service. |

The long-running binary is named `sonde-azure-companion`.

---

## 3  Runtime architecture

> **Requirements:** AZC-0100, AZC-0101, AZC-0102, AZC-0103, AZC-0104, AZC-0105, AZC-0205, AZC-0301, AZC-0302, AZC-0303, AZC-0304, AZC-0305, AZC-0310, AZC-0311

### 3.1  Process model

The Azure companion architecture separates steady-state runtime from bootstrap
execution. The deployment entrypoints are:

1. **Linux runtime container deployment** uses a small shell entrypoint that
   performs filesystem and startup orchestration and then execs the Rust binary.
2. **Windows native-service deployment** is started directly by SCM and runs the
   Rust binary without the Linux shell wrapper.

3. **Bootstrap-image deployment** runs the dedicated `sonde-azure-bootstrap`
   image, which contains Azure CLI, bundled Bicep files, and the bootstrap
   script for device-code login plus Azure deployment.

This keeps the gateway-facing logic and Azure-facing runtime in typed Rust while
isolating bootstrap-only tooling and provisioning assets in a separate image.

#### 3.1.1  Linux runtime container startup

For Linux runtime container deployment:

1. **Shell script** handles environment preparation, state-directory setup, and
   bootstrap-state detection orchestration.
2. **Rust binary** owns gateway admin gRPC communication, connector-socket
   runtime communication, bootstrap device flow, and broker integration.

#### 3.1.2  Windows native-service startup

For Windows deployment:

1. SCM launches the real `sonde-azure-companion` binary directly.
2. The binary exposes service-management entrypoints (`install` / `uninstall`)
   plus a service runtime entrypoint used by SCM.
3. The service runtime uses the Windows defaults for the admin pipe, connector
   pipe, and persistent state directory unless the operator overrides them.
4. If bootstrap-complete state is absent, the service fails closed with a clear
   diagnostic rather than attempting to run the bootstrap image automatically.

#### 3.1.3  Bootstrap-image startup

For explicit or auto-launched bootstrap:

1. `sonde-azure-companion` resolves the default bootstrap image reference as
   `ghcr.io/alan-jowett/sonde-azure-bootstrap:<matching companion version>`,
   unless a development or test override is configured.
2. The Rust companion uses Bollard to create and run that bootstrap image.
3. The bootstrap image runs its bundled script, which executes
   `az login --use-device-code` followed by Azure deployment using the bundled
   Bicep files.

### 3.2  Mounted and configured inputs

The active deployment entrypoint expects the following runtime inputs:

| Input | Purpose |
|-------|---------|
| State directory | Persistent storage for local provisioning artifacts such as the runtime certificate PEM, private-key PEM, service-principal metadata file, and persisted Storage Queue configuration. On Linux this is the mounted state volume; on Windows this defaults to `%ProgramData%\sonde-azure-companion`. |
| Gateway admin socket | Local IPC path used by bootstrap to call `GatewayAdmin` RPCs such as `ShowModemDisplayMessage`. |
| Gateway connector socket | Local framed IPC path used by the long-running runtime after bootstrap succeeds. |
| Docker socket (bootstrap only) | Docker Engine API socket, bind-mounted from the host, used by the companion to launch the dedicated bootstrap image via Bollard. This is part of the Linux runtime-container bootstrap path and explicit Windows bootstrap, and is not a steady-state Windows service requirement. |
| Storage Queue endpoint | Runtime configuration for the Azure Storage Queue service URI (e.g., `https://<account>.queue.core.windows.net`), from environment variable or persisted `storage-queues.json`. |
| Upstream queue name | Runtime configuration for the queue that carries gateway-originated connector messages, from environment variable or persisted `storage-queues.json`. |
| Downstream queue name | Runtime configuration for the queue that carries cloud-originated desired-state messages, from environment variable or persisted `storage-queues.json`. |

Bootstrap-complete state is defined by the combination of:

1. the required local provisioning artifacts in the state directory, and
2. the required queue configuration (from either environment variables or
   persisted `storage-queues.json` in the state directory).

The current runtime artifact shape is a companion-owned `service-principal.json`
file containing the Entra tenant ID, client ID, login endpoint, PEM certificate
path, and PEM private-key path, plus the referenced certificate and key files in
the state directory. After bootstrap, the state directory also contains
`storage-queues.json` with the Storage Queue endpoint and queue names. New bootstrap
commits are written into a generation directory under the state directory and made
current by atomically updating a `.current-state` marker file. For backward
compatibility, startup also accepts the legacy flat-file layout when the marker
is absent.

### 3.3  Bootstrap-state decision

Startup follows a platform-specific decision:

1. Ensure the state directory exists and is writable.
2. Check whether the required local provisioning artifacts exist.
3. Check whether the required Storage Queue endpoint and queue configuration are
   present.
4. **Linux runtime-container path:** if both are present, skip bootstrap and
   start `run`; otherwise start `bootstrap`, which launches the dedicated
   bootstrap image and waits for bootstrap-complete state before starting
   runtime.
5. **Windows service path:** if both are present, start `run`; otherwise emit a
   clear diagnostic and exit with a non-zero service status.

When the Linux bootstrap path is entered, the unified `bootstrap` subcommand
orchestrates the full provisioning lifecycle by launching the dedicated
bootstrap image as described in section 4.2.

### 3.4  Windows MSI integration

The Windows installer exposes the Azure companion as an optional feature rather
than a mandatory peer of the gateway binaries.

1. The Azure companion feature is unchecked by default.
2. If selected, the MSI installs the companion binary and registers the
   `sonde-azure-companion` service with `SERVICE_AUTO_START`.
3. If not selected, the MSI leaves the companion service unregistered.
4. The MSI does not collect Azure tenant, subscription, namespace, queue, or
   certificate settings; those are established later through explicit bootstrap
   or provisioning steps outside the installer UI.
5. Silent or unattended installs select the feature through standard MSI
   feature/property input rather than requiring an interactive dialog.

### 3.5  Windows CLI service management

The companion binary mirrors the gateway's Windows service-management fallback:

1. `sonde-azure-companion install` validates Administrator privileges and then
   creates or updates the SCM service definition.
2. `sonde-azure-companion uninstall` stops and deletes the service registration
   idempotently.
3. These commands manage only the service registration; they preserve the
   companion state directory and provisioning artifacts.

---

### 3.6  Live Azure CI runtime topology

The live Azure validation workflow uses a narrower runtime topology than a full
gateway deployment. It starts the real `sonde-azure-companion` runtime against:

1. a local connector harness that speaks the framed connector protocol, and
2. the disposable Azure Storage Queue endpoint and queues created earlier in the
   same workflow run.

The harness is sufficient because the purpose of this workflow is to validate
the Azure companion's cloud bridge contract at the connector boundary, not to
re-validate gateway startup, modem ownership, or admin gRPC behavior.

### 3.7  Live validation sequence

Within the single manually triggered workflow, the live validation sequence is:

1. provision the disposable Azure stack,
2. derive runtime configuration from the deployment outputs,
3. start the local connector harness,
4. start the real `sonde-azure-companion` runtime in bootstrap-complete mode,
5. inject representative upstream connector payloads through the harness and
   assert they reach the upstream queue unchanged,
6. enqueue representative downstream desired-state payloads in Azure Storage Queue
   and assert they are delivered unchanged to the harness, and
7. assert that downstream settlement happens only after the harness accepts the
   local handoff.

This sequence keeps live Azure validation focused on the real Storage Queue
transport and the runtime bridge semantics already defined in section 5.

---

## 4  Bootstrap flow

> **Requirements:** AZC-0200, AZC-0201, AZC-0202, AZC-0203, AZC-0204, AZC-0205, AZC-0300,
> AZC-0400, AZC-0401, AZC-0402, AZC-0403, AZC-0404, AZC-0405, AZC-0406, AZC-0407, AZC-0408, AZC-0409, AZC-0411

### 4.1  Bootstrap trigger

The Linux bootstrap path is entered only when bootstrap-complete state is
absent. This differs from the earlier draft, which always re-entered device-code
login on restart. The Windows service path does not auto-enter bootstrap when
state is absent; it fails closed until the operator performs provisioning
explicitly. Both Linux auto-bootstrap and explicit Windows bootstrap use the
same dedicated bootstrap image.

Re-running the `bootstrap` subcommand explicitly (e.g., for credential rotation)
is safe: it regenerates the certificate, re-runs Bicep, and rewrites the runtime
artifacts. This explicit bootstrap path is supported from the Windows native
binary as well as from the Linux runtime-container deployment path; only the Windows
service auto-start path is fail-closed before bootstrap-complete state exists.

### 4.2  Unified bootstrap sequence

When bootstrap is required, the Azure companion performs this sequence:

1. Invoke `sonde-azure-companion bootstrap`.
   - **Early-exit guard (AZC-0411):** Before any provisioning work, check
     whether valid bootstrap-complete state already exists by calling
     `active_state_generation_name(state_dir)`. If it returns `Some(…)` and
     the `--force` flag is not set, print a diagnostic message to stderr
     (e.g., "Bootstrap state already present; skipping. Use --force to
     re-deploy.") and return success immediately. This allows
     `docker compose up -d` to skip redundant bootstrap on restart.
     If `--force` is set, proceed with the full sequence below.
2. Display "Generating cert…" on the modem display via the admin API.
3. Generate a self-signed ECDSA P-256 X.509 certificate and private key using
   Rust crypto libraries. The certificate has a 2-year default validity period.
   Write the certificate PEM and private-key PEM to a staging directory within
   the mounted state volume using platform-appropriate restricted permissions for
   the private key. On Unix this is mode `0600`; on Windows this is a restricted
   ACL that allows the bootstrap operator and the configured Windows service
   identity for steady-state runtime to read the key while excluding generic
   broad-access principals.
4. Base64-encode the certificate's DER public material and pass it as
   `COMPANION_CERT_BASE64` to the bootstrap container.
5. Display "Authenticating…" on the modem display.
6. Resolve the default bootstrap image reference as
   `ghcr.io/alan-jowett/sonde-azure-bootstrap:<matching companion version>`
   unless a development or test override is configured.
7. Use the Bollard crate to pull (if needed) that bootstrap image tag.
8. Create a bootstrap container and pass only dynamic inputs such as the
   generated certificate via container environment variables. The bootstrap
   flow does not use bind mounts or host-side Bicep paths.
9. The bootstrap container runs its bundled script, which executes
   `az login --use-device-code` followed by `az deployment sub create` using
   the Bicep files already bundled inside the bootstrap image.
10. Rust monitors the bootstrap container's output stream, looking for the
    device-code pattern produced by `az login --use-device-code`. Because the
    bootstrap image tag is version-matched to the companion release, the output
    format is controlled by the shipped bootstrap artifact for that release.
11. When the device code is detected, call the gateway admin
    `ShowModemDisplayMessage` RPC with a short prompt plus the exact device
    code and `persistent = true` so the code remains visible until the next
    phase replaces it.
12. Wait for the container to finish. On success, `az deployment sub create`
    produces JSON deployment outputs on stdout.
13. Display "Deploying Azure…" on the modem display (transitions from auth to
    deployment phase may overlap in the single container session).
14. Capture and parse the JSON outputs to extract `tenantId`, `clientId`,
    Storage Queue endpoint, queue names, Function App name, and deployment
    container values from the `companionBootstrapValues` output object.
15. Use the bundled prebuilt `sonde-azure-handler` package from the bootstrap
    image to populate the provisioned Function App deployment target.
16. Poll Azure for Function App activation until the uploaded package is active
    and at least one function is reported as loaded.
17. The bootstrap script then deploys the Web UI SPA content and configures the
    Entra app registration. This phase runs inside the bootstrap container
    where the Azure CLI session is still authenticated. The script:
    a. Extracts `staticWebAppName`, `staticWebAppHostname`, `companionClientId`,
       `companionTenantId`, `storageAccountName`, and `functionAppName` from
       the Bicep deployment outputs.
    b. Generates `config.json` with MSAL client ID, authority URL (derived from
       `companionTenantId`), storage account name, and function app name.
    c. Obtains the SWA deployment token via `az staticwebapp secrets list`.
    d. Deploys the bundled SPA content (including generated `config.json`) to
       the Static Web App using the pre-installed SWA CLI (`swa deploy`).
    e. Registers `https://<staticWebAppHostname>` as a SPA redirect URI on the
       Entra app registration, merging with any existing redirect URIs.
    f. Adds Azure Storage `user_impersonation` API permission to the Entra app.
    g. Exposes `api://<clientId>/user_impersonation` as an API scope on the
       Entra app registration (required for EasyAuth token validation on the
       Function App). If the scope already exists, this step is a no-op.
    If any sub-step fails, the bootstrap script exits non-zero and the
    bootstrap container reports failure.
18. Write `service-principal.json` and `storage-queues.json` to the staging
    directory with the extracted values and relative paths to the certificate
    and private-key PEM files.
19. Rename the staging directory into a new generation directory under the state
    volume, then atomically update the `.current-state` marker to point at that
    generation, leaving the previous generation untouched until the new one is
    fully committed.
20. Remove the bootstrap container.
21. Display "Bootstrap complete" on the modem display.
22. The bootstrap wrapper/entrypoint reports overall success only after
    bootstrap-complete state has been established.

If any step fails, the staging directory is cleaned up, the bootstrap
container is removed, and the existing state volume contents remain untouched,
preserving the previous working state for the next retry or runtime start.

### 4.3  Display failure handling

If the display update fails because the gateway rejects the transient display
request or no modem transport is available, bootstrap exits immediately with a
non-zero status. It does not continue to a console-only fallback.

---

## 5  Rust binary interface

> **Requirements:** AZC-0100, AZC-0102, AZC-0103, AZC-0104, AZC-0105, AZC-0201, AZC-0202, AZC-0205, AZC-0300, AZC-0301, AZC-0302, AZC-0304, AZC-0305,
> AZC-0400, AZC-0401, AZC-0402, AZC-0403, AZC-0404, AZC-0405, AZC-0406, AZC-0410, AZC-0411

The `sonde-azure-companion` binary exposes three cross-platform runtime modes
plus Windows service-management entrypoints:

1. **`run`** — default long-running runtime mode. It connects to the gateway
   connector socket and bridges connector traffic to Azure Storage Queue.
2. **`bootstrap`** — performs the unified provisioning lifecycle: self-signed
   ECDSA P-256 certificate generation, launching the version-matched
   `sonde-azure-bootstrap` image via the Bollard Docker API, and runtime
   artifact creation (`service-principal.json`, `storage-queues.json`,
   certificate PEM, private-key PEM). The Rust code monitors the bootstrap
   container's output to extract the device code and display it on the modem
   via the gateway admin API. If valid bootstrap-complete state already exists
   and `--force` is not passed, the subcommand exits successfully without
   performing any provisioning work (AZC-0411). Accepts `--force` (env:
   `SONDE_AZURE_BOOTSTRAP_FORCE`) to override this early-exit behavior.
3. **`display-message`** — helper mode used by bootstrap logic to call the
   gateway admin `ShowModemDisplayMessage` RPC with 1 to 4 lines of text.
4. **`install`** *(Windows)* — registers or updates the native Windows service
   definition used for steady-state runtime.
5. **`uninstall`** *(Windows)* — removes the native Windows service definition
   without deleting persisted companion state.
6. **service runtime entrypoint** *(Windows)* — the SCM-launched execution path
   that performs the runtime-ready check and either starts `run` or fails closed
   per AZC-0205.

The companion receives explicit runtime configuration for the Storage Queue
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
crate. `reqwest` is the first required broker transport implementation.

### 7.2  Azure Storage Queue runtime

The Azure Storage Queue transport implementation uses HTTP REST to connect to:

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

The OAuth token endpoint URL is constructed from the `login_endpoint` field
persisted in `service-principal.json`, with any trailing slash stripped before
appending `/{tenant_id}/oauth2/v2.0/token`. When the field is absent (pre-existing
deployments that predate sovereign-cloud support), the runtime defaults to
`https://login.microsoftonline.com`. When the field is present but empty or
whitespace-only, startup fails with a configuration error.

### 7.4  Transparent message bodies

The Storage Queue message body carries the raw Sonde connector payload bytes
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

---

## 8  Provisioning orchestration internals

> **Requirements:** AZC-0400, AZC-0401, AZC-0402, AZC-0403, AZC-0404, AZC-0405,
> AZC-0406, AZC-0407, AZC-0408, AZC-0409

### 8.1  Certificate generation

The `bootstrap` subcommand generates a self-signed ECDSA P-256 X.509
certificate using Rust crypto libraries already present in the workspace
(`p256`, `x509-cert`, or `rcgen`). The certificate is generated with:

- Subject CN: `sonde-azure-companion`
- Key algorithm: ECDSA P-256
- Signature algorithm: ECDSA with SHA-256
- Validity: 2 years from generation time
- Output: `cert.pem` and `key.pem` written to a staging directory within the
  state volume

The DER-encoded certificate public material is base64-encoded and passed as
`COMPANION_CERT_BASE64` to the bootstrap script, which registers it on the
Entra app registration via the Microsoft Graph API.

### 8.2  Bollard bootstrap-image integration

The `bootstrap` subcommand uses the Bollard crate to interact with the Docker
Engine API. It does not shell out to the `docker` CLI. The integration flow:

1. Connect to the Docker daemon via Bollard's auto-detection (typically
   `/var/run/docker.sock` inside the container).
2. Resolve the default bootstrap image reference as
   `ghcr.io/alan-jowett/sonde-azure-bootstrap:<matching companion version>`
   unless a development or test override is configured.
3. Pull that bootstrap image if it is not already present. If the pull fails
   (e.g., network is unavailable or the version tag is missing), bootstrap
   fails with a clear error message.
4. Create a container with environment variables for deployment parameters
   (`SONDE_AZURE_LOCATION`, `COMPANION_CERT_BASE64`, etc.).
5. Pass only dynamic bootstrap inputs such as the generated certificate into
   the container environment. The Bicep files and bootstrap script are already
   part of the bootstrap image, so bootstrap does not depend on runtime-image
   or host-side Bicep paths.
6. Start the container and stream its output (stdout/stderr).
7. Monitor the output for the device-code pattern from `az login --use-device-code`.
   When detected, extract the code and display it on the modem via the admin API.
8. Wait for the container to exit and capture the JSON deployment outputs.
9. Remove the container after completion (regardless of success or failure).

If the container fails to start or the Docker daemon is unreachable, bootstrap
fails with a descriptive error and cleans up any created-but-unstarted
container resources.

### 8.3  Bootstrap-image execution

Inside the `sonde-azure-bootstrap` image, the bootstrap runs a shell script
that:

1. Authenticates using `az login --use-device-code`. The device code and
   verification URL appear on stderr, which Rust monitors and relays to the
   modem display. Because the bootstrap image tag is version-matched to the
   companion release, the output format is controlled by the shipped bootstrap
   artifact for that release.
2. Optionally sets the target subscription if `SONDE_AZURE_SUBSCRIPTION_ID` is
   provided via `az account set --subscription`.
3. Runs `az deployment sub create` with:
   - `--location` from `SONDE_AZURE_LOCATION` (default: `eastus`)
   - `--template-file` pointing to the copied `main.bicep`
   - `--parameters companionClientId=<appId>` and
     `companionServicePrincipalObjectId=<spOid>` (resolved by the bootstrap
     script before calling Bicep) plus any additional
     parameter overrides from environment variables
   - `--query properties.outputs` to extract only the deployment outputs
   - `--output json` for machine-parseable output
4. Materializes the bundled prebuilt `sonde-azure-handler` package and deploys
   it into the Function App deployment target described by the Bicep outputs.
5. Polls Azure until the Function App reports at least one loaded function.

The deployment outputs JSON is emitted on stdout. All other `az` output
(device-code prompt, login confirmation, deployment progress) goes to stderr,
keeping stdout clean for JSON parsing.

### 8.4  Staged artifact creation

All new artifacts are written to a staging directory (`$state_dir/.staging/`)
before being committed to the state volume. This ensures that a failed
re-bootstrap does not corrupt the previous working state.

After parsing the Bicep deployment outputs, the bootstrap creates in the
staging directory:

1. **`cert.pem`** and **`key.pem`** — already written during certificate
   generation (step 3 of §4.2).
2. **`service-principal.json`** containing:
   ```json
   {
     "tenant_id": "<from companionBootstrapValues.tenantId>",
     "client_id": "<from companionBootstrapValues.clientId>",
     "login_endpoint": "<from companionBootstrapValues.loginEndpoint>",
     "certificate_path": "cert.pem",
     "private_key_path": "key.pem"
   }
   ```
3. **`storage-queues.json`** containing:
   ```json
   {
     "queue_endpoint": "<from companionBootstrapValues.storageQueueEndpoint>",
     "upstream_queue": "<from companionBootstrapValues.upstreamQueue>",
     "downstream_queue": "<from companionBootstrapValues.downstreamQueue>"
   }
   ```

Only after all staging writes succeed does the bootstrap atomically move the
staged files into the state volume root, replacing any previous artifacts.
If any write fails (e.g., disk full), the staging directory is removed and the
previous state remains intact.

The `key.pem` file MUST be written with platform-appropriate restricted access
to protect the private key material. On Unix this is owner-only read permissions
(mode `0600`). On Windows this is a restricted ACL that permits read access to
the bootstrap operator and the configured Windows service identity for
steady-state runtime while excluding generic broad-access principals such as
`Everyone` and `Users`.

The runtime's `check-runtime-ready` path is updated to also check for the
persisted Storage Queue configuration file (`storage-queues.json`) as an alternative
to environment variables, enabling a fully automated startup after bootstrap.
Environment variables, if set, override the persisted file values.

### 8.5  Bootstrap-image contents

The bootstrap Dockerfile includes:

```dockerfile
COPY deploy/bicep/ /opt/sonde/deploy/bicep/
COPY deploy/web-ui/index.html deploy/web-ui/app.js deploy/web-ui/style.css deploy/web-ui/staticwebapp.config.json /opt/sonde/deploy/web-ui/
```

It also includes the bootstrap script that runs `az login --use-device-code`
and Azure deployment inside the bootstrap image. The `jq` utility is installed
for JSON manipulation during SPA deployment (Entra redirect URI merging), and
Node.js/npm is installed and a pinned version of the SWA CLI
(`@azure/static-web-apps-cli`) is pre-installed globally for SPA content
deployment.

This bundles the complete Azure provisioning surface and the Web UI SPA
content into the dedicated bootstrap image. The runtime companion image no
longer needs to carry these bootstrap-only assets.

### 8.6  Docker socket requirements

The `bootstrap.sh` entrypoint script is updated to bind-mount the Docker
socket from the host:

```sh
-v /var/run/docker.sock:/var/run/docker.sock
```

This is required only during bootstrap. Normal runtime operation after
bootstrap does not need Docker socket access.

### 8.7  Progress display phases

The bootstrap displays these messages on the modem via the admin API:

| Phase | Modem display | stderr log | `persistent` |
|-------|--------------|------------|--------------|
| Certificate generation | "Generating cert..." | "Generating ECDSA P-256 self-signed certificate" | false |
| Bootstrap image start | "Authenticating..." | "Starting sonde-azure-bootstrap for device-code auth" | false |
| Device code received | "Azure login" + device code | "Device auth: open {uri} and enter code {code}" | **true** |
| Bicep deployment | "Deploying Azure..." | "Running Bicep deployment" | false |
| Handler deployment | "Deploying handler..." | "Deploying bundled Azure handler package" | false |
| Entra configuration | "Configuring Entra..." | "Configuring Entra app registration for Web UI" | false |
| Config writing | "Writing config..." | "Writing runtime artifacts to state volume" | false |
| Bootstrap complete | "Bootstrap complete" | "Bootstrap completed successfully" | false |
| Bootstrap failure | "Bootstrap failed" | Error details | false |

The device-code display uses `persistent = true` so the code remains visible
on the OLED until authentication completes and the next phase replaces it,
avoiding the 60-second timeout that would otherwise clear the screen before
the operator finishes the device-code login flow.

Sub-phase transitions after Bicep deployment are signaled by the bootstrap
container via structured stderr markers (`__SONDE_AZURE_DEPLOYING_HANDLER__`
and `__SONDE_AZURE_CONFIGURING_ENTRA__`). The companion watches for these
markers alongside the existing `__SONDE_AZURE_DEPLOYMENT_START__` marker and
updates the modem display accordingly.
