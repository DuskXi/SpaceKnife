from __future__ import annotations

import struct

import numpy as np
import pytest

from spaceknife_protocol.binary import ALIGN, decode_frame, encode_frame
from spaceknife_protocol.errors import ProtocolError


def test_roundtrip_mixed_dtypes_is_aligned_and_zero_copy() -> None:
    cols = {
        "id": np.arange(7, dtype=np.uint32),
        "x": np.linspace(0, 1, 7, dtype=np.float32),
        "size": np.arange(7, dtype=np.int64) * 10**12,
        "flags": np.array([1, 0, 1, 0, 1, 0, 1], dtype=np.uint8),
        "w": np.ones(7, dtype=np.float64),
    }
    frame = encode_frame(stream="drawlist", layout="rect.v1", columns=cols, seq=3, final=False, request_id=42)
    header, out = decode_frame(frame)
    assert (header.stream, header.layout, header.seq, header.final, header.request_id) == (
        "drawlist",
        "rect.v1",
        3,
        False,
        42,
    )
    assert header.count == 7
    for name, arr in cols.items():
        np.testing.assert_array_equal(out[name], arr)
        assert out[name].dtype == arr.dtype.newbyteorder("<")
        assert not out[name].flags.writeable
        assert header.columns[name].offset % ALIGN == 0
    (hlen,) = struct.unpack_from("<I", frame)
    assert (4 + hlen + (-(4 + hlen)) % ALIGN) % ALIGN == 0


def test_empty_frame() -> None:
    header, out = decode_frame(encode_frame(stream="s", layout="l", columns={"a": np.zeros(0, np.float32)}))
    assert header.count == 0
    assert out["a"].shape == (0,)


def test_big_endian_input_is_normalised() -> None:
    arr = np.arange(4, dtype=">u4")
    _, out = decode_frame(encode_frame(stream="s", layout="l", columns={"a": arr}))
    assert out["a"].dtype.str == "<u4"
    np.testing.assert_array_equal(out["a"], [0, 1, 2, 3])


@pytest.mark.parametrize(
    "columns",
    [
        {"a": np.zeros((2, 2), np.float32)},
        {"a": np.zeros(3, np.float32), "b": np.zeros(4, np.float32)},
        {"a": np.zeros(3, dtype=bool)},
        {"a": np.zeros(3, dtype=np.complex64)},
        {"a": np.array(["x"], dtype=object)},
    ],
)
def test_encode_rejects_bad_columns(columns: dict[str, np.ndarray]) -> None:
    with pytest.raises(ProtocolError):
        encode_frame(stream="s", layout="l", columns=columns)


def test_decode_rejects_truncated_and_garbage() -> None:
    frame = encode_frame(stream="s", layout="l", columns={"a": np.arange(100, dtype=np.float64)})
    with pytest.raises(ProtocolError):
        decode_frame(frame[:-8])
    with pytest.raises(ProtocolError):
        decode_frame(b"\x01\x00")
    with pytest.raises(ProtocolError):
        decode_frame(struct.pack("<I", 10) + b"not json!!")
    with pytest.raises(ProtocolError):
        decode_frame(struct.pack("<I", 1 << 30))


def test_blobs_travel_after_the_columns() -> None:
    names = "Users\0视频.mp4\0".encode()
    frame = encode_frame(
        stream="view",
        layout="drawlist.v1",
        columns={"name_len": np.array([5, 10], dtype=np.uint32)},
        blobs={"names": names},
    )
    header, out = decode_frame(frame)
    assert header.blobs["names"].length == len(names)
    assert header.blobs["names"].offset % ALIGN == 0
    assert out["names"].dtype == np.uint8 and not out["names"].flags.writeable
    assert out["names"].tobytes() == names
    np.testing.assert_array_equal(out["name_len"], [5, 10])


def test_blob_names_must_not_shadow_columns() -> None:
    with pytest.raises(ProtocolError):
        encode_frame(stream="s", layout="l", columns={"a": np.zeros(1, np.uint8)}, blobs={"a": b"x"})
