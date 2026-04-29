// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

targetScope = 'resourceGroup'

@description('System-assigned managed identity principal ID for the placeholder Function App.')
param functionPrincipalId string

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

var serviceBusDataSenderRoleId = subscriptionResourceId('Microsoft.Authorization/roleDefinitions', '69a216fc-b8fb-44d8-bc22-1f3c2cd27a39')
var serviceBusDataReceiverRoleId = subscriptionResourceId('Microsoft.Authorization/roleDefinitions', '4f6d3b9b-027b-4f4c-9142-0e5a2a2247e0')
var storageTableDataContributorRoleId = subscriptionResourceId('Microsoft.Authorization/roleDefinitions', '0a9a7e1f-b9d0-4cc4-a60d-0319b160aaa3')

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

resource functionUpstreamReceiver 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: guid('function-upstream-receiver', functionPrincipalId, serviceBusDataReceiverRoleId, serviceBusNamespaceName, upstreamQueueName)
  scope: existingUpstreamQueue
  properties: {
    principalId: functionPrincipalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: serviceBusDataReceiverRoleId
  }
}

resource functionDownstreamSender 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: guid('function-downstream-sender', functionPrincipalId, serviceBusDataSenderRoleId, serviceBusNamespaceName, downstreamQueueName)
  scope: existingDownstreamQueue
  properties: {
    principalId: functionPrincipalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: serviceBusDataSenderRoleId
  }
}

resource functionTableContributor 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: guid('function-table-contributor', functionPrincipalId, storageTableDataContributorRoleId, existingStorageTable.id)
  scope: existingStorageTable
  properties: {
    principalId: functionPrincipalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: storageTableDataContributorRoleId
  }
}
