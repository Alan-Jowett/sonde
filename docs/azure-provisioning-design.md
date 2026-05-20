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

1. **Resource-plane provisioning** via Bicep for Storage Queue, Storage, and
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
| `deploy/bicep/modules/storage.bicep` | Storage Account, tables, queues, and deployment container. |
| `deploy/bicep/modules/function-placeholder.bicep` | Function hosting resources and deployment target for the Azure handler package. |
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

### 3.3  Storage Queue resources

The Storage Queue resources are provisioned within the Storage Account:

1. a queue service on the Storage Account,
2. one upstream queue for gateway-originated connector traffic, and
3. one downstream queue for desired-state ingress.

The design keeps the queue names and the queue service endpoint URI as explicit
deployment outputs so bootstrap and runtime configuration can consume them
directly rather than relying on embedded defaults inside `sonde-azure-companion`.

### 3.4  Storage resources

The storage module provisions:

1. one Storage Account, and
2. the Table resources used by the Azure handler, including separate tables for
   `NodeState` and `ProgramRoute`.

This module intentionally stops at resource creation and RBAC-ready wiring. It
does not define the tables' logical schema, retention semantics, or row
contracts; those belong to the Azure handler specification that owns cloud-side
reconciliation behavior. When the Azure handler Function App needs storage
credentials for deployment wiring, the design keeps that secret handling inside
the consuming module rather than surfacing raw account keys as deployment
outputs.

### 3.5  Azure handler Function App resources

The function-placeholder module provisions the hosting resources used by the
Azure handler Function App. This module creates the Function App shell, the
Classic Consumption hosting plan (`Y1` / `Dynamic` SKU), storage linkage, and
baseline app settings required for the repository-owned bootstrap path to
deploy the runnable handler package.

The Classic Consumption plan is chosen over the Flex Consumption plan so that
built-in filesystem log streaming is available through the Azure Portal
without requiring a separate Application Insights resource. The Function App
uses `WEBSITE_RUN_FROM_PACKAGE=1` so that the Azure Functions host runs the
handler package deployed via the zip deploy API. The `AzureWebJobsStorage`
connection string provides the runtime storage backend, and
`FUNCTIONS_WORKER_RUNTIME=custom` tells the Azure Functions host to use the
Sonde custom handler binary. The bootstrap script explicitly clears
`linuxFxVersion` via `az functionapp config set` after Bicep provisioning
and before zip deployment, which suppresses the Azure CLI runtime-detection
warning that otherwise appears during `config-zip`.

The runnable package itself is not compiled during bootstrap. Instead, the
bootstrap image carries a prebuilt Linux package for the matching Sonde
release, uploads it into the provisioned deployment target, and waits until
Azure reports that at least one function is loaded before reporting success.

The function-placeholder module also configures Azure App Service
Authentication (EasyAuth / `authSettingsV2`) on the Function App. This
validates Entra ID bearer tokens on HTTP-triggered routes (e.g.,
`ProgramIngest`) at the platform level, eliminating the need for Azure
Functions API keys. The EasyAuth configuration requires two parameters:
the companion Entra app client ID (used as the AAD audience) and the
tenant ID (used to construct the OpenID issuer URL). Queue-triggered
invocations are unaffected because they bypass HTTP auth entirely.

The parameter flow is:
- `main.bicep` accepts `companionClientId` and `companionTenantId` as
  top-level parameters (populated from the Entra app registration).
- `main.bicep` passes these to `stack.bicep` as module parameters.
- `stack.bicep` passes them to `function-placeholder.bicep` as
  `functionAuthClientId` and `functionAuthTenantId`.
- `function-placeholder.bicep` uses them together with the Bicep
  `environment().authentication.loginEndpoint` function to construct the
  `authSettingsV2` resource with the correct issuer URL and audience,
  ensuring compatibility with sovereign clouds.

The `companionBootstrapValues` Bicep output also includes a `loginEndpoint`
field populated from `environment().authentication.loginEndpoint`. The Azure
companion bootstrap persists this value in `service-principal.json` so the
runtime can construct the correct OAuth token endpoint for the target cloud
without hardcoding a public-cloud URL.

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
4. Storage Queue permissions aligned with the bridge's upstream send and
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

The identity step must assign only the Storage Queue permissions required by the
Azure companion bridge:

1. send to the upstream queue, and
2. receive and settle on the downstream queue.

The design intentionally avoids broader "owner" or "administrator" roles for
normal runtime operation.

### 4.4  Azure handler Function App identity

The Azure handler Function App uses its own system-assigned managed identity. It
is not reused as the Azure companion runtime identity, because the two
processes have different trust boundaries and different steady-state
permissions.

The Function App identity receives the narrow data-plane permissions needed by
the Azure handler path:

1. receive from the upstream queue,
2. send on the downstream queue, and
3. read and write the Azure Table resources used by the handler.

The design keeps these grants scoped to the resources actually used by the
Function App rather than assigning broader namespace-wide or account-wide
administrator privileges.

When the handler delivers `GW-0813` messages to pre-provisioned external queues,
the Function App identity also needs send permission on those queues. Because
those queues may be provisioned outside the Sonde Bicep stack, that grant is an
external deployment dependency documented alongside the route-table contract
rather than an automatically created in-stack role assignment.

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
| Login endpoint | `service-principal.json` — authority host URL for the target cloud (e.g., `https://login.microsoftonline.com`), used to construct OAuth token endpoints |
| Certificate reference or exported PEM | certificate PEM material used by the runtime |
| Private-key reference or exported PEM | private-key PEM material used by the runtime |
| Storage Queue endpoint | Azure companion runtime configuration |
| Upstream queue name | Azure companion runtime configuration |
| Downstream queue name | Azure companion runtime configuration |
| Function App name | bootstrap package activation checks |
| Deployment container name / URL | bootstrap package upload target |

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

> **Requirements:** AZP-0100, AZP-0102, AZP-0103, AZP-0104, AZP-0300, AZP-0301, AZP-0302, AZP-0303, AZP-0304, AZP-0305

### 6.1  Outputs

The top-level workflow exposes or documents the following outputs:

1. resource group name,
2. Storage Queue endpoint URI,
3. upstream queue name,
4. downstream queue name,
5. Storage Account name,
6. deployment container name / URL,
7. Function App name and identity, and
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

### 6.4  Live Azure CI workflow boundary

The repository also exposes a manually triggered GitHub Actions workflow for
live Azure validation. This workflow is intentionally separate from routine PR
CI so ordinary code-review runs do not require Azure credentials or incur cloud
cost.

That workflow performs one end-to-end disposable validation cycle:

1. log in to Azure with GitHub OIDC,
2. preflight-delete the dedicated CI-owned resource group and wait until Azure reports it absent,
3. deploy the Bicep stack into that clean resource group,
4. run live validation against the deployed resources, and
5. attempt teardown again in an `always()`-style cleanup step.

### 6.5  CI configuration model

The workflow logic is repository-owned, but its Azure target is repo-specific.
To keep the workflow fork-portable, subscription ID, tenant/client identifiers,
the dedicated CI resource-group name, and similar deployment defaults are read
from repository or environment configuration rather than being embedded in the
workflow file itself.

The design assumes GitHub Actions OIDC federation into an Azure application or
service principal that has only the RBAC required to:

1. create/update the disposable stack,
2. inspect deployed resources for validation, and
3. delete the CI-owned resource group afterward.

### 6.6  Disposable resource-group safety rule

Because the workflow performs destructive preflight cleanup, it must never point
at an arbitrary shared resource group. The configured resource group for this
workflow is therefore part of the safety boundary: it is a dedicated,
CI-owned disposable group reserved exclusively for this live validation path.

### 6.7  Live CI setup guide boundary

The repository separates the operator-facing provisioning reference from the
operator-facing live-CI setup runbook.

1. `deploy/bicep/README.md` remains the reference surface for the Bicep
   deployment inputs, outputs, and high-level live-CI prerequisites.
2. `deploy/bicep/azure-live-ci-setup.md` carries the
   step-by-step procedure for configuring and running the GitHub Actions
   workflow.

The setup guide is part of the repository-owned deployment surface, not an
external wiki-only procedure, so operators can configure the workflow from the
same revision they intend to run.
`deploy/bicep/README.md` links to this guide from its live-CI prerequisite
section so the procedural path is discoverable from the provisioning reference.

### 6.8  Setup guide content contract

The live-CI setup guide must present the setup flow in operator order:

1. identify the GitHub environment name and the variables the workflow consumes,
2. configure GitHub OIDC federation for the Azure identity,
3. assign the minimum Azure RBAC, Storage Queue data-plane roles, and Microsoft
   Graph permissions required by the workflow,
4. choose and document a safe disposable resource-group name and optional CI
   prefix override,
5. manually dispatch the workflow, and
6. verify both the success path and the teardown path of the first run.

For the key setup actions above, the guide provides both:

1. an Azure Portal path for operators performing the setup interactively, and
2. an Azure CLI path for operators automating or scripting the setup.

The guide may reference the workflow file for exact variable names, but it must
not force an operator to infer the required permissions or safety boundary by
reading the YAML directly.

