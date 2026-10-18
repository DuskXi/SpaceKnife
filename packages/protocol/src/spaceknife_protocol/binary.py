"""Binary-frame codec for bulk data (draw lists, child lists, scan batches).

Wire layout of one binary WebSocket frame::

    [u32 little-endian: header length H]
    [H bytes: BinaryFrameHeader as UTF-8 JSON]
    [zero padding so the data section starts on an 8-byte boundary]
    [data section: each column little-endian, starting on an 8-byte boundary,
     then each blob (raw bytes, e.g. UTF-8 names), also 8-byte aligned]

Column and blob offsets in the header are relative to the start of the data section. Columns all
have ``count`` rows; a blob is an opaque byte string whose meaning the ``layout`` defines (the
draw list stores row names in one blob and their byte lengths in a column). Decoding is
zero-copy: the returned numpy arrays (``uint8`` arrays for blobs) are read-only views over the
received buffer, and a JavaScript client can build TypedArray views over the same bytes.
"""

from __future__ import annotations

import struct
from collections.abc import Mapping

import numpy as np
from pydantic import Field

from .base import Payload
from .envelope import RequestId
from .errors import ProtocolError

ALIGN = 8
_LEN = struct.Struct("<I")
MAX_HEADER_BYTES = 64 * 1024

# Only fixed-width little-endian numeric types: every frontend can map these to typed arrays.
ALLOWED_DTYPES: frozenset[str] = frozenset(
    {"<u1", "<i1", "<u2", "<i2", "<u4", "<i4", "<u8", "<i8", "<f4", "<f8"}
)


class ColumnSpec(Payload):
    offset: int = Field(ge=0)
    dtype: str


class BlobSpec(Payload):
    offset: int = Field(ge=0)
    length: int = Field(ge=0)


class BinaryFrameHeader(Payload):
    stream: str = Field(min_length=1, max_length=64, description="Logical stream, e.g. 'drawlist'")
    request_id: RequestId | None = None
    seq: int = Field(ge=0)
    final: bool = True
    layout: str = Field(min_length=1, max_length=64, description="Versioned column layout, e.g. 'rect.v1'")
    count: int = Field(ge=0, description="Rows; every column has exactly this many elements")
    columns: dict[str, ColumnSpec]
    blobs: dict[str, BlobSpec] = Field(default_factory=dict)


def _pad(n: int) -> int:
    return (-n) % ALIGN


def encode_frame(
    *,
    stream: str,
    layout: str,
    columns: Mapping[str, np.ndarray],
    seq: int = 0,
    final: bool = True,
    request_id: RequestId | None = None,
    blobs: Mapping[str, bytes] | None = None,
) -> bytes:
    """Pack equally long 1-D numeric columns, plus optional raw blobs, into one binary frame."""
    count: int | None = None
    specs: dict[str, ColumnSpec] = {}
    chunks: list[bytes] = []
    offset = 0
    for name, array in columns.items():
        arr = np.asarray(array)
        if arr.ndim != 1:
            raise ProtocolError(f"column {name!r} must be 1-D, got shape {arr.shape}")
        dtype = arr.dtype.newbyteorder("<") if arr.dtype.byteorder == ">" else arr.dtype
        code = np.dtype(dtype).str.replace("|", "<")
        if code not in ALLOWED_DTYPES:
            raise ProtocolError(f"column {name!r} has unsupported dtype {arr.dtype}")
        if count is None:
            count = arr.shape[0]
        elif arr.shape[0] != count:
            raise ProtocolError(f"column {name!r} has {arr.shape[0]} rows, expected {count}")
        data = np.ascontiguousarray(arr, dtype=np.dtype(code)).tobytes()
        specs[name] = ColumnSpec(offset=offset, dtype=code)
        chunks.append(data)
        pad = _pad(len(data))
        if pad:
            chunks.append(b"\0" * pad)
        offset += len(data) + pad
    blob_specs: dict[str, BlobSpec] = {}
    for name, blob in (blobs or {}).items():
        if name in specs:
            raise ProtocolError(f"blob {name!r} has the same name as a column")
        data = bytes(blob)
        blob_specs[name] = BlobSpec(offset=offset, length=len(data))
        chunks.append(data)
        pad = _pad(len(data))
        if pad:
            chunks.append(b"\0" * pad)
        offset += len(data) + pad
    header = BinaryFrameHeader(
        stream=stream,
        request_id=request_id,
        seq=seq,
        final=final,
        layout=layout,
        count=count or 0,
        columns=specs,
        blobs=blob_specs,
    )
    header_bytes = header.model_dump_json(exclude_none=True).encode("utf-8")
    if len(header_bytes) > MAX_HEADER_BYTES:
        raise ProtocolError("binary frame header too large")
    prefix = _LEN.pack(len(header_bytes)) + header_bytes
    return prefix + b"\0" * _pad(len(prefix)) + b"".join(chunks)


def decode_frame(buf: bytes | bytearray | memoryview) -> tuple[BinaryFrameHeader, dict[str, np.ndarray]]:
    """Unpack a binary frame. Returned arrays are read-only zero-copy views over ``buf``."""
    view = memoryview(buf).cast("B")
    if len(view) < _LEN.size:
        raise ProtocolError("binary frame shorter than its length prefix")
    (header_len,) = _LEN.unpack_from(view, 0)
    if header_len > MAX_HEADER_BYTES or _LEN.size + header_len > len(view):
        raise ProtocolError("binary frame header length out of range")
    try:
        header = BinaryFrameHeader.model_validate_json(bytes(view[_LEN.size : _LEN.size + header_len]))
    except ValueError as exc:
        raise ProtocolError(f"invalid binary frame header: {exc}") from exc
    data_start = _LEN.size + header_len
    data_start += _pad(data_start)
    data = view[data_start:]
    columns: dict[str, np.ndarray] = {}
    for name, spec in header.columns.items():
        if spec.dtype not in ALLOWED_DTYPES:
            raise ProtocolError(f"column {name!r} has unsupported dtype {spec.dtype}")
        if spec.offset % ALIGN:
            raise ProtocolError(f"column {name!r} is not {ALIGN}-byte aligned")
        dtype = np.dtype(spec.dtype)
        end = spec.offset + dtype.itemsize * header.count
        if end > len(data):
            raise ProtocolError(f"column {name!r} runs past the end of the frame")
        arr = np.frombuffer(data, dtype=dtype, count=header.count, offset=spec.offset)
        arr.flags.writeable = False
        columns[name] = arr
    for name, blob in header.blobs.items():
        if name in columns:
            raise ProtocolError(f"blob {name!r} has the same name as a column")
        if blob.offset % ALIGN or blob.offset + blob.length > len(data):
            raise ProtocolError(f"blob {name!r} is misaligned or runs past the end of the frame")
        arr = np.frombuffer(data, dtype=np.uint8, count=blob.length, offset=blob.offset)
        arr.flags.writeable = False
        columns[name] = arr
    return header, columns
