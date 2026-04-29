// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

targetScope = 'resourceGroup'

@description('Azure region for the Function placeholder resources.')
param location string

@description('Function App name.')
param functionAppName string

@description('Function hosting plan name.')
param functionPlanName string

@description('Blob container URL used by the Function placeholder deployment configuration.')
param deploymentContainerUrl string

@secure()
@description('Connection string used by the Function placeholder deployment storage configuration.')
param storageConnectionString string

@description('Tags applied to provisioned resources.')
param tags object

resource hostingPlan 'Microsoft.Web/serverfarms@2024-04-01' = {
  name: functionPlanName
  location: location
  kind: 'functionapp'
  tags: tags
  sku: {
    name: 'FC1'
    tier: 'FlexConsumption'
  }
  properties: {
    reserved: true
  }
}

resource functionApp 'Microsoft.Web/sites@2024-04-01' = {
  name: functionAppName
  location: location
  kind: 'functionapp,linux'
  tags: tags
  identity: {
    type: 'SystemAssigned'
  }
  properties: {
    httpsOnly: true
    serverFarmId: hostingPlan.id
    siteConfig: {
      minTlsVersion: '1.2'
    }
    functionAppConfig: {
      deployment: {
        storage: {
          type: 'blobContainer'
          value: deploymentContainerUrl
          authentication: {
            type: 'StorageAccountConnectionString'
            storageAccountConnectionStringName: 'DEPLOYMENT_STORAGE_CONNECTION_STRING'
          }
        }
      }
      runtime: {
        name: 'custom'
        version: '1.0'
      }
      scaleAndConcurrency: {
        maximumInstanceCount: 100
        instanceMemoryMB: 512
      }
    }
  }
}

resource appSettings 'Microsoft.Web/sites/config@2024-04-01' = {
  parent: functionApp
  name: 'appsettings'
  properties: {
    DEPLOYMENT_STORAGE_CONNECTION_STRING: storageConnectionString
  }
}

output functionAppName string = functionApp.name
output functionAppResourceId string = functionApp.id
output principalId string = functionApp.identity.principalId
