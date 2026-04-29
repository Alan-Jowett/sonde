<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Azure Provisioning Design Specification

> **Document status:** Draft
> **Scope:** Design for the Azure provisioning workflow that supports the Azure
> companion deployment model: Bicep-managed resource provisioning, runtime
> identity setup, and bootstrap handoff artifacts.
> **Audience:** Implementers building the provisioning workflow under
> `deploy/bicep/`.
> **Related:** [azure-provisioning-requirements.md](azure-provisioning-requirements.md),
> [azure-provisioning-validation.md](azure-provisioning-validation.md),
> [azure-companion-requirements.md](azure-companion-requirements.md),
> [azure-companion-design.md](azure-companion-design.md)

---

## 1  Overview

The Azure provisioning workflow exists to supply the Azure-side prerequisites
for the current Azure companion architecture. It is not the runtime bridge
itself; instead, it provisions the resources and identity material that let the
bootstrap and runtime paths described in `azure-companion-design.md` operate.

This document therefore separates the problem into three layers:

1. **Resource-plane provisioning** via Bicep for Service Bus, Storage, and
   Function placeholder resources.
2. **Runtime identity provisioning** for the certificate-authenticated Entra
   application/service principal used by `sonde-azure-companion`.
3. **Bootstrap handoff** that turns Azure deployment outputs into the local
   runtime artifacts consumed by the Azure companion container.

---

## 2  Repository layout

> **Requirements:** AZP-0100

The provisioning work is rooted under `deploy/bicep/`.

The design assumes this layout:

| Artifact | Purpose |
|----------|---------|
| `deploy/bicep/main.bicep` | Top-level deployment entrypoint. |
| `deploy/bicep/modules/service-bus.bicep` | Service Bus namespace and queue provisioning. |
| `deploy/bicep/modules/storage.bicep` | Storage Account and Table resource provisioning. |
| `deploy/bicep/modules/function-placeholder.bicep` | Placeholder Function hosting resources for the later decoder issue. |
| `deploy/bicep/modules/identity.*` | Runtime identity provisioning artifacts or wrappers used by the Bicep-driven workflow. |
| `deploy/bicep/README.md` or equivalent inline deployment documentation | Operator-facing description of inputs, outputs, and post-deploy handoff. |

The exact file count is not normative. The important design constraint is that
the repository exposes one Bicep-rooted deployment surface rather than a mix of
unrelated ad hoc provisioning entrypoints.

---

## 3  Deployment model

> **Requirements:** AZP-0100, AZP-0101, AZP-0102, AZP-0103, AZP-0104, AZP-0300, AZP-0301

### 3.1  Inputs

The top-level deployment accepts these caller-provided inputs:

| Input | Purpose |
|-------|---------|
| `location` | Azure region for the stack. Default: `eastus`. |
| `project_name` | Resource-name prefix and default project tag value. Default: `sonde`. |
| `resource_group_name` | Optional override for the target resource group name. |

The deployment may accept additional inputs as implementation details, but
those three are the required stable interface inherited from issue #772.
Derived resource names may normalize `project_name` as needed to satisfy Azure
provider naming constraints while preserving the caller-visible deployment
interface.

### 3.2  Resource group and tagging

The deployment targets one resource group for the Azure companion stack. If the
caller does not supply `resource_group_name`, the workflow derives one from
`project_name`. All resource-plane resources managed by this workflow inherit a
common tag set whose required baseline entry is `project = sonde` unless the
caller intentionally overrides the project value.

### 3.3  Service Bus resources

The Service Bus module provisions:

1. one namespace,
2. one upstream queue for gateway-originated connector traffic, and
3. one downstream queue for desired-state ingress.

The design keeps the queue names explicit deployment outputs so bootstrap and
runtime configuration can consume them directly rather than relying on embedded
defaults inside `sonde-azure-companion`. The default namespace tier is
Standard.
The namespace disables local/SAS authentication by default so steady-state
access is mediated through Entra identities and the scoped RBAC grants defined
elsewhere in this design.

### 3.4  Storage resources

The storage module provisions:

1. one Storage Account, and
2. the Table resource reserved for later decoded-data persistence.

This module intentionally stops at resource creation. It does not define the
table's logical schema, retention semantics, or decoded-column contract; those
belong to the later Azure Function work that will own decode/storage behavior.
When the Function placeholder needs storage credentials for deployment wiring,
the design keeps that secret handling inside the consuming module rather than
surfacing raw account keys as deployment outputs.

### 3.5  Function placeholder resources

The function-placeholder module provisions the hosting resources that reserve
space for the later decoder Function App. This module may create the Function
App shell, a consumption hosting plan, storage linkage, and placeholder app
settings, but it must not depend on the later function code package being
present in this issue.

---

## 4  Runtime identity provisioning

> **Requirements:** AZP-0200, AZP-0201, AZP-0202

The current Azure companion runtime design uses a certificate-authenticated
Entra application/service principal rather than managed identity. The
provisioning workflow therefore needs an identity phase in addition to the
resource-plane Bicep modules.

### 4.1  Identity model

The runtime identity consists of:

1. an Entra application registration,
2. its corresponding service principal,
3. a certificate credential bound to that application identity, and
4. Service Bus permissions aligned with the bridge's upstream send and
   downstream receive/settle behavior.

### 4.2  Bicep boundary

The Bicep workflow is the canonical deployment surface, but the design does not
assume that every Entra and certificate operation can or should be expressed as
pure resource-plane declarations. Instead, the workflow may include an adjunct
identity step as long as:

1. the overall operator entrypoint remains Bicep-rooted,
2. the identity step is repository-owned and documented, and
3. the resulting artifacts and outputs satisfy the bootstrap handoff contract.

This keeps the requirements honest about the difference between Azure
resource-plane provisioning and Entra/certificate lifecycle work, while still
treating both as part of the same issue.

### 4.3  Role assignments

The identity step must assign only the Service Bus permissions required by the
Azure companion bridge:

1. send to the upstream queue, and
2. receive and settle on the downstream queue.

The design intentionally avoids broader "owner" or "administrator" roles for
normal runtime operation.

### 4.4  Function placeholder identity

The placeholder Function App uses its own system-assigned managed identity. It
is not reused as the Azure companion runtime identity, because the two
processes have different trust boundaries and different steady-state
permissions.

The Function App identity receives the narrow data-plane permissions needed by
the later decoder/control-plane path:

1. receive from the upstream queue,
2. send on the downstream queue, and
3. write decoded records to the reserved Table Storage resource.

The design keeps these grants scoped to the resources actually used by the
Function App rather than assigning broader namespace-wide or account-wide
administrator privileges.

---

## 5  Bootstrap handoff contract

> **Requirements:** AZP-0203, AZP-0300

The Azure provisioning workflow must end with a documented handoff that
bootstrap can use to create the runtime-state files expected by
`sonde-azure-companion`.

### 5.1  Required handoff values

The handoff contract includes:

| Value | Consumer |
|-------|----------|
| Entra tenant ID | `service-principal.json` |
| Entra client ID | `service-principal.json` |
| Certificate reference or exported PEM | certificate PEM material used by the runtime |
| Private-key reference or exported PEM | private-key PEM material used by the runtime |
| Service Bus namespace | Azure companion runtime configuration |
| Upstream queue name | Azure companion runtime configuration |
| Downstream queue name | Azure companion runtime configuration |

### 5.2  Local artifact compatibility

The handoff is complete only when the provisioning workflow's outputs can be
translated into the local runtime artifact shape already defined by the Azure
companion specs:

1. `service-principal.json`,
2. certificate PEM, and
3. private-key PEM.

This document does not require the Bicep deployment itself to write those files
onto the gateway host. It does require the design to define how the deployment
workflow makes the necessary values available to the bootstrap path that does.

---

## 6  Outputs and lifecycle

> **Requirements:** AZP-0100, AZP-0102, AZP-0103, AZP-0104, AZP-0300, AZP-0301

### 6.1  Outputs

The top-level workflow exposes or documents the following outputs:

1. resource group name,
2. Service Bus namespace name,
3. upstream queue name,
4. downstream queue name,
5. Storage Account name,
6. Table resource name,
7. Function placeholder resource identity, and
8. the runtime identity / bootstrap handoff values described in section 5.

### 6.2  Idempotency

Repeated deployment runs converge the stack instead of duplicating foundational
resources. Any intentionally one-time or manually managed identity/certificate
operations must be called out explicitly in the deployment documentation.

### 6.3  Teardown

Teardown removes the resource-plane stack or clearly documents any intentionally
retained artifacts. If some identity artifacts cannot be safely removed by the
same workflow, the teardown documentation must say so plainly rather than
pretending full cleanup occurred.

