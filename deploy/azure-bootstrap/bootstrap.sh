#!/bin/sh
set -eu

trim_string() {
    printf '%s' "$1" | tr -d '\r' | sed 's/^[[:space:]]*//;s/[[:space:]]*$//'
}

require_deployment_output_string() {
    field="$1"
    query="$2"
    value="$(trim_string "$3")"
    if [ -z "$value" ] || [ "$value" = "null" ]; then
        echo "deployment output \`$field\` is missing or null for query \`$query\`" >&2
        exit 1
    fi
    printf '%s\n' "$value"
}

read_required_deployment_outputs() {
    # Wrap in an outer array so `--output tsv` emits a single row with
    # tab-separated columns.  A flat JMESPath array produces one value per
    # line (newline-separated), which breaks the tab-based field splitting
    # below.
    query='[[properties.outputs.resourceGroupName.value, properties.outputs.functionAppName.value, properties.outputs.deploymentContainerName.value, properties.outputs.deploymentContainerUrl.value, properties.outputs.staticWebAppName.value, properties.outputs.staticWebAppHostname.value, properties.outputs.companionClientId.value, properties.outputs.storageAccountName.value, properties.outputs.companionTenantId.value]]'
    stderr_file="$(mktemp "${TMPDIR:-/tmp}/sonde-azure-deployment-show.XXXXXX")"
    if ! deployment_runtime_values="$(az deployment sub show \
        --name "$deployment_name" \
        --query "$query" \
        --output tsv 2>"$stderr_file")"; then
        if [ -s "$stderr_file" ]; then
            deployment_show_error="$(cat "$stderr_file")"
            echo "failed to read deployment outputs for query \`$query\`: $deployment_show_error" >&2
        else
            echo "failed to read deployment outputs for query \`$query\`" >&2
        fi
        rm -f "$stderr_file"
        exit 1
    fi
    if [ -s "$stderr_file" ]; then
        cat "$stderr_file" >&2
    fi
    rm -f "$stderr_file"

    old_ifs="$IFS"
    IFS="$(printf '\t')"
    set -- $deployment_runtime_values
    IFS="$old_ifs"
    field_count="$#"
    if [ "$field_count" -ne 9 ]; then
        echo "deployment output query \`$query\` returned $field_count field(s); expected 9 tab-separated values" >&2
        exit 1
    fi

    resource_group_name="$(
        require_deployment_output_string \
            resourceGroupName \
            "$query" \
            "$1"
    )"
    function_app_name="$(
        require_deployment_output_string \
            functionAppName \
            "$query" \
            "$2"
    )"
    deployment_container_name="$(
        require_deployment_output_string \
            deploymentContainerName \
            "$query" \
            "$3"
    )"
    deployment_container_url="$(
        require_deployment_output_string \
            deploymentContainerUrl \
            "$query" \
            "$4"
    )"
    static_web_app_name="$(
        require_deployment_output_string \
            staticWebAppName \
            "$query" \
            "$5"
    )"
    static_web_app_hostname="$(
        require_deployment_output_string \
            staticWebAppHostname \
            "$query" \
            "$6"
    )"
    companion_client_id="$(
        require_deployment_output_string \
            companionClientId \
            "$query" \
            "$7"
    )"
    storage_account_name="$(
        require_deployment_output_string \
            storageAccountName \
            "$query" \
            "$8"
    )"
    companion_tenant_id="$(
        require_deployment_output_string \
            companionTenantId \
            "$query" \
            "$9"
    )"
}

validate_positive_integer() {
    name="$1"
    value="$2"
    case "$value" in
        ''|*[!0-9]*)
            echo "$name must be a positive integer; got: $value" >&2
            exit 1
            ;;
        0)
            echo "$name must be greater than zero; got: $value" >&2
            exit 1
            ;;
        0[0-9]*)
            echo "$name must not contain leading zeros; got: $value" >&2
            exit 1
            ;;
    esac
}

wait_for_function_activation() {
    resource_group_name="$1"
    function_app_name="$2"
    timeout_secs="$3"
    deadline="$(( $(date +%s) + timeout_secs ))"

    while :; do
        function_list_stderr="$(mktemp "${TMPDIR:-/tmp}/sonde-azure-function-list.XXXXXX")"
        if loaded_count="$(az functionapp function list \
            --name "$function_app_name" \
            --resource-group "$resource_group_name" \
            --query 'length(@)' \
            --output tsv 2>"$function_list_stderr")"; then
            if [ -s "$function_list_stderr" ]; then
                cat "$function_list_stderr" >&2
            fi
            rm -f "$function_list_stderr"
            loaded_count="$(trim_string "$loaded_count")"
            case "$loaded_count" in
                ''|*[!0-9]*)
                    echo "invalid Azure function-list response while waiting for activation" >&2
                    exit 1
                    ;;
            esac

            if [ "$loaded_count" -gt 0 ]; then
                echo "Azure Function App reports $loaded_count loaded function(s)" >&2
                return 0
            fi

            echo "Waiting for Azure Function App to load functions..." >&2
        else
            if [ -s "$function_list_stderr" ]; then
                function_list_error="$(cat "$function_list_stderr")"
                echo "Azure Function activation probe failed: $function_list_error" >&2
            else
                echo "Azure Function activation probe failed" >&2
            fi
            rm -f "$function_list_stderr"
        fi

        if [ "$(date +%s)" -ge "$deadline" ]; then
            echo "timed out waiting for Azure Function App activation" >&2
            return 1
        fi

        sleep 5
    done
}

activation_timeout_secs="${SONDE_AZURE_FUNCTION_ACTIVATION_TIMEOUT_SECS:-300}"
deployment_timeout_secs="${SONDE_AZURE_FUNCTION_DEPLOY_TIMEOUT_SECS:-600}"
validate_positive_integer "SONDE_AZURE_FUNCTION_ACTIVATION_TIMEOUT_SECS" "$activation_timeout_secs"
validate_positive_integer "SONDE_AZURE_FUNCTION_DEPLOY_TIMEOUT_SECS" "$deployment_timeout_secs"

az login --use-device-code --output none >&2
if [ -n "${SONDE_AZURE_SUBSCRIPTION_ID:-}" ]; then
    az account set --subscription "$SONDE_AZURE_SUBSCRIPTION_ID" >&2
fi

# Resolve login endpoint from the active cloud for sovereign cloud compatibility
login_endpoint="$(az cloud show --query endpoints.activeDirectory -o tsv)"
login_endpoint="${login_endpoint%/}"
if [ -z "$login_endpoint" ]; then
    echo "could not resolve Azure login endpoint from active cloud" >&2
    exit 1
fi

echo "__SONDE_AZURE_DEPLOYMENT_START__" >&2
deployment_name="sonde-bootstrap-$(date +%Y%m%d%H%M%S)-$$"
# Validate custom domain parameter consistency — supplying one without the
# other silently disables the custom domain feature, which is hard to diagnose.
if [ -n "${SONDE_AZURE_CUSTOM_DOMAIN_NAME:-}" ] && [ -z "${SONDE_AZURE_CUSTOM_DOMAIN_DNS_RESOURCE_GROUP:-}" ]; then
    echo "SONDE_AZURE_CUSTOM_DOMAIN_NAME is set but SONDE_AZURE_CUSTOM_DOMAIN_DNS_RESOURCE_GROUP is empty; both are required for custom domain support" >&2
    exit 1
fi
if [ -z "${SONDE_AZURE_CUSTOM_DOMAIN_NAME:-}" ] && [ -n "${SONDE_AZURE_CUSTOM_DOMAIN_DNS_RESOURCE_GROUP:-}" ]; then
    echo "SONDE_AZURE_CUSTOM_DOMAIN_DNS_RESOURCE_GROUP is set but SONDE_AZURE_CUSTOM_DOMAIN_NAME is empty; both are required for custom domain support" >&2
    exit 1
fi
# dns-record.bicep only supports apex (naked) domains — the A ALIAS record
# is hardcoded to '@'.  When an explicit DNS zone name is provided it must
# match the custom domain name; subdomain bindings are not supported.
if [ -n "${SONDE_AZURE_CUSTOM_DOMAIN_DNS_ZONE_NAME:-}" ] && \
   [ "${SONDE_AZURE_CUSTOM_DOMAIN_DNS_ZONE_NAME}" != "${SONDE_AZURE_CUSTOM_DOMAIN_NAME:-}" ]; then
    echo "SONDE_AZURE_CUSTOM_DOMAIN_DNS_ZONE_NAME ($SONDE_AZURE_CUSTOM_DOMAIN_DNS_ZONE_NAME) differs from SONDE_AZURE_CUSTOM_DOMAIN_NAME (${SONDE_AZURE_CUSTOM_DOMAIN_NAME:-}); only apex domains are supported" >&2
    exit 1
fi
# Reject values containing whitespace — these are Azure resource identifiers
# and domain names which structurally cannot have spaces.  Catching this early
# prevents confusing word-splitting failures in the az CLI invocation below.
for _cdv in \
    "${SONDE_AZURE_CUSTOM_DOMAIN_NAME:-}" \
    "${SONDE_AZURE_CUSTOM_DOMAIN_DNS_RESOURCE_GROUP:-}" \
    "${SONDE_AZURE_CUSTOM_DOMAIN_DNS_ZONE_NAME:-}"; do
    case "$_cdv" in
        *[[:space:]]*) echo "custom domain parameter contains whitespace: '$_cdv'" >&2; exit 1 ;;
    esac
done
custom_domain_params=""
if [ -n "${SONDE_AZURE_CUSTOM_DOMAIN_NAME:-}" ]; then
    custom_domain_params="customDomainName=$SONDE_AZURE_CUSTOM_DOMAIN_NAME"
fi
if [ -n "${SONDE_AZURE_CUSTOM_DOMAIN_DNS_RESOURCE_GROUP:-}" ]; then
    custom_domain_params="$custom_domain_params customDomainDnsResourceGroup=$SONDE_AZURE_CUSTOM_DOMAIN_DNS_RESOURCE_GROUP"
fi
if [ -n "${SONDE_AZURE_CUSTOM_DOMAIN_DNS_ZONE_NAME:-}" ]; then
    custom_domain_params="$custom_domain_params customDomainDnsZoneName=$SONDE_AZURE_CUSTOM_DOMAIN_DNS_ZONE_NAME"
fi
# shellcheck disable=SC2086 — intentional word-splitting on custom_domain_params;
# all values are Azure resource identifiers which cannot contain whitespace.
deployment_outputs="$(az deployment sub create \
    --name "$deployment_name" \
    --location "$SONDE_AZURE_LOCATION" \
    --template-file /opt/sonde/deploy/bicep/main.bicep \
    --parameters "companionCertificateBase64=$COMPANION_CERT_BASE64" \
                 "location=$SONDE_AZURE_LOCATION" \
                 "project_name=$SONDE_AZURE_PROJECT_NAME" \
                 $custom_domain_params \
    --query 'properties.outputs' \
    --output json)"

read_required_deployment_outputs
function_package_path="${SONDE_AZURE_FUNCTION_PACKAGE_PATH:-/opt/sonde/deploy/azure-handler/sonde-azure-handler-function.zip}"

if [ ! -r "$function_package_path" ]; then
    echo "bundled Azure handler package not found: $function_package_path" >&2
    exit 1
fi

echo "Deploying bundled Azure handler package to Function App $function_app_name" >&2
echo "Using deployment target $deployment_container_name ($deployment_container_url)" >&2
# Explicitly clear linuxFxVersion so the Azure CLI does not warn about
# missing runtime detection during zip deployment.  Custom handlers do not
# need a managed runtime stack.
az functionapp config set \
    --name "$function_app_name" \
    --resource-group "$resource_group_name" \
    --linux-fx-version '' \
    --output none 1>&2 || true
# The CLI's post-deployment health check ("Failed to fetch host key") can
# fail on Flex Consumption plans even when the zip upload (HTTP 202)
# succeeded.  Tolerate this exit code and rely on wait_for_function_activation
# to confirm the function is actually loaded.
config_zip_exit=0
az functionapp deployment source config-zip \
    --src "$function_package_path" \
    --name "$function_app_name" \
    --resource-group "$resource_group_name" \
    --timeout "$deployment_timeout_secs" \
    --output none 1>&2 || config_zip_exit=$?
if [ "$config_zip_exit" -ne 0 ]; then
    echo "WARNING: config-zip exited $config_zip_exit; verifying via function activation probe" >&2
fi

wait_for_function_activation "$resource_group_name" "$function_app_name" "$activation_timeout_secs"

# ── SPA deployment ──────────────────────────────────────────────────────────
echo "Deploying Web UI to Static Web App $static_web_app_name" >&2
web_ui_dir="${SONDE_AZURE_WEB_UI_DIR:-/opt/sonde/deploy/web-ui}"

if [ ! -d "$web_ui_dir" ]; then
    echo "bundled Web UI content not found: $web_ui_dir" >&2
    exit 1
fi

# Generate config.json from deployment outputs
cat > "$web_ui_dir/config.json" <<CONFIGEOF
{
  "msalClientId": "$companion_client_id",
  "msalAuthority": "$login_endpoint/$companion_tenant_id",
  "storageAccount": "$storage_account_name",
  "functionAppName": "$function_app_name"
}
CONFIGEOF
echo "Generated config.json for SPA" >&2

# Deploy SPA content to Static Web App using the StaticSitesClient binary
# from the multi-arch Docker image. This replaces the SWA CLI npm package
# (which required amd64-only native binaries) with the same underlying
# deployment binary packaged as a multi-arch container image.

swa_deployment_token="$(trim_string "$(az staticwebapp secrets list \
    --name "$static_web_app_name" \
    --resource-group "$resource_group_name" \
    --query 'properties.apiKey' \
    --output tsv)")"
if [ -z "$swa_deployment_token" ]; then
    echo "failed to retrieve Static Web App deployment token" >&2
    exit 1
fi

docker run --rm \
    -v "$web_ui_dir:/app" \
    -e "DEPLOYMENT_TOKEN=$swa_deployment_token" \
    -e "DEPLOYMENT_ACTION=upload" \
    -e "DEPLOYMENT_PROVIDER=sonde-bootstrap" \
    -e "APP_LOCATION=/app" \
    -e "SKIP_APP_BUILD=true" \
    -e "SKIP_API_BUILD=true" \
    -e "VERBOSE=false" \
    mcr.microsoft.com/appsvc/staticappsclient:stable 1>&2

echo "SPA content deployed to https://$static_web_app_hostname" >&2

# ── Custom domain binding ───────────────────────────────────────────────────
# Bicep creates the DNS ALIAS record, but the SWA custom domain binding
# (ownership validation + managed SSL certificate) must be done via the CLI.
if [ -n "${SONDE_AZURE_CUSTOM_DOMAIN_NAME:-}" ]; then
    echo "Binding custom domain $SONDE_AZURE_CUSTOM_DOMAIN_NAME to Static Web App $static_web_app_name" >&2
    # `hostname set` is idempotent — safe to re-run on subsequent deployments.
    az staticwebapp hostname set \
        --name "$static_web_app_name" \
        --resource-group "$resource_group_name" \
        --hostname "$SONDE_AZURE_CUSTOM_DOMAIN_NAME" \
        --output none 1>&2
    echo "Custom domain bound; managed SSL certificate provisioning initiated" >&2
fi

# ── Entra app configuration ─────────────────────────────────────────────────
echo "Configuring Entra app registration for Web UI" >&2

# Resolve the Entra app object ID from the companion client ID
app_object_id="$(az ad app show --id "$companion_client_id" --query 'id' --output tsv)"
if [ -z "$app_object_id" ]; then
    echo "failed to resolve Entra app object ID for client ID $companion_client_id" >&2
    exit 1
fi

# Register the SWA hostname as a SPA redirect URI (merge with existing)
redirect_uri="https://$static_web_app_hostname"
current_uris="$(az ad app show --id "$app_object_id" \
    --query 'spa.redirectUris' --output json)"
if [ -z "$current_uris" ] || [ "$current_uris" = "null" ]; then
    current_uris="[]"
fi
uri_exists=0
echo "$current_uris" | jq -e --arg uri "$redirect_uri" 'index($uri) != null' >/dev/null 2>&1 && uri_exists=1
if [ "$uri_exists" -eq 1 ]; then
    echo "SPA redirect URI already registered" >&2
else
    merged_uris="$(echo "$current_uris" | jq -c --arg uri "$redirect_uri" \
        '(. // []) + [$uri]')" || {
        echo "failed to merge redirect URIs" >&2
        exit 1
    }
    if [ -z "$merged_uris" ]; then
        echo "redirect URI merge produced empty result" >&2
        exit 1
    fi
    patch_body="$(jq -n -c --argjson uris "$merged_uris" '{"spa":{"redirectUris":$uris}}')"
    az rest --method PATCH \
        --url "https://graph.microsoft.com/v1.0/applications/$app_object_id" \
        --headers "Content-Type=application/json" \
        --body "$patch_body" \
        --output none
    echo "Added SPA redirect URI: $redirect_uri" >&2
fi

# If a custom domain was configured, also register it as a SPA redirect URI
if [ -n "${SONDE_AZURE_CUSTOM_DOMAIN_NAME:-}" ]; then
    custom_redirect_uri="https://$SONDE_AZURE_CUSTOM_DOMAIN_NAME"
    custom_uris="$(az ad app show --id "$app_object_id" \
        --query 'spa.redirectUris' --output json 2>/dev/null || echo '[]')"
    if [ -z "$custom_uris" ] || [ "$custom_uris" = "null" ]; then
        custom_uris="[]"
    fi
    custom_uri_exists=0
    echo "$custom_uris" | jq -e --arg uri "$custom_redirect_uri" 'index($uri) != null' >/dev/null 2>&1 && custom_uri_exists=1
    if [ "$custom_uri_exists" -eq 1 ]; then
        echo "Custom domain redirect URI already registered" >&2
    else
        custom_merged="$(echo "$custom_uris" | jq -c --arg uri "$custom_redirect_uri" '. + [$uri]')" || {
            echo "failed to merge custom domain redirect URI" >&2
            exit 1
        }
        custom_patch="$(jq -n -c --argjson uris "$custom_merged" '{"spa":{"redirectUris":$uris}}')"
        az rest --method PATCH \
            --url "https://graph.microsoft.com/v1.0/applications/$app_object_id" \
            --headers "Content-Type=application/json" \
            --body "$custom_patch" \
            --output none
        echo "Added custom domain redirect URI: $custom_redirect_uri" >&2
    fi
fi

# Add Azure Storage user_impersonation API permission (idempotent)
if az ad app permission list --id "$app_object_id" --query "[?resourceAppId=='e406a681-f3d4-42a8-90b6-c2b029497af1'].resourceAccess[?id=='da399722-a3ea-4c11-8b0d-7b37b3d5fa83'] | [0]" --output tsv 2>/dev/null | grep -q .; then
    echo "Azure Storage user_impersonation permission already configured" >&2
else
    # Suppress the "Invoking `az ad app permission grant` is needed" warning
    # emitted by `az ad app permission add` — the grant is performed below.
    _perm_err="$(mktemp)"
    if ! az ad app permission add --id "$app_object_id" \
        --api "e406a681-f3d4-42a8-90b6-c2b029497af1" \
        --api-permissions "da399722-a3ea-4c11-8b0d-7b37b3d5fa83=Scope" 2>"$_perm_err"; then
        cat "$_perm_err" >&2
        rm -f "$_perm_err"
        exit 1
    fi
    grep -v 'is needed to make the change effective' "$_perm_err" >&2 || true
    rm -f "$_perm_err"
    echo "Azure Storage user_impersonation permission declared" >&2
fi

# Grant admin consent for Azure Storage so users don't need to consent individually
az ad app permission grant \
    --id "$companion_client_id" \
    --api "e406a681-f3d4-42a8-90b6-c2b029497af1" \
    --scope "user_impersonation" \
    --output none 2>/dev/null || true
echo "Azure Storage user_impersonation permission granted" >&2

# Expose api://<clientId>/user_impersonation API scope on the Entra app
# so EasyAuth can validate SPA-acquired tokens for the Function App.
current_api="$(az ad app show --id "$app_object_id" \
    --query 'api.oauth2PermissionScopes' --output json 2>/dev/null || echo '[]')"
if [ -z "$current_api" ] || [ "$current_api" = "null" ]; then
    current_api="[]"
fi
has_scope=0
echo "$current_api" | jq -e '[.[] | select(.value == "user_impersonation")] | length > 0' >/dev/null 2>&1 && has_scope=1
current_identifier_uris="$(az ad app show --id "$app_object_id" \
    --query 'identifierUris' --output json 2>/dev/null || echo '[]')"
if [ -z "$current_identifier_uris" ] || [ "$current_identifier_uris" = "null" ]; then
    current_identifier_uris="[]"
fi
has_identifier_uri=0
echo "$current_identifier_uris" | jq -e --arg uri "api://$companion_client_id" 'index($uri) != null' >/dev/null 2>&1 && has_identifier_uri=1
if [ "$has_scope" -eq 1 ] && [ "$has_identifier_uri" -eq 1 ]; then
    echo "API scope user_impersonation already exposed" >&2
else
    scope_id="$(cat /proc/sys/kernel/random/uuid 2>/dev/null || python3 -c 'import uuid; print(uuid.uuid4())')"
    if [ "$has_scope" -eq 1 ]; then
        merged_scopes="$current_api"
    else
        merged_scopes="$(echo "$current_api" | jq -c --arg sid "$scope_id" \
            '. + [{"adminConsentDescription":"Allow the SPA to call the Function App on behalf of the signed-in user","adminConsentDisplayName":"Access Sonde Function App","id":$sid,"isEnabled":true,"type":"User","userConsentDescription":"Allow the app to access the Sonde Function App on your behalf","userConsentDisplayName":"Access Sonde Function App","value":"user_impersonation"}]')"
    fi
    merged_identifier_uris="$(echo "$current_identifier_uris" | jq -c --arg uri "api://$companion_client_id" \
        'if index($uri) != null then . else . + [$uri] end')"
    patch_body="$(jq -n -c --argjson uris "$merged_identifier_uris" --argjson scopes "$merged_scopes" \
        '{"identifierUris":$uris,"api":{"oauth2PermissionScopes":$scopes}}')"
    az rest --method PATCH \
        --url "https://graph.microsoft.com/v1.0/applications/$app_object_id" \
        --headers "Content-Type=application/json" \
        --body "$patch_body" \
        --output none
    echo "Exposed api://$companion_client_id/user_impersonation scope" >&2
fi

# Assign Storage Table Data Contributor to the deploying user so they can
# access the programs table via the SPA immediately after bootstrap.
deployer_principal="$(az ad signed-in-user show --query id --output tsv 2>/dev/null || true)"
if [ -z "$deployer_principal" ]; then
    echo "WARNING: Could not determine signed-in user. Skipping role assignment." >&2
    echo "  Grant 'Storage Table Data Contributor' manually on storage account $storage_account_name" >&2
else
    subscription_id="$(az account show --query id --output tsv 2>/dev/null || true)"
    if [ -z "$subscription_id" ]; then
        echo "WARNING: Could not determine subscription ID. Skipping role assignment." >&2
        echo "  Grant 'Storage Table Data Contributor' manually on storage account $storage_account_name" >&2
    else
        storage_scope="/subscriptions/$subscription_id/resourceGroups/$resource_group_name/providers/Microsoft.Storage/storageAccounts/$storage_account_name"
        existing_role="$(az role assignment list \
            --assignee "$deployer_principal" \
            --role "Storage Table Data Contributor" \
            --scope "$storage_scope" \
            --query "length(@)" \
            --output tsv 2>/dev/null || echo 0)"
        if [ "$existing_role" -gt 0 ] 2>/dev/null; then
            echo "'Storage Table Data Contributor' already assigned to deploying user" >&2
        else
            role_assign_stderr="$(mktemp "${TMPDIR:-/tmp}/sonde-role-assign.XXXXXX")"
            role_assign_exit=0
            az role assignment create --assignee "$deployer_principal" \
                --role "Storage Table Data Contributor" \
                --scope "$storage_scope" \
                --output none 2>"$role_assign_stderr" || role_assign_exit=$?
            if [ "$role_assign_exit" -eq 0 ]; then
                echo "Assigned 'Storage Table Data Contributor' to deploying user" >&2
            else
                role_assign_error="$(cat "$role_assign_stderr")"
                echo "WARNING: Role assignment failed (exit $role_assign_exit): $role_assign_error" >&2
                echo "  Grant 'Storage Table Data Contributor' manually on storage account $storage_account_name" >&2
            fi
            rm -f "$role_assign_stderr"
        fi
    fi
fi

echo "Web UI deployment complete" >&2

printf '%s\n' "$deployment_outputs"
