from __future__ import annotations

import random

import numpy as np
import pytest

from spaceknife.store.nodes import ENTRY_COLUMNS, Flag, Kind, NodeStore, decode_name, encode_name


def entries(kinds: list[Kind], sizes: list[int], flags: list[int] | None = None) -> dict[str, np.ndarray]:
    n = len(kinds)
    cols = {k: np.zeros(n, dtype=np.int64) for k in ENTRY_COLUMNS}
    cols["kind"] = np.array(kinds, dtype=np.uint8)
    cols["size"] = np.array(sizes, dtype=np.int64)
    cols["alloc"] = np.array([-(-s // 4096) * 4096 for s in sizes], dtype=np.int64)
    cols["flags"] = np.array(flags or [0] * n, dtype=np.uint32)
    cols["nlink"] = np.ones(n, dtype=np.uint16)
    return cols


def test_ingest_builds_contiguous_children_and_totals() -> None:
    s = NodeStore()
    root = s.add_root("/data")
    first = s.ingest_names(
        root, [b"docs", b"a.txt", b"b.bin"], entries([Kind.DIR, Kind.FILE, Kind.FILE], [0, 10, 5000])
    )
    docs = first
    assert list(s.children(root)) == [first, first + 1, first + 2]
    s.ingest_names(docs, [b"x.pdf"], entries([Kind.FILE], [9000]))
    assert s.effective_size(root) == (14010, 4096 + 8192 + 12288)
    assert s.effective_size(docs) == (9000, 12288)
    v = s.view(root)
    assert (v.tot_files, v.tot_dirs) == (3, 1)
    assert s.path(docs + 3).replace("\\", "/").endswith("/data/docs/x.pdf")
    assert not s.view(docs).flags & Flag.PENDING
    assert s.view(root).flags == Flag.NONE


def test_hardlink_duplicates_do_not_count() -> None:
    s = NodeStore()
    root = s.add_root("/r")
    s.ingest_names(
        root,
        [b"a", b"a-link"],
        entries([Kind.FILE, Kind.FILE], [8192, 8192], [0, int(Flag.HARDLINK_DUP)]),
    )
    assert s.effective_size(root) == (8192, 8192)
    assert s.effective_size(root + 2) == (0, 0)


def test_double_listing_and_non_dir_are_rejected() -> None:
    s = NodeStore()
    root = s.add_root("/r")
    s.ingest_names(root, [b"f"], entries([Kind.FILE], [1]))
    with pytest.raises(ValueError, match="already listed"):
        s.ingest_names(root, [b"g"], entries([Kind.FILE], [1]))
    with pytest.raises(ValueError, match="not a directory"):
        s.ingest_names(root + 1, [b"g"], entries([Kind.FILE], [1]))


def random_store(seed: int, n_dirs: int = 300) -> NodeStore:
    rng = random.Random(seed)
    s = NodeStore()
    pending = [s.add_root("/root")]
    listed = 0
    while pending and listed < n_dirs:
        d = pending.pop(rng.randrange(len(pending)))
        k = rng.randrange(0, 8)
        kinds = [rng.choice([Kind.FILE, Kind.FILE, Kind.DIR, Kind.LINK]) for _ in range(k)]
        sizes = [rng.randrange(0, 10**7) if kind == Kind.FILE else 0 for kind in kinds]
        flags = [int(Flag.HARDLINK_DUP) if kind == Kind.FILE and rng.random() < 0.05 else 0 for kind in kinds]
        start = s.ingest_names(d, [f"n{i}".encode() for i in range(k)], entries(kinds, sizes, flags))
        pending += [start + i for i, kind in enumerate(kinds) if kind == Kind.DIR]
        listed += 1
    return s


@pytest.mark.parametrize("seed", [1, 2, 3])
def test_reaggregate_matches_incremental(seed: int) -> None:
    s = random_store(seed)
    before = {k: s.col(k).copy() for k in ("tot_size", "tot_alloc", "tot_files", "tot_dirs")}
    s.reaggregate()
    for key, arr in before.items():
        np.testing.assert_array_equal(s.col(key), arr, err_msg=key)


def test_sorted_children_and_find() -> None:
    s = NodeStore()
    root = s.add_root("/r")
    s.ingest_names(root, [b"b", b"A", b"c"], entries([Kind.FILE] * 3, [10, 9000, 100_000]))
    by_size = [s.name(int(i)) for i in s.sorted_children(root)]
    assert by_size == ["c", "A", "b"]
    by_name = [s.name(int(i)) for i in s.sorted_children(root, by="name", descending=False)]
    assert by_name == ["A", "b", "c"]
    assert s.find(s.path(root + 2)) == root + 2
    assert s.find(s.path(root) + "/missing") is None


def test_from_columns_roundtrip() -> None:
    s = random_store(7, 50)
    clone = NodeStore.from_columns(s.export_columns(), s.names_blob, s.ext_table, separated=True)
    assert len(clone) == len(s)
    for key, arr in s.export_columns().items():
        np.testing.assert_array_equal(clone.col(key), arr)
    assert clone.path(len(s) - 1) == s.path(len(s) - 1)


def test_name_codec_roundtrips_odd_names() -> None:
    for name in ["普通", "emoji-🙂", "tab\tname", "trailing space "]:
        assert decode_name(encode_name(name)) == name
