"""Acceptance: live monitoring and the ``tree_changed`` hook on a real backend (round 5's probes
``rapid``, ``rapidslow``, ``lostwrite`` and ``newfolder``). The files change on disk; the scan's
monitor applies the changes; the probe's hook reads the tree pinned to the call's epoch and records
what it was told (``TreeChange.paths/before/after``) and what it read."""

from __future__ import annotations

import asyncio
import os
import shutil
from pathlib import Path
from typing import Any

import pytest

from .harness import Backend, by_path, follows_on, key

pytestmark = pytest.mark.timeout(240)


async def _changes(backend: Backend, pid: str) -> list[dict[str, Any]]:
    return list((await backend.dump(pid))["changes"])


async def _heard(backend: Backend, pid: str, sizes: dict[str, int]) -> list[dict[str, Any]]:
    """The hook's records, once each folder of ``sizes`` was last told its size there."""

    def done(changes: list[dict[str, Any]]) -> bool:
        got = by_path(changes)
        return all(got.get(k) and got[k][-1][1] == v for k, v in sizes.items())

    return await backend.until(f"the hook to hear {sizes}", lambda: _changes(backend, pid), done)


def _no_hook_trouble(backend: Backend, pid: str) -> None:
    """No tree_changed call failed or was given up on (the host's warnings about the hook)."""
    trouble = [line for line in backend.log().splitlines() if f"hook tree_changed of {pid}" in line]
    assert not trouble, "\n".join(trouble)


async def test_rapid_changes_reach_the_hook_in_order(backend: Backend, probe: str, data: Path) -> None:
    """Round 5's ``rapid``: three bursts of new files 50 ms apart in a watched folder (and one in
    another folder each time). Each new file reorganises the tree (a new epoch) while the monitor
    gathers them; every call the hook gets reads its own tree, and each folder's sizes follow on
    from call to call, from before the first file to the size at the end. The next burst starts
    once the hook was told the last one, so there are several calls."""
    root = data / "rapid"
    (root / "dl").mkdir(parents=True)
    (root / "other" / "deep").mkdir(parents=True)
    (root / "dl" / "seed.bin").write_bytes(b"s" * 10_000)
    sid = await backend.scan(root)
    await backend.watching(sid)
    # the folders whose files change (TreeChange lists those, not every folder above them)
    dl, deep = key(root / "dl"), key(root / "other" / "deep")
    folders = {dl: root / "dl", deep: root / "other" / "deep"}

    async def sizes() -> dict[str, int]:
        return {k: int((await backend.node(sid, p))["alloc"]) for k, p in folders.items()}

    async def now() -> tuple[dict[str, int], list[dict[str, Any]]]:
        return await sizes(), await _changes(backend, probe)

    first = await sizes()
    last: dict[str, int] = {}
    changes: list[dict[str, Any]] = []
    for burst in range(1, 4):
        for i in range(4):
            (root / "dl" / f"part{burst}-{i}.bin").write_bytes(b"p" * 200_000)
            await asyncio.sleep(0.05)  # the pace of the changes, not a wait for a result
        (root / "other" / "deep" / f"x{burst}.bin").write_bytes(b"x" * 50_000)
        want = {dl: first[dl] + burst * 4 * 200_000, deep: first[deep] + burst * 50_000}

        def settled(v: tuple[dict[str, int], list[dict[str, Any]]], want: dict[str, int] = want) -> bool:
            s, ch = v
            got = by_path(ch)
            return all(s[k] >= want[k] and got.get(k) and got[k][-1][1] == s[k] for k in want)

        # the burst in the tree, and the hook told the sizes the tree has now (writes still
        # landing move both on: they agree once all of it is in)
        last, changes = await backend.until(f"burst {burst} in the tree and told to the hook", now, settled)
    got = by_path(changes)
    for k in first:
        assert follows_on(got[k], first[k], last[k]), (k, got[k])
    assert len(got[dl]) >= 3, got[dl]  # a call or more per burst
    for c in changes:
        assert c["scan"] == sid and c["tree"] == sid and c["tree_epoch"] == c["epoch"], c
    _no_hook_trouble(backend, probe)


async def test_a_hook_that_meets_a_newer_tree_hears_both_changes_at_once(
    backend: Backend, probe: str, data: Path, tmp_path: Path
) -> None:
    """The hook hearing of one file waits until a second one has reorganised the tree, then reads
    its (older) tree: STALE_TREE. The host calls it again once, with both changes, from the size
    before the first file to the size after the second, and that is the tree it reads (round 5's
    ``rapidslow``: both calls were told 1,216,512 → 2,420,736, the first should have ended at
    1,216,512). The files arrive whole (renamed in), so nothing changes after the second one."""
    root = data / "slow"
    (root / "dl").mkdir(parents=True)
    (root / "dl" / "seed.bin").write_bytes(b"s" * 12_000)
    outside = tmp_path / "outside"
    outside.mkdir()
    for name in ("one.bin", "two.bin"):
        (outside / name).write_bytes(name[0].encode() * 1_200_000)
    sid = await backend.scan(root)
    await backend.watching(sid)
    dl = key(root / "dl")
    first = (await backend.node(sid, root / "dl"))["alloc"]
    calls_before = (await backend.dump(probe))["hook_calls"]
    await backend.pcall(probe, "hold_next")
    os.replace(outside / "one.bin", root / "dl" / "one.bin")
    await backend.until(
        "the hook to hold", lambda: backend.pcall(probe, "dump"), lambda d: d["hold"]["holding"]
    )
    os.replace(outside / "two.bin", root / "dl" / "two.bin")

    async def size() -> int:
        return int((await backend.node(sid, root / "dl"))["alloc"])

    last = await backend.until("both files in the tree", size, lambda s: s >= first + 2_400_000)
    changes = await _heard(backend, probe, {dl: last})
    steps = by_path(changes)[dl]
    assert steps == [(first, last)], steps  # one call for both
    seen = await backend.dump(probe)
    assert seen["hook_calls"] - calls_before >= 2  # the held call, and the one again
    for c in changes:
        assert c["read"] == c["after"], c  # what it is told is the tree it reads
    _no_hook_trouble(backend, probe)


async def test_a_file_written_right_after_the_scan_reaches_the_tree_and_the_hook(
    backend: Backend, probe: str, data: Path
) -> None:
    """Round 5's ``lostwrite``: 91 folders; the moment the scan says done, 0.5 MB go into the last
    one. Linux watches each folder on its own, placed after the scan listed it; it checks the
    folder against the mtime the scan saw when the watch is placed, so the file is not lost
    (Windows and macOS watch the whole tree from the scan's start)."""
    root = data / "lost"
    for i in range(30):
        deep = root / f"d{i}" / "sub" / "deep"
        deep.mkdir(parents=True)
        (deep / "f.bin").write_bytes(b"x" * 1000)
    sid = await backend.scan(root)
    late = root / "d29" / "sub" / "deep" / "late.bin"
    late.write_bytes(b"l" * 500_000)

    async def found() -> int | None:
        return await backend.find(sid, late)

    await backend.until("late.bin in the tree", found, lambda n: n is not None)
    assert (await backend.node(sid, late))["size"] == 500_000
    deep = key(late.parent)
    changes = await backend.until(
        "the hook to hear of the folder",
        lambda: _changes(backend, probe),
        lambda ch: any(after >= 500_000 for _b, after in by_path(ch).get(deep, [])),
    )
    assert all(c["scan"] == sid for c in changes)
    _no_hook_trouble(backend, probe)


async def test_folders_that_come_or_go_with_contents_count_all_of_it(
    backend: Backend, probe: str, data: Path, tmp_path: Path
) -> None:
    """Round 5's ``newfolder``: a tree moved in (three nested folders, 1 MB in each), a new folder
    given a 2 MB file, a folder with 1.5 MB removed. Each folder's sizes run from what it took
    before (0 for the new ones; they used to grow by their own 4096-byte block only) to what the
    tree has at the end, over however many calls the monitor's batches make of it."""
    root = data / "sizes"
    (root / "dl").mkdir(parents=True)
    (root / "dl" / "seed.bin").write_bytes(b"s" * 10_000)
    (root / "old" / "inner").mkdir(parents=True)
    (root / "old" / "a.bin").write_bytes(b"o" * 1_000_000)
    (root / "old" / "inner" / "b.bin").write_bytes(b"o" * 500_000)
    outside = tmp_path / "outside"
    (outside / "x" / "deep" / "deeper").mkdir(parents=True)
    for rel in ("a.bin", "deep/b.bin", "deep/deeper/c.bin"):
        (outside / "x" / rel).write_bytes(b"m" * 1_000_000)
    (outside / "f.bin").write_bytes(b"c" * 2_000_000)
    sid = await backend.scan(root)
    await backend.watching(sid)
    start = {
        key(root): (await backend.node(sid, root))["alloc"],
        key(root / "dl"): (await backend.node(sid, root / "dl"))["alloc"],
    }

    os.rename(outside / "x", root / "dl" / "x")  # moved (extracted, cloned) in at once
    (root / "c").mkdir()
    os.replace(outside / "f.bin", root / "c" / "f.bin")  # the file comes whole
    shutil.rmtree(root / "old")

    async def applied() -> bool:
        return (
            await backend.find(sid, root / "dl" / "x" / "deep" / "deeper" / "c.bin") is not None
            and await backend.find(sid, root / "c" / "f.bin") is not None
            and await backend.find(sid, root / "old") is None
        )

    await backend.until("the changes in the tree", applied)
    end = {
        key(p): (await backend.node(sid, p))["alloc"]
        for p in (root, root / "dl", root / "dl" / "x", root / "c")
    }
    got = by_path(await _heard(backend, probe, end))
    for k in start:
        assert follows_on(got[k], start[k], end[k]), (k, got[k])
    for k in (key(root / "dl" / "x"), key(root / "c")):  # new: from nothing
        assert follows_on(got[k], 0, end[k]), (k, got[k])
    assert end[key(root / "dl" / "x")] >= 3_000_000 and end[key(root / "c")] >= 2_000_000
    _no_hook_trouble(backend, probe)
