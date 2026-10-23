"""The live monitor tells duplicate-file results about every file it saw modified, a same-size
rewrite in place included (docs/dupes.md §7.5)."""

from __future__ import annotations

import asyncio
from pathlib import Path
from typing import Any

import pytest

from .test_monitor_pipeline import isolated_home, scanned, watched

__all__ = ["isolated_home", "watched"]

MIB = 1 << 20


class Recorder:
    def __init__(self) -> None:
        self.calls: list[tuple[str, dict[int, int]]] = []

    def on_tree_changed(self, scan_id: str, changes: dict[int, int]) -> None:
        self.calls.append((scan_id, dict(changes)))

    def remap(self, scan_id: str, new_ids: Any) -> None:
        pass

    def has_dupes(self, scan_id: str) -> bool:
        return False


@pytest.mark.parametrize("same_size", [False, True])
async def test_a_modified_file_reaches_the_duplicate_results(
    watched: Any, tmp_path: Path, same_size: bool
) -> None:
    from spaceknife.store.lookup import PathIndex

    mgr, views, _monitors = watched
    root = tmp_path / "t"
    root.mkdir()
    f = root / "copy.bin"
    f.write_bytes(b"A" * (2 * MIB))
    handle = await scanned(mgr, root)
    rec = Recorder()
    views.dupes = rec
    node = PathIndex(handle.store).locate(str(f))[0]
    await asyncio.sleep(1.2)  # past the second the scan saw
    with open(f, "r+b") as fh:  # noqa: ASYNC230
        if same_size:
            fh.write(b"B" * 4096)  # rewritten in place: same size, other content
        else:
            fh.seek(0, 2)
            fh.write(b"B" * MIB)
    for _ in range(80):
        if any(node in ch for _, ch in rec.calls):
            break
        await asyncio.sleep(0.1)
    assert any(node in ch for _, ch in rec.calls), rec.calls
