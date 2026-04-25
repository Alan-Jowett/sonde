<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Azure Companion Validation Plan

> **Document status:** Draft
> **Scope:** Validation for the initial Azure companion slice: container packaging, bootstrap scripts, gateway companion display integration, and Azure device-code login state handling.
> **Audience:** Implementers and reviewers validating the Azure companion bootstrap flow.
> **Related:** [azure-companion-requirements.md](azure-companion-requirements.md), [azure-companion-design.md](azure-companion-design.md), [gateway-validation.md](gateway-validation.md)

---

## 1  Test cases

### T-AZC-0100  Azure companion container image smoke test

**Validates:** AZC-0100

**Procedure:**
1. Build the Azure companion Docker image from the repository Dockerfile.
2. Run `docker run --rm <image> sonde-azure-companion --help`.
3. Run `docker run --rm <image> sonde-azure-companion bootstrap-auth --help`.
4. Assert: both commands succeed.

---

### T-AZC-0101  Bootstrap initializes the mounted state volume and enters device auth

**Validates:** AZC-0101, AZC-0102, AZC-0200, AZC-0201

**Procedure:**
1. Create an empty temporary directory to use as the mounted state volume.
2. Invoke the provided Azure companion bootstrap script with that directory mounted as the state volume and with the Rust device-flow HTTP endpoints stubbed or intercepted for test control.
3. Assert: the bootstrap path runs before the long-running process starts.
4. Assert: the bootstrap path invokes the Rust `bootstrap-auth` flow and requests a device code from the configured OAuth endpoint.
5. Complete the stubbed login successfully.
6. Assert: the bootstrap path starts the long-running Azure companion process without requiring manual in-container steps.

---

### T-AZC-0102  Device code is shown through the gateway companion display RPC

**Validates:** AZC-0202, AZC-0300

**Procedure:**
1. Start a test gateway exposing the companion socket and instrument the companion `ShowModemDisplayMessage` RPC.
2. Start the Azure companion bootstrap path with the Rust device-flow endpoint stubbed to emit a known device code.
3. Assert: the Azure companion connects to the gateway companion socket.
4. Assert: it issues `ShowModemDisplayMessage` through the companion API with text containing both a short prompt and the exact known device code.
5. Assert: the bootstrap path does not attempt raw modem control.

---

### T-AZC-0103  Display failure aborts bootstrap

**Validates:** AZC-0203

**Procedure:**
1. Start a test gateway whose companion `ShowModemDisplayMessage` RPC returns `FAILED_PRECONDITION` in one sub-case and `UNAVAILABLE` in another.
2. Start the Azure companion bootstrap path with the Rust device-flow endpoint stubbed to emit a device code.
3. Assert: bootstrap exits with a non-zero status in both sub-cases.
4. Assert: bootstrap reports the display failure to the operator.
5. Assert: bootstrap does not continue with a console-only fallback.

---

### T-AZC-0104  Previously used state volume still enters device login

**Validates:** AZC-0101, AZC-0200, AZC-0204

**Procedure:**
1. Populate the mounted state volume with unrelated placeholder files representing future provisioning output.
2. Start the Azure companion container with that mounted directory.
3. Assert: startup still performs Azure device-code login.
4. Assert: startup issues a new modem display request for the new device code.
5. Assert: the long-running Azure companion process starts normally after successful auth.
6. Assert: startup succeeds without requiring access to the gateway admin socket.

---

### T-AZC-0105  Repeated starts continue to perform device login until provisioning exists

**Validates:** AZC-0204

**Procedure:**
1. Start the Azure companion bootstrap flow successfully with a mounted state volume.
2. Start it again with the same mounted state volume.
3. Assert: the second startup re-enters the bootstrap login path.
4. Assert: a new Azure device-code login attempt is invoked.

---

### T-AZC-0106  Azure login failure aborts bootstrap

**Validates:** AZC-0201

**Procedure:**
1. Create an empty auth state volume.
2. Invoke the Azure companion bootstrap path with the Rust device-flow token polling endpoint stubbed to fail.
3. Assert: bootstrap exits with a non-zero status.
4. Assert: the long-running Azure companion process is not started.
5. Assert: bootstrap does not report success.

