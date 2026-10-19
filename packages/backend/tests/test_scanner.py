from __future__ import annotations

import asyncio
import os
import sys
from pathlib import Path

import numpy as np
import pytest

from spaceknife.scan import worker
from spaceknife.scan.scanner import ScanError, Scanner, ScanState, make_pool, native_threads
from spaceknife.scan.volume import volume_for
from spaceknife.scan.worker import SliceResult
from spaceknife.store.nodes import Flag, Kind

from .tree_fixture import TreeFacts, build_tree


@pytest.fixture
def tree(tmp_path: Path) -> TreeFacts:
    return build_tree(tmp_path / "root")


def check_totals(scanner: Scanner, facts: TreeFacts) -> None:
    store = scanner.store
    root = scanner.roots[0].node
    v = store.view(root)
    assert v.tot_files == facts.files
    assert v.tot_dirs == facts.dirs
    assert v.tot_size == facts.size
    store.reaggregate()
    assert store.view(root).tot_size == facts.size


async def test_inline_scan_matches_tree(tree: TreeFacts) -> None:
    scanner = Scanner([str(tree.root)])
    result = await scanner.run()
    assert result.state == ScanState.DONE
    assert result.errors == 0
    check_totals(scanner, tree)
    movie = scanner.store.find(str(tree.root / "a" / "sub" / "movie.mp4"))
    assert movie is not None
    view = scanner.store.view(movie)
    assert (view.size, view.ext, view.kind) == (123456, "mp4", Kind.FILE)
    photo = scanner.store.find(str(tree.root / "中文 文件夹" / "照片.jpg"))
    assert photo is not None
    assert scanner.store.name(photo) == "照片.jpg"


async def test_zero_budget_slices_give_identical_result(tree: TreeFacts) -> None:
    scanner = Scanner([str(tree.root)], budget_s=0.0)
    await scanner.run()
    check_totals(scanner, tree)


async def test_process_pool_scan_matches_inline(tree: TreeFacts) -> None:
    pool = make_pool(2)
    try:
        scanner = Scanner([str(tree.root)], executor=pool, concurrency=2, budget_s=0.001)
        result = await scanner.run()
    finally:
        pool.shutdown()
    assert result.state == ScanState.DONE
    check_totals(scanner, tree)


async def test_cancel_leaves_consistent_partial_tree(tree: TreeFacts) -> None:
    scanner = Scanner([str(tree.root)], budget_s=0.0)
    seen: list[ScanState] = []

    def on_progress(p: object) -> None:
        seen.append(scanner.state)
        scanner.cancel()

    result = await scanner.run(on_progress=on_progress, progress_interval=0.0)
    assert result.state == ScanState.CANCELLED
    assert result.files < tree.files
    pending = (scanner.store.col("flags") & int(Flag.PENDING)) != 0
    assert pending.any()
    before = scanner.store.col("tot_size").copy()
    scanner.store.reaggregate()
    assert (scanner.store.col("tot_size") == before).all()


async def test_progress_callback_and_final_report(tree: TreeFacts) -> None:
    reports = []
    scanner = Scanner([str(tree.root)], budget_s=0.0)
    result = await scanner.run(on_progress=reports.append, progress_interval=0.0)
    assert reports
    assert reports[-1].state == ScanState.DONE
    assert result.items == tree.files + tree.dirs + 1  # + the root node itself
    assert result.rate > 0


def test_bad_roots(tmp_path: Path) -> None:
    with pytest.raises(ScanError, match="cannot access"):
        Scanner([str(tmp_path / "missing")])
    (tmp_path / "f.txt").write_text("x")
    with pytest.raises(ScanError, match="not a directory"):
        Scanner([str(tmp_path / "f.txt")])
    with pytest.raises(ScanError):
        Scanner([])


def test_nested_roots_are_deduplicated(tree: TreeFacts) -> None:
    scanner = Scanner([str(tree.root), str(tree.root / "a")])
    assert [r.path for r in scanner.roots] == [str(tree.root)]


async def test_multiple_roots(tmp_path: Path) -> None:
    a = build_tree(tmp_path / "one")
    b = build_tree(tmp_path / "two")
    scanner = Scanner([str(a.root), str(b.root)])
    await scanner.run()
    assert len(scanner.roots) == 2
    assert scanner.store.view(0).tot_size == a.size + b.size


@pytest.mark.skipif(sys.platform != "win32", reason="NTFS allocation heuristics")
async def test_windows_allocation_rounding(tmp_path: Path) -> None:
    if volume_for(str(tmp_path)).fs_type.upper() != "NTFS":
        pytest.skip("temp dir is not on NTFS")
    (tmp_path / "small.txt").write_bytes(b"x" * 100)
    (tmp_path / "big.bin").write_bytes(b"x" * 5000)
    scanner = Scanner([str(tmp_path)])
    await scanner.run()
    cluster = scanner.roots[0].volume.cluster_size  # type: ignore[union-attr]
    small = scanner.store.view(scanner.store.find(str(tmp_path / "small.txt")))  # type: ignore[arg-type]
    big = scanner.store.view(scanner.store.find(str(tmp_path / "big.bin")))  # type: ignore[arg-type]
    assert small.alloc == 0  # resident in the MFT record
    assert big.alloc == -(-5000 // cluster) * cluster


@pytest.mark.skipif(sys.platform == "win32", reason="POSIX link and permission semantics")
async def test_posix_links_hardlinks_and_denied(tmp_path: Path) -> None:
    root = tmp_path / "r"
    (root / "dir").mkdir(parents=True)
    (root / "dir" / "data").write_bytes(b"x" * 10_000)
    os.link(root / "dir" / "data", root / "hard")
    os.symlink(root / "dir", root / "link-to-dir")
    locked = root / "locked"
    locked.mkdir()
    (locked / "secret").write_bytes(b"x" * 10)
    locked.chmod(0)
    try:
        scanner = Scanner([str(root)])
        result = await scanner.run()
    finally:
        locked.chmod(0o755)
    store = scanner.store
    link = store.find(str(root / "link-to-dir"))
    assert link is not None and store.view(link).kind == Kind.LINK
    assert store.view(link).child_count == 0
    data = store.view(store.find(str(root / "dir" / "data")))  # type: ignore[arg-type]
    hard = store.view(store.find(str(root / "hard")))  # type: ignore[arg-type]
    # Exactly one of the two links is counted; which one depends on listing order.
    assert bool(data.flags & Flag.HARDLINK_DUP) != bool(hard.flags & Flag.HARDLINK_DUP)
    data_alloc = data.alloc
    # Counted once for the two hard links, plus the symlink's own size (the length of its target path).
    assert store.view(scanner.roots[0].node).tot_size == 10_000 + store.view(link).size
    assert data_alloc > 0
    if os.geteuid() != 0:
        node = store.find(str(locked))
        assert node is not None and store.view(node).flags & Flag.ACCESS_DENIED
        assert result.errors == 1


async def test_scan_runs_inside_running_loop_without_blocking(tree: TreeFacts) -> None:
    scanner = Scanner([str(tree.root)], budget_s=0.0)
    ticks = 0

    async def ticker() -> None:
        nonlocal ticks
        while scanner.state != ScanState.DONE:
            ticks += 1
            await asyncio.sleep(0)

    await asyncio.gather(scanner.run(), ticker())
    assert ticks > 0


def test_progress_by_items_else_bytes_and_unknown_past_the_ceiling(tmp_path: Path) -> None:
    """Used space is no exact total (APFS clones can pass it): past 95 % of it the progress is
    unknown instead of stuck at 99 %; the last scan's item count wins when there is one."""
    from dataclasses import replace

    scanner = Scanner([str(tmp_path)])
    root = scanner.roots[0]
    root.volume = replace(volume_for(str(tmp_path)), total=4000, free=3000, reserved=0)  # 1000 used
    root.whole_volume = True
    scanner.state = ScanState.SCANNING
    tot = scanner.store.cols["tot_alloc"]
    tot[0] = 500
    assert scanner.progress().progress == pytest.approx(0.5)
    tot[0] = 960
    assert scanner.progress().progress is None
    tot[0] = 2000  # more than the volume says it uses
    assert scanner.progress().progress is None
    scanner.expected_items = 10  # the last scan of this root: by items, whatever the bytes say
    now = scanner.progress()
    assert now.progress == pytest.approx(now.items / 10)
    scanner.state = ScanState.DONE
    assert scanner.progress().progress == 1.0


async def test_profile_accounts_for_the_scan(tree: TreeFacts) -> None:
    scanner = Scanner([str(tree.root)], budget_s=0.0)
    assert scanner.profile().slices == 0  # not started: nothing to show yet
    result = await scanner.run()
    p = scanner.profile()
    assert p.items == result.items - 1  # all but the root, which the scan starts from
    assert p.slices >= tree.dirs  # a zero budget lists one folder per slice
    assert p.wall_s > 0 and p.busy_s > 0 and 0 < p.ingest_s < p.wall_s
    assert p.busy > 0 and 0 < p.ingest < 1 and p.limited == 0  # no disk-load limit here
    assert p.threads == scanner.concurrency
    text = p.text()
    assert f"{p.items:,} items" in text and "listing threads busy" in text


async def test_profile_leaves_out_the_time_paused(tree: TreeFacts) -> None:
    scanner = Scanner([str(tree.root)])
    scanner.pause()
    task = asyncio.create_task(scanner.run())
    await asyncio.sleep(0.3)
    scanner.resume()
    await task
    p = scanner.profile()
    assert p.wall_s < 0.25 < scanner.progress().elapsed_s  # the elapsed time shown includes the pause


def test_scan_threads_can_be_set_for_measurements(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("SPACEKNIFE_SCAN_THREADS", "24")
    assert native_threads() == 24
    monkeypatch.setenv("SPACEKNIFE_SCAN_THREADS", "500")
    assert native_threads() == 64
    for bad in ("0", "-3", "many", ""):
        monkeypatch.setenv("SPACEKNIFE_SCAN_THREADS", bad)
        assert 2 <= native_threads() <= 16


@pytest.mark.parametrize(
    ("platform", "cpus", "threads"),
    [
        ("win32", 4, 4),
        ("win32", 32, 16),
        ("linux", 1, 2),
        ("darwin", 1, 4),
        ("darwin", 3, 6),
        ("darwin", 12, 16),
    ],
)
def test_default_listing_threads(
    monkeypatch: pytest.MonkeyPatch, platform: str, cpus: int, threads: int
) -> None:
    monkeypatch.delenv("SPACEKNIFE_SCAN_THREADS", raising=False)
    monkeypatch.setattr(sys, "platform", platform)
    monkeypatch.setattr(os, "cpu_count", lambda: cpus)
    assert native_threads() == threads


def _store_rows(scanner: Scanner) -> list[tuple[object, ...]]:
    store = scanner.store
    return sorted(
        (
            store.path(i),
            v.kind,
            v.flags,
            v.size,
            v.alloc,
            v.tot_size,
            v.tot_alloc,
            v.tot_files,
            v.tot_dirs,
            v.ext,
        )
        for i in range(1, len(store))
        for v in [store.view(i)]
    )


async def test_slices_folded_together_build_the_same_tree(tree: TreeFacts) -> None:
    """Slices that land together are folded as one (worker.merge_slices): the tree is the same as
    folding them one by one, references between listings, frontiers and extensions included."""
    recorded: list[SliceResult] = []
    live = Scanner([str(tree.root)], budget_s=0.0)  # one folder per slice: many small slices
    fold = live._fold
    live._fold = lambda res: (recorded.append(res), fold(res))[1]  # type: ignore[method-assign]
    await live.run()
    assert len(recorded) > 5

    def replay(group: int) -> Scanner:
        sc = Scanner([str(tree.root)])
        i = 0
        while i < len(recorded):
            n, j = len(sc.store), i + 1
            while j < len(recorded) and j - i < group:  # only slices whose folders exist already
                refs = np.asarray(recorded[j].refs).reshape(-1, 3)
                if int(refs[refs[:, 0] == 0, 1].max(initial=0)) >= n:
                    break
                j += 1
            sc._apply(*recorded[i:j])
            i = j
        return sc

    one_by_one = _store_rows(replay(1))
    assert one_by_one == _store_rows(live)
    for group in (2, 3, 8):
        assert _store_rows(replay(group)) == one_by_one


def test_merging_renumbers_references_and_extensions() -> None:
    a = worker.SliceResult(
        refs=np.array([[0, 5, 0], [1, 0, 1]]),
        paths=["/r/a", "/r/a/x"],
        bounds=np.array([0, 2, 3]),
        name_blob=b"p.txtxq.md",
        name_lens=np.array([5, 1, 4], dtype=np.uint16),
        cols={"kind": np.array([1, 0, 1], dtype=np.uint8)},
        ext_table=["", "txt", "md"],
        ext_ids=np.array([1, 0, 2], dtype=np.uint16),
        frontier=[((1, 1, 0), "/r/a/x/q.md", 0)],
        errors=[((1, 0, 0), "/r/a/p.txt", "denied")],
        elapsed=0.5,
    )
    b = worker.SliceResult(
        refs=np.array([[0, 7, 0], [1, 0, 0]]),
        paths=["/r/b", "/r/b/y"],
        bounds=np.array([0, 1, 2]),
        name_blob=b"yz.md",
        name_lens=np.array([1, 4], dtype=np.uint16),
        cols={"kind": np.array([1, 0], dtype=np.uint8)},
        ext_table=["", "md"],
        ext_ids=np.array([0, 1], dtype=np.uint16),
        frontier=[((1, 1, 0), "/r/b/y/z.md", 0), ((0, 9, 0), "/r/c", 0)],
        errors=[],
        elapsed=0.25,
    )
    m = worker.merge_slices([a, b])
    assert m.refs.tolist() == [[0, 5, 0], [1, 0, 1], [0, 7, 0], [1, 2, 0]]
    assert m.bounds.tolist() == [0, 2, 3, 4, 5]
    assert m.name_blob == b"p.txtxq.mdyz.md" and m.name_lens.tolist() == [5, 1, 4, 1, 4]
    assert [m.ext_table[i] for i in m.ext_ids.tolist()] == ["txt", "", "md", "", "md"]
    assert m.frontier == [
        ((1, 1, 0), "/r/a/x/q.md", 0),
        ((1, 3, 0), "/r/b/y/z.md", 0),
        ((0, 9, 0), "/r/c", 0),
    ]
    assert m.errors == [((1, 0, 0), "/r/a/p.txt", "denied")]
    assert m.paths == ["/r/a", "/r/a/x", "/r/b", "/r/b/y"] and m.elapsed == 0.75
    assert worker.merge_slices([a]) is a
