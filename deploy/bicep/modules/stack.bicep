// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

targetScope = 'resourceGroup'

@description('Azure region for all deployed resources.')
param location string

@description('Tags applied to provisioned resources.')
param tags object

@description('Storage Account name.')
param storageAccountName string

@description('Azure handler node-state table name.')
param nodeStateTableName string

@description('Azure handler program-route table name.')
param programRouteTableName string

@description('Azure handler Function App name.')
param functionAppName string

@description('Azure handler Function hosting plan name.')
param functionPlanName string

@description('Object ID of the Azure companion runtime service principal.')
param companionServicePrincipalObjectId string

var storageQueueDataContributorRoleId = subscriptionResourceId('Microsoft.Authorization/roleDefinitions', '974c5e8b-45b9-4653-ba55-5f855dd0fb88')
var companionQueueContributorAssignmentName = guid('companion-queue-contributor', companionServicePrincipalObjectId, storageQueueDataContributorRoleId, storageAccountName)
var deploymentStorageContainerName = 'app-package-${take(uniqueString(resourceGroup().id, functionAppName, 'deployment-package'), 20)}'

module storage './storage.bicep' = {
  name: 'storage'
  params: {
    location: location
    storageAccountName: storageAccountName
    nodeStateTableName: nodeStateTableName
    programRouteTableName: programRouteTableName
    upstreamQueueName: upstreamQueueName
    downstreamQueueName: downstreamQueueName
    deploymentContainerName: deploymentStorageContainerName
    tags: tags
  }
}

var functionDeploymentContainerUrl = '${storage.outputs.blobEndpoint}${storage.outputs.deploymentContainerName}'

module functionPlaceholder './function-placeholder.bicep' = {
  name: 'functionPlaceholder'
  params: {
    location: location
    functionAppName: functionAppName
    functionPlanName: functionPlanName
    deploymentContainerUrl: functionDeploymentContainerUrl
    storageAccountName: storage.outputs.storageAccountName
    queueServiceUri: storage.outputs.queueServiceUri
    upstreamQueueName: upstreamQueueName
    downstreamQueueName: downstreamQueueName
    nodeStateTableName: storage.outputs.nodeStateTableName
    programRouteTableName: storage.outputs.programRouteTableName
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
    nodeStateTableName: storage.outputs.nodeStateTableName
    programRouteTableName: storage.outputs.programRouteTableName
  }
  dependsOn: [
    storage
    functionPlaceholder
  ]
}

output storageAccountName string = storage.outputs.storageAccountName
output queueServiceUri string = storage.outputs.queueServiceUri
output upstreamQueueName string = storage.outputs.upstreamQueueName
output downstreamQueueName string = storage.outputs.downstreamQueueName
output deploymentContainerName string = storage.outputs.deploymentContainerName
output deploymentContainerUrl string = functionDeploymentContainerUrl
output nodeStateTableName string = storage.outputs.nodeStateTableName
output programRouteTableName string = storage.outputs.programRouteTableName
output functionAppName string = functionPlaceholder.outputs.functionAppName
output functionPrincipalId string = functionPlaceholder.outputs.principalId
