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
> needed by `sonde-azure-companion`. Logical Azure handler behavior and table
> schema semantics are specified separately in
> [azure-handler-requirements.md](azure-handler-requirements.md).
> **Related:** [azure-provisioning-design.md](azure-provisioning-design.md),
> [azure-provisioning-validation.md](azure-provisioning-validation.md),
> [azure-companion-requirements.md](azure-companion-requirements.md),
> [azure-companion-design.md](azure-companion-design.md),
> [azure-handler-requirements.md](azure-handler-requirements.md)

---

## 1  Definitions

| Term | Definition |
|------|------------|
| **Azure provisioning workflow** | The repository-owned deployment workflow that creates the Azure resources and identity material required by the Azure companion architecture. |
| **Bicep root deployment** | The top-level Bicep entrypoint under `deploy/bicep/` that composes the provisioning modules for this workflow. |
| **Runtime identity bundle** | The Entra tenant/client identity plus certificate-authenticated service-principal material required for the Azure companion runtime after bootstrap completes. |
| **Bootstrap handoff contract** | The defined set of outputs and artifact locations that lets bootstrap materialize `service-principal.json`, certificate PEM, and private-key PEM for `sonde-azure-companion`. |
| **Azure handler Function App** | Azure Function hosting resources used by the Sonde cloud-side handler. This document covers the hosting surface and identity, not the handler's runtime logic. |
| **Storage resources** | The Azure Storage Account and Table resources used by the Azure handler. This document covers only provisioning and RBAC, not the logical table schema. |

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
3. Storage Queue, Storage, and Azure handler Function App resources deployed by this workflow carry the `project = sonde` tag unless the caller overrides the value explicitly.

---

### AZP-0102  Storage Queue endpoint and queues

**Priority:** Must
**Source:** [issue #772](https://github.com/Alan-Jowett/sonde/issues/772), AZC-0302, AZC-0304

**Description:**
The provisioning workflow MUST create the Azure Storage Queue resources required
by the Azure companion runtime: one namespace plus one upstream queue and one
downstream queue. The namespace uses the Standard tier unless the caller
explicitly opts into a different supported tier.

**Acceptance criteria:**

1. The workflow provisions one Storage Queue endpoint.
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
The provisioning workflow MUST create the Azure Storage resources used by the
Azure handler: a Storage Account plus the Table resources required by the
handler's `NodeState` and `ProgramRoute` storage. This document does not define
the tables' logical schema.

**Acceptance criteria:**

1. The workflow provisions one Azure Storage Account for this stack.
2. The workflow provisions the Table resources needed by the Azure handler path, including separate tables for node-state rows and program-route rows.
3. The workflow documents that logical table schema ownership lives in `azure-handler-requirements.md` and is not defined by this provisioning specification.
4. The workflow does not expose raw Storage Account keys in deployment outputs or bootstrap handoff values.

---

### AZP-0104  Azure handler Function App infrastructure

**Priority:** Must
**Source:** [issue #772](https://github.com/Alan-Jowett/sonde/issues/772), reviewed discovery output

**Description:**
The provisioning workflow MUST create the Azure Function hosting resources used
by the Sonde Azure handler and the deployment target that the repository-owned
bootstrap path populates with the runnable handler package. The Function App
uses the Classic Consumption plan (`Y1` / `Dynamic` SKU) so that built-in
filesystem log streaming is available through the Azure Portal without
requiring an Application Insights resource.

**Acceptance criteria:**

1. The workflow provisions the Azure resources needed to host the Azure handler Function App.
2. The workflow provisions or documents the deployment target consumed by the repository-owned handler package deployment step.
3. The Function App resources use the Classic Consumption plan (`Y1` / `Dynamic` SKU).
4. The deployment outputs or documentation identify the Function App resources used by the Azure handler path.
5. The Function App does not require an Application Insights resource for basic log access.

---

### AZP-0105  Repository-owned Azure handler package deployment

**Priority:** Must
**Source:** USER-REQUEST: implement "Function code deployment" in azure function, reviewed discovery output

**Description:**
The repository-owned Azure provisioning/bootstrap workflow MUST deploy a
prebuilt `sonde-azure-handler` package into the provisioned Function App rather
than leaving a manual post-provision upload step to the operator. Successful
bootstrap MUST make the Function App runnable.

**Acceptance criteria:**

1. The deployed package is a prebuilt repository-owned artifact for the Function App's Linux runtime rather than a package built ad hoc during bootstrap.
2. The package contents include the `sonde-azure-handler` executable, `host.json`, and the function metadata required for Azure Functions to load at least one function.
3. The bootstrap workflow deploys that package without requiring the operator to upload a separate handler artifact manually.
4. Bootstrap does not report overall success until Azure reports the package active and at least one function is loaded in the Function App.
5. Re-running bootstrap may replace the deployed package, but it does not require manual cleanup of the prior package first.

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

### AZP-0201  Storage Queue role assignments for bridge directions

**Priority:** Must
**Source:** reviewed discovery output, AZC-0304, AZC-0308

**Description:**
The provisioning workflow MUST assign Storage Queue permissions that match the
Azure companion bridge's bidirectional behavior: upstream publish plus
downstream consume and settlement.

**Acceptance criteria:**

1. The runtime identity can send messages to the configured upstream queue.
2. The runtime identity can receive and settle messages from the configured downstream queue.
3. The documented permissions align with the Azure companion bridge responsibilities and do not rely on unrelated administrator privileges.

---

### AZP-0202  Azure handler Function App managed identity and data-plane RBAC

**Priority:** Must
**Source:** reviewed discovery output

**Description:**
The provisioning workflow MUST attach a system-assigned managed identity to the
Azure handler Function App and grant the data-plane permissions needed for the
Sonde cloud-side handler path: receive from the upstream queue, send on the
downstream queue, and read/write the Azure Table resources used by the handler.
When handler delivery queues are provisioned outside this workflow, the
additional queue-specific send permission for those queues is an external
dependency rather than an implicit responsibility of this Bicep stack.

**Acceptance criteria:**

1. The Azure handler Function App has a system-assigned managed identity.
2. That identity can receive messages from the configured upstream queue.
3. That identity can send messages to the configured downstream queue.
4. That identity can read and write the Azure Table resources used by the handler.
5. The Function App identity is distinct from the Azure companion runtime identity unless a later specification explicitly merges them.
6. The deployment documentation identifies externally provisioned handler queues as requiring separate send permission grants for the Function App identity.

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
2. The handoff contract includes the tenant ID, client ID, certificate reference or material, private-key reference or material, Storage Queue endpoint/queue configuration, and the Function App / deployment-target values needed for package deployment and activation checks.
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
2. Teardown behavior is documented for Storage Queue, Storage, and Function placeholder resources.

---

### AZP-0302  On-demand disposable CI deployment workflow

**Priority:** Must
**Source:** CI validation discovery review, [azure-companion-requirements.md](azure-companion-requirements.md)

**Description:**
The repository MUST provide an on-demand GitHub Actions workflow that exercises
the provisioning workflow against a disposable Azure test stack. This workflow
MUST remain separate from routine pull-request CI and MUST provision, validate,
and tear down the stack within the same manually triggered run.

**Acceptance criteria:**

1. The repository contains a manually triggered GitHub Actions workflow dedicated to live Azure validation.
2. The workflow provisions a disposable test stack using the repository-owned Bicep entrypoint rather than a separate ad hoc deployment path.
3. The workflow runs validation steps against the deployed Azure resources before teardown.
4. The workflow attempts teardown in the same run after validation completes.
5. The workflow is not required for routine pull-request CI on every change.

---

### AZP-0303  Federated CI authentication with repo-configured targeting

**Priority:** Must
**Source:** CI validation discovery review

**Description:**
The live Azure validation workflow MUST authenticate to Azure using GitHub
federated identity (OIDC) rather than a stored Azure client secret. The target
subscription and disposable resource-group name MUST be supplied through
repository or environment configuration so forks can bind the same workflow
logic to their own Azure subscription without editing workflow code.

**Acceptance criteria:**

1. The workflow uses GitHub OIDC/federated identity for Azure login.
2. The workflow does not require a long-lived Azure client secret for CI authentication.
3. The target subscription ID is supplied through repository or environment configuration rather than being hard-coded in the workflow.
4. The disposable CI resource-group name is supplied through repository or environment configuration rather than being hard-coded in the workflow.
5. The documented setup identifies the minimum Azure RBAC needed by the federated CI identity for deployment, validation, and teardown.

---

### AZP-0304  Dedicated CI-owned resource-group cleanup

**Priority:** Must
**Source:** CI validation discovery review

**Description:**
The live Azure validation workflow MUST use a dedicated disposable resource
group that is owned exclusively by CI for this purpose. Before provisioning, the
workflow MUST delete any pre-existing copy of that CI-owned resource group to
remove leftovers from prior failed runs. After validation, the workflow MUST
attempt teardown again even when earlier steps failed.

**Acceptance criteria:**

1. The live Azure validation workflow targets only a dedicated disposable CI-owned resource group for destructive cleanup.
2. The workflow performs preflight deletion of that resource group and does not begin provisioning until the previous group instance is confirmed absent.
3. The workflow performs teardown in a failure-safe post-run path even if provisioning or validation fails.
4. If teardown fails, the workflow surfaces the retained resource-group failure explicitly rather than silently reporting success.
5. The workflow documentation states that arbitrary operator-managed resource groups are out of scope for CI deletion.

---

### AZP-0305  Step-by-step live CI setup guide

**Priority:** Must
**Source:** reviewed discovery output, operator usability review for the live Azure CI workflow

**Description:**
The repository MUST provide a standalone step-by-step setup guide for the live
Azure validation workflow. This guide MUST tell a first-time operator how to
configure the GitHub environment, Azure federated identity, minimum required
permissions, disposable resource-group safety settings, and the first manual
workflow dispatch without reverse-engineering the workflow YAML.

**Acceptance criteria:**

1. The repository contains a standalone operator-facing setup guide at `deploy/bicep/azure-live-ci-setup.md` rather than relying only on a brief prerequisite summary inside `deploy/bicep/README.md`.
2. The setup guide identifies the GitHub environment name and the required repository or environment variables consumed by the workflow.
3. The setup guide describes both an Azure Portal path and an Azure CLI path for the key setup actions where those paths are practical.
4. The setup guide names the exact Azure RBAC roles, Storage Queue data-plane roles, and Microsoft Graph permissions required by the federated CI identity.
5. The setup guide explains the disposable resource-group safety boundary, including the default CI prefix behavior and the requirement that only CI-owned disposable groups are in scope for destructive cleanup.
6. The setup guide describes how to manually dispatch the workflow and what success or cleanup signals an operator should expect from the first run.
7. `deploy/bicep/README.md` links operators to `deploy/bicep/azure-live-ci-setup.md` for the procedural setup path.

