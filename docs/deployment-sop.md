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
| `clang` with BPF target | Compile BPF programs |

## 1. Download firmware from CI

Use the latest CI artifacts from your branch (or `main`).

**Linux / macOS:**
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

# Gateway + admin binaries
gh run download "$(gh run list --branch "$BRANCH" \
  -w CI --json databaseId -q '.[0].databaseId')" \
  --name gateway-linux-x86_64 --dir ./bin/
chmod +x ./bin/sonde-gateway ./bin/sonde-admin
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

# Gateway + admin binaries
$runId = (gh run list --branch $BRANCH -w CI --json databaseId -q ".[0].databaseId")
gh run download $runId --name gateway-windows-x86_64 --dir .\bin\
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

```sh
espflash write-bin -p PORT 0x0 ./firmware-verbose/flash_image.bin
```

The node will boot into BLE pairing mode (no PSK in NVS yet).

## 4. Start the gateway

Release builds default to WARN-level logging. Set `RUST_LOG` to see
lifecycle events during initial testing:

**Linux / macOS:**
```sh
export RUST_LOG=sonde_gateway=info
./bin/sonde-gateway \
  --port /dev/ttyACM0 \
  --db sonde.db \
  --master-key-file master-key.hex \
  --generate-master-key
```

**Windows (PowerShell):**
```powershell
$env:RUST_LOG = "sonde_gateway=info"
.\bin\sonde-gateway.exe `
  --port COM5 `
  --db sonde.db `
  --master-key-file master-key.hex `
  --generate-master-key
```

> **Note:** `--generate-master-key` is only needed on first run. Once
> `master-key.hex` exists, omit it.

| Flag | Purpose |
|------|---------|
| `--port` | Serial port of the modem's USB-CDC connector |
| `--db` | SQLite database (created if absent) |
| `--master-key-file` | 64-hex-char key file (back this up securely!) |
| `--generate-master-key` | Auto-generate key if file missing |
| `--channel` | ESP-NOW radio channel 1–14 (default 1) — must match the channel chosen during node provisioning (step 6) |
| `--handler-config` | YAML handler routing — add after creating `handlers.yaml` in step 8 |

The gateway logs `modem transport ready` when the modem handshake completes.

Admin socket (configurable via `--admin-socket`):
- Windows: `\\.\pipe\sonde-admin`
- Linux: `/var/run/sonde/admin.sock` (may require root)
- For local/dev runs on macOS or unprivileged Linux, use e.g. `--admin-socket ./admin.sock`
  and point `sonde-admin` at that path with the same flag.

## 5. Choose an ESP-NOW channel

ESP-NOW shares the 2.4 GHz band with WiFi. Pick a channel with the
fewest nearby access points to minimize interference:

**Linux:** `./bin/sonde-admin modem scan`  
**Windows:** `.\bin\sonde-admin.exe modem scan`

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

**Linux:** `./bin/sonde-admin modem set-channel 3`  
**Windows:** `.\bin\sonde-admin.exe modem set-channel 3`

## 6. Pair a node (BLE provisioning)

### Download the pairing tool

**Windows (PowerShell):**
```powershell
$runId = (gh run list --branch $BRANCH -w "Tauri Desktop Build" --json databaseId -q ".[0].databaseId")
gh run download $runId --name sonde-pair-windows --dir .\pairing-tool\
# Run the .exe installer from .\pairing-tool\
```

**Linux:**
```sh
gh run download "$(gh run list --branch "$BRANCH" -w 'Tauri Desktop Build' \
  --json databaseId -q '.[0].databaseId')" \
  --name sonde-pair-linux --dir ./pairing-tool/
# Install the .deb package
sudo dpkg -i ./pairing-tool/*.deb
```

### Phase 1: Register provisioning device with gateway

Before you can provision nodes, the pairing tool (laptop/phone) must
register itself with the gateway. This is a one-time step per device.

1. **Start the gateway** (step 4) — it must be running with the modem connected.
2. **Open a registration window** from the admin CLI:

   **Linux:**
   ```sh
   ./bin/sonde-admin pairing start --duration-s 120
   ```
   **Windows:**
   ```powershell
   .\bin\sonde-admin.exe pairing start --duration-s 120
   ```
3. **Launch the pairing tool** on a machine with Bluetooth.
4. The tool connects to the modem's BLE GATT service, performs ECDH key
   exchange, and receives a phone PSK and RF channel from the gateway.
5. Confirm the passkey to complete registration.

### Phase 2: Provision a node over BLE

Once your provisioning device is registered (Phase 1), you can provision
individual nodes. Repeat this phase for each new node.

1. **Launch the pairing tool** (already registered from Phase 1).
2. The tool scans for sonde nodes advertising the pairing service.
3. Select the node, confirm the passkey, and enter a label + RF channel.
4. The tool generates a node PSK and provisions the node with PSK,
   key_hint, RF channel, and the encrypted registration payload.
5. The node reboots, sends `PEER_REQUEST`, and the gateway registers it.

Verify registration:

**Linux:** `./bin/sonde-admin node list`  
**Windows:** `.\bin\sonde-admin.exe node list`

## 7. Compile a BPF program

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

## 8. Deploy the BPF program

**Linux:**
```sh
./bin/sonde-admin program ingest test-programs/tmp102_sensor.o --profile resident
./bin/sonde-admin program assign my-node-001 PROGRAM_HASH
./bin/sonde-admin schedule set my-node-001 60
```

**Windows (PowerShell):**
```powershell
.\bin\sonde-admin.exe program ingest test-programs\tmp102_sensor.o --profile resident
.\bin\sonde-admin.exe program assign my-node-001 PROGRAM_HASH
.\bin\sonde-admin.exe schedule set my-node-001 60
```

Note the program hash from the `ingest` output and use it in the `assign` command.

Profiles:
- `resident` — stored in node flash, runs every wake cycle
- `ephemeral` — one-shot diagnostic, discarded after execution

## 9. Configure a handler

Create `handlers.yaml`:

```yaml
handlers:
  - program_hash: "*"
    command: "python3"
    args: ["test-programs/tmp102_handler.py"]
```

On Windows, use `python` instead of `python3` if that's how Python is
installed.

Handlers receive `APP_DATA` from nodes via length-prefixed CBOR on stdin
and can reply via stdout. See `test-programs/tmp102_handler.py` for a
working example.

Restart the gateway with `--handler-config handlers.yaml`:

**Linux:**
```sh
./bin/sonde-gateway \
  --port /dev/ttyACM0 \
  --db sonde.db \
  --master-key-file master-key.hex \
  --handler-config handlers.yaml
```

**Windows:**
```powershell
.\bin\sonde-gateway.exe `
  --port COM5 `
  --db sonde.db `
  --master-key-file master-key.hex `
  --handler-config handlers.yaml
```

## 10. Verify end-to-end

**Linux:** `./bin/sonde-admin status my-node-001`  
**Windows:** `.\bin\sonde-admin.exe status my-node-001`

Watch gateway logs for the WAKE/COMMAND cycle:
```
session created node_id=my-node-001 seq=...
WAKE received node_id=my-node-001 seq=... battery_mv=...
COMMAND selected node_id=my-node-001 command_type=UpdateProgram
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

## 11. Switch to production firmware

Once verified, flash the quiet (production) firmware:

```sh
espflash write-bin -p PORT 0x0 ./firmware/flash_image.bin
```

The quiet variant strips INFO/DEBUG/TRACE logs at compile time for
minimal power consumption. To debug later, reflash the verbose variant.

## Troubleshooting

| Symptom | Check |
|---------|-------|
| Node stuck in BLE pairing mode | No PSK in NVS — pair via BLE (step 6) |
| WAKE timeout (no COMMAND) | Gateway not running, wrong channel, modem not connected |
| `0 APs on all channels` | WiFi scan error — check modem UART for error code |
| Handler not receiving data | Check `handlers.yaml` path, ensure handler is executable |
| `non-ELF program images not accepted` | Release gateway rejects raw CBOR — submit ELF files |
