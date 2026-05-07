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
    query='[[properties.outputs.resourceGroupName.value, properties.outputs.functionAppName.value, properties.outputs.deploymentContainerName.value, properties.outputs.deploymentContainerUrl.value]]'
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
    if [ "$field_count" -ne 4 ]; then
        echo "deployment output query \`$query\` returned $field_count field(s); expected 4 tab-separated values" >&2
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
echo "__SONDE_AZURE_DEPLOYMENT_START__" >&2
deployment_name="sonde-bootstrap-$(date +%Y%m%d%H%M%S)-$$"
deployment_outputs="$(az deployment sub create \
    --name "$deployment_name" \
    --location "$SONDE_AZURE_LOCATION" \
    --template-file /opt/sonde/deploy/bicep/main.bicep \
    --parameters companionCertificateBase64="$COMPANION_CERT_BASE64" \
    --parameters location="$SONDE_AZURE_LOCATION" \
    --parameters project_name="$SONDE_AZURE_PROJECT_NAME" \
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
az functionapp deployment source config-zip \
    --src "$function_package_path" \
    --name "$function_app_name" \
    --resource-group "$resource_group_name" \
    --timeout "$deployment_timeout_secs" \
    --output none 1>&2

wait_for_function_activation "$resource_group_name" "$function_app_name" "$activation_timeout_secs"

printf '%s\n' "$deployment_outputs"
