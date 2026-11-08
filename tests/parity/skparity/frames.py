"""Binary frames (docs/protocol.md §4): decode without numpy or the protocol package."""

from __future__ import annotations

import json
import struct
from typing import Any

DTYPES = {
    "<u1": "B",
    "<i1": "b",
    "<u2": "H",
    "<i2": "h",
    "<u4": "I",
    "<i4": "i",
    "<u8": "Q",
    "<i8": "q",
    "<f4": "f",
    "<f8": "d",
}


def decode(buf: bytes) -> tuple[dict[str, Any], dict[str, tuple[str, list[Any]]], dict[str, bytes]]:
    """(header, {column: (dtype, values)}, {blob: bytes})."""
    (n,) = struct.unpack_from("<I", buf, 0)
    header = json.loads(buf[4 : 4 + n].decode("utf-8"))
    start = 4 + n
    start += (8 - start % 8) % 8
    count = int(header["count"])
    columns: dict[str, tuple[str, list[Any]]] = {}
    for name, spec in header.get("columns", {}).items():
        code = DTYPES[spec["dtype"]]
        values = list(struct.unpack_from(f"<{count}{code}", buf, start + int(spec["offset"])))
        columns[name] = (spec["dtype"], values)
    blobs: dict[str, bytes] = {}
    for name, spec in header.get("blobs", {}).items():
        off = start + int(spec["offset"])
        blobs[name] = bytes(buf[off : off + int(spec["length"])])
    return header, columns, blobs


def split_names(blob: bytes, lengths: list[int]) -> list[str]:
    """The `names` blob cut by the `name_len` column (UTF-8; undecodable bytes as U+FFFD)."""
    out: list[str] = []
    pos = 0
    for n in lengths:
        out.append(blob[pos : pos + n].decode("utf-8", "replace"))
        pos += n
    return out
