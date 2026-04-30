<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Azure Companion Validation Plan

> **Document status:** Draft
> **Scope:** Validation for the Azure companion container, bootstrap-state
> detection, gateway admin/connector integration, provisioning orchestration
> (certificate generation, Bicep deployment via Docker API, runtime artifact
> creation), and Azure Service Bus bridge.
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

### T-AZC-0101  Startup enters bootstrap when provisioning artifacts are missing

**Validates:** AZC-0101, AZC-0102, AZC-0200, AZC-0201

**Procedure:**
1. Create an empty temporary directory to use as the mounted state volume.
2. Provide valid queue configuration to the bootstrap script.
3. Invoke the Azure companion bootstrap entrypoint with that directory mounted as the state volume.
4. Assert: the bootstrap path runs before the long-running runtime starts.
5. Assert: the bootstrap path invokes the Rust `bootstrap` flow and requests a device code from the configured OAuth endpoint.

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

### T-AZC-0109  Runtime requires explicit Service Bus queue configuration

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

### T-AZC-0115  Alpine image reaches the concrete `azservicebus` runtime path

**Validates:** AZC-0100, AZC-0303, AZC-0304, AZC-0305

**Procedure:**
1. Build the Azure companion Alpine image.
2. Prepare bootstrap-complete state and explicit queue configuration.
3. Start the runtime in the container with the concrete `azservicebus` transport selected and with a deliberately unreachable or test-only Service Bus endpoint.
4. Assert: the runtime reaches the concrete Azure transport initialization path and fails, if it fails, with an Azure transport/connectivity error rather than a missing binary, missing dynamic library, or unsupported-platform error.
5. Assert: the runtime does not fall back to device-code login in this bootstrap-complete case.

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

### T-AZC-0401  Bootstrap uses Bollard to run Azure CLI container

**Validates:** AZC-0401, AZC-0402

**Procedure:**
1. Start the bootstrap subcommand with a mock Docker API server (Bollard supports custom connection).
2. Assert: bootstrap sends Docker API requests to create and start a container using the pinned Azure CLI image digest.
3. Assert: bootstrap does not invoke the `docker` CLI binary.
4. Assert: the container creation request includes bind mounts for the bundled Bicep files and generated certificate.
5. Assert: bootstrap captures the container's stdout output containing Bicep deployment JSON.
6. Assert: bootstrap removes the container after completion.

---

### T-AZC-0402  Companion image bundles Bicep files

**Validates:** AZC-0402

**Procedure:**
1. Build the Azure companion Docker image.
2. Run `docker run --rm <image> ls /opt/sonde/deploy/bicep/`.
3. Assert: the listing includes `main.bicep`, `bicepconfig.json`, and the `modules/` directory.
4. Assert: the `modules/` directory contains the expected Bicep module files.

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

### T-AZC-0404  Bootstrap persists Service Bus configuration

**Validates:** AZC-0404

**Procedure:**
1. Run the bootstrap subcommand with stubbed Bicep outputs containing known namespace and queue names.
2. Assert: the Service Bus namespace, upstream queue, and downstream queue values are persisted in the state volume.
3. Restart the container without the Service Bus environment variables.
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
2. Assert: the Bicep deployment command within the Azure CLI container targets the specified subscription.
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

**Validates:** AZC-0401, AZC-0405

**Procedure:**
1. Start bootstrap with Bollard configured to reject the image pull (simulated network failure).
2. Assert: bootstrap fails with a non-zero exit status.
3. Assert: the error message identifies the image pull failure.
4. Assert: the modem displays an error indication.
5. Assert: no Azure CLI container is left running.

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
3. Assert: the private key file has owner-only read permissions (mode 0600 or equivalent).
4. Assert: the certificate file (`cert.pem`) is world-readable.

---

### T-AZC-0414  Access token is not logged

**Validates:** AZC-0401

**Procedure:**
1. Run bootstrap with a stubbed device-flow that returns a known token value.
2. Capture all stderr and stdout output from the bootstrap process.
3. Assert: the access token value does not appear in any log output.
4. Assert: the access token is not persisted to any file in the state volume.
