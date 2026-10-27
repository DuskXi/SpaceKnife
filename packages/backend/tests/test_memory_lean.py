"""Memory the backend no longer spends (docs/memory.md): snapshots open without a second copy,
the treemap and the palette work out sizes per node instead of an array of every node, scans
reserve their columns once, and the palette does not import pypinyin."""

from __future__ import annotations

import subprocess
import sys
import tracemalloc
from pathlib import Path

import numpy as np
import pytest

from spaceknife.layout.api import StoreSizes
from spaceknife.layout.basis import Basis, LazySizes
from spaceknife.layout.engine import LayoutParams, RootSpec, TreemapEngine
from spaceknife.store import mapped, snapshot
from spaceknife.store.nodes import COLUMNS, ENTRY_COLUMNS, Kind, NodeStore


@pytest.fixture(autouse=True)
def plain_memory(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(mapped.POLICY, "force_bytes", None)


def tree(files: int, folders: int = 50, seed: int = 1) -> NodeStore:
    rng = np.random.default_rng(seed)
    store = NodeStore()
    root = store.add_root("/r")
    m = folders + files
    entries = {k: np.zeros(m, dtype=COLUMNS[k]) for k in ENTRY_COLUMNS}
    entries["kind"][:folders] = Kind.DIR
    entries["kind"][folders:] = Kind.FILE
    entries["alloc"][folders:] = rng.integers(0, 1 << 24, files)
    first = store.ingest_names(root, [f"entry-{i:07d}.bin".encode() for i in range(m)], entries)
    empty = {k: np.zeros(0, dtype=COLUMNS[k]) for k in ENTRY_COLUMNS}
    for d in range(first, first + folders):
        store.ingest_names(d, [], empty)
    store.reaggregate()
    return store


def test_snapshots_open_without_a_second_copy(tmp_path: Path) -> None:
    store = tree(200_000)
    path = tmp_path / "s.sks"
    snapshot.save(store, path, roots=["/r"])
    tracemalloc.start()
    try:
        loaded, _ = snapshot.load(path)
        _, peak = tracemalloc.get_traced_memory()
    finally:
        tracemalloc.stop()
    size = sum(a.nbytes for a in loaded.cols.values()) + loaded.names_size()
    assert peak < 1.15 * size, f"peak {peak / size:.2f} × the store"  # was ~2× (bytes, then arrays)
    assert [loaded.name_bytes(i) for i in range(len(loaded))] == [
        store.name_bytes(i) for i in range(len(store))
    ]
    for key in COLUMNS:
        if key != "name_off":  # rebased: the loaded names are one block
            assert np.array_equal(loaded.col(key), store.col(key)), key


def test_a_damaged_snapshot_block_is_a_snapshot_error(tmp_path: Path) -> None:
    path = tmp_path / "s.sks"
    snapshot.save(tree(2000), path, roots=["/r"])
    data = bytearray(path.read_bytes())
    data[-40:-20] = b"\xff" * 20  # inside the names block
    path.write_bytes(bytes(data))
    with pytest.raises(snapshot.SnapshotError):
        snapshot.load(path)


def test_the_treemap_works_out_only_the_sizes_it_lays_out() -> None:
    store = tree(300)  # few enough to be drawn one by one
    basis = Basis(store)
    draw = TreemapEngine(basis).run(LayoutParams(width=800, height=600), RootSpec(node=1), StoreSizes(basis))
    assert len(draw.node) > 10
    assert basis._sizes is None  # no array of every node (8 B a node, again after every change)


def test_lazy_sizes_answer_like_the_array() -> None:
    store = tree(5000)
    basis = Basis(store)
    lazy = LazySizes(basis)
    full = basis.effective_sizes(store)
    assert len(lazy) == len(full)
    ids = np.random.default_rng(2).integers(0, len(store), 300)
    assert lazy[ids].tolist() == full[ids].tolist()
    assert lazy[7] == int(full[7]) and lazy[np.int64(9)] == int(full[9])
    assert basis._sizes is None


def test_a_scan_reserves_its_columns_once() -> None:
    store = NodeStore()
    store.add_root("/r")
    store.reserve(100_000)
    assert store._cap == 100_000
    names = store.names_size()
    store.reserve(10)  # never shrinks
    assert store._cap == 100_000 and store.names_size() == names
    entries = {k: np.zeros(99_000, dtype=COLUMNS[k]) for k in ENTRY_COLUMNS}
    entries["kind"][:] = Kind.FILE
    store.ingest_names(1, [f"f{j}".encode() for j in range(99_000)], entries)
    assert store._cap == 100_000  # the expected rows fit: no growth, no copy


def test_the_palette_does_not_import_pypinyin() -> None:
    code = (
        "import sys; from spaceknife.palette import commands; r = commands.registry(); "
        "keys = [k for k, _ in next(c for c in r if c.name == '切换配色').search]; "
        "print('pypinyin' in sys.modules, 'qhps' in keys)"
    )
    out = subprocess.run(
        [sys.executable, "-X", "utf8", "-c", code], capture_output=True, text=True, timeout=120
    )
    assert out.stdout.split() == ["False", "True"], out.stderr
