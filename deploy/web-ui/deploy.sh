#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 sonde contributors
#
# Deploy the Sonde Web UI to Azure Static Web App.
#
# Prerequisites:
#   - az CLI logged in
#   - jq available (for JSON merging)
#   - npm/npx available (for SWA CLI)
#
# Usage:
#   ./deploy/web-ui/deploy.sh <COMPANION_CLIENT_ID> [RESOURCE_GROUP]
#
# COMPANION_CLIENT_ID is the Entra app (client) ID of the Azure companion
# app registration (output as companionClientId from Bicep deployment).
# If RESOURCE_GROUP is omitted, defaults to 'sonde-azure'.

set -euo pipefail

if [ $# -lt 1 ]; then
  echo "Usage: $0 <COMPANION_CLIENT_ID> [RESOURCE_GROUP]" >&2
  exit 1
fi

COMPANION_CLIENT_ID="$1"
RESOURCE_GROUP="${2:-sonde-azure}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

echo "=== Gathering deployment outputs ==="

# Find the Static Web App
SWA_NAME="$(az staticwebapp list --resource-group "$RESOURCE_GROUP" \
  --query '[0].name' -o tsv 2>/dev/null)"
if [ -z "$SWA_NAME" ]; then
  echo "ERROR: No Static Web App found in resource group $RESOURCE_GROUP" >&2
  exit 1
fi

SWA_HOSTNAME="$(az staticwebapp show --name "$SWA_NAME" \
  --resource-group "$RESOURCE_GROUP" \
  --query 'defaultHostname' -o tsv)"

# Find the Function App
FUNCTION_APP="$(az functionapp list --resource-group "$RESOURCE_GROUP" \
  --query '[0].name' -o tsv 2>/dev/null)"
if [ -z "$FUNCTION_APP" ]; then
  echo "ERROR: No Function App found in resource group $RESOURCE_GROUP" >&2
  exit 1
fi

# Find the Storage Account
STORAGE_ACCOUNT="$(az storage account list --resource-group "$RESOURCE_GROUP" \
  --query '[0].name' -o tsv 2>/dev/null)"
if [ -z "$STORAGE_ACCOUNT" ]; then
  echo "ERROR: No Storage Account found in resource group $RESOURCE_GROUP" >&2
  exit 1
fi

# Resolve the Entra app from the supplied client ID
CLIENT_ID="$COMPANION_CLIENT_ID"
APP_OBJECT_ID="$(az ad app show --id "$CLIENT_ID" --query 'id' -o tsv)"
if [ -z "$APP_OBJECT_ID" ]; then
  echo "ERROR: Could not resolve Entra app for client ID $CLIENT_ID" >&2
  exit 1
fi
TENANT_ID="$(az account show --query tenantId -o tsv)"

echo "  Static Web App: $SWA_NAME ($SWA_HOSTNAME)"
echo "  Function App:   $FUNCTION_APP"
echo "  Storage Account: $STORAGE_ACCOUNT"
echo "  Client ID:      $CLIENT_ID"
echo "  Tenant ID:      $TENANT_ID"

echo ""
echo "=== Generating config.json ==="
cat > "$SCRIPT_DIR/config.json" <<EOF
{
  "msalClientId": "$CLIENT_ID",
  "msalAuthority": "https://login.microsoftonline.com/$TENANT_ID",
  "storageAccount": "$STORAGE_ACCOUNT",
  "functionAppName": "$FUNCTION_APP"
}
EOF
cat "$SCRIPT_DIR/config.json"

echo ""
echo "=== Adding SPA redirect URI to Entra app ==="
REDIRECT_URI="https://$SWA_HOSTNAME"

# Get current SPA redirect URIs and add ours if not present
CURRENT_URIS="$(az ad app show --id "$APP_OBJECT_ID" \
  --query 'spa.redirectUris' -o json 2>/dev/null || echo '[]')"
if echo "$CURRENT_URIS" | grep -Fq "$REDIRECT_URI"; then
  echo "  Redirect URI already registered"
else
  # Merge with existing URIs to avoid overwriting the list
  MERGED_URIS="$(echo "$CURRENT_URIS" | jq -c --arg uri "$REDIRECT_URI" '(. // []) + [$uri]')"
  az rest --method PATCH \
    --url "https://graph.microsoft.com/v1.0/applications/$APP_OBJECT_ID" \
    --headers "Content-Type=application/json" \
    --body "{\"spa\":{\"redirectUris\":$MERGED_URIS}}"
  echo "  Added redirect URI: $REDIRECT_URI"
fi

echo ""
echo "=== Adding Azure Storage API permission ==="
# Grant user_impersonation on Azure Storage (e406a681-f3d4-42a8-90b6-c2b029497af1)
az ad app permission add --id "$APP_OBJECT_ID" \
  --api "e406a681-f3d4-42a8-90b6-c2b029497af1" \
  --api-permissions "da399722-a3ea-4c11-8b0d-7b37b3d5fa83=Scope" || true
echo "  Azure Storage user_impersonation permission configured"

echo ""
echo "=== Deploying SPA content ==="
DEPLOYMENT_TOKEN="$(az staticwebapp secrets list --name "$SWA_NAME" \
  --resource-group "$RESOURCE_GROUP" \
  --query 'properties.apiKey' -o tsv)"

npx --yes @azure/static-web-apps-cli deploy "$SCRIPT_DIR" \
  --deployment-token "$DEPLOYMENT_TOKEN" \
  --env production

echo ""
echo "=== Deployment complete ==="
echo "  URL: https://$SWA_HOSTNAME"
echo ""
echo "  To use the SPA, users need 'Storage Table Data Contributor' role"
echo "  on the storage account. Grant with:"
echo "    az role assignment create --assignee <USER_PRINCIPAL> \\"
echo "      --role 'Storage Table Data Contributor' \\"
echo "      --scope /subscriptions/<SUB>/resourceGroups/$RESOURCE_GROUP/providers/Microsoft.Storage/storageAccounts/$STORAGE_ACCOUNT"
