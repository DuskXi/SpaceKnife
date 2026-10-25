"""Messages between the host and a plugin process (docs/plugins.md, 「子进程」).

Framing as the scan helper's: ``u32`` length (with the kind byte) + kind byte; ``J`` one JSON message,
``B`` one binary frame (:func:`spaceknife_protocol.binary.encode_frame`). A message with ``frames = n``
is preceded by its ``n`` binary frames. Both sides send requests and answer the other's (ids are per
direction); the host checks every message of the plugin against the models below.
"""

from __future__ import annotations

import json
import struct
from typing import Annotated, Any, Literal

from pydantic import BaseModel, ConfigDict, Field, TypeAdapter

KIND_JSON = 0x4A  # "J"
KIND_BINARY = 0x42  # "B"
HEADER = struct.Struct("<IB")
MAX_JSON = 16 << 20
MAX_BINARY = 256 << 20
MAX_ID = 2**53 - 1
WIRE_VERSION = 1
PIPE_PREFIX = "spaceknife-plugin-"
HEARTBEAT_S = 5.0


class Msg(BaseModel):
    model_config = ConfigDict(extra="forbid", frozen=True)


Id = Annotated[int, Field(ge=1, le=MAX_ID)]
Frames = Annotated[int, Field(ge=0, le=64)]


# --------------------------------------------------------------------------- plugin -> host


class Hello(Msg):
    type: Literal["hello"] = "hello"
    version: int = WIRE_VERSION
    token: str = Field(pattern=r"^[0-9a-f]{64}$")
    pid: int = Field(gt=0)
    sdk: str = ""


class Ready(Msg):
    """After ``init``: the plugin's code is imported (with its description when asked for)."""

    type: Literal["ready"] = "ready"
    description: dict[str, Any] | None = None
    error: dict[str, Any] | None = None


class Result(Msg):
    type: Literal["result"] = "result"
    id: Id
    value: Any = None
    frames: Frames = 0


class Error(Msg):
    type: Literal["error"] = "error"
    id: Id
    code: int
    message: str = Field(max_length=8192)
    data: Any = None


class Progress(Msg):
    type: Literal["progress"] = "progress"
    id: Id
    done: float
    total: float
    text: str = Field(default="", max_length=512)
    # the text in both languages (a T(...)): a background task's stays right after a language switch
    texts: dict[Literal["zh-CN", "en"], Annotated[str, Field(max_length=512)]] | None = None


class Chunk(Msg):
    """Part of a call's output: an export's text, a source's entries, a stream's item."""

    type: Literal["chunk"] = "chunk"
    id: Id
    value: Any = None
    frames: Frames = 0


class Event(Msg):
    type: Literal["event"] = "event"
    name: str = Field(min_length=1, max_length=24)
    payload: dict[str, Any] = Field(default_factory=dict)
    session: str | None = None


class State(Msg):
    type: Literal["state"] = "state"
    name: str = Field(min_length=1, max_length=24)
    value: dict[str, Any]


class Toast(Msg):
    type: Literal["toast"] = "toast"
    text: str = Field(max_length=512)
    tone: Literal["info", "ok", "warn", "danger"] = "info"
    session: str | None = None
    call: int | None = Field(default=None, ge=0, le=MAX_ID, description="The call that shows it")


class Clipboard(Msg):
    """``ctx.clipboard.write_text``: the app of ``session`` puts ``text`` on the clipboard."""

    type: Literal["clipboard"] = "clipboard"
    text: str = Field(max_length=1 << 20)
    session: str | None = None


class Request(Msg):
    type: Literal["request"] = "request"
    id: Id
    method: str = Field(pattern=r"^host\.[a-z_]{1,32}$")
    params: dict[str, Any] = Field(default_factory=dict)
    frames: Frames = 0


class Log(Msg):
    type: Literal["log"] = "log"
    level: Literal["DEBUG", "INFO", "WARNING", "ERROR"] = "INFO"
    message: str = Field(max_length=8192)


class ImageDrop(Msg):
    """The plugin let go of a tree image (it mapped a newer one)."""

    type: Literal["image_drop"] = "image_drop"
    key: str = Field(max_length=128)


class IoRelease(Msg):
    """``ctx.files`` is done with these disk-read permits (``host.io``)."""

    type: Literal["io_release"] = "io_release"
    permits: list[Annotated[int, Field(ge=1, le=MAX_ID)]] = Field(default_factory=list, max_length=64)


class Stuck(Msg):
    """Every worker thread is busy with a cancelled call that does not return: restart me."""

    type: Literal["stuck"] = "stuck"
    threads: int = Field(ge=0, le=1024)


class Heartbeat(Msg):
    type: Literal["heartbeat"] = "heartbeat"
    busy: list[int] = Field(default_factory=list, max_length=64)
    rss: int = 0


FromPlugin = Annotated[
    Hello
    | Ready
    | Result
    | Error
    | Progress
    | Chunk
    | Event
    | State
    | Toast
    | Clipboard
    | Request
    | Log
    | Heartbeat
    | Stuck
    | ImageDrop
    | IoRelease,
    Field(discriminator="type"),
]
FROM_PLUGIN: TypeAdapter[Any] = TypeAdapter(FromPlugin)


# --------------------------------------------------------------------------- host -> plugin (dicts)


def init_message(**fields: Any) -> dict[str, Any]:
    return {"type": "init", **fields}


def call_message(
    id: int, kind: str, name: str, params: Any, ctx: dict[str, Any], frames: int = 0
) -> dict[str, Any]:
    return {
        "type": "call",
        "id": id,
        "kind": kind,
        "name": name,
        "params": params,
        "ctx": ctx,
        "frames": frames,
    }


def reply_message(
    id: int, value: Any = None, error: dict[str, Any] | None = None, frames: int = 0
) -> dict[str, Any]:
    return {"type": "reply", "id": id, "value": value, "error": error, "frames": frames}


# --------------------------------------------------------------------------- framing


def header(kind: int, body_len: int) -> bytes:
    return HEADER.pack(body_len + 1, kind)


def json_bytes(message: dict[str, Any] | BaseModel) -> bytes:
    if isinstance(message, BaseModel):
        return message.model_dump_json().encode("utf-8")
    return json.dumps(message, ensure_ascii=False, separators=(",", ":")).encode("utf-8")


def frame_json(message: dict[str, Any] | BaseModel) -> list[bytes]:
    body = json_bytes(message)
    return [header(KIND_JSON, len(body)), body]


def frame_binary(data: bytes) -> list[bytes]:
    return [header(KIND_BINARY, len(data)), data]


def check_header(head: bytes | bytearray) -> tuple[int, int]:
    length, kind = HEADER.unpack(head)
    body = length - 1
    limit = MAX_JSON if kind == KIND_JSON else MAX_BINARY if kind == KIND_BINARY else -1
    if limit < 0:
        raise ValueError(f"unknown frame kind 0x{kind:02x}")
    if length < 1 or body > limit:
        raise ValueError(f"frame length {length} out of range")
    return kind, body


__all__ = [
    "FROM_PLUGIN",
    "HEADER",
    "HEARTBEAT_S",
    "KIND_BINARY",
    "KIND_JSON",
    "MAX_BINARY",
    "MAX_JSON",
    "PIPE_PREFIX",
    "WIRE_VERSION",
    "Chunk",
    "Clipboard",
    "Error",
    "Event",
    "Heartbeat",
    "Hello",
    "IoRelease",
    "Log",
    "Progress",
    "Ready",
    "Request",
    "Result",
    "State",
    "Stuck",
    "Toast",
    "call_message",
    "check_header",
    "frame_binary",
    "frame_json",
    "init_message",
    "reply_message",
]
