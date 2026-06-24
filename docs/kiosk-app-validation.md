<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Kiosk Dashboard App Validation Specification

> **Document status:** Draft  
> **Scope:** Test plan for the Sonde Android kiosk dashboard app.  
> **Audience:** Implementers writing kiosk-app tests and manual validation
> procedures.  
> **Related:** [kiosk-app-requirements.md](kiosk-app-requirements.md),
> [kiosk-app-design.md](kiosk-app-design.md),
> [web-ui-validation.md](web-ui-validation.md)

---

## 1  Overview

This document defines test cases that validate the kiosk app against
`kiosk-app-requirements.md`. The kiosk app is additive to the SPA and Azure
bootstrap flows, so these tests focus on kiosk-specific behavior rather than
re-testing every existing SPA dashboard case.

Where possible, validation should reuse:

1. shared dashboard-runtime unit tests also exercised by the SPA, and
2. mocked backend/auth/cache tests for kiosk-specific setup and data flows.

Manual Android validation remains necessary for gestures, secure storage, and
full device UX.

---

## 2  Traceability matrix

| Requirement | Covered by |
|---|---|
| KA-0100 | T-KA-100 |
| KA-0101 | T-KA-101, T-KA-300 |
| KA-0102 | T-KA-102 |
| KA-0103 | T-KA-103 |
| KA-0200 | T-KA-200 |
| KA-0201 | T-KA-201 |
| KA-0202 | T-KA-202, T-KA-207 |
| KA-0203 | T-KA-203 |
| KA-0204 | T-KA-204 |
| KA-0205 | T-KA-205 |
| KA-0206 | T-KA-206 |
| KA-0207 | T-KA-208 |
| KA-0208 | T-KA-209 |
| KA-0209 | T-KA-210 |
| KA-0300 | T-KA-300 |
| KA-0301 | T-KA-301 |
| KA-0302 | T-KA-302 |
| KA-0303 | T-KA-303 |
| KA-0400 | T-KA-400 |
| KA-0401 | T-KA-401 |
| KA-0402 | T-KA-402 |
| KA-0403 | T-KA-403 |
| KA-0404 | T-KA-404 |
| KA-0405 | T-KA-405 |
| KA-0406 | T-KA-406 |
| KA-0407 | T-KA-407 |

---

## 3  Platform and architecture tests

### T-KA-100  Android kiosk build succeeds

**Validates:** KA-0100

**Procedure:**
1. Build the kiosk app for Android.
2. Assert: the build completes successfully.
3. Assert: the resulting app launches on an Android device.

---

### T-KA-101  Shared dashboard runtime matches SPA semantics

**Validates:** KA-0101

**Procedure:**
1. Select a representative imported environment JSON with multiple dashboards,
   charts, variables, and metrics.
2. Render it through the shared dashboard runtime in SPA test coverage.
3. Render the same input through the kiosk app's dashboard runtime path.
4. Assert: normalized dashboard structure, chart membership, and metric
   evaluation results match.

---

### T-KA-102  Kiosk changes are additive to existing components

**Validates:** KA-0102

**Procedure:**
1. Inspect the changed specifications.
2. Assert: no existing SPA, pairing-tool, Azure-companion, or Azure-provisioning
   requirement was removed.
3. Assert: supporting changes in other component specs are additive only.

---

### T-KA-103  Lock Task Mode remains optional future support

**Validates:** KA-0103

**Procedure:**
1. Inspect the kiosk requirements and design documents.
2. Assert: Android Lock Task Mode is documented as optional future support.
3. Assert: no current kiosk requirement depends on the device being fully managed.

---

## 4  Setup and identity tests

### T-KA-200  SPA-exported environment JSON imports unchanged

**Validates:** KA-0200

**Procedure:**
1. Export an environment JSON from the SPA.
2. Import the same JSON into the kiosk app.
3. Assert: the import succeeds without schema translation or manual editing.
4. Assert: an SPA export that includes the optional `sensorData` object is still
   accepted by the kiosk app.
5. Assert: unknown additive JSON properties are ignored rather than causing
   import failure.
6. Import malformed JSON and JSON missing required fields.
7. Assert: the kiosk app reports actionable validation errors.

---

### T-KA-201  Re-import replaces the single active environment

**Validates:** KA-0201

**Procedure:**
1. Import environment A.
2. Import environment B.
3. Assert: environment B becomes the only active environment.
4. Reset the app.
5. Assert: no active environment remains.

---

### T-KA-202  Setup flow follows the required state machine

**Validates:** KA-0202

**Procedure:**
1. Start from reset state.
2. Import a valid environment JSON.
3. Complete user sign-in.
4. Complete kiosk certificate provisioning.
5. Assert: the app clears the local operator sign-in session state after
   provisioning and does not continue dashboard reads on the operator session.
6. Assert: the app signs in as the application.
7. Assert: dashboards are shown only after application sign-in succeeds.

---

### T-KA-203  Kiosk setup attaches a per-install certificate to the shared app

**Validates:** KA-0203

**Procedure:**
1. Configure setup against a known shared Entra app.
2. Run setup on kiosk installation A.
3. Run setup on kiosk installation B.
4. Assert: both installations attach distinct certificate credentials to the
   same shared Entra app.
5. Assert: neither installation creates a new app registration.

---

### T-KA-204  Secure credential survives restart and is cleared on reset

**Validates:** KA-0204

**Procedure:**
1. Complete kiosk setup.
2. Restart the app.
3. Assert: the app reaches application sign-in without prompting for user sign-in.
4. Reset the app.
5. Assert: secure credential state is removed and setup must start over.

---

### T-KA-205  Reset reports remote certificate cleanup outcome

**Validates:** KA-0205

**Procedure:**
1. Complete kiosk setup and confirm a remote kiosk certificate exists.
2. Trigger reset with remote cleanup succeeding.
3. Assert: local state is cleared and the UI confirms remote removal.
4. Repeat with remote cleanup forced to fail.
5. Assert: local state is still cleared and the UI reports that remote cleanup
   may require manual follow-up.

---

### T-KA-206  Certificate renewal rotates credentials before expiry

**Validates:** KA-0206

**Procedure:**
1. Complete kiosk setup with a certificate that is near expiry in the test
   fixture.
2. Trigger the renewal path.
3. Assert: the app detects impending expiry and attempts to attach a replacement
   certificate.
4. Assert: successful renewal updates the local credential state used for
   unattended app sign-in.
5. Force renewal failure.
6. Assert: the app surfaces an actionable warning or error rather than silently
   continuing.

---

### T-KA-207  Restart bypasses user sign-in and uses application sign-in

**Validates:** KA-0202

**Procedure:**
1. Complete setup successfully.
2. Fully terminate and relaunch the app.
3. Assert: the app does not prompt for interactive user sign-in.
4. Assert: the app signs in as the application and reaches dashboard mode.

---

### T-KA-208  Certificate-management permission failures are actionable

**Validates:** KA-0207

**Procedure:**
1. Simulate a signed-in user who lacks permission to modify credentials on the
   shared Entra app.
2. Attempt initial certificate attachment.
3. Assert: setup fails with an actionable permission error.
4. Repeat for renewal and reset cleanup flows.
5. Assert: each flow reports an actionable permission error rather than a silent
   or generic failure.

---

### T-KA-209  Certificate identity metadata supports unambiguous cleanup

**Validates:** KA-0208

**Procedure:**
1. Complete kiosk setup.
2. Inspect locally persisted non-secret certificate metadata.
3. Assert: at least one stable remote-correlation identifier is stored for the
   active kiosk certificate.
4. Trigger reset with remote cleanup failure.
5. Assert: the app retains or reports enough non-secret identifying metadata for
   manual cleanup follow-up without exposing the private key.

---

### T-KA-210  Device-code setup uses Sonde-managed login metadata

**Validates:** KA-0209

**Procedure:**
1. Import an environment JSON containing additive kiosk setup-login metadata such
   as `kioskSetupClientId` and `loginEndpoint`.
2. Start the operator sign-in flow.
3. Assert: the device-code request uses the imported/publicly provisioned setup
   client rather than the shared certificate-authenticated runtime app client ID.
4. Assert: the setup flow targets the imported authority host for the tenant/cloud.
5. Repeat with the setup-login metadata omitted.
6. Assert: the kiosk app reports an actionable configuration error rather than
   silently falling back to an unrelated third-party public client.

---

## 5  Dashboard UX tests

### T-KA-300  Only imported dashboards appear as full-screen navigation pages

**Validates:** KA-0101, KA-0300

**Procedure:**
1. Import an environment JSON containing multiple dashboards.
2. Open dashboard mode.
3. Assert: each imported dashboard appears as a full-screen navigation page.
4. Assert: the UI does not expose `Dashboard`, `Desired State`, `Programs`, or
   `Sensor Data` tabs.
5. Assert: the kiosk UI does not dedicate persistent chart area to a tab strip.

---

### T-KA-301  Dashboard configuration is read-only

**Validates:** KA-0301

**Procedure:**
1. Open a populated dashboard in the kiosk app.
2. Inspect the UI for dashboard, variable, chart, and metric actions.
3. Assert: no add/edit/delete/reorder controls are present.
4. Assert: rendering the dashboard does not mutate the imported configuration.

---

### T-KA-302  Imported time ranges are rendered but not editable

**Validates:** KA-0302

**Procedure:**
1. Import dashboards with known fixed time ranges.
2. Open each dashboard.
3. Assert: chart data is fetched and rendered using the imported time range.
4. Assert: the kiosk UI does not expose time-range editing controls.

---

### T-KA-303  Horizontal swipe switches dashboards

**Validates:** KA-0303

**Procedure:**
1. Import an environment with at least three dashboards.
2. Open the first dashboard page.
3. Swipe left.
4. Assert: the second dashboard page becomes active.
5. Swipe right.
6. Assert: the first dashboard page becomes active again.

---

## 6  Data, caching, and refresh tests

### T-KA-400  Dashboard reads use application authentication

**Validates:** KA-0400

**Procedure:**
1. Complete setup and sign the user out.
2. Trigger dashboard data reads.
3. Assert: reads continue succeeding without a user session.
4. Force application sign-in failure.
5. Assert: the app surfaces an authentication/configuration error instead of
   presenting stale state as live data.

---

### T-KA-401  Telemetry cache persists across restart

**Validates:** KA-0401

**Procedure:**
1. Open a dashboard and allow telemetry to populate.
2. Terminate and relaunch the app.
3. Assert: the dashboard can render from persisted cache before the next live
   refresh completes.
4. Reset the app.
5. Assert: the persisted telemetry cache is removed.

---

### T-KA-402  Background refresh updates dashboards without blocking display

**Validates:** KA-0402

**Procedure:**
1. Open a dashboard with cached data.
2. Trigger or wait for background refresh.
3. Assert: the dashboard remains visible while refresh is in progress.
4. Assert: refreshed data is incorporated after the fetch completes.
5. Assert: the refresh uses the dashboard's imported fixed time range rather
   than an operator-edited or ad hoc range.
6. Assert: the refresh path runs on an implementation-defined cadence without
   requiring user interaction.

---

### T-KA-403  Pull-to-refresh triggers immediate active-dashboard refresh

**Validates:** KA-0403

**Procedure:**
1. Open a dashboard.
2. Perform the intentional downward swipe gesture.
3. Assert: an immediate refresh starts for the active dashboard scope.
4. Assert: the UI indicates refresh progress.
5. Assert: the refresh uses the dashboard's imported fixed time range.

---

### T-KA-404  Shared cache accelerates dashboard switching

**Validates:** KA-0404

**Procedure:**
1. Import an environment with dashboards that share telemetry sources.
2. Open dashboard A and allow its data to populate the local cache.
3. Switch to dashboard B.
4. Assert: shared telemetry is reused from cache when valid local coverage exists.
5. Assert: switching does not require dropping the entire environment cache.

---

### T-KA-405  Offline mode renders cached dashboards with stale-data indication

**Validates:** KA-0405

**Procedure:**
1. Populate the kiosk cache for a dashboard.
2. Simulate network or Azure read failure.
3. Open or refresh the dashboard.
4. Assert: the dashboard continues rendering from cached data.
5. Assert: the UI indicates cached/offline status rather than implying a fresh
   live refresh succeeded.
6. Restart the app while still offline.
7. Assert: the dashboard still renders from cached data after restart.
8. Repeat with no cache present.
9. Assert: the app shows an actionable offline or connectivity error.

---

### T-KA-406  Guarded operator controls stay hidden during normal kiosk use

**Validates:** KA-0406

**Procedure:**
1. Open dashboard mode in steady-state kiosk presentation.
2. Assert: no persistent settings toolbar or administrator menu consumes chart area.
3. Perform ordinary swipe and pull-to-refresh gestures.
4. Assert: operator controls do not appear accidentally.
5. Perform the guarded operator gesture or equivalent deliberate action.
6. Assert: reset and re-import actions become available.

---

### T-KA-407  Telemetry cache eviction preserves recent dashboard usefulness

**Validates:** KA-0407

**Procedure:**
1. Configure the kiosk cache near its retention or size limit.
2. Insert additional telemetry until eviction is required.
3. Assert: cache growth remains bounded.
4. Assert: older or less recently used historical telemetry is evicted before
   recently used dashboard data.
5. Restart the app.
6. Assert: enough recent data remains to support warm startup for recently used
   dashboards.

---

## 7  Revision history

| Date | Author | Description |
|------|--------|-------------|
| 2026-06-23 | maintain skill | Updated setup-flow validation to check local operator-session teardown after provisioning instead of an explicit kiosk-owned user sign-out step. |
| 2026-06-19 | evolve skill | Added optional kiosk validation coverage for offline cached presentation, guarded operator controls, and bounded cache eviction. |
| 2026-06-19 | evolve skill | Added initial kiosk dashboard app validation coverage for setup flow, shared-app certificate attachment, dashboard-only UI, fixed imported layouts, persistent telemetry cache, background refresh, pull-to-refresh, and swipe navigation. |
