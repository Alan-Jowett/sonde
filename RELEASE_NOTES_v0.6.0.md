# Sonde v0.6.0 Release Notes

**Release date:** 2026-05-16
**Commits since v0.5.0:** 84
**Tag:** [`v0.6.0`](https://github.com/Alan-Jowett/sonde/releases/tag/v0.6.0)

---

## Highlights

- **Sonde Web UI** — a new zero-build vanilla HTML/JS/CSS single-page application for managing nodes, programs, desired state, and sensor data from a browser.
- **BPF decoder programs** — the gateway can now run BPF decoder programs to enrich raw `APP_DATA` before it reaches downstream handlers.
- **Sensor data pipeline** — end-to-end path from node telemetry through decoder enrichment into Azure Table storage and the web UI.
- **Azure infrastructure overhaul** — Service Bus replaced with Storage Queues, Flex Consumption replaced with Classic Consumption, and full Docker Compose deployment support.
- **Windows support for Azure companion** — the companion process can now run as a native Windows service with full bootstrap support.

---

## Features

### Web UI
- Add Sonde Web UI SPA and ProgramIngest Azure Function (#880)
- Deploy SPA and configure Entra app during bootstrap (#882)
- Add Azure Static Web App Bicep resource (#881)
- Populate Node ID from actual nodes on Desired State page (#916)
- Default ABI version to 2 on program ingestion (#917)
- Show Diverged status when unassigned program not yet cleared (#918)
- Remove `If-Match` header from `upsertEntity` to enable insert-or-replace (#919)
- Switch MSAL CDN from alcdn.msauth.net to jsdelivr (#890)
- Implement Sensor Data tab (WEB-0700/0701/0702) (#932)
- Add sensor series display customization (#935)
- Preserve status messages, configure CORS and role in deploy (#897)

### Gateway
- BPF decoder programs for `APP_DATA` enrichment (GW-1900–GW-1906) (#925)
- Decoder storage and enrichment tests (T-1902, T-1903) (#930)
- Remove deprecated `ProgramRoute` table and app-data queue routing (#934)

### BPF Interpreter
- Tag pointers loaded from context via `ContextPointerField` (#929)

### Azure Handler
- Add Azure handler for cloud reconciliation (#840)
- Implement ProgramIngest HTTP trigger endpoint (#898)
- Implement SensorData table storage (AZH-0500, AZH-0501) (#931)
- Refactor to use append-only actual and desired state history (#862)
- Log handler errors to stderr for App Insights visibility (#874)

### Azure Companion
- Add Windows service support (#839)
- Support Windows bootstrap (#851)

### Node Firmware
- Show program filenames in node status (#828)
- Make battery telemetry runtime-only (#832)

### Protocol & BLE
- Implement rebooted pre-provisioning test mode (#829)
- Ingest inline ELF from `DESIRED_STATE` with full Prevail verification (#904)

### Infrastructure & CI
- Add Docker Compose deployment for gateway + Azure stack (#899)
- Add dedicated Azure bootstrap image (#853)
- Add on-demand Azure live CI workflow (#833)
- Add Azure live CI setup guide (#834)
- Deploy Azure handler during bootstrap (#857)
- Use SWA CLI for SPA deployment, add live CI coverage (#885)
- Add App Insights observability + fix queue `messageEncoding` (#872)

---

## Azure Infrastructure Changes

- Replace Azure Service Bus with Storage Queues (#867)
- Replace Flex Consumption plan with Classic Consumption (Y1/Dynamic) (#866)
- Replace ProgramIngest API key auth with Entra ID EasyAuth (#901)
- Assign Storage Table Data Contributor in bootstrap (#900)
- Use Graph API for SPA redirect URI, grant Storage permission (#889)
- Use separate location for Static Web App (#884)

---

## Bug Fixes

- Fix bootstrap output parsing on Windows (#855)
- Fix Azure bootstrap Service Bus host handoff (#856)
- Fix bundled bootstrap Bicep namespace wiring (#854)
- Fix bootstrap JMESPath query returns newline-separated TSV (#864)
- Tolerate `config-zip` health-check failure on Flex Consumption (#865)
- Remove awk and Python from Azure bootstrap (#861, #863)
- Fix reusable container workflow cancellation (#860)
- Install protoc in Azure handler package job (#859)
- Fix Azure handler queue message processing (#873)
- Use axum 0.7+ wildcard route syntax (#871)
- Use string `dataType` for queue trigger (#875)
- Strip double-quoted payloads and deserialize f64 timestamps (#876)
- Deserialize `timestamp_ms` from string, f64, or u64 (#878)
- Build Azure handler in Alpine container for static musl linkage (#868)
- Azure Live CI: add aiohttp, deploy and validate handler function (#869)
- Fix Azure live CI downstream failure injection (#836)
- Fix Azure live CI teardown and key generation (#835)
- Retry `config-zip` on transient failures in Azure Live CI (#914)
- Rewrite jq stub and add az rest handler for bootstrap test (#896)
- Update azure-cli image digest to valid 2.86.0 manifest (#902, #903)
- Install icu for SWA CLI StaticSitesClient (#888)
- Install nodejs24 + nodejs24-npm as matched set (#886)
- Use tdnf instead of apk for azure-cli base image (#883)
- Replace hardcoded `login.microsoftonline.com` with dynamic cloud endpoints (#920, #926)
- Suppress noisy `az ad app permission add` warning (#924)
- Suppress `linuxFxVersion` warning during bootstrap zip deployment (#922)
- Fix warning cleanup in node and pair UI (#841)

---

## Documentation

- Clarify Windows Azure companion bootstrap sequence in deployment SOP (#913)
- BPF decoder programs and sensor data pipeline spec (#923)
- Azure live CI setup guide (#834)

---

## Maintenance

- Maintenance audit: realign drift findings (#848)
- Maintenance audit: 11 findings, 7 corrective patches (#933)
- Bump version strings to v0.6.0 (#826)

---

## Dependency Updates

- Bump `tauri` 2.10.3 → 2.11.0 → 2.11.1 (#845, #892)
- Bump `tauri-build` 2.5.6 → 2.6.0 (#846)
- Bump `tokio` 1.52.1 → 1.52.2 (#844)
- Bump `rpassword` 7.5.1 → 7.5.2 (#847)
- Bump `tonic` (gRPC group) (#891)
- Bump `getrandom` 0.3.4 → 0.4.2 (#893)
- Bump `serial2-tokio` 0.1.23 → 0.1.24 (#894)
- Bump `windows-service` 0.8.0 → 0.8.1 (#895)
- Bump `azure/login` CI action 2.3.0 → 3.0.0 (#842)

---

**Full changelog:** [`v0.5.0...v0.6.0`](https://github.com/Alan-Jowett/sonde/compare/v0.5.0...v0.6.0)
