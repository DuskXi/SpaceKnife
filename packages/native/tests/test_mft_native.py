"""Rust $MFT parser and reader against the numpy reference (docs/native.md 2.1)."""

from __future__ import annotations

import importlib.util
import itertools
import os
import sys
import threading
from pathlib import Path
from types import ModuleType
from typing import Any

import numpy as np
import pytest

from spaceknife.scan.mft import COLUMN_DTYPES, parse_mft_numpy, read_mft_image

native: Any = pytest.importorskip("spaceknife_native")
if not hasattr(native, "parse_mft"):
    pytest.skip("spaceknife_native was built without the mft feature", allow_module_level=True)

RS, CS = 1024, 4096


def _image_module() -> ModuleType:
    path = Path(__file__).resolve().parents[2] / "backend" / "tests" / "mft_image.py"
    spec = importlib.util.spec_from_file_location("mft_image", path)
    assert spec is not None and spec.loader is not None
    mod = importlib.util.module_from_spec(spec)
    sys.modules.setdefault("mft_image", mod)
    spec.loader.exec_module(mod)
    return mod


im = _image_module()


def assert_same(data: Any, rs: int = RS) -> dict[str, Any]:
    a = native.parse_mft(data, rs, CS)
    b = parse_mft_numpy(data, rs, CS)
    assert set(a) == set(b)
    for key, dtype in COLUMN_DTYPES.items():
        assert a[key].dtype == dtype, key
        assert np.array_equal(a[key], b[key]), key
    assert a["names"] == b["names"]
    assert a["bad_records"] == b["bad_records"]
    return dict(a)


@pytest.mark.parametrize("rs", [1024, 4096])
def test_sample_image(rs: int) -> None:
    table = assert_same(im.build_mft(im.sample_records(), rs, CS), rs)
    assert table["bad_records"] == 5 and len(table["record"]) == 60


def test_synthetic_image() -> None:
    arr = im.synthetic_mft(60_000, seed=7)
    table = assert_same(memoryview(arr))
    assert len(table["record"]) > 60_000 and table["bad_records"] == 0


def test_fuzzed_images_match() -> None:
    """Random corruption anywhere in the records: both parsers must agree on every outcome."""
    rng = np.random.default_rng(1234)
    base = np.frombuffer(im.build_mft(im.sample_records()), dtype=np.uint8)
    n = len(base) // RS
    hot = np.array(
        [0x04, 0x06, 0x14, 0x16, 0x18, 0x20, 0x26, 0x10, 0x3A, 0x3C, 0x40, 0x50, 0x9C, 0xA0, 0xA2, 0xB0]
    )
    for _ in range(300):
        img = base.copy()
        for _ in range(int(rng.integers(1, 12))):
            rec = int(rng.integers(0, n))
            off = int(rng.choice(hot)) if rng.random() < 0.3 else int(rng.integers(0, RS))
            img[rec * RS + off] = rng.integers(0, 256)
        assert_same(img.tobytes())


def test_buffer_types() -> None:
    data = im.build_mft(im.sample_records())
    ref = native.parse_mft(data, RS, CS)
    for buf in (bytearray(data), memoryview(data), np.frombuffer(data, dtype=np.uint8)):
        out = native.parse_mft(buf, RS, CS)
        assert all(np.array_equal(out[k], ref[k]) for k in COLUMN_DTYPES) and out["names"] == ref["names"]
    with pytest.raises((ValueError, BufferError)):
        native.parse_mft(memoryview(data)[::2], RS, CS)
    with pytest.raises(ValueError, match="record_size"):
        native.parse_mft(data, 1000, CS)
    with pytest.raises(ValueError, match="cluster_size"):
        native.parse_mft(data, RS, 0)
    empty = native.parse_mft(b"", RS, CS)
    assert len(empty["record"]) == 0 and empty["names"] == b"" and empty["bad_records"] == 0


def test_releases_the_gil() -> None:
    arr = im.synthetic_mft(400_000, seed=3)
    result: list[Any] = []
    worker = threading.Thread(target=lambda: result.append(native.parse_mft(memoryview(arr), RS, CS)))
    spins = 0
    worker.start()
    while worker.is_alive():
        spins += 1
    worker.join()
    assert result and len(result[0]["record"]) > 400_000
    assert spins > 1000  # this thread kept running while Rust parsed


@pytest.mark.parametrize(
    ("runs", "split", "valid", "rs", "cs"),
    [
        ([(10, 20)], None, None, 1024, 4096),
        ([(40, 4), (12, 3), (90, 11)], None, None, 1024, 4096),
        ([(40, 6), (12, 3), (90, 9)], 1, None, 1024, 4096),
        ([(40, 6), (12, 3), (90, 9)], 2, 50, 1024, 4096),
        ([(64, 201), (1000, 43), (600, 300)], 1, None, 4096, 512),
    ],
)
def test_read_mft_image(
    tmp_path: Path, runs: list[tuple[int, int]], split: int | None, valid: int | None, rs: int, cs: int
) -> None:
    recs = [r for r in im.sample_records() if r.number <= 66]
    img = tmp_path / "vol.img"
    expected = im.build_volume(
        img, recs, runs, record_size=rs, cluster_size=cs, split_at=split, valid_records=valid
    )
    data = native._read_mft_image(str(img))
    assert isinstance(data, bytes) and data == expected
    info = native._mft_image_info(str(img))
    py_data, py_info = read_mft_image(img)
    assert info == py_info and bytes(py_data) == data
    assert info["record_size"] == rs and info["mft_size"] == len(expected)


def test_fuzzed_volume_images_match(tmp_path: Path) -> None:
    """Corrupt boot sector / record 0 / extension record 16: same bytes, or both readers fail."""
    rng = np.random.default_rng(99)
    recs = [r for r in im.sample_records() if r.number <= 66]
    base = tmp_path / "base.img"
    runs = [(40, 6), (12, 3), (90, 9)]
    im.build_volume(base, recs, runs, split_at=1)
    raw = base.read_bytes()
    mft_at = runs[0][0] * CS
    targets = [(0, 512), (mft_at, RS), (mft_at + 16 * RS, RS)]
    img = tmp_path / "fuzzed.img"
    for _ in range(150):
        data = bytearray(raw)
        for _ in range(int(rng.integers(1, 5))):
            start, span = targets[int(rng.integers(0, len(targets)))]
            data[start + int(rng.integers(0, span))] = int(rng.integers(0, 256))
        img.write_bytes(data)
        try:
            ours: bytes | None = native._read_mft_image(str(img))
        except OSError:
            ours = None
        try:
            theirs: bytes | None = bytes(read_mft_image(img)[0])
        except OSError:
            theirs = None
        assert ours == theirs


def test_read_mft_image_errors(tmp_path: Path) -> None:
    img = tmp_path / "zero.img"
    img.write_bytes(bytes(1 << 16))
    with pytest.raises(OSError, match="not an NTFS volume"):
        native._read_mft_image(str(img))
    with pytest.raises(FileNotFoundError):
        native._mft_image_info(str(tmp_path / "missing.img"))


def _elevated() -> bool:
    if sys.platform != "win32":
        return os.geteuid() == 0
    import ctypes

    return bool(ctypes.windll.shell32.IsUserAnAdmin())  # type: ignore[attr-defined,unused-ignore]


def test_volume_functions() -> None:
    if sys.platform != "win32":
        for fn in (native.mft_volume_info, native.read_mft):
            with pytest.raises(NotImplementedError):
                fn("C:")
        return
    for fn in (native.mft_volume_info, native.read_mft):
        with pytest.raises(ValueError, match="local volume"):
            fn("C:\\Windows")
    if _elevated():
        pytest.skip("elevated: opening the volume would succeed (never reads the $MFT in tests)")
    for fn in (native.mft_volume_info, native.read_mft):
        with pytest.raises(PermissionError):
            fn(os.environ.get("SYSTEMDRIVE", "C:"))


@pytest.mark.skipif(
    sys.platform != "win32" or os.environ.get("SPACEKNIFE_TEST_RAW_VOLUME") != "1",
    reason="reads the system volume's boot sector and $MFT record 0 (set SPACEKNIFE_TEST_RAW_VOLUME=1, elevated)",
)
def test_volume_info_of_system_volume() -> None:
    info = native.mft_volume_info(os.environ.get("SYSTEMDRIVE", "C:"))
    assert set(info) >= {"bytes_per_sector", "cluster_size", "record_size", "mft_lcn", "mft_size"}
    assert info["mft_size"] % info["record_size"] == 0 and info["mft_valid_size"] <= info["mft_size"]


# --------------------------------------------------------------------------- streaming (scan_mft)


def _by_record(table: dict[str, Any]) -> tuple[dict[str, np.ndarray], list[bytes]]:
    """Rows in record order (stable: a record's names keep their order) and their names."""
    order = np.argsort(table["record"], kind="stable")
    cols = {key: np.asarray(table[key])[order] for key in COLUMN_DTYPES}
    names = bytes(table["names"])
    off, ln = cols["name_off"].astype(np.int64), cols["name_len"].astype(np.int64)
    return cols, [names[o : o + n] for o, n in zip(off.tolist(), ln.tolist(), strict=True)]


def assert_same_rows(streamed: dict[str, Any], whole: dict[str, Any]) -> None:
    a_cols, a_names = _by_record(streamed)
    b_cols, b_names = _by_record(whole)
    for key, dtype in COLUMN_DTYPES.items():
        if key == "name_off":
            continue  # offsets into each table's own blob
        assert a_cols[key].dtype == dtype, key
        assert np.array_equal(a_cols[key], b_cols[key]), key
    assert a_names == b_names
    assert streamed["bad_records"] == whole["bad_records"]


@pytest.mark.parametrize("window", [1, 2, 7, 64, 100_000])
@pytest.mark.parametrize("rs", [1024, 4096])
def test_streaming_parse_matches(window: int, rs: int) -> None:
    """Extension records before and after their base, in other windows: same rows as parse_mft."""
    data = im.build_mft(im.sample_records(), rs, CS)
    assert_same_rows(native._parse_mft_stream(data, rs, CS, window), native.parse_mft(data, rs, CS))


def test_streaming_parse_matches_synthetic_and_fuzzed() -> None:
    arr = im.synthetic_mft(30_000, seed=11)
    assert_same_rows(
        native._parse_mft_stream(memoryview(arr), RS, CS, 4093), native.parse_mft(memoryview(arr), RS, CS)
    )
    rng = np.random.default_rng(77)
    base = np.frombuffer(im.build_mft(im.sample_records()), dtype=np.uint8)
    n = len(base) // RS
    for _ in range(100):
        img = base.copy()
        for _ in range(int(rng.integers(1, 10))):
            img[int(rng.integers(0, n)) * RS + int(rng.integers(0, RS))] = rng.integers(0, 256)
        data = img.tobytes()
        assert_same_rows(
            native._parse_mft_stream(data, RS, CS, int(rng.integers(1, 9))), native.parse_mft(data, RS, CS)
        )


@pytest.mark.parametrize(
    ("runs", "split", "valid", "rs", "cs", "window"),
    [
        ([(10, 20)], None, None, 1024, 4096, 0),
        ([(40, 4), (12, 3), (90, 11)], None, None, 1024, 4096, 3 * 1024),
        ([(40, 6), (12, 3), (90, 9)], 1, None, 1024, 4096, 5 * 1024),
        ([(40, 6), (12, 3), (90, 9)], 2, 50, 1024, 4096, 1024),
        ([(64, 201), (1000, 43), (600, 300)], 1, None, 4096, 512, 3 * 4096),
    ],
)
def test_scan_mft_image_streams_the_volume(
    tmp_path: Path,
    runs: list[tuple[int, int]],
    split: int | None,
    valid: int | None,
    rs: int,
    cs: int,
    window: int,
) -> None:
    """Windows across data runs, a split run list and a short valid length: same rows."""
    recs = [r for r in im.sample_records() if r.number <= 66]
    img = tmp_path / "vol.img"
    im.build_volume(img, recs, runs, record_size=rs, cluster_size=cs, split_at=split, valid_records=valid)
    table, info = native._scan_mft_image(str(img), 1.0, window)
    assert info == native._mft_image_info(str(img))
    assert_same_rows(table, native.parse_mft(native._read_mft_image(str(img)), rs, cs))
    paced, _ = native._scan_mft_image(str(img), 0.5, window)  # an I/O limit changes timing only
    assert_same_rows(paced, table)
    with pytest.raises(ValueError, match="duty"):
        native._scan_mft_image(str(img), 0.0)

    # progress: (done, total) from 0 to all of it, once per window; a failing callback is ignored
    seen: list[tuple[int, int]] = []
    native._scan_mft_image(str(img), 1.0, window, lambda done, total: seen.append((done, total)))
    total = seen[0][1]
    assert seen[0] == (0, total) and seen[-1] == (total, total) and total > 0
    assert all(b[0] > a[0] for a, b in itertools.pairwise(seen)) and {t for _, t in seen} == {total}
    if window:
        assert len(seen) - 1 == -(-total // window) or window % rs  # windows are whole records

    def boom(done: int, total: int) -> None:
        raise RuntimeError("progress callbacks never stop a scan")

    failing, _ = native._scan_mft_image(str(img), 1.0, window, boom)
    assert_same_rows(failing, table)
