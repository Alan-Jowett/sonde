<!-- SPDX-License-Identifier: MIT -->
<!-- Copyright (c) 2026 sonde contributors -->

# Maintenance Audit — 2026-05-15

## 1. Executive Summary

This audit applies the full D1–D13 drift detection taxonomy across all
Sonde components. It succeeds the 2026-04-08 audit (27 findings, of which
20 have been resolved in the intervening 37-day period).

**Key metrics:**

| Component    | Findings | Critical | High | Medium | Low |
|-------------|----------|----------|------|--------|-----|
| Gateway      | 3        | 0        | 0    | 2      | 1   |
| Modem        | 2        | 0        | 0    | 2      | 0   |
| Azure Handler| 2        | 0        | 2    | 0      | 0   |
| Bundle       | 2        | 0        | 0    | 1      | 1   |
| Hardware     | 1        | 0        | 0    | 1      | 0   |
| Modem (residual) | 1   | 0        | 0    | 1      | 0   |
| **Total**    | **11**   | **0**    | **2**| **7**  | **2**|

**Previous audit residual status:** 20 of 27 findings resolved.  5 findings
remain as accepted/deferred (F-001, F-004, F-010, F-022, F-026).  2 findings
(F-007, F-014) are partially resolved — the modem now has extensive BLE
bridge tests in `bridge.rs`, but the underlying `NoBle` mock limitation
remains for hardware-level BLE event injection.

**Overall assessment:** The codebase is in strong shape. The major
development since April 8 — decoder BPF programs (GW-1900 series), Azure
handler, pre-provisioning test mode reboot, modem display lifecycle — has
been well-implemented with good spec coverage.  The highest-impact finding
is that `AZH-0502` (SensorData query support) is specified but not yet
implemented in the Azure handler.  Gateway decoder tests cover most
acceptance criteria but miss the context ABI and `bpf_trace_printk` logging.
Modem display validation test cases (T-0900 series) exist in the spec but
have no automated test implementation.

---

## 2. Problem Statement

Periodic maintenance audit to detect specification drift across all artifact
layers — requirements, design, validation plans, source code, and test code.
This is Phase 1 of the maintain workflow: systematic drift detection before
human classification.

Focus areas: **full audit** — all components, all drift categories (D1–D13).

---

## 3. Investigation Scope

### Source documents consulted

| Document | Purpose |
|----------|---------|
| `docs/protocol-crate-design.md` | Protocol design |
| `docs/protocol-crate-validation.md` | Protocol test cases |
| `docs/protocol.md` | Wire format reference |
| `docs/gateway-requirements.md` | Gateway REQ-IDs |
| `docs/gateway-design.md` | Gateway design |
| `docs/gateway-validation.md` | Gateway test cases |
| `docs/gateway-api.md` | gRPC API |
| `docs/gateway-companion-api.md` | Connector API |
| `docs/node-requirements.md` | Node REQ-IDs |
| `docs/node-design.md` | Node design |
| `docs/node-validation.md` | Node test cases |
| `docs/modem-requirements.md` | Modem REQ-IDs |
| `docs/modem-design.md` | Modem design |
| `docs/modem-validation.md` | Modem test cases |
| `docs/modem-protocol.md` | Modem protocol spec |
| `docs/ble-pairing-tool-requirements.md` | BLE pairing REQ-IDs |
| `docs/ble-pairing-tool-design.md` | BLE pairing design |
| `docs/ble-pairing-tool-validation.md` | BLE pairing test cases |
| `docs/ble-pairing-protocol.md` | BLE pairing protocol spec |
| `docs/safe-bpf-interpreter.md` | BPF interpreter design |
| `docs/safe-bpf-interpreter-validation.md` | BPF test cases |
| `docs/bpf-environment.md` | BPF environment spec |
| `docs/azure-handler-requirements.md` | Azure handler REQ-IDs |
| `docs/azure-handler-design.md` | Azure handler design |
| `docs/azure-handler-validation.md` | Azure handler test cases |
| `docs/azure-companion-requirements.md` | Azure companion REQ-IDs |
| `docs/azure-companion-design.md` | Azure companion design |
| `docs/azure-companion-validation.md` | Azure companion test cases |
| `docs/bundle-tool-requirements.md` | Bundle tool REQ-IDs |
| `docs/bundle-tool-design.md` | Bundle tool design |
| `docs/bundle-tool-validation.md` | Bundle test cases |
| `docs/bundle-format.md` | Bundle format spec |
| `docs/e2e-validation.md` | E2E test cases |
| `docs/kicad-export-requirements.md` | KiCad export REQ-IDs |
| `docs/hw-requirements.md` | Hardware REQ-IDs |
| `docs/audits/maintenance-audit-2026-04-08.md` | Previous audit baseline |

### Crates examined

| Crate | Source dir | Test locations |
|-------|-----------|----------------|
| `sonde-protocol` | `crates/sonde-protocol/src/` | `crates/sonde-protocol/tests/` |
| `sonde-gateway` | `crates/sonde-gateway/src/` | `crates/sonde-gateway/tests/` |
| `sonde-node` | `crates/sonde-node/src/` | In-source `#[cfg(test)]` |
| `sonde-modem` | `crates/sonde-modem/src/` | `crates/sonde-modem/tests/`, in-source `#[cfg(test)]` |
| `sonde-bpf` | `crates/sonde-bpf/src/` | `crates/sonde-bpf/tests/` |
| `sonde-pair` | `crates/sonde-pair/src/` | In-source `#[cfg(test)]` |
| `sonde-pair-ui` | `crates/sonde-pair-ui/src-tauri/src/` | In-source `#[cfg(test)]` |
| `sonde-admin` | `crates/sonde-admin/src/` | In-source `#[cfg(test)]` |
| `sonde-azure-handler` | `crates/sonde-azure-handler/src/` | In-source `#[cfg(test)]` |
| `sonde-azure-companion` | `crates/sonde-azure-companion/src/` | In-source `#[cfg(test)]` |
| `sonde-bundle` | `crates/sonde-bundle/src/` | In-source `#[cfg(test)]` |
| `sonde-e2e` | `crates/sonde-e2e/tests/` | Integration tests |
| `sonde-kicad` | `crates/sonde-kicad/src/` | In-source `#[cfg(test)]` |

### Method

- 5 parallel exploration agents for spec-vs-code comparison (protocol/BPF,
  gateway, node/modem, pair/admin/azure, bundle/E2E/HW)
- Manual `grep`/`view` verification of all agent findings and all 27
  previous-audit residuals
- Targeted deep-read of new decoder BPF, Azure handler, and modem display
  implementations

### Excluded

- Handler crates (`sonde-tmp102-handler`, `sonde-sht40-handler`): external
  process handlers, no spec artifacts
- Web UI (`sonde-pair-ui` Tauri frontend): TypeScript/Svelte UI layer
  excluded; Rust backend `lib.rs` examined

---

## 4. Findings

### F-001 — AZH-0502 SensorData query support not implemented

- **Severity:** High
- **Category:** `D8_UNIMPLEMENTED_REQUIREMENT`
- **Location:** `docs/azure-handler-requirements.md` AZH-0502 ↔
  `crates/sonde-azure-handler/src/lib.rs`
- **Description:** The spec requires the `SensorData` table to be queryable
  by node ID and time range via Azure Table Storage REST API.  The handler
  crate implements `handle_app_data` (AZH-0500 row storage), but no query
  endpoint or method exists.  `HandlerStore` exposes only
  append/load/retrieve methods for actual/desired state and program images.
- **Evidence:** `grep` for `query`, `SensorData` in the handler source
  returns 0 matches.  AZH-0502 acceptance criteria:  "SPA can query
  SensorData rows for a specific node within a time range."
- **Root Cause:** The SensorData query is consumed by the SPA (web UI)
  directly via Azure Table Storage REST API, not via the handler.  The
  handler only writes rows.  The requirement may be misplaced — the SPA
  queries the table directly using SAS tokens, so the handler need not
  implement a query endpoint.
- **Impact:** If the SPA relies on the handler for queries, this feature
  is missing.  If the SPA queries Table Storage directly (likely), the
  requirement description is misleading but the system works correctly.
- **Confidence:** Medium
- **Remediation:** Clarify AZH-0502 — is this a handler requirement or a
  SPA requirement?  If the SPA queries directly, re-scope AZH-0502 to
  document the SPA's responsibility and confirm the handler's role is
  write-only.

---

### F-002 — Modem display validation tests (T-0900 series) unimplemented

- **Severity:** Medium
- **Category:** `D11_UNIMPLEMENTED_TEST_CASE`
- **Location:** `docs/modem-validation.md` T-0900–T-0907 ↔
  `crates/sonde-modem/tests/device_tests.rs`
- **Description:** The validation plan defines 9 display-related test cases
  (T-0900 through T-0907) covering reliable transfer, rendering,
  non-interference, error handling, idle timeout, and panel wake.  The
  hardware test suite (`device_tests.rs`) has no display tests.  However,
  the bridge unit tests in `bridge.rs` already cover:
  - `display_frame_queued_for_display_driver` (≈ T-0900)
  - `malformed_display_frame_chunk_emits_event_error` (≈ T-0901)
  - `duplicate_display_frame_chunk_resends_progress_ack` (≈ T-0901a)
  - `display_write_failure_emits_event_error` (≈ T-0905)
  The remaining gaps are T-0902/T-0903 (non-interference, hardware-only),
  T-0904 (architectural constraint), T-0906 (idle timeout), and T-0907
  (panel wake), which require timer/clock mocking.
- **Evidence:** `grep display crates/sonde-modem/tests/` → 0 matches.
  Validation plan entries at `modem-validation.md:1183-1300`.
- **Root Cause:** Display tests require hardware (OLED panel) or mock
  display infrastructure not yet built.  The gateway-side display transfer
  tests exist (`admin_display.rs`), but the modem-side reception and
  rendering path is untested.
- **Impact:** Display bugs (rendering errors, idle timeout regression,
  interference with ESP-NOW) would not be caught by automated tests.
- **Confidence:** High
- **Remediation:** Implement mock display infrastructure and add at least
  T-0900 (transfer accepted) and T-0901 (invalid metadata rejected) as
  unit tests.  T-0902/T-0903 (non-interference) may remain hardware-only.

---

### F-003 — GW-1904 AC1 decoder context ABI untested

- **Severity:** Medium
- **Category:** `D12_UNTESTED_ACCEPTANCE_CRITERION`
- **Location:** `docs/gateway-requirements.md` GW-1904 AC-1 ↔
  `crates/sonde-gateway/tests/decoder.rs`
- **Description:** GW-1904 AC-1 specifies the decoder context ABI
  (`decoder_context` with `input_data`/`input_end` pointer fields).  The
  decoder test suite covers `emit_reading` (T-1902), `rodata` (T-1903),
  and `map_update_elem` (T-1904a–e), but no test exercises the context
  pointer bounds (validating that `input_data` and `input_end` bracket
  the APP_DATA payload correctly).
- **Evidence:** `docs/gateway-validation.md` T-1904a–e test list;
  `decoder.rs` test functions.  Context ABI is documented at
  `decoder.rs:317-344` but untested.
- **Root Cause:** Context pointer semantics are implicitly tested by
  decoder programs that access `input_data`, but no test explicitly
  validates the ABI contract (pointer values, bounds).
- **Impact:** Low — context ABI is verified by the Prevail verifier at
  ingestion time.  An explicit test would catch regressions in the
  `decoder_context` layout.
- **Confidence:** Medium
- **Remediation:** Add a T-1904f test that validates `input_data` and
  `input_end` pointer values in the decoder context match the APP_DATA
  payload bounds.

---

### F-004 — T-1304 validates compile-time vars, not binary output

- **Severity:** Medium
- **Category:** `D13_ASSERTION_MISMATCH`
- **Location:** `docs/gateway-validation.md` T-1304 ↔
  `crates/sonde-gateway/tests/build_metadata.rs:13-58`
- **Description:** The validation spec says "Run `sonde-gateway --version`
  and match output pattern."  The test instead inspects compile-time
  environment variables (`CARGO_PKG_VERSION`, `SONDE_GIT_COMMIT`) and
  validates their format.  It never invokes the `sonde-gateway` binary.
- **Evidence:** Test at `build_metadata.rs:13-58` checks `env!()` macros;
  spec at `gateway-validation.md:2709-2719` says "run the binary."
- **Root Cause:** Running the actual binary in a test requires building
  the binary first and is a system-level test rather than a unit test.
  The test validates the underlying data, not the integration with the
  binary's CLI parser.
- **Impact:** Low — a mismatch between the `--version` CLI output and the
  compile-time data would not be caught.
- **Confidence:** High
- **Remediation:** Either update the validation plan to reflect that T-1304
  validates build metadata format (not CLI output), or add an integration
  test that invokes the binary.

---

### F-005 — MODEM_READY timing not validated in test (residual F-010)

- **Severity:** Medium
- **Category:** `D12_UNTESTED_ACCEPTANCE_CRITERION`
- **Location:** `docs/modem-requirements.md` MD-0104 ↔
  `crates/sonde-modem/tests/device_tests.rs:146-169`
- **Description:** MD-0104 requires `MODEM_READY` within 2 seconds.
  T-0101 checks the message content (version, MAC) but does not assert
  the timing deadline.
- **Evidence:** `device_tests.rs:146-169` asserts field values only.
  MD-0104 acceptance criteria: "within 2 seconds of power-on."
- **Root Cause:** Timing assertions on real hardware are fragile; the
  modem boot sequence depends on ESP-IDF initialization time.
- **Impact:** A modem that takes >2 seconds to send `MODEM_READY` would
  not be caught by automated tests.
- **Confidence:** High
- **Remediation:** Add a timing assertion with a generous margin (e.g.,
  3 seconds) or document as a manual test.
- **Residual:** Yes — F-010 from 2026-04-08 audit.

---

### F-006 — Bundle `reply_timeout_ms` negative rejection untestable

- **Severity:** Medium
- **Category:** `D12_UNTESTED_ACCEPTANCE_CRITERION`
- **Location:** `docs/bundle-tool-requirements.md` SB-0202 AC-5 ↔
  `crates/sonde-bundle/src/manifest.rs:84-97`
- **Description:** The spec says `reply_timeout_ms: 0 or negative` must be
  rejected.  The field is `Option<u32>`, so negative values cannot be
  represented.  Validation only covers `0`.
- **Evidence:** `manifest.rs` type definition uses `u32`; validation at
  `validate.rs:309-315` checks `timeout == 0`.
- **Root Cause:** The spec was written before the type was finalized.
  `u32` inherently rejects negatives via deserialization.
- **Impact:** None — negative values are impossible with the current type.
  The spec is misleading.
- **Confidence:** High
- **Remediation:** Update SB-0202 AC-5 to say "0 must be rejected" (remove
  "or negative" since the type is unsigned).

---

### F-007 — Hardware CI workflow still missing (residual F-022)

- **Severity:** Medium
- **Category:** `D1_REQUIREMENT_WITHOUT_DESIGN`
- **Location:** `docs/kicad-export-requirements.md` KE-1200+ ↔
  `.github/workflows/`
- **Description:** No CI workflow exists for `sonde-kicad` or hardware
  artifact generation.  The CI only builds and uploads `sonde-bundle`.
- **Evidence:** `.github/workflows/ci.yml` — no `sonde-kicad` job.
- **Root Cause:** Hardware tool is still maturing; CI deferred.
- **Impact:** Hardware tool regressions not caught in CI.
- **Confidence:** High
- **Remediation:** Add a basic `cargo build -p sonde-kicad` + `cargo test
  -p sonde-kicad` CI job when the tool stabilizes.
- **Residual:** Yes — F-022 from 2026-04-08 audit.

---

### F-008 — GW-1904 AC6 `bpf_trace_printk` logging untested

- **Severity:** Low
- **Category:** `D12_UNTESTED_ACCEPTANCE_CRITERION`
- **Location:** `docs/gateway-requirements.md` GW-1904 AC-6 ↔
  `crates/sonde-gateway/src/decoder.rs:273-287`
- **Description:** `bpf_trace_printk` is implemented (emits
  `tracing::debug!` at target `decoder_bpf`), but no test asserts the
  log output.
- **Evidence:** `decoder.rs:283` emits the log.  No test in `decoder.rs`
  captures tracing output.
- **Root Cause:** Tracing assertions require `tracing-test` or similar
  infrastructure not yet set up in the decoder test module.
- **Impact:** Low — the helper is simple and unlikely to regress.
- **Confidence:** High
- **Remediation:** Add a `#[traced_test]` test that runs a decoder program
  calling `bpf_trace_printk` and asserts the output appears.

---

### F-009 — Bundle handler path-safety checks are undocumented

- **Severity:** Low
- **Category:** `D9_UNDOCUMENTED_BEHAVIOR`
- **Location:** `crates/sonde-bundle/src/validate.rs:318-365` ↔
  `docs/bundle-tool-requirements.md` SB-0202
- **Description:** The implementation rejects `handler.working_dir` path
  traversal and non-directory paths, and rejects path traversal in
  `handler.command` and `handler.args`.  The spec (SB-0202) only requires
  program reference, catch-all uniqueness, non-empty command, and
  timeout > 0.
- **Evidence:** `validate.rs:318-365` implements path traversal checks.
  SB-0202 acceptance criteria do not mention path safety.
- **Root Cause:** Path traversal rejection was added as a defense-in-depth
  measure without updating the spec.
- **Impact:** Low — the behavior is correct and desirable.  Undocumented
  in the spec.
- **Confidence:** High
- **Remediation:** Add path-safety validation to SB-0202 acceptance
  criteria (document the existing behavior).

---

### F-010 — Modem firmware-version assertion too specific

- **Severity:** Low
- **Category:** `D13_ASSERTION_MISMATCH`
- **Location:** `docs/modem-requirements.md` MD-0104 ↔
  `crates/sonde-modem/tests/device_tests.rs:154-155`
- **Description:** The test asserts `firmware_version == [0, 1, 0, 0]`
  (exact value).  The requirement only says the field must be a valid
  4-byte value derived from `Cargo.toml` version.  This will break on
  every version bump.
- **Evidence:** `device_tests.rs:154-155` hard-codes the expected version.
  MD-0104 AC: "valid 4-byte value."
- **Root Cause:** Test was written at a specific version and not
  parameterized.
- **Impact:** Test fragility — breaks on version bumps but is easily
  noticed and fixed.
- **Confidence:** High
- **Remediation:** Change assertion to validate format (4 non-zero bytes)
  rather than exact value, or derive expected value from `CARGO_PKG_VERSION`.

---

### F-011 — AZH-0500/0501 SensorData storage not yet implemented

- **Severity:** High
- **Category:** `D8_UNIMPLEMENTED_REQUIREMENT`
- **Location:** `docs/azure-handler-requirements.md` AZH-0500, AZH-0501 ↔
  `crates/sonde-azure-handler/src/lib.rs`
- **Description:** AZH-0500 requires every `GW-0813` APP_DATA message to
  produce a row in the `SensorData` Azure Table.  AZH-0501 requires a
  `decoded_readings` column populated from decoder BPF output.  The current
  handler routes APP_DATA to handler processes but does not write to any
  `SensorData` table.  The provisioning Bicep also does not create a
  `SensorData` table.
- **Evidence:** `handle_app_data` in `lib.rs:335-353` only routes to
  handlers.  No `SensorData` table creation in Bicep modules.
- **Root Cause:** The SensorData feature is specified but not yet
  implemented — it depends on decoder BPF enrichment (GW-1903) which was
  only recently completed.
- **Impact:** Sensor data is not persisted for SPA visualization.  The
  SPA cannot query historical readings until this is implemented.
- **Confidence:** High
- **Status:** In progress on branch `feat/azh-0500-sensor-data-table`
  (commit `532dc52`).  Not yet merged to main.
- **Remediation:** Merge the in-progress branch.

---

## 5. Root Cause Analysis

### Pattern 1: Display test infrastructure gap (F-002)

The modem display lifecycle was implemented in PRs #790, #797, #800 with
clear spec coverage (T-0900–T-0907).  However, the modem test suite runs
on real hardware via `device_tests.rs`, and no mock display infrastructure
exists for unit testing.  The gateway-side display transfer is well-tested
(`admin_display.rs`), but the modem-side reception path is not.

### Pattern 2: Decoder test coverage near-miss (F-003, F-008)

The decoder BPF feature (GW-1900 series) was implemented with solid test
coverage: T-1900a–d, T-1902, T-1903, T-1904a–e.  Two acceptance criteria
(context ABI and `bpf_trace_printk` logging) were missed.  Both are
low-risk because Prevail verification covers the context ABI, and the
logging helper is simple.

### Pattern 3: Spec-type mismatch (F-006)

The bundle spec was written with language-agnostic types ("0 or negative")
while the implementation uses `u32`.  The mismatch creates a dead-letter
acceptance criterion.

### Pattern 4: Residual deferrals (F-005, F-007)

Two findings from the April 2026 audit remain open: modem timing assertion
(fragile on real hardware) and hardware CI workflow (tool still maturing).

---

## 6. Remediation Plan

Prioritized by severity:

| Priority | Finding | Action | Effort |
|----------|---------|--------|--------|
| 1 | F-001 (High) | Clarify AZH-0502 scope — handler vs SPA responsibility | Small (spec) |
| 2 | F-002 (Medium) | Add mock display for modem unit tests | Medium (code) |
| 3 | F-003 (Medium) | Add T-1904f decoder context ABI test | Small (test) |
| 4 | F-004 (Medium) | Update T-1304 spec or add integration test | Small (spec/test) |
| 5 | F-005 (Medium) | Add timing assertion or document as manual | Small (test/spec) |
| 6 | F-006 (Medium) | Update SB-0202 AC-5 to remove "negative" | Small (spec) |
| 7 | F-007 (Medium) | Add `sonde-kicad` CI job when tool stabilizes | Small (CI) |
| 8 | F-008 (Low) | Add `bpf_trace_printk` tracing test | Small (test) |
| 9 | F-009 (Low) | Add path-safety to SB-0202 acceptance criteria | Small (spec) |
| 10 | F-010 (Low) | Parameterize firmware-version assertion | Small (test) |

---

## 7. Prevention

1. **Test the spec items you add.** When adding validation plan entries
   (e.g., T-0900 series), implement at least the happy-path test in the
   same PR to prevent D11 accumulation.

2. **Update specs when types diverge.** If a spec says "0 or negative" but
   the type is `u32`, fix the spec in the implementation PR.

3. **Decoder test checklist.** For new BPF feature areas, walk through
   each acceptance criterion and verify test coverage before merging.

---

## 8. Open Questions

1. **AZH-0502 scope:** Does the SPA query the `SensorData` table directly
   via Azure Table Storage REST API with SAS tokens?  If so, AZH-0502
   should be re-scoped as a SPA/provisioning requirement, not a handler
   requirement.

2. **Modem display test strategy:** Should modem display tests use a mock
   OLED driver, or should they remain hardware-only?  Mock infrastructure
   would enable CI coverage for T-0900–T-0907.

---

## 9. Revision History

| Version | Date | Author | Changes |
|---------|------|--------|---------|
| 1.0 | 2026-05-15 | Copilot (maintenance audit) | Full audit — D1–D13 across all components. 11 findings (0 Critical, 2 High, 7 Medium, 2 Low). 20 of 27 previous-audit findings resolved. 5 deferred residuals carried forward. |
| 1.1 | 2026-05-15 | Copilot (patch audit) | Added F-011 (AZH-0500/0501 SensorData storage). Updated F-002 with existing bridge test coverage. Added T-1904f/T-1904g to gateway-validation.md. |

---

## Appendix A: Previous Audit Residual Status

| Finding | Component | Status | Evidence |
|---------|-----------|--------|----------|
| F-001 | Protocol | **Deferred** (intentional) | PeerRequest/PeerAck bypass codec by design |
| F-002 | Gateway | **FIXED** | `phase2b.rs:319` — `firmware_version` assertion added |
| F-003 | Gateway | **FIXED** | `phase2b.rs:335,376` — split into two sub-tests |
| F-004 | Protocol | **Deferred** (with F-001) | Intentional |
| F-005 | BPF | **FIXED** | `safe-bpf-interpreter.md:67,179` — spec updated |
| F-006 | BPF | **FIXED** | `safe-bpf-interpreter.md:72` — `Memory` tag added to spec |
| F-007 | Modem | **Partially fixed** | `bridge.rs` has BLE tests; `NoBle` mock limitation remains |
| F-008 | BLE Pairing | **FIXED** | `error.rs:55,64` — `ConnectionFailed`/`MtuTooLow` have device context |
| F-009 | Protocol | **FIXED** | Test count reconciled |
| F-010 | Modem | **Open** → carried as F-005 | Timing assertion still missing |
| F-011 | Modem | **FIXED** | `modem-requirements.md:509` — MD-0409 AC6 documents single-slot |
| F-012 | Modem | **FIXED** | `modem-requirements.md:510` — MD-0409 AC7 documents 32-entry queue |
| F-013 | Modem | **FIXED** | `bridge.rs:3399` — 65-chunk rejection test exists |
| F-014 | Modem | **Partially fixed** (with F-007) | `bridge.rs:2777` has full pre-auth flow test |
| F-015 | BLE Pairing | **FIXED** | `ble-pairing-protocol.md:611` — only in RETIRED section |
| F-016 | BLE Pairing | **FIXED** | `pair-ui/lib.rs:1042` — `resolve_board_layout_rejects_non_adc_battery_pin` |
| F-017 | BLE Pairing | **FIXED** | `phase2.rs:1523` — `board_layout_cbor_deterministic` test |
| F-018 | Node | **FIXED** | `node-requirements.md:755` — clear "1 initial + up to 3 retries" wording |
| F-019 | Node | **FIXED** | `map_storage.rs:536` — `log::debug!()` added |
| F-020 | Modem | **Accepted** | Documented as known limitation per spec |
| F-021 | Gateway | **FIXED** | `build_metadata.rs` — T-1304, T-1305a, T-1305b implemented |
| F-022 | Hardware | **Open** → carried as F-007 | No CI workflow yet |
| F-023 | Node | **FIXED** | `node-validation.md:1376,1392` — naming notes added |
| F-024 | BLE Pairing | **FIXED** | Named constants in all timeout locations |
| F-025 | BLE Pairing | **FIXED** | `AlreadyPaired` error variant and handling |
| F-026 | Modem | **Deferred** (hardware-only) | Watchdog requires physical hardware |
| F-027 | Modem | **Deferred** (low priority) | Logging assertion infrastructure not built |
