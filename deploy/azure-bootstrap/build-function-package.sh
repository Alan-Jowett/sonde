#!/bin/sh
set -eu

script_dir="$(CDPATH= cd -- "$(dirname "$0")" && pwd)"
repo_root="$(CDPATH= cd -- "$script_dir/../.." && pwd)"
artifact_dir="${1:-$repo_root/.artifacts/azure-handler-function}"
package_path="$artifact_dir/sonde-azure-handler-function.zip"

mkdir -p "$artifact_dir"

# Build the handler binary inside an Alpine container to produce a
# statically-linked musl binary that runs on any Linux host (including
# the Azure Functions Consumption runtime, which ships an older glibc).
# Use `docker buildx build` explicitly so BuildKit output export works
# regardless of the host Docker configuration.  Pin --platform to
# linux/amd64 so the binary matches the Azure Functions x86_64 runtime
# even when the script is run on ARM hosts.
docker buildx build \
    --platform linux/amd64 \
    -f "$repo_root/.github/docker/Dockerfile.azure-handler-builder" \
    -o "type=local,dest=$artifact_dir" \
    "$repo_root"

if [ ! -f "$package_path" ]; then
    echo "built handler package not found: $package_path" >&2
    exit 1
fi

printf '%s\n' "$package_path"
