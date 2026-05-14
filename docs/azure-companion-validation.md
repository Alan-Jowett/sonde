<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Azure Companion Validation Plan

> **Document status:** Draft
> **Scope:** Validation for the Azure companion deployment surfaces (Linux
> runtime container and Windows native service), the dedicated bootstrap image,
> bootstrap-state detection, gateway admin/connector integration, provisioning
> orchestration (certificate generation, bootstrap-image execution via Docker
> API, runtime artifact creation), and Azure Storage Queue bridge.
> **Audience:** Implementers and reviewers validating the Azure companion
> bootstrap and runtime bridge behavior.
> **Related:** [azure-provisioning-validation.md](azure-provisioning-validation.md),
> [azure-companion-requirements.md](azure-companion-requirements.md),
> [azure-companion-design.md](azure-companion-design.md),
> [gateway-validation.md](gateway-validation.md)

---

## 1  Test cases

### T-AZC-0100  Azure companion container image smoke test

**Validates:** AZC-0100

**Procedure:**
1. Build the Azure companion Docker image from the repository Dockerfile.
2. Run `docker run --rm <image> sonde-azure-companion --help`.
3. Run `docker run --rm <image> sonde-azure-companion bootstrap --help`.
4. Run `docker run --rm <image> sonde-azure-companion run --help`.
5. Assert: all commands succeed.

---

### T-AZC-0123  Azure bootstrap image smoke test

**Validates:** AZC-0106, AZC-0402

**Procedure:**
1. Build the `sonde-azure-bootstrap` Docker image from the repository Dockerfile.
2. Run `docker run --rm --entrypoint sh <image> -c "az version >/dev/null"`.
3. Run `docker run --rm --entrypoint sh <image> -c "ls /opt/sonde/deploy/bicep/"`.
4. Assert: the image contains working Azure CLI tooling plus the bundled Bicep deployment files.
5. Assert: the listing includes `main.bicep`, `bicepconfig.json`, and the `modules/` directory.

---

### T-AZC-0101  Startup enters bootstrap when provisioning artifacts are missing

**Validates:** AZC-0101, AZC-0102, AZC-0200, AZC-0201

**Procedure:**
1. Create an empty temporary directory to use as the mounted state volume.
2. Provide valid queue configuration to the bootstrap script.
3. Invoke the Azure companion bootstrap entrypoint with that directory mounted as the state volume.
4. Assert: the bootstrap path runs the dedicated bootstrap image before the long-running runtime starts.
5. Assert: the bootstrap path invokes the Rust `bootstrap` flow, parses the device code from Azure CLI stderr, and forwards it to the modem display.

---

### T-AZC-0102  Startup enters bootstrap when queue configuration is missing

**Validates:** AZC-0102, AZC-0200, AZC-0302

**Procedure:**
1. Populate the mounted state volume with the expected provisioning artifacts.
2. Start the Azure companion entrypoint without the required namespace and queue configuration.
3. Assert: startup enters bootstrap instead of normal runtime mode.
4. Assert: runtime bridge startup is not reported as successful.

---

### T-AZC-0103  Bootstrapped state skips device login

**Validates:** AZC-0102, AZC-0200, AZC-0204, AZC-0302

**Procedure:**
1. Populate the mounted state volume with the expected provisioning artifacts.
2. Provide the required namespace and queue configuration.
3. Start the Azure companion entrypoint.
4. Assert: startup skips the bootstrap path.
5. Assert: the long-running runtime starts directly.
6. Assert: no device-code login attempt is made.

---

### T-AZC-0104  Device code is shown through the gateway admin display RPC

**Validates:** AZC-0202, AZC-0300

**Procedure:**
1. Start a test gateway exposing the admin socket and instrument the admin `ShowModemDisplayMessage` RPC.
2. Start the Azure companion bootstrap path with the Rust device-flow endpoint stubbed to emit a known device code.
3. Assert: the Azure companion connects to the gateway admin socket.
4. Assert: it issues `ShowModemDisplayMessage` through the admin API with text containing both a short prompt and the exact known device code.
5. Assert: the bootstrap path does not attempt raw modem control.

---

### T-AZC-0105  Display failure aborts bootstrap

**Validates:** AZC-0203

**Procedure:**
1. Start a test gateway whose admin `ShowModemDisplayMessage` RPC returns `FAILED_PRECONDITION` in one sub-case and `UNAVAILABLE` in another.
2. Start the Azure companion bootstrap path with the Rust device-flow endpoint stubbed to emit a device code.
3. Assert: bootstrap exits with a non-zero status in both sub-cases.
4. Assert: bootstrap reports the display failure to the operator.
5. Assert: bootstrap does not continue with a console-only fallback.

---

### T-AZC-0106  Azure login failure aborts bootstrap

**Validates:** AZC-0201

**Procedure:**
1. Create an empty auth state volume.
2. Invoke the Azure companion bootstrap path with the Rust device-flow token polling endpoint stubbed to fail.
3. Assert: bootstrap exits with a non-zero status.
4. Assert: the long-running runtime is not started.
5. Assert: bootstrap does not report success.

---

### T-AZC-0107  Bootstrap does not report success until bootstrap-complete state exists

**Validates:** AZC-0102, AZC-0201

**Procedure:**
1. Start the Azure companion bootstrap path with the Rust device-flow endpoint stubbed to complete successfully.
2. Prevent the out-of-scope provisioning handoff from creating bootstrap-complete state.
3. Assert: bootstrap does not report success.
4. Assert: the long-running runtime is not started.
5. Assert: startup remains blocked or fails closed until bootstrap-complete state exists.

---

### T-AZC-0108  Long-running runtime connects through the gateway connector socket

**Validates:** AZC-0301

**Procedure:**
1. Start a test gateway exposing the connector socket.
2. Provide bootstrap-complete state and valid queue configuration.
3. Start the long-running Azure companion runtime.
4. Assert: the runtime connects to the configured connector socket and keeps the connector session open.
5. Assert: runtime startup does not require a legacy companion socket.

---

### T-AZC-0109  Runtime requires explicit Storage Queue configuration

**Validates:** AZC-0302

**Procedure:**
1. Prepare bootstrap-complete state in the mounted state volume.
2. Start the runtime with the namespace missing, then with the upstream queue missing, then with the downstream queue missing.
3. Assert: each sub-case fails closed before the runtime begins bridging traffic.
4. Assert: the failure identifies missing configuration rather than silently selecting defaults.

---

### T-AZC-0110  Upstream connector payloads are published transparently

**Validates:** AZC-0303, AZC-0304, AZC-0306

**Procedure:**
1. Start the runtime with a test double for the broker transport layer.
2. Inject representative gateway-originated connector payloads through the local connector socket, covering at least one `ACTUAL_STATE`, one `APP_DATA`, and one `CONNECTOR_HEALTH` message.
3. Assert: the transport layer receives the raw connector payload bytes unchanged.
4. Assert: all upstream message types are routed to the configured upstream queue.
5. Assert: any broker metadata used is supplementary and does not replace the raw payload body.

---

### T-AZC-0111  Downstream desired-state payloads are forwarded transparently

**Validates:** AZC-0303, AZC-0304, AZC-0307

**Procedure:**
1. Start the runtime with a test double for the broker transport layer and a test gateway connector socket.
2. Deliver a representative raw `DESIRED_STATE` connector payload from the configured downstream queue.
3. Assert: the runtime writes the raw payload bytes unchanged to the local gateway connector socket.
4. Assert: the downstream queue path is not used for upstream state or app-data traffic.

---

### T-AZC-0112  Downstream settlement waits for local connector handoff

**Validates:** AZC-0308

**Procedure:**
1. Start the runtime with broker and local connector test doubles that let the test control when the local connector write succeeds or fails.
2. Deliver one desired-state message from the downstream queue.
3. In the success sub-case, allow the local connector write to complete and assert the broker message is then settled successfully.
4. In the failure sub-case, force the local connector write to fail and assert the broker message is not reported as successfully processed.
5. Assert: no extra synchronous acknowledgement path is required from the gateway.

---

### T-AZC-0113  Runtime uses certificate-based Azure authentication after bootstrap

**Validates:** AZC-0305

**Procedure:**
1. Prepare bootstrap-complete state containing the required certificate PEM, private-key PEM, and service-principal metadata.
2. Start the runtime with a broker test double that records the credential path used.
3. Assert: runtime startup uses the provisioned certificate and private-key material rather than entering device-code login.
4. Remove or corrupt the certificate or private-key material and assert: runtime startup fails closed.

---

### T-AZC-0114  Detected broker or local connector failures are surfaced

**Validates:** AZC-0309

**Procedure:**
1. Start the runtime with test doubles that can inject an upstream publish failure, a downstream receive failure, a settlement failure, and a local connector write failure.
2. Trigger each failure mode independently.
3. Assert: each failure is surfaced through logging, process status, or both.
4. Assert: the runtime does not silently claim success after a detected bridge failure.

---

### T-AZC-0115  Alpine image reaches the concrete `reqwest` runtime path

**Validates:** AZC-0100, AZC-0303, AZC-0304, AZC-0305

**Procedure:**
1. Build the Azure companion Alpine image.
2. Prepare bootstrap-complete state and explicit queue configuration.
3. Start the runtime in the container with the concrete `reqwest` transport selected and with a deliberately unreachable or test-only Storage Queue endpoint.
4. Assert: the runtime reaches the concrete Azure transport initialization path and fails, if it fails, with an Azure transport/connectivity error rather than a missing binary, missing dynamic library, or unsupported-platform error.
5. Assert: the runtime does not fall back to device-code login in this bootstrap-complete case.

---

### T-AZC-0116  Live Azure workflow publishes upstream payloads through the real bridge

**Validates:** AZC-0310, AZC-0311

**Procedure:**
1. Dispatch the live Azure validation workflow after provisioning a disposable Azure stack.
2. Start the real `sonde-azure-companion` runtime in bootstrap-complete mode against the workflow's connector harness.
3. Inject at least one representative upstream connector payload through the harness.
4. Read the configured upstream Storage Queue.
5. Assert: the queue receives the raw connector payload bytes unchanged.
6. Assert: the runtime under test is the real `sonde-azure-companion`, not a broker mock.

---

### T-AZC-0117  Live Azure workflow delivers downstream desired state unchanged

**Validates:** AZC-0310, AZC-0311

**Procedure:**
1. Dispatch the live Azure validation workflow after provisioning a disposable Azure stack.
2. Start the real `sonde-azure-companion` runtime against the connector harness.
3. Enqueue a representative raw desired-state connector payload into the configured downstream Storage Queue.
4. Observe the payload received by the connector harness.
5. Assert: the harness receives the raw payload bytes unchanged.
6. Assert: the workflow does not require a full `sonde-gateway` process to validate this handoff.

---

### T-AZC-0118  Live Azure workflow settles downstream messages only after local handoff

**Validates:** AZC-0311

**Procedure:**
1. Dispatch the live Azure validation workflow with a connector harness that can delay or reject local writes.
2. Enqueue one desired-state message in the downstream queue.
3. In the success sub-case, allow the harness write to complete and assert the Storage Queue message is then settled successfully.
4. In the failure sub-case, force the harness write to fail and assert the message is not reported as successfully processed.
5. Assert: detected transport or local handoff failure is surfaced in workflow logs or process status.

---

### T-AZC-0119  `sonde-azure-companion install` registers the Windows service

**Validates:** AZC-0103, AZC-0105

**Procedure:**
1. On a Windows machine with the companion binary on PATH, open an elevated PowerShell prompt.
2. Run `sonde-azure-companion install`.
3. Assert: the command exits with code 0 and prints a success message.
4. Run `sc.exe qc sonde-azure-companion`.
5. Assert: the service exists with `START_TYPE` = `AUTO_START`.
6. Assert: the binary path references the native `sonde-azure-companion.exe` executable rather than the Linux container bootstrap wrapper.
7. Run `sonde-azure-companion install` again.
8. Assert: the command exits with code 0 and updates the existing service definition idempotently.
9. Run `sonde-azure-companion install` from a non-elevated prompt.
10. Assert: the command exits with a clear privilege error instead of modifying the service registration.

---

### T-AZC-0120  `sonde-azure-companion uninstall` removes the Windows service idempotently

**Validates:** AZC-0105

**Procedure:**
1. Prerequisite: a service registered via `sonde-azure-companion install`.
2. Start the service: `sc.exe start sonde-azure-companion`.
3. Run `sonde-azure-companion uninstall` from an elevated prompt.
4. Assert: the command exits with code 0.
5. Run `sc.exe query sonde-azure-companion`.
6. Assert: the service is not found.
7. Assert: `%ProgramData%\sonde-azure-companion\` is preserved on disk.
8. Run `sonde-azure-companion uninstall` again.
9. Assert: the command exits with code 0 and prints an informational "not registered" message.
10. Re-register the service, then run `sonde-azure-companion uninstall` from a non-elevated prompt.
11. Assert: the command exits with a clear privilege error instead of deleting the service.

---

### T-AZC-0121  Windows service startup fails closed when bootstrap-complete state is missing

**Validates:** AZC-0103, AZC-0205

**Procedure:**
1. Register the service via `sonde-azure-companion install`.
2. Expose the gateway admin and connector named pipes at their default Windows paths.
3. Ensure `%ProgramData%\sonde-azure-companion\` does not contain bootstrap-complete state.
4. Start the service: `sc.exe start sonde-azure-companion`.
5. Assert: the service does not reach steady `RUNNING` runtime-bridge operation.
6. Assert: service diagnostics clearly identify the missing provisioning artifacts or queue configuration.
7. Assert: the startup path does not attempt to pull or run the bootstrap image automatically.
8. Populate bootstrap-complete state in `%ProgramData%\sonde-azure-companion\` and restart the service.
9. Assert: the service now starts the runtime bridge normally and connects through the default named-pipe paths.
10. Stop the service through SCM.
11. Assert: the service stops cleanly without deleting the populated bootstrap-complete state.

---

### T-AZC-0122  MSI companion feature is optional and defaults off

**Validates:** AZC-0104

**Procedure:**
1. Install the MSI on a clean Windows VM and inspect the feature-selection UI.
2. Assert: the Azure companion component is presented as an optional choice and is unchecked by default.
3. Complete installation without selecting the component.
4. Assert: `sc.exe query sonde-azure-companion` reports that the service is not installed.
5. Re-run installation with the component selected.
6. Assert: the companion binary is installed and `sc.exe qc sonde-azure-companion` succeeds.
7. Assert: the installer does not prompt for Azure tenant, subscription, namespace, queue, or certificate settings.
8. Perform a silent install selecting the companion feature via MSI property/feature selection.
9. Assert: the companion service is installed successfully without interactive UI.

---

### T-AZC-0400  Bootstrap generates ECDSA P-256 self-signed certificate

**Validates:** AZC-0400

**Procedure:**
1. Start the bootstrap subcommand with device-flow and Bicep deployment stubbed.
2. Assert: bootstrap creates `cert.pem` and `key.pem` in the mounted state volume.
3. Parse the generated `cert.pem` and assert: the certificate uses ECDSA P-256 key algorithm.
4. Assert: the certificate's validity period is approximately 2 years from the generation time.
5. Assert: the certificate is self-signed (issuer equals subject).
6. Assert: the DER-encoded public material can be base64-encoded without error.

---

### T-AZC-0401  Bootstrap uses Bollard to run the bootstrap image

**Validates:** AZC-0106, AZC-0401, AZC-0402

**Procedure:**
1. Start the bootstrap subcommand with a mock Docker API server (Bollard supports custom connection).
2. Assert: bootstrap sends Docker API requests to create and start a container using the version-tagged `ghcr.io/alan-jowett/sonde-azure-bootstrap` image that matches the companion release version.
3. Assert: bootstrap does not invoke the `docker` CLI binary.
4. Assert: bootstrap passes only dynamic bootstrap inputs such as the generated certificate into the bootstrap container environment before running the deployment.
5. Assert: bootstrap captures the container's stdout output containing Bicep deployment JSON.
6. Assert: bootstrap removes the container after completion.

---

### T-AZC-0402  Bootstrap image bundles Bicep files

**Validates:** AZC-0402

**Procedure:**
1. Build the `sonde-azure-bootstrap` Docker image.
2. Run `docker run --rm <image> ls /opt/sonde/deploy/bicep/`.
3. Assert: the listing includes `main.bicep`, `bicepconfig.json`, and the `modules/` directory.
4. Assert: the `modules/` directory contains the expected Bicep module files.
5. Assert: the image also contains the bundled prebuilt Azure handler package intended for Function App deployment.

---

### T-AZC-0403  Bootstrap writes service-principal.json from Bicep outputs

**Validates:** AZC-0403

**Procedure:**
1. Run the bootstrap subcommand with device-flow stubbed to succeed and Bicep deployment stubbed to return known output values (tenantId, clientId, namespace, queue names).
2. Assert: `service-principal.json` exists in the state volume after bootstrap completes.
3. Parse `service-principal.json` and assert: it contains `tenant_id`, `client_id`, `certificate_path`, and `private_key_path`.
4. Assert: `tenant_id` and `client_id` match the stubbed Bicep output values.
5. Assert: `certificate_path` and `private_key_path` are relative paths that resolve to existing PEM files in the state volume.
6. Assert: `check-runtime-ready` succeeds after bootstrap completes.

---

### T-AZC-0404  Bootstrap persists Storage Queue configuration

**Validates:** AZC-0404

**Procedure:**
1. Run the bootstrap subcommand with stubbed Bicep outputs containing known namespace and queue names.
2. Assert: the Storage Queue endpoint, upstream queue, and downstream queue values are persisted in the state volume.
3. Restart the container without the Storage Queue environment variables.
4. Assert: the runtime can read the persisted configuration and startup succeeds (or reaches the Azure transport path).

---

### T-AZC-0405  Bootstrap displays progress on modem

**Validates:** AZC-0405

**Procedure:**
1. Start a test gateway exposing the admin socket and instrument the admin `ShowModemDisplayMessage` RPC.
2. Run the bootstrap subcommand with device-flow and Bicep deployment stubbed.
3. Assert: at least four distinct modem display updates are received during bootstrap.
4. Assert: the updates include messages for authentication, certificate generation, deployment, and completion phases.
5. Assert: each phase also emits a corresponding stderr log message.
6. Run the bootstrap subcommand with a forced failure in the Bicep deployment phase.
7. Assert: the modem displays an error indication before bootstrap exits.

---

### T-AZC-0406  Bootstrap fails if Docker socket is inaccessible

**Validates:** AZC-0406

**Procedure:**
1. Start the bootstrap subcommand without the Docker socket mounted.
2. Assert: bootstrap fails with a non-zero exit status.
3. Assert: the error message identifies the inaccessible Docker socket as the cause.
4. Assert: bootstrap does not hang indefinitely waiting for Docker connectivity.

---

### T-AZC-0407  Re-bootstrap on already-provisioned system succeeds

**Validates:** AZC-0407

**Procedure:**
1. Run bootstrap to completion with stubbed services, creating all runtime artifacts.
2. Record the certificate fingerprint and `service-principal.json` content.
3. Re-run bootstrap with the same stubbed services.
4. Assert: bootstrap completes successfully.
5. Assert: the certificate PEM file has been regenerated (different fingerprint).
6. Assert: `service-principal.json` has been rewritten.
7. Assert: `check-runtime-ready` succeeds after re-bootstrap.

---

### T-AZC-0408  Bootstrap accepts optional subscription ID

**Validates:** AZC-0408

**Procedure:**
1. Run bootstrap with `SONDE_AZURE_SUBSCRIPTION_ID` set to a known value.
2. Assert: the Bicep deployment command within the bootstrap container targets the specified subscription.
3. Run bootstrap without `SONDE_AZURE_SUBSCRIPTION_ID`.
4. Assert: the Bicep deployment uses the default subscription from the device-login session.

---

### T-AZC-0409  Failed re-bootstrap preserves previous working state

**Validates:** AZC-0407, AZC-0403

**Procedure:**
1. Run bootstrap to completion with stubbed services, creating all runtime artifacts.
2. Record the content of `service-principal.json`, `cert.pem`, and `key.pem`.
3. Re-run bootstrap with the Bicep deployment phase stubbed to fail.
4. Assert: bootstrap exits with a non-zero status.
5. Assert: the previous `service-principal.json`, `cert.pem`, and `key.pem` are unchanged.
6. Assert: `check-runtime-ready` still succeeds with the previous artifacts.
7. Assert: no `.staging/` directory remains in the state volume.

---

### T-AZC-0410  Bootstrap fails cleanly on image pull failure

**Validates:** AZC-0106, AZC-0401, AZC-0405

**Procedure:**
1. Start bootstrap with Bollard configured to reject the bootstrap image pull (simulated network failure or missing version tag).
2. Assert: bootstrap fails with a non-zero exit status.
3. Assert: the error message identifies the image pull failure.
4. Assert: the modem displays an error indication.
5. Assert: no bootstrap container is left running.

---

### T-AZC-0411  Bootstrap fails cleanly on container start failure

**Validates:** AZC-0401, AZC-0406

**Procedure:**
1. Start bootstrap with a mock Docker API that accepts container creation but rejects the start request.
2. Assert: bootstrap fails with a non-zero exit status.
3. Assert: bootstrap cleans up the created-but-unstarted container.
4. Assert: previous state volume contents are preserved.

---

### T-AZC-0412  Bootstrap fails cleanly on state volume write failure

**Validates:** AZC-0403, AZC-0407

**Procedure:**
1. Start bootstrap with a state volume configured to reject writes (read-only mount or simulated disk-full).
2. Assert: bootstrap fails with a non-zero exit status.
3. Assert: the error message identifies the write failure.
4. Assert: no partial artifacts are left in the state volume.

---

### T-AZC-0413  Private key file permissions

**Validates:** AZC-0400

**Procedure:**
1. Run bootstrap to completion.
2. Inspect the file permissions of `key.pem` in the state volume.
3. Assert: on Unix, the private key file has owner-only read permissions (mode 0600).
4. Assert: on Windows, the private key file ACL grants read access to the bootstrap operator and the configured Windows service identity for steady-state runtime, and does not grant read access to generic broad-access principals such as `Everyone` or `Users`.
5. Assert: the certificate file (`cert.pem`) is world-readable or otherwise readable by the normal runtime without requiring elevated access.

---

### T-AZC-0414  Windows native bootstrap completes and produces service-readable key material

**Validates:** AZC-0201, AZC-0400

**Procedure:**
1. On a Windows machine with Docker available, run `sonde-azure-companion bootstrap` from an elevated PowerShell prompt with the required Azure environment variables set and the gateway admin pipe exposed.
2. Assert: the command completes successfully without emitting the Unix-only private-key creation failure.
3. Assert: the generated `cert.pem`, `key.pem`, `service-principal.json`, and `storage-queues.json` files exist in `%ProgramData%\sonde-azure-companion\` (or the configured state directory).
4. Inspect the ACL on `key.pem`.
5. Assert: the ACL permits read access to the bootstrap operator and the configured Windows service identity for steady-state runtime.
6. If the `sonde-azure-companion` Windows service is not yet installed, install it; then start or restart the service against the generated state.
7. Assert: the service reaches the normal runtime path without failing due to private-key access.

---

### T-AZC-0415  Access token is not logged

**Validates:** AZC-0401

**Procedure:**
1. Run bootstrap with a stubbed device-flow that returns a known token value.
2. Capture all stderr and stdout output from the bootstrap process.
3. Assert: the access token value does not appear in any log output.
4. Assert: the access token is not persisted to any file in the state volume.

---

### T-AZC-0416  Bootstrap image override is honored for development and test flows

**Validates:** AZC-0106, AZC-0401

**Procedure:**
1. Configure `sonde-azure-companion` with an explicit bootstrap image override reference such as `sonde-azure-bootstrap:test-override`, using either `SONDE_AZURE_BOOTSTRAP_IMAGE` or `--bootstrap-image`.
2. Start bootstrap with a mock Docker API server (or equivalent traceable test double).
3. Assert: the Docker API requests use the configured override image reference rather than the default version-matched release tag.
4. Assert: bootstrap reaches the container-creation path using the override image reference when the override image is available.

---

### T-AZC-0417  Bootstrap deploys the bundled Azure handler package and waits for activation

**Validates:** AZC-0409, AZC-0402

**Procedure:**
1. Run bootstrap with device auth and Azure deployment stubbed or directed at a disposable Azure test stack whose Function App can be queried after deployment.
2. Assert: bootstrap uses the prebuilt `sonde-azure-handler` package bundled in the version-matched bootstrap image rather than requiring a separate operator-supplied upload or an ad hoc local build.
3. After the package-deployment phase, query Azure for the provisioned Function App.
4. Assert: bootstrap does not report success until Azure reports at least one loaded function for that Function App.
5. Force package activation to fail or never complete.
6. Assert: bootstrap exits non-zero and does not report success-shaped completion.

---

### T-AZC-0418  Bootstrap deploys SPA content and configures Entra app for Web UI

**Validates:** AZC-0410

**Procedure:**
1. Run bootstrap with device auth stubbed and Bicep deployment stubbed to return known output values including `staticWebAppName`, `staticWebAppHostname`, `companionClientId`, `companionTenantId`, `storageAccountName`, and `functionAppName`.
2. Assert: bootstrap generates `config.json` containing the correct `msalClientId`, `msalAuthority` (derived from `companionTenantId`), `storageAccount`, and `functionAppName` values from the stubbed outputs.
3. Assert: the bootstrap script attempts to deploy SPA content (HTML, JS, CSS, config.json) to the Static Web App named in the outputs.
4. After bootstrap completes, assert: the Static Web App serves the SPA content at its default hostname without any additional manual deployment step.
5. Assert: the bootstrap script registers `https://<staticWebAppHostname>` as a SPA redirect URI on the Entra app registration.
6. Assert: existing SPA redirect URIs are preserved (not overwritten) during the registration.
7. Assert: the bootstrap script adds the Azure Storage `user_impersonation` API permission to the Entra app.
8. Assert: the bootstrap script exposes `api://<clientId>/user_impersonation` as an API scope on the Entra app registration.
9. Assert: if SPA deployment fails, bootstrap exits non-zero.
10. Re-run bootstrap with the same stubbed outputs.
11. Assert: the SPA deployment and Entra configuration succeed idempotently (including the API scope exposure in step 8).
