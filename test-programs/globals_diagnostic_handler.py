#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 sonde contributors
"""
globals_diagnostic_handler.py — log decoded globals diagnostic results.

This handler expects the gateway to enrich incoming DATA messages with a
`readings` map produced by the paired decoder in `globals_diagnostic.c`.

Usage with gateway:
  handlers:
    - program_hash: "*"
      command: "python"
      args: ["test-programs/globals_diagnostic_handler.py"]
"""

import struct
import sys

DATA = 0x01
DATA_REPLY = 0x81
LOG = 0x82
MAX_MESSAGE_SIZE = 1_048_576
INVALID_MESSAGE = object()

KEY_MSG_TYPE = 1
KEY_REQUEST_ID = 2
KEY_NODE_ID = 3
KEY_DATA = 5
KEY_READINGS = 16

RODATA_INIT = 0x13579BDF
DATA_INIT = 0x2468ACE0


def read_exact(length):
    chunks = bytearray()
    while len(chunks) < length:
        chunk = sys.stdin.buffer.read(length - len(chunks))
        if not chunk:
            return None if not chunks else INVALID_MESSAGE
        chunks.extend(chunk)
    return bytes(chunks)


def discard_exact(length):
    remaining = length
    while remaining > 0:
        chunk = sys.stdin.buffer.read(min(remaining, 4096))
        if not chunk:
            return False
        remaining -= len(chunk)
    return True


def read_message():
    header = read_exact(4)
    if header is None:
        return None
    if header is INVALID_MESSAGE:
        return INVALID_MESSAGE
    length = struct.unpack(">I", header)[0]
    if length > MAX_MESSAGE_SIZE:
        if not discard_exact(length):
            return INVALID_MESSAGE
        return INVALID_MESSAGE
    payload = read_exact(length)
    if payload is None:
        return None
    if payload is INVALID_MESSAGE:
        return INVALID_MESSAGE
    try:
        return decode_cbor_map(payload)
    except (ValueError, IndexError, UnicodeDecodeError):
        return INVALID_MESSAGE


def write_message(message):
    payload = encode_cbor_map(message)
    sys.stdout.buffer.write(struct.pack(">I", len(payload)))
    sys.stdout.buffer.write(payload)
    sys.stdout.buffer.flush()


def decode_uint(info, data, index):
    if info < 24:
        return info, index
    if info == 24:
        return data[index], index + 1
    if info == 25:
        return int.from_bytes(data[index:index + 2], "big"), index + 2
    if info == 26:
        return int.from_bytes(data[index:index + 4], "big"), index + 4
    if info == 27:
        return int.from_bytes(data[index:index + 8], "big"), index + 8
    raise ValueError(f"unsupported additional info {info}")


def decode_item(data, index):
    major = (data[index] >> 5) & 0x07
    info = data[index] & 0x1F
    index += 1

    if major == 0:
        value, index = decode_uint(info, data, index)
        return value, index
    if major == 1:
        value, index = decode_uint(info, data, index)
        return -1 - value, index
    if major == 2:
        length, index = decode_uint(info, data, index)
        return data[index:index + length], index + length
    if major == 3:
        length, index = decode_uint(info, data, index)
        return data[index:index + length].decode("utf-8"), index + length
    if major == 5:
        count, index = decode_uint(info, data, index)
        value = {}
        for _ in range(count):
            key, index = decode_item(data, index)
            item, index = decode_item(data, index)
            value[key] = item
        return value, index

    raise ValueError(f"unsupported major type {major}")


def decode_cbor_map(data):
    value, index = decode_item(data, 0)
    if not isinstance(value, dict) or index != len(data):
        raise ValueError("expected one CBOR map payload")
    return value


def encode_uint(major, value):
    if value < 24:
        return bytes([(major << 5) | value])
    if value <= 0xFF:
        return bytes([(major << 5) | 24, value])
    if value <= 0xFFFF:
        return bytes([(major << 5) | 25]) + value.to_bytes(2, "big")
    if value <= 0xFFFFFFFF:
        return bytes([(major << 5) | 26]) + value.to_bytes(4, "big")
    return bytes([(major << 5) | 27]) + value.to_bytes(8, "big")


def encode_item(value):
    if isinstance(value, int):
        if value < 0:
            return encode_uint(1, -1 - value)
        return encode_uint(0, value)
    if isinstance(value, bytes):
        return encode_uint(2, len(value)) + value
    if isinstance(value, str):
        encoded = value.encode("utf-8")
        return encode_uint(3, len(encoded)) + encoded
    if isinstance(value, dict):
        output = encode_uint(5, len(value))
        for key, item in value.items():
            output += encode_item(key)
            output += encode_item(item)
        return output

    raise ValueError(f"unsupported type {type(value)}")


def encode_cbor_map(value):
    return encode_item(value)


def status_from_readings(readings):
    wake_index = int(readings.get("wake_index", -1))
    rodata_value = int(readings.get("rodata_value", -1))
    data_before = int(readings.get("data_before", -1))
    data_after = int(readings.get("data_after", -1))
    bss_before = int(readings.get("bss_before", -1))
    bss_after = int(readings.get("bss_after", -1))

    errors = []
    if rodata_value != RODATA_INIT:
        errors.append("rodata")
    if data_after != (data_before + 1) & 0xFFFFFFFF:
        errors.append("data-step")
    if wake_index == 0:
        if data_before != DATA_INIT:
            errors.append("data-init")
        if bss_before != 0:
            errors.append("bss-init")
    if bss_after != (bss_before + 1) & 0xFFFFFFFF:
        errors.append("bss-step")

    status = "PASS" if not errors else "FAIL"
    detail = ",".join(errors) if errors else "ok"
    return (
        f"globals_diagnostic {status} node={{node_id}} wake={wake_index} "
        f"ro=0x{rodata_value:08x} data=0x{data_before:08x}->0x{data_after:08x} "
        f"bss={bss_before}->{bss_after} detail={detail}"
    )


def main():
    while True:
        message = read_message()
        if message is None:
            break
        if message is INVALID_MESSAGE:
            continue

        if message.get(KEY_MSG_TYPE) != DATA:
            continue

        request_id = message.get(KEY_REQUEST_ID, 0)
        node_id = message.get(KEY_NODE_ID, "unknown")
        readings = message.get(KEY_READINGS)

        if isinstance(readings, dict):
            log_text = status_from_readings(readings).format(node_id=node_id)
        else:
            raw = message.get(KEY_DATA, b"")
            log_text = (
                f"globals_diagnostic FAIL node={node_id} "
                f"detail=missing_readings raw={raw.hex()}"
            )

        write_message({
            KEY_MSG_TYPE: LOG,
            2: "info",
            3: log_text,
        })
        write_message({
            KEY_MSG_TYPE: DATA_REPLY,
            KEY_REQUEST_ID: request_id,
            3: b"",
        })


if __name__ == "__main__":
    main()
