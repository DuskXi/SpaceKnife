"""spaceknife.dupes: the engine on synthetic trees in temporary folders (docs/dupes.md §14.1).

Every test runs with the native reader (when built) and with the Python fallback. Read volumes are
checked against the design's exact formula: S2 reads the head and tail blocks, S3 reads each
verified member from 4 KiB (the head block was hashed in S2) to its size, and each member found
different at a checkpoint up to that checkpoint.
"""

from __future__ import annotations

import asyncio
import os
import stat
import sys
from pathlib import Path
from typing import Any

import numpy as np
import pytest

from spaceknife import native
from spaceknife.dupes import Job, JobOptions, KeepOptions, Results, ScanInput, Scope
from spaceknife.dupes import codes as C
from spaceknife.dupes.cache import ENTRY_DTYPE
from spaceknife.dupes.reads import FallbackOps, NativeOps, Ops
from spaceknife.dupes.select import plan, select
from spaceknife.scan.scanner import Scanner
from spaceknife.store.nodes import Flag

MIB = 1 << 20
OPS = ["python"] + (["native"] if native.has("DupesReader") else [])


def make_ops(name: str) -> Ops:
    return NativeOps() if name == "native" else FallbackOps()


@pytest.fixture(params=OPS)
def ops(request: pytest.FixtureRequest) -> Ops:
    return make_ops(request.param)


def write(root: Path, rel: str, data: bytes) -> Path:
    p = root / rel
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_bytes(data)
    return p


def blob(n: int, seed: int) -> bytes:
    return np.random.default_rng(seed).integers(0, 256, n, dtype=np.uint8).tobytes()


async def scan(*roots: Path, links_each: bool = False) -> list[ScanInput]:
    out = []
    for i, r in enumerate(roots):
        sc = Scanner([str(r)], count_links_each=links_each)
        await sc.run()
        out.append(ScanInput(f"s{i}", sc.store, sc.roots))
    return out


async def run(
    scans: list[ScanInput],
    ops: Ops,
    cache: Path,
    *,
    min_size: int = MIB,
    keep: KeepOptions | None = None,
    **opts: Any,
) -> tuple[Job, Results]:
    res = Results(scans, keep)
    job = Job(
        Scope(scans, min_size=min_size),
        options=JobOptions(cache_root=cache, racy_s=0.0, **opts),
        ops=ops,
        sink=res,
    )
    await job.run()
    assert job.state in ("done", "cancelled"), job.error
    return job, res


def groups(res: Results) -> list[set[str]]:
    out = []
    for g in res.valid_groups():
        if g.shared:
            continue
        out.append({os.path.basename(res.paths[r]) for r in range(g.start, g.start + g.count)})
    return sorted(out, key=sorted)


def read_bytes(job: Job) -> int:
    return job.counters.bytes_probe + job.counters.bytes_full


def probe(size: int) -> int:
    return C.probe_bytes(size)


def joins_from_cache(ops: Ops) -> None:
    """Tests of the cache joining files: the Python reader on Windows has no change time, so its
    entries are of weak trust and never join (docs/dupes.md §7.3; test_weak_volumes_never_join_*)."""
    if sys.platform == "win32" and ops.name == "python":
        pytest.skip("the Python reader on Windows has no change time: its cache never joins files")


# --------------------------------------------------------------------------- layouts


def test_table_layouts_are_40_and_64_bytes() -> None:
    assert C.CAND_DTYPE.itemsize == 40
    assert C.MEMBER_DTYPE.itemsize == 64
    assert ENTRY_DTYPE.itemsize == 128


def test_checkpoints() -> None:
    assert C.checkpoints(3 * MIB) == [MIB, 3 * MIB]
    assert C.checkpoints(MIB) == [MIB]
    assert C.checkpoints(600 * MIB) == [MIB, 4 * MIB, 16 * MIB, 64 * MIB, 256 * MIB, 512 * MIB, 600 * MIB]


# --------------------------------------------------------------------------- zero false positives


async def test_identical_files_form_one_group_and_read_exactly_the_design(tmp_path: Path, ops: Ops) -> None:
    root = tmp_path / "t"
    a = blob(3 * MIB + 5, 1)
    for name in ("x/a.bin", "y/a.bin", "z/a copy.bin"):
        write(root, name, a)
    write(root, "lone.bin", blob(5 * MIB, 2))  # a size of its own: never read
    job, res = await run(await scan(root), ops, tmp_path / "c")
    assert groups(res) == [{"a.bin", "a copy.bin"}]
    size = len(a)
    assert job.counters.bytes_probe == 3 * probe(size)
    assert job.counters.bytes_full == 3 * (size - C.BLOCK)
    assert job.sel is not None and job.sel.excluded.unique == 1
    g = res.valid_groups()[0]
    roles = sorted(res.role(r) for r in range(g.start, g.start + g.count))
    assert roles == [C.ROLE_KEEP, C.ROLE_EXTRA, C.ROLE_EXTRA]
    assert job.open_handles() == 0


@pytest.mark.parametrize(
    ("where", "split_at"),
    [
        (0, None),  # the first byte: S2 tells them apart
        (-1, None),  # the last byte: S2 too
        (MIB - 1, MIB),  # just before the first checkpoint
        (MIB, 4 * MIB),  # just after it
        (2 * MIB + 7, 4 * MIB),
    ],
)
async def test_same_size_different_content_is_never_a_group(
    tmp_path: Path, ops: Ops, where: int, split_at: int | None
) -> None:
    root = tmp_path / "t"
    size = 6 * MIB
    a = bytearray(blob(size, 3))
    b = bytearray(a)
    b[where] ^= 0x5A
    write(root, "a.bin", bytes(a))
    write(root, "b.bin", bytes(b))
    job, res = await run(await scan(root), ops, tmp_path / "c")
    assert groups(res) == []
    assert job.counters.bytes_probe == 2 * probe(size)
    expect = 0 if split_at is None else 2 * (split_at - C.BLOCK)
    assert job.counters.bytes_full == expect


async def test_equal_head_tail_and_middle_samples_but_different_elsewhere(tmp_path: Path, ops: Ops) -> None:
    root = tmp_path / "t"
    size = 70 * MIB  # ≥ 64 MiB: S2 also samples two middle blocks
    a = bytearray(blob(size, 4))
    b = bytearray(a)
    b[size // 2] ^= 1  # not in any sampled block
    write(root, "a.bin", bytes(a))
    write(root, "b.bin", bytes(b))
    job, res = await run(await scan(root), ops, tmp_path / "c")
    assert groups(res) == []
    assert job.counters.bytes_probe == 2 * probe(size) == 2 * 4 * C.BLOCK
    assert job.counters.bytes_full == 2 * (64 * MIB - C.BLOCK)  # split at the 64 MiB checkpoint


async def test_a_constant_quick_digest_changes_nothing(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Even if every quick digest collided, only the full content decides."""
    root = tmp_path / "t"
    a = blob(2 * MIB, 5)
    write(root, "a1.bin", a)
    write(root, "a2.bin", a)
    write(root, "b.bin", blob(2 * MIB, 6))
    ops = FallbackOps()
    import spaceknife.dupes.reads as R

    monkeypatch.setattr(R, "_quick", lambda size, parts: 42)
    _, res = await run(await scan(root), ops, tmp_path / "c")
    assert groups(res) == [{"a1.bin", "a2.bin"}]


async def test_files_changing_while_they_are_read_are_dropped(tmp_path: Path, ops: Ops) -> None:
    root = tmp_path / "t"
    a = blob(24 * MIB, 7)
    write(root, "a.bin", a)
    victim = write(root, "b.bin", a)
    write(root, "c.bin", a)
    done = []

    def between(job: Job, g: int) -> None:
        if g >= 0 and not done:
            done.append(1)
            with open(victim, "r+b") as f:  # rewritten in place, same size
                f.seek(10)
                f.write(b"changed")

    job, res = await run(await scan(root), ops, tmp_path / "c", between=between)
    assert done
    assert groups(res) == [{"a.bin", "c.bin"}]
    assert job.skipped_by_reason.get("changed") == 1


async def test_appended_and_truncated_files_are_dropped(tmp_path: Path, ops: Ops) -> None:
    root = tmp_path / "t"
    a = blob(24 * MIB, 8)
    grow = write(root, "grow.bin", a)
    shrink = write(root, "shrink.bin", a)
    write(root, "keep1.bin", a)
    write(root, "keep2.bin", a)
    done = []

    def between(job: Job, g: int) -> None:
        if g >= 0 and not done:
            done.append(1)
            with open(grow, "ab") as f:
                f.write(b"tail")
            os.truncate(shrink, len(a) - 1)

    _, res = await run(await scan(root), ops, tmp_path / "c", between=between)
    assert groups(res) == [{"keep1.bin", "keep2.bin"}]


async def test_a_file_swapped_between_s1_and_s2_is_not_read(tmp_path: Path, ops: Ops) -> None:
    root = tmp_path / "t"
    a = blob(2 * MIB, 9)
    write(root, "a.bin", a)
    b = write(root, "b.bin", a)
    write(root, "c.bin", a)

    def between(job: Job, g: int) -> None:
        if g == -1:  # after S1 of the wave
            # same name, same size, same content: another file (created first: a new file id)
            tmp = write(root, "new.tmp", a)
            os.replace(tmp, b)

    job, res = await run(await scan(root), ops, tmp_path / "c", between=between)
    assert groups(res) == [{"a.bin", "c.bin"}]
    assert job.skipped_by_reason.get("changed") == 1


# --------------------------------------------------------------------------- identity


async def test_hard_links_are_one_file_never_a_duplicate(tmp_path: Path, ops: Ops) -> None:
    root = tmp_path / "t"
    a = blob(2 * MIB, 10)
    p = write(root, "a.bin", a)
    os.link(p, root / "a-link.bin")
    job, res = await run(await scan(root, links_each=True), ops, tmp_path / "c")
    assert groups(res) == []
    shared = [g for g in res.valid_groups() if g.shared]
    assert len(shared) == 1 and shared[0].reclaim == 0
    assert read_bytes(job) == 0  # one physical file: nothing to compare
    # counted once by the scan (the default): the other name is not even a candidate
    job, res = await run(await scan(root), ops, tmp_path / "c2")
    assert groups(res) == [] and read_bytes(job) == 0


async def test_a_hard_linked_copy_is_kept_never_deleted(tmp_path: Path, ops: Ops) -> None:
    root = tmp_path / "t"
    a = blob(2 * MIB, 11)
    p = write(root, "linked.bin", a)
    os.link(p, tmp_path / "outside.bin")  # a second name outside the scan
    write(root, "copy.bin", a)
    _, res = await run(await scan(root), ops, tmp_path / "c")
    assert groups(res) == [{"linked.bin", "copy.bin"}]
    g = res.valid_groups()[0]
    roles = {os.path.basename(res.paths[r]): res.role(r) for r in range(g.start, g.start + g.count)}
    assert roles == {"linked.bin": C.ROLE_KEEP, "copy.bin": C.ROLE_EXTRA}


async def test_overlapping_scans_see_one_file(tmp_path: Path, ops: Ops) -> None:
    root = tmp_path / "t"
    a = blob(2 * MIB, 12)
    write(root, "sub/a.bin", a)
    write(root, "b.bin", a)
    scans = await scan(root, root / "sub")  # the parent and the child: a.bin twice
    res = Results(scans)
    job = Job(Scope(scans), options=JobOptions(cache_root=tmp_path / "c", racy_s=0), ops=ops, sink=res)
    await job.run()
    gs = [g for g in res.valid_groups() if not g.shared]
    assert len(gs) == 1
    g = gs[0]
    assert g.units == 2  # a.bin (two names, one file) and b.bin
    own = [r for r in range(g.start, g.start + g.count) if res.role(r) == C.ROLE_EXTRA and not res.is_view(r)]
    assert g.reclaim == res.m["alloc"][own].sum()
    # a.bin seen by two paths is one single-named file: it may go like any other copy, its second
    # path carries the same role and is counted once
    rows = range(g.start, g.start + g.count)
    views = [r for r in rows if res.is_view(r)]
    assert [res.paths[r] for r in views] == [str(root / "sub" / "a.bin")]
    assert res.role(views[0]) == res.role(int(res.m["unit"][views[0]]))
    extra = [res.paths[r] for r in rows if res.role(r) == C.ROLE_EXTRA and not res.is_view(r)]
    assert len(extra) == 1
    assert res.totals()[1] == 1  # one extra copy, not two


# --------------------------------------------------------------------------- what is not read


async def test_cloud_placeholders_and_small_and_empty_files_are_not_read(tmp_path: Path, ops: Ops) -> None:
    root = tmp_path / "t"
    a = blob(2 * MIB, 13)
    write(root, "a.bin", a)
    write(root, "cloud.bin", a)
    write(root, "small1.bin", b"x" * 1000)
    write(root, "small2.bin", b"x" * 1000)
    write(root, "empty1", b"")
    write(root, "empty2", b"")
    scans = await scan(root)
    store = scans[0].store
    node = store.find(str(root / "cloud.bin"))
    assert node is not None
    store.cols["flags"][node] |= np.uint32(Flag.CLOUD)
    job, res = await run(scans, ops, tmp_path / "c")
    assert groups(res) == []
    assert read_bytes(job) == 0
    assert job.sel is not None
    assert job.sel.excluded.cloud == 1 and job.sel.excluded.small == 2


async def test_cloud_attributes_seen_at_s1_stop_before_any_read(tmp_path: Path, ops: Ops) -> None:
    root = tmp_path / "t"
    a = blob(2 * MIB, 14)
    write(root, "a.bin", a)
    write(root, "b.bin", a)
    write(root, "c.bin", a)
    inner = ops

    class Recalling:
        alg, name, has_usn = inner.alg, inner.name, inner.has_usn

        def identity(self, paths: list[str], want_usn: bool = False) -> dict[str, np.ndarray]:
            out = inner.identity(paths, want_usn)
            for i, p in enumerate(paths):
                if p.endswith("b.bin"):
                    out["reason"][i] = C.CLOUD  # RECALL_ON_DATA_ACCESS, as Windows reports it
            return out

        def extents(self, paths: list[str]) -> Any:
            return inner.extents(paths)

        def reader(self, **kw: Any) -> Any:
            return inner.reader(**kw)

    job, res = await run(await scan(root), Recalling(), tmp_path / "c")  # type: ignore[arg-type]
    assert groups(res) == [{"a.bin", "c.bin"}]
    assert job.skipped_by_reason == {"cloud": 1}
    assert job.counters.bytes_probe == 2 * probe(len(a))


@pytest.mark.skipif(sys.platform == "win32" or os.geteuid() == 0, reason="needs POSIX permissions, not root")
async def test_unreadable_files_are_skipped_with_a_reason(tmp_path: Path, ops: Ops) -> None:
    root = tmp_path / "t"
    a = blob(2 * MIB, 15)
    write(root, "a.bin", a)
    locked = write(root, "b.bin", a)
    locked.chmod(0)
    try:
        job, res = await run(await scan(root), ops, tmp_path / "c")
    finally:
        locked.chmod(stat.S_IRUSR | stat.S_IWUSR)
    assert groups(res) == []
    assert job.skipped_by_reason == {"denied": 1}


async def test_sparse_and_dense_copies_have_the_same_content(tmp_path: Path, ops: Ops) -> None:
    root = tmp_path / "t"
    size = 8 * MIB
    tail = blob(4096, 16)
    sparse = root / "sparse.bin"
    root.mkdir(parents=True)
    with open(sparse, "wb") as f:  # noqa: ASYNC230
        f.truncate(size - len(tail))
        f.seek(size - len(tail))
        f.write(tail)
    write(root, "dense.bin", bytes(size - len(tail)) + tail)
    _, res = await run(await scan(root), ops, tmp_path / "c")
    assert groups(res) == [{"sparse.bin", "dense.bin"}]


# --------------------------------------------------------------------------- the cache


async def test_a_second_run_reads_nothing_and_a_changed_file_only_it(tmp_path: Path, ops: Ops) -> None:
    joins_from_cache(ops)
    root = tmp_path / "t"
    a = blob(6 * MIB, 17)
    b = bytearray(a)
    b[2 * MIB] ^= 1  # splits at 4 MiB
    write(root, "a1.bin", a)
    p2 = write(root, "a2.bin", a)
    write(root, "b.bin", bytes(b))
    write(root, "c.bin", blob(6 * MIB, 18))  # split by S2
    cache = tmp_path / "c"
    scans = await scan(root)
    _, res1 = await run(scans, ops, cache)
    assert groups(res1) == [{"a1.bin", "a2.bin"}]
    job2, res2 = await run(scans, ops, cache)
    assert groups(res2) == [{"a1.bin", "a2.bin"}]
    assert read_bytes(job2) == 0
    assert job2.reader_counters()["data_opens"] == 0
    assert job2.counters.meta_ops == 4  # S1 only
    # rewrite a2.bin in place with other bytes in the middle (same size): only it is read
    with open(p2, "r+b") as f:  # noqa: ASYNC230
        f.seek(3 * MIB)
        f.write(b"\xff" * 16)
    job3, res3 = await run(await scan(root), ops, cache)
    assert groups(res3) == []
    # a2 is read, and so is b (it might equal the new a2): both stop at 4 MiB, where the
    # cached prefix of a1 and their own digests tell all three apart; a1 and c are not opened
    assert job3.counters.bytes_probe == 2 * probe(len(a))
    assert job3.counters.bytes_full == 2 * (4 * MIB - C.BLOCK)


async def test_a_renamed_file_is_read_once_more(tmp_path: Path, ops: Ops) -> None:
    joins_from_cache(ops)
    root = tmp_path / "t"
    a = blob(2 * MIB, 19)
    write(root, "a1.bin", a)
    write(root, "a2.bin", a)
    cache = tmp_path / "c"
    await run(await scan(root), ops, cache)
    os.rename(root / "a2.bin", root / "renamed.bin")  # the change time moves
    job, res = await run(await scan(root), ops, cache)
    assert groups(res) == [{"a1.bin", "renamed.bin"}]
    assert job.counters.bytes_probe == probe(len(a))
    assert job.counters.bytes_full == len(a) - C.BLOCK


async def test_a_forged_cache_entry_cannot_join_files(tmp_path: Path, ops: Ops) -> None:
    root = tmp_path / "t"
    a = blob(2 * MIB, 20)
    b = bytearray(a)
    b[MIB + 5] ^= 1
    write(root, "a.bin", a)
    write(root, "b.bin", bytes(b))
    cache = tmp_path / "c"
    scans = await scan(root)
    _ = await run(scans, ops, cache)
    # give both entries the same full digest, but with b's stamps off by a nanosecond
    files = list(cache.glob("*.skh"))
    assert len(files) == 1
    raw = bytearray(files[0].read_bytes())
    data = np.frombuffer(bytes(raw[64:]), dtype=ENTRY_DTYPE).copy()
    data["full"] = data["full"][0]
    data["flags"] |= 1
    data["mtime"][1] += 1
    files[0].write_bytes(bytes(raw[:64]) + data.tobytes())
    _, res = await run(scans, ops, cache)
    assert groups(res) == []
    # stamps right but marked racy: still cannot join
    data["mtime"][1] -= 1
    data["flags"] |= 2
    files[0].write_bytes(bytes(raw[:64]) + data.tobytes())
    _, res = await run(scans, ops, cache)
    assert groups(res) == []


async def test_weak_volumes_never_join_from_the_cache(
    tmp_path: Path, ops: Ops, monkeypatch: pytest.MonkeyPatch
) -> None:
    import spaceknife.dupes.engine as E

    monkeypatch.setattr(E, "trust_of", lambda info, native: "weak")  # as on FAT / exFAT
    root = tmp_path / "t"
    a = blob(2 * MIB, 21)
    write(root, "a1.bin", a)
    write(root, "a2.bin", a)
    cache = tmp_path / "c"
    scans = await scan(root)
    await run(scans, ops, cache)
    job, res = await run(scans, ops, cache)
    assert groups(res) == [{"a1.bin", "a2.bin"}]
    assert job.counters.bytes_full == 2 * (len(a) - C.BLOCK)  # read again


async def test_another_algorithm_never_shares_entries(tmp_path: Path) -> None:
    if not native.has("DupesReader"):
        pytest.skip("needs the native module")
    root = tmp_path / "t"
    a = blob(2 * MIB, 22)
    write(root, "a1.bin", a)
    write(root, "a2.bin", a)
    cache = tmp_path / "c"
    scans = await scan(root)
    await run(scans, NativeOps(), cache)
    job, _ = await run(scans, FallbackOps(), cache)
    assert job.counters.bytes_full == 2 * (len(a) - C.BLOCK)


async def test_a_broken_cache_file_is_rebuilt(tmp_path: Path, ops: Ops) -> None:
    joins_from_cache(ops)
    root = tmp_path / "t"
    a = blob(2 * MIB, 23)
    write(root, "a1.bin", a)
    write(root, "a2.bin", a)
    cache = tmp_path / "c"
    scans = await scan(root)
    await run(scans, ops, cache)
    f = next(cache.glob("*.skh"))
    f.write_bytes(f.read_bytes()[:100])  # torn
    job, res = await run(scans, ops, cache)
    assert groups(res) == [{"a1.bin", "a2.bin"}]
    job, _ = await run(scans, ops, cache)
    assert read_bytes(job) == 0


# --------------------------------------------------------------------------- control


async def test_cancel_stops_within_a_batch_and_keeps_what_was_learnt(tmp_path: Path, ops: Ops) -> None:
    root = tmp_path / "t"
    a = blob(40 * MIB, 24)
    write(root, "a1.bin", a)
    write(root, "a2.bin", a)
    small = blob(2 * MIB, 25)
    write(root, "s1.bin", small)
    write(root, "s2.bin", small)
    scans = await scan(root)
    res = Results(scans)
    job = Job(Scope(scans), options=JobOptions(cache_root=tmp_path / "c", racy_s=0), ops=ops, sink=res)
    seen: list[int] = []

    def between(j: Job, g: int) -> None:
        if g >= 0 and j.counters.bytes_full > 8 * MIB and not seen:
            seen.append(j.counters.bytes_full)
            asyncio.get_running_loop().create_task(j.cancel())

    job.opts.between = between
    await job.run()
    assert job.state == "cancelled"
    assert job.open_handles() == 0
    assert job.counters.bytes_full < 40 * MIB
    assert all(q.reader.members() == 0 for q in job.queues.values())


async def test_pause_closes_every_handle_and_resume_finishes(tmp_path: Path, ops: Ops) -> None:
    root = tmp_path / "t"
    a = blob(30 * MIB, 26)
    write(root, "a1.bin", a)
    write(root, "a2.bin", a)
    scans = await scan(root)
    res = Results(scans)
    job = Job(Scope(scans), options=JobOptions(cache_root=tmp_path / "c", racy_s=0), ops=ops, sink=res)
    paused: list[int] = []

    def between(j: Job, g: int) -> None:
        if g >= 0 and not paused:
            paused.append(1)

            async def pause_resume() -> None:
                await j.pause()
                paused.append(j.open_handles())
                await asyncio.sleep(0.05)
                j.resume()

            asyncio.get_running_loop().create_task(pause_resume())

    job.opts.between = between
    await job.run()
    assert job.state == "done"
    assert paused[1] == 0
    assert groups(res) == [{"a1.bin", "a2.bin"}]
    assert job.counters.bytes_full == 2 * (len(a) - C.BLOCK)  # nothing read twice
    assert job.counters.reopens >= 2


async def test_budget_stops_the_job(tmp_path: Path, ops: Ops) -> None:
    root = tmp_path / "t"
    for i in range(4):
        a = blob(8 * MIB, 30 + i)
        write(root, f"a{i}.bin", a)
        write(root, f"b{i}.bin", a)
    job, res = await run(await scan(root), ops, tmp_path / "c", budget_bytes=10 * MIB)
    assert job.state == "done"
    assert read_bytes(job) < 10 * MIB + 33 * MIB  # stops within a batch per member
    assert len(groups(res)) < 4


class FakeGovernor:
    def __init__(self) -> None:
        self.level = "balanced"
        self.permits = 0
        self.paused_calls = 0
        self.levels: list[str] = []

    class _P:
        def __init__(self, g: FakeGovernor) -> None:
            self.g = g

        def bind(self, fut: Any) -> None:
            pass

    async def permit(self) -> Any:
        self.permits += 1
        return FakeGovernor._P(self)

    def set_level(self, level: str, custom: float | None = None) -> None:
        self.level = level
        self.levels.append(level)

    def pause(self, owner: object = None) -> None:
        self.paused_calls += 1

    @property
    def core(self) -> Any:
        return self


async def test_every_io_call_holds_a_permit_and_the_level_comes_back(tmp_path: Path, ops: Ops) -> None:
    root = tmp_path / "t"
    a = blob(9 * MIB, 40)
    for i in range(3):
        write(root, f"a{i}.bin", a)
    write(root, "b.bin", blob(9 * MIB, 41))
    scans = await scan(root)
    gov = FakeGovernor()
    busy = {"on": False}
    res = Results(scans)
    job = Job(
        Scope(scans),
        options=JobOptions(cache_root=tmp_path / "c", racy_s=0, load="quiet"),
        ops=ops,
        sink=res,
        scan_busy=lambda disk: busy["on"],
    )

    real_queue_for = job._queue_for

    async def with_gov(si: int, ri: int) -> Any:
        q = await real_queue_for(si, ri)
        if q.governor is None:
            q.governor = gov  # type: ignore[assignment]
            job._apply_level(q)
        return q

    job._queue_for = with_gov  # type: ignore[method-assign]
    await job.run()
    assert gov.permits == job.counters.io_calls > 0
    assert gov.paused_calls == 0
    assert gov.levels[0] == "quiet" and gov.level == "balanced"


async def test_a_scan_on_the_disk_makes_the_job_wait(tmp_path: Path, ops: Ops) -> None:
    root = tmp_path / "t"
    a = blob(2 * MIB, 42)
    write(root, "a1.bin", a)
    write(root, "a2.bin", a)
    scans = await scan(root)
    busy = {"on": True}
    res = Results(scans)
    job = Job(
        Scope(scans),
        options=JobOptions(cache_root=tmp_path / "c", racy_s=0),
        ops=ops,
        sink=res,
        scan_busy=lambda disk: busy["on"],
    )
    task = asyncio.create_task(job.run())
    await asyncio.sleep(0.3)
    assert job.state == "waiting_scan"
    assert read_bytes(job) == 0
    busy["on"] = False
    await asyncio.wait_for(task, 10)
    assert groups(res) == [{"a1.bin", "a2.bin"}]


# --------------------------------------------------------------------------- cross-volume


def two_volumes(inner: Ops, second: Path) -> Ops:
    """Ops whose S1 reports a different device for files below ``second`` (a stand-in volume)."""

    class TwoVolumes:
        alg, name, has_usn = inner.alg, inner.name, inner.has_usn

        def identity(self, paths: list[str], want_usn: bool = False) -> dict[str, np.ndarray]:
            out = inner.identity(paths, want_usn)
            for i, p in enumerate(paths):
                if p.startswith(str(second)):
                    out["dev"][i] ^= 0xABCDEF
            return out

        def extents(self, paths: list[str]) -> Any:
            return inner.extents(paths)

        def reader(self, **kw: Any) -> Any:
            r = inner.reader(**kw)

            class Fixed:
                def __getattr__(self, name: str) -> Any:
                    return getattr(r, name)

                def probe(self, keys: Any, paths: list[str], dev: np.ndarray, *rest: Any) -> Any:
                    dev = dev.copy()
                    for i, p in enumerate(paths):
                        if p.startswith(str(second)):
                            dev[i] ^= 0xABCDEF
                    return r.probe(keys, paths, dev, *rest)

            return Fixed()

    return TwoVolumes()  # type: ignore[return-value]


async def test_cross_volume_is_off_per_scan_and_on_keeps_one_per_volume(tmp_path: Path, ops: Ops) -> None:
    d1, d2 = tmp_path / "one", tmp_path / "two"
    a = blob(2 * MIB, 50)
    write(d1, "a.bin", a)
    write(d1, "a-again.bin", a)
    write(d2, "a-backup.bin", a)
    scans = await scan(d1, d2)
    vops = two_volumes(ops, d2)
    # off: each scan by itself (the server runs one job per scan)
    _, res = await run(scans[:1], vops, tmp_path / "c")
    assert groups(res) == [{"a.bin", "a-again.bin"}]
    # on: both scans; the copy on the other volume is kept (one per volume)
    job, res = await run(scans, vops, tmp_path / "c")
    assert groups(res) == [{"a.bin", "a-again.bin", "a-backup.bin"}]
    assert len(job.vols) == 2
    g = res.valid_groups()[0]
    roles = {os.path.basename(res.paths[r]): res.role(r) for r in range(g.start, g.start + g.count)}
    assert roles["a-backup.bin"] == C.ROLE_KEEP
    assert sorted(roles.values()).count(C.ROLE_EXTRA) == 1
    led = res.ledger()
    assert sum(v.extra for v in led.values()) == 1
    # one per volume off: only one copy stays
    _, res = await run(scans, vops, tmp_path / "c", keep=KeepOptions(per_volume=False))
    g = res.valid_groups()[0]
    assert sorted(res.role(r) for r in range(g.start, g.start + g.count)).count(C.ROLE_EXTRA) == 2


# --------------------------------------------------------------------------- keep rules


async def test_folder_consistent_keeps_the_same_side(tmp_path: Path, ops: Ops) -> None:
    root = tmp_path / "t"
    for i in range(5):
        a = blob(MIB + i, 60 + i)
        write(root, f"photos/{i}.jpg", a)
        write(root, f"backup/{i}.jpg", a)
    # make one backup file the oldest by birth/mtime: the folder rule still keeps one side
    os.utime(root / "backup" / "3.jpg", (1_000_000_000, 1_000_000_000))
    _, res = await run(await scan(root), ops, tmp_path / "c", min_size=MIB)
    kept = set()
    for g in res.valid_groups():
        for r in range(g.start, g.start + g.count):
            if res.role(r) == C.ROLE_KEEP:
                kept.add(os.path.basename(os.path.dirname(res.paths[r])))
    assert len(kept) == 1
    res.set_keep(KeepOptions(folder_consistent=False, rule="shortest", prefer=[str(root / "photos")]))
    for g in res.valid_groups():
        keepers = [res.paths[r] for r in range(g.start, g.start + g.count) if res.role(r) == C.ROLE_KEEP]
        assert len(keepers) == 1 and "photos" in keepers[0]


async def test_manual_choices_survive_and_one_copy_always_stays(tmp_path: Path, ops: Ops) -> None:
    root = tmp_path / "t"
    a = blob(2 * MIB, 70)
    for n in ("a.bin", "b.bin", "c.bin"):
        write(root, n, a)
    _, res = await run(await scan(root), ops, tmp_path / "c")
    g = res.valid_groups()[0]
    assert g.digest is not None
    paths = [res.paths[r] for r in range(g.start, g.start + g.count)]
    over: dict[tuple[bytes, str], Any] = {(g.digest, p): "extra" for p in paths}
    res.set_keep(KeepOptions(overrides=over))
    roles = [res.role(r) for r in range(g.start, g.start + g.count)]
    assert roles.count(C.ROLE_KEEP) == 1
    over = {(g.digest, paths[2]): "keep"}
    res.set_keep(KeepOptions(overrides=over))
    assert res.role(g.start + 2) == C.ROLE_KEEP


# --------------------------------------------------------------------------- S0 and the plan


async def test_the_plan_does_no_io(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    root = tmp_path / "t"
    a = blob(2 * MIB, 80)
    write(root, "a.bin", a)
    write(root, "b.bin", a)
    write(root, "c.bin", blob(3 * MIB, 81))
    scans = await scan(root)

    def boom(*a: Any, **k: Any) -> Any:
        raise AssertionError("IO during S0")

    monkeypatch.setattr(os, "open", boom)
    monkeypatch.setattr(os, "stat", boom)
    sel = select(Scope(scans))
    p = plan(sel, n_scans=1)
    assert p["candidates"] == 2 and p["groups"] == 1
    assert p["probe_bytes"] == 2 * probe(2 * MIB)


async def test_modes_narrow_the_groups(tmp_path: Path) -> None:
    root = tmp_path / "t"
    a = blob(2 * MIB, 82)
    b = blob(3 * MIB, 83)
    write(root, "in/a.bin", a)
    write(root, "out/a.bin", a)
    write(root, "out/b1.bin", b)
    write(root, "out/b2.bin", b)
    scans = await scan(root)
    store = scans[0].store
    folder = store.find(str(root / "in"))
    assert folder is not None
    assert len(select(Scope(scans)).groups) == 2
    assert len(select(Scope(scans, mode="against", node=folder)).groups) == 1
    assert len(select(Scope(scans, mode="inside", node=folder)).groups) == 0
    f = store.find(str(root / "out" / "b1.bin"))
    assert f is not None
    sel = select(Scope(scans, mode="file", node=f))
    assert len(sel.groups) == 1 and int(sel.groups["size"][0]) == len(b)


async def test_coverage_of_a_folder(tmp_path: Path, ops: Ops) -> None:
    root = tmp_path / "t"
    a = blob(2 * MIB, 84)
    write(root, "f/a.bin", a)  # also elsewhere
    write(root, "g/a.bin", a)
    write(root, "f/unique.bin", blob(5 * MIB, 85))  # a size of its own: only here, not read
    write(root, "f/tiny.txt", b"x" * 100)
    scans = await scan(root)
    store = scans[0].store
    folder = store.find(str(root / "f"))
    assert folder is not None
    res = Results(scans)
    job = Job(
        Scope(scans, mode="against", node=folder),
        options=JobOptions(cache_root=tmp_path / "c", racy_s=0),
        ops=ops,
        sink=res,
    )
    await job.run()
    cov = res.coverage(0, folder)
    assert cov.elsewhere == 1 and cov.only_here == 1 and cov.small == 1
    assert cov.only_here_list[0][1].endswith("unique.bin")
    # where the other copy is: g, with the bytes of f's file that has it
    g_node = store.find(str(root / "g"))
    assert cov.elsewhere_list == [
        (0, str(root / "g"), int(store.cols["alloc"][store.find(str(root / "f" / "a.bin"))]), g_node)
    ]


# --------------------------------------------------------------------------- frames


async def test_frame_columns(tmp_path: Path, ops: Ops) -> None:
    from spaceknife.view.state import View

    root = tmp_path / "t"
    a = blob(2 * MIB, 90)
    write(root, "x/a1.bin", a)
    write(root, "x/a2.bin", a)
    write(root, "y/other.bin", blob(MIB // 2, 91))
    scans = await scan(root)
    _, res = await run(scans, ops, tmp_path / "c")
    sc = scans[0]
    view = View("v", sc.store, sc.roots)
    frame = view.render()
    dup, dupg = res.annotate(0, sc.store, frame.draw)
    names = {frame.draw.names[i]: (int(dup[i]), int(dupg[i])) for i in range(len(dup)) if frame.draw.names[i]}
    assert {names["a1.bin"][0], names["a2.bin"][0]} == {C.DUP_KEEP, C.DUP_EXTRA}
    assert names["a1.bin"][1] == names["a2.bin"][1] == 1
    assert names["other.bin"] == (0, 0)
    assert names["x"][0] == 3  # half its bytes are an extra copy: the ≥40 % tier


async def test_fallback_and_native_agree(tmp_path: Path) -> None:
    if not native.has("DupesReader"):
        pytest.skip("needs the native module")
    root = tmp_path / "t"
    a = blob(3 * MIB, 100)
    b = bytearray(a)
    b[-10] ^= 1
    for n in ("a1", "a2", "a3"):
        write(root, f"{n}.bin", a)
    write(root, "b.bin", bytes(b))
    write(root, "c1.bin", blob(MIB, 101))
    write(root, "c2.bin", blob(MIB, 101))
    scans = await scan(root)
    _, r1 = await run(scans, NativeOps(), tmp_path / "c1")
    _, r2 = await run(scans, FallbackOps(), tmp_path / "c2")
    assert groups(r1) == groups(r2) == [{"a1.bin", "a2.bin", "a3.bin"}, {"c1.bin", "c2.bin"}]


def test_select_allocates_nothing_as_long_as_the_store(tmp_path: Path) -> None:
    import tracemalloc

    from spaceknife.scan.scanner import RootInfo
    from spaceknife.store.nodes import Flag as F
    from spaceknife.store.nodes import Kind, NodeStore

    n = 1_000_000
    total = n + 2
    names = [b"", str(tmp_path).encode()] + [f"f{i}".encode() for i in range(n)]
    lens = np.array([len(x) for x in names], dtype=np.uint16)
    off = np.r_[0, np.cumsum(lens[:-1], dtype=np.int64)].astype(np.uint32)
    parent = np.r_[-1, 0, np.ones(n)].astype(np.int32)
    kind = np.r_[Kind.DIR, Kind.DIR, np.full(n, Kind.FILE)].astype(np.uint8)
    size = np.r_[0, 0, np.full(n, 100)].astype(np.int64)
    size[2:12] = 2 * MIB
    cols = {
        "parent": parent,
        "depth": np.r_[0, 1, np.full(n, 2)].astype(np.uint16),
        "kind": kind,
        "flags": np.r_[np.uint32(F.VIRTUAL), np.zeros(n + 1)].astype(np.uint32),
        "name_off": off,
        "name_len": lens,
        "size": size,
        "alloc": size,
        "child_start": np.r_[1, 2, np.zeros(n)].astype(np.int32),
        "child_count": np.r_[1, n, np.zeros(n)].astype(np.int32),
    }
    store = NodeStore.from_columns(cols, b"".join(names), [""])
    assert len(store) == total
    sc = ScanInput("s", store, [RootInfo(str(tmp_path), 1, None, False)])
    from spaceknife.view.trash import find_bins

    find_bins(store)  # the views' recycle-bin search, cached per store version: not S0's cost
    tracemalloc.start()
    sel = select(Scope([sc]))
    _, peak = tracemalloc.get_traced_memory()
    tracemalloc.stop()
    assert len(sel) == 10 and len(sel.groups) == 1
    assert peak < total * 8  # below one i8 column of the store: slices, not whole-store temporaries


# --------------------------------------------------------------------------- access times


@pytest.mark.skipif(
    sys.platform == "darwin", reason="macOS: the reader asks for no access-time updates per thread"
)
async def test_reading_contents_leaves_access_times_alone(tmp_path: Path, ops: Ops) -> None:
    """A month-old access time stays after the whole file was read (O_NOATIME as the owner on
    Linux; the handle's -1 last-access time on Windows), where the mount would otherwise move it."""
    root = tmp_path / "t"
    a = blob(2 * MIB + 3, 21)
    paths = [write(root, f"{d}/a.bin", a) for d in ("x", "y")]
    month = 30 * 86400 * 10**9
    for p in paths:
        st = p.stat()
        os.utime(p, ns=(st.st_mtime_ns - month, st.st_mtime_ns))
    before = [p.stat().st_atime_ns for p in paths]
    job, res = await run(await scan(root), ops, tmp_path / "c")
    assert groups(res) == [{"a.bin"}]
    assert job.counters.bytes_full > 0  # the contents were read
    assert [p.stat().st_atime_ns for p in paths] == before
    assert job.counters.atime_unkept == 0


# --------------------------------------------------------------------------- review fixes


async def test_more_than_256_verified_members(tmp_path: Path, ops: Ops) -> None:
    """The member table's ``unit`` column holds any row (it was one byte: the 257th member broke
    the search)."""
    root = tmp_path / "t"
    for i in range(140):
        a = blob(8192 + i, 300 + i)
        write(root, f"a/{i}.bin", a)
        write(root, f"b/{i}.bin", a)
    job, res = await run(await scan(root), ops, tmp_path / "c", min_size=4096)
    assert job.state == "done", job.error
    assert res.n == 280 and len(groups(res)) == 140
    assert res.totals()[:2] == (140, 140)


async def test_one_group_of_more_than_255_copies(tmp_path: Path, ops: Ops) -> None:
    root = tmp_path / "t"
    a = blob(9000, 7)
    for i in range(300):
        write(root, f"c/{i:03}.bin", a)
    job, res = await run(await scan(root), ops, tmp_path / "c", min_size=4096)
    assert job.state == "done", job.error
    (g,) = res.valid_groups()
    assert g.count == 300 and g.units == 300
    assert sum(1 for r in range(g.start, g.start + g.count) if res.role(r) == C.ROLE_EXTRA) == 299


def _no_handles(job: Job) -> None:
    assert job.open_handles() == 0
    assert all(q.reader.members() == 0 for q in job.queues.values())


async def test_no_handle_outlives_a_budget_stop_or_a_failure(tmp_path: Path, ops: Ops) -> None:
    root = tmp_path / "t"
    for i in range(6):
        a = blob(12 * MIB, 400 + i)
        write(root, f"a{i}.bin", a)
        write(root, f"b{i}.bin", a)
    scans = await scan(root)
    job, _ = await run(scans, ops, tmp_path / "c", budget_bytes=6 * MIB)
    _no_handles(job)
    assert job.skipped_by_reason.get("stopped", 0) > 0
    # a failure halfway (here: in the test seam between two S3 batches)
    res = Results(scans)
    job = Job(Scope(scans), options=JobOptions(cache_root=tmp_path / "c2", racy_s=0), ops=ops, sink=res)

    def boom(j: Job, g: int) -> None:
        if g >= 0 and j.counters.bytes_full > 2 * MIB:
            raise RuntimeError("boom")

    job.opts.between = boom
    await job.run()
    assert job.state == "failed" and "boom" in job.error
    _no_handles(job)
    assert job.skipped_by_reason.get("stopped", 0) > 0  # what it did not decide is counted


class SlowGovernor(FakeGovernor):
    """Grants permits late, like a governor throttling a busy disk."""

    def __init__(self, delay: float) -> None:
        super().__init__()
        self.delay = delay

    async def permit(self) -> Any:
        await asyncio.sleep(self.delay)
        return await super().permit()


async def test_a_pause_while_permits_are_slow_stops_every_read(tmp_path: Path, ops: Ops) -> None:
    root = tmp_path / "t"
    for i in range(3):
        a = blob(24 * MIB, 500 + i)
        write(root, f"a{i}.bin", a)
        write(root, f"b{i}.bin", a)
    scans = await scan(root)
    gov = SlowGovernor(0.05)
    res = Results(scans)
    job = Job(Scope(scans), options=JobOptions(cache_root=tmp_path / "c", racy_s=0), ops=ops, sink=res)
    real_queue_for = job._queue_for

    async def with_gov(si: int, ri: int) -> Any:
        q = await real_queue_for(si, ri)
        q.governor = gov  # type: ignore[assignment]
        return q

    job._queue_for = with_gov  # type: ignore[method-assign]
    seen: dict[str, int] = {}

    def between(j: Job, g: int) -> None:
        if g >= 0 and not seen:
            seen["at"] = 1

            async def pause_then_watch() -> None:
                await j.pause()
                seen["calls"] = j.counters.io_calls
                seen["read"] = read_bytes(j)
                seen["open"] = j.open_handles()
                await asyncio.sleep(0.4)  # several permits would have been granted meanwhile
                seen["calls_later"] = j.counters.io_calls
                seen["read_later"] = read_bytes(j)
                seen["open_later"] = j.open_handles()
                j.resume()

            asyncio.get_running_loop().create_task(pause_then_watch())

    job.opts.between = between
    await job.run()
    assert job.state == "done"
    assert seen["open"] == 0 and seen["open_later"] == 0
    assert seen["calls_later"] == seen["calls"] and seen["read_later"] == seen["read"]
    assert groups(res) == [{"a0.bin", "b0.bin"}, {"a1.bin", "b1.bin"}, {"a2.bin", "b2.bin"}]


async def test_quiet_keeps_s3_to_its_share_of_the_disk_time() -> None:
    """The duty cycle: S3 busy time drains at the level's share; a batch waits while in debt."""
    from spaceknife.dupes.engine import DUTY_SLACK_S, DutyCycle

    d = DutyCycle()
    share = 0.25
    t0 = asyncio.get_running_loop().time()
    busy = 0.0
    while asyncio.get_running_loop().time() - t0 < 0.8:
        await d.wait(lambda: share, lambda: False)
        d.begin(share)
        await asyncio.sleep(0.02)  # a batch that keeps the disk busy
        d.end(share)
        busy += 0.02
    wall = asyncio.get_running_loop().time() - t0
    assert busy <= share * wall + DUTY_SLACK_S + 0.05
    assert d.waited_s > 0
    # at 「最大」 nothing waits
    m = DutyCycle()
    m.begin(1.0)
    m.end(1.0)
    await asyncio.wait_for(m.wait(lambda: 1.0, lambda: False), 0.05)


def test_prefix_separation_is_linear() -> None:
    """3,000+ copies of one size told apart by cached prefixes: counted, not compared pairwise."""
    import time as _t

    from spaceknife.dupes.engine import Unit

    n = 6000
    job = Job(Scope([]), options=JobOptions())
    dropped: list[int] = []
    job._unique = lambda rows, info, u, size: dropped.append(u.rep)  # type: ignore[method-assign]
    part = [Unit(rep=i, names=[i], prefix=(512 << 10, i.to_bytes(16, "little"))) for i in range(n)]
    part.append(Unit(rep=n, names=[n], prefix=(512 << 10, (5).to_bytes(16, "little"))))  # a twin of 5
    part.append(Unit(rep=n + 1, names=[n + 1]))  # no prefix: nothing can be said of the others
    t = _t.perf_counter()
    kept = job._separate_by_prefix([], [], part, 1 << 30)
    assert _t.perf_counter() - t < 2.0
    assert not dropped  # the unit without a prefix keeps every other one in
    kept = job._separate_by_prefix([], [], part[:-1], 1 << 30)
    assert sorted(u.rep for u in kept) == [5, n]
    assert len(dropped) == n - 1


async def test_a_network_mount_below_a_local_root_is_not_read(
    tmp_path: Path, ops: Ops, monkeypatch: pytest.MonkeyPatch
) -> None:
    import spaceknife.dupes.engine as E
    from spaceknife.scan.volume import VolumeInfo

    local, net = tmp_path / "t", tmp_path / "t" / "mnt"
    a = blob(2 * MIB, 600)
    write(local, "a.bin", a)
    write(net, "a-copy.bin", a)
    write(net, "a-copy2.bin", a)
    scans = await scan(local)
    vops = two_volumes(ops, net)
    real = E.volume_for

    def fake_volume_for(path: str) -> VolumeInfo:
        if path.startswith(str(net)):
            return VolumeInfo(str(net), "server:/export", "nfs4", "", 0, 0, 4096, True, False)
        return real(path)

    monkeypatch.setattr(E, "volume_for", fake_volume_for)
    job, res = await run(scans, vops, tmp_path / "c")
    assert groups(res) == []
    assert job.skipped_by_reason.get("network_mount") == 2
    assert read_bytes(job) == 0  # the lone local file has no partner left: nothing is read


def test_another_serial_on_windows_asks_for_its_own_volume(monkeypatch: pytest.MonkeyPatch) -> None:
    """A FAT or exFAT volume mounted in a folder of an NTFS root is weak, not the root's trust."""
    import types

    import spaceknife.dupes.engine as E
    from spaceknife.scan.volume import VolumeInfo

    root = VolumeInfo("C:\\", "\\\\?\\Volume{1}", "NTFS", "", 0, 0, 4096, False, False, serial="1234ABCD")
    stick = VolumeInfo("C:\\mnt\\stick\\", "\\\\?\\Volume{2}", "exFAT", "", 0, 0, 4096, False, True)
    monkeypatch.setattr(E, "sys", types.SimpleNamespace(platform="win32"))
    monkeypatch.setattr(E, "volume_for", lambda path: stick)
    monkeypatch.setattr(E, "volume_key", lambda mount, dev: mount.encode() + dev.to_bytes(8, "little"))
    job = Job(Scope([]), options=JobOptions(use_cache=False), ops=FallbackOps())
    same = job._volume(0x55AA_0000_1234ABCD, "C:\\x\\a.bin", root)
    other = job._volume(0x77, "C:\\mnt\\stick\\a.bin", root)
    from spaceknife.dupes.identity import trust_of

    assert job.vols[same].fs_type == "NTFS" and job.vols[same].trust == trust_of(root, native=False)
    assert job.vols[other].fs_type == "exFAT" and job.vols[other].trust == "weak"


def test_btrfs_subvolumes_are_one_file_system_for_block_maps(monkeypatch: pytest.MonkeyPatch) -> None:
    import spaceknife.dupes.identity as I
    from spaceknife.scan.volume import VolumeInfo

    monkeypatch.setattr(I, "_linux_uuid", lambda mount: b"")
    a = VolumeInfo("/", "/dev/sda2", "btrfs", "", 0, 0, 4096, False, False, dev=30)
    b = VolumeInfo("/home", "/dev/sda2", "btrfs", "", 0, 0, 4096, False, False, dev=31)
    c = VolumeInfo("/data", "/dev/sdb1", "btrfs", "", 0, 0, 4096, False, False, dev=40)
    assert I.fs_group(a, "/", 30) == I.fs_group(b, "/home", 31) != I.fs_group(c, "/data", 40)
    x = VolumeInfo("/x", "/dev/sdc1", "xfs", "", 0, 0, 4096, False, False, dev=50)
    assert I.fs_group(x, "/x", 50) != I.fs_group(x, "/x", 51)


async def test_a_stopped_search_says_what_it_did_not_decide(tmp_path: Path, ops: Ops) -> None:
    root = tmp_path / "t"
    for i in range(5):
        a = blob(16 * MIB, 700 + i)
        write(root, f"a{i}.bin", a)
        write(root, f"b{i}.bin", a)
    job, _ = await run(await scan(root), ops, tmp_path / "c", budget_bytes=4 * MIB)
    assert job.sel is not None
    undecided = int(job.skipped_by_reason.get("stopped", 0))
    decided = int(np.isin(job.sel.cand["state"], (C.ST_CONFIRMED, C.ST_UNIQUE)).sum())
    assert undecided > 0 and undecided + decided == len(job.sel.cand)
    stopped = job.sel.cand["reason"][job.sel.cand["state"] == C.ST_SKIPPED]
    assert (stopped == C.STOPPED).sum() == undecided


async def test_manual_keeps_pile_up_and_survive_a_rule_change(tmp_path: Path, ops: Ops) -> None:
    root = tmp_path / "t"
    for i in range(3):
        a = blob(MIB + i, 800 + i)
        write(root, f"x/{i}.bin", a)
        write(root, f"y{i}/{i}.bin", a)
    _, res = await run(await scan(root), ops, tmp_path / "c")
    extras = {}
    for g in res.valid_groups():
        rows = range(g.start, g.start + g.count)
        extras[g.id] = next(res.paths[r] for r in rows if res.role(r) == C.ROLE_EXTRA)
    over = {}
    for g in res.valid_groups():
        over[(g.digest or b"", extras[g.id])] = "keep"
        await res.set_keep_async(KeepOptions(overrides=dict(over)))
    for rule in ("newest", "shortest"):
        await res.set_keep_async(KeepOptions(rule=rule, overrides=dict(over)))  # type: ignore[arg-type]
        kept = {
            res.paths[r]
            for g in res.valid_groups()
            for r in range(g.start, g.start + g.count)
            if res.role(r) == C.ROLE_KEEP
        }
        assert kept == set(extras.values())


def test_touching_entries_seen_today_does_not_dirty_the_cache(tmp_path: Path) -> None:
    from spaceknife.dupes.cache import Stamp, VolumeCache

    vc = VolumeCache.load(b"vol", C.ALG_BLAKE2B, C.PROBE_PARAMS, True, tmp_path)
    vc.put(1, 2, Stamp(10, 1, 1), quick=5)
    vc.save()
    vc = VolumeCache.load(b"vol", C.ALG_BLAKE2B, C.PROBE_PARAMS, True, tmp_path)
    hits = vc.lookup(np.array([1], np.uint64), np.array([2], np.uint64))
    vc.touch(hits.tolist())
    assert not vc.dirty  # seen today already: nothing to write


@pytest.mark.skipif(sys.platform != "win32", reason="Windows opens names case-insensitively")
async def test_a_root_typed_in_another_case_still_finds_copies(tmp_path: Path, ops: Ops) -> None:
    """``c:\\users\\x\\photos`` typed for ``C:\\Users\\x\\Photos``: no entry has a case twin, so
    nothing is ambiguous (only a real twin in a folder on the way is)."""
    root = tmp_path / "Photos"
    a = blob(2 * MIB, 900)
    write(root, "Trip/a.bin", a)
    write(root, "Backup/a.bin", a)
    lower = Path(str(root).lower())
    job, res = await run(await scan(lower), ops, tmp_path / "c")
    assert "ambiguous_name" not in job.skipped_by_reason
    assert groups(res) == [{"a.bin"}]
