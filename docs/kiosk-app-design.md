<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Kiosk Dashboard App Design Specification

> **Document status:** Draft  
> **Scope:** Architecture and internal design for the Sonde Android kiosk
> dashboard app. This design is additive to the existing Web UI, Tauri pairing
> app, Azure companion, and Azure provisioning designs.  
> **Audience:** Implementers building the kiosk app and any supporting additive
> Azure or shared-frontend changes.  
> **Related:** [kiosk-app-requirements.md](kiosk-app-requirements.md),
> [web-ui-design.md](web-ui-design.md),
> [azure-provisioning-design.md](azure-provisioning-design.md),
> [azure-companion-design.md](azure-companion-design.md)

---

## 1  Overview

The kiosk dashboard app is an Android-first Tauri application that renders the
same imported dashboard definitions already supported by the SPA, but in a
locked-down, continuously running presentation mode.

The design has four high-level goals:

1. **Exact environment compatibility** with SPA-exported environment JSON.
2. **Maximum dashboard-runtime reuse** with the existing SPA.
3. **Unattended application-authenticated reads** after one-time operator setup.
4. **Low-latency navigation** through persistent local telemetry caching plus
   swipe-first dashboard presentation.

The kiosk app is additive. It does not replace the SPA or remove pairing flows.

---

## 2  Architecture

### 2.1  Layering

The kiosk app uses a Tauri split similar to the existing pairing tool:

```text
┌─────────────────────────────────────────────────────────────┐
│ Frontend shell (HTML/CSS/JS in Tauri WebView)              │
│ - setup wizard                                              │
│ - full-screen chart pager                                   │
│ - swipe / pull-to-refresh gesture handling                  │
│ - transient cached/live status overlays                     │
├─────────────────────────────────────────────────────────────┤
│ Shared dashboard runtime                                    │
│ - environment import validation                             │
│ - dashboard normalization                                   │
│ - metric evaluation                                         │
│ - chart rendering                                           │
│ - chart/page composition primitives                         │
├─────────────────────────────────────────────────────────────┤
│ Kiosk app backend (Rust/Tauri commands)                     │
│ - secure storage                                             │
│ - certificate generation                                     │
│ - Entra user sign-in + shared-app certificate attachment     │
│ - application-auth token acquisition                         │
│ - Azure telemetry fetch                                      │
│ - persisted telemetry cache                                  │
├─────────────────────────────────────────────────────────────┤
│ External dependencies                                        │
│ - bootstrap-provisioned shared Entra app/service principal   │
│ - Azure Table data plane                                     │
│ - Android secure storage                                     │
└─────────────────────────────────────────────────────────────┘
```

### 2.2  Shared dashboard runtime extraction

The SPA currently holds dashboard import, normalization, evaluation, and
rendering logic in `deploy/web-ui/app.js`. The kiosk app design requires this
dashboard runtime to be factored into a shared frontend module consumed by:

1. the SPA, and
2. the kiosk frontend shell.

Because the current Web UI is a zero-build vanilla HTML/JS/CSS SPA, this
extraction must remain compatible with that deployment model. The shared module
therefore uses directly served frontend assets and must not require introducing
a mandatory bundler or build pipeline for the existing SPA.

The shared module owns:

- imported environment/dashboard normalization,
- shared chart/page composition primitives for imported dashboards,
- metric-expression evaluation,
- chart rendering and refresh coordination, and
- any dashboard-level empty/error states that remain valid in read-only mode.

The kiosk shell adds only kiosk-specific concerns:

- setup flow,
- secure identity/bootstrap orchestration,
- cache freshness display,
- swipe navigation, and
- pull-to-refresh behavior.

This preserves dashboard parity while avoiding a second implementation of the
dashboard engine.

---

## 3  Setup and identity lifecycle

### 3.1  Setup state machine

The kiosk setup flow is:

```text
Reset
  -> ImportEnvironment
  -> UserDeviceCodeSignIn
  -> GenerateKioskCertificate
  -> AttachCertificateToSharedApp
  -> ClearLocalOperatorSession
  -> ApplicationSignIn
  -> WarmCacheAndShowDashboards
```

Restart flow:

```text
LoadEnvironmentAndCredential
  -> ApplicationSignIn
  -> RenderFromCache
  -> BackgroundRefresh
```

### 3.2  Shared Entra app model

Azure bootstrap remains the owner of:

1. the shared Entra app registration/service principal used for unattended
   certificate-authenticated dashboard reads, and
2. a separate public-client Entra app used only for operator device-code sign-in
   during setup, renewal, and reset cleanup.

The kiosk app does **not** create a new app registration during setup.

Each kiosk installation instead:

1. generates a local certificate/private-key pair,
2. attaches the certificate credential to the shared Entra app, and
3. stores only its own private key and related local metadata.

This supports multiple kiosks using one shared Entra app with distinct per-kiosk
certificate credentials.

The setup and renewal flows assume the signed-in operator has enough permission
to add or replace credentials on that shared Entra app. When that permission is
absent, the kiosk app fails explicitly with an actionable operator-facing error.

Because operator setup uses device-code sign-in, the kiosk app is not the owner
of a browser-authenticated user session in the same way as an embedded web app.
After provisioning succeeds, the kiosk therefore clears its local operator
session state and proceeds to unattended application sign-in rather than
requiring a stronger kiosk-owned logout primitive.

### 3.3  Credential material

The kiosk backend stores:

- imported environment metadata,
- non-secret setup-login metadata such as the device-code public-client ID and
  authority host,
- non-secret remote-correlation metadata for the kiosk certificate credential
  (for example thumbprint, key ID, or credential object ID),
- a local kiosk credential identifier sufficient to target later cleanup,
- the generated private key in secure storage, and
- any non-secret state needed to re-establish application sign-in on restart.

The backend must never depend on browser localStorage for secret material.

### 3.4  Certificate renewal

The kiosk backend monitors certificate validity and treats renewal as an
extension of the existing setup contract:

1. detect that the current kiosk certificate is approaching expiry,
2. sign in or re-use sufficient operator authority for credential management,
3. generate and attach a replacement certificate,
4. update local secure credential state and remote-correlation metadata, and
5. retire the prior kiosk certificate when policy and permissions allow.

If renewal cannot complete because of permission or policy constraints, the app
surfaces an actionable warning or error rather than silently drifting into
credential expiry.

### 3.5  Reset cleanup

Reset performs two cleanup phases:

1. **Local cleanup** — always clear secure credential state, local environment
   state, and telemetry cache.
2. **Remote cleanup** — attempt to remove the kiosk certificate credential from
   the shared Entra app and surface success/failure explicitly.

Remote cleanup failure is visible to the operator; it is not silently ignored.
Non-secret remote-correlation metadata is retained long enough to record and
report that cleanup outcome, even if private-key material has already been
destroyed.

---

## 4  Frontend UX model

### 4.1  Screens

The kiosk frontend has two major modes:

1. **Setup mode**
   - reset action,
   - environment import action,
   - user sign-in prompt,
   - provisioning progress,
   - application sign-in progress.
2. **Dashboard mode**
   - full-screen chart pages derived from imported dashboards,
   - active chart view,
   - swipe navigation across the derived chart sequence,
   - pull-to-refresh,
   - transient overlays for chart identity or refresh state,
   - guarded reset/re-import action.

The kiosk frontend intentionally omits SPA editing surfaces:

- no environment manager,
- no dashboard/variable/chart/metric edit controls,
- no Dashboard / Desired State / Programs / Sensor Data tabs,
- no dashboard time-range editor.

### 4.2  Full-screen chart pages

Unlike the SPA, where dashboards live under a single `Dashboards` tab, the
kiosk app promotes imported chart content to full-screen chart pages in a
horizontal pager. Page order is derived from imported dashboard order and chart
order within each dashboard.

The kiosk does not keep persistent chart-area chrome on screen. In steady-state
presentation, the active chart fills the available viewport without a
persistent product header, status row, variables pane, chart-details pane, or
tab strip.

The one deliberate exception is a lightweight persistent title overlay that
shows the active dashboard name. Optional transient overlays may still be used
for chart identity, chart position, or refresh status without becoming the
primary title surface.

### 4.3  Fixed time-range rendering

The shared dashboard runtime still interprets the same `timeRange` structure as
the SPA, but the kiosk shell does not expose controls that mutate it. Refresh
operations preserve imported dashboard time ranges; when a shared environment
refresh serves multiple dashboards, the fetch horizon is derived from the
largest imported dashboard time range and each active chart still renders only
its source dashboard's imported window.

### 4.4  Gesture handling

The kiosk frontend adds two mobile gestures:

1. **Horizontal swipe** — moves between adjacent chart pages.
2. **Intentional pull-to-refresh** — triggers an immediate environment refresh
   that repopulates the shared telemetry cache used by the active chart page.

The gesture layer is orthogonal to the shared dashboard runtime and must not
change imported dashboard semantics.

### 4.5  Orientation adaptation

The kiosk frontend treats both portrait and landscape as first-class
presentations. Rotation does not change what dashboard or chart is active; it
only changes how the active chart occupies the viewport.

The chart-first shell therefore:

1. reflows the active chart to consume the available viewport in either
   orientation,
2. avoids introducing persistent non-chart chrome as a fallback for portrait
   layouts, and
3. updates the active page in place when device orientation changes rather than
   restarting the view flow.

### 4.6  Guarded operator controls

The kiosk frontend keeps operator controls off the steady-state chart
surface. Reset and re-import remain available through a guarded entrypoint such
as a hidden gesture, long-press corner affordance, or similarly deliberate
interaction.

The guarded operator entrypoint must:

1. avoid consuming persistent chart area,
2. resist accidental activation during ordinary dashboard swiping or
   pull-to-refresh, and
3. lead to the minimal operator actions needed for reset, re-import, and setup
   recovery.

### 4.7  Optional future Lock Task support

The initial kiosk design does not require Android Lock Task Mode or full
managed-device deployment. However, the frontend and backend split should avoid
unnecessary assumptions that would block a future managed-device deployment mode.

---

## 5  Telemetry data flow and caching

### 5.1  Read path

For a given active chart render:

1. load normalized imported dashboard configuration,
2. identify the active chart plus its source dashboard's variable bindings and
   fixed time range,
3. query the persistent environment-scoped telemetry cache for local coverage,
4. render from cache immediately when suitable data exists,
5. issue shared Azure reads for uncovered or stale environment telemetry
   intervals,
6. merge refreshed rows into the persistent cache, and
7. re-render the active chart page from the merged cache result.

### 5.2  Persistent telemetry cache

The kiosk cache is environment-scoped and survives restart. It is optimized for:

1. warm startup,
2. reuse across multiple dashboards and chart pages in one environment, and
3. low-latency chart switching.

The cache stores Azure-fetched telemetry rows in one logical sensor-data table
per environment, keyed so multiple dashboards can read the same source rows
without per-dashboard duplication. It also stores enough metadata to reason
about freshness, incremental append boundaries, and partial coverage for each
environment scope.

### 5.3  Cache-backed startup

On restart, the kiosk app:

1. loads the active environment,
2. re-establishes application sign-in,
3. renders the active chart page from persisted cache if available, and
4. refreshes in the background.

This allows the kiosk's primary chart presentation to appear quickly even when a
network refresh is still in flight.

### 5.4  Refresh triggers

Refresh is triggered by:

1. initial entry into dashboard mode,
2. background refresh cadence,
3. active-chart switching when needed, and
4. explicit pull-to-refresh.

Each trigger feeds the same environment-scoped refresh planner. That planner:

1. computes the union of telemetry sources referenced by imported dashboards,
2. uses the largest imported dashboard time range as the cold-start fetch
   horizon,
3. reuses the persisted environment cache for render-time filtering, and
4. after a successful fetch, requests only telemetry newer than the last
   successful environment refresh.

The frontend keeps the current chart visible while refresh is in progress.
Successful background refreshes do not replace the steady-state title or status
surface with transient progress or success chrome; failure states may still
surface cached/offline or actionable error messaging.

### 5.5  Offline presentation

If a refresh cannot complete because the device is offline or Azure reads are
temporarily unavailable, the kiosk remains in dashboard mode when usable cached
data exists.

In that case the frontend:

1. continues rendering the most recent cached chart state,
2. marks the display through a transient overlay or explicit state rather than
   silently implying fresh data,
   and
3. continues retrying through the normal background or manual refresh paths.

If no usable cache exists, the kiosk shows an actionable connectivity or
authentication error or explicit no-data state rather than an empty
success-shaped chart shell.

After app restart under offline conditions, the same cached-first behavior
applies before any successful live refresh is available.

### 5.6  Cache bounds and eviction

The persistent telemetry cache is explicitly bounded. The implementation may use
an LRU-like policy, time-window trimming, or a hybrid policy, but it must
preserve the kiosk goals of:

1. warm startup after restart,
2. fast switching among recently viewed chart pages, and
3. bounded local storage growth during long-lived kiosk operation.

Eviction therefore targets older or less recently used historical telemetry
before more recent data that is likely to support the active chart set.

### 5.7  Background refresh cadence

The background refresh scheduler is fixed at 900 seconds and is not
operator-configured in the kiosk UI. It must:

1. operate without user interaction,
2. preserve the active chart display while refresh work runs,
3. avoid changing imported dashboard time-range semantics, and
4. use the shared environment refresh planner rather than per-dashboard or
   per-series polling.

---

## 6  Azure and security integration

### 6.1  Bootstrap dependency

The kiosk app depends on Azure bootstrap having already created the shared Entra
app/service principal, the setup public-client app, and the read access required
for dashboard data. The imported environment JSON identifies:

1. the shared runtime app via `clientId` and `tenantId`, and
2. the setup-login path via additive non-secret metadata such as
   `kioskSetupClientId` and `loginEndpoint`.

### 6.2  Application-authenticated telemetry reads

Normal kiosk reads use application authentication backed by the kiosk
certificate. The user login is setup-only and is not reused for normal runtime
telemetry refreshes.

### 6.3  Certificate attachment contract

The setup backend must attach the kiosk certificate credential to the existing
shared Entra app before application sign-in begins. The exact Azure API surface
used to attach the credential is an implementation detail, but the contract is:

1. the target app is the bootstrap-created shared Entra app from the imported environment,
2. the credential attached is unique to the kiosk installation, and
3. enough local metadata is retained to target later remote cleanup.

---

## 7  Traceability summary

| Requirement | Design coverage |
|---|---|
| KA-0100, KA-0102 | §§1, 2 |
| KA-0101 | §2.2 |
| KA-0200, KA-0201 | §§3.1, 4.1 |
| KA-0202, KA-0203, KA-0204, KA-0205, KA-0206, KA-0207, KA-0208, KA-0209 | §§3, 6 |
| KA-0300, KA-0301, KA-0302, KA-0303, KA-0304, KA-0305, KA-0306 | §4 |
| KA-0400, KA-0401, KA-0402, KA-0403, KA-0404, KA-0405, KA-0407, KA-0408 | §5, §6 |
| KA-0406 | §4.6 |
| KA-0103 | §4.7 |

---

## 8  Revision history

| Date | Author | Description |
|------|--------|-------------|
| 2026-06-24 | evolve skill | Revised the kiosk frontend design around full-screen chart pages, deterministic chart sequencing, orientation-aware layout, persistent dashboard-name titling, a 900-second silent-success refresh cadence, an environment-scoped shared telemetry refresh/cache plan, and explicit empty-chart states. |
| 2026-06-23 | maintain skill | Replaced the explicit `UserSignOut` state with local operator-session teardown to match the kiosk device-code architecture. |
| 2026-06-20 | evolve skill | Added setup-login public-client metadata for kiosk device-code sign-in and clarified that bootstrap owns both the shared runtime app and the setup-login app. |
| 2026-06-19 | evolve skill | Clarified kiosk certificate lifecycle, permission-failure reporting, certificate identity persistence, offline restart behavior, implementation-defined refresh cadence, and optional future Lock Task support. |
| 2026-06-19 | evolve skill | Added optional kiosk design coverage for offline cached presentation, guarded operator-only controls, and bounded cache eviction. |
| 2026-06-19 | evolve skill | Added initial kiosk dashboard app design covering shared dashboard-runtime extraction, one-time setup/user sign-in, per-kiosk certificate attachment to the shared Entra app, app-authenticated reads, persistent telemetry caching, and swipe/pull-to-refresh UX. |
