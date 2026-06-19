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
│ - full-screen dashboard pager                               │
│ - swipe / pull-to-refresh gesture handling                  │
│ - cached/live status surfaces                               │
├─────────────────────────────────────────────────────────────┤
│ Shared dashboard runtime                                    │
│ - environment import validation                             │
│ - dashboard normalization                                   │
│ - metric evaluation                                         │
│ - chart rendering                                           │
│ - dashboard page/view composition                           │
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

The shared module owns:

- imported environment/dashboard normalization,
- full-screen dashboard page composition for imported dashboards,
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
  -> UserSignIn
  -> GenerateKioskCertificate
  -> AttachCertificateToSharedApp
  -> UserSignOut
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

Azure bootstrap remains the owner of the shared Entra app registration and
service principal. The kiosk app does **not** create a new app registration.

Each kiosk installation instead:

1. generates a local certificate/private-key pair,
2. attaches the certificate credential to the shared Entra app, and
3. stores only its own private key and related local metadata.

This supports multiple kiosks using one shared Entra app with distinct per-kiosk
certificate credentials.

### 3.3  Credential material

The kiosk backend stores:

- imported environment metadata,
- a local kiosk credential identifier sufficient to target later cleanup,
- the generated private key in secure storage, and
- any non-secret state needed to re-establish application sign-in on restart.

The backend must never depend on browser localStorage for secret material.

### 3.4  Reset cleanup

Reset performs two cleanup phases:

1. **Local cleanup** — always clear secure credential state, local environment
   state, and telemetry cache.
2. **Remote cleanup** — attempt to remove the kiosk certificate credential from
   the shared Entra app and surface success/failure explicitly.

Remote cleanup failure is visible to the operator; it is not silently ignored.

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
   - full-screen dashboard pages derived from imported dashboards,
   - active dashboard chart view,
   - swipe navigation,
   - pull-to-refresh,
   - lightweight cached/live/refresh status indicator,
   - guarded reset/re-import action.

The kiosk frontend intentionally omits SPA editing surfaces:

- no environment manager,
- no dashboard/variable/chart/metric edit controls,
- no Dashboard / Desired State / Programs / Sensor Data tabs,
- no dashboard time-range editor.

### 4.2  Full-screen dashboard pages

Unlike the SPA, where dashboards live under a single `Dashboards` tab, the
kiosk app promotes each imported dashboard to a full-screen page in a horizontal
pager. Page order is derived entirely from imported dashboard order.

The kiosk does not keep a persistent tab strip on screen. Instead, it may show
a lightweight transient overlay containing:

1. the active dashboard name, and
2. the current dashboard position within the imported sequence.

### 4.3  Fixed time-range rendering

The shared dashboard runtime still interprets the same `timeRange` structure as
the SPA, but the kiosk shell does not expose controls that mutate it. Refresh
operations reuse the imported `timeRange` for the active dashboard.

### 4.4  Gesture handling

The kiosk frontend adds two mobile gestures:

1. **Horizontal swipe** — moves between adjacent dashboard pages.
2. **Intentional pull-to-refresh** — triggers immediate refresh for the active
   dashboard scope.

The gesture layer is orthogonal to the shared dashboard runtime and must not
change imported dashboard semantics.

---

## 5  Telemetry data flow and caching

### 5.1  Read path

For a given dashboard render:

1. load normalized imported dashboard configuration,
2. determine the dashboard's variable bindings and fixed time range,
3. query the persistent telemetry cache for local coverage,
4. render from cache immediately when suitable data exists,
5. issue background Azure reads for uncovered or stale intervals,
6. merge refreshed rows into the persistent cache, and
7. re-render the active dashboard from the merged cache result.

### 5.2  Persistent telemetry cache

The kiosk cache is environment-scoped and survives restart. It is optimized for:

1. warm startup,
2. reuse across multiple dashboards in one environment, and
3. low-latency dashboard switching.

The cache stores Azure-fetched telemetry rows plus enough metadata to reason
about freshness and partial coverage for each environment scope.

### 5.3  Cache-backed startup

On restart, the kiosk app:

1. loads the active environment,
2. re-establishes application sign-in,
3. renders the dashboard from persisted cache if available, and
4. refreshes in the background.

This allows dashboards to appear quickly even when a network refresh is still in
flight.

### 5.4  Refresh triggers

Refresh is triggered by:

1. initial entry into dashboard mode,
2. background refresh cadence,
3. active-dashboard switching when needed, and
4. explicit pull-to-refresh.

The frontend keeps the current dashboard visible while refresh is in progress.

---

## 6  Azure and security integration

### 6.1  Bootstrap dependency

The kiosk app depends on Azure bootstrap having already created the shared Entra
app/service principal and granted the read access required for dashboard data.
The imported environment JSON identifies that shared app via `clientId` and
`tenantId`.

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
| KA-0202, KA-0203, KA-0204, KA-0205 | §§3, 6 |
| KA-0300, KA-0301, KA-0302, KA-0303 | §4 |
| KA-0400, KA-0401, KA-0402, KA-0403, KA-0404 | §5, §6 |

---

## 8  Revision history

| Date | Author | Description |
|------|--------|-------------|
| 2026-06-19 | evolve skill | Added initial kiosk dashboard app design covering shared dashboard-runtime extraction, one-time setup/user sign-in, per-kiosk certificate attachment to the shared Entra app, app-authenticated reads, persistent telemetry caching, and swipe/pull-to-refresh UX. |
