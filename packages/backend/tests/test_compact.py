"""store.mutate.compact gathers the live tree piece by piece: the same store as gathering it all at
once, without one index per name byte of the whole store (+6.3 GB on a volume of 8.7 million)."""

from __future__ import annotations

import tracemalloc

import numpy as np
import pytest

from spaceknife.store import mapped, mutate
from spaceknife.store.nodes import COLUMNS, ENTRY_COLUMNS, Kind, NodeStore
from spaceknife.store.subtree import ranges_to_ids


@pytest.fixture(autouse=True)
def plain_memory(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(mapped.POLICY, "force_bytes", None)


def entries(kinds: list[int], rng: np.random.Generator) -> dict[str, np.ndarray]:
    m = len(kinds)
    out = {key: np.zeros(m, dtype=COLUMNS[key]) for key in ENTRY_COLUMNS}
    out["kind"][:] = kinds
    files = out["kind"] == Kind.FILE
    out["size"][files] = rng.integers(0, 1 << 30, int(files.sum()))
    out["alloc"][files] = out["size"][files] // 4096 * 4096
    out["mtime"][:] = rng.integers(0, 2**31, m)
    return out


def listing(names: list[bytes], kinds: list[int], rng: np.random.Generator):  # type: ignore[no-untyped-def]
    lens = np.array([len(x) for x in names], dtype=np.uint16)
    return b"".join(names), lens, entries(kinds, rng)


def list_new_folders(store: NodeStore, folders: list[int]) -> None:
    empty = {key: np.zeros(0, dtype=COLUMNS[key]) for key in ENTRY_COLUMNS}
    for f in folders:
        store.ingest_names(f, [], empty)


def churned_store(seed: int, folders: int = 60, relists: int = 120) -> NodeStore:
    """A tree whose folders were listed again many times: renamed, deleted and new entries."""
    rng = np.random.default_rng(seed)
    store = NodeStore()
    root = store.add_root("/r")
    todo, dirs, k = [root], [root], 0
    while todo:
        d = todo.pop()
        m = int(rng.integers(0, 30))
        sub = int(rng.integers(0, 3)) if len(dirs) < folders else 0
        kinds = [Kind.DIR] * min(sub, m) + [Kind.FILE] * (m - min(sub, m))
        names = [f"名字{k}-{i}.{'txt' if i % 3 else 'mp4'}".encode() for i in range(m)]
        k += 1
        blob, lens, ent = listing(names, kinds, rng)
        start = store.ingest_listing(d, blob, lens, ent)
        new = list(range(start, start + min(sub, m)))
        todo.extend(new)
        dirs.extend(new)
    for r in range(relists):
        live = [d for d in dirs if mutate.can_relist(store, d)]
        d = int(rng.choice(live))
        kids = store.children(d)
        keep = [store.name_bytes(int(i)) for i in kids if rng.random() < 0.7]
        kinds = [int(store.cols["kind"][int(i)]) for i in kids if store.name_bytes(int(i)) in keep]
        more = int(rng.integers(0, 12))
        names = keep + [f"new{r}-{i}".encode() for i in range(more)]
        kinds += [Kind.FILE if i % 4 else Kind.DIR for i in range(more)]
        blob, lens, ent = listing(names, kinds, rng)
        rel = mutate.relist(store, d, blob, lens, ent)
        list_new_folders(store, rel.pending.tolist())
        dirs = [x for x in dirs if not mutate.is_gone(store.cols["flags"][x : x + 1])[0]]
        dirs += [int(x) for x in rel.added if store.cols["kind"][int(x)] == Kind.DIR]
        dirs += [int(x) for x in rel.moved_to if store.cols["kind"][int(x)] == Kind.DIR]
    return store


def whole_compact(store: NodeStore) -> tuple[NodeStore, np.ndarray]:
    """The compaction before it was chunked: the whole store at once (the reference)."""
    c = store.cols
    n = len(store)
    order_parts = [np.array([0], dtype=np.int64)]
    level = order_parts[0]
    is_dir = c["kind"][:n] == Kind.DIR
    is_dir[0] = True
    while level.size:
        dirs = level[is_dir[level]]
        level = ranges_to_ids(
            c["child_start"][dirs].astype(np.int64), c["child_count"][dirs].astype(np.int64)
        )
        if level.size:
            order_parts.append(level)
    order = np.concatenate(order_parts)
    remap = np.full(n, -1, dtype=np.int64)
    remap[order] = np.arange(len(order), dtype=np.int64)
    cols = {key: col[:n][order] for key, col in c.items()}
    parent = cols["parent"].astype(np.int64)
    cols["parent"] = np.where(parent >= 0, remap[np.maximum(parent, 0)], -1).astype(c["parent"].dtype)
    listed = cols["child_count"] > 0
    start = cols["child_start"].astype(np.int64)
    cols["child_start"] = np.where(listed, remap[np.where(listed, start, 0)], 0).astype(
        c["child_start"].dtype
    )
    image = np.frombuffer(store.names_blob, dtype=np.uint8)
    ln = cols["name_len"].astype(np.int64)
    off = store._heap.image_positions(cols["name_off"].astype(np.int64))
    names = image[ranges_to_ids(off, ln)].tobytes()
    cols["name_off"] = (np.cumsum(ln) - ln).astype(np.int64)
    return NodeStore.from_columns(cols, names, store.ext_table), remap


@pytest.mark.parametrize("seed", range(4))
def test_compact_matches_the_whole_store_gather(seed: int, monkeypatch: pytest.MonkeyPatch) -> None:
    store = churned_store(seed)
    assert mutate.gone_count(store) > 0
    want, want_remap = whole_compact(store)
    for chunk, name_chunk in ((1 << 18, 1 << 15), (7, 3), (1, 1)):
        monkeypatch.setattr(mutate, "COMPACT_CHUNK", chunk)
        monkeypatch.setattr(mutate, "COMPACT_NAME_CHUNK", name_chunk)
        got, remap = mutate.compact(store)
        assert remap.tolist() == want_remap.tolist()
        assert len(got) == len(want) and mutate.gone_count(got) == 0
        for key in COLUMNS:
            if key != "name_off":
                assert got.col(key).tolist() == want.col(key).tolist(), key
        assert got.names_blob == want.names_blob
        assert [got.name_bytes(i) for i in range(len(got))] == [want.name_bytes(i) for i in range(len(want))]


def test_compact_reads_names_from_every_heap_chunk(monkeypatch: pytest.MonkeyPatch) -> None:
    from spaceknife.store import heap

    monkeypatch.setattr(heap, "FIRST_CHUNK", 256)
    monkeypatch.setattr(heap, "CHUNK", 512)
    store = churned_store(9, folders=30, relists=60)
    assert len(store._heap._chunks) > 5
    want, _ = whole_compact(store)
    got, _ = mutate.compact(store)
    assert [got.name_bytes(i) for i in range(len(got))] == [want.name_bytes(i) for i in range(len(want))]


def test_compact_needs_little_beyond_the_copy() -> None:
    """Peak beyond the new store: the order, remap and one piece's temporaries (was ~760 B a row)."""
    rng = np.random.default_rng(3)
    store = NodeStore()
    root = store.add_root("/r")
    names = [f"some-longer-file-name-{i:07d}.extension".encode() for i in range(300_000)]
    blob, lens, ent = listing(names, [Kind.FILE] * len(names), rng)
    store.ingest_listing(root, blob, lens, ent)
    blob, lens, ent = listing(names[::2], [Kind.FILE] * len(names[::2]), rng)
    mutate.relist(store, root, blob, lens, ent)  # half of the rows are GONE now
    tracemalloc.start()
    try:
        fresh, _ = mutate.compact(store)
        _, peak = tracemalloc.get_traced_memory()
    finally:
        tracemalloc.stop()
    rows = len(store)
    kept = sum(a.nbytes for a in fresh.cols.values()) + fresh.names_size()
    assert len(fresh) == 1 + 1 + len(names[::2])
    assert (peak - kept) / rows < 40, f"{(peak - kept) / rows:.0f} B a row beyond the copy"
