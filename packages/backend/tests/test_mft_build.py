"""The native tree build (``spaceknife_native.mft_build``) against the numpy build, column by column.

``table_to_store`` uses the native build whenever it is installed; ``_table_to_store_numpy`` is the
reference (and the fallback). Every store column (node order included), the ``\\0name`` image,
the extension table and the store version must be the same, on parsed images and on random
tables with every oddity a table can have: unsorted and repeated records, missing, foreign and
cyclic parents, metafiles, the root's own entry, streams, huge and negative values.
"""

from __future__ import annotations

from typing import Any

import numpy as np
import pytest
from hypothesis import HealthCheck, given, settings
from hypothesis import strategies as st

from spaceknife import native
from spaceknife.scan import mft
from spaceknife.scan.mft import COLUMN_DTYPES, MftTable, parse_mft_numpy
from spaceknife.store import mapped
from spaceknife.store.nodes import NodeStore

from . import mft_image as im

pytestmark = pytest.mark.skipif(not native.has("mft_build"), reason="native extension not installed")

RS, CS = 1024, 4096


def assert_same_store(a: NodeStore, b: NodeStore) -> None:
    ca, cb = a.export_columns(), b.export_columns()
    assert set(ca) == set(cb)
    for key in ca:
        assert ca[key].dtype == cb[key].dtype, key
        assert np.array_equal(ca[key], cb[key]), key
    assert a.names_blob == b.names_blob
    assert a.ext_table == b.ext_table
    assert a.version == b.version and len(a) == len(b)


def both(table: MftTable, root: str = "C:\\", **kw: Any) -> NodeStore:
    """The numpy and the native build of copies of ``table``: equal, or the test fails."""
    ref = mft._table_to_store_numpy(dict(table), root, **kw)
    got = mft.table_to_store(dict(table), root, **kw)
    assert_same_store(ref, got)
    return got


@pytest.fixture(scope="module")
def sample() -> MftTable:
    return parse_mft_numpy(im.build_mft(im.sample_records()), RS, CS)


@pytest.mark.parametrize(
    "kw",
    [{}, {"include_ads": True}, {"count_links_each": True}, {"include_ads": True, "count_links_each": True}],
)
def test_sample_image(sample: MftTable, kw: dict[str, Any]) -> None:
    both(sample, **kw)


def test_cycles_orphans_and_odd_parents() -> None:
    recs = [
        *im.metafile_records(),
        im.Rec(70, [im.Name(72, "c1", im.WIN32)], dir=True),
        im.Rec(71, [im.Name(70, "c2", im.WIN32)], dir=True),
        im.Rec(72, [im.Name(71, "c3", im.WIN32)], dir=True),
        im.Rec(73, [im.Name(71, "below.txt", im.WIN32)], streams=[im.Stream(5000)]),
        im.Rec(74, [im.Name(74, "self", im.WIN32)], dir=True),
        im.Rec(75, [im.Name(75, "selfkid", im.WIN32)], streams=[im.Stream(1)]),
        im.Rec(76, [im.Name(66, "under-a-file", im.WIN32)]),
        im.Rec(66, [im.Name(im.ROOT, "file", im.WIN32)], streams=[im.Stream(1)]),
        im.Rec(77, [im.Name(1 << 40, "far-parent", im.WIN32)]),
    ]
    both(parse_mft_numpy(im.build_mft(recs), RS, CS), "D:\\")


def test_minimal_tables() -> None:
    both(parse_mft_numpy(b"", RS, CS), "E:\\")
    both(parse_mft_numpy(im.build_mft(im.tree_records({"a": {"b": 1}})), RS, CS), "F:\\")


def test_synthetic_volume() -> None:
    arr = im.synthetic_mft(120_000, seed=21)
    table = parse_mft_numpy(memoryview(arr), RS, CS)
    for kw in ({}, {"include_ads": True}):
        both(table, **kw)


def test_extensions_in_any_mix() -> None:
    wide = "\U00010000" * 5
    spec = {
        f"f{i}.{ext}": 1
        for i, ext in enumerate(
            ["TXT", "zip", "Abcdefghij", "txt", "longerext", "", "7z", wide, "é" * 17, "É" * 3, "ĲX"] * 5
        )
    }
    spec.update({".hidden": 1, "trailing.": 1, "a.b.C": 1, "x.SixteenCharsExtX": 1, "y.SeventeenCharsExt": 1})
    both(parse_mft_numpy(im.build_mft(im.tree_records(spec)), RS, CS))


def test_a_cycle_longer_than_the_label_rounds() -> None:
    # 5000 folders in one parent cycle: the numpy label propagation gives up after 4096 rounds and
    # breaks every member; the native build must do the same
    n, first = 5000, 100
    rec = np.arange(first, first + n, dtype=np.uint64)
    other = {
        k: np.zeros(n, dtype=COLUMN_DTYPES[k]) for k in ("flags", "size", "alloc", "ads_size", "ads_alloc")
    }
    times = {k: np.zeros(n, dtype=COLUMN_DTYPES[k]) for k in ("mtime", "ctime", "atime", "nlink")}
    table = {
        "record": rec,
        "parent": np.roll(rec, 1),
        "name_off": np.arange(n, dtype=np.uint32),
        "name_len": np.ones(n, dtype=np.uint16),
        "kind": np.ones(n, dtype=np.uint8),
        **other,
        **times,
        "names": b"a" * n,
        "bad_records": 0,
    }
    s = both(table)
    orphans = s.children(next(i for i in s.children(1) if s.name(i) == mft.FOLDERS["orphan"]))
    assert len(orphans) == n  # every member broken out


def test_consume_empties_the_table(sample: MftTable) -> None:
    a, b = dict(sample), dict(sample)
    ref = mft._table_to_store_numpy(a, "C:\\", consume=True)
    got = mft.table_to_store(b, "C:\\", consume=True)
    assert_same_store(ref, got)
    assert a == {} and b == {}


def test_columns_of_other_types_are_converted(sample: MftTable) -> None:
    odd = {k: (np.asarray(v).astype(np.int64) if k in COLUMN_DTYPES else v) for k, v in sample.items()}
    odd["names"] = bytearray(sample["names"])
    assert_same_store(mft._table_to_store_numpy(dict(sample), "C:\\"), mft.table_to_store(odd, "C:\\"))


def test_mapped_store(sample: MftTable, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(mapped.POLICY, "force_bytes", 1)  # every store goes to file-backed memory
    s = both(sample)
    assert s.mapped


def test_bad_tables_are_refused(sample: MftTable) -> None:
    bad = dict(sample)
    bad["name_off"] = np.asarray(sample["name_off"]).copy()
    bad["name_off"][3] = len(sample["names"]) + 10
    with pytest.raises(ValueError, match="name offsets"):
        mft.table_to_store(bad, "C:\\")
    short = dict(sample)
    short["kind"] = np.asarray(sample["kind"])[:-1]
    with pytest.raises(ValueError, match="rows"):
        mft.table_to_store(short, "C:\\")


# --------------------------------------------------------------------------- random tables

_TEXT = st.text(
    alphabet=st.sampled_from([*"abcXYZ.é文\U0001f600-ĲİΣ", "\ud800", "\udfff"])
    | st.characters(codec="utf-8"),
    min_size=0,
    max_size=12,
)


def random_table(rng: np.random.Generator, n: int, names: list[str] | None = None) -> MftTable:
    """A table with every oddity: records unsorted, repeated (not always next to each other),
    metafile and root records, parents anywhere (unknown, files, self, cycles), any values."""
    top = max(8, n // 2 + 20)
    record = rng.integers(0, top, n).astype(np.uint64)
    if n and rng.random() < 0.5:
        record.sort()
    parent = np.where(rng.random(n) < 0.9, rng.integers(0, top, n), rng.integers(0, 1 << 47, n)).astype(
        np.uint64
    )
    if names is None:
        names = ["".join(rng.choice(list("ab.c文x"), int(rng.integers(0, 9)))) for _ in range(n)]
    enc = [s.encode("utf-8", "surrogatepass") for s in names]
    lens = np.array([len(e) for e in enc], dtype=np.int64)
    blob = b"".join(enc)
    off = np.cumsum(lens) - lens
    perm = rng.permutation(n)  # names in the blob in another order than the rows
    blob_perm = b"".join(enc[i] for i in perm)
    pos = np.empty(n, dtype=np.int64)
    pos[perm] = np.cumsum(lens[perm]) - lens[perm]
    del blob, off

    def ints(dtype: Any, lo: int, hi: int) -> np.ndarray:
        return rng.integers(lo, hi, n, dtype=np.int64).astype(dtype)

    kind = rng.choice(np.array([0, 1, 1, 2], dtype=np.uint8), n)
    if n and rng.random() < 0.5:  # a ring of folders (a parent cycle), sometimes with a tree hanging off
        _, first = np.unique(record, return_index=True)
        dirs = first[kind[first] == 1]
        ring = rng.permutation(dirs)[: int(rng.integers(2, 8))]
        if len(ring) >= 2:
            parent[ring] = record[np.roll(ring, -1)]
    big = 1 << 62
    return {
        "record": record,
        "parent": parent,
        "name_off": pos.astype(np.uint32),
        "name_len": lens.astype(np.uint16),
        "kind": kind,
        "flags": ints(np.uint32, 0, 1 << 19),
        "size": ints(np.int64, -big, big),
        "alloc": ints(np.int64, -big, big),
        "ads_size": np.where(rng.random(n) < 0.2, ints(np.int64, -big, big), 0).astype(np.int64),
        "ads_alloc": np.where(rng.random(n) < 0.2, ints(np.int64, -big, big), 0).astype(np.int64),
        "mtime": ints(np.int64, -(1 << 40), 1 << 40),
        "ctime": ints(np.int64, -5, 1 << 33),
        "atime": ints(np.int64, 0, 1 << 32),
        "nlink": ints(np.uint16, 0, 1 << 16),
        "names": blob_perm,
        "bad_records": 0,
    }


@settings(max_examples=60, deadline=None, suppress_health_check=[HealthCheck.too_slow])
@given(
    seed=st.integers(0, 2**32 - 1),
    n=st.integers(0, 400),
    include_ads=st.booleans(),
    count_links_each=st.booleans(),
)
def test_random_tables(seed: int, n: int, include_ads: bool, count_links_each: bool) -> None:
    table = random_table(np.random.default_rng(seed), n)
    both(table, include_ads=include_ads, count_links_each=count_links_each)


@settings(max_examples=25, deadline=None)
@given(names=st.lists(_TEXT, min_size=1, max_size=80), seed=st.integers(0, 2**32 - 1))
def test_random_names(names: list[str], seed: int) -> None:
    table = random_table(np.random.default_rng(seed), len(names), names)
    table["kind"] = np.zeros(len(names), dtype=np.uint8)  # files: every name gets an extension id
    table["parent"] = np.full(len(names), 5, dtype=np.uint64)
    both(table)


def test_large_random_table() -> None:
    both(random_table(np.random.default_rng(99), 60_000), include_ads=True)
