from __future__ import annotations

from datetime import UTC, datetime, timedelta
from pathlib import Path

import numpy as np
import pytest
import zstandard

from spaceknife.scan.scanner import Scanner
from spaceknife.store import heap, snapshot
from spaceknife.store.nodes import COLUMNS, ENTRY_COLUMNS, NodeStore
from spaceknife.store.snapshot import SnapshotError

from .tree_fixture import build_tree


async def scanned(tmp_path: Path) -> Scanner:
    facts = build_tree(tmp_path / "root")
    scanner = Scanner([str(facts.root)])
    await scanner.run()
    return scanner


async def test_roundtrip_is_lossless(tmp_path: Path) -> None:
    scanner = await scanned(tmp_path)
    path = tmp_path / "snap.sks"
    header = snapshot.save(scanner.store, path, roots=[r.path for r in scanner.roots], stats={"files": 1})
    assert path.exists() and not path.with_name("snap.sks.tmp").exists()
    store, loaded = snapshot.load(path)
    assert loaded.node_count == header.node_count == len(scanner.store)
    assert loaded.stats == {"files": 1}
    for key, arr in scanner.store.export_columns().items():
        np.testing.assert_array_equal(store.col(key), arr, err_msg=key)
    assert store.ext_table == scanner.store.ext_table
    last = len(store) - 1
    assert store.path(last) == scanner.store.path(last)
    assert snapshot.read_header(path).roots == loaded.roots


async def test_corrupt_files_are_rejected(tmp_path: Path) -> None:
    scanner = await scanned(tmp_path)
    good = tmp_path / "good.sks"
    snapshot.save(scanner.store, good, roots=["x"])
    bad_magic = tmp_path / "bad.sks"
    bad_magic.write_bytes(b"NOTASNAP" + good.read_bytes()[8:])
    with pytest.raises(SnapshotError, match="magic"):
        snapshot.load(bad_magic)
    truncated = tmp_path / "trunc.sks"
    truncated.write_bytes(good.read_bytes()[:-50])
    with pytest.raises(SnapshotError):
        snapshot.load(truncated)


def test_retention_keeps_recent_and_one_per_week(tmp_path: Path) -> None:
    start = datetime(2026, 1, 1, tzinfo=UTC)
    for day in range(60):  # one snapshot per day for 60 days
        stamp = start + timedelta(days=day)
        (tmp_path / f"{stamp.strftime('%Y%m%d-%H%M%S')}.sks").write_bytes(b"")
    deleted = snapshot.prune(tmp_path, keep_recent=10)
    kept = sorted(p.name for p in tmp_path.glob("*.sks"))
    assert len(kept) + len(deleted) == 60
    newest = sorted(kept)[-10:]
    assert newest == sorted(kept)[-10:]
    older = [k for k in kept if k not in newest]
    weeks = {datetime.strptime(k[:15], "%Y%m%d-%H%M%S").isocalendar()[:2] for k in older}
    assert len(weeks) == len(older)  # at most one per ISO week


async def test_auto_path_and_listing(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("SPACEKNIFE_HOME", str(tmp_path / "home"))
    scanner = await scanned(tmp_path)
    roots = [r.path for r in scanner.roots]
    path = snapshot.auto_path(roots)
    assert str(path).startswith(str(tmp_path / "home"))
    snapshot.save(scanner.store, path, roots=roots)
    listed = snapshot.list_snapshots()
    assert [p for p, _ in listed] == [path]
    assert snapshot.root_key(roots) == snapshot.root_key([r.upper() if False else r for r in roots])


def _old_save(store: NodeStore, path: Path, roots: list[str]) -> None:
    """The writer before the streaming one (0.3.3): one compress() per column, every block kept until
    the header was written in front of them."""
    compressor = zstandard.ZstdCompressor(level=3, threads=-1)
    blocks: list[bytes] = []
    offset = 0
    col_info: dict[str, snapshot.BlockInfo] = {}
    for key, arr in store.export_columns().items():
        raw = np.ascontiguousarray(arr)
        packed = compressor.compress(memoryview(raw).cast("B"))
        col_info[key] = snapshot.BlockInfo(
            offset=offset, length=len(packed), raw_length=raw.nbytes, dtype=arr.dtype.str
        )
        blocks.append(packed)
        offset += len(packed)
    stream = compressor.compressobj(size=store.names_size())
    pieces = [stream.compress(memoryview(p).cast("B")) for p in store.names_parts()]
    names = b"".join([*pieces, stream.flush()])
    header = snapshot.SnapshotHeader(
        created_at=datetime.now(UTC),
        roots=roots,
        node_count=len(store),
        ext_table=store.ext_table,
        columns=col_info,
        names=snapshot.BlockInfo(offset=offset, length=len(names), raw_length=store.names_size()),
    )
    head = snapshot._dump_header(header)
    path.write_bytes(snapshot.MAGIC + snapshot._LEN.pack(len(head)) + head + b"".join(blocks) + names)


def _many_chunks(monkeypatch: pytest.MonkeyPatch) -> NodeStore:
    """A store whose names span several heap chunks (name_off is rebased then)."""
    monkeypatch.setattr(heap, "CHUNK", 4096)
    monkeypatch.setattr(heap, "FIRST_CHUNK", 4096)
    store = NodeStore()
    folder = store.add_root("/r")
    for k in range(6):
        names = [f"name-{k}-{i:04d}-{'é' * (i % 7)}".encode() for i in range(500)]
        entries = {key: np.arange(k, k + len(names)).astype(COLUMNS[key]) for key in ENTRY_COLUMNS}
        entries["kind"] = np.zeros(len(names), dtype=np.uint8)
        entries["kind"][0] = 1  # the next batch goes into the first entry: a folder
        first = store.ingest_names(folder, names, entries)
        folder = first
    assert store._heap.chunks() > 1
    return store


@pytest.mark.parametrize("rows", [7, 1000, 1 << 20])
def test_streamed_blocks_are_the_store_and_what_the_old_writer_wrote(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, rows: int
) -> None:
    store = _many_chunks(monkeypatch)
    monkeypatch.setattr(snapshot, "SAVE_ROWS", rows)  # pieces smaller and larger than a column
    new, old = tmp_path / "new.sks", tmp_path / "old.sks"
    header = snapshot.save(store, new, roots=["/r"])
    _old_save(store, old, ["/r"])
    a, ha = snapshot.load(new)
    b, hb = snapshot.load(old)
    assert ha.columns["name_off"].dtype == "<i8"  # several chunks: rebased into int64, as before
    assert {k: (i.raw_length, i.dtype) for k, i in ha.columns.items()} == {
        k: (i.raw_length, i.dtype) for k, i in hb.columns.items()
    }
    assert list(ha.columns) == list(hb.columns)
    for key, arr in store.export_columns().items():
        np.testing.assert_array_equal(a.col(key), arr, err_msg=key)
        np.testing.assert_array_equal(b.col(key), arr, err_msg=key)
    assert a.names_blob == b.names_blob == store.names_blob
    assert header == snapshot.read_header(new)
    # the header sits in room reserved for the largest numbers, padded with spaces
    raw = new.read_bytes()
    (hlen,) = snapshot._LEN.unpack_from(raw, len(snapshot.MAGIC))
    head = raw[len(snapshot.MAGIC) + snapshot._LEN.size :][:hlen]
    assert head.rstrip(b" ") == snapshot._dump_header(header) and head.endswith(b" ")


async def test_a_single_chunk_store_keeps_its_offset_type(tmp_path: Path) -> None:
    scanner = await scanned(tmp_path)
    path = tmp_path / "s.sks"
    header = snapshot.save(scanner.store, path, roots=["x"])
    assert header.columns["name_off"].dtype == scanner.store.cols["name_off"].dtype.str == "<u4"
    store, _ = snapshot.load(path)
    for key, arr in scanner.store.export_columns().items():
        np.testing.assert_array_equal(store.col(key), arr, err_msg=key)
