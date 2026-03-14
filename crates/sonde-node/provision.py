#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 sonde contributors
#
# Pre-provision an ESP32-C3 node with a PSK for testing.
#
# Usage:
#   python provision.py --port COM6 --key-hint 1 --psk AA..AA
#   python provision.py --port COM6  (uses defaults: key_hint=1, psk=all-0xAA)
#
# This generates an NVS partition image with the pairing key pre-loaded,
# then flashes it to the NVS partition at offset 0x9000. The node will
# boot as "already paired" and skip pairing mode.
#
# Requires: esptool (pip install esptool)

import argparse
import os
import subprocess
import sys
import tempfile


# NVS partition offset and size (from partition-table: "Single factory app (large)")
NVS_OFFSET = 0x9000
NVS_SIZE = 0x6000  # 24K

# Must match crates/sonde-node/src/esp_storage.rs
NVS_NAMESPACE = "sonde"
MAGIC_VALUE = 0xDEADBEEF


def find_nvs_gen():
    """Locate nvs_partition_gen.py from the embuild ESP-IDF installation."""
    # Check if it's importable as a package
    try:
        from esp_idf_nvs_partition_gen import nvs_partition_gen
        return nvs_partition_gen.__file__
    except ImportError:
        pass

    # Search common locations
    for base in [
        os.path.join(os.path.dirname(__file__), "..", ".embuild"),
        os.path.expanduser("~/.espressif"),
    ]:
        for root, dirs, files in os.walk(base):
            if "nvs_partition_gen.py" in files:
                return os.path.join(root, "nvs_partition_gen.py")

    return None


def generate_nvs_csv(key_hint: int, psk: bytes) -> str:
    """Generate NVS CSV content for the sonde namespace."""
    psk_hex = psk.hex()
    return (
        "key,type,encoding,value\n"
        f"{NVS_NAMESPACE},namespace,,\n"
        f"magic,data,u32,{MAGIC_VALUE}\n"
        f"key_hint,data,u32,{key_hint}\n"
        f"psk,data,hex2bin,{psk_hex}\n"
    )


def main():
    parser = argparse.ArgumentParser(
        description="Pre-provision an ESP32-C3 sonde node with a PSK."
    )
    parser.add_argument("--port", required=True, help="Serial port (e.g. COM6)")
    parser.add_argument(
        "--key-hint",
        type=lambda x: int(x, 0),
        default=1,
        help="Key hint (u16, default: 1)",
    )
    parser.add_argument(
        "--psk",
        default="AA" * 32,
        help="PSK as 64 hex chars (default: all-0xAA)",
    )
    parser.add_argument(
        "--chip",
        default="esp32c3",
        help="ESP chip type (default: esp32c3)",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Generate NVS image but don't flash",
    )
    args = parser.parse_args()

    # Validate inputs
    if args.key_hint < 0 or args.key_hint > 0xFFFF:
        sys.exit("Error: key-hint must be 0-65535")

    try:
        psk = bytes.fromhex(args.psk)
    except ValueError:
        sys.exit("Error: PSK must be valid hex")
    if len(psk) != 32:
        sys.exit(f"Error: PSK must be 32 bytes (got {len(psk)})")

    # Find NVS partition generator
    nvs_gen = find_nvs_gen()
    if not nvs_gen:
        sys.exit(
            "Error: nvs_partition_gen.py not found.\n"
            "Install with: pip install esp-idf-nvs-partition-gen"
        )

    with tempfile.TemporaryDirectory() as tmpdir:
        csv_path = os.path.join(tmpdir, "nvs.csv")
        bin_path = os.path.join(tmpdir, "nvs.bin")

        # Generate CSV
        csv_content = generate_nvs_csv(args.key_hint, psk)
        with open(csv_path, "w") as f:
            f.write(csv_content)

        print(f"Key hint: 0x{args.key_hint:04x}")
        print(f"PSK:      {psk.hex()}")
        print()

        # Generate NVS binary
        cmd = [
            sys.executable, nvs_gen,
            "generate", csv_path, bin_path, str(NVS_SIZE),
        ]
        result = subprocess.run(cmd, capture_output=True, text=True)
        if result.returncode != 0:
            print(result.stderr, file=sys.stderr)
            sys.exit(f"Error: NVS partition generation failed (exit {result.returncode})")

        bin_size = os.path.getsize(bin_path)
        print(f"Generated NVS image: {bin_size} bytes")

        if args.dry_run:
            # Copy to current directory for inspection
            import shutil
            out = os.path.join(os.getcwd(), "nvs_provision.bin")
            shutil.copy(bin_path, out)
            print(f"Saved to: {out}")
            return

        # Flash NVS partition
        print(f"Flashing to {args.port} at offset 0x{NVS_OFFSET:04x}...")
        flash_cmd = [
            sys.executable, "-m", "esptool",
            "--chip", args.chip,
            "--port", args.port,
            "write_flash",
            f"0x{NVS_OFFSET:x}", bin_path,
        ]
        result = subprocess.run(flash_cmd)
        if result.returncode != 0:
            sys.exit(f"Error: flash failed (exit {result.returncode})")

        print()
        print("Done! Node is now provisioned with:")
        print(f"  key_hint = 0x{args.key_hint:04x}")
        print(f"  PSK      = {psk.hex()}")
        print()
        print("Reset the device to boot in paired mode.")


if __name__ == "__main__":
    main()
