// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

targetScope = 'resourceGroup'

@description('DNS zone name (e.g., sondeplatform.com).')
param dnsZoneName string

@description('Resource ID of the Static Web App to alias.')
param staticWebAppResourceId string

resource dnsZone 'Microsoft.Network/dnsZones@2018-05-01' existing = {
  name: dnsZoneName
}

// ALIAS A record — resolves the apex domain to the Static Web App's
// dynamic IP addresses.  Azure DNS ALIAS records use targetResource
// instead of a static IP address.  Domain ownership validation is
// handled by the deploy script via `az staticwebapp hostname set`.
resource aliasRecord 'Microsoft.Network/dnsZones/A@2018-05-01' = {
  parent: dnsZone
  name: '@'
  properties: {
    TTL: 3600
    targetResource: {
      id: staticWebAppResourceId
    }
  }
}

output aliasRecordFqdn string = aliasRecord.properties.fqdn
