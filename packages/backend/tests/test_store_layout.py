"""The node store's memory layout: name heap, zero-copy name index, narrow offsets, mapped mode."""

from __future__ import annotations

import gc
import json
import random
import sys
from pathlib import Path

import numpy as np
import pytest
import zstandard

from spaceknife.filter import names as names_mod
from spaceknife.filter.names import name_index
from spaceknife.store import heap as heap_mod
from spaceknife.store import mapped, nodes, snapshot
from spaceknife.store.heap import NameHeap
from spaceknife.store.nodes import COLUMNS, ENTRY_COLUMNS, Kind, NodeStore


@pytest.fixture(autouse=True)
def isolated_home(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("SPACEKNIFE_HOME", str(tmp_path / "home"))
    monkeypatch.setattr(mapped.POLICY, "force_bytes", None)


NAMES = [
    "readme.TXT",
    "Photo.jpg",
    "数据.csv",
    "a",
    "cache",
    "Cache.db",
    "node_modules",
    "x" * 300,
    "Ünïcode.md",
]


def _build(files: int, *, seed: int = 1) -> NodeStore:
    rnd = random.Random(seed)
    store = NodeStore(capacity=16)
    root = store.add_root("/r")
    dirs = [root]
    made = 0
    while made < files:
        d = dirs.pop(0)
        m = min(40, files - made)
        names = [f"{rnd.choice(NAMES)}{made + i}".encode() for i in range(m)]
        entries = {k: np.zeros(m, dtype=COLUMNS[k]) for k in ENTRY_COLUMNS}
        entries["kind"][: m // 4] = Kind.DIR
        entries["kind"][m // 4 :] = Kind.FILE
        entries["alloc"][:] = 4096
        start = store.ingest_names(d, names, entries)
        dirs.extend(range(start, start + m // 4))
        made += m
        if not dirs:
            break
    return store


def _all_names(store: NodeStore) -> list[bytes]:
    return [store.name_bytes(i) for i in range(len(store))]


# ------------------------------------------------------------------ the heap


def test_heap_runs_views_and_image(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(heap_mod, "CHUNK", 64)
    monkeypatch.setattr(heap_mod, "FIRST_CHUNK", 64)
    heap = NameHeap()
    words = [b"alpha", b"be", b"", b"gamma-delta", b"z" * 100, b"q"]
    offs = list(heap.append(b"".join(words[:3]), np.array([len(w) for w in words[:3]])))
    offs += list(heap.append(b"".join(words[3:]), np.array([len(w) for w in words[3:]])))
    offs.append(heap.append_one(b"last"))
    words.append(b"last")
    assert [heap.get(int(o), len(w)) for o, w in zip(offs, words, strict=True)] == words
    assert len(heap._chunks) >= 3  # small chunks: names spread over several, none split
    image = heap.image()
    rebased = heap.rebase(np.asarray(offs, dtype=np.int64))
    assert [image[int(o) : int(o) + len(w)] for o, w in zip(rebased, words, strict=True)] == words
    assert all(image[int(o) - 1] == 0 for o in rebased)
    run = heap.view(int(offs[0]) - 1, int(offs[2]) + 0)
    assert bytes(run) == b"\x00alpha\x00be\x00" and not run.flags.writeable
    with pytest.raises(ValueError, match="span"):
        heap.view(int(offs[0]) - 1, int(offs[4]) + 100)


def test_frozen_heap_is_used_in_place_and_grows_elsewhere() -> None:
    image = b"\x00one\x00two"
    heap = NameHeap.frozen(image)
    assert np.shares_memory(heap._chunks[0], np.frombuffer(image, dtype=np.uint8))
    off = heap.append_one(b"three")
    assert heap.get(off, 5) == b"three" and len(heap._chunks) == 2
    assert heap.image() == image + b"\x00three"


# ------------------------------------------------------------------ store + index over many heap chunks


def test_index_over_many_chunks_matches_one_chunk(monkeypatch: pytest.MonkeyPatch) -> None:
    one = _build(3000)
    monkeypatch.setattr(heap_mod, "CHUNK", 512)  # names spread over dozens of chunks
    monkeypatch.setattr(heap_mod, "FIRST_CHUNK", 512)
    many = _build(3000)
    assert _all_names(one) == _all_names(many)
    assert len(many._heap._chunks) > 20
    for store in (one, many):
        name_index(store).update(len(store))
    a, b = name_index(one), name_index(many)
    assert len(b.segments) > len(a.segments)
    n = len(one)
    for pat in ("*.jpg*", "cache*", "*CACHE*", "数据*", "*1?", "ünïcode*"):
        assert np.array_equal(a.match(pat, n), b.match(pat, n)), pat
    for q in ("cache", "nod", "x", "txt1"):
        for ta, tb in zip(a.ascii_tiers(q, n), b.ascii_tiers(q, n), strict=True):
            assert np.array_equal(ta, tb), q


def test_name_offsets_are_narrow_then_widen(monkeypatch: pytest.MonkeyPatch) -> None:
    store = _build(200)
    assert store.cols["name_off"].dtype == np.dtype("<u4")
    monkeypatch.setattr(nodes, "OFF_NARROW_LIMIT", store._heap.end + 50)
    before = _all_names(store)
    more = _build(400, seed=2)
    root = store.add_root  # roots must come first: append into an existing folder instead
    del root
    entries = {k: np.zeros(3, dtype=COLUMNS[k]) for k in ENTRY_COLUMNS}
    folder = next(
        i for i in range(len(store)) if store.cols["kind"][i] == Kind.DIR and not store.cols["child_count"][i]
    )
    store.ingest_names(folder, [b"x" * 40, b"y" * 40, b"z"], entries)
    assert store.cols["name_off"].dtype == np.dtype("<i8")
    assert _all_names(store)[: len(before)] == before
    assert store.name(len(store) - 1) == "z"
    del more


def test_from_columns_lays_out_names_given_in_any_order() -> None:
    store = _build(500)
    cols = store.export_columns()
    names = _all_names(store)
    order = list(range(len(names)))
    random.Random(3).shuffle(order)
    blob = bytearray()
    off = np.zeros(len(names), dtype=np.int64)
    for i in order:  # back to back, but not in id order
        off[i] = len(blob)
        blob += names[i]
    cols = {**cols, "name_off": off}
    clone = NodeStore.from_columns(cols, bytes(blob), store.ext_table)
    assert _all_names(clone) == names
    assert clone.cols["name_off"].dtype == np.dtype("<u4")
    # and the separated image (snapshot format 2) is taken without copying
    image = np.frombuffer(store.names_blob, dtype=np.uint8)
    again = NodeStore.from_columns(store.export_columns(), image, store.ext_table, separated=True)
    assert np.shares_memory(again._heap._chunks[0], image)
    assert _all_names(again) == names


# ------------------------------------------------------------------ snapshots


def _write_format1(store: NodeStore, path: Path) -> None:
    """A snapshot as format 1 wrote it: names back to back, offsets into that blob."""
    names = _all_names(store)
    lens = np.array([len(n) for n in names], dtype=np.int64)
    cols = {k: np.asarray(v) for k, v in store.export_columns().items()}
    cols["name_off"] = (np.cumsum(lens) - lens).astype(np.int64)
    zc = zstandard.ZstdCompressor()
    blocks, info, offset = [], {}, 0
    for key, arr in cols.items():
        packed = zc.compress(np.ascontiguousarray(arr).tobytes())
        info[key] = {
            "offset": offset,
            "length": len(packed),
            "raw_length": arr.nbytes,
            "dtype": arr.dtype.str,
        }
        blocks.append(packed)
        offset += len(packed)
    raw = b"".join(names)
    packed = zc.compress(raw)
    header = {
        "format": 1,
        "created_at": "2026-10-01T00:00:00Z",
        "roots": ["/r"],
        "node_count": len(store),
        "ext_table": store.ext_table,
        "columns": info,
        "names": {"offset": offset, "length": len(packed), "raw_length": len(raw), "dtype": ""},
    }
    head = json.dumps(header).encode()
    path.write_bytes(snapshot.MAGIC + snapshot._LEN.pack(len(head)) + head + b"".join(blocks) + packed)


def test_snapshots_read_format_1_and_write_format_2(tmp_path: Path) -> None:
    store = _build(800)
    old = tmp_path / "old.sks"
    _write_format1(store, old)
    loaded, header = snapshot.load(old)
    assert header.format == 1
    assert _all_names(loaded) == _all_names(store)
    new = tmp_path / "new.sks"
    snapshot.save(loaded, new, roots=["/r"])
    again, header2 = snapshot.load(new)
    assert header2.format == snapshot.FORMAT_VERSION == 2
    assert _all_names(again) == _all_names(store)
    for key in ("parent", "kind", "tot_alloc", "child_start", "child_count"):
        assert np.array_equal(again.export_columns()[key], store.export_columns()[key]), key


# ------------------------------------------------------------------ the lower-cased cache


def test_lower_cased_runs_are_a_bounded_shared_cache(monkeypatch: pytest.MonkeyPatch) -> None:
    store = _build(2000)
    n = len(store)
    full = {q: name_index(store).ascii_tiers(q, n) for q in ("cache", "photo")}
    fresh = _build(2000)
    monkeypatch.setattr(names_mod, "LOWER_CACHE_BYTES", names_mod._LOWER_BUDGET.used)  # nothing more
    index = name_index(fresh)
    for q, want in full.items():
        for got, exp in zip(index.ascii_tiers(q, n), want, strict=True):
            assert np.array_equal(got, exp), q
    assert not index._lower  # computed per query, not kept
    used = names_mod._LOWER_BUDGET.used
    held = sum(a.nbytes for a in name_index(store)._lower.values())
    assert held > 0
    del store, full
    gc.collect()
    assert names_mod._LOWER_BUDGET.used <= used - held  # a dead index gives its share back


# ------------------------------------------------------------------ memory-mapped mode


def test_mapped_mode_keeps_columns_out_of_private_memory(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    monkeypatch.setattr(mapped.POLICY, "force_bytes", 0)  # every store maps from its first growth
    store = _build(5000)
    assert store.mapped
    assert all(mapped.is_mapped(a) for a in store.cols.values())
    assert store.ram_bytes() < 1 << 20  # only the first, tiny name chunk stayed in plain memory
    names = _all_names(store)
    plain_tot = int(store.cols["tot_alloc"][1])
    assert store.compact() == 0  # mapped columns keep their tail: never touched, never written
    assert all(mapped.is_mapped(a) for a in store.cols.values())
    assert _all_names(store) == names and int(store.cols["tot_alloc"][1]) == plain_tot
    index = name_index(store)
    assert len(index.match("*cache*", len(store))) > 0
    path = tmp_path / "m.sks"
    snapshot.save(store, path, roots=["/r"])
    loaded, _ = snapshot.load(path)
    assert loaded.mapped and _all_names(loaded) == names
    assert mapped.is_mapped(loaded._heap._chunks[0])  # loaded names went to a mapped file too
    files_dir = mapped.columns_dir()
    del store, loaded, index
    gc.collect()
    if sys.platform == "win32":  # delete-on-close: the files go with their mappings
        assert not list(files_dir.iterdir())
    else:  # unlinked at creation
        assert not list(files_dir.iterdir())


def _bytes_written() -> int | None:
    """Bytes this process has written through system calls so far (None: not known here)."""
    if sys.platform == "win32":
        import ctypes
        from ctypes import wintypes

        class Counters(ctypes.Structure):
            _fields_ = [(n, ctypes.c_ulonglong) for n in ("ro", "wo", "oo", "rb", "wb", "ob")]

        k = ctypes.WinDLL("kernel32")
        k.GetCurrentProcess.restype = wintypes.HANDLE
        k.GetProcessIoCounters.argtypes = [wintypes.HANDLE, ctypes.POINTER(Counters)]
        c = Counters()
        k.GetProcessIoCounters(k.GetCurrentProcess(), ctypes.byref(c))
        return int(c.wb)
    try:
        for line in Path("/proc/self/io").read_text().splitlines():
            if line.startswith("wchar:"):
                return int(line.split()[1])
    except OSError:
        pass
    return None


def test_mapped_files_are_not_filled_with_zeros() -> None:
    """Making a mapped file longer writes nothing: on Windows ``os.ftruncate`` wrote every new
    byte as a zero, so each mapped column went to disk once more for nothing."""
    before = _bytes_written()
    if before is None:
        pytest.skip("no write counter on this platform")
    arr = mapped.mapped_zeros(32 << 20, np.uint8, growable=True)
    big = mapped.grow(arr, 96 << 20)
    assert big is not None and not big[-4096:].any()
    after = _bytes_written()
    assert after is not None and after - before < 1 << 20


def test_mapped_columns_grow_in_place(monkeypatch: pytest.MonkeyPatch) -> None:
    arr = mapped.mapped_zeros(1000, np.int64, growable=True)
    arr[:] = np.arange(1000)
    big = mapped.grow(arr, 50_000)
    assert big is not None and len(big) == 50_000
    assert big[:1000].tolist() == list(range(1000)) and not big[1000:].any()
    big[3] = -7
    assert arr[3] == -7  # the same pages: nothing was copied (or written again)
    assert mapped.grow(np.zeros(10), 20) is None  # plain memory
    assert mapped.grow(mapped.mapped_zeros(10, np.int64), 20) is None  # a name chunk: not growable
    assert mapped.grow(big[5:], 60_000) is None  # a view that starts inside the file

    monkeypatch.setattr(mapped.POLICY, "force_bytes", 0)
    store = _build(3000)
    files = {k: mapped._backings[mapped._mapping(a)] for k, a in store.cols.items()}  # type: ignore[index]
    cap, names = store._cap, _all_names(store)
    c, n = store.cols, len(store)
    leaf = int(np.flatnonzero((c["kind"][:n] == Kind.DIR) & (c["child_count"][:n] == 0))[0])
    first = store.ingest_names(  # one listing larger than the headroom: the columns grow
        leaf,
        [f"g{i}".encode() for i in range(cap)],
        {k: np.zeros(cap, dtype=COLUMNS[k]) for k in ENTRY_COLUMNS},
    )
    assert store._cap > cap and _all_names(store)[:first] == names
    assert all(mapped._backings[mapped._mapping(a)] is files[k] for k, a in store.cols.items())  # type: ignore[index]


def test_policy_maps_beyond_the_budget(monkeypatch: pytest.MonkeyPatch) -> None:
    policy = mapped.Policy()
    policy.force_bytes = None
    policy.large = None  # the budget alone (large stores: the next test)
    monkeypatch.setattr(mapped, "physical_memory", lambda: 1000)
    policy.fraction = 0.5
    assert not policy.should_map(None, 400)
    assert policy.should_map(None, 600)
    monkeypatch.setattr(mapped, "physical_memory", lambda: None)
    assert not policy.should_map(None, 10**12)  # unknown RAM: stay in plain memory


def test_policy_maps_large_stores_by_themselves(monkeypatch: pytest.MonkeyPatch) -> None:
    policy = mapped.Policy()
    policy.force_bytes = None
    monkeypatch.setattr(mapped, "physical_memory", lambda: 1 << 40)  # 1 TiB: the budget never decides
    assert policy.large is None  # 「大扫描存进映射文件」 is off by default
    assert not policy.should_map(None, 4 * mapped.LARGE_BYTES)
    policy.large = mapped.LARGE_BYTES  # switched on
    assert not policy.should_map(None, mapped.LARGE_BYTES - 1)
    assert policy.should_map(None, mapped.LARGE_BYTES)
    monkeypatch.setattr(mapped, "physical_memory", lambda: None)
    assert policy.should_map(None, mapped.LARGE_BYTES)  # also with the RAM unknown
    policy.large = None  # 「大扫描存进映射文件」 off: only the budget
    assert not policy.should_map(None, 4 * mapped.LARGE_BYTES)
    policy.large = mapped.LARGE_BYTES
    policy.force_bytes = 10**15  # SPACEKNIFE_MAP_ABOVE replaces both rules
    assert not policy.should_map(None, 4 * mapped.LARGE_BYTES)


def test_a_scan_maps_once_it_grows_large(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(mapped, "physical_memory", lambda: 1 << 40)
    monkeypatch.setattr(mapped.POLICY, "large", 1 << 20)  # 1 MiB: about 9000 rows with their names
    small = _build(2000)
    assert not small.mapped  # small scans stay in plain memory
    store = _build(30_000)
    assert store.mapped and all(mapped.is_mapped(a) for a in store.cols.values())
    names = _all_names(store)
    store.compact()  # the end of the scan: the shrunk columns stay mapped
    assert all(mapped.is_mapped(a) for a in store.cols.values()) and _all_names(store) == names
    loaded = NodeStore.from_columns(store.export_columns(), store.names_blob, store.ext_table, separated=True)
    assert loaded.mapped and all(mapped.is_mapped(a) for a in loaded.cols.values())  # a snapshot or MFT tree
    monkeypatch.setattr(mapped.POLICY, "large", None)
    assert not _build(30_000).mapped
