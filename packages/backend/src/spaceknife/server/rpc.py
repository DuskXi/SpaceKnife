"""Method routing: binds protocol :class:`MethodSpec`\\ s to async handlers."""

from __future__ import annotations

from collections.abc import Awaitable, Callable
from contextvars import ContextVar
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any

from pydantic import BaseModel

from spaceknife_protocol.errors import ErrorCode
from spaceknife_protocol.methods import MethodSpec

if TYPE_CHECKING:
    from .session import Session


class RpcError(Exception):
    """Raise from a handler to answer the request with a JSON-RPC error."""

    def __init__(self, code: int, message: str, data: Any = None) -> None:
        super().__init__(message)
        self.code = int(code)
        self.message = message
        self.data = data


Handler = Callable[["Session", Any], Awaitable[BaseModel]]

# Id of the request a handler is answering (each request runs in its own task, so this is per request).
CURRENT_REQUEST: ContextVar[int | str | None] = ContextVar("current_request", default=None)


@dataclass(frozen=True, slots=True)
class Route:
    spec: MethodSpec[Any, Any]
    handler: Handler


class Router:
    def __init__(self) -> None:
        self._routes: dict[str, Route] = {}

    def add[P: BaseModel, R: BaseModel](
        self, spec: MethodSpec[P, R], handler: Callable[[Session, P], Awaitable[R]]
    ) -> None:
        if spec.name in self._routes:
            raise ValueError(f"method {spec.name!r} registered twice")
        self._routes[spec.name] = Route(spec, handler)

    def method[P: BaseModel, R: BaseModel](
        self, spec: MethodSpec[P, R]
    ) -> Callable[[Callable[[Session, P], Awaitable[R]]], Callable[[Session, P], Awaitable[R]]]:
        def register(fn: Callable[[Session, P], Awaitable[R]]) -> Callable[[Session, P], Awaitable[R]]:
            self.add(spec, fn)
            return fn

        return register

    def get(self, name: str) -> Route | None:
        return self._routes.get(name)

    def names(self) -> list[str]:
        return sorted(self._routes)


def not_found(name: str) -> RpcError:
    return RpcError(ErrorCode.METHOD_NOT_FOUND, f"method not found: {name}")
