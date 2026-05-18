// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

targetScope = 'resourceGroup'

@description('Azure region for all deployed resources.')
param location string

@description('Tags applied to provisioned resources.')
param tags object

@description('Storage Account name.')
param storageAccountName string

@description('Queue name for gateway-originated connector traffic.')
param upstreamQueueName string

@description('Queue name for cloud-originated desired-state traffic.')
param downstreamQueueName string

@description('Azure handler actual-state table name.')
param actualStateTableName string

@description('Azure handler desired-state table name.')
param desiredStateTableName string

@description('Program storage table name.')
param programsTableName string

@description('Sensor data table name.')
param sensorDataTableName string

@description('Azure handler Function App name.')
param functionAppName string

@description('Azure handler Function hosting plan name.')
param functionPlanName string

@description('Object ID of the Azure companion runtime service principal.')
param companionServicePrincipalObjectId string

@description('Entra app (client) ID for Function App EasyAuth token validation.')
param functionAuthClientId string

@description('Entra tenant ID for Function App EasyAuth OpenID issuer URL.')
param functionAuthTenantId string

@description('CORS allowed origins for the Function App (e.g. GitHub Pages URL, custom domain). At least one origin is required.')
param corsAllowedOrigins array

var monitoringWorkspaceName = take('${functionAppName}-logs', 63)
var monitoringAppInsightsName = take('${functionAppName}-insights', 260)

var storageQueueDataContributorRoleId = subscriptionResourceId('Microsoft.Authorization/roleDefinitions', '974c5e8b-45b9-4653-ba55-5f855dd0fb88')
var companionQueueContributorAssignmentName = guid('companion-queue-contributor', companionServicePrincipalObjectId, storageQueueDataContributorRoleId, storageAccountName)
var deploymentStorageContainerName = 'app-package-${take(uniqueString(resourceGroup().id, functionAppName, 'deployment-package'), 20)}'

module storage './storage.bicep' = {
  name: 'storage'
  params: {
    location: location
    storageAccountName: storageAccountName
    actualStateTableName: actualStateTableName
    desiredStateTableName: desiredStateTableName
    programsTableName: programsTableName
    sensorDataTableName: sensorDataTableName
    upstreamQueueName: upstreamQueueName
    downstreamQueueName: downstreamQueueName
    deploymentContainerName: deploymentStorageContainerName
    tags: tags
  }
}

module monitoring './monitoring.bicep' = {
  name: 'monitoring'
  params: {
    location: location
    workspaceName: monitoringWorkspaceName
    appInsightsName: monitoringAppInsightsName
    tags: tags
  }
}

module functionPlaceholder './function-placeholder.bicep' = {
  name: 'functionPlaceholder'
  params: {
    location: location
    functionAppName: functionAppName
    functionPlanName: functionPlanName
    storageAccountName: storage.outputs.storageAccountName
    queueServiceUri: storage.outputs.queueServiceUri
    upstreamQueueName: upstreamQueueName
    downstreamQueueName: downstreamQueueName
    actualStateTableName: storage.outputs.actualStateTableName
    desiredStateTableName: storage.outputs.desiredStateTableName
    programsTableName: storage.outputs.programsTableName
    sensorDataTableName: storage.outputs.sensorDataTableName
    appInsightsConnectionString: monitoring.outputs.connectionString
    corsAllowedOrigins: corsAllowedOrigins
    functionAuthClientId: functionAuthClientId
    functionAuthTenantId: functionAuthTenantId
    tags: tags
  }
}

resource existingStorageAccount 'Microsoft.Storage/storageAccounts@2023-05-01' existing = {
  name: storageAccountName
}

resource companionQueueContributor 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: companionQueueContributorAssignmentName
  scope: existingStorageAccount
  dependsOn: [
    storage
  ]
  properties: {
    principalId: companionServicePrincipalObjectId
    principalType: 'ServicePrincipal'
    roleDefinitionId: storageQueueDataContributorRoleId
  }
}

module functionRbac './function-rbac.bicep' = {
  name: 'functionRbac'
  params: {
    functionPrincipalId: functionPlaceholder.outputs.principalId
    storageAccountName: storageAccountName
    actualStateTableName: storage.outputs.actualStateTableName
    desiredStateTableName: storage.outputs.desiredStateTableName
    programsTableName: storage.outputs.programsTableName
    sensorDataTableName: storage.outputs.sensorDataTableName
  }
}

output storageAccountName string = storage.outputs.storageAccountName
output queueServiceUri string = storage.outputs.queueServiceUri
output tableServiceUri string = storage.outputs.tableServiceUri
output upstreamQueueName string = storage.outputs.upstreamQueueName
output downstreamQueueName string = storage.outputs.downstreamQueueName
output deploymentContainerName string = storage.outputs.deploymentContainerName
output deploymentContainerUrl string = '${storage.outputs.blobEndpoint}${storage.outputs.deploymentContainerName}'
output actualStateTableName string = storage.outputs.actualStateTableName
output desiredStateTableName string = storage.outputs.desiredStateTableName
output programsTableName string = storage.outputs.programsTableName
output sensorDataTableName string = storage.outputs.sensorDataTableName
output functionAppName string = functionPlaceholder.outputs.functionAppName
output functionPrincipalId string = functionPlaceholder.outputs.principalId
