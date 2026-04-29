// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

targetScope = 'resourceGroup'

@description('Azure region for the Service Bus resources.')
param location string

@description('Service Bus namespace name.')
param namespaceName string

@description('Queue name for gateway-originated connector traffic.')
param upstreamQueueName string

@description('Queue name for cloud-originated desired-state traffic.')
param downstreamQueueName string

@description('Tags applied to provisioned resources.')
param tags object

resource serviceBusNamespace 'Microsoft.ServiceBus/namespaces@2024-01-01' = {
  name: namespaceName
  location: location
  tags: tags
  sku: {
    name: 'Standard'
    tier: 'Standard'
    capacity: 0
  }
  properties: {
    disableLocalAuth: false
    publicNetworkAccess: 'Enabled'
    zoneRedundant: false
  }
}

resource upstreamQueue 'Microsoft.ServiceBus/namespaces/queues@2024-01-01' = {
  parent: serviceBusNamespace
  name: upstreamQueueName
  properties: {
    deadLetteringOnMessageExpiration: true
    enableBatchedOperations: true
    enableExpress: false
    enablePartitioning: false
    maxDeliveryCount: 10
    maxSizeInMegabytes: 1024
    requiresDuplicateDetection: false
    requiresSession: false
    status: 'Active'
  }
}

resource downstreamQueue 'Microsoft.ServiceBus/namespaces/queues@2024-01-01' = {
  parent: serviceBusNamespace
  name: downstreamQueueName
  properties: {
    deadLetteringOnMessageExpiration: true
    enableBatchedOperations: true
    enableExpress: false
    enablePartitioning: false
    maxDeliveryCount: 10
    maxSizeInMegabytes: 1024
    requiresDuplicateDetection: false
    requiresSession: false
    status: 'Active'
  }
}

output namespaceName string = serviceBusNamespace.name
output namespaceFqdn string = '${serviceBusNamespace.name}.servicebus.windows.net'
output namespaceResourceId string = serviceBusNamespace.id
output upstreamQueueName string = upstreamQueue.name
output upstreamQueueResourceId string = upstreamQueue.id
output downstreamQueueName string = downstreamQueue.name
output downstreamQueueResourceId string = downstreamQueue.id
