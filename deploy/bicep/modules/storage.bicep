// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

targetScope = 'resourceGroup'

@description('Azure region for the Storage resources.')
param location string

@description('Storage Account name.')
param storageAccountName string

@description('Table name for Azure handler actual-state rows.')
param actualStateTableName string

@description('Table name for Azure handler desired-state rows.')
param desiredStateTableName string

@description('Table name for Azure handler program-route rows.')
param programRouteTableName string

@description('Table name for program storage.')
param programsTableName string

@description('Blob container name used by the Azure handler Function App deployment slot.')
param deploymentContainerName string

@description('Queue name for gateway-originated connector traffic.')
param upstreamQueueName string

@description('Queue name for cloud-originated desired-state traffic.')
param downstreamQueueName string

@description('Tags applied to provisioned resources.')
param tags object

resource storageAccount 'Microsoft.Storage/storageAccounts@2023-05-01' = {
  name: storageAccountName
  location: location
  tags: tags
  sku: {
    name: 'Standard_LRS'
  }
  kind: 'StorageV2'
  properties: {
    accessTier: 'Hot'
    allowBlobPublicAccess: false
    minimumTlsVersion: 'TLS1_2'
    supportsHttpsTrafficOnly: true
  }
}

resource tableService 'Microsoft.Storage/storageAccounts/tableServices@2023-05-01' = {
  parent: storageAccount
  name: 'default'
  properties: {
    cors: {
      corsRules: [
        {
          allowedOrigins: ['*']
          allowedMethods: ['GET', 'POST', 'PUT', 'DELETE', 'OPTIONS', 'HEAD', 'MERGE']
          allowedHeaders: ['*']
          exposedHeaders: ['x-ms-continuation-NextPartitionKey', 'x-ms-continuation-NextRowKey', 'x-ms-request-id']
          maxAgeInSeconds: 3600
        }
      ]
    }
  }
}

resource blobService 'Microsoft.Storage/storageAccounts/blobServices@2023-05-01' = {
  parent: storageAccount
  name: 'default'
}

resource queueService 'Microsoft.Storage/storageAccounts/queueServices@2023-05-01' = {
  parent: storageAccount
  name: 'default'
}

resource upstreamQueue 'Microsoft.Storage/storageAccounts/queueServices/queues@2023-05-01' = {
  parent: queueService
  name: upstreamQueueName
}

resource downstreamQueue 'Microsoft.Storage/storageAccounts/queueServices/queues@2023-05-01' = {
  parent: queueService
  name: downstreamQueueName
}

resource deploymentContainer 'Microsoft.Storage/storageAccounts/blobServices/containers@2023-05-01' = {
  parent: blobService
  name: deploymentContainerName
  properties: {
    publicAccess: 'None'
  }
}

resource actualStateTable 'Microsoft.Storage/storageAccounts/tableServices/tables@2023-05-01' = {
  parent: tableService
  name: actualStateTableName
  properties: {
    signedIdentifiers: []
  }
}

resource desiredStateTable 'Microsoft.Storage/storageAccounts/tableServices/tables@2023-05-01' = {
  parent: tableService
  name: desiredStateTableName
  properties: {
    signedIdentifiers: []
  }
}

resource programRouteTable 'Microsoft.Storage/storageAccounts/tableServices/tables@2023-05-01' = {
  parent: tableService
  name: programRouteTableName
  properties: {
    signedIdentifiers: []
  }
}

resource programsTable 'Microsoft.Storage/storageAccounts/tableServices/tables@2023-05-01' = {
  parent: tableService
  name: programsTableName
  properties: {
    signedIdentifiers: []
  }
}

output storageAccountName string = storageAccount.name
output storageAccountResourceId string = storageAccount.id
output blobEndpoint string = storageAccount.properties.primaryEndpoints.blob
output deploymentContainerName string = deploymentContainer.name
output deploymentContainerResourceId string = deploymentContainer.id
output actualStateTableName string = actualStateTable.name
output actualStateTableResourceId string = actualStateTable.id
output desiredStateTableName string = desiredStateTable.name
output desiredStateTableResourceId string = desiredStateTable.id
output programRouteTableName string = programRouteTable.name
output programRouteTableResourceId string = programRouteTable.id
output programsTableName string = programsTable.name
output programsTableResourceId string = programsTable.id
output queueServiceUri string = storageAccount.properties.primaryEndpoints.queue
output tableServiceUri string = storageAccount.properties.primaryEndpoints.table
output upstreamQueueName string = upstreamQueue.name
output downstreamQueueName string = downstreamQueue.name
