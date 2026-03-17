#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 sonde contributors

"""
tmp102_handler — gateway handler for TMP102 temperature sensor data.

Receives APP_DATA from nodes running the tmp102_sensor BPF program,
decodes the temperature, and appends readings to a file named after
the node's assigned name (node_id).

Protocol: length-prefixed CBOR over stdin/stdout.
  - Receives DATA messages (type 0x01) with 6-byte TMP102 payload.
  - Replies with empty DATA_REPLY (no response back to node).

Payload format (from tmp102_sensor.c):
  [0:1] raw_hi, raw_lo  — raw TMP102 register bytes
  [2:5] temp_mc          — temperature in millidegrees Celsius (i32 LE)

Usage in handler config (handlers.yaml):
  handlers:
    - program_hash: "*"
      command: "python3"
      args: ["test-programs/tmp102_handler.py", "--output-dir", "./sensor-data"]
"""

import argparse
import re
import struct
import sys
from datetime import datetime, timezone
from pathlib import Path

try:
    import cbor2
except ImportError:
    sys.exit("ERROR: cbor2 package required — install with: pip install cbor2")


# Handler protocol message types (CBOR integer key 1).
MSG_DATA = 0x01
MSG_EVENT = 0x02
MSG_DATA_REPLY = 0x81
MSG_LOG = 0x82


def _read_exact(stream, n):
    """Read exactly n bytes from stream, handling partial reads."""
    buf = bytearray()
    while len(buf) < n:
        chunk = stream.read(n - len(buf))
        if not chunk:
            return None
        buf.extend(chunk)
    return bytes(buf)


def read_message(stream):
    """Read a length-prefixed CBOR message from a binary stream."""
    len_bytes = _read_exact(stream, 4)
    if len_bytes is None:
        return None
    length = struct.unpack(">I", len_bytes)[0]
    if length > 1_048_576:
        return None
    payload = _read_exact(stream, length)
    if payload is None:
        return None
    return cbor2.loads(payload)


def write_message(stream, msg):
    """Write a length-prefixed CBOR message to a binary stream."""
    payload = cbor2.dumps(msg)
    stream.write(struct.pack(">I", len(payload)))
    stream.write(payload)
    stream.flush()


def send_log(stream, level, message):
    """Send a LOG message to the gateway."""
    write_message(stream, {1: MSG_LOG, 2: level, 3: message})


def decode_tmp102_payload(data):
    """Decode the 6-byte TMP102 payload into (raw_hi, raw_lo, temp_mc)."""
    if len(data) != 6:
        raise ValueError(f"expected exactly 6 bytes, got {len(data)}")
    raw_hi = data[0]
    raw_lo = data[1]
    temp_mc = struct.unpack("<i", data[2:6])[0]
    return raw_hi, raw_lo, temp_mc


def main():
    parser = argparse.ArgumentParser(description="TMP102 sensor data handler")
    parser.add_argument(
        "--output-dir",
        default="./sensor-data",
        help="Directory for per-node data files (default: ./sensor-data)",
    )
    args = parser.parse_args()

    output_dir = Path(args.output_dir)
    output_dir.mkdir(parents=True, exist_ok=True)

    stdin = sys.stdin.buffer
    stdout = sys.stdout.buffer

    while True:
        msg = read_message(stdin)
        if msg is None:
            break

        msg_type = msg.get(1)

        if msg_type == MSG_DATA:
            request_id = msg.get(2)
            node_id = msg.get(3, "unknown")
            data = msg.get(5, b"")
            timestamp = msg.get(6, 0)

            # Sanitize node_id to prevent path traversal.
            safe_id = re.sub(r"[^a-zA-Z0-9_\-]", "_", node_id)
            if not safe_id:
                safe_id = "unknown"

            try:
                raw_hi, raw_lo, temp_mc = decode_tmp102_payload(data)
                temp_c = temp_mc / 1000.0
                ts = datetime.fromtimestamp(timestamp, tz=timezone.utc)

                # Append to file named after sanitized node_id.
                node_file = output_dir / f"{safe_id}.csv"
                is_new = not node_file.exists()
                with open(node_file, "a") as f:
                    if is_new:
                        f.write("timestamp,temp_c,temp_mc,raw_hi,raw_lo\n")
                    f.write(
                        f"{ts.isoformat()},{temp_c:.3f},{temp_mc},"
                        f"0x{raw_hi:02x},0x{raw_lo:02x}\n"
                    )

                send_log(stdout, "info", f"{node_id}: {temp_c:.3f} °C")
            except Exception as e:
                send_log(stdout, "error", f"{node_id}: parse error: {e}")

            # Reply with empty data (no response back to node).
            write_message(stdout, {1: MSG_DATA_REPLY, 2: request_id, 3: b""})

        elif msg_type == MSG_EVENT:
            node_id = msg.get(3, "unknown")
            event_type = msg.get(4, "")
            send_log(stdout, "info", f"{node_id}: event {event_type}")


if __name__ == "__main__":
    main()
