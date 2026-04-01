# Wokwi Firmware Simulation

[Wokwi](https://wokwi.com/) provides full-chip simulation for ESP32 targets,
including BLE, WiFi, and peripheral emulation — features that QEMU cannot
currently emulate (QEMU asserts during BLE/WiFi init).

## Directory layout

```
wokwi/
├── node/          # ESP32-C3 node firmware simulation
│   ├── wokwi.toml
│   └── diagram.json
├── modem/         # ESP32-S3 modem firmware simulation
│   ├── wokwi.toml
│   └── diagram.json
└── README.md      # this file
```

Each subdirectory contains:

| File             | Purpose                                                  |
|------------------|----------------------------------------------------------|
| `wokwi.toml`    | Points Wokwi at the firmware binary and (optional) ELF  |
| `diagram.json`  | Defines the simulated board and wiring                   |

## How it works

The GitHub Actions workflow (`.github/workflows/wokwi-smoke.yml`) runs on every
push to `main` and on PRs that touch node, modem, or protocol code:

1. **Downloads** the latest `flash_image.bin` from the firmware CI workflows
   (`ESP32-C3 Node Firmware CI` / `ESP32-S3 Modem Firmware CI`).
2. **Launches** Wokwi simulation using
   [`wokwi/wokwi-ci-action@v1`](https://github.com/wokwi/wokwi-ci-action).
3. **Watches** serial output for boot markers:
   - Node: `sonde-node booting`
   - Modem: `sonde-modem firmware starting`
4. **Fails** immediately if `panic` appears in the serial output.
5. **Uploads** the full serial log as a workflow artifact for debugging.

## Boot markers

These strings are emitted by the firmware during startup and used as
pass/fail criteria:

| Target   | Pass marker (`expect_text`)          | Fail marker (`fail_text`) |
|----------|--------------------------------------|---------------------------|
| Node     | `sonde-node booting`                 | `panic`                   |
| Modem    | `sonde-modem firmware starting`      | `panic`                   |

## Prerequisites

### `WOKWI_CLI_TOKEN` secret

The workflow requires a **Wokwi CLI token** stored as a GitHub Actions secret
named `WOKWI_CLI_TOKEN`.

1. Sign in at <https://wokwi.com/dashboard/ci>.
2. Generate a new CI token.
3. Add it as a repository secret:
   **Settings → Secrets and variables → Actions → New repository secret**
   - Name: `WOKWI_CLI_TOKEN`
   - Value: *(paste token)*

> **Forks:** The workflow has an `if` guard that skips Wokwi jobs when the
> secret is missing, so fork CI will not fail — it will simply skip the
> simulation step.

### Firmware artifacts

The Wokwi workflow does **not** build firmware itself. It downloads
`flash_image.bin` from the most recent successful run of:

- `ESP32-C3 Node Firmware CI` (uploads `node-firmware`)
- `ESP32-S3 Modem Firmware CI` (uploads `modem-firmware`)

Make sure these workflows have completed at least once on the branch (or
`main`) before the Wokwi workflow runs.

## Running locally

### Install the Wokwi CLI

```bash
curl -L https://wokwi.com/ci/install.sh | sh
# or via npm:
npm i -g @wokwi/wokwi-cli
```

### Authenticate

```bash
wokwi-cli auth   # opens browser for one-time login
```

### Run the simulation

```bash
# Ensure firmware/flash_image.bin is up to date.
# Option A: download from CI
BRANCH=$(git branch --show-current)
gh run download "$(gh run list --branch "$BRANCH" \
  -w 'ESP32-C3 Node Firmware CI' --json databaseId -q '.[0].databaseId')" \
  --name node-firmware --dir ./firmware/

# Option B: build locally (requires ESP Docker image)
docker run --rm -v "$(pwd)":/sonde -w /sonde \
  ghcr.io/alan-jowett/sonde-esp-dev:latest \
  cargo +esp build -p sonde-node --bin node --features esp \
  --profile firmware --target riscv32imc-esp-espidf \
  -Zbuild-std=std,panic_abort

# Run the Wokwi simulation
cd wokwi/node
wokwi-cli --timeout 30000 \
  --expect-text 'sonde-node booting' \
  --fail-text 'panic'
```

## Known limitations

- **No radio interaction**: Wokwi simulates the BLE/WiFi hardware, but
  the node and modem run in separate simulations with no shared radio
  medium. End-to-end ESP-NOW tests are not possible yet.
- **Timeout tuning**: The 30-second timeout is conservative. If the
  firmware initialization expands, the timeout may need adjustment.
- **ELF optional**: The `elf` path in `wokwi.toml` is optional. The
  simulation works with just the merged `flash_image.bin`. The ELF
  enables symbol-level debugging in the Wokwi VS Code extension but is
  not uploaded by the firmware CI workflows.
- **Secret required**: The simulation only runs when `WOKWI_CLI_TOKEN`
  is configured. Fork PRs will skip the Wokwi jobs silently.
