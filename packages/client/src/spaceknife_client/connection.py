"""Asyncio client for protocol v1, shared by the native frontends, the CLI and the tests.

Typical use (on the client core thread's event loop)::

    conn = await Connection.open(url, token, ClientInfo(name="qt", version="0.1.0", kind="qt"))
    info = await conn.call(SYSTEM_INFO, Empty())
    sub = await conn.subscribe(TOPIC_LOG, LogTopicParams(min_level="INFO"))
    conn.on_event(EVENT_LOG, handle_log)
"""

from __future__ import annotations

import asyncio
import contextlib
import itertools
from collections import defaultdict
from collections.abc import Callable, Sequence
from typing import Any

import numpy as np
from pydantic import BaseModel
from websockets.asyncio.client import ClientConnection, connect
from websockets.exceptions import ConnectionClosed
from websockets.typing import Origin, Subprotocol

from spaceknife_protocol import KNOWN_CAPABILITIES, PROTOCOL_VERSION, SUBPROTOCOL
from spaceknife_protocol.binary import BinaryFrameHeader, decode_frame
from spaceknife_protocol.envelope import (
    ErrorObject,
    ErrorResponse,
    Notification,
    Request,
    RequestId,
    SuccessResponse,
    dump_message,
    parse_message,
)
from spaceknife_protocol.errors import ProtocolError
from spaceknife_protocol.methods import CANCEL, SESSION_HELLO, SUBSCRIBE, UNSUBSCRIBE, EventSpec, MethodSpec
from spaceknife_protocol.session import ClientInfo, HelloParams, HelloResult
from spaceknife_protocol.streams import CancelParams, SubscribeParams, UnsubscribeParams

EventHandler = Callable[[Any], None]
BinaryHandler = Callable[[BinaryFrameHeader, dict[str, np.ndarray]], None]


class RemoteError(Exception):
    """The server answered a request with a JSON-RPC error."""

    def __init__(self, error: ErrorObject) -> None:
        super().__init__(f"{error.code}: {error.message}")
        self.code = error.code
        self.message = error.message
        self.data = error.data


class Connection:
    def __init__(self, ws: ClientConnection, hello: HelloResult) -> None:
        self.ws = ws
        self.hello = hello
        self._ids = itertools.count(1)
        self._pending: dict[RequestId, asyncio.Future[Any]] = {}
        self._handlers: dict[str, list[EventHandler]] = defaultdict(list)
        self._binary: list[BinaryHandler] = []
        self._reader: asyncio.Task[None] | None = None
        self.closed = asyncio.Event()

    # ------------------------------------------------------------------ setup

    @classmethod
    async def open(
        cls,
        url: str,
        token: str,
        client: ClientInfo,
        *,
        capabilities: Sequence[str] = KNOWN_CAPABILITIES,
        protocol: str = PROTOCOL_VERSION,
        max_size: int = 64 << 20,
        origin: str | None = None,
    ) -> Connection:
        ws = await connect(
            url,
            subprotocols=[Subprotocol(SUBPROTOCOL)],
            compression=None,
            max_size=max_size,
            origin=Origin(origin) if origin is not None else None,
        )
        hello = HelloParams(token=token, protocol=protocol, client=client, capabilities=list(capabilities))
        await ws.send(
            dump_message(Request(id=0, method=SESSION_HELLO.name, params=hello.model_dump(mode="json")))
        )
        try:
            reply = parse_message(await ws.recv())
        except ConnectionClosed:
            raise
        if isinstance(reply, ErrorResponse):
            await ws.close()
            raise RemoteError(reply.error)
        if not isinstance(reply, SuccessResponse):
            await ws.close()
            raise ProtocolError("unexpected reply to session.hello")
        conn = cls(ws, HelloResult.model_validate(reply.result))
        conn._reader = asyncio.create_task(conn._read_loop(), name="spaceknife-client-reader")
        return conn

    async def close(self) -> None:
        await self.ws.close()
        if self._reader is not None:
            with contextlib.suppress(asyncio.CancelledError):
                await self._reader

    async def __aenter__(self) -> Connection:
        return self

    async def __aexit__(self, *exc: object) -> None:
        await self.close()

    # ------------------------------------------------------------------ requests

    async def call[P: BaseModel, R: BaseModel](self, spec: MethodSpec[P, R], params: P) -> R:
        """Send a request and await its typed result. Cancelling the awaiting task sends ``$/cancel``."""
        req_id = next(self._ids)
        fut: asyncio.Future[Any] = asyncio.get_running_loop().create_future()
        self._pending[req_id] = fut
        await self.ws.send(
            dump_message(Request(id=req_id, method=spec.name, params=params.model_dump(mode="json")))
        )
        try:
            raw = await fut
        except asyncio.CancelledError:
            with contextlib.suppress(ConnectionClosed):
                await self.notify_cancel(req_id)
            raise
        finally:
            self._pending.pop(req_id, None)
        return spec.result.model_validate(raw)

    async def call_raw(self, method: str, params: dict[str, Any] | None = None) -> Any:
        """A request by name with JSON params (a plugin's ``plugin.<id>.<name>``): the raw result."""
        req_id = next(self._ids)
        fut: asyncio.Future[Any] = asyncio.get_running_loop().create_future()
        self._pending[req_id] = fut
        await self.ws.send(dump_message(Request(id=req_id, method=method, params=params or {})))
        try:
            return await fut
        except asyncio.CancelledError:
            with contextlib.suppress(ConnectionClosed):
                await self.notify_cancel(req_id)
            raise
        finally:
            self._pending.pop(req_id, None)

    def on_raw(self, name: str, handler: Callable[[dict[str, Any]], None]) -> None:
        """Every notification named ``name`` (a plugin's events, topics) with its raw params."""
        self._handlers[name].append(handler)

    async def notify_cancel(self, req_id: RequestId) -> None:
        params = CancelParams(id=req_id).model_dump(mode="json")
        await self.ws.send(dump_message(Notification(method=CANCEL.name, params=params)))

    async def send_raw(self, frame: str | bytes) -> None:
        """Escape hatch for tests: send an arbitrary frame."""
        await self.ws.send(frame)

    # ------------------------------------------------------------------ events

    def on_event[E: BaseModel](self, spec: EventSpec[E], handler: Callable[[E], None]) -> None:
        self._handlers[spec.name].append(lambda raw: handler(spec.payload.model_validate(raw)))

    def on_binary(self, handler: BinaryHandler) -> None:
        self._binary.append(handler)

    async def subscribe(
        self, topic: str, params: BaseModel | None = None, rate_hz: float | None = None
    ) -> str:
        p = SubscribeParams(
            topic=topic, params=params.model_dump(mode="json") if params else None, rate_hz=rate_hz
        )
        return (await self.call(SUBSCRIBE, p)).sub

    async def unsubscribe(self, sub: str) -> None:
        await self.call(UNSUBSCRIBE, UnsubscribeParams(sub=sub))

    # ------------------------------------------------------------------ reader

    async def _read_loop(self) -> None:
        try:
            async for raw in self.ws:
                if isinstance(raw, bytes):
                    header, cols = decode_frame(raw)
                    for bin_handler in self._binary:
                        bin_handler(header, cols)
                    continue
                msg = parse_message(raw)
                if isinstance(msg, SuccessResponse):
                    self._resolve(msg.id, msg.result, None)
                elif isinstance(msg, ErrorResponse):
                    if msg.id is not None:
                        self._resolve(msg.id, None, msg.error)
                elif isinstance(msg, Notification):
                    for handler in self._handlers.get(msg.method, ()):
                        handler(msg.params or {})
        except ConnectionClosed:
            pass
        finally:
            for fut in self._pending.values():
                if not fut.done():
                    fut.set_exception(ConnectionError("connection closed"))
            self.closed.set()

    def _resolve(self, req_id: RequestId, result: Any, error: ErrorObject | None) -> None:
        fut = self._pending.get(req_id)
        if fut is None or fut.done():
            return
        if error is None:
            fut.set_result(result)
        else:
            # REQUEST_CANCELLED here was the server's idea (a newer request superseded this one): a
            # call the client cancelled itself is no longer pending. Raising CancelledError into a
            # task nobody cancelled would read as a cancellation of that task.
            fut.set_exception(RemoteError(error))
