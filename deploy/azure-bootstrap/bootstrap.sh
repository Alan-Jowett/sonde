#!/bin/sh
set -eu

json_output_string() {
    field="$1"
    printf '%s' "$deployment_outputs" | python3 - "$field" <<'PY'
import json
import sys

field = sys.argv[1]
data = json.load(sys.stdin)

try:
    if field == "resourceGroupName":
        value = data["resourceGroupName"]["value"]
    else:
        companion_values = data["companionBootstrapValues"]["value"]
        value = companion_values[field]
except (KeyError, TypeError):
    if not isinstance(data, dict):
        raise SystemExit("deployment outputs must be a JSON object")
    if field == "resourceGroupName":
        available_keys = ", ".join(sorted(data.keys())) or "(none)"
        raise SystemExit(
            f"missing deployment output `{field}`; available top-level outputs: {available_keys}"
        )
    companion_output = data.get("companionBootstrapValues")
    if isinstance(companion_output, dict):
        companion_values = companion_output.get("value")
    else:
        companion_values = None
    available_keys = (
        ", ".join(sorted(companion_values.keys()))
        if isinstance(companion_values, dict)
        else "(none)"
    )
    raise SystemExit(
        f"missing deployment output `{field}` in companionBootstrapValues; available keys: {available_keys}"
    )

if not isinstance(value, str) or not value.strip():
    raise SystemExit(f"deployment output `{field}` must be a non-empty string")

print(value.strip())
PY
}

wait_for_function_activation() {
    resource_group_name="$1"
    function_app_name="$2"
    timeout_secs="${SONDE_AZURE_FUNCTION_ACTIVATION_TIMEOUT_SECS:-300}"
    deadline="$(( $(date +%s) + timeout_secs ))"

    while :; do
        function_list_stderr="$(mktemp "${TMPDIR:-/tmp}/sonde-azure-function-list.XXXXXX")"
        if function_list_json="$(az functionapp function list \
            --name "$function_app_name" \
            --resource-group "$resource_group_name" \
            --output json 2>"$function_list_stderr")"; then
            if [ -s "$function_list_stderr" ]; then
                cat "$function_list_stderr" >&2
            fi
            rm -f "$function_list_stderr"
            loaded_count="$(printf '%s' "$function_list_json" | python3 - <<'PY'
import json
import sys

print(len(json.load(sys.stdin)))
PY
)"
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

az login --use-device-code --output none >&2
if [ -n "${SONDE_AZURE_SUBSCRIPTION_ID:-}" ]; then
    az account set --subscription "$SONDE_AZURE_SUBSCRIPTION_ID" >&2
fi
echo "__SONDE_AZURE_DEPLOYMENT_START__" >&2
deployment_outputs="$(az deployment sub create \
    --location "$SONDE_AZURE_LOCATION" \
    --template-file /opt/sonde/deploy/bicep/main.bicep \
    --parameters companionCertificateBase64="$COMPANION_CERT_BASE64" \
    --parameters location="$SONDE_AZURE_LOCATION" \
    --parameters project_name="$SONDE_AZURE_PROJECT_NAME" \
    --query 'properties.outputs' \
    --output json)"

resource_group_name="$(json_output_string resourceGroupName)"
function_app_name="$(json_output_string functionAppName)"
deployment_container_name="$(json_output_string deploymentContainerName)"
deployment_container_url="$(json_output_string deploymentContainerUrl)"
function_package_path='/opt/sonde/deploy/azure-handler/sonde-azure-handler-function.zip'
deployment_timeout_secs="${SONDE_AZURE_FUNCTION_DEPLOY_TIMEOUT_SECS:-600}"

if [ ! -r "$function_package_path" ]; then
    echo "bundled Azure handler package not found: $function_package_path" >&2
    exit 1
fi

echo "Deploying bundled Azure handler package to Function App $function_app_name" >&2
echo "Using deployment target $deployment_container_name ($deployment_container_url)" >&2
az functionapp deployment source config-zip \
    --src "$function_package_path" \
    --name "$function_app_name" \
    --resource-group "$resource_group_name" \
    --timeout "$deployment_timeout_secs" \
    --output none 1>&2

wait_for_function_activation "$resource_group_name" "$function_app_name"

printf '%s\n' "$deployment_outputs"
