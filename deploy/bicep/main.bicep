// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

targetScope = 'subscription'

@description('Azure region for all deployed resources.')
param location string = 'eastus'

@description('Prefix used for resource names and default tags.')
param project_name string = 'sonde'

@description('Optional override for the resource group name. Leave empty to derive one from project_name.')
param resource_group_name string = ''

@description('Base64-encoded DER certificate public data to register on the Azure companion app registration.')
param companionCertificateBase64 string

@description('Display name for the registered companion certificate credential.')
param companionCertificateDisplayName string = 'sonde-azure-companion'

@description('Optional override for the Service Bus namespace name.')
param serviceBusNamespaceName string = ''

@description('Queue name for gateway-originated connector traffic.')
param upstreamQueueName string = 'connector-upstream'

@description('Queue name for cloud-originated desired-state traffic.')
param downstreamQueueName string = 'desired-state'

@description('Optional override for the Storage Account name.')
param storageAccountName string = ''

@description('Table name reserved for later decoded-data persistence.')
param tableName string = 'decodeddata'

@description('Optional override for the placeholder Azure Function App name.')
param functionAppName string = ''

@description('Optional override for the placeholder Function hosting plan name.')
param functionPlanName string = ''

var projectSlug = toLower(replace(replace(replace(replace(replace(project_name, '-', ''), '_', ''), ' ', ''), '.', ''), '/', ''))
var effectiveProjectSlug = empty(projectSlug) ? 'sonde' : projectSlug
var effectiveResourceGroupName = empty(resource_group_name) ? '${take(effectiveProjectSlug, 84)}-azure' : resource_group_name
var effectiveServiceBusNamespaceName = empty(serviceBusNamespaceName)
  ? take('${take(effectiveProjectSlug, 20)}-sb-${take(uniqueString(subscription().subscriptionId, effectiveResourceGroupName), 8)}', 50)
  : serviceBusNamespaceName
var effectiveStorageAccountName = empty(storageAccountName)
  ? take('st${take(uniqueString(subscription().subscriptionId, project_name, effectiveResourceGroupName, 'storage'), 22)}', 24)
  : storageAccountName
var effectiveFunctionAppName = empty(functionAppName)
  ? take('${take(effectiveProjectSlug, 24)}-decoder-${take(uniqueString(subscription().subscriptionId, effectiveResourceGroupName, 'func'), 8)}', 60)
  : functionAppName
var effectiveFunctionPlanName = empty(functionPlanName)
  ? take('${take(effectiveProjectSlug, 24)}-func-plan', 40)
  : functionPlanName
var tags = {
  project: project_name
}

resource stackResourceGroup 'Microsoft.Resources/resourceGroups@2024-03-01' = {
  name: effectiveResourceGroupName
  location: location
  tags: tags
}

module companionIdentity './modules/companion-identity.bicep' = {
  name: 'companionIdentity'
  params: {
    projectName: project_name
    identitySuffix: take(uniqueString(subscription().subscriptionId, effectiveResourceGroupName, 'companion-identity'), 8)
    certificateBase64: companionCertificateBase64
    certificateDisplayName: companionCertificateDisplayName
  }
}

module stack './modules/stack.bicep' = {
  name: 'azureCompanionStack'
  scope: stackResourceGroup
  params: {
    location: location
    tags: tags
    serviceBusNamespaceName: effectiveServiceBusNamespaceName
    upstreamQueueName: upstreamQueueName
    downstreamQueueName: downstreamQueueName
    storageAccountName: effectiveStorageAccountName
    tableName: tableName
    functionAppName: effectiveFunctionAppName
    functionPlanName: effectiveFunctionPlanName
    companionServicePrincipalObjectId: companionIdentity.outputs.servicePrincipalObjectId
  }
}

output resourceGroupName string = stackResourceGroup.name
output serviceBusNamespaceName string = stack.outputs.serviceBusNamespaceName
output upstreamQueueName string = stack.outputs.upstreamQueueName
output downstreamQueueName string = stack.outputs.downstreamQueueName
output storageAccountName string = stack.outputs.storageAccountName
output storageTableName string = stack.outputs.tableName
output functionAppName string = stack.outputs.functionAppName
output functionPrincipalId string = stack.outputs.functionPrincipalId
output companionClientId string = companionIdentity.outputs.clientId
output companionTenantId string = companionIdentity.outputs.tenantId
output companionServicePrincipalObjectId string = companionIdentity.outputs.servicePrincipalObjectId
output companionBootstrapValues object = {
  tenantId: companionIdentity.outputs.tenantId
  clientId: companionIdentity.outputs.clientId
  serviceBusNamespace: stack.outputs.serviceBusNamespaceName
  upstreamQueue: stack.outputs.upstreamQueueName
  downstreamQueue: stack.outputs.downstreamQueueName
  note: 'The deployment registers the supplied certificate public material on the Entra app. The matching PEM certificate and private key remain caller-managed local artifacts for sonde-azure-companion bootstrap.'
}
