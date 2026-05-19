<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Azure Live CI setup guide

This guide walks through the first-time setup for the repository's on-demand
Azure live-validation workflow in `.github/workflows/azure-live-ci.yml`.

Use this guide when you want GitHub Actions to:

1. log in to Azure with GitHub OIDC,
2. deploy the disposable Azure companion stack,
3. run the real `sonde-azure-companion` against Azure Storage Queues, and
4. tear the disposable stack down again in the same run.

`deploy/bicep/README.md` remains the reference document for the provisioning
surface. This file is the operator runbook for configuring the CI workflow.

## 1. What you are setting up

The workflow expects:

| Item | Value |
|------|-------|
| GitHub environment | `azure-live-ci` |
| Required variables | `SONDE_AZURE_CI_CLIENT_ID`, `SONDE_AZURE_CI_TENANT_ID`, `SONDE_AZURE_CI_SUBSCRIPTION_ID`, `SONDE_AZURE_CI_RESOURCE_GROUP` |
| Optional variables | `SONDE_AZURE_CI_RESOURCE_GROUP_PREFIX`, `SONDE_AZURE_CI_LOCATION`, `SONDE_AZURE_CI_PROJECT_NAME` |
| Default project name | `sonde` |
| Default CI prefix | `sonde-ci-` |
| Required OIDC audience | `api://AzureADTokenExchange` |
| Required OIDC subject format | `repo:OWNER/REPO:environment:azure-live-ci` |

The workflow only deletes resource groups that:

1. match the configured CI prefix, and
2. carry `sonde-ci-owner=azure-live-ci`.

Do **not** point this workflow at an operator-managed or production resource
group.

## 2. Choose the values you will use

Before touching GitHub or Azure, pick the values below.

| Name | Example | Notes |
|------|---------|-------|
| Repository owner | `Alan-Jowett` | Used in the OIDC subject string. |
| Repository name | `sonde` | Used in the OIDC subject string. |
| GitHub environment | `azure-live-ci` | Fixed by the workflow. |
| Azure subscription ID | `00000000-0000-0000-0000-000000000000` | The disposable CI subscription target. |
| Azure location | `eastus` | Optional; defaults to `eastus`. |
| Project name | `sonde` | Optional; defaults to `sonde`. |
| Disposable resource group | `sonde-ci-eastus` | Must be CI-owned and disposable. |
| Optional CI prefix override | `sonde-ci-` | If omitted, the workflow derives this from the project name. |

## 3. Create the GitHub environment

In GitHub:

1. Open **Settings** for the repository.
2. Open **Environments**.
3. Create an environment named `azure-live-ci`.
4. If you want tighter control, add environment protection rules before the
   workflow is used broadly.

The workflow reads its Azure configuration from repository variables or from
variables defined on this environment.

## 4. Create the Azure OIDC application and service principal

You need one Entra application/service principal that GitHub Actions will use
for `azure/login`.

### Portal path

1. Open **Microsoft Entra ID** in the Azure Portal.
2. Open **App registrations**.
3. Create a new registration for the workflow, for example
   `sonde-azure-live-ci`.
4. Open the registration and note:
   - **Application (client) ID**
   - **Directory (tenant) ID**
5. Open **Enterprise applications** and confirm the matching service principal
   exists.
6. Open **Certificates & secrets** → **Federated credentials**.
7. Add a GitHub federated credential with:
   - **Issuer:** `https://token.actions.githubusercontent.com`
   - **Audience:** `api://AzureADTokenExchange`
   - **Subject:** `repo:OWNER/REPO:environment:azure-live-ci`

### Azure CLI path

```powershell
$app = az ad app create --display-name "sonde-azure-live-ci" | ConvertFrom-Json
$appId = $app.appId

$sp = az ad sp create --id $appId | ConvertFrom-Json

@"
{
  "name": "github-azure-live-ci",
  "issuer": "https://token.actions.githubusercontent.com",
  "subject": "repo:OWNER/REPO:environment:azure-live-ci",
  "audiences": [
    "api://AzureADTokenExchange"
  ]
}
"@ | Set-Content -Path .\federated-credential.json

az ad app federated-credential create `
  --id $appId `
  --parameters @federated-credential.json
```

Replace `OWNER` and `REPO` with the real repository coordinates.

## 5. Grant the Azure permissions the workflow needs

As implemented today, the workflow needs permissions for five distinct jobs:

1. create and delete the disposable resource group,
2. run a subscription-scope Bicep deployment,
3. create RBAC assignments inside that deployment,
4. send to / receive from Storage Queues during live validation, and
5. query Azure Tables to verify handler processing.

### 5.1 Azure RBAC roles

Grant the GitHub OIDC service principal these Azure RBAC roles:

| Role | Recommended scope | Why |
|------|-------------------|-----|
| `Contributor` | Subscription named by `SONDE_AZURE_CI_SUBSCRIPTION_ID` | Needed for `az group create`, `az group delete`, and the subscription-scope deployment. |
| `User Access Administrator` | Same subscription | Needed because the Bicep deployment creates queue/table `roleAssignments`. |
| `Storage Queue Data Contributor` | Subscription | Needed for the live validation harness to send to and receive from the deployed queues. |
| `Storage Table Data Reader` | Subscription | Needed for the handler validation step to query the actual-state table after processing. |

**Why subscription scope for the Storage Queue/Table data roles?** The workflow deletes
and recreates the disposable resource group every run. Narrower assignments
inside that resource group would be destroyed on teardown, so the current
workflow needs persistent assignments above the disposable stack scope.

### Portal path

For each role above:

1. Open the target **Subscription**.
2. Open **Access control (IAM)**.
3. Add a role assignment.
4. Select the role.
5. Assign it to the GitHub OIDC service principal you created in step 4.

### Azure CLI path

```powershell
$subscriptionId = "00000000-0000-0000-0000-000000000000"
$principalObjectId = "<service-principal-object-id>"
$subscriptionScope = "/subscriptions/$subscriptionId"

foreach ($role in @(
  "Contributor",
  "User Access Administrator",
  "Storage Queue Data Contributor",
  "Storage Table Data Reader"
)) {
  az role assignment create `
    --assignee-object-id $principalObjectId `
    --assignee-principal-type ServicePrincipal `
    --role $role `
    --scope $subscriptionScope
}
```

## 6. Grant the Microsoft Graph permissions for Entra app management

The Azure Live CI workflow creates and configures an Entra app registration
and service principal via CLI (`az rest`, `az ad sp create`) before deploying
the Bicep stack.

For the app-only identity used by GitHub OIDC, grant
`Application.ReadWrite.OwnedBy` as the least-privileged application permission.

### Portal path

1. Open the GitHub OIDC app registration from step 4.
2. Open **API permissions**.
3. Add a permission for **Microsoft Graph**.
4. Choose **Application permissions**.
5. Add `Application.ReadWrite.OwnedBy`.
6. Grant **admin consent** for the tenant.

If your tenant policy or ownership model prevents that least-privileged grant
from working, grant `Application.ReadWrite.All` instead.

### CLI note

This guide intentionally treats the Graph-permission grant as a Portal-first
step. The exact automation command depends on the Graph tooling you standardize
on for admin-consent workflows, while the required permission name is stable:
`Application.ReadWrite.OwnedBy`.

## 7. Create the GitHub variables

Set these variables either:

1. on the repository, or
2. on the `azure-live-ci` environment.

Required:

| Variable | Example |
|----------|---------|
| `SONDE_AZURE_CI_CLIENT_ID` | app/client ID from step 4 |
| `SONDE_AZURE_CI_TENANT_ID` | tenant ID from step 4 |
| `SONDE_AZURE_CI_SUBSCRIPTION_ID` | subscription ID chosen in step 2 |
| `SONDE_AZURE_CI_RESOURCE_GROUP` | `sonde-ci-eastus` |

Optional:

| Variable | Default | Notes |
|----------|---------|-------|
| `SONDE_AZURE_CI_RESOURCE_GROUP_PREFIX` | `${SONDE_AZURE_CI_PROJECT_NAME}-ci-`, or `sonde-ci-` by default | Safety boundary for destructive cleanup. |
| `SONDE_AZURE_CI_LOCATION` | `eastus` | Deployment region. |
| `SONDE_AZURE_CI_PROJECT_NAME` | `sonde` | Drives naming and default CI prefix. |

Set `SONDE_AZURE_CI_RESOURCE_GROUP_PREFIX` if you want a stricter allow-list
than the default derived prefix.

## 8. Sanity-check the disposable resource-group safety boundary

Before running the workflow:

1. Confirm `SONDE_AZURE_CI_RESOURCE_GROUP` is disposable.
2. Confirm its name starts with the configured CI prefix.
3. Confirm no operator-managed or production resource group shares that name.

During the workflow, the resource group is created or updated with:

```text
sonde-ci-owner=azure-live-ci
```

The workflow refuses to delete a resource group that does not carry that tag.

## 9. Run the workflow for the first time

In GitHub:

1. Open **Actions**.
2. Select **Azure Live CI**.
3. Choose **Run workflow**.
4. Dispatch it from the branch that contains the current workflow definition.

## 10. What success looks like

On a healthy first run:

1. **Validate required Azure CI variables** passes.
2. **Azure login (OIDC)** passes.
3. **Preflight-delete disposable resource group** either removes an old CI-owned
   resource group or confirms none exists.
4. **Generate runtime certificate and deploy disposable stack** succeeds.
5. **Run live Azure companion validation** prints:
   - `connector harness connected`
   - `success path passed`
   - `failure path passed`
6. **Teardown disposable resource group** succeeds and the resource group no
   longer exists in Azure.

On failure:

1. the workflow still attempts teardown, and
2. GitHub uploads `azure-live-ci-artifacts` containing deployment/runtime-state
   debug files (but not the private key).

## 11. Common misconfigurations

| Symptom | Likely cause |
|--------|--------------|
| `SONDE_AZURE_CI_* must be configured` | Missing repository/environment variables. |
| Azure login fails | Client ID, tenant ID, subscription ID, or federated credential subject is wrong. |
| Graph deployment fails while creating the companion identity | The OIDC app is missing `Application.ReadWrite.OwnedBy` or tenant admin consent. |
| Bicep deployment fails on role assignments | The OIDC identity is missing `User Access Administrator`. |
| Live validation fails with Storage Queue authorization errors | The OIDC identity is missing `Storage Queue Data Contributor`. |
| Cleanup refuses to delete the resource group | The group name is outside the configured CI prefix or the CI-ownership tag is missing. |
