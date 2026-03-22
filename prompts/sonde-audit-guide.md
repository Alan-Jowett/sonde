# Sonde Audit Guide — Using PromptKit for Specification Integrity

This guide explains how to use the assembled PromptKit audit prompts
to audit any Sonde component. Three prompts are provided:

1. **Trifecta audit** (`sonde-trifecta-audit.md`) — cross-checks
   requirements ↔ design ↔ validation for specification drift (D1–D7)
2. **Code compliance audit** (`sonde-code-compliance-audit.md`) —
   cross-checks requirements ↔ source code for implementation drift
   (D8–D10)
3. **Test compliance audit** (`sonde-test-compliance-audit.md`) —
   cross-checks requirements ↔ validation plan ↔ test code for test
   compliance drift (D11–D13)

## Component Map

Each Sonde component has a matching set of artifacts:

| Component | Requirements | Design | Validation | Code |
|-----------|-------------|--------|------------|------|
| **Node** | `node-requirements.md` | `node-design.md` | `node-validation.md` | `crates/sonde-node/` |
| **Gateway** | `gateway-requirements.md` | `gateway-design.md` | `gateway-validation.md` | `crates/sonde-gateway/` |
| **Modem** | `modem-requirements.md` | `modem-design.md` | `modem-validation.md` | `crates/sonde-modem/` |
| **Pairing tool** | `ble-pairing-tool-requirements.md` | `ble-pairing-tool-design.md` | `ble-pairing-tool-validation.md` | `crates/sonde-pair/` |
| **Protocol crate** | (embedded in `protocol.md`) | `protocol-crate-design.md` | `protocol-crate-validation.md` | `crates/sonde-protocol/` |
| **E2E** | (derived from component reqs) | — | `e2e-validation.md` | `crates/sonde-e2e/` |

## Pass 1: Trifecta Audit (Doc ↔ Doc ↔ Doc)

### What it finds

- **D1**: Requirements not referenced in design
- **D2**: Requirements with no test case
- **D3**: Design decisions with no originating requirement
- **D4**: Test cases with no linked requirement
- **D5**: Assumption conflicts across documents
- **D6**: Design violates a stated constraint
- **D7**: Test case doesn't verify its linked acceptance criteria

### How to run

Open a new LLM session and paste the contents of
`sonde-trifecta-audit.md`. Then fill in the placeholders:

```
{{project_name}}    →  e.g., "Sonde Modem"
{{requirements_doc}} →  paste contents of modem-requirements.md
{{design_doc}}       →  paste contents of modem-design.md
{{validation_plan}}  →  paste contents of modem-validation.md
{{focus_areas}}      →  "all" (or narrow: "security requirements only")
```

For large documents, you may need to use a model with a large context
window (128K+). The modem is the smallest component and fits easily.
For gateway (58KB reqs + 37KB design + 54KB validation = ~149KB of
spec content), use a 200K+ context model or narrow the focus areas.

### CLI alternative

```bash
npx @alan-jowett/promptkit assemble audit-traceability \
  -p project_name="Sonde Modem" \
  -p requirements_doc="$(cat docs/modem-requirements.md)" \
  -p design_doc="$(cat docs/modem-design.md)" \
  -p validation_plan="$(cat docs/modem-validation.md)" \
  -p focus_areas="all" \
  -o modem-trifecta-audit.md
```

Then feed the assembled prompt to your LLM.

### Repeat for each component

Replace the three document paths for each component from the table
above. The prompt itself never changes — only the input documents.

## Pass 2: Code Compliance Audit (Spec ↔ Code)

### What it finds

- **D8**: Requirement in spec has no implementation in code
- **D9**: Code implements behavior not in any requirement
- **D10**: Code violates a stated constraint

### How to run

Open a new LLM session and paste the contents of
`sonde-code-compliance-audit.md`. Then fill in the placeholders:

```
{{project_name}}      →  e.g., "Sonde Modem"
{{requirements_doc}}  →  paste contents of modem-requirements.md
{{design_doc}}        →  paste contents of modem-design.md (or "N/A")
{{code_context}}      →  paste key source files from crates/sonde-modem/
{{focus_areas}}       →  "all" (or narrow: "authentication requirements")
```

For the code context, focus on the **behavioral surface**:
- `src/lib.rs` or `src/main.rs` — entry points
- Public API modules — what the crate exposes
- Configuration and initialization — how behavior is parameterized
- Error handling — how failures are managed

You don't need to paste every file. The protocol instructs the LLM
to trace inward from the API surface only as needed.

### Scoping for large codebases

For larger components (gateway, node), use `focus_areas` to narrow:

```
-p focus_areas="authentication and session management requirements only"
-p focus_areas="program distribution requirements (GW-05xx)"
-p focus_areas="wake cycle and power management (ND-03xx)"
```

This keeps the audit tractable while still producing useful findings.

## Pass 3: Test Compliance Audit (Spec ↔ Test Code)

### What it finds

- **D11**: Test case in validation plan has no corresponding automated test
- **D12**: Test exists but doesn't assert all acceptance criteria
- **D13**: Test assertions don't match the expected behavior from the plan

### How to run

Open a new LLM session and paste the contents of
`sonde-test-compliance-audit.md`. Then fill in the placeholders:

```
{{project_name}}      →  e.g., "Sonde Modem"
{{requirements_doc}}  →  paste contents of modem-requirements.md
{{validation_plan}}   →  paste contents of modem-validation.md
{{test_code}}         →  paste test files from crates/sonde-modem/tests/
{{focus_areas}}       →  "all" (or narrow: "TC-001 through TC-020")
```

For the test code, focus on:
- Test files (`tests/`, `src/**/tests.rs`, `#[cfg(test)]` modules)
- Test helper/fixture files
- Integration test directories

The protocol maps TC-NNN definitions from the validation plan to test
functions by name, comment references, or behavioral equivalence.

### Scoping for large test suites

For components with many test cases, use `focus_areas` to narrow:

```
-p focus_areas="security test cases (TC-SEC-*)"
-p focus_areas="BLE pairing test cases only"
-p focus_areas="TC-P001 through TC-P020"
```

## Recommended Order

1. **Modem** — smallest component, good first test
2. **Protocol crate** — well-tested (validation suite + fuzz targets)
3. **Node** — recently closed gaps (#369), interesting to verify
4. **Gateway** — largest surface, most likely to surface drift
5. **Pairing tool** — biggest spec/code divergence, most findings expected

## Recording Results

For the case study, record for each component:

1. Which prompt was used (trifecta, code compliance, test compliance, or all)
2. Total findings by drift type (D1–D13)
3. Top 3 most impactful findings (with evidence)
4. Coverage metrics from the executive summary
5. Any false positives or areas where the LLM struggled
6. Time spent (prompt assembly vs. LLM execution vs. review)
