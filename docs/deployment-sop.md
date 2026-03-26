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

Use the latest CI artifacts from your branch (or `main`):

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

# Gateway + admin binaries (Linux)
gh run download "$(gh run list --branch "$BRANCH" \
  -w CI --json databaseId -q '.[0].databaseId')" \
  --name gateway-linux-x86_64 --dir ./bin/
chmod +x ./bin/sonde-gateway ./bin/sonde-admin
```

On Windows, download `gateway-windows-x86_64` instead.

## 2. Flash modem firmware

Connect the ESP32-S3 modem board via USB.

```sh
espflash write-bin -p PORT 0x0 ./firmware-modem/flash_image.bin
```

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

```sh
# Generate a master key on first run
./bin/sonde-gateway \
  --port /dev/ttyACM0 \
  --db sonde.db \
  --master-key-file master-key.hex \
  --generate-master-key \
  --handler-config handlers.yaml
```

| Flag | Purpose |
|------|---------|
| `--port` | Serial port of the modem's USB-CDC connector |
| `--db` | SQLite database (created if absent) |
| `--master-key-file` | 64-hex-char key file (backs up securely!) |
| `--generate-master-key` | Auto-generate if file missing |
| `--handler-config` | YAML handler routing (see step 8) |

The gateway logs `modem transport ready` when the modem handshake completes.

On Windows, the admin socket is `\\.\pipe\sonde-admin`. On Linux/macOS,
it defaults to `/var/run/sonde/admin.sock`.

## 5. Register a node

```sh
# Generate a PSK (32 random bytes as 64 hex chars)
PSK=$(openssl rand -hex 32)

# Compute the key_hint: SHA-256(PSK)[30..32] as big-endian u16
KEY_HINT=$(echo -n "$PSK" | xxd -r -p | sha256sum | cut -c61-64)
KEY_HINT_DEC=$((16#$KEY_HINT))

./bin/sonde-admin node register my-node-001 "$KEY_HINT_DEC" "$PSK"
```

> **Note:** In practice, use the BLE pairing tool (`sonde-admin pairing`)
> to provision nodes automatically — it handles PSK generation, key_hint
> derivation, and encrypted payload exchange.

### BLE pairing flow (preferred)

```sh
# Open a 120-second BLE registration window on the gateway
./bin/sonde-admin pairing start --duration-s 120

# The phone pairing app (sonde-pair) connects to the node via BLE,
# negotiates LESC, and provisions the node with:
#   - PSK, key_hint, RF channel
#   - Encrypted payload for gateway registration
#   - Optional I2C pin config (ND-0608)

# The node reboots, sends PEER_REQUEST, gateway registers it
```

## 6. Compile a BPF program

```sh
cd test-programs
make tmp102_sensor.o
```

Or manually:
```sh
clang -target bpf -O2 -Wall -Wextra -I. -c tmp102_sensor.c -o tmp102_sensor.o
```

The output is a BPF ELF object file. The gateway converts ELF → CBOR
program image internally (extracting bytecode, map definitions, and
.rodata/.data initial values).

## 7. Deploy the BPF program

```sh
# Ingest the ELF into the gateway's program library
./bin/sonde-admin program ingest test-programs/tmp102_sensor.o --profile resident

# Note the program hash from output, then assign to node
./bin/sonde-admin program assign my-node-001 PROGRAM_HASH

# Set the wake interval (seconds between sensor readings)
./bin/sonde-admin schedule set my-node-001 60
```

Profiles:
- `resident` — stored in node flash, runs every wake cycle
- `ephemeral` — one-shot diagnostic, discarded after execution

## 8. Configure a handler

Create `handlers.yaml`:

```yaml
handlers:
  - program_hash: "*"
    command: "python3"
    args: ["test-programs/tmp102_handler.py"]
```

Handlers receive `APP_DATA` from nodes via length-prefixed CBOR on stdin
and can reply via stdout. See `test-programs/tmp102_handler.py` for a
working example.

Restart the gateway with `--handler-config handlers.yaml` if it wasn't
started with one.

## 9. Verify end-to-end

```sh
# Check node status
./bin/sonde-admin status my-node-001

# Watch gateway logs for WAKE/COMMAND cycle
# Expected log sequence:
#   WAKE received: node_id=my-node-001, seq=1
#   COMMAND selected: node_id=my-node-001, command_type=UpdateProgram
#   session created: node_id=my-node-001
```

On the node UART (verbose firmware):
```
sonde-node booting (commit xxxxxxx)
boot_reason=deep_sleep_wake
WAKE frame sent: key_hint=XXXX
COMMAND received: command_type=UpdateProgram
BPF execution: program_hash=XXXXXXXX, result=Ok(0)
deep sleep entry: duration_seconds=60
```

## 10. Switch to production firmware

Once verified, flash the quiet (production) firmware:

```sh
espflash write-bin -p PORT 0x0 ./firmware/flash_image.bin
```

The quiet variant strips INFO/DEBUG/TRACE logs at compile time for
minimal power consumption. To debug later, reflash the verbose variant.

## Troubleshooting

| Symptom | Check |
|---------|-------|
| Node stuck in BLE pairing mode | No PSK in NVS — pair via BLE or register manually |
| WAKE timeout (no COMMAND) | Gateway not running, wrong channel, modem not connected |
| `0 APs on all channels` | WiFi scan error — check modem UART for error code |
| Handler not receiving data | Check `handlers.yaml` path, ensure handler is executable |
| `non-ELF program images not accepted` | Release gateway rejects raw CBOR — submit ELF files |
