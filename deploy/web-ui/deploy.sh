#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 sonde contributors
#
# Deploy the Sonde Web UI to Azure Static Web App.
#
# Prerequisites:
#   - az CLI logged in
#   - jq available (for JSON merging)
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

# Resolve login endpoint from the active cloud for sovereign cloud compatibility
LOGIN_ENDPOINT="$(az cloud show --query endpoints.activeDirectory -o tsv)"
LOGIN_ENDPOINT="${LOGIN_ENDPOINT%/}"
if [ -z "$LOGIN_ENDPOINT" ]; then
  echo "ERROR: Could not resolve Azure login endpoint from active cloud" >&2
  exit 1
fi

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
  "msalAuthority": "$LOGIN_ENDPOINT/$TENANT_ID",
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
if [ -z "$CURRENT_URIS" ] || [ "$CURRENT_URIS" = "null" ]; then
  CURRENT_URIS="[]"
fi
URI_EXISTS=0
echo "$CURRENT_URIS" | jq -e --arg uri "$REDIRECT_URI" 'index($uri) != null' >/dev/null 2>&1 && URI_EXISTS=1
if [ "$URI_EXISTS" -eq 1 ]; then
  echo "  Redirect URI already registered"
else
  # Merge with existing URIs to avoid overwriting the list
  MERGED_URIS="$(echo "$CURRENT_URIS" | jq -c --arg uri "$REDIRECT_URI" '(. // []) + [$uri]')"
  PATCH_BODY="$(jq -n -c --argjson uris "$MERGED_URIS" '{"spa":{"redirectUris":$uris}}')"
  az rest --method PATCH \
    --url "https://graph.microsoft.com/v1.0/applications/$APP_OBJECT_ID" \
    --headers "Content-Type=application/json" \
    --body "$PATCH_BODY" \
    --output none
  echo "  Added redirect URI: $REDIRECT_URI"
fi

echo ""
echo "=== Adding Azure Storage API permission ==="
# Declare user_impersonation on Azure Storage (e406a681-f3d4-42a8-90b6-c2b029497af1)
# Suppress the "Invoking `az ad app permission grant` is needed" warning
# emitted by `az ad app permission add` — the grant is performed below.
_perm_err="$(mktemp)"
az ad app permission add --id "$APP_OBJECT_ID" \
  --api "e406a681-f3d4-42a8-90b6-c2b029497af1" \
  --api-permissions "da399722-a3ea-4c11-8b0d-7b37b3d5fa83=Scope" 2>"$_perm_err" || true
grep -v 'is needed to make the change effective' "$_perm_err" >&2 || true
rm -f "$_perm_err"
# Grant admin consent so users don't need to consent individually
az ad app permission grant \
  --id "$COMPANION_CLIENT_ID" \
  --api "e406a681-f3d4-42a8-90b6-c2b029497af1" \
  --scope "user_impersonation" \
  --output none 2>/dev/null || true
echo "  Azure Storage user_impersonation permission granted"

echo ""
echo "=== Exposing Function App API scope on Entra app ==="
# Ensure the Entra app exposes api://<clientId>/user_impersonation so that
# the SPA can acquire tokens scoped to the Function App and EasyAuth can
# validate them.
CURRENT_API="$(az ad app show --id "$APP_OBJECT_ID" \
  --query 'api.oauth2PermissionScopes' -o json 2>/dev/null || echo '[]')"
if [ -z "$CURRENT_API" ] || [ "$CURRENT_API" = "null" ]; then
  CURRENT_API="[]"
fi
HAS_SCOPE=0
echo "$CURRENT_API" | jq -e '[.[] | select(.value == "user_impersonation")] | length > 0' >/dev/null 2>&1 && HAS_SCOPE=1
# Also check identifierUris includes our API URI
CURRENT_IDENTIFIER_URIS="$(az ad app show --id "$APP_OBJECT_ID" \
  --query 'identifierUris' -o json 2>/dev/null || echo '[]')"
if [ -z "$CURRENT_IDENTIFIER_URIS" ] || [ "$CURRENT_IDENTIFIER_URIS" = "null" ]; then
  CURRENT_IDENTIFIER_URIS="[]"
fi
HAS_IDENTIFIER_URI=0
echo "$CURRENT_IDENTIFIER_URIS" | jq -e --arg uri "api://$CLIENT_ID" 'index($uri) != null' >/dev/null 2>&1 && HAS_IDENTIFIER_URI=1
if [ "$HAS_SCOPE" -eq 1 ] && [ "$HAS_IDENTIFIER_URI" -eq 1 ]; then
  echo "  API scope user_impersonation already exposed"
else
  SCOPE_ID="$(python3 -c 'import uuid; print(uuid.uuid4())' 2>/dev/null || uuidgen 2>/dev/null || cat /proc/sys/kernel/random/uuid)"
  if [ "$HAS_SCOPE" -eq 1 ]; then
    MERGED_SCOPES="$CURRENT_API"
  else
    MERGED_SCOPES="$(echo "$CURRENT_API" | jq -c --arg sid "$SCOPE_ID" \
      '. + [{"adminConsentDescription":"Allow the SPA to call the Function App on behalf of the signed-in user","adminConsentDisplayName":"Access Sonde Function App","id":$sid,"isEnabled":true,"type":"User","userConsentDescription":"Allow the app to access the Sonde Function App on your behalf","userConsentDisplayName":"Access Sonde Function App","value":"user_impersonation"}]')"
  fi
  MERGED_IDENTIFIER_URIS="$(echo "$CURRENT_IDENTIFIER_URIS" | jq -c --arg uri "api://$CLIENT_ID" \
    'if index($uri) != null then . else . + [$uri] end')"
  PATCH_BODY="$(jq -n -c --argjson uris "$MERGED_IDENTIFIER_URIS" --argjson scopes "$MERGED_SCOPES" \
    '{"identifierUris":$uris,"api":{"oauth2PermissionScopes":$scopes}}')"
  az rest --method PATCH \
    --url "https://graph.microsoft.com/v1.0/applications/$APP_OBJECT_ID" \
    --headers "Content-Type=application/json" \
    --body "$PATCH_BODY" \
    --output none
  echo "  Exposed api://$CLIENT_ID/user_impersonation scope"
fi

echo ""
echo "=== Configuring Function App EasyAuth ==="
# Configure Azure App Service Authentication (EasyAuth) on the Function App
# to validate Entra ID bearer tokens. This replaces function-key auth.
# Use az rest with the authSettingsV2 JSON directly for reliable configuration.
FUNCTION_APP_ID="$(az functionapp show --name "$FUNCTION_APP" \
  --resource-group "$RESOURCE_GROUP" --query 'id' -o tsv)"
AUTH_BODY="$(jq -n -c --arg clientId "$CLIENT_ID" --arg tenantId "$TENANT_ID" --arg loginEndpoint "$LOGIN_ENDPOINT" '{
  properties: {
    platform: { enabled: true },
    globalValidation: { unauthenticatedClientAction: "Return401" },
    identityProviders: {
      azureActiveDirectory: {
        enabled: true,
        registration: {
          clientId: $clientId,
          openIdIssuer: ($loginEndpoint + "/" + $tenantId + "/v2.0")
        },
        validation: {
          allowedAudiences: [("api://" + $clientId), $clientId],
          defaultAuthorizationPolicy: {
            allowedApplications: [$clientId]
          }
        }
      }
    }
  }
}')"
az rest --method PUT \
  --url "https://management.azure.com${FUNCTION_APP_ID}/config/authsettingsV2?api-version=2024-04-01" \
  --headers "Content-Type=application/json" \
  --body "$AUTH_BODY" \
  --output none
echo "  EasyAuth configured with Entra ID provider"

echo ""
echo "=== Configuring Function App CORS ==="
# The web UI calls the Function App ingest endpoint from the browser.
# Without CORS, the preflight request is rejected and fetch() fails.
EXISTING_CORS="$(az functionapp cors show --name "$FUNCTION_APP" \
  --resource-group "$RESOURCE_GROUP" --query 'allowedOrigins' -o json 2>/dev/null || echo '[]')"
SWA_ORIGIN="https://$SWA_HOSTNAME"
HAS_ORIGIN=0
echo "$EXISTING_CORS" | jq -e --arg o "$SWA_ORIGIN" 'index($o) != null' >/dev/null 2>&1 && HAS_ORIGIN=1
if [ "$HAS_ORIGIN" -eq 1 ]; then
  echo "  CORS origin already registered"
else
  az functionapp cors add --name "$FUNCTION_APP" \
    --resource-group "$RESOURCE_GROUP" \
    --allowed-origins "$SWA_ORIGIN" --output none
  echo "  Added CORS origin: $SWA_ORIGIN"
fi

# If the SWA has a custom domain, also register it as a CORS origin and
# add its redirect URI to the Entra app registration.
# The custom domain binding itself is done in bootstrap.sh via
# `az staticwebapp hostname set` after Bicep provisions the DNS record.
CUSTOM_DOMAINS="$(az staticwebapp hostname list --name "$SWA_NAME" \
  --resource-group "$RESOURCE_GROUP" --query '[].domainName' -o json 2>/dev/null || echo '[]')"
for DOMAIN in $(echo "$CUSTOM_DOMAINS" | jq -r '.[]? // empty'); do
  # Skip the default azurestaticapps.net hostname — already handled above
  case "$DOMAIN" in *.azurestaticapps.net) continue ;; esac

  CUSTOM_ORIGIN="https://$DOMAIN"

  # CORS origin
  CORS_NOW="$(az functionapp cors show --name "$FUNCTION_APP" \
    --resource-group "$RESOURCE_GROUP" --query 'allowedOrigins' -o json 2>/dev/null || echo '[]')"
  HAS_CUSTOM_CORS=0
  echo "$CORS_NOW" | jq -e --arg o "$CUSTOM_ORIGIN" 'index($o) != null' >/dev/null 2>&1 && HAS_CUSTOM_CORS=1
  if [ "$HAS_CUSTOM_CORS" -eq 1 ]; then
    echo "  CORS origin $CUSTOM_ORIGIN already registered"
  else
    az functionapp cors add --name "$FUNCTION_APP" \
      --resource-group "$RESOURCE_GROUP" \
      --allowed-origins "$CUSTOM_ORIGIN" --output none
    echo "  Added CORS origin: $CUSTOM_ORIGIN"
  fi

  # SPA redirect URI — re-fetch current URIs to avoid stale state
  URIS_NOW="$(az ad app show --id "$APP_OBJECT_ID" \
    --query 'spa.redirectUris' -o json 2>/dev/null || echo '[]')"
  if [ -z "$URIS_NOW" ] || [ "$URIS_NOW" = "null" ]; then
    URIS_NOW="[]"
  fi
  CUSTOM_URI_EXISTS=0
  echo "$URIS_NOW" | jq -e --arg uri "$CUSTOM_ORIGIN" 'index($uri) != null' >/dev/null 2>&1 && CUSTOM_URI_EXISTS=1
  if [ "$CUSTOM_URI_EXISTS" -eq 1 ]; then
    echo "  Redirect URI $CUSTOM_ORIGIN already registered"
  else
    MERGED_URIS="$(echo "$URIS_NOW" | jq -c --arg uri "$CUSTOM_ORIGIN" '. + [$uri]')"
    PATCH_BODY="$(jq -n -c --argjson uris "$MERGED_URIS" '{"spa":{"redirectUris":$uris}}')"
    az rest --method PATCH \
      --url "https://graph.microsoft.com/v1.0/applications/$APP_OBJECT_ID" \
      --headers "Content-Type=application/json" \
      --body "$PATCH_BODY" \
      --output none
    echo "  Added redirect URI: $CUSTOM_ORIGIN"
  fi
done

echo ""
echo "=== Assigning Storage Table Data Contributor to deploying user ==="
DEPLOYER_PRINCIPAL="$(az ad signed-in-user show --query id -o tsv 2>/dev/null || true)"
if [ -z "$DEPLOYER_PRINCIPAL" ]; then
  echo "  WARNING: Could not determine signed-in user. Skipping role assignment."
  echo "  Grant 'Storage Table Data Contributor' manually — see instructions below."
else
  STORAGE_SCOPE="/subscriptions/$(az account show --query id -o tsv)/resourceGroups/$RESOURCE_GROUP/providers/Microsoft.Storage/storageAccounts/$STORAGE_ACCOUNT"
  az role assignment create --assignee "$DEPLOYER_PRINCIPAL" \
    --role "Storage Table Data Contributor" \
    --scope "$STORAGE_SCOPE" \
    --output none 2>/dev/null || true
  echo "  Assigned 'Storage Table Data Contributor' to deploying user"
fi

echo ""
echo "=== Deploying SPA content ==="

# Zip the SPA content using Python (always available with az CLI).
SPA_ZIP="$(mktemp "${TMPDIR:-/tmp}/sonde-spa-XXXXXX.zip")"
_SPA_BLOB_NAME="spa-deploy-$(date +%s)-$$-${RANDOM}.zip"
cleanup_spa_blob() {
    rm -f "$SPA_ZIP"
    if [ -n "${_SPA_BLOB_UPLOADED:-}" ]; then
        az storage blob delete \
            --account-name "$STORAGE_ACCOUNT" \
            --container-name "spa-deploy" \
            --name "$_SPA_BLOB_NAME" \
            --output none 2>/dev/null || true
    fi
}
trap cleanup_spa_blob EXIT

python3 - "$SCRIPT_DIR" "$SPA_ZIP" <<'PYEOF'
import os, sys, zipfile
src_dir, dst_zip = sys.argv[1], sys.argv[2]
with zipfile.ZipFile(dst_zip, "w", zipfile.ZIP_DEFLATED) as zf:
    for root, _dirs, files in os.walk(src_dir):
        for f in sorted(files):
            full = os.path.join(root, f)
            arcname = os.path.relpath(full, src_dir)
            zf.write(full, arcname)
PYEOF

# Upload the zip to a temporary blob in the storage account.
# Use key-based auth: the deployer has Contributor/Owner from az login,
# so they can access storage account keys directly.
az storage container create \
    --account-name "$STORAGE_ACCOUNT" \
    --name "spa-deploy" \
    --public-access off \
    --output none 2>/dev/null || true
az storage blob upload \
    --account-name "$STORAGE_ACCOUNT" \
    --container-name "spa-deploy" \
    --name "$_SPA_BLOB_NAME" \
    --file "$SPA_ZIP" \
    --overwrite \
    --output none
_SPA_BLOB_UPLOADED=1

# Generate a short-lived read-only SAS URL for the blob.
# Resolve the blob endpoint from the storage account to support sovereign clouds.
SAS_EXPIRY="$(python3 -c "from datetime import datetime,timedelta,timezone;print((datetime.now(timezone.utc)+timedelta(minutes=30)).strftime('%Y-%m-%dT%H:%M:%SZ'))")"
BLOB_ENDPOINT="$(az storage account show \
    --name "$STORAGE_ACCOUNT" \
    --resource-group "$RESOURCE_GROUP" \
    --query 'primaryEndpoints.blob' \
    --output tsv)"
BLOB_ENDPOINT="${BLOB_ENDPOINT%/}"
SAS_TOKEN="$(az storage blob generate-sas \
    --account-name "$STORAGE_ACCOUNT" \
    --container-name "spa-deploy" \
    --name "$_SPA_BLOB_NAME" \
    --permissions r \
    --expiry "$SAS_EXPIRY" \
    --output tsv)"
BLOB_URL="${BLOB_ENDPOINT}/spa-deploy/${_SPA_BLOB_NAME}?${SAS_TOKEN}"

# Deploy via ARM zipdeploy REST API.
# Resolve the ARM endpoint from the active cloud for sovereign cloud support.
SUBSCRIPTION_ID="$(az account show --query id --output tsv)"
ARM_ENDPOINT="$(az cloud show --query endpoints.resourceManager --output tsv)"
ARM_ENDPOINT="${ARM_ENDPOINT%/}"
ZIPDEPLOY_URL="${ARM_ENDPOINT}/subscriptions/${SUBSCRIPTION_ID}/resourceGroups/${RESOURCE_GROUP}/providers/Microsoft.Web/staticSites/${SWA_NAME}/zipdeploy?api-version=2024-04-01"
az rest --method POST \
    --url "$ZIPDEPLOY_URL" \
    --body "{\"properties\":{\"appZipUrl\":\"${BLOB_URL}\",\"provider\":\"sonde-deploy\"}}" \
    --output none

# Wait for deployment to complete by polling for config.json content.
echo "  Waiting for deployment..."
SPA_DEADLINE="$(( $(date +%s) + 120 ))"
while :; do
    FETCHED_CONFIG="$(curl -sf "https://$SWA_HOSTNAME/config.json" 2>/dev/null || true)"
    if echo "$FETCHED_CONFIG" | grep -q "$CLIENT_ID" 2>/dev/null; then
        break
    fi
    if [ "$(date +%s)" -ge "$SPA_DEADLINE" ]; then
        echo "  ERROR: timed out verifying SPA deployment after 120s"
        exit 1
    fi
    sleep 5
done

echo ""
echo "=== Deployment complete ==="
echo "  URL: https://$SWA_HOSTNAME"
for DOMAIN in $(echo "$CUSTOM_DOMAINS" | jq -r '.[]? // empty'); do
  case "$DOMAIN" in *.azurestaticapps.net) continue ;; esac
  echo "  Custom domain: https://$DOMAIN"
done
echo ""
echo "  To use the SPA, users need 'Storage Table Data Contributor' role"
echo "  on the storage account. Grant with:"
echo "    az role assignment create --assignee <USER_PRINCIPAL> \\"
echo "      --role 'Storage Table Data Contributor' \\"
echo "      --scope /subscriptions/<SUB>/resourceGroups/$RESOURCE_GROUP/providers/Microsoft.Storage/storageAccounts/$STORAGE_ACCOUNT"
