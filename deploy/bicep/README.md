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
- An Azure Storage Account and Table resource for later decoded-data storage
- A placeholder Azure Function App on a consumption plan
- A system-assigned managed identity on the Function App with:
  - receive permissions on the upstream queue
  - send permissions on the downstream queue
  - write permissions on the Storage Table
- An Entra application / service principal for `sonde-azure-companion` using a
  caller-supplied certificate public credential
- Azure companion Service Bus RBAC:
  - send on the upstream queue
  - receive on the downstream queue

## Inputs

| Parameter | Default | Purpose |
|-----------|---------|---------|
| `location` | `eastus` | Azure region for the stack |
| `projectName` | `sonde` | Prefix for resource names and tags |
| `resourceGroupName` | empty | Optional override for the resource group name |
| `companionCertificateBase64` | none | Base64-encoded DER certificate public material registered on the Azure companion app |
| `serviceBusNamespaceName` | derived | Optional Service Bus namespace override |
| `upstreamQueueName` | `connector-upstream` | Gateway-originated connector traffic queue |
| `downstreamQueueName` | `desired-state` | Desired-state ingress queue |
| `storageAccountName` | derived | Optional Storage Account override |
| `tableName` | `decodeddata` | Placeholder decoded-data table resource |
| `functionAppName` | derived | Optional Function App override |
| `functionPlanName` | derived | Optional Function hosting plan override |

When resource names are derived automatically, the deployment normalizes
`projectName` to satisfy Azure naming rules for the target resource types.

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
explicitly after teardown.
