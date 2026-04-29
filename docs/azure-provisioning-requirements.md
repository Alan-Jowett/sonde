<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Azure Provisioning Requirements Specification

> **Document status:** Draft
> **Source:** [issue #772](https://github.com/Alan-Jowett/sonde/issues/772),
> discovery review for the Azure companion architecture, and
> [azure-companion-requirements.md](azure-companion-requirements.md).
> **Scope:** This document covers the Azure-side provisioning workflow for the
> Azure companion deployment model: Bicep-managed resource provisioning,
> companion runtime identity provisioning, and the bootstrap handoff contract
> needed by `sonde-azure-companion`. It does not define Azure Function decoder
> logic, table schema semantics, or dashboarding.
> **Related:** [azure-provisioning-design.md](azure-provisioning-design.md),
> [azure-provisioning-validation.md](azure-provisioning-validation.md),
> [azure-companion-requirements.md](azure-companion-requirements.md),
> [azure-companion-design.md](azure-companion-design.md)

---

## 1  Definitions

| Term | Definition |
|------|------------|
| **Azure provisioning workflow** | The repository-owned deployment workflow that creates the Azure resources and identity material required by the Azure companion architecture. |
| **Bicep root deployment** | The top-level Bicep entrypoint under `deploy/bicep/` that composes the provisioning modules for this workflow. |
| **Runtime identity bundle** | The Entra tenant/client identity plus certificate-authenticated service-principal material required for the Azure companion runtime after bootstrap completes. |
| **Bootstrap handoff contract** | The defined set of outputs and artifact locations that lets bootstrap materialize `service-principal.json`, certificate PEM, and private-key PEM for `sonde-azure-companion`. |
| **Function placeholder** | Azure Function hosting resources reserved for a later decoder implementation, without requiring the decoder code to exist in this issue. |
| **Storage resources** | The Azure Storage Account and Table resources reserved for later decoded-data persistence. This document covers only their provisioning, not the logical table schema. |

---

## 2  Requirement format

Each requirement uses the following fields:

- **ID** — Unique identifier (`AZP-XXXX`).
- **Title** — Short name.
- **Description** — What the provisioning workflow must do.
- **Acceptance criteria** — Observable, testable conditions that confirm the requirement is met.
- **Priority** — MoSCoW: **Must**, **Should**, **May**.
- **Source** — Issue, companion specification, or reviewed discovery output that motivates the requirement.

---

## 3  Bicep deployment structure

### AZP-0100  Bicep-based provisioning entrypoint

**Priority:** Must
**Source:** [issue #772](https://github.com/Alan-Jowett/sonde/issues/772), reviewed discovery output

**Description:**
The repository MUST provide a Bicep-based provisioning entrypoint under
`deploy/bicep/` for the Azure companion deployment model. The Bicep workflow
MUST be the canonical infrastructure-as-code surface for this issue.

**Acceptance criteria:**

1. The repository contains a top-level Bicep deployment entrypoint under `deploy/bicep/`.
2. The deployment exposes `location`, `project_name`, and `resource_group_name` inputs.
3. The default `location` is `eastus` unless the caller overrides it.
4. The default `project_name` is `sonde` unless the caller overrides it.
5. `resource_group_name` remains an optional override rather than a required input.
6. The deployment can render a plan/what-if view of the resources it intends to create.
7. When the workflow derives resource names from `project_name`, it documents or applies any normalization needed to satisfy Azure provider naming rules.
8. The deployment documentation identifies the parameters and outputs required for Azure companion provisioning.

---

### AZP-0101  Resource group and tagging

**Priority:** Must
**Source:** [issue #772](https://github.com/Alan-Jowett/sonde/issues/772)

**Description:**
The provisioning workflow MUST create or target a dedicated Azure resource group
for the Sonde Azure companion stack and MUST tag the managed Azure resources
with `project = sonde` by default.

**Acceptance criteria:**

1. The workflow can create a dedicated resource group when one does not already exist.
2. The workflow can target a caller-specified resource-group override instead of inventing a second group.
3. Service Bus, Storage, and Function placeholder resources deployed by this workflow carry the `project = sonde` tag unless the caller overrides the value explicitly.

---

### AZP-0102  Service Bus namespace and queues

**Priority:** Must
**Source:** [issue #772](https://github.com/Alan-Jowett/sonde/issues/772), AZC-0302, AZC-0304

**Description:**
The provisioning workflow MUST create the Azure Service Bus resources required
by the Azure companion runtime: one namespace plus one upstream queue and one
downstream queue. The namespace uses the Standard tier unless the caller
explicitly opts into a different supported tier.

**Acceptance criteria:**

1. The workflow provisions one Service Bus namespace.
2. The workflow provisions one upstream queue for gateway-originated connector traffic.
3. The workflow provisions one downstream queue for cloud-originated desired-state traffic.
4. The default namespace tier is Standard.
5. The workflow exposes the namespace and queue names as deployment outputs or documented post-deploy values consumable by Azure companion bootstrap/runtime configuration.
6. The default namespace configuration disables local/SAS authentication so Entra-based RBAC is the only steady-state access path unless a later specification explicitly broadens it.

---

### AZP-0103  Storage account and table resources

**Priority:** Must
**Source:** [issue #772](https://github.com/Alan-Jowett/sonde/issues/772), reviewed discovery output

**Description:**
The provisioning workflow MUST create the Azure Storage resources reserved for
later decoded-data persistence: a Storage Account and the required Table
resource. This issue does not define the table's logical schema.

**Acceptance criteria:**

1. The workflow provisions one Azure Storage Account for this stack.
2. The workflow provisions the Table resource needed by the later decoder path.
3. The workflow documents that table schema ownership is deferred to the later Azure Function issue and is not defined by this provisioning specification.
4. The workflow does not expose raw Storage Account keys in deployment outputs or bootstrap handoff values.

---

### AZP-0104  Function placeholder infrastructure

**Priority:** Must
**Source:** [issue #772](https://github.com/Alan-Jowett/sonde/issues/772), reviewed discovery output

**Description:**
The provisioning workflow MUST create placeholder Azure Function hosting
resources for the later decoder implementation without requiring the decoder
code to exist in this issue. The placeholder Function App uses a consumption
plan unless a later issue explicitly changes that hosting model.

**Acceptance criteria:**

1. The workflow provisions the Azure resources needed to host the later decoder Function App.
2. The workflow does not require the decoder function code package to exist in order to deploy the placeholder resources.
3. The placeholder Function App resources use a consumption-plan hosting model.
4. The deployment outputs or documentation identify the placeholder Function App resources reserved for the later issue.

---

## 4  Runtime identity and bootstrap handoff

### AZP-0200  Certificate-authenticated runtime identity

**Priority:** Must
**Source:** reviewed discovery output, AZC-0305

**Description:**
The provisioning workflow MUST define and provision the Azure runtime identity
model required by `sonde-azure-companion`: an Entra application/service
principal that authenticates with a certificate rather than managed identity or
interactive login during normal runtime operation.

**Acceptance criteria:**

1. The workflow creates or configures an Entra application/service principal for the Azure companion runtime.
2. The workflow defines certificate-based authentication material for that identity.
3. The workflow does not require Azure managed identity, Azure Arc, or interactive device login for steady-state runtime authentication.

---

### AZP-0201  Service Bus role assignments for bridge directions

**Priority:** Must
**Source:** reviewed discovery output, AZC-0304, AZC-0308

**Description:**
The provisioning workflow MUST assign Service Bus permissions that match the
Azure companion bridge's bidirectional behavior: upstream publish plus
downstream consume and settlement.

**Acceptance criteria:**

1. The runtime identity can send messages to the configured upstream queue.
2. The runtime identity can receive and settle messages from the configured downstream queue.
3. The documented permissions align with the Azure companion bridge responsibilities and do not rely on unrelated administrator privileges.

---

### AZP-0202  Function placeholder managed identity and data-plane RBAC

**Priority:** Must
**Source:** reviewed discovery output

**Description:**
The provisioning workflow MUST attach a system-assigned managed identity to the
placeholder Azure Function App and grant only the data-plane permissions needed
for the later decoder/control-plane function path: receive from the upstream
queue, send on the downstream queue, and write decoded records to Table
Storage.

**Acceptance criteria:**

1. The placeholder Function App has a system-assigned managed identity.
2. That identity can receive messages from the configured upstream queue.
3. That identity can send messages to the configured downstream queue.
4. That identity can write to the Storage Table reserved for decoded data.
5. The Function App identity is distinct from the Azure companion runtime identity unless a later specification explicitly merges them.

---

### AZP-0203  Bootstrap handoff contract

**Priority:** Must
**Source:** reviewed discovery output, AZC-0200, AZC-0305

**Description:**
The provisioning workflow MUST define a handoff contract that lets Azure
companion bootstrap produce the local runtime artifacts expected by
`sonde-azure-companion`, including `service-principal.json`, certificate PEM,
and private-key PEM.

**Acceptance criteria:**

1. The workflow documents which values and artifacts must be handed off to bootstrap for runtime starts.
2. The handoff contract includes the tenant ID, client ID, certificate reference or material, private-key reference or material, and Service Bus namespace/queue configuration.
3. The handoff contract is compatible with the current Azure companion runtime expectations in `azure-companion-requirements.md`.

---

## 5  Lifecycle behavior

### AZP-0300  Idempotent deployment workflow

**Priority:** Must
**Source:** [issue #772](https://github.com/Alan-Jowett/sonde/issues/772), [issue #771](https://github.com/Alan-Jowett/sonde/issues/771)

**Description:**
The Bicep-based provisioning workflow MUST support repeatable deployment of the
Azure companion infrastructure without requiring manual cleanup between runs.

**Acceptance criteria:**

1. Re-running the deployment against an already-provisioned stack succeeds without creating duplicate foundational resources.
2. The deployment documentation identifies any resources whose updates are intentionally constrained or one-time.
3. The workflow surfaces deployment failures rather than silently masking them.

---

### AZP-0301  Removable stack

**Priority:** Must
**Source:** [issue #772](https://github.com/Alan-Jowett/sonde/issues/772)

**Description:**
The provisioning workflow SHOULD support removal of the Azure resources it
created for this stack or document any intentionally retained artifacts that
require explicit manual handling.

**Acceptance criteria:**

1. The documented teardown path removes the resource-plane infrastructure created for this stack, or clearly enumerates any retained artifacts.
2. Teardown behavior is documented for Service Bus, Storage, and Function placeholder resources.

