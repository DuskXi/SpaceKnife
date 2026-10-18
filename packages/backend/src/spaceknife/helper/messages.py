"""Wire format between the backend and the helper (docs/native.md 3).

A frame is ``u32 little-endian length`` + payload. The payload's first byte says what follows:
``J`` one JSON control message (the pydantic models below), ``B`` one bulk binary frame from
:func:`spaceknife_protocol.binary.encode_frame` (MFT columns). The backend sends requests; the
helper sends ``hello`` first, then for each request any number of ``progress`` messages and
binary frames (scans announce them with ``begin``), closed by exactly one ``response`` or
``error`` with the request's id.
"""

from __future__ import annotations

import struct
from typing import Annotated, Literal

from pydantic import BaseModel, ConfigDict, Field, StringConstraints, TypeAdapter

PROTOCOL_VERSION = 1
KIND_JSON = 0x4A  # "J"
KIND_BINARY = 0x42  # "B"
HEADER = struct.Struct("<IB")  # payload length (kind byte included), kind
MAX_CONTROL_BYTES = 1 << 20
MAX_BINARY_BYTES = 1 << 30
MAX_ID = 2**53 - 1

MFT_STREAM = "mft"
MFT_LAYOUT = "mft.v1"
DEFAULT_BATCH_ROWS = 1_000_000
MAX_BATCH_ROWS = 4_000_000

Volume = Annotated[str, StringConstraints(pattern=r"^[A-Za-z]:$")]
BatchRows = Annotated[int, Field(ge=1, le=MAX_BATCH_ROWS)]
Duty = Annotated[float, Field(gt=0, le=1)]  # share of time the $MFT reader may be busy (I/O limit)
Scalar = bool | int | float | str | None


class FrameError(Exception):
    """The byte stream is not a valid frame sequence; the connection cannot continue."""


class Message(BaseModel):
    """Both ends run the same code, so every message is strict and closed."""

    model_config = ConfigDict(extra="forbid", frozen=True, strict=True)


# --------------------------------------------------------------------------- requests (backend → helper)


class Ping(Message):
    op: Literal["ping"] = "ping"


class VolumeInfo(Message):
    op: Literal["volume_info"] = "volume_info"
    volume: Volume


class ScanMft(Message):
    op: Literal["scan_mft"] = "scan_mft"
    volume: Volume
    batch_rows: BatchRows = DEFAULT_BATCH_ROWS
    duty: Duty = 1.0


class ScanMftImage(Message):
    """``scan_mft`` on an NTFS volume image file (``mft.read_mft_image``).

    Test-only: refused by an elevated helper, which must never read arbitrary files.
    """

    op: Literal["scan_mft_image"] = "scan_mft_image"
    path: str = Field(min_length=1, max_length=32767)
    batch_rows: BatchRows = DEFAULT_BATCH_ROWS
    duty: Duty = 1.0


class Shutdown(Message):
    op: Literal["shutdown"] = "shutdown"


Call = Annotated[Ping | VolumeInfo | ScanMft | ScanMftImage | Shutdown, Field(discriminator="op")]


class Request(Message):
    type: Literal["request"] = "request"
    id: int = Field(ge=1, le=MAX_ID)
    call: Call


# --------------------------------------------------------------------------- replies (helper → backend)


class Hello(Message):
    type: Literal["hello"] = "hello"
    version: int
    token: str = Field(pattern=r"^[0-9a-f]{64}$")
    pid: int = Field(gt=0)
    elevated: bool


class Progress(Message):
    """Heartbeat while a long operation runs (also keeps the client's inactivity timeout fed)."""

    type: Literal["progress"] = "progress"
    id: int = Field(ge=1, le=MAX_ID)
    phase: Literal["scan", "send"]
    elapsed: float = Field(ge=0)
    done: int = Field(default=0, ge=0, description="Bytes of $MFT parsed so far (phase scan)")
    total: int = Field(default=0, ge=0, description="Bytes of $MFT to parse; 0: not known yet")


class Begin(Message):
    """Sent before the bulk frames of a scan, so the client can allocate the table once."""

    type: Literal["begin"] = "begin"
    id: int = Field(ge=1, le=MAX_ID)
    rows: int = Field(ge=0)
    frames: int = Field(ge=1)


class Response(Message):
    """``result``: the operation's result (for scans: geometry, row/frame counts, timings);
    ``extras``: scans only, the scalar entries of the parsed MftTable (``bad_records`` …)."""

    type: Literal["response"] = "response"
    id: int = Field(ge=1, le=MAX_ID)
    result: dict[str, Scalar] = Field(default_factory=dict)
    extras: dict[str, Scalar] = Field(default_factory=dict)


ErrorCode = Literal[
    "needs_admin",  # the operation needs an elevated helper
    "forbidden",  # not allowed in this helper (scan_mft_image when elevated)
    "invalid_request",  # well-framed but not a valid request
    "not_supported",  # not available on this platform / volume
    "not_ntfs",  # not a local NTFS volume
    "io_error",  # reading the volume or file failed
    "unavailable",  # the MFT module is missing or broken
    "bad_frame",  # framing error; the helper closes the connection after this
    "internal",
]


class Error(Message):
    type: Literal["error"] = "error"
    id: int | None = Field(default=None, ge=1, le=MAX_ID)
    code: ErrorCode
    message: str = Field(max_length=4096)


Reply = Annotated[Hello | Progress | Begin | Response | Error, Field(discriminator="type")]
REPLY: TypeAdapter[Hello | Progress | Begin | Response | Error] = TypeAdapter(Reply)


# --------------------------------------------------------------------------- framing


def frame_header(kind: int, body_len: int) -> bytes:
    return HEADER.pack(body_len + 1, kind)


def json_frame(message: Message) -> list[bytes]:
    body = message.model_dump_json().encode("utf-8")
    return [frame_header(KIND_JSON, len(body)), body]


def check_header(head: bytes | bytearray, *, max_binary: int) -> tuple[int, int]:
    """Validate a frame header; returns (kind, body length)."""
    length, kind = HEADER.unpack(head)
    body_len = length - 1
    if kind == KIND_JSON:
        limit = MAX_CONTROL_BYTES
    elif kind == KIND_BINARY:
        limit = max_binary
    else:
        raise FrameError(f"未知的帧类型 0x{kind:02x}")
    if length < 1 or body_len > limit:
        raise FrameError(f"帧长度 {length} 超出范围")
    return kind, body_len
