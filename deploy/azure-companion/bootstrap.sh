#!/bin/sh
set -eu

if [ "${SONDE_AZURE_COMPANION_IN_CONTAINER:-0}" != "1" ]; then
    image="${SONDE_AZURE_COMPANION_IMAGE:-sonde-azure-companion:local}"
    state_dir="${SONDE_AZURE_COMPANION_STATE_DIR:-$PWD/.sonde-azure-companion}"
    runtime_dir="${SONDE_GATEWAY_RUNTIME_DIR:-/var/run/sonde}"
    socket_path="$runtime_dir/companion.sock"
    container_state_dir="/var/lib/sonde-azure-companion"
    container_runtime_dir="/var/run/sonde"

    mkdir -p "$state_dir"
    if [ ! -d "$runtime_dir" ]; then
        echo "gateway runtime directory not found: $runtime_dir" >&2
        exit 1
    fi
    if [ ! -S "$socket_path" ]; then
        echo "gateway companion socket not found: $socket_path" >&2
        exit 1
    fi
    if [ "$#" -eq 0 ]; then
        if [ -z "${SONDE_AZURE_DEVICE_CLIENT_ID:-}" ]; then
            echo "SONDE_AZURE_DEVICE_CLIENT_ID must be set for bootstrap" >&2
            exit 1
        fi
        if [ -z "${SONDE_AZURE_DEVICE_SCOPES:-}" ]; then
            echo "SONDE_AZURE_DEVICE_SCOPES must be set for bootstrap" >&2
            exit 1
        fi
    fi

    exec docker run --rm \
        --name "${SONDE_AZURE_COMPANION_CONTAINER_NAME:-sonde-azure-companion}" \
        -e SONDE_AZURE_COMPANION_IN_CONTAINER=1 \
        -e SONDE_AZURE_COMPANION_STATE_DIR="$container_state_dir" \
        -e SONDE_GATEWAY_COMPANION_SOCKET="$container_runtime_dir/companion.sock" \
        -e SONDE_AZURE_DEVICE_CLIENT_ID \
        -e SONDE_AZURE_DEVICE_SCOPES \
        -e SONDE_AZURE_DEVICE_POLL_INTERVAL_SECS \
        -e SONDE_AZURE_DEVICE_MAX_ATTEMPTS \
        -v "$state_dir:$container_state_dir" \
        -v "$runtime_dir:$container_runtime_dir" \
        "$image" "$@"
fi

state_dir="${SONDE_AZURE_COMPANION_STATE_DIR:-/var/lib/sonde-azure-companion}"
socket_path="${SONDE_GATEWAY_COMPANION_SOCKET:-/var/run/sonde/companion.sock}"

if [ "$#" -gt 0 ]; then
    exec "$@"
fi

mkdir -p "$state_dir"

sonde-azure-companion \
    --companion-socket "$socket_path" \
    bootstrap-auth \
    --state-dir "$state_dir"

exec sonde-azure-companion --companion-socket "$socket_path" run
