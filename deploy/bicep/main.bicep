// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

targetScope = 'subscription'

@description('Azure region for all deployed resources.')
param location string = 'eastus'

@description('Prefix used for resource names and default tags.')
param project_name string = 'sonde'

@description('Optional override for the resource group name. Leave empty to derive one from project_name.')
param resource_group_name string = ''

@description('Entra application (client) ID for the Azure companion. Created via CLI before deployment because the Microsoft Graph Bicep extension does not reliably return server-generated read-only properties on first creation (microsoftgraph/msgraph-bicep-types#193).')
param companionClientId string

@description('Entra service principal object ID for the Azure companion.')
param companionServicePrincipalObjectId string

@description('Optional ownership tag value applied to the deployment resource group. Leave empty for non-CI deployments.')
param resourceGroupOwnerTag string = ''

@description('Queue name for gateway-originated connector traffic.')
param upstreamQueueName string = 'connector-upstream'

@description('Queue name for cloud-originated desired-state traffic.')
param downstreamQueueName string = 'desired-state'

@description('Optional override for the Storage Account name.')
param storageAccountName string = ''

@description('Table name for Azure handler actual-state rows.')
param actualStateTableName string = ''

@description('Table name for Azure handler desired-state rows.')
param desiredStateTableName string = ''

@description('Table name for program storage.')
param programsTableName string = ''

@description('Table name for sensor data rows.')
param sensorDataTableName string = ''

@description('Table name for gateway escrow metadata.')
param escrowTableName string = ''

@description('Optional override for the Azure handler Function App name.')
param functionAppName string = ''

@description('Optional override for the Azure handler Function hosting plan name.')
param functionPlanName string = ''

@description('GitHub Pages origin URL for the Web UI (used for Function App CORS). Defaults to the well-known GitHub Pages URL for the sonde repository.')
param githubPagesOrigin string = 'https://alan-jowett.github.io'

@description('Custom domain origin for the Web UI (used for Function App CORS). Empty = GitHub Pages origin only.')
param customDomainOrigin string = 'https://sondeplatform.com'

var projectSlug = toLower(replace(replace(replace(replace(replace(project_name, '-', ''), '_', ''), ' ', ''), '.', ''), '/', ''))
var effectiveProjectSlug = empty(projectSlug) ? 'sonde' : projectSlug
var effectiveResourceGroupName = empty(resource_group_name) ? '${take(effectiveProjectSlug, 84)}-azure' : resource_group_name
var effectiveStorageAccountName = empty(storageAccountName)
  ? take('st${take(uniqueString(subscription().subscriptionId, project_name, effectiveResourceGroupName, 'storage'), 22)}', 24)
  : storageAccountName
var effectiveActualStateTableName = empty(actualStateTableName)
  ? 'actualstate'
  : actualStateTableName
var effectiveDesiredStateTableName = empty(desiredStateTableName)
  ? 'desiredstate'
  : desiredStateTableName
var effectiveProgramsTableName = empty(programsTableName) ? 'programs' : programsTableName
var effectiveSensorDataTableName = empty(sensorDataTableName) ? 'sensordata' : sensorDataTableName
var effectiveEscrowTableName = empty(escrowTableName) ? 'gatewayescrow' : escrowTableName
// Keep the historical `-decoder-` stem as the derived default to avoid replacing
// existing Function App resources during in-place redeploys. New deployments can
// still override `functionAppName` if they want a handler-specific resource name.
var effectiveFunctionAppName = empty(functionAppName)
  ? take('${take(effectiveProjectSlug, 24)}-decoder-${take(uniqueString(subscription().subscriptionId, effectiveResourceGroupName, 'func'), 8)}', 60)
  : functionAppName
var effectiveFunctionPlanName = empty(functionPlanName)
  ? take('${take(effectiveProjectSlug, 24)}-func-plan', 40)
  : functionPlanName
var corsOrigins = empty(customDomainOrigin)
  ? [githubPagesOrigin]
  : [githubPagesOrigin, customDomainOrigin]
var tags = {
  project: project_name
}
var resourceGroupTags = empty(resourceGroupOwnerTag)
  ? tags
  : union(tags, {
      'sonde-ci-owner': resourceGroupOwnerTag
    })

resource stackResourceGroup 'Microsoft.Resources/resourceGroups@2024-03-01' = {
  name: effectiveResourceGroupName
  location: location
  tags: resourceGroupTags
}

module stack './modules/stack.bicep' = {
  name: 'azureCompanionStack'
  scope: stackResourceGroup
  params: {
    location: location
    tags: tags
    storageAccountName: effectiveStorageAccountName
    upstreamQueueName: upstreamQueueName
    downstreamQueueName: downstreamQueueName
    actualStateTableName: effectiveActualStateTableName
    desiredStateTableName: effectiveDesiredStateTableName
    programsTableName: effectiveProgramsTableName
    sensorDataTableName: effectiveSensorDataTableName
    escrowTableName: effectiveEscrowTableName
    functionAppName: effectiveFunctionAppName
    functionPlanName: effectiveFunctionPlanName
    companionServicePrincipalObjectId: companionServicePrincipalObjectId
    functionAuthClientId: companionClientId
    functionAuthTenantId: tenant().tenantId
    corsAllowedOrigins: corsOrigins
  }
}

output resourceGroupName string = stackResourceGroup.name
output storageAccountName string = stack.outputs.storageAccountName
output queueServiceUri string = stack.outputs.queueServiceUri
output tableServiceUri string = stack.outputs.tableServiceUri
output upstreamQueueName string = stack.outputs.upstreamQueueName
output downstreamQueueName string = stack.outputs.downstreamQueueName
output actualStateTableName string = stack.outputs.actualStateTableName
output desiredStateTableName string = stack.outputs.desiredStateTableName
output deploymentContainerName string = stack.outputs.deploymentContainerName
output deploymentContainerUrl string = stack.outputs.deploymentContainerUrl
output programsTableName string = stack.outputs.programsTableName
output sensorDataTableName string = stack.outputs.sensorDataTableName
output functionAppName string = stack.outputs.functionAppName
output functionPrincipalId string = stack.outputs.functionPrincipalId
output companionClientId string = companionClientId
output companionTenantId string = tenant().tenantId
output companionServicePrincipalObjectId string = companionServicePrincipalObjectId
output companionBootstrapValues object = {
  tenantId: tenant().tenantId
  clientId: companionClientId
  loginEndpoint: environment().authentication.loginEndpoint
  storageQueueEndpoint: stack.outputs.queueServiceUri
  upstreamQueue: stack.outputs.upstreamQueueName
  downstreamQueue: stack.outputs.downstreamQueueName
  functionAppName: stack.outputs.functionAppName
  deploymentContainerName: stack.outputs.deploymentContainerName
  deploymentContainerUrl: stack.outputs.deploymentContainerUrl
  actualStateTable: stack.outputs.actualStateTableName
  desiredStateTable: stack.outputs.desiredStateTableName
  note: 'The Entra app registration and service principal are created before this deployment via CLI. The matching PEM certificate and private key remain caller-managed local artifacts for sonde-azure-companion bootstrap.'
}
