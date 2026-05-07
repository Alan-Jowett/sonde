#!/bin/sh
set -eu

script_dir="$(CDPATH= cd -- "$(dirname "$0")" && pwd)"
repo_root="$(CDPATH= cd -- "$script_dir/../.." && pwd)"
artifact_dir="${1:-$repo_root/.artifacts/azure-handler-function}"
target_dir="${CARGO_TARGET_DIR:-$repo_root/target}"
package_path="$artifact_dir/sonde-azure-handler-function.zip"
staging_dir="$(mktemp -d "${TMPDIR:-/tmp}/sonde-azure-handler-package.XXXXXX")"

cleanup() {
    rm -rf "$staging_dir"
}
trap cleanup EXIT INT TERM

mkdir -p "$artifact_dir" "$staging_dir/UpstreamConnector"

cargo build --locked --release -p sonde-azure-handler --manifest-path "$repo_root/Cargo.toml"

cp "$repo_root/crates/sonde-azure-handler/function-app/host.json" "$staging_dir/host.json"
cp "$repo_root/crates/sonde-azure-handler/function-app/UpstreamConnector/function.json" \
    "$staging_dir/UpstreamConnector/function.json"
cp "$target_dir/release/sonde-azure-handler" "$staging_dir/sonde-azure-handler"
chmod 0755 "$staging_dir/sonde-azure-handler"

rm -f "$package_path"
(
    cd "$staging_dir"
    zip -q -r "$package_path" .
)

printf '%s\n' "$package_path"
