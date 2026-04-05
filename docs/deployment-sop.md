# Sonde End-to-End Deployment SOP

Standard operating procedure for deploying the full sonde stack: modem,
gateway, node, BPF program, and handler.

## Prerequisites

| Item | Purpose |
|------|---------|
| ESP32-S3 dev board (modem) | USB-CDC ↔ ESP-NOW bridge |
| ESP32-C3 dev board (node) | Sensor node running BPF programs |
| Host machine (Linux/Windows) | Gateway service + admin CLI |
| `espflash` CLI | Firmware flashing (`cargo install espflash`) |
| `gh` CLI | Download CI artifacts |
| `clang` with BPF target | Compile BPF programs (see install note below) |

**Installing clang (required for step 9):**
- **Ubuntu/Debian:** `sudo apt install clang`
- **macOS:** `brew install llvm`
- **Windows:** Download from https://releases.llvm.org/ or `winget install LLVM.LLVM`

## 1. Download firmware and installers from CI

Use the latest CI artifacts from your branch (or `main`).

**Linux:**
```sh
BRANCH=main

# Node firmware (ESP32-C3) — quiet (production) and verbose (debug)
gh run download "$(gh run list --branch "$BRANCH" \
  -w 'ESP32-C3 Node Firmware CI' --json databaseId -q '.[0].databaseId')" \
  --name node-firmware --dir ./firmware/
gh run download "$(gh run list --branch "$BRANCH" \
  -w 'ESP32-C3 Node Firmware CI' --json databaseId -q '.[0].databaseId')" \
  --name node-firmware-verbose --dir ./firmware-verbose/

# Modem firmware (ESP32-S3)
gh run download "$(gh run list --branch "$BRANCH" \
  -w 'ESP32-S3 Modem Firmware CI' --json databaseId -q '.[0].databaseId')" \
  --name modem-firmware --dir ./firmware-modem/

# Gateway + admin installer (.deb)
gh run download "$(gh run list --branch "$BRANCH" \
  -w 'Nightly Release' --json databaseId -q '.[0].databaseId')" \
  --name sonde-installer-linux --dir ./installer/

# Pairing tool (.deb package)
gh run download "$(gh run list --branch "$BRANCH" -w 'Tauri Desktop Build' \
  --json databaseId -q '.[0].databaseId')" \
  --name sonde-pair-linux --dir ./pairing-tool/
```

**Windows (PowerShell):**
```powershell
$BRANCH = "main"

# Node firmware (ESP32-C3)
$runId = (gh run list --branch $BRANCH -w "ESP32-C3 Node Firmware CI" --json databaseId -q ".[0].databaseId")
gh run download $runId --name node-firmware --dir .\firmware\
gh run download $runId --name node-firmware-verbose --dir .\firmware-verbose\

# Modem firmware (ESP32-S3)
$runId = (gh run list --branch $BRANCH -w "ESP32-S3 Modem Firmware CI" --json databaseId -q ".[0].databaseId")
gh run download $runId --name modem-firmware --dir .\firmware-modem\

# Gateway + admin installer (.msi)
$runId = (gh run list --branch $BRANCH -w "Nightly Release" --json databaseId -q ".[0].databaseId")
gh run download $runId --name sonde-installer-windows --dir .\installer\

# Pairing tool (NSIS installer)
$runId = (gh run list --branch $BRANCH -w "Tauri Desktop Build" --json databaseId -q ".[0].databaseId")
gh run download $runId --name sonde-pair-windows --dir .\pairing-tool\
```

## 2. Flash modem firmware

Connect the ESP32-S3 modem board via USB.

```sh
espflash write-bin -p PORT 0x0 ./firmware-modem/flash_image.bin
```

Replace `PORT` with the serial port (`/dev/ttyUSB0` on Linux, `COM3` on
Windows). Omit `-p PORT` to auto-detect.

> **Important:** Use `espflash write-bin` at offset `0x0`, NOT `espflash flash`.
> The CI artifact is a merged image (bootloader + partition table + app).
> `espflash flash` substitutes its own bootloader, causing version mismatches.

Verify on UART (115200 baud, the UART port — not USB-CDC):
```
sonde-modem firmware starting (commit xxxxxxx)
WiFi started in station mode
ESP-NOW initialized on channel 1
```

## 3. Flash node firmware

Connect the ESP32-C3 node board via USB. Use the verbose variant for
initial testing:

> **Tip:** Close any open serial monitors (e.g., `espflash monitor`,
> PuTTY, minicom) before flashing — an open monitor holds the port and
> causes "port busy" errors.

```sh
espflash write-bin -p PORT 0x0 ./firmware-verbose/flash_image.bin
```

The node will boot into BLE pairing mode (no PSK in NVS yet).

## 4. Install the gateway

The gateway and admin CLI are distributed as platform installers that
register the gateway as a system service (daemon).

### Linux

```sh
sudo dpkg -i ./installer/sonde_*_amd64.deb
```

The `.deb` package:
- Installs `sonde-gateway` and `sonde-admin` to `/usr/local/bin/`
- Creates a `sonde` system user and group, and adds `sonde` to the `dialout` group (for serial access)
- Installs a systemd service unit (`sonde-gateway.service`)
- Creates `/etc/sonde/` (config) and `/var/lib/sonde/` (database, keys)
- Generates the master key automatically on first start (`--generate-master-key`)
- Enables and starts the service

### Windows

Connect the ESP32-S3 modem board via USB before running the installer so
the modem COM port is auto-detected.

```powershell
msiexec /i .\installer\sonde-x86_64.msi
```

The MSI:
- Installs `sonde-gateway.exe` and `sonde-admin.exe` to `C:\Program Files\Sonde\bin\`
- Adds the `bin` directory to the system `PATH`
- Auto-detects the ESP32-S3 modem COM port (VID `303A` / PID `1001`)
- Creates `%ProgramData%\sonde\` for the database and master key
- Registers `sonde-gateway` as an auto-start Windows service
- Generates the master key automatically on first start (`--generate-master-key`)

For silent/unattended installs, pass the modem port explicitly:
```powershell
msiexec /i .\installer\sonde-x86_64.msi MODEM_PORT=COM5 /quiet
```

> **Note:** The installer requires the modem to be connected. If no
> modem is detected and no `MODEM_PORT` is supplied, the installer will
> abort. Connect the modem and retry, or supply the port manually.

## 5. Configure the gateway

### Linux

The gateway generates its master key automatically on first start and
stores it at `/var/lib/sonde/master-key.hex`. No manual key setup is
required.

If your modem serial port differs from the default (`/dev/ttyUSB0`),
edit `/etc/sonde/environment`:

```sh
sudo nano /etc/sonde/environment
```
```sh
# Serial port for the ESP-NOW modem (required).
SERIAL_PORT=/dev/ttyACM0
```

Then restart the service:

```sh
sudo systemctl restart sonde-gateway
sudo systemctl status sonde-gateway
```

The gateway logs to the systemd journal. Check startup with:

```sh
journalctl -u sonde-gateway -n 30 --no-pager
```

You should see `master key loaded` and `modem transport ready`.

The gateway stores its database and master key under `/var/lib/sonde/`:

| File | Purpose |
|------|---------|
| `gateway.db` | SQLite database (nodes, programs, sessions) |
| `master-key.hex` | 64-hex-char master key (**back this up securely!**) |

### Windows

The MSI configures the service automatically during installation.
The gateway stores its database and master key under
`%ProgramData%\sonde\`:

| File | Purpose |
|------|---------|
| `gateway.db` | SQLite database (nodes, programs, sessions) |
| `master-key.hex` | 64-hex-char master key (**back this up securely!**) |

The service starts automatically after installation and on each boot.
Verify it is running:

```powershell
sc query sonde-gateway
```

Admin socket (configurable via `--admin-socket`):
- **Windows:** `\\.\pipe\sonde-admin` (named pipe)
- **Linux:** `/var/run/sonde/admin.sock` (UDS, created by systemd `RuntimeDirectory`)

## 6. Verify gateway and modem (smoke test)

Before proceeding, confirm the gateway and modem are operational.
With the installers, `sonde-admin` is on your `PATH`:

```sh
sonde-admin modem status
sonde-admin modem scan
sonde-admin node list
sonde-admin program list
```

**Expected results on a fresh deployment:**
- `modem status` — modem connected, firmware version and current RF channel displayed
- `modem scan` — channel/AP table displayed (see step 7 for interpretation)
- `node list` — empty (no nodes provisioned yet)
- `program list` — empty (no programs ingested yet)

> **Tip:** Run these checks any time you suspect a connectivity issue —
> they are a fast way to verify the gateway ↔ modem link is healthy.

## 7. Choose an ESP-NOW channel

ESP-NOW shares the 2.4 GHz band with WiFi. Pick a channel with the
fewest nearby access points to minimize interference:

```sh
sonde-admin modem scan
```

Example output:
```
Channel    APs        Best RSSI
1          2          -91 dBm
2          2          -73 dBm
3          0          0 dBm        ← good choice
6          5          -26 dBm      ← busy, avoid
9          0          0 dBm        ← good choice
```

**How to read it:**
- **APs** = number of WiFi access points visible on that channel
- **Best RSSI** = signal strength of the strongest AP (closer to 0 = stronger = more interference)
- **Pick a channel with 0 APs** — no WiFi traffic to compete with

Set the gateway to your chosen channel (must match the channel used
during node provisioning in the next step):

```sh
sonde-admin modem set-channel 3
```

## 8. Pair a node (BLE provisioning)

> **ℹ️ Channel handling:** During Phase 1 registration, the gateway
> passes its current RF channel to the pairing tool via
> `PHONE_REGISTERED`. The pairing tool pre-fills this channel when
> provisioning nodes, so it will match the modem channel automatically —
> provided you select the channel (step 7) **before** pairing.
> Changing the modem channel **after** nodes are provisioned will strand
> them on the old channel
> ([issue #518](https://github.com/alan-jowett/sonde/issues/518)).

The pairing tool was downloaded in step 1. Install it now if you haven't
already:

- **Linux:** `sudo dpkg -i ./pairing-tool/*.deb`
- **Windows:** Run the NSIS installer from `.\pairing-tool\`:
  ```powershell
  Start-Process ".\pairing-tool\Sonde Pairing Tool_X.X.X_x64-setup.exe"
  ```
  (Replace `X.X.X` with the version number shown in the downloaded filename.)

> **Note:** The Windows pairing tool installer follows the NSIS filename
> pattern `Sonde Pairing Tool_X.X.X_x64-setup.exe`. After installation
> the app is available in the Start Menu as **Sonde Pairing Tool** or at
> `C:\Program Files\Sonde Pairing Tool\Sonde Pairing Tool.exe`.

### Phase 1: Register provisioning device with gateway

Before you can provision nodes, the pairing tool (laptop/phone) must
register itself with the gateway. This is a one-time step per device.

1. **Ensure the gateway service is running** (step 5) with the modem connected.
2. **Open a registration window** from the admin CLI:
   ```sh
   sonde-admin pairing start --duration-s 120
   ```
3. **Launch the pairing tool** on a machine with Bluetooth.
4. The tool connects to the modem's BLE GATT service, authenticates via BLE LESC,
   and receives a phone PSK and RF channel from the gateway.
5. Confirm the passkey to complete registration.

### Phase 2: Provision a node over BLE

Once your provisioning device is registered (Phase 1), you can provision
individual nodes. Repeat this phase for each new node.

> **Windows re-provisioning:** If you are re-provisioning a node that was
> previously paired (e.g., after a factory reset or firmware reflash),
> remove the stale BLE pairing for that node from Windows first:
> **Settings → Bluetooth & devices → Devices**, find the sonde node entry
> corresponding to the device you're re-provisioning, and click **Remove device**.
> Without this step, Windows reuses cached pairing keys that no longer match
> the node and the LESC handshake fails silently.

1. **Launch the pairing tool** (already registered from Phase 1).
2. The tool scans for sonde nodes advertising the pairing service.
3. Select the node, confirm the passkey, and enter a label + RF channel.
   The channel should be pre-filled from the gateway (see note above) —
   **verify it matches the modem channel (step 7) before confirming.**
4. The tool generates a node PSK and provisions the node with PSK,
   key_hint, RF channel, and the encrypted registration payload.
5. The node reboots, sends `PEER_REQUEST`, and the gateway registers it.

Verify registration:

```sh
sonde-admin node list
```

## 9. Compile a BPF program

```sh
cd test-programs
make tmp102_sensor.o
```

Or manually (works on both Linux and Windows with clang installed):
```sh
clang -target bpf -O2 -Wall -Wextra -I. -c tmp102_sensor.c -o tmp102_sensor.o
```

The output is a BPF ELF object file. The gateway converts ELF → CBOR
program image internally (extracting bytecode, map definitions, and
.rodata/.data initial values).

## 10. Deploy the BPF program

```sh
sonde-admin program ingest test-programs/tmp102_sensor.o --profile resident
sonde-admin program assign sensor-01 PROGRAM_HASH
sonde-admin schedule set sensor-01 60
```

Note the program hash from the `ingest` output and use it in the `assign` command.

Profiles:
- `resident` — stored in node flash, runs every wake cycle
- `ephemeral` — one-shot diagnostic, discarded after execution

## 11. Configure a handler

Handlers are external processes that receive `APP_DATA` from nodes via
length-prefixed CBOR on stdin and can reply via stdout. The workspace
includes two reference handlers for TMP102 temperature sensors:

- **Rust** (`sonde-tmp102-handler`) — built with `cargo build`, no runtime
  dependencies. Recommended for deployment.
- **Python** (`test-programs/tmp102_handler.py`) — requires `python3` and the
  `cbor2` pip package. Useful as a readable protocol example.

Build the Rust handler before registering it:

```sh
cargo build -p sonde-tmp102-handler --release
```

The resulting binary is at `target/release/sonde-tmp102-handler`
(`target\release\sonde-tmp102-handler.exe` on Windows).

Use the admin CLI to add a handler while the gateway is running — no
restart required:

```sh
# Rust handler (recommended)
sonde-admin handler add "*" target/release/sonde-tmp102-handler

# Python handler (alternative)
sonde-admin handler add "*" python3 test-programs/tmp102_handler.py
```

The first argument is the program hash to match (or `"*"` for a
catch-all that handles all programs). Additional arguments are passed
to the handler command.

On Windows:
```powershell
sonde-admin handler add "*" .\target\release\sonde-tmp102-handler.exe
sonde-admin handler add "*" python test-programs\tmp102_handler.py
```

The handler writes `temperature_log.jsonl` relative to its working
directory. Use `--working-dir` to control where output lands:

**Optional flags:**
- `--working-dir DIR` — set the handler's working directory
- `--reply-timeout-ms MS` — override the default 30-second reply timeout

**Managing handlers:**

```sh
# List all configured handlers
sonde-admin handler list

# Remove a handler by program hash (or "*" for catch-all)
sonde-admin handler remove "*"
```

Handler configurations are persisted in the gateway database and survive
service restarts.

## 12. Verify end-to-end

```sh
sonde-admin status sensor-01
```

Watch gateway logs for the WAKE/COMMAND cycle:

**Linux:**
```sh
journalctl -u sonde-gateway -f
```

**Windows** (log file — default path is alongside the database):
```powershell
Get-Content "$env:ProgramData\sonde\gateway.log" -Tail 30 -Wait
```

Expected log output:
```
session created node_id=sensor-01 seq=...
WAKE received node_id=sensor-01 seq=... battery_mv=...
COMMAND selected node_id=sensor-01 command_type=UpdateProgram
```

On the node UART (verbose firmware):
```
sonde-node booting (commit xxxxxxx)
boot_reason=deep_sleep_wake (ND-1000)
WAKE sent key_hint=0x.... nonce=0x................ attempt=0 (ND-1002)
COMMAND received command_type=UpdateProgram program_hash=........
BPF execute program_hash=........
BPF execution completed rc=0
entering deep sleep duration_seconds=60 reason=scheduled (ND-1007)
```

## 13. Switch to production firmware

Once verified, flash the quiet (production) firmware:

```sh
espflash write-bin -p PORT 0x0 ./firmware/flash_image.bin
```

The quiet variant strips INFO/DEBUG/TRACE logs at compile time for
minimal power consumption. To debug later, reflash the verbose variant.

## Monitoring

### Linux

The gateway logs to the systemd journal. Useful commands:

```sh
# Follow logs in real time
journalctl -u sonde-gateway -f

# Recent logs
journalctl -u sonde-gateway -n 100 --no-pager

# Logs since last boot
journalctl -u sonde-gateway -b

# Filter by priority (errors and above)
journalctl -u sonde-gateway -p err
```

**Log verbosity** is controlled by the `RUST_LOG` environment variable.
To change the level, edit the environment file and restart:

```sh
# Edit the environment file (preserves ownership/permissions unlike sed -i)
sudoedit /etc/sonde/environment
# In the editor: set the RUST_LOG line to the desired level, e.g.:
#   RUST_LOG=sonde_gateway=info
sudo systemctl restart sonde-gateway
```

### Windows

The gateway service writes to two log sinks:

1. **Log file** — `%ProgramData%\sonde\gateway.log` (or the path set
   via `--log-file`). This is the primary log for day-to-day monitoring.

   ```powershell
   # View recent log entries
   Get-Content "$env:ProgramData\sonde\gateway.log" -Tail 50

   # Follow in real time
   Get-Content "$env:ProgramData\sonde\gateway.log" -Tail 30 -Wait
   ```

2. **ETW (Event Tracing for Windows)** — provider name `sonde-gateway`.
   Use standard ETW tooling (`logman`, `tracelog`, `perfview`, WPA) for
   production diagnostics without touching the log file.

   ```powershell
   # Create a trace session
   logman create trace sonde -p "sonde-gateway" -o sonde-trace.etl

   # Start / stop
   logman start sonde
   logman stop sonde
   ```

**Changing the log level (requires service restart):**

```powershell
# Set the desired log level at the machine scope (persists across restarts)
[Environment]::SetEnvironmentVariable("RUST_LOG", "sonde_gateway=debug", "Machine")

# Restart the service — the gateway reads RUST_LOG at startup, so a
# restart is required for the new value to take effect.
sc stop sonde-gateway
sc start sonde-gateway
```

> **Note:** The gateway does not currently support runtime log-level
> reload via `sc control sonde-gateway paramchange`. Changing
> `RUST_LOG` always requires a service restart.

Release builds default to `sonde_gateway=warn`. Set
`RUST_LOG=sonde_gateway=info` for lifecycle events during initial
testing or `sonde_gateway=debug` for detailed diagnostics.

## Troubleshooting

| Symptom | Check |
|---------|-------|
| Node stuck in BLE pairing mode | No PSK in NVS — pair via BLE (step 8) |
| WAKE timeout (no COMMAND) | Gateway not running, wrong channel, modem not connected |
| `0 APs on all channels` | WiFi scan error — check modem UART for error code |
| Handler not receiving data | Check `sonde-admin handler list` output; verify handler command is executable and on `PATH` |
| `non-ELF program images not accepted` | Release gateway rejects raw CBOR — submit ELF files |
| Windows BLE pairing fails with "Not connected" | Stale Bluetooth cache — open Windows Settings → Bluetooth & devices → Devices, find the modem/node entry, click **Remove device**, then retry pairing from scratch |
| `espflash` "port busy" error | Close any open serial monitor (e.g., `espflash monitor`, PuTTY) before flashing |
| Gateway service won't start (Linux) | Check `journalctl -u sonde-gateway -n 30` — common causes: wrong serial port in `/etc/sonde/environment`, cannot create master key file (check `/var/lib/sonde/` permissions) |
| Gateway service won't start (Windows) | Check `%ProgramData%\sonde\gateway.log` and Event Viewer → Application log — common cause: modem COM port changed (re-run MSI or update service args) |
| `NODE_ACK` indication warning in pairing tool | Non-fatal — the node is provisioned successfully. Verify with `sonde-admin node list` |
| Node not responding after pairing | Verify the node's RF channel matches the modem/gateway channel (step 7). If you changed the modem channel after pairing, re-pair the node on the new channel |
