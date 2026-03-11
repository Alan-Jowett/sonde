<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Getting Started

> **Document status:** Draft
> **Scope:** Developer environment setup for building, testing, and flashing all Sonde crates.
> **Audience:** Contributors and LLM agents working on any part of the Sonde codebase.
> **Related:** [implementation-guide.md](implementation-guide.md), [README.md](../README.md)

---

## 1  Overview

The Sonde workspace will contain five crates with different platform requirements. Currently `sonde-protocol` and `sonde-gateway` are implemented; the remaining crates are planned:

| Crate | Runs on | Toolchain needed |
|-------|---------|-----------------|
| `sonde-protocol` | Any (no_std) | Standard Rust |
| `sonde-gateway` | Host (Linux/macOS/Windows) | Standard Rust |
| `sonde-admin` (planned) | Host (Linux/macOS/Windows) | Standard Rust |
| `sonde-node` (planned) | ESP32-C3 or ESP32-S3 | Espressif Rust (RISC-V and/or Xtensa) |
| `sonde-modem` (planned) | ESP32-S3 | Espressif Rust (Xtensa) |

You only need the Espressif toolchain once the node or modem firmware crates are available. The protocol crate, gateway, and admin CLI build with a standard Rust toolchain on any platform.

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

## 3  Espressif Rust toolchain (ESP32 firmware)

The node firmware targets ESP32-C3 (RISC-V) and ESP32-S3 (Xtensa). The modem firmware targets ESP32-S3 (Xtensa) only. Both use the ESP-IDF framework via `esp-idf-hal` and `esp-idf-svc`.

### 3.1  System prerequisites

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

### 3.2  Install espup

[espup](https://github.com/esp-rs/espup) manages the Espressif Rust toolchain (Xtensa LLVM fork, RISC-V target, and ESP-IDF source):

```sh
cargo install espup
```

Or, for a faster binary install:
```sh
cargo install cargo-binstall
cargo binstall espup
```

### 3.3  Install the Espressif toolchain

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

### 3.4  Install ldproxy

The ESP-IDF build system requires `ldproxy` to wrap the linker:

```sh
cargo install ldproxy
```

### 3.5  Install espflash

[espflash](https://github.com/esp-rs/espflash) is the tool for flashing firmware and monitoring serial output:

```sh
cargo install espflash
```

### 3.6  Verify the toolchain

After setup, verify you can target the ESP32 chips. These commands will work once the firmware crates are added to the workspace (see [implementation-guide.md](implementation-guide.md) Phase 5 and Phase 3):

```sh
# List installed targets (should include esp targets)
rustup target list --installed

# Build the modem firmware (ESP32-S3, Xtensa) — requires sonde-modem crate
cargo build -p sonde-modem --target xtensa-esp32s3-espidf

# Build the node firmware (ESP32-C3, RISC-V) — requires sonde-node crate
cargo build -p sonde-node --target riscv32imc-esp-espidf

# Build the node firmware (ESP32-S3, Xtensa) — requires sonde-node crate
cargo build -p sonde-node --target xtensa-esp32s3-espidf
```

---

## 4  Flashing firmware

> **Note:** The firmware crates (`sonde-modem`, `sonde-node`) are not yet in the workspace. The commands below will work once they are added (see [implementation-guide.md](implementation-guide.md) Phase 3 and Phase 5).

### 4.1  Modem (ESP32-S3)

Connect the ESP32-S3 board via USB, then:

```sh
cargo espflash flash -p sonde-modem --target xtensa-esp32s3-espidf --monitor
```

The `--monitor` flag opens a serial console after flashing so you can see log output.

### 4.2  Node (ESP32-C3)

```sh
cargo espflash flash -p sonde-node --target riscv32imc-esp-espidf --monitor
```

### 4.3  Node (ESP32-S3)

```sh
cargo espflash flash -p sonde-node --target xtensa-esp32s3-espidf --monitor
```

### 4.4  Serial port permissions (Linux)

On Linux, you may need to add your user to the `dialout` group to access serial ports:

```sh
sudo usermod -a -G dialout $USER
```

Log out and back in for the group change to take effect.

---

## 5  Repository setup

### 5.1  Clone and configure git hooks

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

### 5.2  SPDX headers

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

## 6  Project structure

The target workspace layout (see [implementation-guide.md §2](implementation-guide.md) for details):

```
sonde/
├── Cargo.toml                    # Workspace root
├── crates/
│   ├── sonde-protocol/           # Shared no_std protocol crate
│   ├── sonde-gateway/            # Async gateway service (tokio)
│   ├── sonde-node/               # ESP32 sensor node firmware (planned)
│   ├── sonde-modem/              # ESP32-S3 radio modem firmware (planned)
│   └── sonde-admin/              # CLI admin tool (planned)
├── docs/                         # Specifications and design docs
└── hooks/                        # Git hooks
```

See [implementation-guide.md](implementation-guide.md) for the full module breakdown and build order.

---

## 7  Common tasks

| Task | Command |
|------|---------|
| Format all code | `cargo fmt --all` |
| Lint all code | `cargo clippy --workspace -- -D warnings` |
| Test protocol crate | `cargo test -p sonde-protocol` |
| Test gateway | `cargo test -p sonde-gateway` |
| Build host crates | `cargo build -p sonde-protocol -p sonde-gateway` |
| Build modem firmware (planned) | `cargo build -p sonde-modem --target xtensa-esp32s3-espidf` |
| Build node firmware (planned) | `cargo build -p sonde-node --target riscv32imc-esp-espidf` |
| Flash modem (planned) | `cargo espflash flash -p sonde-modem --target xtensa-esp32s3-espidf --monitor` |
| Flash node (planned) | `cargo espflash flash -p sonde-node --target riscv32imc-esp-espidf --monitor` |

---

## 8  Troubleshooting

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
