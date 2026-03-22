<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Contributing to Sonde

> **Document status:** Draft
> **Scope:** Contribution guidelines for the Sonde project: prerequisites, coding standards, commit requirements, and review process.
> **Audience:** Contributors (human or LLM agent) submitting code, documentation, or tests.
> **Related:** [overview.md](overview.md), [getting-started.md](getting-started.md), [README.md](../README.md)

---

## Repository status

**Lifecycle:** Active development — pre-1.0. Contributions of all sizes are welcome: bug fixes, documentation improvements, new features, and test coverage. See [overview.md § Repository status](overview.md#repository-status) for the current state of each crate.

**Maintenance:** The project is actively maintained. If you have questions before opening a pull request, open an issue to discuss the approach.

---

## Before you start

1. Read [overview.md](overview.md) to understand the project goals and architecture.
2. Read [getting-started.md](getting-started.md) to set up your development environment.
3. Browse open issues and pull requests to avoid duplicating work.
4. For significant changes, open an issue first to discuss the approach.

---

## Spec-first development model

Sonde follows a **spec-driven development workflow**. The specification documents — not the source code — are the source of truth. Every component has a document trifecta:

| Document | Purpose | Example |
|----------|---------|---------|
| **Requirements** (`*-requirements.md`) | What to build — REQ-IDs, acceptance criteria | `gateway-requirements.md` |
| **Design** (`*-design.md`) | How to build it — architecture, modules, traits | `gateway-design.md` |
| **Validation** (`*-validation.md`) | How to verify it — test cases, traceability matrix | `gateway-validation.md` |

### Why this matters

Code modules may be **regenerated from the spec** by LLM agents. If you edit code without updating the corresponding spec documents, your changes may be silently overwritten the next time a module is regenerated or audited against the spec.

### The contribution workflow

For any change that affects behavior:

1. **Update the requirements doc** — add or modify REQ-IDs and acceptance criteria.
2. **Update the design doc** — describe how the change is realized architecturally.
3. **Update the validation doc** — add or modify test cases with traceability links.
4. **Implement the code** — write the code that satisfies the spec.
5. **Audit** — verify the code matches the spec (see `prompts/` for audit templates).

For pure bug fixes where the spec is already correct, step 4 alone is sufficient — but add a test (step 3) if one is missing.

### What NOT to do

- **Don't edit code without updating the spec.** The spec trifecta must always reflect the intended behavior. Code that drifts from the spec will be flagged by traceability audits.
- **Don't add features by code alone.** A feature without a requirement ID is untracked and untestable against the spec.
- **Don't modify test assertions without checking the validation doc.** Test cases are specified in the validation document; the code tests should match.

### Audit prompts

The `prompts/` directory contains reusable audit templates:

- `sonde-trifecta-audit.md` — cross-document traceability (D1–D7: requirements ↔ design ↔ validation)
- `sonde-code-compliance-audit.md` — code vs spec compliance (D8–D10: is the code what the spec says?)
- `sonde-test-compliance-audit.md` — test vs spec compliance (D11–D13: do the tests match the validation plan?)
- `sonde-audit-guide.md` — workflow guide for running audits

---

## Requirements for all contributions

Every pull request must satisfy the following requirements before it will be merged.

### 1. SPDX license headers

Every `.rs` file must start with:

```rust
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors
```

Every `.md` file must start with:

```html
<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
```

The repository's git hooks enforce this automatically (see [Git hooks](#git-hooks) below).

### 2. DCO sign-off

Every commit must include a `Signed-off-by:` trailer. Use `git commit -s` to add it automatically:

```sh
git commit -s -m "your commit message"
```

The sign-off certifies that you have the right to submit the work under the project license (see [Developer Certificate of Origin](https://developercertificate.org/)).

### 3. Formatting and linting

All code must pass `cargo fmt` and `cargo clippy` before submission:

```sh
cargo fmt --all
cargo clippy --workspace -- -D warnings
```

### 4. Tests

- All new code paths must have tests.
- Tests must pass: `cargo test --workspace`.
- For protocol changes, also run: `cargo test -p sonde-protocol`.

---

## Git hooks

Install the repository's git hooks to enforce the SPDX and DCO requirements locally:

```sh
git config core.hooksPath hooks
```

Alternatively, use [pre-commit](https://pre-commit.com):

```sh
pip install pre-commit
pre-commit install --hook-type pre-commit --hook-type commit-msg
```

---

## Code style

- Follow Rust standard conventions enforced by `rustfmt` and `clippy`.
- Use backticks (not backslash-escaped quotes) to wrap identifiers in PR descriptions and commit messages.
- CBOR maps use integer keys (not strings) for compactness — see [protocol.md](protocol.md) for details.
- Platform-specific behavior must be injected via traits (`HmacProvider`, `Sha256Provider`, `Transport`, `Storage`, `BpfInterpreter`), never hard-coded.

---

## Pull request process

1. Fork the repository and create a branch from `main`.
2. Make your changes, following the requirements above.
3. Ensure all tests pass locally.
4. Open a pull request with a clear description of what changed and why.
5. Address any review feedback.

CI runs formatting, clippy, build, workspace tests, fuzz (protocol), and an ESP32 QEMU smoke test on every PR. All checks must pass before merging.

---

## Further reading

- [Overview](overview.md) — project status, goals, and architecture summary
- [Getting Started](getting-started.md) — development environment setup
- [Implementation Guide](implementation-guide.md) — phased build plan and module ordering
- [Protocol](protocol.md) — wire protocol specification
- [Security Model](security.md) — security model and threat analysis
