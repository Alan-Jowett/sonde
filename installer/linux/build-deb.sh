#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 sonde contributors
#
# build-deb.sh — Build a Debian (.deb) package for sonde-gateway and sonde-admin.
#
# Usage:
#   ./installer/linux/build-deb.sh [--arch ARCH] [--version VERSION]
#
# Requirements: dpkg-deb (from dpkg), fakeroot
#
# The script expects the release binaries to have been built already:
#   cargo build --release -p sonde-gateway -p sonde-admin
#
# Output: sonde_<VERSION>_<ARCH>.deb in the current working directory.

set -euo pipefail

ARCH="amd64"
VERSION="0.1.0"
WORKSPACE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TARGET_DIR="${WORKSPACE_ROOT}/target/release"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --arch)    ARCH="$2";    shift 2 ;;
        --version) VERSION="$2"; shift 2 ;;
        *) echo "Unknown argument: $1" >&2; exit 1 ;;
    esac
done

PACKAGE="sonde"
PKG_DIR="$(mktemp -d)"
trap 'rm -rf "$PKG_DIR"' EXIT

# ── Directory layout ──────────────────────────────────────────────────────────
install -d \
    "${PKG_DIR}/usr/local/bin" \
    "${PKG_DIR}/lib/systemd/system" \
    "${PKG_DIR}/etc/sonde" \
    "${PKG_DIR}/var/lib/sonde" \
    "${PKG_DIR}/DEBIAN"

# ── Binaries ──────────────────────────────────────────────────────────────────
install -m 755 "${TARGET_DIR}/sonde-gateway" "${PKG_DIR}/usr/local/bin/sonde-gateway"
install -m 755 "${TARGET_DIR}/sonde-admin"   "${PKG_DIR}/usr/local/bin/sonde-admin"

# ── systemd unit ──────────────────────────────────────────────────────────────
install -m 644 \
    "${WORKSPACE_ROOT}/installer/linux/sonde-gateway.service" \
    "${PKG_DIR}/lib/systemd/system/sonde-gateway.service"

# ── DEBIAN control files ──────────────────────────────────────────────────────
cat > "${PKG_DIR}/DEBIAN/control" <<EOF
Package: ${PACKAGE}
Version: ${VERSION}
Architecture: ${ARCH}
Maintainer: sonde contributors <https://github.com/Alan-Jowett/sonde>
Description: Sonde gateway and admin tools
 sonde-gateway is the radio gateway service that authenticates sensor nodes,
 distributes BPF programs, and routes telemetry data.
 sonde-admin is the command-line administration tool for the gateway.
Depends: libc6
Section: net
Priority: optional
EOF

cat > "${PKG_DIR}/DEBIAN/conffiles" <<EOF
/etc/sonde/gateway.yaml
EOF

# ── postinst ──────────────────────────────────────────────────────────────────
cat > "${PKG_DIR}/DEBIAN/postinst" <<'POSTINST'
#!/bin/sh
set -e

# Create sonde system user/group if they do not exist.
if ! getent group sonde > /dev/null 2>&1; then
    addgroup --system sonde
fi
if ! getent passwd sonde > /dev/null 2>&1; then
    adduser --system --ingroup sonde --no-create-home \
            --shell /usr/sbin/nologin sonde
    # Note: /var/lib/sonde is created below with correct ownership.
fi

# Add sonde to the dialout group for serial port access.
usermod -aG dialout sonde 2>/dev/null || true

# Create config and data directories.
install -d -o sonde -g sonde -m 750 /etc/sonde
install -d -o sonde -g sonde -m 750 /var/lib/sonde

# Write a default config file only if one does not exist yet.
if [ ! -f /etc/sonde/gateway.yaml ]; then
    cat > /etc/sonde/gateway.yaml <<'YAML'
# Sonde gateway configuration
# See https://github.com/Alan-Jowett/sonde for documentation.

# db: /var/lib/sonde/gateway.db
# listen: "[::]:5683"
YAML
    chown sonde:sonde /etc/sonde/gateway.yaml
    chmod 640 /etc/sonde/gateway.yaml
fi

# Enable and start systemd service when installed under systemd.
if [ -d /run/systemd/system ]; then
    systemctl daemon-reload || true
    systemctl enable sonde-gateway.service || true
    systemctl start sonde-gateway.service || true
fi

exit 0
POSTINST
chmod 755 "${PKG_DIR}/DEBIAN/postinst"

# ── prerm ─────────────────────────────────────────────────────────────────────
cat > "${PKG_DIR}/DEBIAN/prerm" <<'PRERM'
#!/bin/sh
set -e
if [ -d /run/systemd/system ]; then
    systemctl stop sonde-gateway.service || true
    systemctl disable sonde-gateway.service || true
fi
exit 0
PRERM
chmod 755 "${PKG_DIR}/DEBIAN/prerm"

# ── Build the package ─────────────────────────────────────────────────────────
DEB_NAME="${PACKAGE}_${VERSION}_${ARCH}.deb"
fakeroot dpkg-deb --build "${PKG_DIR}" "${DEB_NAME}"
echo "Package built: ${DEB_NAME}"
