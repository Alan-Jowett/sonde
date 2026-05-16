// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

targetScope = 'resourceGroup'

@description('Azure region for the Azure handler Function App resources.')
param location string

@description('Function App name.')
param functionAppName string

@description('Function hosting plan name.')
param functionPlanName string

@description('Storage Account name used by the Function placeholder deployment configuration.')
param storageAccountName string

@description('Storage Queue service URI for identity-based trigger connections.')
param queueServiceUri string

@description('Queue name for gateway-originated connector traffic.')
param upstreamQueueName string

@description('Queue name for cloud-originated desired-state traffic.')
param downstreamQueueName string

@description('Azure handler actual-state table name.')
param actualStateTableName string

@description('Azure handler desired-state table name.')
param desiredStateTableName string

@description('Azure handler program-route table name.')
param programRouteTableName string

@description('Program storage table name.')
param programsTableName string

@description('Sensor data table name.')
param sensorDataTableName string

@description('Tags applied to provisioned resources.')
param tags object

@description('Application Insights connection string. When non-empty, enables telemetry and backtraces.')
param appInsightsConnectionString string = ''

@description('Allowed CORS origins for the Function App (e.g. the SWA hostname). Empty array disables CORS.')
param corsAllowedOrigins array = []

@description('Entra app (client) ID for EasyAuth token validation on ProgramIngest.')
@minLength(1)
param functionAuthClientId string

@description('Entra tenant ID for EasyAuth OpenID issuer URL.')
@minLength(1)
param functionAuthTenantId string

resource existingStorageAccount 'Microsoft.Storage/storageAccounts@2023-05-01' existing = {
  name: storageAccountName
}

var storageConnectionString = 'DefaultEndpointsProtocol=https;AccountName=${existingStorageAccount.name};EndpointSuffix=${environment().suffixes.storage};AccountKey=${existingStorageAccount.listKeys().keys[0].value}'

resource hostingPlan 'Microsoft.Web/serverfarms@2024-04-01' = {
  name: functionPlanName
  location: location
  kind: 'functionapp'
  tags: tags
  sku: {
    name: 'Y1'
    tier: 'Dynamic'
  }
  properties: {
    reserved: true
  }
}

var baseAppSettings = [
        {
          name: 'AzureWebJobsStorage'
          value: storageConnectionString
        }
        {
          name: 'FUNCTIONS_WORKER_RUNTIME'
          value: 'custom'
        }
        {
          name: 'FUNCTIONS_EXTENSION_VERSION'
          value: '~4'
        }
        {
          name: 'WEBSITE_RUN_FROM_PACKAGE'
          value: '1'
        }
        {
          name: 'SONDE_AZURE_HANDLER_STORAGE_QUEUE_ENDPOINT'
          value: queueServiceUri
        }
        {
          name: 'SONDE_AZURE_HANDLER_UPSTREAM_QUEUE'
          value: upstreamQueueName
        }
        {
          name: 'SONDE_AZURE_HANDLER_DOWNSTREAM_QUEUE'
          value: downstreamQueueName
        }
        {
          name: 'SONDE_AZURE_HANDLER_STORAGE_ACCOUNT'
          value: storageAccountName
        }
        {
          name: 'SONDE_AZURE_HANDLER_ACTUAL_STATE_TABLE'
          value: actualStateTableName
        }
        {
          name: 'SONDE_AZURE_HANDLER_DESIRED_STATE_TABLE'
          value: desiredStateTableName
        }
        {
          name: 'SONDE_AZURE_HANDLER_PROGRAM_ROUTE_TABLE'
          value: programRouteTableName
        }
        {
          name: 'SONDE_AZURE_HANDLER_PROGRAMS_TABLE'
          value: programsTableName
        }
        {
          name: 'SONDE_AZURE_HANDLER_SENSOR_DATA_TABLE'
          value: sensorDataTableName
        }
      ]

var observabilityAppSettings = empty(appInsightsConnectionString) ? [] : [
        {
          name: 'APPLICATIONINSIGHTS_CONNECTION_STRING'
          value: appInsightsConnectionString
        }
        {
          name: 'RUST_BACKTRACE'
          value: '1'
        }
      ]

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
      appSettings: concat(baseAppSettings, observabilityAppSettings)
      cors: empty(corsAllowedOrigins) ? null : {
        allowedOrigins: corsAllowedOrigins
      }
    }
  }
}

resource authSettings 'Microsoft.Web/sites/config@2024-04-01' = {
  parent: functionApp
  name: 'authsettingsV2'
  properties: {
    platform: {
      enabled: true
    }
    globalValidation: {
      unauthenticatedClientAction: 'Return401'
    }
    identityProviders: {
      azureActiveDirectory: {
        enabled: true
        registration: {
          clientId: functionAuthClientId
          openIdIssuer: '${environment().authentication.loginEndpoint}${functionAuthTenantId}/v2.0'
        }
        validation: {
          allowedAudiences: [
            'api://${functionAuthClientId}'
            functionAuthClientId
          ]
          defaultAuthorizationPolicy: {
            allowedApplications: [
              functionAuthClientId
            ]
          }
        }
      }
    }
  }
}

output functionAppName string = functionApp.name
output functionAppResourceId string = functionApp.id
output principalId string = functionApp.identity.principalId
