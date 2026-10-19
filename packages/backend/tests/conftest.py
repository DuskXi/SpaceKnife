from __future__ import annotations

import asyncio
import os
import sys
from collections.abc import AsyncIterator, Iterator

import pytest

os.environ.setdefault("SPACEKNIFE_LANGUAGE", "zh-CN")  # M7.5: tests read the Chinese source text
from loguru import logger
from pydantic import Field

from spaceknife.logbus import LogHub
from spaceknife.server import Server, ServerConfig
from spaceknife.server.session import Session
from spaceknife_client.connection import Connection
from spaceknife_protocol.base import EmptyResult, Params
from spaceknife_protocol.methods import MethodSpec
from spaceknife_protocol.session import ClientInfo


def pytest_report_header() -> str:
    from spaceknife.io.priority import io_priority_is_low

    low = io_priority_is_low()
    return f"process I/O priority at start: {'unknown' if low is None else 'low' if low else 'normal'}"


@pytest.fixture(autouse=True)
def _io_priority_guard(request: pytest.FixtureRequest) -> Iterator[None]:
    """A test that leaves this process at low I/O priority fails, by name (Windows: the priority
    is per process and inherited by every later test and child)."""
    if sys.platform != "win32":
        yield
        return
    from spaceknife.io.priority import io_priority_is_low

    before = io_priority_is_low()
    yield
    if before is False and io_priority_is_low() is True:
        pytest.fail(f"{request.node.nodeid} left this process at low I/O priority", pytrace=False)


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
