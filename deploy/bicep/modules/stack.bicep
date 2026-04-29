// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

targetScope = 'resourceGroup'

@description('Azure region for all deployed resources.')
param location string

@description('Tags applied to provisioned resources.')
param tags object

@description('Service Bus namespace name.')
param serviceBusNamespaceName string

@description('Queue name for gateway-originated connector traffic.')
param upstreamQueueName string

@description('Queue name for cloud-originated desired-state traffic.')
param downstreamQueueName string

@description('Storage Account name.')
param storageAccountName string

@description('Storage Table name.')
param tableName string

@description('Placeholder Function App name.')
param functionAppName string

@description('Placeholder Function hosting plan name.')
param functionPlanName string

@description('Object ID of the Azure companion runtime service principal.')
param companionServicePrincipalObjectId string

var serviceBusDataSenderRoleId = subscriptionResourceId('Microsoft.Authorization/roleDefinitions', '69a216fc-b8fb-44d8-bc22-1f3c2cd27a39')
var serviceBusDataReceiverRoleId = subscriptionResourceId('Microsoft.Authorization/roleDefinitions', '4f6d3b9b-027b-4f4c-9142-0e5a2a2247e0')
var companionUpstreamSenderAssignmentName = guid('companion-upstream-sender', companionServicePrincipalObjectId, serviceBusDataSenderRoleId, serviceBusNamespaceName, upstreamQueueName)
var companionDownstreamReceiverAssignmentName = guid('companion-downstream-receiver', companionServicePrincipalObjectId, serviceBusDataReceiverRoleId, serviceBusNamespaceName, downstreamQueueName)
var deploymentStorageContainerName = 'app-package-${take(uniqueString(resourceGroup().id, functionAppName, 'deployment-package'), 20)}'

module serviceBus './service-bus.bicep' = {
  name: 'serviceBus'
  params: {
    location: location
    namespaceName: serviceBusNamespaceName
    upstreamQueueName: upstreamQueueName
    downstreamQueueName: downstreamQueueName
    tags: tags
  }
}

module storage './storage.bicep' = {
  name: 'storage'
  params: {
    location: location
    storageAccountName: storageAccountName
    tableName: tableName
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
    tags: tags
  }
}

resource existingServiceBusNamespace 'Microsoft.ServiceBus/namespaces@2024-01-01' existing = {
  name: serviceBusNamespaceName
}

resource existingUpstreamQueue 'Microsoft.ServiceBus/namespaces/queues@2024-01-01' existing = {
  parent: existingServiceBusNamespace
  name: upstreamQueueName
}

resource existingDownstreamQueue 'Microsoft.ServiceBus/namespaces/queues@2024-01-01' existing = {
  parent: existingServiceBusNamespace
  name: downstreamQueueName
}

resource companionUpstreamSender 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: companionUpstreamSenderAssignmentName
  scope: existingUpstreamQueue
  dependsOn: [
    serviceBus
  ]
  properties: {
    principalId: companionServicePrincipalObjectId
    principalType: 'ServicePrincipal'
    roleDefinitionId: serviceBusDataSenderRoleId
  }
}

resource companionDownstreamReceiver 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: companionDownstreamReceiverAssignmentName
  scope: existingDownstreamQueue
  dependsOn: [
    serviceBus
  ]
  properties: {
    principalId: companionServicePrincipalObjectId
    principalType: 'ServicePrincipal'
    roleDefinitionId: serviceBusDataReceiverRoleId
  }
}

module functionRbac './function-rbac.bicep' = {
  name: 'functionRbac'
  params: {
    functionPrincipalId: functionPlaceholder.outputs.principalId
    serviceBusNamespaceName: serviceBusNamespaceName
    upstreamQueueName: upstreamQueueName
    downstreamQueueName: downstreamQueueName
    storageAccountName: storageAccountName
    tableName: tableName
  }
  dependsOn: [
    serviceBus
    storage
    functionPlaceholder
  ]
}

output serviceBusNamespaceName string = serviceBus.outputs.namespaceName
output upstreamQueueName string = serviceBus.outputs.upstreamQueueName
output downstreamQueueName string = serviceBus.outputs.downstreamQueueName
output storageAccountName string = storage.outputs.storageAccountName
output tableName string = storage.outputs.tableName
output functionAppName string = functionPlaceholder.outputs.functionAppName
output functionPrincipalId string = functionPlaceholder.outputs.principalId
