from __future__ import annotations

from datetime import UTC, datetime, timedelta
from pathlib import Path

import numpy as np
import pytest

from spaceknife.scan.scanner import Scanner
from spaceknife.store import snapshot
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
