// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

targetScope = 'resourceGroup'

@description('System-assigned managed identity principal ID for the Azure handler Function App.')
param functionPrincipalId string

@description('Storage Account name.')
param storageAccountName string

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

var storageQueueDataContributorRoleId = subscriptionResourceId('Microsoft.Authorization/roleDefinitions', '974c5e8b-45b9-4653-ba55-5f855dd0fb88')
var storageTableDataContributorRoleId = subscriptionResourceId('Microsoft.Authorization/roleDefinitions', '0a9a7e1f-b9d0-4cc4-a60d-0319b160aaa3')

resource existingStorageAccount 'Microsoft.Storage/storageAccounts@2023-05-01' existing = {
  name: storageAccountName
}

resource existingTableService 'Microsoft.Storage/storageAccounts/tableServices@2023-05-01' existing = {
  parent: existingStorageAccount
  name: 'default'
}

resource existingActualStateTable 'Microsoft.Storage/storageAccounts/tableServices/tables@2023-05-01' existing = {
  parent: existingTableService
  name: actualStateTableName
}

resource existingDesiredStateTable 'Microsoft.Storage/storageAccounts/tableServices/tables@2023-05-01' existing = {
  parent: existingTableService
  name: desiredStateTableName
}

resource existingProgramRouteTable 'Microsoft.Storage/storageAccounts/tableServices/tables@2023-05-01' existing = {
  parent: existingTableService
  name: programRouteTableName
}

resource existingProgramsTable 'Microsoft.Storage/storageAccounts/tableServices/tables@2023-05-01' existing = {
  parent: existingTableService
  name: programsTableName
}

resource existingSensorDataTable 'Microsoft.Storage/storageAccounts/tableServices/tables@2023-05-01' existing = {
  parent: existingTableService
  name: sensorDataTableName
}

resource functionQueueContributor 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: guid('function-queue-contributor', functionPrincipalId, storageQueueDataContributorRoleId, storageAccountName)
  scope: existingStorageAccount
  properties: {
    principalId: functionPrincipalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: storageQueueDataContributorRoleId
  }
}

resource functionActualStateTableContributor 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: guid('function-actual-state-table-contributor', functionPrincipalId, storageTableDataContributorRoleId, existingActualStateTable.id)
  scope: existingActualStateTable
  properties: {
    principalId: functionPrincipalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: storageTableDataContributorRoleId
  }
}

resource functionDesiredStateTableContributor 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: guid('function-desired-state-table-contributor', functionPrincipalId, storageTableDataContributorRoleId, existingDesiredStateTable.id)
  scope: existingDesiredStateTable
  properties: {
    principalId: functionPrincipalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: storageTableDataContributorRoleId
  }
}

resource functionProgramRouteTableContributor 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: guid('function-program-route-table-contributor', functionPrincipalId, storageTableDataContributorRoleId, existingProgramRouteTable.id)
  scope: existingProgramRouteTable
  properties: {
    principalId: functionPrincipalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: storageTableDataContributorRoleId
  }
}

resource functionProgramsTableContributor 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: guid('function-programs-table-contributor', functionPrincipalId, storageTableDataContributorRoleId, existingProgramsTable.id)
  scope: existingProgramsTable
  properties: {
    principalId: functionPrincipalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: storageTableDataContributorRoleId
  }
}

resource functionSensorDataTableContributor 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: guid('function-sensor-data-table-contributor', functionPrincipalId, storageTableDataContributorRoleId, existingSensorDataTable.id)
  scope: existingSensorDataTable
  properties: {
    principalId: functionPrincipalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: storageTableDataContributorRoleId
  }
}
