#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 sonde contributors
"""
tmp102_handler.py — Gateway handler for TMP102 temperature sensor.

Receives APP_DATA via the sonde handler protocol (4-byte BE length +
CBOR on stdin), decodes TMP102 payloads, and writes JSON records to
a file.

Protocol: gateway sends DATA messages as length-prefixed CBOR:
  {1: 1, 2: request_id, 3: node_id, 4: program_hash, 5: data, 6: timestamp}

The handler replies with DATA_REPLY (no reply data):
  {1: 3, 2: request_id, 5: b""}

Usage with gateway:
  Create handlers.yaml:
    handlers:
      - program_hash: "*"
        command: "python"
        args: ["test-programs/tmp102_handler.py"]

  Start gateway with:
    sonde-gateway --handler-config handlers.yaml ...
"""

import sys
import struct
import json
import cbor2
from datetime import datetime, timezone

OUTPUT = "temperature_log.jsonl"


def read_message():
    """Read a length-prefixed CBOR message from stdin."""
    len_buf = sys.stdin.buffer.read(4)
    if len(len_buf) < 4:
        return None
    length = struct.unpack(">I", len_buf)[0]
    payload = sys.stdin.buffer.read(length)
    if len(payload) < length:
        return None
    return cbor2.loads(payload)


def write_message(msg):
    """Write a length-prefixed CBOR message to stdout."""
    payload = cbor2.dumps(msg)
    sys.stdout.buffer.write(struct.pack(">I", len(payload)))
    sys.stdout.buffer.write(payload)
    sys.stdout.buffer.flush()


def decode_tmp102(data):
    """Decode a 6-byte TMP102 payload into temperature."""
    if len(data) != 6:
        return None
    raw_hi = data[0]
    raw_lo = data[1]
    temp_mc = int.from_bytes(data[2:6], "little", signed=True)
    return {
        "raw_12bit": (raw_hi << 4) | (raw_lo >> 4),
        "temperature_c": temp_mc / 1000.0,
    }


def main():
    while True:
        msg = read_message()
        if msg is None:
            break

        msg_type = msg.get(1)
        if msg_type != 1:  # DATA = 1
            continue

        request_id = msg.get(2, 0)
        node_id = msg.get(3, "unknown")
        data = msg.get(5, b"")
        timestamp = msg.get(6, 0)

        decoded = decode_tmp102(data)

        record = {
            "timestamp": datetime.now(timezone.utc).isoformat(),
            "device": node_id,
            "raw_hex": data.hex(),
        }
        if decoded:
            record["temperature_c"] = decoded["temperature_c"]
            record["raw_12bit"] = decoded["raw_12bit"]

        # Append to JSON lines file
        with open(OUTPUT, "a") as f:
            f.write(json.dumps(record) + "\n")

        # Log to stderr (gateway captures handler stderr for diagnostics)
        temp_str = f"{decoded['temperature_c']:.3f}°C" if decoded else "decode failed"
        print(f"[TMP102] {node_id}: {temp_str}", file=sys.stderr, flush=True)

        # Reply with empty DATA_REPLY so gateway knows we processed it
        # msg_type 0x81 = DATA_REPLY, key 2 = request_id, key 3 = data
        write_message({1: 0x81, 2: request_id, 3: b""})


if __name__ == "__main__":
    main()
