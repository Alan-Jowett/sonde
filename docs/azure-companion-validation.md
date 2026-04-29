<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Azure Companion Validation Plan

> **Document status:** Draft
> **Scope:** Validation for the Azure companion container, bootstrap-state
> detection, gateway admin/connector integration, and Azure Service Bus bridge.
> The internals of Azure provisioning that create the runtime certificate and
> broker resources are outside this document's scope.
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
3. Run `docker run --rm <image> sonde-azure-companion bootstrap-auth --help`.
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
5. Assert: the bootstrap path invokes the Rust `bootstrap-auth` flow and requests a device code from the configured OAuth endpoint.

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
