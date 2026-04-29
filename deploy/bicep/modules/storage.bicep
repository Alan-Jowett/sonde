// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

targetScope = 'resourceGroup'

@description('Azure region for the Storage resources.')
param location string

@description('Storage Account name.')
param storageAccountName string

@description('Table name reserved for decoded data.')
param tableName string

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
}

resource storageTable 'Microsoft.Storage/storageAccounts/tableServices/tables@2023-05-01' = {
  parent: tableService
  name: tableName
  properties: {
    signedIdentifiers: []
  }
}

@secure()
output primaryKey string = storageAccount.listKeys().keys[0].value
output storageAccountName string = storageAccount.name
output storageAccountResourceId string = storageAccount.id
output tableName string = storageTable.name
output tableResourceId string = storageTable.id
