<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Azure provisioning workflow for issue #772

This directory contains the Bicep-based provisioning surface for the current
Azure companion architecture.

## What it provisions

- A dedicated resource group (or a caller-specified existing group target)
- A Standard-tier Azure Service Bus namespace
- Two Service Bus queues:
  - `connector-upstream`
  - `desired-state`
- An Azure Storage Account plus two Azure Table resources for the Azure handler:
  - `nodestate`
  - `programroute`
- An Azure handler Function App on a Flex Consumption plan
- A system-assigned managed identity on the Function App with:
  - receive permissions on the upstream queue
  - send permissions on the downstream queue
  - read/write permissions on the Azure handler tables
- An Entra application / service principal for `sonde-azure-companion` using a
  caller-supplied certificate public credential
- Azure companion Service Bus RBAC:
  - send on the upstream queue
  - receive on the downstream queue

## Inputs

| Parameter | Default | Purpose |
|-----------|---------|---------|
| `location` | `eastus` | Azure region for the stack |
| `project_name` | `sonde` | Prefix for resource names and tags |
| `resource_group_name` | empty | Optional override for the resource group name |
| `resourceGroupOwnerTag` | empty | Optional `sonde-ci-owner` tag value applied to the deployment resource group |
| `companionCertificateBase64` | none | Base64-encoded DER certificate public material registered on the Azure companion app |
| `companionCertificateDisplayName` | `sonde-azure-companion` | Optional display name for the registered certificate credential |
| `serviceBusNamespaceName` | derived | Optional Service Bus namespace override |
| `upstreamQueueName` | `connector-upstream` | Gateway-originated connector traffic queue |
| `downstreamQueueName` | `desired-state` | Desired-state ingress queue |
| `storageAccountName` | derived | Optional Storage Account override |
| `tableName` | empty | Legacy compatibility alias for the node-state table name |
| `nodeStateTableName` | derived | Azure handler node-state table |
| `programRouteTableName` | `programroute` | Azure handler program-route table |
| `functionAppName` | derived | Optional Function App override |
| `functionPlanName` | derived | Optional Function hosting plan override |

When resource names are derived automatically, the deployment normalizes
`project_name` to satisfy Azure naming rules for the target resource types.

For backward compatibility with earlier templates, callers may still pass `tableName`.
When `nodeStateTableName` is omitted, that legacy alias is used as the node-state table name.

## Companion certificate input

The deployment registers only the **public certificate** on the Entra app. The
matching certificate PEM and private-key PEM remain caller-managed local
artifacts for `sonde-azure-companion` bootstrap.

One way to derive `companionCertificateBase64` from a PEM certificate is:

```powershell
openssl x509 -in companion-cert.pem -outform der | openssl base64 -A
```

## Plan / apply

Plan the deployment:

```powershell
$cert = openssl x509 -in companion-cert.pem -outform der | openssl base64 -A
az deployment sub what-if `
  --location eastus `
  --template-file .\deploy\bicep\main.bicep `
  --parameters companionCertificateBase64=$cert
```

Create or update the stack:

```powershell
$cert = openssl x509 -in companion-cert.pem -outform der | openssl base64 -A
az deployment sub create `
  --location eastus `
  --template-file .\deploy\bicep\main.bicep `
  --parameters companionCertificateBase64=$cert
```

## Custom handler package deployment

The Bicep stack provisions the Azure handler Function App shell and the storage-backed
deployment configuration, but it does **not** upload the runnable custom-handler package
by itself. After provisioning, you still need to publish a package containing:

- the `sonde-azure-handler` binary
- `host.json`
- `UpstreamConnector/function.json`

The deployment outputs `deploymentContainerName` and `deploymentContainerUrl` so automation
can discover the blob container that must receive that package.

Until that package is uploaded to the configured deployment container, the Function App
is provisioned but not yet runnable.

## Bootstrap handoff

The deployment outputs the values needed to create the Azure companion runtime
state:

- tenant ID
- client ID
- Service Bus namespace
- upstream queue name
- downstream queue name

You still need to place the matching PEM certificate and private key into the
Azure companion state directory and write `service-principal.json` that points
at those local files.

## Teardown

Delete the resource group that was created for the stack, or target the
documented resource group explicitly:

```powershell
az group delete --name <resource-group-name> --yes --no-wait
```

This removes the Azure resource-plane stack. If you also want to remove the
Entra application and service principal, delete those identity objects
explicitly after teardown. If the Azure handler publishes to pre-provisioned
external handler queues, those queues and their RBAC grants are outside this
stack and must be managed separately.

## Live CI prerequisites

For the full step-by-step setup procedure, see
[`azure-live-ci-setup.md`](azure-live-ci-setup.md).

The repository's on-demand Azure live-validation workflow binds the job to the
GitHub environment `azure-live-ci`, so its target values can come from either
repository variables or variables defined on that environment. A typical setup
provides:

- `SONDE_AZURE_CI_CLIENT_ID`
- `SONDE_AZURE_CI_TENANT_ID`
- `SONDE_AZURE_CI_SUBSCRIPTION_ID`
- `SONDE_AZURE_CI_RESOURCE_GROUP`
- optional `SONDE_AZURE_CI_RESOURCE_GROUP_PREFIX`
- optional `SONDE_AZURE_CI_LOCATION`
- optional `SONDE_AZURE_CI_PROJECT_NAME`

The workflow uses GitHub OIDC for Azure login. The configured identity needs:

- permission to create, inspect, and delete the dedicated disposable CI resource group,
- permission to deploy the Bicep stack in that subscription, and
- Microsoft Graph permissions required by `modules/companion-identity.bicep` to create the Entra application and service principal used by `sonde-azure-companion`, and
- Service Bus data-plane roles that let the live-validation harness send to and receive from the deployed queues when it authenticates via `AzureCliCredential`.

For the default queue topology, the GitHub OIDC identity therefore needs enough
Service Bus queue permissions to:

- receive from `connector-upstream`,
- send to `desired-state`, and
- receive from `desired-state`.

Because the workflow deletes and recreates its disposable resource group, any
role assignment scoped only inside that resource group would be deleted on
teardown. The step-by-step setup guide therefore documents a persistent
assignment strategy for the CI identity rather than a queue-scoped assignment
that would not survive repeated runs.

For destructive cleanup safety, the workflow only deletes a resource group when
both of the following are true:

- the configured resource-group name starts with the configured CI prefix (by
  default `sonde-ci-`, or `${SONDE_AZURE_CI_PROJECT_NAME}-ci-` when the project
  name is overridden), and
- the existing group is tagged `sonde-ci-owner=azure-live-ci`.
