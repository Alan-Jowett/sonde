<!-- SPDX-License-Identifier: MIT -->
<!-- Copyright (c) 2026 sonde contributors -->
# Sonde Web UI — Extraction Audit

> **Document status:** Complete
> **Scope:** Consistency audit of the web-ui requirements/design/validation trifecta.
> **Audit date:** 2026-05-19

---

## 1  Executive Summary

The Web UI spec trifecta is internally consistent after remediation. The
extraction produced 42 formal requirements (WEB-0101 through WEB-CC-04),
all at High confidence, traced to code and design documentation. Three
blocking inconsistencies were found and fixed during the audit; 8
non-blocking gaps were addressed by adding 13 new test cases. The spec
set is ready for approval.

**Verdict: PASS**

---

## 2  Problem Statement

The Sonde Web UI had a design doc and validation plan but no formal
requirements document. The validation plan referenced WEB-XXXX requirement
IDs that were never formally defined. This audit verifies the newly
extracted requirements document is consistent with the existing design
doc and validation plan.

---

## 3  Investigation Scope

| Document | Path | Role |
|----------|------|------|
| Requirements | `docs/web-ui-requirements.md` | New — extracted |
| Design | `docs/web-ui-design.md` | Existing — updated |
| Validation | `docs/web-ui-validation.md` | Existing — restructured |
| Source code | `deploy/web-ui/app.js` | Reference implementation |

---

## 4  Findings

### F-001  [D6] ProgramIngest route naming (Non-blocking)

**Severity:** Low
**Evidence:** Design doc uses both `/ProgramIngest` (Azure Functions internal
route name) and `/api/programs/ingest` (external HTTP route). Requirements
use the external route.
**Resolution:** Not a true inconsistency — these are different layers. The
internal route name (`/ProgramIngest`) is the Azure Functions handler name;
the external HTTP route (`/api/programs/ingest`) is what the SPA calls.
Design doc §6.1 and §9.3 already explain this distinction.

### F-002  [D6] sessionStorage clearing scope (Fixed)

**Severity:** Medium
**Evidence:** Design §11.5 said "`sessionStorage` is cleared" but
requirements WEB-0806 AC-5 and the code (`clearMsalSessionStorage()`)
only clear MSAL-related keys.
**Resolution:** Updated design §11.5 to specify MSAL-related keys only.

### F-003  [D6] Redirect URI path normalization (Fixed)

**Severity:** Medium
**Evidence:** Design §8 said `redirectUri` uses `window.location.pathname`
directly, but the code normalizes it by stripping filename components.
Requirements WEB-0508 AC-3 requires this normalization.
**Resolution:** Updated design §8 to describe normalized directory path.

### F-004  [D3] Stale `programroute` reference (Fixed)

**Severity:** Low
**Evidence:** Design §2 architecture diagram referenced `programroute` table
but no requirement, test, or code uses it.
**Resolution:** Removed from architecture diagram.

### F-005  [D2] Untested cross-cutting requirements (Fixed)

**Severity:** Medium
**Evidence:** WEB-CC-01 through WEB-CC-04 had no test case IDs.
**Resolution:** Added T-WEB-CC-01 through T-WEB-CC-04 with appropriate
methods (inspection, code review, manual).

### F-006  [D7] Missing schedule divergence tests (Fixed)

**Severity:** Medium
**Evidence:** WEB-0104 AC-4 (schedule divergence) and AC-5 (tooltip) had no
dedicated test cases.
**Resolution:** Added T-WEB-0107 and T-WEB-0108.

### F-007  [D7] Missing graph constraint tests (Fixed)

**Severity:** Low
**Evidence:** WEB-0701 AC-3 (20-series max), AC-4 (500-point downsample),
and AC-5 (tooltip) had no dedicated test cases.
**Resolution:** Added T-WEB-0713, T-WEB-0714, T-WEB-0715.

### F-008  [D7] Environment validation test gaps (Fixed)

**Severity:** Low
**Evidence:** WEB-0802 GUID/format validation acceptance criteria were not
individually tested.
**Resolution:** Added T-WEB-0807, T-WEB-0808, T-WEB-0809. Fixed traceability
matrix to map T-WEB-0806 to WEB-0802.

### F-009  [D7] Redirect URI edge case (Fixed)

**Severity:** Low
**Evidence:** WEB-0508 AC-3 (`/index.html` stripping) was untested.
**Resolution:** Added T-WEB-0509.

### F-010  [D5] Sovereign cloud scope (Informational)

**Severity:** Informational
**Evidence:** Requirements ASM-001 says Azure public cloud only. Design §9.4
uses `environment()` in Bicep which works in sovereign clouds. Design §11.3
also says sovereign cloud out of scope.
**Resolution:** No change needed — `environment()` is incidental Bicep
portability, not an end-to-end sovereign cloud commitment.

### F-011  [D5] Skipped ID WEB-0601 (Informational)

**Severity:** Informational
**Evidence:** WEB-0601 is not defined. The infrastructure section starts at
WEB-0602.
**Resolution:** Accepted as intentional — WEB-0601 may have been reserved or
removed during design evolution.

---

## 5  Root Cause Analysis

The primary root cause is that the design doc served dual duty as both
requirements and design specification. This created:
- Implicit requirements (behaviors described but not formally identified)
- Inconsistencies between high-level design descriptions and actual code behavior
- Missing test coverage for cross-cutting concerns

---

## 6  Remediation Plan

All blocking and non-blocking findings have been remediated in this session:
- Design doc: 3 corrections (F-002, F-003, F-004)
- Validation doc: 13 new test cases, updated traceability matrix
- No requirements doc changes needed (all findings were in design/validation)

---

## 7  Prevention

1. Maintain the requirements doc as the single source of truth for WEB-XXXX IDs
2. Update the traceability matrix whenever adding new requirements or test cases
3. Run traceability audits after each major spec update

---

## 8  Open Questions

None. All findings resolved.

**Verdict: PASS**

---

## 9  Revision History

| Date | Author | Description |
|------|--------|-------------|
| 2026-05-19 | Spec extraction (automated) | Initial extraction audit. |
