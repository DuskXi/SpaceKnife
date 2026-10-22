"""Shared bits of the plugin tests: the test plugins (plugin_fixtures/) and a server with them."""

from __future__ import annotations

import shutil
from collections.abc import AsyncIterator
from contextlib import asynccontextmanager
from pathlib import Path

from spaceknife.server import Server, ServerConfig
from spaceknife_client.connection import Connection
from spaceknife_protocol.session import ClientInfo
from spaceknife_sdk.testing import MockHost, make_tree

FIXTURES = Path(__file__).parent / "plugin_fixtures"
TOKEN = "plugin-test-token-0123456789"
CLIENT = ClientInfo(name="pytest", version="0.0.0", kind="test")

TREE = make_tree(
    {
        "a.log": 500,
        "b.txt": 10,
        "movie.mp4": (5000, "2020-01-01"),
        "photo.jpg": 3000,
        "sub": {"c.log": 200, "d.bin": 1000, "e.dat": b"hello world"},
        "project": {".gradle": {"caches": {"x.jar": 4000}}},
    }
)


def fixture(name: str) -> Path:
    return FIXTURES / name


def mock(name: str, *, process: bool = False, grant: object = "requested") -> MockHost:
    host = MockHost.load(fixture(name), process=process, grant=grant)  # type: ignore[arg-type]
    host.scan(TREE)
    return host


def copy_plugin(name: str, plugins_dir: Path) -> Path:
    """A test plugin installed by hand into a plugins folder (the trust flow starts from there)."""
    target = plugins_dir / name
    shutil.copytree(fixture(name), target, ignore=shutil.ignore_patterns("__pycache__"))
    return target


@asynccontextmanager
async def plugin_server(*linked: str, runtime: str = "inprocess") -> AsyncIterator[Server]:
    """A listening server with test plugins linked (developer links, enabled with what they ask)."""
    srv = Server(ServerConfig(token=TOKEN, hello_timeout=0.5, ping_interval=None))
    srv.plugins.force_runtime = runtime  # type: ignore[assignment]
    await srv.start()
    try:
        for name in linked:
            await srv.plugins.link_dev(fixture(name))
        yield srv
    finally:
        await srv.close()


@asynccontextmanager
async def connect(srv: Server, capabilities: list[str] | None = None) -> AsyncIterator[Connection]:
    if capabilities is None:
        conn = await Connection.open(srv.url, TOKEN, CLIENT)
    else:
        conn = await Connection.open(srv.url, TOKEN, CLIENT, capabilities=capabilities)
    try:
        yield conn
    finally:
        await conn.close()


__all__ = [
    "CLIENT",
    "FIXTURES",
    "TOKEN",
    "TREE",
    "connect",
    "copy_plugin",
    "fixture",
    "mock",
    "plugin_server",
]
