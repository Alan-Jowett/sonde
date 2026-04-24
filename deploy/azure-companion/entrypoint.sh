#!/bin/sh
set -eu

export SONDE_AZURE_COMPANION_IN_CONTAINER=1
exec /opt/sonde/deploy/azure-companion/bootstrap.sh "$@"
