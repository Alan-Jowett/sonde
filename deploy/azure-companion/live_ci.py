#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 sonde contributors

from __future__ import annotations

import argparse
import asyncio
import contextlib
import sys
import time
from pathlib import Path

from azure.identity.aio import AzureCliCredential
from azure.servicebus import ServiceBusMessage
from azure.servicebus.aio import ServiceBusClient


CONNECT_TIMEOUT_SECS = 30
MESSAGE_TIMEOUT_SECS = 60
CONNECTOR_MAX_FRAME_LENGTH = 1_048_576


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Run live Azure companion validation against Service Bus."
    )
    parser.add_argument("--companion-bin", required=True)
    parser.add_argument("--state-dir", required=True)
    parser.add_argument("--connector-socket", required=True)
    parser.add_argument("--namespace", required=True)
    parser.add_argument("--upstream-queue", required=True)
    parser.add_argument("--downstream-queue", required=True)
    return parser.parse_args()


def normalize_namespace(namespace: str) -> str:
    namespace = namespace.strip()
    if namespace.endswith(".servicebus.windows.net"):
        return namespace
    return f"{namespace}.servicebus.windows.net"


async def write_framed(writer: asyncio.StreamWriter, payload: bytes) -> None:
    writer.write(len(payload).to_bytes(4, "big"))
    writer.write(payload)
    await writer.drain()


async def read_framed(reader: asyncio.StreamReader) -> bytes | None:
    try:
        length_bytes = await reader.readexactly(4)
    except asyncio.IncompleteReadError as exc:
        if exc.partial:
            raise RuntimeError("truncated connector frame length prefix") from exc
        return None
    frame_length = int.from_bytes(length_bytes, "big")
    if frame_length > CONNECTOR_MAX_FRAME_LENGTH:
        raise RuntimeError(
            f"connector frame length {frame_length} exceeds max {CONNECTOR_MAX_FRAME_LENGTH}"
        )
    return await reader.readexactly(frame_length)


def service_bus_body_bytes(message) -> bytes:
    return b"".join(
        chunk if isinstance(chunk, (bytes, bytearray)) else bytes(chunk)
        for chunk in message.body
    )


class ConnectorHarness:
    def __init__(self, socket_path: Path) -> None:
        self.socket_path = socket_path
        self.connected = asyncio.Event()
        self.received_frame = asyncio.Event()
        self._server: asyncio.AbstractServer | None = None
        self._reader: asyncio.StreamReader | None = None
        self._writer: asyncio.StreamWriter | None = None
        self._downstream_payloads: list[bytes] = []

    async def start(self) -> None:
        if self.socket_path.exists():
            self.socket_path.unlink()
        self._server = await asyncio.start_unix_server(self._handle_client, path=str(self.socket_path))

    async def _handle_client(
        self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter
    ) -> None:
        self._reader = reader
        self._writer = writer
        self.connected.set()
        try:
            while True:
                payload = await read_framed(reader)
                if payload is None:
                    break
                self._downstream_payloads.append(payload)
                self.received_frame.set()
        finally:
            writer.close()
            with contextlib.suppress(Exception):
                await writer.wait_closed()

    async def wait_connected(self) -> None:
        await asyncio.wait_for(self.connected.wait(), timeout=CONNECT_TIMEOUT_SECS)

    async def send_upstream(self, payload: bytes) -> None:
        if self._writer is None:
            raise RuntimeError("connector harness has no connected writer")
        await write_framed(self._writer, payload)

    async def wait_downstream(self) -> bytes:
        await asyncio.wait_for(self.received_frame.wait(), timeout=MESSAGE_TIMEOUT_SECS)
        if not self._downstream_payloads:
            raise RuntimeError("connector harness did not capture a downstream payload")
        return self._downstream_payloads.pop(0)

    async def close_peer(self) -> None:
        if self._writer is not None:
            self._writer.close()
            with contextlib.suppress(Exception):
                await self._writer.wait_closed()

    async def stop(self) -> None:
        if self._server is not None:
            self._server.close()
            await self._server.wait_closed()
        if self.socket_path.exists():
            self.socket_path.unlink()


async def receive_one(
    client: ServiceBusClient, queue_name: str, timeout_secs: int = MESSAGE_TIMEOUT_SECS
):
    deadline = time.monotonic() + timeout_secs
    async with client.get_queue_receiver(queue_name=queue_name, max_wait_time=5) as receiver:
        while time.monotonic() < deadline:
            messages = await receiver.receive_messages(max_message_count=1, max_wait_time=5)
            if messages:
                return receiver, messages[0]
    raise RuntimeError(f"timed out waiting for a message on queue `{queue_name}`")


async def expect_queue_empty(
    client: ServiceBusClient, queue_name: str, timeout_secs: int = 10
) -> None:
    async with client.get_queue_receiver(queue_name=queue_name, max_wait_time=5) as receiver:
        messages = await receiver.receive_messages(max_message_count=1, max_wait_time=timeout_secs)
        if messages:
            first = messages[0]
            await receiver.abandon_message(first)
            raise RuntimeError(f"expected queue `{queue_name}` to be empty after settlement")


async def capture_stream(prefix: str, stream: asyncio.StreamReader, sink: list[str]) -> None:
    while True:
        line = await stream.readline()
        if not line:
            return
        text = line.decode("utf-8", errors="replace")
        sink.append(text)
        print(f"{prefix}{text}", end="", flush=True)


async def terminate_process(process: asyncio.subprocess.Process) -> int:
    if process.returncode is not None:
        return process.returncode
    process.terminate()
    try:
        await asyncio.wait_for(process.wait(), timeout=10)
    except asyncio.TimeoutError:
        process.kill()
        await process.wait()
    return process.returncode


async def run_success_path(
    harness: ConnectorHarness,
    client: ServiceBusClient,
    upstream_queue: str,
    downstream_queue: str,
) -> None:
    upstream_payload = b"ci-live-upstream-payload"
    downstream_payload = b"ci-live-downstream-payload"

    await harness.send_upstream(upstream_payload)
    upstream_receiver, upstream_message = await receive_one(client, upstream_queue)
    actual_upstream = service_bus_body_bytes(upstream_message)
    if actual_upstream != upstream_payload:
        raise RuntimeError(
            f"upstream payload mismatch: expected {upstream_payload!r}, got {actual_upstream!r}"
        )
    await upstream_receiver.complete_message(upstream_message)

    async with client.get_queue_sender(queue_name=downstream_queue) as sender:
        await sender.send_messages(ServiceBusMessage(downstream_payload))

    actual_downstream = await harness.wait_downstream()
    if actual_downstream != downstream_payload:
        raise RuntimeError(
            f"downstream payload mismatch: expected {downstream_payload!r}, got {actual_downstream!r}"
        )

    await expect_queue_empty(client, downstream_queue)


async def run_failure_path(
    client: ServiceBusClient, downstream_queue: str, companion: asyncio.subprocess.Process
) -> None:
    oversized_payload = b"x" * (CONNECTOR_MAX_FRAME_LENGTH + 1)
    async with client.get_queue_sender(queue_name=downstream_queue) as sender:
        await sender.send_messages(ServiceBusMessage(oversized_payload))

    try:
        await asyncio.wait_for(companion.wait(), timeout=MESSAGE_TIMEOUT_SECS)
    except asyncio.TimeoutError as exc:
        raise RuntimeError("companion did not exit after oversized downstream payload") from exc
    if companion.returncode == 0:
        raise RuntimeError("companion unexpectedly succeeded after oversized downstream payload")

    downstream_receiver, downstream_message = await receive_one(client, downstream_queue)
    actual_payload = service_bus_body_bytes(downstream_message)
    if actual_payload != oversized_payload:
        raise RuntimeError("downstream message was altered before re-delivery after failure")
    await downstream_receiver.complete_message(downstream_message)


async def async_main() -> int:
    args = parse_args()
    namespace = normalize_namespace(args.namespace)

    socket_path = Path(args.connector_socket)
    socket_path.parent.mkdir(parents=True, exist_ok=True)

    harness = ConnectorHarness(socket_path)
    stdout_lines: list[str] = []
    stderr_lines: list[str] = []
    companion: asyncio.subprocess.Process | None = None

    await harness.start()
    credential = AzureCliCredential()
    client = ServiceBusClient(namespace, credential=credential, logging_enable=False)

    try:
        companion = await asyncio.create_subprocess_exec(
            args.companion_bin,
            "--connector-socket",
            args.connector_socket,
            "--state-dir",
            args.state_dir,
            "run",
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
        stdout_task = asyncio.create_task(
            capture_stream("[companion stdout] ", companion.stdout, stdout_lines)
        )
        stderr_task = asyncio.create_task(
            capture_stream("[companion stderr] ", companion.stderr, stderr_lines)
        )

        await harness.wait_connected()
        print("connector harness connected", flush=True)

        async with client:
            await run_success_path(harness, client, args.upstream_queue, args.downstream_queue)
            print("success path passed", flush=True)

            await run_failure_path(client, args.downstream_queue, companion)
            print("failure path passed", flush=True)

        await stdout_task
        await stderr_task
        return 0
    finally:
        if companion is not None:
            await terminate_process(companion)
        await harness.stop()
        await credential.close()


def main() -> int:
    try:
        return asyncio.run(async_main())
    except Exception as exc:  # noqa: BLE001
        print(f"live Azure validation failed: {exc}", file=sys.stderr, flush=True)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
