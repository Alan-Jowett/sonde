<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Kiosk Dashboard App Requirements Specification

> **Document status:** Draft  
> **Scope:** This document covers the Sonde Android kiosk dashboard app only. It
> is additive to the existing Web UI, BLE pairing tool, Azure companion, and
> Azure provisioning specifications. It does not remove or weaken any existing
> component requirements.  
> **Related:** [web-ui-requirements.md](web-ui-requirements.md),
> [web-ui-design.md](web-ui-design.md),
> [azure-provisioning-requirements.md](azure-provisioning-requirements.md),
> [azure-companion-requirements.md](azure-companion-requirements.md),
> [ble-pairing-tool-requirements.md](ble-pairing-tool-requirements.md)

---

## 1  Definitions

| Term | Definition |
|---|---|
| **Kiosk app** | The Android application that displays imported Sonde dashboards in a continuously running, operator-light presentation mode. |
| **Setup flow** | The one-time operator flow: reset, import environment JSON, interactive user sign-in, kiosk certificate provisioning, user sign-out, application sign-in, and dashboard display. |
| **Shared Entra app** | The existing certificate-authenticated Entra application/service principal provisioned by Sonde Azure bootstrap. |
| **Kiosk certificate** | A per-installation certificate credential generated locally by the kiosk app and attached to the shared Entra app for unattended application authentication. |
| **Imported environment JSON** | The exact environment JSON schema already used by the SPA environment import/export flow, including `clientId`, `tenantId`, `storageAccount`, `functionAppName`, and `dashboards`. |
| **Telemetry cache** | Local persisted storage of Azure-fetched dashboard telemetry retained across restarts to improve startup and dashboard-switch latency. |

---

## 2  Requirement format

Each requirement uses the following fields:

- **ID** — Unique identifier (`KA-XXXX`).
- **Title** — Short name.
- **Description** — What the kiosk app must do.
- **Acceptance criteria** — Observable, testable conditions that confirm the requirement is met.
- **Priority** — MoSCoW: **Must**, **Should**, **May**.
- **Source** — User request or upstream specification that motivates the requirement.

---

## 3  Platform and architecture

### KA-0100  Android kiosk deployment

**Priority:** Must  
**Source:** USER-REQUEST: "I want a Android app"

**Description:**  
The initial kiosk release MUST run on Android physical devices and MUST be
implemented as an additive app surface that does not remove or degrade existing
SPA, pairing-tool, or Azure-companion functionality.

**Acceptance criteria:**

1. The kiosk app builds and runs on Android.
2. The kiosk implementation is additive to existing repository components.
3. Existing Web UI functionality remains specified and available unchanged.

---

### KA-0101  Maximum dashboard-runtime reuse

**Priority:** Must  
**Source:** USER-REQUEST: "It should share as much of the code as possible with the existing SPA"

**Description:**  
The kiosk app MUST maximize reuse of the existing SPA dashboard import,
normalization, evaluation, and rendering logic so that the kiosk and SPA
interpret the same dashboard JSON consistently.

**Acceptance criteria:**

1. The kiosk app uses the same imported dashboard schema as the SPA.
2. Dashboard rendering semantics match the SPA for the same environment JSON.
3. Kiosk-specific logic is limited to setup, credentialing, caching, and
   navigation behavior rather than duplicating the dashboard engine.

---

### KA-0102  Additive-only scope

**Priority:** Must  
**Source:** USER-REQUEST: "This is entirely additive, no functional requirements from other components are being removed."

**Description:**  
The kiosk app specification MUST add new behavior without retiring or weakening
existing requirements for the SPA, BLE pairing tool, Azure companion, Azure
provisioning flow, gateway, node, or modem.

**Acceptance criteria:**

1. No existing requirement is deleted or redefined to narrow current component behavior.
2. Any supporting changes to other components are expressed as additive requirements.
3. The kiosk app's dashboard-only behavior is scoped to the kiosk app and does
   not constrain the SPA navigation model.

---

## 4  Environment onboarding and identity lifecycle

### KA-0200  Exact SPA environment import compatibility

**Priority:** Must  
**Source:** USER selection: "Use the exact existing SPA environment JSON format"

**Description:**  
The kiosk app MUST import the exact environment JSON schema already accepted by
the SPA environment import flow. It MUST NOT require a kiosk-specific JSON
variant.

**Acceptance criteria:**

1. The kiosk import accepts the same `version`, `name`, `clientId`, `tenantId`,
   `storageAccount`, `functionAppName`, and `dashboards` fields used by the SPA.
2. An environment JSON exported from the SPA can be imported into the kiosk app
   without manual editing.
3. The kiosk import rejects malformed or incomplete environment JSON with an
   actionable error.

---

### KA-0201  Single active environment

**Priority:** Must  
**Source:** USER selection: "Exactly one active environment at a time"

**Description:**  
The kiosk app MUST manage exactly one active environment at a time.

**Acceptance criteria:**

1. The app stores exactly one active imported environment for display.
2. Re-importing environment JSON replaces the active environment rather than
   creating a switchable environment list.
3. Reset clears the active environment.

---

### KA-0202  Kiosk setup state machine

**Priority:** Must  
**Source:** USER input: "The flow should be: Reset -> Import JSON -> sign in using user creds -> provision cert -> signout user -> signin app -> show graphs. On restart, signin app -> show graphs."

**Description:**  
The kiosk app MUST implement the setup and restart flow exactly as a staged
state machine.

**Acceptance criteria:**

1. Reset returns the app to an unconfigured state with no active environment,
   no local credential, and no cached telemetry.
2. After reset, the operator imports an environment JSON before Azure sign-in.
3. Setup then prompts for interactive user sign-in.
4. After user sign-in succeeds, the app provisions a kiosk certificate and
   attaches it to the shared Entra app.
5. The app signs the user out after certificate provisioning succeeds.
6. The app then signs in as the shared Entra app using the kiosk certificate.
7. The dashboard view is entered only after application sign-in succeeds.
8. On later restarts, the app signs in as the application and proceeds directly
   to dashboard display without prompting for user sign-in.

---

### KA-0203  Shared Entra app certificate attachment

**Priority:** Must  
**Source:** USER input: "create the app registration in the existing sonde-azure-companion bootstrap path and only do the cert assignment during the kiosk app setup"

**Description:**  
The kiosk app MUST reuse the existing bootstrap-provisioned shared Entra
application/service principal. Each kiosk installation MUST generate and attach
its own certificate credential to that shared app rather than creating a new
app registration.

**Acceptance criteria:**

1. The kiosk app does not create a new Entra application registration during setup.
2. Setup generates a certificate credential unique to the kiosk installation.
3. Setup attaches the generated certificate to the shared Entra app identified
   by the imported environment.
4. Multiple kiosk installations can coexist by using distinct certificate
   credentials on the same shared Entra app.

---

### KA-0204  Secure credential storage

**Priority:** Must  
**Source:** USER-REQUEST: "store the credential for the created account securely"

**Description:**  
The kiosk app MUST store the kiosk private key and any related application
credential state in platform-appropriate secure storage rather than browser
storage.

**Acceptance criteria:**

1. The kiosk private key is stored using Android secure storage facilities.
2. The imported environment JSON is not treated as a secret and may be stored separately.
3. Dashboard rendering after restart does not require the operator to re-enter
   credential material when secure storage is intact.
4. Reset clears the locally stored secure credential material.

---

### KA-0205  Reset and remote certificate cleanup

**Priority:** Should  
**Source:** USER selection: "one certificate per kiosk install, and remove it on reset when possible"

**Description:**  
Reset SHOULD remove the kiosk installation's certificate credential from the
shared Entra app when the app can do so safely and explicitly surface when
remote cleanup could not be completed.

**Acceptance criteria:**

1. Reset always clears local secure credential state and telemetry cache.
2. Reset attempts to remove the kiosk certificate from the shared Entra app.
3. If remote certificate removal succeeds, the app confirms cleanup.
4. If remote certificate removal cannot be completed, the app reports that the
   remote credential may require manual cleanup.

---

## 5  Dashboard presentation

### KA-0300  Dashboard-only full-screen navigation

**Priority:** Must  
**Source:** USER-REQUEST: "It shouldn't show the other 'Dashboard', 'Desired State', 'Programs', 'Sensor Data' tabs, just the dashboards, as top level tabs." + follow-up review approving full-screen swipeable pages without a persistent tab strip

**Description:**  
The kiosk app MUST expose only imported dashboards as the kiosk's top-level
navigation destinations, rendered as full-screen swipeable pages rather than a
persistent navigation tab strip.

**Acceptance criteria:**

1. Each imported dashboard appears as a full-screen page in the kiosk UI.
2. The kiosk UI does not expose SPA tabs for `Dashboard`, `Desired State`,
   `Programs`, or `Sensor Data`.
3. When the imported environment contains multiple dashboards, the operator can
   navigate among them without entering an edit mode or relying on a persistent
   tab strip.
4. The kiosk UI may show a lightweight transient dashboard title/position
   indicator, but it does not dedicate persistent chart area to a tab bar.

---

### KA-0301  Static imported dashboard configuration

**Priority:** Must  
**Source:** USER-REQUEST: "It should also not have options for add/editing/removing the variables, addings/editing/removing charts, adding/editing/removing metrics. Layout and charts should be static and derived from the json."

**Description:**  
The kiosk app MUST treat imported dashboard configuration as read-only.

**Acceptance criteria:**

1. The kiosk UI does not expose controls to add, edit, rename, reorder, or
   delete dashboards, variables, charts, or metrics.
2. Dashboard layout, variables, chart membership, metric expressions, and chart
   labels are derived entirely from the imported environment JSON.
3. Rendering a dashboard does not mutate its imported configuration.

---

### KA-0302  Fixed imported time ranges

**Priority:** Must  
**Source:** USER selection: "Fix time ranges to the imported environment JSON"

**Description:**  
The kiosk app MUST honor the imported dashboard time ranges as fixed display
configuration.

**Acceptance criteria:**

1. Each dashboard uses the `timeRange` defined in the imported environment JSON.
2. The kiosk UI does not expose dashboard time-range editing controls.
3. Background or manual refresh reuses the imported dashboard time range rather
   than prompting for a new range.

---

### KA-0303  Swipe-first dashboard navigation

**Priority:** Should  
**Source:** USER input: "optimize for fast graph switching via left/right swipes"

**Description:**  
The kiosk app SHOULD optimize dashboard switching for left/right swipe gestures
on touch devices.

**Acceptance criteria:**

1. A left swipe moves to the next dashboard page when one exists.
2. A right swipe moves to the previous dashboard page when one exists.
3. Gesture-based navigation preserves the active dashboard's rendered state
   until the new dashboard is ready to display.

---

## 6  Data access, caching, and refresh

### KA-0400  Application-authenticated read access

**Priority:** Must  
**Source:** USER-REQUEST: kiosk app should sign in as the app and continually display data

**Description:**  
After setup completes, the kiosk app MUST read dashboard data using
application-authenticated access associated with the shared Entra app and the
kiosk certificate.

**Acceptance criteria:**

1. Normal dashboard reads do not depend on a user login session.
2. Dashboard reads succeed using the stored kiosk certificate and the imported
   environment's tenant/client/storage settings.
3. If application sign-in fails, the dashboard view does not pretend to be live
   and instead surfaces a configuration or authentication error.

---

### KA-0401  Persistent local telemetry cache

**Priority:** Must  
**Source:** USER selection: "persist cached telemetry across restarts"

**Description:**  
The kiosk app MUST persist fetched telemetry locally across app restarts to
reduce startup latency and improve dashboard-switch responsiveness.

**Acceptance criteria:**

1. Telemetry fetched for dashboard rendering is stored in a local cache that
   survives app restart.
2. Restarting the app allows previously viewed dashboards to render from cache
   before a fresh network fetch completes.
3. Reset clears the persisted telemetry cache.

---

### KA-0402  Background refresh

**Priority:** Must  
**Source:** USER input: "data refresh in the background"

**Description:**  
The kiosk app MUST refresh dashboard data in the background while preserving an
interactive dashboard display.

**Acceptance criteria:**

1. The app refreshes data without requiring the operator to re-enter setup or
   manually reload each dashboard.
2. The currently displayed dashboard remains visible while background refresh is
   in progress.
3. Refresh completion updates subsequent renders without requiring app restart.

---

### KA-0403  Intentional downward swipe refresh

**Priority:** Should  
**Source:** USER input: "or on a intentional downward swipe"

**Description:**  
The kiosk app SHOULD provide an intentional downward swipe gesture that triggers
an immediate dashboard refresh.

**Acceptance criteria:**

1. The app distinguishes intentional pull-to-refresh from ordinary vertical scroll behavior.
2. Triggering the gesture starts an immediate refresh of the active dashboard's data scope.
3. The UI indicates when a manual refresh is in progress.

---

### KA-0404  Fast dashboard switching from shared cache

**Priority:** Should  
**Source:** USER input: "optimize for fast graph switching"

**Description:**  
The kiosk app SHOULD prefer fast dashboard switching by reusing locally cached
telemetry across dashboards in the same environment whenever their data scopes
overlap.

**Acceptance criteria:**

1. Switching between previously viewed dashboards does not require discarding the
   entire local cache.
2. Shared telemetry needed by multiple dashboards is reused when valid local
   coverage exists.
3. Cache reuse does not change the imported dashboard semantics.

---

## 7  Revision history

| Date | Author | Description |
|------|--------|-------------|
| 2026-06-19 | evolve skill | Added initial Android kiosk dashboard app requirements for static imported dashboards, one-time user setup, per-kiosk certificate credentials, app-authenticated reads, persistent telemetry cache, swipe navigation, and additive-only scope. |
