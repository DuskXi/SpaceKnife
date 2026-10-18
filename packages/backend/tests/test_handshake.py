from __future__ import annotations

import json

import pytest
from websockets.asyncio.client import connect
from websockets.exceptions import ConnectionClosed, InvalidStatus

from spaceknife.server import Server
from spaceknife_client.connection import Connection, RemoteError
from spaceknife_protocol import ALL_CAPABILITIES, PROTOCOL_VERSION, SUBPROTOCOL
from spaceknife_protocol.errors import CloseCode, ErrorCode

from .conftest import CLIENT, TOKEN


def hello_frame(token: str = TOKEN, protocol: str = PROTOCOL_VERSION, req_id: int = 0) -> str:
    return json.dumps(
        {
            "jsonrpc": "2.0",
            "id": req_id,
            "method": "session.hello",
            "params": {
                "token": token,
                "protocol": protocol,
                "client": {"name": "raw", "version": "0", "kind": "test"},
                "capabilities": ["subscriptions"],
            },
        }
    )


async def test_successful_hello_negotiates_capabilities(server: Server) -> None:
    conn = await Connection.open(server.url, TOKEN, CLIENT, capabilities=["subscriptions", "made-up"])
    try:
        assert conn.hello.protocol == PROTOCOL_VERSION
        assert conn.hello.capabilities == ["subscriptions"]
        assert conn.hello.schema_hash == server.schema_hash
        assert conn.hello.session_id
        assert set(server.capabilities) == set(ALL_CAPABILITIES)
    finally:
        await conn.close()


async def test_wrong_token_is_rejected_with_4401(server: Server) -> None:
    async with connect(server.url, subprotocols=[SUBPROTOCOL]) as ws:  # type: ignore[list-item]
        await ws.send(hello_frame(token="x" * 32))
        reply = json.loads(await ws.recv())
        assert reply["error"]["code"] == ErrorCode.UNAUTHORIZED
        with pytest.raises(ConnectionClosed) as info:
            await ws.recv()
        assert info.value.rcvd is not None
        assert info.value.rcvd.code == CloseCode.UNAUTHORIZED


async def test_client_helper_raises_remote_error_on_bad_token(server: Server) -> None:
    with pytest.raises(RemoteError) as info:
        await Connection.open(server.url, "y" * 32, CLIENT)
    assert info.value.code == ErrorCode.UNAUTHORIZED


async def test_missing_hello_times_out_with_4408(server: Server) -> None:
    async with connect(server.url, subprotocols=[SUBPROTOCOL]) as ws:  # type: ignore[list-item]
        with pytest.raises(ConnectionClosed) as info:
            await ws.recv()
        assert info.value.rcvd is not None
        assert info.value.rcvd.code == CloseCode.HANDSHAKE_TIMEOUT


async def test_request_before_hello_is_rejected(server: Server) -> None:
    async with connect(server.url, subprotocols=[SUBPROTOCOL]) as ws:  # type: ignore[list-item]
        await ws.send('{"jsonrpc":"2.0","id":5,"method":"system.info"}')
        reply = json.loads(await ws.recv())
        assert reply["id"] == 5
        assert reply["error"]["code"] == ErrorCode.NOT_HANDSHAKEN
        with pytest.raises(ConnectionClosed) as info:
            await ws.recv()
        assert info.value.rcvd is not None
        assert info.value.rcvd.code == CloseCode.UNAUTHORIZED


async def test_major_version_mismatch_closes_with_4426(server: Server) -> None:
    async with connect(server.url, subprotocols=[SUBPROTOCOL]) as ws:  # type: ignore[list-item]
        await ws.send(hello_frame(protocol="2.0"))
        reply = json.loads(await ws.recv())
        assert reply["error"]["code"] == ErrorCode.PROTOCOL_MISMATCH
        with pytest.raises(ConnectionClosed) as info:
            await ws.recv()
        assert info.value.rcvd is not None
        assert info.value.rcvd.code == CloseCode.VERSION_MISMATCH


async def test_missing_subprotocol_is_rejected(server: Server) -> None:
    # websockets refuses the upgrade itself (HTTP 400); the session-level check is defence in depth.
    with pytest.raises(InvalidStatus) as info:
        async with connect(server.url):
            pass
    assert info.value.response.status_code == 400


async def test_wrong_path_gets_404(server: Server) -> None:
    url = server.url.removesuffix("/v1") + "/other"
    with pytest.raises(InvalidStatus) as info:
        async with connect(url, subprotocols=[SUBPROTOCOL]):  # type: ignore[list-item]
            pass
    assert info.value.response.status_code == 404


async def test_foreign_origin_gets_403(server: Server) -> None:
    with pytest.raises(InvalidStatus) as info:
        async with connect(server.url, subprotocols=[SUBPROTOCOL], origin="https://evil.example"):  # type: ignore[list-item,arg-type]
            pass
    assert info.value.response.status_code == 403


async def test_app_origin_is_allowed(server: Server) -> None:
    conn = await Connection.open(server.url, TOKEN, CLIENT, origin="app://spaceknife")
    await conn.close()
