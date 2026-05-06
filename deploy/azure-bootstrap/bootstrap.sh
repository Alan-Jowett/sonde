#!/bin/sh
set -eu

az login --use-device-code --output none >&2
if [ -n "${SONDE_AZURE_SUBSCRIPTION_ID:-}" ]; then
    az account set --subscription "$SONDE_AZURE_SUBSCRIPTION_ID" >&2
fi
echo "__SONDE_AZURE_DEPLOYMENT_START__" >&2
az deployment sub create \
    --location "$SONDE_AZURE_LOCATION" \
    --template-file /opt/sonde/deploy/bicep/main.bicep \
    --parameters companionCertificateBase64="$COMPANION_CERT_BASE64" \
    --parameters location="$SONDE_AZURE_LOCATION" \
    --parameters project_name="$SONDE_AZURE_PROJECT_NAME" \
    --query 'properties.outputs' \
    --output json
