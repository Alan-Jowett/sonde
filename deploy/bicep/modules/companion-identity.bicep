// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

targetScope = 'subscription'

extension microsoftGraphV1

@description('Project prefix used for the companion app registration display name.')
param projectName string

@description('Stable per-stack suffix used to keep the companion identity unique across deployments.')
param identitySuffix string

@description('Base64-encoded DER certificate public data to register on the Azure companion app registration.')
param certificateBase64 string

@description('Display name for the registered companion certificate credential.')
param certificateDisplayName string = 'sonde-azure-companion'

var projectSlug = toLower(replace(projectName, '-', ''))
var appDisplayName = '${projectName}-azure-companion'
var appUniqueName = '${projectSlug}-azure-companion-${identitySuffix}'

resource companionApp 'Microsoft.Graph/applications@v1.0' = {
  uniqueName: appUniqueName
  displayName: appDisplayName
  signInAudience: 'AzureADMyOrg'
  keyCredentials: [
    {
      displayName: certificateDisplayName
      usage: 'Verify'
      type: 'AsymmetricX509Cert'
      key: certificateBase64
    }
  ]
}

resource companionServicePrincipal 'Microsoft.Graph/servicePrincipals@v1.0' = {
  appId: companionApp.appId
}

output clientId string = companionApp.appId
output tenantId string = tenant().tenantId
output servicePrincipalObjectId string = companionServicePrincipal.id
