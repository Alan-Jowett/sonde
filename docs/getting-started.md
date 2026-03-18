<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Getting Started

> **Document status:** Draft
> **Scope:** Developer environment setup for building, testing, and flashing all Sonde crates.
> **Audience:** Contributors and LLM agents working on any part of the Sonde codebase.
> **Related:** [implementation-guide.md](implementation-guide.md), [README.md](../README.md)

> **Repository status:** Active development — pre-1.0. Core crates (protocol, gateway, modem, node) are implemented and tested. See the [Project status](../README.md#project-status) section in the README for the current state of each crate and the roadmap.

---

## 1  Overview

The Sonde workspace contains several crates with different platform requirements:

| Crate | Runs on | Toolchain needed |
|-------|---------|-----------------|
| `sonde-protocol` | Any (no_std) | Standard Rust |
| `sonde-gateway` | Host (Linux/macOS/Windows) | Standard Rust |
| `sonde-admin` | Host (Linux/macOS/Windows) | Standard Rust |
| `sonde-node` | ESP32-C3 or ESP32-S3 | Espressif Rust (RISC-V and/or Xtensa) |
| `sonde-modem` | ESP32-S3 | Espressif Rust (Xtensa) |
| `sonde-bpf` | Host (used by `sonde-node`) | Standard Rust |
| `sonde-e2e` | Host | Standard Rust |
| `sonde-pair` (planned) | Android / Windows / Linux | Standard Rust + Android NDK ([dev container](#11--android--tauri-development-container)) |

You only need the Espressif toolchain if you intend to build firmware (`sonde-node` or `sonde-modem`). The remaining crates build with a standard Rust toolchain on any platform.

---

## 2  Standard Rust toolchain

### 2.1  Install Rust

Install Rust via [rustup](https://rustup.rs/):

**Linux / macOS:**
```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

**Windows:**

Download and run [rustup-init.exe](https://rustup.rs/).

After installation, verify:
```sh
rustc --version
cargo --version
```

### 2.2  Build and test (host crates)

With the standard toolchain you can build and test the protocol crate, gateway, and admin CLI:

```sh
# Format, lint, and test
cargo fmt --all
cargo clippy --workspace -- -D warnings
cargo test -p sonde-protocol
cargo test -p sonde-gateway

# Build host crates
cargo build -p sonde-protocol -p sonde-gateway
```

---

## 3  Docker-based ESP32 development (recommended)

The easiest way to build ESP32 firmware is with the pre-built development container, which has all toolchain dependencies pre-installed.

### 3.1  Using VS Code / GitHub Codespaces (devcontainer)

The repository includes a [devcontainer](../.devcontainer/devcontainer.json) configuration. In VS Code:

1. Install the [Dev Containers extension](https://marketplace.visualstudio.com/items?itemName=ms-vscode-remote.remote-containers).
2. Open the repo and select **"Reopen in Container"** when prompted.
3. All ESP32 tools are available immediately — build firmware directly in the integrated terminal.

On GitHub Codespaces, open the repo in a codespace and the container starts automatically.

### 3.2  Using Docker directly

Pull the image and mount the repo:

```sh
docker pull ghcr.io/alan-jowett/sonde-esp-dev:latest
docker run --rm -v "$(pwd)":/sonde -w /sonde \
    -e ESP_IDF_SDKCONFIG_DEFAULTS=crates/sonde-node/sdkconfig.defaults \
    ghcr.io/alan-jowett/sonde-esp-dev:latest \
    cargo +esp build -p sonde-node --bin node --features esp --profile firmware \
    --target riscv32imc-esp-espidf -Zbuild-std=std,panic_abort
```

Or start an interactive shell:

```sh
docker run --rm -it -v "$(pwd)":/sonde -w /sonde ghcr.io/alan-jowett/sonde-esp-dev:latest
```

### 3.3  Building both firmware targets

Each firmware build requires `ESP_IDF_SDKCONFIG_DEFAULTS` to be set so the ESP-IDF build system applies the correct settings (see §3.4).

**ESP32-C3 node firmware (RISC-V):**
```sh
ESP_IDF_SDKCONFIG_DEFAULTS=crates/sonde-node/sdkconfig.defaults \
    cargo +esp build -p sonde-node --bin node --features esp --profile firmware \
    --target riscv32imc-esp-espidf -Zbuild-std=std,panic_abort
```

**ESP32-S3 modem firmware (Xtensa):**
```sh
ESP_IDF_SDKCONFIG_DEFAULTS="crates/sonde-modem/sdkconfig.defaults;sdkconfig.defaults.esp32s3" \
    cargo +esp build -p sonde-modem --bin modem --features esp --profile firmware \
    --target xtensa-esp32s3-espidf -Zbuild-std=std,panic_abort
```

> **Note:** On Windows, use a short `CARGO_TARGET_DIR` (e.g., `F:\t`) to avoid exceeding MAX_PATH. See the [README](../README.md) for details.

### 3.4  sdkconfig.defaults and ESP-IDF configuration

Each firmware crate has a `sdkconfig.defaults` file that controls ESP-IDF settings (main task stack size, FreeRTOS tick rate, flash mode, WiFi features, etc.):

- `crates/sonde-node/sdkconfig.defaults` — node settings (stack size, flash config, tick rate)
- `crates/sonde-modem/sdkconfig.defaults` — modem settings (console, watchdog, WiFi)
- `sdkconfig.defaults.esp32s3` — **workspace-root** chip-specific settings for ESP32-S3 (Bluetooth/NimBLE configuration required by the modem's BLE GATT server)

The modem build uses **both** its crate-local defaults and the workspace-root ESP32-S3 file (semicolon-separated in the env var). The node build uses only its crate-local defaults.

These are passed to the ESP-IDF build system via the `ESP_IDF_SDKCONFIG_DEFAULTS` environment variable. The CI workflows set this automatically. For local Docker builds, pass it explicitly:

```sh
# Node:
docker run --rm -v "$(pwd)":/sonde -w /sonde \
    -e ESP_IDF_SDKCONFIG_DEFAULTS=crates/sonde-node/sdkconfig.defaults \
    ghcr.io/alan-jowett/sonde-esp-dev:latest \
    cargo +esp build -p sonde-node --bin node --features esp --profile firmware \
    --target riscv32imc-esp-espidf -Zbuild-std=std,panic_abort

# Modem (both defaults files):
docker run --rm -v "$(pwd)":/sonde -w /sonde \
    -e "ESP_IDF_SDKCONFIG_DEFAULTS=crates/sonde-modem/sdkconfig.defaults;sdkconfig.defaults.esp32s3" \
    ghcr.io/alan-jowett/sonde-esp-dev:latest \
    cargo +esp build -p sonde-modem --bin modem --features esp --profile firmware \
    --target xtensa-esp32s3-espidf -Zbuild-std=std,panic_abort
```

The path is **relative to the workspace root**.

**Important caveats:**

1. **Generated sdkconfig persists in the target directory.** Once the ESP-IDF build runs, it creates a `sdkconfig` file in the build output. On subsequent builds, this generated file takes precedence over `sdkconfig.defaults`. If you change `sdkconfig.defaults`, you must **delete the target directory** (or at least the generated `sdkconfig` under `target/<target>/<profile>/build/esp-idf-sys-*/out/sdkconfig`) for the changes to take effect.

2. **Silent fallback to ESP-IDF defaults.** If `esp-idf-sys` cannot find `sdkconfig.defaults` (missing `[package.metadata.esp-idf-sys]` or wrong path), it silently falls back to ESP-IDF's built-in defaults. This can cause hard-to-debug issues like stack overflows (ESP-IDF's default main task stack is 3584 bytes, but the node firmware needs 16KB).

3. **CI cache invalidation.** The CI workflows include `sdkconfig.defaults` in the cache key, so changing the file automatically busts the cache. A CI step also asserts that critical config values (stack size, tick rate) appear in the generated `sdkconfig`.

---

## 4  Espressif Rust toolchain (manual setup)

The node firmware targets ESP32-C3 (RISC-V) and ESP32-S3 (Xtensa). The modem firmware targets ESP32-S3 (Xtensa) only. Both use the ESP-IDF framework via `esp-idf-hal` and `esp-idf-svc`.

### 4.1  System prerequisites

Install these before proceeding:

**Linux (Debian/Ubuntu):**
```sh
sudo apt update
sudo apt install -y git curl gcc build-essential pkg-config libssl-dev \
    libudev-dev python3 python3-venv cmake ninja-build
```

**macOS (Homebrew):**
```sh
xcode-select --install
brew install cmake ninja python3
```

**Windows:**

1. Install [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) with the "Desktop development with C++" workload.
2. Install [Python 3](https://www.python.org/downloads/) (add to PATH).
3. Install [CMake](https://cmake.org/download/) and [Ninja](https://github.com/nicknisi/ninja/releases) (add both to PATH).
4. Install [Git for Windows](https://gitforwindows.org/).

### 4.2  Install espup

[espup](https://github.com/esp-rs/espup) manages the Espressif Rust toolchain (Xtensa LLVM fork, RISC-V target, and ESP-IDF source):

```sh
cargo install espup
```

Or, for a faster binary install:
```sh
cargo install cargo-binstall
cargo binstall espup
```

### 4.3  Install the Espressif toolchain

```sh
espup install
```

This downloads and configures:
- The Espressif fork of the Rust compiler (with Xtensa backend support).
- The `riscv32imc-esp-espidf` and `xtensa-esp32s3-espidf` targets.
- The ESP-IDF SDK source (used at build time via `esp-idf-sys`).

After installation, **source the environment file** so the custom toolchain is active:

**Linux / macOS:**
```sh
. $HOME/export-esp.sh
```

**Windows (PowerShell):**
```powershell
. $HOME\export-esp.ps1
```

> **Tip:** Add the source command to your shell profile (`.bashrc`, `.zshrc`, or PowerShell `$PROFILE`) so it runs automatically in every terminal session.

### 4.4  Install ldproxy

The ESP-IDF build system requires `ldproxy` to wrap the linker:

```sh
cargo install ldproxy
```

### 4.5  Install espflash

[espflash](https://github.com/esp-rs/espflash) is the tool for flashing firmware and monitoring serial output:

```sh
cargo install espflash
```

### 4.6  Verify the toolchain

After setup, verify you can target the ESP32 chips:

```sh
# List installed targets (should include esp targets)
rustup target list --installed

# Build the modem firmware (ESP32-S3, Xtensa) — requires sonde-modem crate
cargo build -p sonde-modem --features esp --target xtensa-esp32s3-espidf

# Build the node firmware (ESP32-C3, RISC-V) — requires sonde-node crate
cargo build -p sonde-node --target riscv32imc-esp-espidf

# Build the node firmware (ESP32-S3, Xtensa) — requires sonde-node crate
cargo build -p sonde-node --target xtensa-esp32s3-espidf
```

---

## 5  Flashing firmware

### 5.1  Cloud build + local flash (recommended)

The fastest way to get a flashable binary is to let CI build it:

1. Push your branch to GitHub.
2. Wait ~2–3 minutes for the ESP32 CI workflows to finish.
3. Download the firmware artifact with the GitHub CLI.
4. Flash directly with `espflash`.

```sh
# Find the latest CI run for your branch, then download its artifacts:
RUN_ID=$(gh run list --branch "$(git branch --show-current)" \
  -w "ESP32-C3 Node Firmware CI" --json databaseId -q '.[0].databaseId')

# Node firmware (ESP32-C3)
gh run download "$RUN_ID" --name node-firmware --dir ./firmware/
espflash write-bin -p PORT 0x0 ./firmware/flash_image.bin
espflash monitor -p PORT

# Modem firmware (ESP32-S3) — use the modem workflow name instead:
RUN_ID=$(gh run list --branch "$(git branch --show-current)" \
  -w "ESP32-S3 Modem Firmware CI" --json databaseId -q '.[0].databaseId')
gh run download "$RUN_ID" --name modem-firmware --dir ./firmware-modem/
espflash write-bin -p PORT 0x0 ./firmware-modem/flash_image.bin
espflash monitor -p PORT
```

> **Note:** The CI artifacts contain **merged flash images** (bootloader + partition table + app) built against the same ESP-IDF version as the app. Using `espflash write-bin` at offset `0x0` writes this image directly, avoiding bootloader/app version mismatches that occur when `espflash flash` substitutes its own bundled bootloader. Replace `PORT` with your device's serial port (e.g., `COM6` on Windows, `/dev/ttyUSB0` on Linux, `/dev/cu.usbmodem*` on macOS). If unsure, omit `-p PORT` and `espflash` will auto-detect or prompt.

**Benefits:**
- **10× faster** — 2–3 min (push + CI + download) vs 20+ min local Docker build.
- **No local toolchain needed** — only `espflash` and USB access required.
- **Consistent builds** — the same binary that CI tests is what gets flashed.

> **Prerequisites:** Install the [GitHub CLI](https://cli.github.com/) (`gh`) and [`espflash`](https://github.com/esp-rs/espflash) (`cargo install espflash`). The gateway binary is also available as a CI artifact (`gh run download "$(gh run list --branch "$(git branch --show-current)" -w CI --json databaseId -q '.[0].databaseId')" --name gateway-linux-x86_64`).

### 5.2  Modem (ESP32-S3) — local build

Connect the ESP32-S3 board via USB, then:

```sh
cargo espflash flash -p sonde-modem --features esp --target xtensa-esp32s3-espidf --monitor
```

The `--monitor` flag opens a serial console after flashing so you can see log output.

### 5.3  Node (ESP32-C3) — local build

```sh
cargo espflash flash -p sonde-node --target riscv32imc-esp-espidf --monitor
```

### 5.4  Node (ESP32-S3) — local build

```sh
cargo espflash flash -p sonde-node --target xtensa-esp32s3-espidf --monitor
```

### 5.5  Serial port permissions (Linux)

On Linux, you may need to add your user to the `dialout` group to access serial ports:

```sh
sudo usermod -a -G dialout $USER
```

Log out and back in for the group change to take effect.

---

## 6  Repository setup

### 6.1  Clone and configure git hooks

```sh
git clone https://github.com/Alan-Jowett/sonde.git
cd sonde
git config core.hooksPath hooks
```

The hooks enforce SPDX license headers and DCO sign-off on every commit.

Alternatively, use [pre-commit](https://pre-commit.com):

```sh
pip install pre-commit
pre-commit install --hook-type pre-commit --hook-type commit-msg
```

### 6.2  SPDX headers

Every `.rs` file must start with:
```rust
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors
```

Every `.md` file must start with:
```html
<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
```

---

## 7  Project structure

The target workspace layout (see [implementation-guide.md §2](implementation-guide.md) for details):

```
sonde/
├── Cargo.toml                    # Workspace root
├── crates/
│   ├── sonde-protocol/           # Shared no_std protocol crate
│   ├── sonde-gateway/            # Async gateway service (tokio)
│   ├── sonde-node/               # ESP32 sensor node firmware
│   ├── sonde-modem/              # ESP32-S3 radio modem firmware
│   ├── sonde-admin/              # CLI admin tool
│   ├── sonde-pair/               # BLE pairing tool — Tauri v2 (planned)
│   ├── sonde-bpf/                # Safe BPF interpreter
│   └── sonde-e2e/                # End-to-end test harness
├── docs/                         # Specifications and design docs
└── hooks/                        # Git hooks
```

See [implementation-guide.md](implementation-guide.md) for the full module breakdown and build order.

---

## 8  Common tasks

| Task | Command |
|------|---------|
| Format all code | `cargo fmt --all` |
| Lint all code | `cargo clippy --workspace -- -D warnings` |
| Test protocol crate | `cargo test -p sonde-protocol` |
| Test gateway | `cargo test -p sonde-gateway` |
| Build host crates | `cargo build -p sonde-protocol -p sonde-gateway` |
| Build modem firmware | `cargo +esp build -p sonde-modem --bin modem --features esp --profile firmware --target xtensa-esp32s3-espidf -Zbuild-std=std,panic_abort` |
| Build node firmware | `cargo +esp build -p sonde-node --bin node --features esp --profile firmware --target riscv32imc-esp-espidf -Zbuild-std=std,panic_abort` |
| Flash modem | `cargo espflash flash -p sonde-modem --features esp --target xtensa-esp32s3-espidf --monitor` |
| Flash node | `cargo espflash flash -p sonde-node --features esp --target riscv32imc-esp-espidf --monitor` |

---

## 9  Linux deployment (systemd)

The gateway can run as a systemd service for production use on Linux. Tracing output goes to stderr, which journald captures automatically — no additional logging configuration is needed.

### 9.1  Prerequisites

1. **Build and install the binary:**
   ```sh
   cargo build -p sonde-gateway --release
   sudo cp target/release/sonde-gateway /usr/local/bin/
   ```

2. **Create the `sonde` system user and add it to the `dialout` group for serial port access:**
   ```sh
   sudo useradd -r -s /usr/sbin/nologin sonde
   sudo usermod -a -G dialout sonde
   ```

3. **Create the required directories and generate a master key:**
   ```sh
   sudo mkdir -p /var/lib/sonde /etc/sonde
   sudo chown sonde:sonde /var/lib/sonde
   # Generate a 32-byte (256-bit) random master key and store it as hex
   # Use install to create the file with correct permissions atomically
   openssl rand -hex 32 | sudo install -m 600 -o sonde -g sonde /dev/stdin /etc/sonde/master-key.hex
   ```

### 9.2  Install and enable the service

The unit file is at `deploy/sonde-gateway.service` in the repository:

```sh
sudo cp deploy/sonde-gateway.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now sonde-gateway
```

The default `ExecStart` line uses `/dev/ttyACM0` as the serial port. If your modem appears on a different port (e.g., `/dev/ttyUSB0`), edit `/etc/systemd/system/sonde-gateway.service` and update the `--port` argument, then reload and restart:

```sh
sudo systemctl daemon-reload
sudo systemctl restart sonde-gateway
```

### 9.3  Managing the service

| Task | Command |
|------|---------|
| Start the service | `sudo systemctl start sonde-gateway` |
| Stop the service | `sudo systemctl stop sonde-gateway` |
| Restart the service | `sudo systemctl restart sonde-gateway` |
| Check service status | `sudo systemctl status sonde-gateway` |
| View live logs | `journalctl -u sonde-gateway -f` |
| View recent logs | `journalctl -u sonde-gateway -n 100` |
| Disable auto-start | `sudo systemctl disable sonde-gateway` |

---

## 10  Troubleshooting

### espup install fails

- Make sure Python 3, CMake, and Ninja are installed and on your PATH.
- On Windows, ensure the Visual Studio Build Tools C++ workload is installed.
- If you have a pre-existing ESP-IDF installation, remove it from your PATH to avoid conflicts.

### Build fails with linker errors

- Ensure `ldproxy` is installed: `cargo install ldproxy`.
- Ensure you sourced the Espressif environment file (`export-esp.sh` or `export-esp.ps1`).

### Serial port not found when flashing

- **Linux:** Add your user to the `dialout` group (see §4.4).
- **Windows:** Check Device Manager for the COM port number.
- **macOS:** The port is typically `/dev/cu.usbmodem*` or `/dev/cu.SLAB_USBtoUART`.

### cargo build --workspace fails for ESP targets

`cargo build --workspace` builds all workspace members for the active toolchain. If firmware crates are added to the workspace, they will fail to build without the Espressif toolchain and an explicit `--target` flag (e.g., `--target xtensa-esp32s3-espidf`). To build only host crates, select them explicitly with `-p` (e.g., `cargo build -p sonde-protocol -p sonde-gateway`).

### Firmware crashes with "Stack protection fault" on boot

The main task stack is too small. Check that `sdkconfig.defaults` is being applied (see §3.4). Common causes:

- **Stale generated sdkconfig:** Delete `target/<target>/<profile>/build/esp-idf-sys-*/out/sdkconfig` and rebuild.
- **Missing `[package.metadata.esp-idf-sys]`:** Each firmware crate's `Cargo.toml` must have this section pointing to `sdkconfig.defaults`. Without it, `esp-idf-sys` silently uses ESP-IDF's default 3584-byte stack.
- **Stack size too small:** If the firmware adds new features that increase stack usage, bump `CONFIG_ESP_MAIN_TASK_STACK_SIZE` in `sdkconfig.defaults`.

---

## 11  Android / Tauri development container

The BLE pairing tool (`sonde-pair`) targets Android (`aarch64-linux-android`) and
Windows/Linux. A pre-built development container provides all required tools so you
don't need to install the Android SDK, NDK, or Tauri dependencies locally.

### 11.1  Using the container image

The container is published to `ghcr.io/alan-jowett/sonde-android-dev:latest`.

```bash
# Cross-compile sonde-pair for Android (once the crate exists — see #163)
docker run --rm -v .:/sonde -w /sonde ghcr.io/alan-jowett/sonde-android-dev:latest \
  cargo ndk -t arm64-v8a build -p sonde-pair --release

# Build sonde-pair for Linux host (once the crate exists)
docker run --rm -v .:/sonde -w /sonde ghcr.io/alan-jowett/sonde-android-dev:latest \
  cargo build -p sonde-pair --release
```

### 11.2  VS Code Dev Container / GitHub Codespaces

Open the repository in VS Code, then select **Reopen in Container** when prompted.
The `.devcontainer/android/devcontainer.json` configuration will use the pre-built
container image. This also works in GitHub Codespaces.

### 11.3  What's included

| Component | Version / Notes |
|-----------|----------------|
| Ubuntu | 24.04 |
| Rust (stable) | + `aarch64-linux-android` target |
| Android SDK | Platform 35, Build Tools 35.0.0 |
| Android NDK | r27 (27.2.12479018) |
| Java JDK | 17 (headless) |
| Node.js | 20 LTS |
| `cargo-ndk` | For Android NDK cross-compilation |
| `cargo-tauri` | Tauri CLI v2 |
| `protobuf-compiler` | For sonde-protocol |
| `libudev-dev`, `libdbus-1-dev` | For btleplug BLE on Linux |
| Tauri system deps | `libwebkit2gtk-4.1-dev`, `libgtk-3-dev`, etc. |

### 11.4  Building the container locally

```bash
docker build -f .github/docker/Dockerfile.android-dev -t sonde-android-dev .
```
