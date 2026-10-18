"""Text-frame envelope: JSON-RPC 2.0 shaped requests, responses and notifications.

Every text frame on the socket is exactly one of:

* ``Request``       — ``{"jsonrpc": "2.0", "id": 1, "method": "...", "params": {...}}``
* ``Notification``  — ``{"jsonrpc": "2.0", "method": "...", "params": {...}}`` (events, ``$/cancel``)
* ``SuccessResponse`` — ``{"jsonrpc": "2.0", "id": 1, "result": ...}``
* ``ErrorResponse``   — ``{"jsonrpc": "2.0", "id": 1 | null, "error": {"code", "message", "data"}}``

The envelope is validated here; ``params``/``result`` stay raw JSON values and are validated
against the per-method models in :mod:`spaceknife_protocol.methods` by the receiving side.
"""

from __future__ import annotations

from typing import Annotated, Any, Literal

from pydantic import BaseModel, ConfigDict, Discriminator, Field, Tag, TypeAdapter, ValidationError

from .errors import ProtocolError

# Ints must stay inside the IEEE-754 safe range so JavaScript clients never lose precision.
MAX_SAFE_INTEGER = 2**53 - 1

RequestId = (
    Annotated[int, Field(ge=0, le=MAX_SAFE_INTEGER)] | Annotated[str, Field(min_length=1, max_length=64)]
)


class _Envelope(BaseModel):
    model_config = ConfigDict(extra="forbid", frozen=True)

    jsonrpc: Literal["2.0"] = "2.0"


class Request(_Envelope):
    id: RequestId
    method: str = Field(min_length=1, max_length=128)
    params: dict[str, Any] | None = None


class Notification(_Envelope):
    method: str = Field(min_length=1, max_length=128)
    params: dict[str, Any] | None = None


class ErrorObject(BaseModel):
    model_config = ConfigDict(extra="forbid", frozen=True)

    code: int
    message: str
    data: Any | None = None


class SuccessResponse(_Envelope):
    id: RequestId
    result: Any = None


class ErrorResponse(_Envelope):
    id: RequestId | None
    error: ErrorObject


Message = Request | Notification | SuccessResponse | ErrorResponse


def _kind(value: Any) -> str | None:
    if isinstance(value, dict):
        if "method" in value:
            return "request" if "id" in value else "notification"
        if "error" in value:
            return "error"
        if "result" in value:
            return "success"
        return None
    if isinstance(value, BaseModel):
        return {
            Request: "request",
            Notification: "notification",
            ErrorResponse: "error",
            SuccessResponse: "success",
        }.get(type(value))
    return None


_MESSAGE_ADAPTER: TypeAdapter[Message] = TypeAdapter(
    Annotated[
        Annotated[Request, Tag("request")]
        | Annotated[Notification, Tag("notification")]
        | Annotated[SuccessResponse, Tag("success")]
        | Annotated[ErrorResponse, Tag("error")],
        Discriminator(_kind),
    ]
)


def parse_message(raw: str | bytes) -> Message:
    """Parse one text frame into an envelope model.

    Raises :class:`ProtocolError` if the frame is not valid JSON or not a valid envelope.
    """
    try:
        return _MESSAGE_ADAPTER.validate_json(raw)
    except ValidationError as exc:
        raise ProtocolError(_summarise(exc)) from exc


def dump_message(message: Message) -> str:
    """Serialise an envelope model to a compact JSON text frame.

    Optional members are omitted rather than sent as ``null`` (JSON-RPC forbids ``"params": null``),
    except where the spec requires them: ``result`` is always present on success and ``id`` is
    always present (possibly ``null``) on errors.
    """
    if isinstance(message, Request | Notification) and message.params is None:
        return message.model_dump_json(exclude={"params"})
    if isinstance(message, ErrorResponse) and message.error.data is None:
        return message.model_dump_json(exclude={"error": {"data"}})
    return message.model_dump_json()


def _summarise(exc: ValidationError) -> str:
    first = exc.errors(include_url=False)[0] if exc.errors() else {"msg": str(exc)}
    loc = ".".join(str(p) for p in first.get("loc", ()))
    return f"{first.get('msg', 'invalid message')}" + (f" at {loc}" if loc else "")
