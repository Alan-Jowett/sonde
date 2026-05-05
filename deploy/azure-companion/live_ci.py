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


DEFAULT_MESSAGE_TIMEOUT_SECS = 180
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
    parser.add_argument("--connect-timeout-secs", type=int)
    parser.add_argument("--message-timeout-secs", type=int, default=DEFAULT_MESSAGE_TIMEOUT_SECS)
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
        self._server: asyncio.AbstractServer | None = None
        self._reader: asyncio.StreamReader | None = None
        self._writer: asyncio.StreamWriter | None = None
        self._downstream_payloads: asyncio.Queue[bytes] = asyncio.Queue()

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
                await self._downstream_payloads.put(payload)
        finally:
            writer.close()
            with contextlib.suppress(Exception):
                await writer.wait_closed()

    async def wait_connected(self, timeout_secs: int) -> None:
        await asyncio.wait_for(self.connected.wait(), timeout=timeout_secs)

    async def send_upstream(self, payload: bytes) -> None:
        if self._writer is None:
            raise RuntimeError("connector harness has no connected writer")
        await write_framed(self._writer, payload)

    async def wait_downstream(self, timeout_secs: int) -> bytes:
        return await asyncio.wait_for(self._downstream_payloads.get(), timeout=timeout_secs)

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


async def receive_one_body(
    client: ServiceBusClient, queue_name: str, timeout_secs: int
) -> bytes:
    deadline = time.monotonic() + timeout_secs
    async with client.get_queue_receiver(queue_name=queue_name, max_wait_time=5) as receiver:
        while time.monotonic() < deadline:
            messages = await receiver.receive_messages(max_message_count=1, max_wait_time=5)
            if messages:
                message = messages[0]
                body = service_bus_body_bytes(message)
                await receiver.complete_message(message)
                return body
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
    message_timeout_secs: int,
) -> None:
    upstream_payload = b"ci-live-upstream-payload"
    downstream_payload = b"ci-live-downstream-payload"

    await harness.send_upstream(upstream_payload)
    actual_upstream = await receive_one_body(client, upstream_queue, message_timeout_secs)
    if actual_upstream != upstream_payload:
        raise RuntimeError(
            f"upstream payload mismatch: expected {upstream_payload!r}, got {actual_upstream!r}"
        )

    async with client.get_queue_sender(queue_name=downstream_queue) as sender:
        await sender.send_messages(ServiceBusMessage(downstream_payload))

    actual_downstream = await harness.wait_downstream(message_timeout_secs)
    if actual_downstream != downstream_payload:
        raise RuntimeError(
            f"downstream payload mismatch: expected {downstream_payload!r}, got {actual_downstream!r}"
        )

    await expect_queue_empty(client, downstream_queue)


async def run_failure_path(
    harness: ConnectorHarness,
    client: ServiceBusClient,
    downstream_queue: str,
    companion: asyncio.subprocess.Process,
    message_timeout_secs: int,
) -> None:
    failed_handoff_payload = b"ci-live-downstream-write-failure"
    await harness.close_peer()
    async with client.get_queue_sender(queue_name=downstream_queue) as sender:
        await sender.send_messages(ServiceBusMessage(failed_handoff_payload))

    try:
        await asyncio.wait_for(companion.wait(), timeout=message_timeout_secs)
    except asyncio.TimeoutError as exc:
        raise RuntimeError("companion did not exit after failed downstream handoff") from exc
    if companion.returncode == 0:
        raise RuntimeError("companion unexpectedly succeeded after failed downstream handoff")

    actual_payload = await receive_one_body(client, downstream_queue, message_timeout_secs)
    if actual_payload != failed_handoff_payload:
        raise RuntimeError("downstream message was altered before re-delivery after failure")


async def async_main() -> int:
    args = parse_args()
    namespace = normalize_namespace(args.namespace)
    connect_timeout_secs = (
        args.connect_timeout_secs
        if args.connect_timeout_secs is not None
        else args.message_timeout_secs
    )

    socket_path = Path(args.connector_socket)
    socket_path.parent.mkdir(parents=True, exist_ok=True)

    harness = ConnectorHarness(socket_path)
    stdout_lines: list[str] = []
    stderr_lines: list[str] = []
    companion: asyncio.subprocess.Process | None = None
    stdout_task: asyncio.Task[None] | None = None
    stderr_task: asyncio.Task[None] | None = None
    credential: AzureCliCredential | None = None

    try:
        await harness.start()
        credential = AzureCliCredential()
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

        await harness.wait_connected(connect_timeout_secs)
        print("connector harness connected", flush=True)

        async with ServiceBusClient(namespace, credential=credential, logging_enable=False) as client:
            await run_success_path(
                harness,
                client,
                args.upstream_queue,
                args.downstream_queue,
                args.message_timeout_secs,
            )
            print("success path passed", flush=True)

            await run_failure_path(
                harness,
                client,
                args.downstream_queue,
                companion,
                args.message_timeout_secs,
            )
            print("failure path passed", flush=True)

        return 0
    finally:
        if companion is not None:
            await terminate_process(companion)
        await asyncio.gather(
            *[
                task
                for task in (stdout_task, stderr_task)
                if task is not None
            ],
            return_exceptions=True,
        )
        await harness.stop()
        if credential is not None:
            await credential.close()


def main() -> int:
    try:
        return asyncio.run(async_main())
    except Exception as exc:  # noqa: BLE001
        print(f"live Azure validation failed: {exc}", file=sys.stderr, flush=True)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
