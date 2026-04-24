#!/bin/sh
set -eu

if [ "${SONDE_AZURE_COMPANION_IN_CONTAINER:-0}" != "1" ]; then
    image="${SONDE_AZURE_COMPANION_IMAGE:-sonde-azure-companion:local}"
    state_dir="${SONDE_AZURE_COMPANION_STATE_DIR:-$PWD/.sonde-azure-companion}"
    runtime_dir="${SONDE_GATEWAY_RUNTIME_DIR:-/var/run/sonde}"
    container_state_dir="/var/lib/sonde-azure-companion"
    container_runtime_dir="/var/run/sonde"

    mkdir -p "$state_dir"
    if [ ! -d "$runtime_dir" ]; then
        echo "gateway runtime directory not found: $runtime_dir" >&2
        exit 1
    fi

    exec docker run --rm \
        --name "${SONDE_AZURE_COMPANION_CONTAINER_NAME:-sonde-azure-companion}" \
        -e SONDE_AZURE_COMPANION_IN_CONTAINER=1 \
        -e SONDE_AZURE_COMPANION_STATE_DIR="$container_state_dir" \
        -e SONDE_GATEWAY_COMPANION_SOCKET="$container_runtime_dir/companion.sock" \
        -v "$state_dir:$container_state_dir" \
        -v "$runtime_dir:$container_runtime_dir" \
        "$image" "$@"
fi

state_dir="${SONDE_AZURE_COMPANION_STATE_DIR:-/var/lib/sonde-azure-companion}"
socket_path="${SONDE_GATEWAY_COMPANION_SOCKET:-/var/run/sonde/companion.sock}"
azure_config_dir="$state_dir/azure"
pipe_path="$state_dir/az-login.pipe.$$"

mkdir -p "$azure_config_dir"
export AZURE_CONFIG_DIR="$azure_config_dir"

has_auth_state() {
    [ -f "$AZURE_CONFIG_DIR/msal_token_cache.json" ] || [ -f "$AZURE_CONFIG_DIR/accessTokens.json" ]
}

cleanup() {
    rm -f "$pipe_path"
}

trap cleanup EXIT INT TERM

if has_auth_state; then
    exec sonde-azure-companion --companion-socket "$socket_path" run
fi

rm -f "$pipe_path"
mkfifo "$pipe_path"

az login --use-device-code >"$pipe_path" 2>&1 &
az_pid=$!
shown=0

while IFS= read -r line; do
    printf '%s\n' "$line"
    if [ "$shown" -eq 0 ]; then
        device_code="$(printf '%s\n' "$line" | sed -nE 's/.*[Cc]ode ([A-Z0-9-]+).*/\1/p')"
        if [ -n "$device_code" ]; then
            if ! sonde-azure-companion --companion-socket "$socket_path" display-message "Azure login" "$device_code"; then
                kill "$az_pid" 2>/dev/null || true
                wait "$az_pid" 2>/dev/null || true
                echo "failed to display Azure device code on the modem; retry when the display is available" >&2
                exit 1
            fi
            shown=1
        fi
    fi
done <"$pipe_path"

if wait "$az_pid"; then
    :
else
    status=$?
    exit "$status"
fi

if [ "$shown" -eq 0 ]; then
    echo "Azure device-code login succeeded but no device code was extracted for modem display" >&2
    exit 1
fi

exec sonde-azure-companion --companion-socket "$socket_path" run
