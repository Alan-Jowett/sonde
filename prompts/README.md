# Sonde Prompts

Structured prompts for driving LLM agents through engineering workflows.
Generated with [PromptKit](https://aka.ms/PromptKit).

Each prompt defines a **persona** (domain expert), **reasoning protocols**
(anti-hallucination guardrails, self-verification), and a **task** with
specific inputs and output format requirements.

## Directory structure

```
prompts/
├── hardware/     # PCB design pipeline (11 stages)
├── software/     # Code review and audit workflows
└── workflows/    # Project lifecycle workflows
```

---

## Hardware (`hardware/`)

An 11-stage pipeline for designing, reviewing, and validating a PCB from
requirements through to manufacturing. Each stage feeds into the next.

| # | Prompt | Persona | Purpose |
|---|--------|---------|---------|
| 00 | `author-schematic-design` | Senior EE | Design the circuit from hw-requirements |
| 01 | `review-schematic` | Senior EE | Adversarial review of the schematic |
| 02 | `validate-simulation` | Senior EE | SPICE simulation validation |
| 03 | `review-bom` | Senior EE | Bill of materials review |
| 04 | `review-layout` | Senior EE | PCB layout review |
| 05 | `author-interface-contract` | Systems Engineer | Hardware↔firmware interface spec |
| 06 | `audit-interface-contract` | Spec Analyst | Cross-check contract vs requirements |
| 07 | `validate-power-budget` | Spec Analyst | Power analysis vs HW-1100 contract |
| 08 | `validate-cost-budget` | Spec Analyst | Cost target validation |
| 09 | `audit-spec-invariants` | Firmware Engineer | Adversarial spec invariant audit |
| 10 | `reconstruct-behavior` | Reverse Engineer | Behavioral reconstruction from design |

**Usage:**

```
# Feed hw-requirements.md as the specification input
# Feed hw-schematic-design.md as the schematic (for stages 01+)
# Each stage produces an investigation report or design document
```

**Input documents:**
- `docs/kicad-export-requirements.md` — KiCad export tool requirements
- `docs/kicad-export-design.md` — KiCad export tool architecture
- `docs/kicad-export-validation.md` — KiCad export tool test plan

---

## Software (`software/`)

Prompts for code review, specification auditing, and PR management.

| Prompt | Purpose |
|--------|---------|
| `sonde-pr-loop` | Autonomous PR review loop (CODER → REVIEWER → VALIDATOR) |
| `sonde-pr-review-workflow` | Review classification and termination rules |
| `sonde-audit-guide` | General audit methodology |
| `sonde-code-compliance-audit` | Cross-check requirements ↔ code |
| `sonde-test-compliance-audit` | Cross-check requirements ↔ validation ↔ tests |
| `sonde-trifecta-audit` | Cross-check requirements ↔ design ↔ validation |

**Usage:**

```
# PR loop: feed the PR diff and review comments
# Audits: feed the relevant spec docs + code/test files
# All produce structured findings with severity classification
```

---

## Workflows (`workflows/`)

Project lifecycle prompts for bootstrapping, evolving, and maintaining
systems software projects.

| Prompt | Purpose |
|--------|---------|
| `bootstrap` | New project setup — architecture, dependencies, CI |
| `evolve` | Feature development — design, implementation, testing |
| `maintain` | Maintenance — bug fixes, refactoring, dependency updates |

All three use a **Senior Systems Engineer** persona with expertise in
memory management, concurrency, performance, and debugging.

---

## How to use

These prompts are designed to be fed to LLM agents (GitHub Copilot CLI,
Claude, GPT, etc.) as system instructions. The general pattern:

1. **Read the prompt** — it defines the persona, protocols, and task
2. **Paste your inputs** where indicated (`<!-- PASTE HERE -->`)
3. **Run the agent** — it follows the protocols and produces structured output
4. **Review the output** — check findings against the source material

For automated workflows, prompts can be loaded programmatically:

```python
with open("prompts/hardware/09-audit-spec-invariants.md") as f:
    system_prompt = f.read()

# Replace placeholders with actual content
system_prompt = system_prompt.replace(
    "<!-- PASTE THE SPECIFICATION TEXT TO AUDIT HERE -->",
    open("docs/hw-requirements.md").read()
)
```

---

## PromptKit

These prompts were generated with [PromptKit](https://aka.ms/PromptKit),
a tool for creating structured, reproducible prompts with built-in
reasoning protocols and anti-hallucination guardrails.
