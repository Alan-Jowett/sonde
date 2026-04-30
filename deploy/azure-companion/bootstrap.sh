#!/bin/sh
set -eu

if [ "${SONDE_AZURE_COMPANION_IN_CONTAINER:-0}" != "1" ]; then
    image="${SONDE_AZURE_COMPANION_IMAGE:-sonde-azure-companion:local}"
    state_dir="${SONDE_AZURE_COMPANION_STATE_DIR:-$PWD/.sonde-azure-companion}"
    runtime_dir="${SONDE_GATEWAY_RUNTIME_DIR:-/var/run/sonde}"
    admin_socket_path="$runtime_dir/admin.sock"
    connector_socket_path="$runtime_dir/connector.sock"
    container_state_dir="/var/lib/sonde-azure-companion"
    container_admin_socket_path="/var/run/sonde/admin.sock"
    container_connector_socket_path="/var/run/sonde/connector.sock"

    mkdir -p "$state_dir"
    if [ ! -d "$runtime_dir" ]; then
        echo "gateway runtime directory not found: $runtime_dir" >&2
        exit 1
    fi
    if [ ! -S "$admin_socket_path" ]; then
        echo "gateway admin socket not found: $admin_socket_path" >&2
        exit 1
    fi
    if [ ! -S "$connector_socket_path" ]; then
        echo "gateway connector socket not found: $connector_socket_path" >&2
        exit 1
    fi

    exec docker run --rm \
        --name "${SONDE_AZURE_COMPANION_CONTAINER_NAME:-sonde-azure-companion}" \
        -e SONDE_AZURE_COMPANION_IN_CONTAINER=1 \
        -e SONDE_AZURE_COMPANION_STATE_DIR="$container_state_dir" \
        -e SONDE_GATEWAY_ADMIN_SOCKET="$container_admin_socket_path" \
        -e SONDE_GATEWAY_CONNECTOR_SOCKET="$container_connector_socket_path" \
        -e SONDE_AZURE_SERVICEBUS_NAMESPACE \
        -e SONDE_AZURE_SERVICEBUS_UPSTREAM_QUEUE \
        -e SONDE_AZURE_SERVICEBUS_DOWNSTREAM_QUEUE \
        -e SONDE_AZURE_LOCATION \
        -e SONDE_AZURE_PROJECT_NAME \
        -e SONDE_AZURE_SUBSCRIPTION_ID \
        -v "$state_dir:$container_state_dir" \
        -v "$admin_socket_path:$container_admin_socket_path" \
        -v "$connector_socket_path:$container_connector_socket_path" \
        -v /var/run/docker.sock:/var/run/docker.sock \
        "$image" "$@"
fi

state_dir="${SONDE_AZURE_COMPANION_STATE_DIR:-/var/lib/sonde-azure-companion}"
admin_socket_path="${SONDE_GATEWAY_ADMIN_SOCKET:-/var/run/sonde/admin.sock}"
connector_socket_path="${SONDE_GATEWAY_CONNECTOR_SOCKET:-/var/run/sonde/connector.sock}"

if [ "$#" -gt 0 ]; then
    exec "$@"
fi

mkdir -p "$state_dir"

check_runtime_ready_with_log() {
    runtime_ready_log="$(mktemp "${TMPDIR:-/tmp}/sonde-azure-companion-runtime-ready.XXXXXX")"
    if sonde-azure-companion \
        --admin-socket "$admin_socket_path" \
        --connector-socket "$connector_socket_path" \
        --state-dir "$state_dir" \
        check-runtime-ready >"$runtime_ready_log" 2>&1; then
        rm -f "$runtime_ready_log"
        return 0
    fi

    if [ -s "$runtime_ready_log" ]; then
        cat "$runtime_ready_log" >&2 || true
    fi
    rm -f "$runtime_ready_log"
    return 1
}

if sonde-azure-companion \
    --admin-socket "$admin_socket_path" \
    --connector-socket "$connector_socket_path" \
    --state-dir "$state_dir" \
    check-runtime-ready >/dev/null 2>&1; then
    exec sonde-azure-companion \
        --admin-socket "$admin_socket_path" \
        --connector-socket "$connector_socket_path" \
        --state-dir "$state_dir" \
        run
fi

bootstrap_pid=

forward_signal() {
    signal="$1"
    if [ -n "${bootstrap_pid:-}" ]; then
        kill "-$signal" "$bootstrap_pid" 2>/dev/null || true
        wait "$bootstrap_pid" 2>/dev/null || true
    fi
}

trap 'forward_signal TERM; exit 143' TERM
trap 'forward_signal INT; exit 130' INT

sonde-azure-companion \
    --admin-socket "$admin_socket_path" \
    --connector-socket "$connector_socket_path" \
    --state-dir "$state_dir" \
    bootstrap &
bootstrap_pid=$!

if wait "$bootstrap_pid"; then
    bootstrap_status=0
else
    bootstrap_status=$?
fi

trap - TERM INT
bootstrap_pid=

if [ "$bootstrap_status" -ne 0 ]; then
    exit "$bootstrap_status"
fi

if ! check_runtime_ready_with_log; then
    echo "bootstrap completed, but runtime state is still incomplete" >&2
    exit 1
fi

exec sonde-azure-companion \
    --admin-socket "$admin_socket_path" \
    --connector-socket "$connector_socket_path" \
    --state-dir "$state_dir" \
    run
