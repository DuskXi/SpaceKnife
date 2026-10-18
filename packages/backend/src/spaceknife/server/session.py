"""One WebSocket connection: handshake, request dispatch, cancellation, outbound queue."""

from __future__ import annotations

import asyncio
import hmac
import itertools
import secrets
from enum import IntEnum
from typing import TYPE_CHECKING, Any, NoReturn

from loguru import logger
from pydantic import BaseModel, ValidationError
from pydantic_core import PydanticSerializationError
from websockets.asyncio.server import ServerConnection
from websockets.exceptions import ConnectionClosed

from spaceknife_protocol import PROTOCOL_VERSION, SUBPROTOCOL, protocol_major
from spaceknife_protocol.envelope import (
    ErrorObject,
    ErrorResponse,
    Message,
    Notification,
    Request,
    RequestId,
    SuccessResponse,
    dump_message,
    parse_message,
)
from spaceknife_protocol.errors import CloseCode, ErrorCode, ProtocolError
from spaceknife_protocol.methods import CANCEL, SESSION_HELLO, EventSpec
from spaceknife_protocol.session import ClientInfo, HelloParams, HelloResult, Limits
from spaceknife_protocol.streams import CancelParams

from ..events import printable
from .rpc import CURRENT_REQUEST, Route, RpcError, not_found

if TYPE_CHECKING:
    from .app import Server
    from .topics import Subscription


class Priority(IntEnum):
    """Outbound ordering: responses first, then bulk data, then events."""

    RESPONSE = 0
    BULK = 1
    EVENT = 2


MAX_QUEUED_EVENTS = 10_000


class _Closed(Exception):
    """The connection was closed during the handshake."""


def _validation_details(exc: ValidationError) -> list[dict[str, Any]]:
    return [
        {"loc": [str(p) for p in err["loc"]], "msg": err["msg"], "type": err["type"]}
        for err in exc.errors(include_url=False, include_context=False, include_input=False)
    ]


def encode_message(message: Message) -> str:
    """:func:`dump_message` that survives lone surrogates (undecodable file names) in any string.

    pydantic refuses to write them as UTF-8; the rare frame holding one is rebuilt with every
    string made :func:`printable`, so the peer still gets the frame instead of silence.
    """
    try:
        return dump_message(message)
    except (PydanticSerializationError, UnicodeError):
        clean = _printable_tree(message.model_dump())  # python mode: JSON mode rejects such keys
        return dump_message(type(message).model_validate(clean))


def _printable_tree(value: Any) -> Any:
    if isinstance(value, str):
        return printable(value)
    if isinstance(value, dict):
        return {_printable_tree(k): _printable_tree(v) for k, v in value.items()}
    if isinstance(value, list | tuple):
        return [_printable_tree(v) for v in value]
    return value


class Session:
    def __init__(self, server: Server, ws: ServerConnection) -> None:
        self.server = server
        self.ws = ws
        self.id = secrets.token_hex(8)
        self.client: ClientInfo | None = None
        self.capabilities: frozenset[str] = frozenset()
        self._tasks: dict[RequestId, asyncio.Task[None]] = {}
        self._outbox: asyncio.PriorityQueue[tuple[int, int, str | bytes]] = asyncio.PriorityQueue()
        self._order = itertools.count()
        self._queued_events = 0
        self._subs: dict[str, Subscription] = {}
        self._closing = False

    # ------------------------------------------------------------------ lifecycle

    async def run(self) -> None:
        try:
            await self._handshake()
        except _Closed:
            return
        writer = asyncio.create_task(self._writer(), name=f"session-{self.id}-writer")
        try:
            async for raw in self.ws:
                await self._on_message(raw)
        except ConnectionClosed:
            pass
        finally:
            await self._teardown(writer)

    async def close(self, code: CloseCode = CloseCode.GOING_AWAY, reason: str = "") -> None:
        await self.ws.close(int(code), reason)

    async def _teardown(self, writer: asyncio.Task[None]) -> None:
        self._closing = True
        for sub in list(self._subs.values()):
            sub.close()
        self._subs.clear()
        tasks = list(self._tasks.values())
        for task in tasks:
            task.cancel()
        await asyncio.gather(*tasks, return_exceptions=True)
        writer.cancel()
        await asyncio.gather(writer, return_exceptions=True)
        if self.client is not None:
            logger.info("session {} closed ({} {})", self.id, self.client.kind, self.client.name)

    # ------------------------------------------------------------------ handshake

    async def _handshake(self) -> None:
        cfg = self.server.config
        if self.ws.subprotocol != SUBPROTOCOL:
            await self._reject(CloseCode.PROTOCOL_ERROR, f"subprotocol {SUBPROTOCOL!r} required")
        try:
            raw = await asyncio.wait_for(self.ws.recv(), timeout=cfg.hello_timeout)
        except TimeoutError:
            await self._reject(CloseCode.HANDSHAKE_TIMEOUT, "session.hello not received in time")
        except ConnectionClosed:
            raise _Closed from None
        if isinstance(raw, bytes):
            await self._reject(CloseCode.PROTOCOL_ERROR, "first frame must be a session.hello text frame")
        try:
            msg = parse_message(raw)
        except ProtocolError as exc:
            await self._send_now(
                ErrorResponse(id=None, error=ErrorObject(code=ErrorCode.PARSE_ERROR, message=str(exc)))
            )
            await self._reject(CloseCode.PROTOCOL_ERROR, "malformed handshake")
        if not isinstance(msg, Request) or msg.method != SESSION_HELLO.name:
            req_id = msg.id if isinstance(msg, Request) else None
            await self._send_now(
                ErrorResponse(
                    id=req_id,
                    error=ErrorObject(code=ErrorCode.NOT_HANDSHAKEN, message="session.hello must come first"),
                )
            )
            await self._reject(CloseCode.UNAUTHORIZED, "not handshaken")
        try:
            hello = HelloParams.model_validate(msg.params or {})
        except ValidationError as exc:
            await self._send_error(
                msg.id,
                ErrorCode.INVALID_PARAMS,
                "invalid session.hello params",
                _validation_details(exc),
                now=True,
            )
            await self._reject(CloseCode.PROTOCOL_ERROR, "invalid session.hello")
        if not hmac.compare_digest(hello.token.encode("utf-8"), cfg.token.encode("utf-8")):
            logger.warning("rejected connection with a wrong token from {}", self.ws.remote_address)
            await self._send_error(msg.id, ErrorCode.UNAUTHORIZED, "invalid token", now=True)
            await self._reject(CloseCode.UNAUTHORIZED, "invalid token")
        if protocol_major(hello.protocol) != protocol_major(PROTOCOL_VERSION):
            await self._send_error(
                msg.id,
                ErrorCode.PROTOCOL_MISMATCH,
                f"server speaks protocol {PROTOCOL_VERSION}, client sent {hello.protocol}",
                {"server": PROTOCOL_VERSION},
                now=True,
            )
            await self._reject(CloseCode.VERSION_MISMATCH, "protocol major version mismatch")

        self.client = hello.client
        self.capabilities = frozenset(hello.capabilities) & frozenset(self.server.capabilities)
        result = HelloResult(
            protocol=PROTOCOL_VERSION,
            server=self.server.server_info,
            session_id=self.id,
            capabilities=sorted(self.capabilities),
            schema_hash=self.server.schema_hash,
            limits=Limits(
                max_inbound_message_bytes=cfg.max_inbound_bytes,
                max_outbound_chunk_bytes=cfg.max_outbound_chunk_bytes,
                hello_timeout_s=cfg.hello_timeout,
            ),
        )
        await self._send_now(SuccessResponse(id=msg.id, result=result.model_dump(mode="json")))
        logger.info(
            "session {} opened: {} {} {} caps={}",
            self.id,
            hello.client.kind,
            hello.client.name,
            hello.client.version,
            ",".join(sorted(self.capabilities)) or "-",
        )

    async def _reject(self, code: CloseCode, reason: str) -> NoReturn:
        await self.ws.close(int(code), reason)
        raise _Closed(reason)

    async def _send_now(self, message: Message) -> None:
        try:
            await self.ws.send(encode_message(message))
        except ConnectionClosed:
            raise _Closed from None

    # ------------------------------------------------------------------ inbound

    async def _on_message(self, raw: str | bytes) -> None:
        if isinstance(raw, bytes):
            await self.close(CloseCode.PROTOCOL_ERROR, "clients must not send binary frames in protocol v1")
            return
        try:
            msg = parse_message(raw)
        except ProtocolError as exc:
            self._enqueue_error(None, ErrorCode.INVALID_REQUEST, str(exc))
            return
        if isinstance(msg, Request):
            self._start_request(msg)
        elif isinstance(msg, Notification):
            self._on_notification(msg)
        else:
            logger.debug("session {} ignored an unexpected response frame", self.id)

    def _start_request(self, msg: Request) -> None:
        if msg.method == SESSION_HELLO.name:
            self._enqueue_error(msg.id, ErrorCode.INVALID_REQUEST, "session already established")
            return
        if msg.id in self._tasks:
            self._enqueue_error(
                msg.id, ErrorCode.INVALID_REQUEST, f"request id {msg.id!r} is already in flight"
            )
            return
        route = self.server.router.get(msg.method)
        if route is None:
            err = not_found(msg.method)
            self._enqueue_error(msg.id, err.code, err.message)
            return
        task = asyncio.create_task(self._run_request(msg, route), name=f"rpc-{msg.method}-{msg.id}")
        self._tasks[msg.id] = task

    async def _run_request(self, msg: Request, route: Route) -> None:
        try:
            try:
                params = route.spec.params.model_validate(msg.params or {})
            except ValidationError as exc:
                self._enqueue_error(
                    msg.id, ErrorCode.INVALID_PARAMS, "invalid params", _validation_details(exc)
                )
                return
            CURRENT_REQUEST.set(msg.id)
            try:
                result: BaseModel = await route.handler(self, params)
            except asyncio.CancelledError:
                if self._closing:
                    raise
                self._enqueue_error(msg.id, ErrorCode.REQUEST_CANCELLED, "request cancelled")
                return
            except RpcError as exc:
                self._enqueue_error(msg.id, exc.code, exc.message, exc.data)
                return
            except Exception:
                logger.exception("handler for {} failed", msg.method)
                self._enqueue_error(msg.id, ErrorCode.INTERNAL_ERROR, "internal error")
                return
            try:
                frame = encode_message(SuccessResponse(id=msg.id, result=result.model_dump(mode="json")))
            except Exception:
                logger.exception("could not encode the result of {}", msg.method)
                self._enqueue_error(msg.id, ErrorCode.INTERNAL_ERROR, "internal error: unencodable result")
                return
            self._enqueue(Priority.RESPONSE, frame)
        finally:
            self._tasks.pop(msg.id, None)

    def _on_notification(self, msg: Notification) -> None:
        if msg.method == CANCEL.name:
            try:
                params = CancelParams.model_validate(msg.params or {})
            except ValidationError:
                logger.debug("session {} sent an invalid $/cancel", self.id)
                return
            task = self._tasks.get(params.id)
            if task is not None:
                task.cancel()
            return
        logger.debug("session {} sent unknown notification {}", self.id, msg.method)

    # ------------------------------------------------------------------ outbound

    def emit[E: BaseModel](self, spec: EventSpec[E], payload: E) -> None:
        """Queue a server → client event (dropped if the client is far behind)."""
        if self._closing:
            return
        if self._queued_events >= MAX_QUEUED_EVENTS:
            return
        try:
            frame = encode_message(Notification(method=spec.name, params=payload.model_dump(mode="json")))
        except Exception:
            logger.exception("could not encode a {} event", spec.name)
            return
        self._queued_events += 1
        self._enqueue(Priority.EVENT, frame)

    def send_binary(self, frame: bytes) -> None:
        """Queue a binary frame produced by :func:`spaceknife_protocol.binary.encode_frame`."""
        self._enqueue(Priority.BULK, frame)

    def outbox_stats(self) -> tuple[int, int]:
        """Queued messages and their bytes (diagnostics; call on the event loop)."""
        items = list(self._outbox._queue)  # type: ignore[attr-defined]  # asyncio internals: a list
        return len(items), sum(len(frame) for _, _, frame in items)

    def add_subscription(self, sub: Subscription) -> None:
        self._subs[sub.id] = sub

    def remove_subscription(self, sub_id: str) -> bool:
        sub = self._subs.pop(sub_id, None)
        if sub is None:
            return False
        sub.close()
        return True

    def _enqueue_error(self, req_id: RequestId | None, code: int, message: str, data: Any = None) -> None:
        frame = encode_message(
            ErrorResponse(id=req_id, error=ErrorObject(code=code, message=message, data=data))
        )
        self._enqueue(Priority.RESPONSE, frame)

    async def _send_error(
        self, req_id: RequestId, code: int, message: str, data: Any = None, *, now: bool = False
    ) -> None:
        response = ErrorResponse(id=req_id, error=ErrorObject(code=code, message=message, data=data))
        if now:
            await self._send_now(response)
        else:
            self._enqueue(Priority.RESPONSE, encode_message(response))

    def _enqueue(self, priority: Priority, frame: str | bytes) -> None:
        self._outbox.put_nowait((int(priority), next(self._order), frame))

    async def _writer(self) -> None:
        while True:
            priority, _, frame = await self._outbox.get()
            if priority == Priority.EVENT:
                self._queued_events -= 1
            try:
                await self.ws.send(frame)
            except ConnectionClosed:
                return
