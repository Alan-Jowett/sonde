<!-- SPDX-License-Identifier: MIT -->
<!-- Copyright (c) 2026 sonde contributors -->

# Sonde Docker Compose Deployment

Single-command deployment of the sonde-gateway and Azure companion stack.

## Prerequisites

- **Docker Engine** ≥ 24.0 with Compose V2 (`docker compose`)
- **USB modem** (ESP32-S3) connected to the host
- **Azure subscription** (for the bootstrap step)

## Quick Start

```bash
cd deploy/docker-compose

# 1. Create your environment file
cp .env.example .env
# Edit .env — at minimum set SONDE_MODEM_PORT if it differs from /dev/ttyACM0

# 2. Start the stack
docker compose up -d

# 3. Follow the bootstrap progress
docker compose logs -f bootstrap
```

On first run, the bootstrap service will:
1. Wait for the gateway to become healthy
2. Generate an ECDSA certificate for Azure authentication
3. Display an Azure device-code login prompt on the modem's screen
4. Deploy Azure infrastructure via Bicep templates
5. Write state files for the companion service

Once bootstrap completes, the companion service starts automatically and
maintains the cloud connection.

## Architecture

```
┌─────────────────────┐
│      gateway        │  ← Manages sensor nodes over ESP-NOW radio
│  (sonde-gateway)    │
│                     │
│  UDS: admin.sock    │──┐
│  UDS: connector.sock│──┤
└─────────────────────┘  │
                         │  shared via sonde-runtime volume
┌─────────────────────┐  │
│     bootstrap       │──┘ (reads admin.sock)
│  (one-shot)         │
│                     │──── Docker socket (runs azure-bootstrap container)
│  Writes state to:   │
│  sonde-companion-   │
│  state volume       │
└─────────────────────┘
         │
         │ service_completed_successfully
         ▼
┌─────────────────────┐
│     companion       │──── connector.sock (from sonde-runtime volume)
│  (long-running)     │
│                     │──── state files (from sonde-companion-state volume)
│  Azure queue bridge │
└─────────────────────┘
```

## Services

| Service | Image | Role |
|---------|-------|------|
| `gateway` | `ghcr.io/alan-jowett/sonde-gateway` | ESP-NOW radio gateway, gRPC admin API |
| `bootstrap` | `ghcr.io/alan-jowett/sonde-azure-companion` | One-shot Azure provisioning |
| `companion` | `ghcr.io/alan-jowett/sonde-azure-companion` | Long-running Azure queue bridge |

## Volumes

| Volume | Purpose |
|--------|---------|
| `sonde-data` | Gateway database and master key (persistent) |
| `sonde-runtime` | Unix domain sockets for inter-service IPC |
| `sonde-companion-state` | Bootstrap output (certs, queue config) |

## Environment Variables

See [`.env.example`](.env.example) for the full list with documentation.

### Required for first run

| Variable | Description |
|----------|-------------|
| `SONDE_MODEM_PORT` | Host serial port for the modem (default: `/dev/ttyACM0`) |
| `SONDE_AZURE_LOCATION` | Azure region (default: `eastus`) |
| `SONDE_AZURE_PROJECT_NAME` | Azure resource prefix (default: `sonde`) |

### Optional

| Variable | Description |
|----------|-------------|
| `SONDE_IMAGE_TAG` | Container image tag (default: `latest`) |
| `SONDE_MODEM_GID` | Host group for modem device access (default: `dialout`) |
| `SONDE_ESPNOW_CHANNEL` | ESP-NOW radio channel 1–14 (default: `1`) |
| `SONDE_AZURE_SUBSCRIPTION_ID` | Azure subscription override |
| `SONDE_AZURE_BOOTSTRAP_IMAGE` | Bootstrap container image override |

## Common Operations

```bash
# View all service logs
docker compose logs -f

# Restart the gateway (companion reconnects automatically)
docker compose restart gateway

# Re-run bootstrap (e.g., after Azure config changes)
docker compose up bootstrap

# Stop everything
docker compose down

# Stop and remove all data (fresh start)
docker compose down -v
```

## Troubleshooting

### Gateway fails to start

- **Permission denied on serial port**: Check `SONDE_MODEM_GID` matches
  the host group that owns the modem device:
  ```bash
  stat -c '%G (%g)' /dev/ttyACM0
  ```
- **Modem not found**: Verify the device path in `SONDE_MODEM_PORT` and
  that the modem is plugged in.

### Bootstrap hangs

- The bootstrap step requires interactive Azure login via device code.
  The device code is displayed on the modem's screen. Check
  `docker compose logs bootstrap` for the URL and code if needed.

### Security: Docker socket access

The bootstrap service mounts `/var/run/docker.sock` because it needs to
pull and run the `sonde-azure-bootstrap` container internally. This grants
the bootstrap process host-level Docker control. For locked-down
environments, consider running the bootstrap step manually outside of
Compose and then starting only the gateway + companion services.

### Companion fails to connect

- Ensure bootstrap completed successfully:
  ```bash
  docker compose ps bootstrap
  ```
- Check that the companion state volume has the expected files:
  ```bash
  docker compose exec companion ls -la /var/lib/sonde-azure-companion/
  ```
