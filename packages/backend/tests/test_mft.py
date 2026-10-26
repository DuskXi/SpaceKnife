"""MFT fast scan, pure-Python side: numpy parser rules, the $MFT reader, table_to_store."""

from __future__ import annotations

import os
import sys
from pathlib import Path
from typing import Any

import numpy as np
import pytest
from hypothesis import given, settings
from hypothesis import strategies as st

from spaceknife import native
from spaceknife.scan import mft
from spaceknife.scan.mft import (
    COLUMN_DTYPES,
    META_FOLDER,
    ORPHAN_FOLDER,
    MftTable,
    parse_mft_numpy,
    read_mft_image,
    table_to_store,
)
from spaceknife.store.nodes import Flag, Kind, NodeStore

from . import mft_image as im
from .store_builder import build as build_hard
from .store_builder import find

RS, CS = 1024, 4096


@pytest.fixture(scope="module")
def sample() -> MftTable:
    return parse_mft_numpy(im.build_mft(im.sample_records()), RS, CS)


def names_of(table: MftTable) -> list[str]:
    blob = table["names"]
    return [
        blob[o : o + n].decode("utf-8", "surrogatepass")
        for o, n in zip(table["name_off"].tolist(), table["name_len"].tolist(), strict=True)
    ]


def row(table: MftTable, name: str) -> dict[str, int]:
    i = names_of(table).index(name)
    return {k: int(table[k][i]) for k in COLUMN_DTYPES}


# --------------------------------------------------------------------------- parser


def test_columns_and_dtypes(sample: MftTable) -> None:
    assert set(sample) == {*COLUMN_DTYPES, "names", "bad_records"}
    for key, dtype in COLUMN_DTYPES.items():
        assert sample[key].dtype == dtype, key
        assert len(sample[key]) == len(sample["record"])
    assert isinstance(sample["names"], bytes)
    assert sample["bad_records"] == 5  # torn, bad usa, bad length, BAAD, bad first attribute
    assert np.all(np.diff(sample["record"].astype(np.int64)) >= 0)  # rows in record order


def test_names_and_namespaces(sample: MftTable) -> None:
    names = names_of(sample)
    assert "PROGRA~1.TXT" not in names  # DOS alias of a long name in the same directory
    assert "Y~1" in names  # a DOS name alone in its directory stays
    assert names.count("hl-a.bin") == names.count("hl-b.bin") == 1
    assert {"CaseSensitive", "casesensitive"} <= set(names)
    for gone in ("deleted.txt", "torn", "bad-usa", "bad-length", "baad", "bad-first", "ghost", "ghost2"):
        assert gone not in names
    assert "lone\ud800surrogate.txt" in names
    assert "\U0001f600.PNG" in names and "文档.docx" in names
    assert row(sample, "hl-b.bin")["nlink"] == 2 and row(sample, "x")["nlink"] == 2
    assert row(sample, "Program Files Long.txt")["nlink"] == 1
    assert row(sample, "hl-b.bin")["parent"] == im.DOCS
    assert row(sample, ".")["record"] == im.ROOT


def test_name_encoding(sample: MftTable) -> None:
    blob = sample["names"]
    assert "lone\ud800surrogate.txt".encode("utf-8", "surrogatepass") in blob
    assert "\U0001f600.PNG".encode() in blob


def test_sizes(sample: MftTable) -> None:
    assert row(sample, "readme.txt")["size"] == 100 and row(sample, "readme.txt")["alloc"] == 0
    big = row(sample, "big.iso")
    assert (big["size"], big["alloc"]) == (10_000_000, 10_002_432)
    comp = row(sample, "comp.log")
    assert (comp["size"], comp["alloc"]) == (1 << 20, 256 << 10)
    sparse = row(sample, "sparse.vhd")
    assert (sparse["size"], sparse["alloc"]) == (1 << 30, 64 << 10)
    ads = row(sample, "with-ads.txt")
    assert (ads["size"], ads["alloc"], ads["ads_size"], ads["ads_alloc"]) == (3000, 4096, 100_026, 102_400)
    assert ads["flags"] & Flag.ADS
    assert row(sample, "docs")["size"] == 0 and row(sample, "adsdir")["ads_size"] == 9000
    assert row(sample, "$UsnJrnl")["ads_alloc"] == 2 << 20


def test_kinds_and_flags(sample: MftTable) -> None:
    assert row(sample, "docs")["kind"] == Kind.DIR
    assert row(sample, "link.txt")["kind"] == Kind.LINK
    assert row(sample, "junction")["kind"] == Kind.LINK  # mount point: shown, never descended
    assert row(sample, "long-target.lnk")["kind"] == Kind.LINK  # tag not in the MFT
    dedup = row(sample, "dedup.vhdx")
    assert dedup["kind"] == Kind.FILE and dedup["flags"] & Flag.REPARSE  # not a name surrogate
    assert row(sample, "hidden-sys-ro")["flags"] == Flag.HIDDEN | Flag.SYSTEM | Flag.READONLY
    assert row(sample, "cloud.docx")["flags"] == Flag.CLOUD | Flag.SPARSE
    assert row(sample, "comp.log")["flags"] == Flag.ARCHIVE | Flag.COMPRESSED
    assert row(sample, "secret.bin")["flags"] == Flag.ENCRYPTED


def test_extension_records(sample: MftTable) -> None:
    a, b = row(sample, "fragmented.dat"), row(sample, "frag-link.dat")
    assert a["record"] == b["record"] == 78
    assert (a["size"], a["nlink"], a["mtime"]) == (7_000_000, 2, 1_700_000_000)
    assert row(sample, "ads-in-ext")["ads_size"] == 5000
    assert row(sample, "stale-ext.bin")["size"] == 0  # its extension record has another sequence


def test_times(sample: MftTable) -> None:
    r = row(sample, "readme.txt")
    assert (r["ctime"], r["mtime"], r["atime"]) == (1_600_000_000, 1_700_000_000, 1_700_000_500)
    r = row(sample, "no-si")
    assert (r["ctime"], r["mtime"], r["atime"], r["flags"]) == (0, 0, 0, 0)


def test_record_size_4096_same_rows(sample: MftTable) -> None:
    other = parse_mft_numpy(im.build_mft(im.sample_records(), 4096, CS), 4096, CS)
    for key in (*COLUMN_DTYPES, "names", "bad_records"):
        assert np.array_equal(other[key], sample[key]) if key in COLUMN_DTYPES else other[key] == sample[key]


def test_edge_inputs() -> None:
    empty = parse_mft_numpy(b"", RS, CS)
    assert len(empty["record"]) == 0 and empty["names"] == b"" and empty["bad_records"] == 0
    data = im.build_mft(im.metafile_records())
    tail = parse_mft_numpy(memoryview(data + b"\xff" * 100), RS, CS)  # a partial record is ignored
    assert np.array_equal(tail["record"], parse_mft_numpy(bytearray(data), RS, CS)["record"])
    with pytest.raises(ValueError, match="record_size"):
        parse_mft_numpy(data, 1000, CS)
    with pytest.raises(ValueError, match="cluster_size"):
        parse_mft_numpy(data, RS, 3000)


def test_dispatch_matches_fallback(sample: MftTable) -> None:
    table = mft.parse_mft_table(im.build_mft(im.sample_records()), RS, CS)
    for key in COLUMN_DTYPES:
        assert np.array_equal(table[key], sample[key]), key
    assert table["names"] == sample["names"] and table["bad_records"] == sample["bad_records"]


# --------------------------------------------------------------------------- $MFT reader


@pytest.mark.parametrize(
    ("runs", "split", "valid", "rs", "cs"),
    [
        ([(10, 20)], None, None, 1024, 4096),
        ([(40, 4), (12, 3), (90, 11)], None, None, 1024, 4096),  # fragmented, backwards jump
        ([(40, 6), (12, 3), (90, 9)], 1, None, 1024, 4096),  # continued in extension record 16
        ([(40, 6), (12, 3), (90, 9)], 2, 50, 1024, 4096),  # valid data length < size
        ([(64, 201), (1000, 43), (600, 300)], 1, None, 4096, 512),  # records span clusters and runs
    ],
)
def test_read_mft_image(
    tmp_path: Path, runs: list[tuple[int, int]], split: int | None, valid: int | None, rs: int, cs: int
) -> None:
    recs = [r for r in im.sample_records() if r.number < 64 or r.number in (64, 65, 66)]
    img = tmp_path / "vol.img"
    expected = im.build_volume(
        img, recs, runs, record_size=rs, cluster_size=cs, split_at=split, valid_records=valid
    )
    data, info = read_mft_image(img)
    assert bytes(data) == expected
    assert info["record_size"] == rs and info["cluster_size"] == cs and info["mft_lcn"] == runs[0][0]
    assert info["mft_size"] == len(expected) and info["mft_fragments"] == len(runs)
    table = parse_mft_numpy(data, rs, cs)
    assert ("big.iso" in names_of(table)) == (valid is None)
    assert table["bad_records"] == 0


def test_read_mft_image_rejects_garbage(tmp_path: Path) -> None:
    img = tmp_path / "zero.img"
    img.write_bytes(bytes(1 << 16))
    with pytest.raises(OSError, match="not an NTFS volume"):
        read_mft_image(img)


def test_decode_runs() -> None:
    assert mft.decode_runs(bytes([0x21, 0x10, 0x00, 0x01, 0x01, 0x05, 0x31, 0x02, 0xFF, 0xFF, 0xFF, 0])) == [
        (0, 0x100, 0x10),
        (0x10, None, 5),
        (0x15, 0xFF, 2),
    ]
    assert mft.decode_runs(bytes([0x21, 0x10])) is None
    assert mft.decode_runs(im.encode_runs([(5, 3), (None, 2), (1, 7)])) == [
        (0, 5, 3),
        (3, None, 2),
        (5, 1, 7),
    ]


def _elevated() -> bool:
    if sys.platform != "win32":
        return os.geteuid() == 0
    import ctypes

    return bool(ctypes.windll.shell32.IsUserAnAdmin())  # type: ignore[attr-defined,unused-ignore]


def test_read_raw_mft_errors(monkeypatch: pytest.MonkeyPatch) -> None:
    if sys.platform != "win32":
        with pytest.raises(NotImplementedError):
            mft.read_raw_mft("C:")
        return
    with pytest.raises(ValueError, match="local volume"):
        mft.read_raw_mft("\\\\server\\share")
    if _elevated():
        pytest.skip("elevated: would read the whole $MFT")  # never done by the test suite
    with pytest.raises(PermissionError):
        mft.read_raw_mft("C:")
    monkeypatch.setattr(native, "has", lambda name: False)  # the ctypes reader
    with pytest.raises(PermissionError):
        mft.read_raw_mft("C:")


@pytest.mark.skipif(
    sys.platform != "win32" or os.environ.get("SPACEKNIFE_TEST_RAW_VOLUME") != "1",
    reason="reads the system volume's boot sector and $MFT record 0 (set SPACEKNIFE_TEST_RAW_VOLUME=1, elevated)",
)
def test_ctypes_layout_of_system_volume() -> None:
    volume = os.environ.get("SYSTEMDRIVE", "C:")
    dev = mft._VolumeReader(mft._device_path(volume))
    try:
        _, info, runs = mft._layout(dev)
    finally:
        dev.close()
    assert info["record_size"] in (1024, 4096) and info["mft_size"] % info["record_size"] == 0
    assert runs and info["mft_fragments"] == len(runs)
    if native.has("mft_volume_info"):
        mod = native.module()
        assert mod is not None
        assert mod.mft_volume_info(volume) == info


# --------------------------------------------------------------------------- table_to_store


def check_invariants(store: NodeStore) -> None:
    n = len(store)
    c = store.export_columns()
    assert c["parent"][0] == -1 and c["depth"][0] == 0 and c["flags"][0] == Flag.VIRTUAL
    dirs = np.flatnonzero(c["kind"] == Kind.DIR)
    seen = np.zeros(n, dtype=np.int64)
    for d in dirs.tolist():
        lo, cnt = int(c["child_start"][d]), int(c["child_count"][d])
        kids = np.arange(lo, lo + cnt)
        assert np.all(c["parent"][kids] == d)
        assert np.all(kids > d)
        assert np.all(c["depth"][kids] == c["depth"][d] + 1)
        seen[kids] += 1
    assert seen[0] == 0 and np.all(seen[1:] == 1)  # every node is exactly one folder's child
    assert np.all(c["child_count"][c["kind"] != Kind.DIR] == 0)
    totals = {k: c[k].copy() for k in ("tot_size", "tot_alloc", "tot_files", "tot_dirs")}
    store.reaggregate()
    for k, v in totals.items():
        assert np.array_equal(store.cols[k][:n], v), k


def wname(store: NodeStore, node_id: int) -> str:
    """A node's name as Windows wrote it (MFT names keep lone surrogates on every platform)."""
    return store.name_bytes(node_id).decode("utf-8", "surrogatepass")


def node(store: NodeStore, path: str) -> int:
    cur = next(iter(store.roots()))
    for part in [p for p in path.split("/") if p]:
        cur = next(ch for ch in store.children(cur) if wname(store, ch) == part)
    return cur


def child_names(store: NodeStore, path: str) -> set[str]:
    return {wname(store, ch) for ch in store.children(node(store, path))}


@pytest.fixture(scope="module")
def sample_store(sample: MftTable) -> NodeStore:
    return table_to_store(sample, "C:\\")


def test_store_shape(sample_store: NodeStore) -> None:
    s = sample_store
    check_invariants(s)
    root = next(iter(s.roots()))
    assert s.name(root) == "C:\\" and s.cols["flags"][root] == 0 and s.cols["ino"][root] == im.ROOT
    top = child_names(s, "")
    assert {"docs", "comp.log", "x", "junction", META_FOLDER, ORPHAN_FOLDER} <= top
    assert "." not in top and "$MFT" not in top
    assert child_names(s, "docs") == {
        "readme.txt",
        "big.iso",
        "hl-b.bin",
        "Y~1",
        "fragmented.dat",
        "文档.docx",
        "sub",
    }
    assert child_names(s, "docs/sub") == {"deep.tar.gz"}
    meta = child_names(s, META_FOLDER)
    assert {"$MFT", "$LogFile", "$Secure", "$Extend"} <= meta and "." not in meta
    assert child_names(s, f"{META_FOLDER}/$Extend") == {"$Quota", "$ObjId", "$UsnJrnl", "$RmMetadata"}
    assert child_names(s, f"{META_FOLDER}/$Extend/$RmMetadata") == {"$Txf"}
    orphans = child_names(s, ORPHAN_FOLDER)
    assert "orphan.txt" in orphans and len(orphans & {"cycle-a", "cycle-b"}) == 1
    cyc = "cycle-a" if "cycle-a" in orphans else "cycle-b"
    other = "cycle-b" if cyc == "cycle-a" else "cycle-a"
    assert other in child_names(s, f"{ORPHAN_FOLDER}/{cyc}")
    for pseudo in (META_FOLDER, ORPHAN_FOLDER):
        assert s.cols["flags"][node(s, pseudo)] == Flag.VIRTUAL


def test_store_values(sample_store: NodeStore) -> None:
    s = sample_store
    v = s.view(node(s, "docs/big.iso"))
    assert (v.size, v.alloc, v.ext, v.mtime, v.ctime, v.atime) == (
        10_000_000,
        10_002_432,
        "iso",
        1_700_000_000,
        1_600_000_000,
        1_700_000_500,
    )
    assert s.cols["ino"][node(s, "docs/big.iso")] == 66
    assert not s.view(node(s, "hl-a.bin")).flags & Flag.HARDLINK_DUP
    assert s.view(node(s, "docs/hl-b.bin")).flags & Flag.HARDLINK_DUP
    assert s.view(node(s, "frag-link.dat")).flags & Flag.HARDLINK_DUP
    assert s.view(node(s, "junction")).kind == Kind.LINK and s.view(node(s, "junction")).alloc == 0
    assert s.view(node(s, "docs")).alloc == 0
    assert s.view(node(s, "with-ads.txt")).size == 3000  # streams count only with include_ads
    exts = {
        name: s.view(node(s, name)).ext
        for name in (
            "comp.log",
            "\U0001f600.PNG",
            "docs/sub/deep.tar.gz",
            ".hidden-dotfile",
            "trailing.",
            "x.ThisExtensionIsTooLong",
            "y.SixteenCharsExtX",
            "z.扩展名",
            "lone\ud800surrogate.txt",
            "docs/文档.docx",
        )
    }
    assert exts == {
        "comp.log": "log",
        "\U0001f600.PNG": "png",
        "docs/sub/deep.tar.gz": "gz",
        ".hidden-dotfile": "",
        "trailing.": "",
        "x.ThisExtensionIsTooLong": "",
        "y.SixteenCharsExtX": "sixteencharsextx",
        "z.扩展名": "扩展名",
        "lone\ud800surrogate.txt": "txt",
        "docs/文档.docx": "docx",
    }


def test_store_totals(sample: MftTable, sample_store: NodeStore) -> None:
    s = sample_store
    first = np.ones(len(sample["record"]), dtype=bool)
    first[1:] = sample["record"][1:] != sample["record"][:-1]
    files = first & (sample["kind"] == Kind.FILE)
    meta = (sample["record"] < 16) & (sample["record"] != 8)  # $BadClus:$Bad never counts
    meta |= np.isin(sample["record"], [24, 25, 26, 27, 28])
    alloc = int(sample["alloc"][files].sum()) + int(sample["ads_alloc"][meta].sum())
    root = next(iter(s.roots()))
    assert int(s.cols["tot_alloc"][root]) == alloc
    assert int(s.cols["tot_size"][root]) == int(sample["size"][files].sum())
    assert int(s.cols["tot_files"][root]) == int(np.count_nonzero(sample["kind"] == Kind.FILE))
    m = node(s, f"{META_FOLDER}/$Extend")
    assert int(s.cols["tot_alloc"][m]) == 2 << 20  # $UsnJrnl:$J counts under the metafiles


def test_badclus_does_not_count(sample: MftTable, sample_store: NodeStore) -> None:
    # $Bad claims the whole volume as allocated (no sparse flag) but maps only bad clusters
    assert row(sample, "$BadClus")["ads_alloc"] == 1 << 40
    for s in (sample_store, table_to_store(sample, "C:\\", include_ads=True)):
        v = s.view(node(s, f"{META_FOLDER}/$BadClus"))
        assert (v.size, v.alloc) == (0, 0)
        assert int(s.cols["tot_alloc"][next(iter(s.roots()))]) < 1 << 40


def test_include_ads(sample: MftTable) -> None:
    s = table_to_store(sample, "C:\\", include_ads=True)
    check_invariants(s)
    v = s.view(node(s, "with-ads.txt"))
    assert (v.size, v.alloc) == (103_026, 106_496)
    assert s.cols["alloc"][node(s, "adsdir")] == 12_288


def _expected_alloc(size: int) -> int:
    return 0 if size <= im.RESIDENT_MAX else -(-size // CS) * CS


def _hard_scan(spec: dict[str, Any]) -> NodeStore:
    """What the hard scan stores for the same tree (resident files occupy no clusters)."""
    store = build_hard(spec, root="C:\\", cluster=CS)
    c = store.cols
    n = len(store)
    files = c["kind"][:n] == Kind.FILE
    c["alloc"][:n] = np.where(files, [_expected_alloc(int(x)) for x in c["size"][:n]], 0)
    store.reaggregate()
    return store


def _dirs(spec: dict[str, Any], prefix: str = "") -> list[str]:
    out = [prefix]
    for name, value in spec.items():
        if isinstance(value, dict):
            out += _dirs(value, f"{prefix}/{name}")
    return out


def assert_same_as_hard_scan(spec: dict[str, Any]) -> None:
    table = parse_mft_numpy(im.build_mft(im.metafile_records() + im.tree_records(spec)), RS, CS)
    fast = table_to_store(table, "C:\\")
    check_invariants(fast)
    hard = _hard_scan(spec)
    assert ORPHAN_FOLDER not in child_names(fast, "")
    meta = node(fast, META_FOLDER)
    for path in _dirs(spec):
        a, b = node(fast, path), find(hard, path)
        got = [int(fast.cols[k][a]) for k in ("tot_size", "tot_alloc", "tot_files", "tot_dirs")]
        want = [int(hard.cols[k][b]) for k in ("tot_size", "tot_alloc", "tot_files", "tot_dirs")]
        if path == "":  # the volume root also holds the metafile folder
            got = [
                g - int(fast.cols[k][meta])
                for g, k in zip(got, ("tot_size", "tot_alloc", "tot_files", "tot_dirs"), strict=True)
            ]
            got[3] -= 1
        assert got == want, path
        assert child_names(fast, path) - {META_FOLDER} == {hard.name(ch) for ch in hard.children(b)}


def test_same_totals_as_hard_scan() -> None:
    assert_same_as_hard_scan(
        {"a": {"b": {"c.txt": 5000, "d": {}}, "e.bin": 100}, "f.iso": 1 << 30, "g": {"h": 1, "i": 4096}}
    )


_names = st.text(
    alphabet=st.characters(blacklist_characters="/\\\0", blacklist_categories=("Cs",)),
    min_size=1,
    max_size=12,
).filter(lambda n: n not in (META_FOLDER, ORPHAN_FOLDER))  # reserved: the check finds them by name
_tree: st.SearchStrategy[dict[str, Any]] = st.recursive(
    st.dictionaries(_names, st.integers(0, 1 << 24), max_size=4),
    lambda inner: st.dictionaries(_names, st.one_of(inner, st.integers(0, 1 << 24)), max_size=4),
    max_leaves=40,
)


@settings(max_examples=40, deadline=None)
@given(_tree)
def test_same_totals_as_hard_scan_random(spec: dict[str, Any]) -> None:
    assert_same_as_hard_scan(spec)


def test_cycles_and_odd_parents() -> None:
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
    s = table_to_store(parse_mft_numpy(im.build_mft(recs), RS, CS), "D:\\")
    check_invariants(s)
    orphans = child_names(s, ORPHAN_FOLDER)
    assert {"self", "selfkid", "under-a-file", "far-parent", "c1"} <= orphans
    assert child_names(s, f"{ORPHAN_FOLDER}/c1") == {"c2"}
    assert child_names(s, f"{ORPHAN_FOLDER}/c1/c2") == {"c3", "below.txt"}
    assert int(s.cols["tot_alloc"][node(s, ORPHAN_FOLDER)]) == 8192


def test_minimal_tables() -> None:
    empty = parse_mft_numpy(b"", RS, CS)
    s = table_to_store(empty, "E:\\")
    check_invariants(s)
    assert len(s) == 2 and s.name(1) == "E:\\" and s.cols["child_count"][1] == 0
    # no root record at all: entries naming record 5 as parent still land under the root
    s = table_to_store(parse_mft_numpy(im.build_mft(im.tree_records({"a": {"b": 1}})), RS, CS), "F:\\")
    check_invariants(s)
    assert child_names(s, "") == {"a"} and child_names(s, "a") == {"b"}


def test_store_round_trips_through_snapshot(sample_store: NodeStore, tmp_path: Path) -> None:
    from spaceknife.store import snapshot

    path = tmp_path / "s.sks"
    snapshot.save(sample_store, path, roots=["C:\\"])
    loaded, _ = snapshot.load(path)
    for key, arr in sample_store.export_columns().items():
        assert np.array_equal(loaded.export_columns()[key], arr), key
    assert loaded.names_blob == sample_store.names_blob and loaded.ext_table == sample_store.ext_table


def test_extension_at_end_of_blob() -> None:
    wide = "\U00010000" * 5  # 5 characters, 20 bytes: needs the character count
    spec = {"a.TXT": 1, f"b.{wide}": 1, "d.Abcdefghij": 1, "c." + "é" * 17: 1}  # gathers run past the end
    s = table_to_store(parse_mft_numpy(im.build_mft(im.tree_records(spec)), RS, CS), "C:\\")
    assert {wname(s, ch): s.view(ch).ext for ch in s.children(1)} == {
        "a.TXT": "txt",
        f"b.{wide}": wide,
        "c." + "é" * 17: "",
        "d.Abcdefghij": "abcdefghij",
    }


def test_extension_ids_do_not_depend_on_the_chunk() -> None:
    # names in chunks keep the temporaries bounded (docs/memory.md §6): the ids and their order
    # are the same for any chunk size, short and long extensions mixed
    spec = {
        f"f{i}.{ext}": 1
        for i, ext in enumerate(["TXT", "zip", "Abcdefghij", "txt", "longerext", "", "7z"] * 9)
    }
    table = parse_mft_numpy(im.build_mft(im.tree_records(spec)), RS, CS)
    files = np.flatnonzero(table["kind"] == Kind.FILE)
    runs = []
    for chunk in (1, 7, mft.EXT_CHUNK):
        ext_table = [""]
        ids = mft._ext_ids(
            table["names"], table["name_off"], table["name_len"], files, ext_table, chunk=chunk
        )
        runs.append((ids.tolist(), ext_table))
    assert runs[0] == runs[1] == runs[2]
    assert sorted(runs[0][1]) == ["", "7z", "abcdefghij", "longerext", "txt", "zip"]
