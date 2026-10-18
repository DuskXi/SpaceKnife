from __future__ import annotations

import asyncio
import os
from collections.abc import AsyncIterator

import pytest
from loguru import logger
from pydantic import Field

from spaceknife.logbus import LogHub
from spaceknife.server import Server, ServerConfig
from spaceknife.server.session import Session
from spaceknife_client.connection import Connection
from spaceknife_protocol.base import EmptyResult, Params
from spaceknife_protocol.methods import MethodSpec
from spaceknife_protocol.session import ClientInfo

TOKEN = "test-token-0123456789abcdef"
CLIENT = ClientInfo(name="pytest", version="0.0.0", kind="test")


class SleepParams(Params):
    seconds: float = Field(ge=0, le=30)


TEST_SLEEP = MethodSpec("test.sleep", SleepParams, EmptyResult, "Test-only: sleep, then return.")


@pytest.fixture
async def server(
    tmp_path_factory: pytest.TempPathFactory, monkeypatch: pytest.MonkeyPatch
) -> AsyncIterator[Server]:
    if "SPACEKNIFE_HOME" not in os.environ:  # never read or write the real user's settings
        monkeypatch.setenv("SPACEKNIFE_HOME", str(tmp_path_factory.mktemp("home")))
    hub = LogHub()
    sink = logger.add(hub.sink, level="DEBUG", format="{message}")
    srv = Server(ServerConfig(token=TOKEN, hello_timeout=0.5, ping_interval=None), log_hub=hub)

    async def sleep(session: Session, params: SleepParams) -> EmptyResult:
        await asyncio.sleep(params.seconds)
        return EmptyResult()

    srv.router.add(TEST_SLEEP, sleep)
    await srv.start()
    try:
        yield srv
    finally:
        await srv.close()
        logger.remove(sink)


@pytest.fixture
async def client(server: Server) -> AsyncIterator[Connection]:
    conn = await Connection.open(server.url, TOKEN, CLIENT)
    try:
        yield conn
    finally:
        await conn.close()
