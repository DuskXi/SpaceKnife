from __future__ import annotations

import asyncio
import json
import os

import numpy as np
import pytest
from loguru import logger
from websockets.asyncio.client import connect

from spaceknife.server import Server
from spaceknife_client.connection import Connection, RemoteError
from spaceknife_protocol import SUBPROTOCOL
from spaceknife_protocol.base import Empty, EmptyResult
from spaceknife_protocol.binary import BinaryFrameHeader, encode_frame
from spaceknife_protocol.errors import ErrorCode
from spaceknife_protocol.methods import APP_SHUTDOWN, EVENT_LOG, SYSTEM_INFO, SYSTEM_PING, MethodSpec
from spaceknife_protocol.streams import TOPIC_LOG, LogRecordEvent, LogTopicParams
from spaceknife_protocol.system import PingParams

from .conftest import CLIENT, TEST_SLEEP, TOKEN, SleepParams
from .test_handshake import hello_frame


async def test_ping_and_info(client: Connection) -> None:
    pong = await client.call(SYSTEM_PING, PingParams(payload="你好"))
    assert pong.payload == "你好"
    info = await client.call(SYSTEM_INFO, Empty())
    assert info.pid == os.getpid()
    assert info.sessions == 1


async def test_unknown_method(client: Connection) -> None:
    bogus = MethodSpec("nope.method", Empty, EmptyResult, "")
    with pytest.raises(RemoteError) as info:
        await client.call(bogus, Empty())
    assert info.value.code == ErrorCode.METHOD_NOT_FOUND


async def test_invalid_params_are_reported_with_details(client: Connection) -> None:
    sleep_bad = MethodSpec("test.sleep", PingParams, EmptyResult, "")
    with pytest.raises(RemoteError) as info:
        await client.call(sleep_bad, PingParams(payload="x"))
    assert info.value.code == ErrorCode.INVALID_PARAMS
    assert isinstance(info.value.data, list)
    assert info.value.data


async def test_cancel_in_flight_request(server: Server, client: Connection) -> None:
    task = asyncio.create_task(client.call(TEST_SLEEP, SleepParams(seconds=10)))
    await asyncio.sleep(0.1)
    task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await task
    session = next(iter(server.sessions))
    for _ in range(50):
        if not session._tasks:
            break
        await asyncio.sleep(0.02)
    assert not session._tasks
    pong = await client.call(SYSTEM_PING, PingParams(payload="still alive"))
    assert pong.payload == "still alive"


async def test_concurrent_requests_complete_independently(client: Connection) -> None:
    slow = asyncio.create_task(client.call(TEST_SLEEP, SleepParams(seconds=0.3)))
    fast = await client.call(SYSTEM_PING, PingParams(payload="fast"))
    assert fast.payload == "fast"
    assert not slow.done()
    assert isinstance(await slow, EmptyResult)


async def test_duplicate_request_id_is_rejected(server: Server) -> None:
    async with connect(server.url, subprotocols=[SUBPROTOCOL]) as ws:  # type: ignore[list-item]
        await ws.send(hello_frame())
        json.loads(await ws.recv())
        req = json.dumps({"jsonrpc": "2.0", "id": 9, "method": "test.sleep", "params": {"seconds": 0.5}})
        await ws.send(req)
        await ws.send(req)
        first = json.loads(await ws.recv())
        assert first["id"] == 9
        assert first["error"]["code"] == ErrorCode.INVALID_REQUEST
        second = json.loads(await ws.recv())
        assert second == {"jsonrpc": "2.0", "id": 9, "result": {}}


async def test_malformed_frame_after_handshake_gets_error(server: Server) -> None:
    async with connect(server.url, subprotocols=[SUBPROTOCOL]) as ws:  # type: ignore[list-item]
        await ws.send(hello_frame())
        json.loads(await ws.recv())
        await ws.send("{oops")
        reply = json.loads(await ws.recv())
        assert reply["id"] is None
        assert reply["error"]["code"] == ErrorCode.INVALID_REQUEST


async def test_log_subscription_streams_records(client: Connection) -> None:
    got: asyncio.Queue[LogRecordEvent] = asyncio.Queue()
    client.on_event(EVENT_LOG, got.put_nowait)
    sub = await client.subscribe(TOPIC_LOG, LogTopicParams(min_level="INFO", backlog=0))
    logger.debug("debug is below the floor")
    logger.warning("磁盘读取 {}", "warn-marker")

    async def ours() -> LogRecordEvent:
        # the server logs too, on its own schedule (e.g. a disk-load sampler unavailable on CI VMs)
        while True:
            event = await got.get()
            assert event.level != "DEBUG"
            if "warn-marker" in event.message:
                return event

    event = await asyncio.wait_for(ours(), timeout=2)
    assert event.sub == sub
    assert event.level == "WARNING"
    await client.unsubscribe(sub)
    with pytest.raises(RemoteError) as info:
        await client.unsubscribe(sub)
    assert info.value.code == ErrorCode.UNKNOWN_SUBSCRIPTION


async def test_subscribe_requires_negotiated_capability(server: Server) -> None:
    conn = await Connection.open(server.url, TOKEN, CLIENT, capabilities=[])
    try:
        with pytest.raises(RemoteError) as info:
            await conn.subscribe(TOPIC_LOG)
        assert info.value.code == ErrorCode.INVALID_REQUEST
    finally:
        await conn.close()


async def test_unknown_topic(client: Connection) -> None:
    with pytest.raises(RemoteError) as info:
        await client.subscribe("nope")
    assert info.value.code == ErrorCode.UNKNOWN_TOPIC


async def test_binary_frames_reach_the_client(server: Server, client: Connection) -> None:
    got: asyncio.Queue[tuple[BinaryFrameHeader, dict[str, np.ndarray]]] = asyncio.Queue()
    client.on_binary(lambda h, c: got.put_nowait((h, c)))
    session = next(iter(server.sessions))
    session.send_binary(
        encode_frame(stream="drawlist", layout="rect.v1", columns={"x": np.arange(3, dtype=np.float32)})
    )
    header, cols = await asyncio.wait_for(got.get(), timeout=2)
    assert header.stream == "drawlist"
    np.testing.assert_array_equal(cols["x"], [0, 1, 2])


async def test_app_shutdown_requests_server_stop(server: Server, client: Connection) -> None:
    result = await client.call(APP_SHUTDOWN, Empty())
    assert result.accepted
    await asyncio.wait_for(server.shutdown_requested.wait(), timeout=2)
