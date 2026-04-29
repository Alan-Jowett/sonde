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

var serviceBusDataSenderRoleId = subscriptionResourceId('Microsoft.Authorization/roleDefinitions', 'e1ecfa86-44a4-4fa6-9e13-a6f4d06e2913')
var serviceBusDataReceiverRoleId = subscriptionResourceId('Microsoft.Authorization/roleDefinitions', 'a638d3c7-ab3a-418d-83e6-5f17a39d4fde')
var storageTableDataContributorRoleId = subscriptionResourceId('Microsoft.Authorization/roleDefinitions', '0a9a7e1f-b00c-4d22-95b9-937d2d07ed72')
var companionUpstreamSenderAssignmentName = guid('companion-upstream-sender', companionServicePrincipalObjectId, serviceBusDataSenderRoleId, serviceBusNamespaceName, upstreamQueueName)
var companionDownstreamReceiverAssignmentName = guid('companion-downstream-receiver', companionServicePrincipalObjectId, serviceBusDataReceiverRoleId, serviceBusNamespaceName, downstreamQueueName)
var functionUpstreamReceiverAssignmentName = guid('function-upstream-receiver', functionAppName, serviceBusDataReceiverRoleId, serviceBusNamespaceName, upstreamQueueName)
var functionDownstreamSenderAssignmentName = guid('function-downstream-sender', functionAppName, serviceBusDataSenderRoleId, serviceBusNamespaceName, downstreamQueueName)
var functionTableContributorAssignmentName = guid('function-table-contributor', functionAppName, storageTableDataContributorRoleId, storageAccountName, tableName)

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
    tags: tags
  }
}

var functionStorageConnectionString = 'DefaultEndpointsProtocol=https;AccountName=${storage.outputs.storageAccountName};EndpointSuffix=${environment().suffixes.storage};AccountKey=${storage.outputs.primaryKey}'

module functionPlaceholder './function-placeholder.bicep' = {
  name: 'functionPlaceholder'
  params: {
    location: location
    functionAppName: functionAppName
    functionPlanName: functionPlanName
    storageConnectionString: functionStorageConnectionString
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

resource existingStorageAccount 'Microsoft.Storage/storageAccounts@2023-05-01' existing = {
  name: storageAccountName
}

resource existingTableService 'Microsoft.Storage/storageAccounts/tableServices@2023-05-01' existing = {
  parent: existingStorageAccount
  name: 'default'
}

resource existingStorageTable 'Microsoft.Storage/storageAccounts/tableServices/tables@2023-05-01' existing = {
  parent: existingTableService
  name: tableName
}

resource companionUpstreamSender 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: companionUpstreamSenderAssignmentName
  scope: existingUpstreamQueue
  properties: {
    principalId: companionServicePrincipalObjectId
    principalType: 'ServicePrincipal'
    roleDefinitionId: serviceBusDataSenderRoleId
  }
}

resource companionDownstreamReceiver 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: companionDownstreamReceiverAssignmentName
  scope: existingDownstreamQueue
  properties: {
    principalId: companionServicePrincipalObjectId
    principalType: 'ServicePrincipal'
    roleDefinitionId: serviceBusDataReceiverRoleId
  }
}

resource functionUpstreamReceiver 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: functionUpstreamReceiverAssignmentName
  scope: existingUpstreamQueue
  properties: {
    principalId: functionPlaceholder.outputs.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: serviceBusDataReceiverRoleId
  }
}

resource functionDownstreamSender 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: functionDownstreamSenderAssignmentName
  scope: existingDownstreamQueue
  properties: {
    principalId: functionPlaceholder.outputs.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: serviceBusDataSenderRoleId
  }
}

resource functionTableContributor 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: functionTableContributorAssignmentName
  scope: existingStorageTable
  properties: {
    principalId: functionPlaceholder.outputs.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: storageTableDataContributorRoleId
  }
}

output serviceBusNamespaceName string = serviceBus.outputs.namespaceName
output upstreamQueueName string = serviceBus.outputs.upstreamQueueName
output downstreamQueueName string = serviceBus.outputs.downstreamQueueName
output storageAccountName string = storage.outputs.storageAccountName
output tableName string = storage.outputs.tableName
output functionAppName string = functionPlaceholder.outputs.functionAppName
output functionPrincipalId string = functionPlaceholder.outputs.principalId
