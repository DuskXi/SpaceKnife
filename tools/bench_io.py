"""M4 benchmark: the I/O governor in a real closed loop on this machine's disk.

    uv run python tools/bench_io.py                       # temp file on the system disk
    uv run python tools/bench_io.py --dir D:\\tmp --levels quiet,balanced,custom:50 --seconds 30

Workload: random reads from a temp file with OS caching bypassed (``FILE_FLAG_NO_BUFFERING`` /
``O_DIRECT``), so every read reaches the disk; the only write is creating that file. A thread
pool runs time-budgeted batches (like the scanner's slices) and every batch waits for
``IoGovernor.permit()``. The live sampler (PDH / sysfs) measures the disk; for each level the
report gives SpaceKnife's own load and the whole-disk load against the target, the 5 s moving
average error, single-second overshoot and convergence time. A cross-check ("self, own clock")
derives our queue depth from our own read timings (Little's law) instead of the baseline.

The M4 target: steady-state 5 s average ≤ the limit, error within ±10 %, single points at most
5 points over, convergence within 3 s of a level change.
"""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import json
import mmap
import multiprocessing
import os
import random
import statistics
import sys
import tempfile
import time
from concurrent.futures import ProcessPoolExecutor
from dataclasses import asdict, dataclass, field
from typing import Any

from loguru import logger
from rich.console import Console
from rich.table import Table

from spaceknife.io import IoGovernor, IoRegistry, Level, set_thread_low_io_priority
from spaceknife.io.governor import level_limit

console = Console()


# --------------------------------------------------------------------------- uncached reader (worker side)


class Reader:
    """Random uncached reads of ``block`` bytes from one file (one per worker process)."""

    def __init__(self, path: str, block: int, budget_s: float) -> None:
        self.block = block
        self.budget_s = budget_s
        self.blocks = os.path.getsize(path) // block
        self.rng = random.Random(os.getpid())
        if sys.platform == "win32":
            import ctypes
            from ctypes import wintypes

            k32 = ctypes.WinDLL("kernel32", use_last_error=True)
            k32.CreateFileW.restype = wintypes.HANDLE
            k32.CreateFileW.argtypes = [
                wintypes.LPCWSTR,
                wintypes.DWORD,
                wintypes.DWORD,
                ctypes.c_void_p,
                wintypes.DWORD,
                wintypes.DWORD,
                wintypes.HANDLE,
            ]
            k32.ReadFile.argtypes = [
                wintypes.HANDLE,
                ctypes.c_void_p,
                wintypes.DWORD,
                ctypes.POINTER(wintypes.DWORD),
                ctypes.c_void_p,
            ]
            k32.VirtualAlloc.restype = ctypes.c_void_p
            k32.VirtualAlloc.argtypes = [ctypes.c_void_p, ctypes.c_size_t, wintypes.DWORD, wintypes.DWORD]

            class Overlapped(ctypes.Structure):
                _fields_ = [
                    ("Internal", ctypes.c_void_p),
                    ("InternalHigh", ctypes.c_void_p),
                    ("Offset", wintypes.DWORD),
                    ("OffsetHigh", wintypes.DWORD),
                    ("hEvent", wintypes.HANDLE),
                ]

            # GENERIC_READ, share read|write, OPEN_EXISTING, FILE_FLAG_NO_BUFFERING
            self.h = k32.CreateFileW(path, 0x80000000, 3, None, 3, 0x20000000, None)
            if not self.h or self.h == wintypes.HANDLE(-1).value:
                raise ctypes.WinError(ctypes.get_last_error())
            self.k32 = k32
            self.buf = k32.VirtualAlloc(None, block, 0x3000, 0x04)  # page aligned
            self.got = wintypes.DWORD(0)
            self.ov = Overlapped()
        else:
            self.fd = os.open(path, os.O_RDONLY | getattr(os, "O_DIRECT", 0))
            self.mem = mmap.mmap(-1, block)  # page aligned, as O_DIRECT wants

    def _read(self, offset: int) -> int:
        if sys.platform == "win32":
            import ctypes

            self.ov.Offset, self.ov.OffsetHigh = offset & 0xFFFFFFFF, offset >> 32
            if not self.k32.ReadFile(
                self.h, self.buf, self.block, ctypes.byref(self.got), ctypes.byref(self.ov)
            ):
                raise ctypes.WinError(ctypes.get_last_error())
            return int(self.got.value)
        return os.preadv(self.fd, [self.mem], offset)

    def batch(self) -> tuple[int, float]:
        """Read random blocks for about ``budget_s``; returns (bytes, seconds inside read calls)."""
        deadline = time.perf_counter() + self.budget_s
        io = 0.0
        total = 0
        while True:
            offset = self.rng.randrange(self.blocks) * self.block
            t0 = time.perf_counter()
            total += self._read(offset)
            t1 = time.perf_counter()
            io += t1 - t0
            if t1 >= deadline:
                return total, io


_reader: Reader | None = None


def init_worker(path: str, block: int, budget_s: float) -> None:
    global _reader
    _reader = Reader(path, block, budget_s)


_low_io = False


def read_batch(low_io: bool = False) -> tuple[int, float]:
    """One batch; ``low_io`` follows ``IoGovernor.low_io_priority`` (the idle level), per batch."""
    global _low_io
    assert _reader is not None
    if low_io != _low_io:
        set_thread_low_io_priority(low_io)  # the pool runs batches on the worker's main thread
        _low_io = low_io
    return _reader.batch()


@dataclass
class Totals:
    """What the workers report back (parent side)."""

    bytes: int = 0
    io_seconds: float = 0.0  # summed time inside read calls: our own queue depth x time


def make_file(directory: str, size_mb: int) -> str:
    fd, path = tempfile.mkstemp(prefix="spaceknife-bench-", suffix=".bin", dir=directory)
    chunk = os.urandom(16 << 20)
    with os.fdopen(fd, "wb", buffering=0) as fh:
        left = size_mb << 20
        while left > 0:
            n = min(left, len(chunk))
            fh.write(chunk[:n])
            left -= n
        os.fsync(fh.fileno())
    return path


# --------------------------------------------------------------------------- recording


@dataclass
class Row:
    t: float  # seconds since the bench started
    phase: str
    limit: float
    self_pct: float
    total_pct: float
    others_pct: float
    active_pct: float
    own_pct: float  # cross-check: own queue depth from our read timings / Q_eff
    u: float
    nbar: float  # mean batches in flight over the window (the governor's own accounting)
    concurrency: int
    inflight: int
    yielding: bool
    probe: bool
    q_eff: float | None
    mbps: float  # our own reads
    # raw sample
    busy: float = 0.0
    queue: float = 0.0
    iops: float = 0.0
    lat_us: float = 0.0
    disk_mbps: float = 0.0


@dataclass
class Recorder:
    gov: IoGovernor
    totals: Totals
    t0: float
    phase: str = "baseline"
    rows: list[Row] = field(default_factory=list)
    _last: tuple[float, float, int] = (0.0, 0.0, 0)

    def hook(self) -> None:
        orig = self.gov.observe

        def observe(sample: Any, wall: float | None = None) -> None:
            probing = self.gov.core._probe == "holding"
            orig(sample, wall)
            self._row(probing, sample)

        self.gov.observe = observe  # type: ignore[method-assign]

    def _row(self, probing: bool, sample: Any) -> None:
        core = self.gov.core
        now = time.monotonic()
        last_t, last_io, last_bytes = self._last
        io, nbytes = self.totals.io_seconds, self.totals.bytes
        self._last = (now, io, nbytes)
        if not last_t:
            return
        span = now - last_t
        q = core.q_eff or 1.0
        st = core.status(history=False)
        self.rows.append(
            Row(
                t=round(now - self.t0, 3),
                phase=self.phase,
                limit=st.limit,
                self_pct=round(100.0 * core.self_load, 2),  # raw window values, not the 1 s display
                total_pct=round(100.0 * core.total, 2),
                others_pct=round(100.0 * (core.total - core.self_load), 2),
                active_pct=round(100.0 * core.active, 2),
                own_pct=round(100.0 * min(1.0, (io - last_io) / span / q), 2),
                u=round(core.u, 3),
                nbar=round(core._last_n, 2),
                concurrency=st.concurrency,
                inflight=st.inflight,
                yielding=st.yielding,
                probe=probing,
                q_eff=st.q_eff,
                mbps=round((nbytes - last_bytes) / span / 1e6, 1),
                busy=round(sample.busy, 4) if sample else 0.0,
                queue=round(sample.queue, 3) if sample else 0.0,
                iops=round(sample.read_iops or 0.0, 1) if sample else 0.0,
                lat_us=round((sample.read_latency or 0.0) * 1e6, 1) if sample else 0.0,
                disk_mbps=round((sample.read_bps or 0.0) / 1e6, 1) if sample else 0.0,
            )
        )


# --------------------------------------------------------------------------- analysis


def per_second(rows: list[Row], key: str) -> list[tuple[float, float]]:
    """Time-weighted 1 s means (what the W2 curve shows), keyed by the second's end."""
    if not rows:
        return []
    out: list[tuple[float, float]] = []
    start = rows[0].t
    bucket: list[float] = []
    edge = start + 1.0
    for r in rows:
        if r.t > edge + 1e-9 and bucket:
            out.append((edge, statistics.fmean(bucket)))
            bucket = []
            edge += 1.0
        bucket.append(float(getattr(r, key)))
    if bucket and rows[-1].t >= edge - 0.3:
        out.append((edge, statistics.fmean(bucket)))
    return out


def moving(values: list[float], n: int = 5) -> list[float]:
    return [statistics.fmean(values[i - n : i]) for i in range(n, len(values) + 1)]


def analyse(rows: list[Row], limit_pct: float, settle_s: float = 3.0) -> dict[str, Any]:
    secs = per_second(rows, "self_pct")
    own = per_second(rows, "own_pct")
    if not secs:
        return {}
    t_start = rows[0].t
    steady = [v for t, v in secs if t - t_start > settle_s]
    steady_own = [v for t, v in own if t - t_start > settle_s]
    mas = moving(steady)
    converge = None
    for i, (t, _v) in enumerate(secs):
        window = [v for _t, v in secs[i : i + 5]]
        if len(window) == 5 and abs(statistics.fmean(window) - limit_pct) <= 0.1 * limit_pct:
            converge = round(t - 1.0 - t_start, 2)  # start of the first 5 s window within ±10 %
            break
    total = [r.total_pct for r in rows if r.t - t_start > settle_s]
    return {
        "limit": limit_pct,
        "self_mean": round(statistics.fmean(steady), 1) if steady else None,
        "own_mean": round(statistics.fmean(steady_own), 1) if steady_own else None,
        "total_mean": round(statistics.fmean(total), 1) if total else None,
        "others_mean": round(statistics.fmean(r.others_pct for r in rows if r.t - t_start > settle_s), 1),
        "active_mean": round(statistics.fmean(r.active_pct for r in rows if r.t - t_start > settle_s), 1),
        "ma_min": round(min(mas), 1) if mas else None,
        "ma_max": round(max(mas), 1) if mas else None,
        "ma_err_max": round(max(abs(m - limit_pct) for m in mas) / limit_pct * 100, 1) if mas else None,
        "ma_over_limit": sum(1 for m in mas if m > limit_pct),
        "ma_windows": len(mas),
        "point_over_max": round(max(v - limit_pct for v in steady), 1) if steady else None,
        "converge_s": converge,
        "mbps": round(statistics.fmean(r.mbps for r in rows if r.t - t_start > settle_s), 1),
        "concurrency": round(statistics.fmean(r.concurrency for r in rows if r.t - t_start > settle_s), 2),
        "u": round(statistics.fmean(r.u for r in rows if r.t - t_start > settle_s), 2),
        "probes": sum(1 for r in rows if r.probe),
        "yielding_s": round(sum(0.25 for r in rows if r.yielding), 2),
        "q_eff": rows[-1].q_eff,
    }


# --------------------------------------------------------------------------- driver


async def drive(gov: IoGovernor, pool: ProcessPoolExecutor, totals: Totals, until: float) -> None:
    """The scanner's dispatch pattern: await a permit, hand a batch to the pool, bind the permit."""
    loop = asyncio.get_running_loop()
    inflight: set[asyncio.Future[tuple[int, float]]] = set()

    def done(fut: asyncio.Future[tuple[int, float]]) -> None:
        inflight.discard(fut)
        if not fut.cancelled() and fut.exception() is None:
            nbytes, io = fut.result()
            totals.bytes += nbytes
            totals.io_seconds += io

    while time.monotonic() < until:
        try:
            permit = await asyncio.wait_for(gov.permit(), timeout=max(0.01, until - time.monotonic()))
        except TimeoutError:
            break
        fut = loop.run_in_executor(pool, read_batch, gov.low_io_priority)
        permit.bind(fut)
        inflight.add(fut)
        fut.add_done_callback(done)
    if inflight:
        await asyncio.gather(*inflight, return_exceptions=True)


async def foreground(pool: ProcessPoolExecutor, n: int, until: float) -> int:
    """Another program's reads: ``n`` ungoverned batches in flight all the time; returns bytes read."""
    loop = asyncio.get_running_loop()
    inflight: set[asyncio.Future[tuple[int, float]]] = set()
    total = 0

    def done(fut: asyncio.Future[tuple[int, float]]) -> None:
        nonlocal total
        inflight.discard(fut)
        if not fut.cancelled() and fut.exception() is None:
            total += fut.result()[0]

    while time.monotonic() < until:
        while len(inflight) < n:
            fut = loop.run_in_executor(pool, read_batch)
            inflight.add(fut)
            fut.add_done_callback(done)
        await asyncio.sleep(0.005)
    if inflight:
        await asyncio.gather(*inflight, return_exceptions=True)
    return total


def analyse_yield(rows: list[Row], limit_pct: float, t_on: float, t_off: float) -> dict[str, Any]:
    """Yield experiment: when did the governor start / stop yielding, what did we get meanwhile."""
    during = [r for r in rows if t_on <= r.t < t_off]
    after = [r for r in rows if r.t >= t_off]
    first_yield = next((r.t for r in during if r.yielding), None)
    recovered = next((r.t for r in after if not r.yielding), None)
    back = None
    secs = per_second([r for r in after], "self_pct")
    for t, v in secs:
        if v >= 0.9 * limit_pct:
            back = round(t - t_off, 2)
            break
    return {
        "limit": limit_pct,
        "others_mean": round(statistics.fmean(r.others_pct for r in during), 1) if during else None,
        "self_during": round(statistics.fmean(r.self_pct for r in during), 1) if during else None,
        "own_during": round(statistics.fmean(r.own_pct for r in during), 1) if during else None,
        "yield_share": round(sum(r.yielding for r in during) / len(during), 2) if during else None,
        "yield_after_s": None if first_yield is None else round(first_yield - t_on, 2),
        "resume_after_s": None if recovered is None else round(recovered - t_off, 2),
        "self_back_s": back,
        "mbps_during": round(statistics.fmean(r.mbps for r in during), 1) if during else None,
    }


def parse_level(text: str) -> tuple[Level, float | None]:
    if text.startswith("custom:"):
        return "custom", float(text.split(":", 1)[1])
    if text not in ("max", "balanced", "quiet", "idle"):
        raise argparse.ArgumentTypeError(f"unknown level {text}")
    return text, None  # type: ignore[return-value]


async def run(args: argparse.Namespace) -> dict[str, Any]:
    levels = [parse_level(x) for x in args.levels.split(",") if x]
    console.print(f"[bold]creating {args.size_mb} MiB temp file in {args.dir}[/] ...")
    path = await asyncio.to_thread(make_file, args.dir, args.size_mb)
    pool = ProcessPoolExecutor(
        max_workers=args.workers,
        mp_context=multiprocessing.get_context("spawn"),  # never fork: the loop has threads
        initializer=init_worker,
        initargs=(path, args.block_kb << 10, args.budget_ms / 1000.0),
    )
    await asyncio.gather(*(asyncio.wrap_future(pool.submit(os.getpid)) for _ in range(args.workers)))
    totals = Totals()
    registry = IoRegistry(max_workers=args.workers)
    result: dict[str, Any] = {"args": vars(args), "levels": []}
    try:
        await registry.start()
        gov = await registry.governor_for_path(path)
        gov.set_yield_when_busy(not args.no_yield)
        ref = gov.disk
        sampler = registry.sampler.name if registry.sampler else "-"
        console.print(
            f"disk [bold]{ref.id}[/] ({ref.media}, sampler {sampler}), "
            f"q_eff prior {gov.core.q_eff}, worker processes <= {args.workers}"
        )
        t0 = time.monotonic()
        rec = Recorder(gov, totals, t0)
        rec.hook()
        await asyncio.sleep(args.settle)  # let the file's write-back finish
        rec.rows.clear()
        rec.phase = "baseline"
        await asyncio.sleep(args.baseline)
        base = [r for r in rec.rows if r.phase == "baseline"]
        result["baseline"] = {
            "total_mean": round(statistics.fmean(r.total_pct for r in base), 2) if base else None,
            "active_mean": round(statistics.fmean(r.active_pct for r in base), 2) if base else None,
        }
        for n in [int(x) for x in args.sweep.split(",") if x]:
            # open-loop curve: fixed parallelism, no limit (where does the disk saturate?)
            gov.set_level("max")
            gov.set_manual_concurrency(n)
            rec.phase = f"sweep {n}"
            await drive(gov, pool, totals, time.monotonic() + args.sweep_seconds)
            rows = [r for r in rec.rows if r.phase == rec.phase][2:]
            if rows:
                result.setdefault("sweep", []).append(
                    {
                        "n": n,
                        "queue": round(statistics.fmean(r.queue for r in rows), 2),
                        "busy": round(statistics.fmean(r.busy for r in rows), 3),
                        "lat_us": round(statistics.fmean(r.lat_us for r in rows), 1),
                        "iops": round(statistics.fmean(r.iops for r in rows)),
                        "disk_mbps": round(statistics.fmean(r.disk_mbps for r in rows)),
                        "rho": round(statistics.fmean(r.total_pct for r in rows), 1),
                        "q_eff": rows[-1].q_eff,
                    }
                )
        if args.sweep:
            gov.set_manual_concurrency(None)
        for level, custom in levels:
            name = level if custom is None else f"custom {custom:g}%"
            gov.set_level(level, custom)
            rec.phase = name
            console.print(f"[bold]{name}[/]: {args.seconds:.0f} s ...")
            await drive(gov, pool, totals, time.monotonic() + args.seconds)
            rows = [r for r in rec.rows if r.phase == name]
            stats = analyse(rows, 100.0 * level_limit(level, custom))
            stats["level"] = name
            result["levels"].append(stats)
            await asyncio.sleep(args.gap)
        if args.yield_test:
            level, custom = parse_level(args.yield_test)
            name = f"yield test {args.yield_test}"
            gov.set_level(level, custom)
            rec.phase = name
            third = args.seconds / 3
            console.print(
                f"[bold]{name}[/]: {args.seconds:.0f} s, {args.others} foreground readers in the middle ..."
            )
            fg = ProcessPoolExecutor(
                max_workers=args.others,
                mp_context=multiprocessing.get_context("spawn"),
                initializer=init_worker,
                initargs=(path, args.block_kb << 10, args.budget_ms / 1000.0),
            )
            fg_with = 0
            try:
                await asyncio.gather(*(asyncio.wrap_future(fg.submit(os.getpid)) for _ in range(args.others)))
                rec.phase = "foreground alone"
                t_alone = time.monotonic()
                fg_alone = await foreground(fg, args.others, t_alone + third) / (time.monotonic() - t_alone)
                await asyncio.sleep(args.gap)
                rec.phase = name
                start = time.monotonic()

                async def others() -> None:
                    nonlocal fg_with
                    await asyncio.sleep(third)
                    t_fg = time.monotonic()
                    fg_with = int(
                        await foreground(fg, args.others, start + 2 * third) / (time.monotonic() - t_fg)
                    )

                await asyncio.gather(drive(gov, pool, totals, start + args.seconds), others())
            finally:
                fg.shutdown(wait=True)
            rows = [r for r in rec.rows if r.phase == name]
            t_on = start - t0 + third
            result["yield_test"] = analyse_yield(rows, 100.0 * level_limit(level, custom), t_on, t_on + third)
            result["yield_test"]["level"] = name
            result["yield_test"]["yield_when_busy"] = not args.no_yield
            result["yield_test"]["fg_alone_mbps"] = round(fg_alone / 1e6, 1)
            result["yield_test"]["fg_with_scan_mbps"] = round(fg_with / 1e6, 1)
            result["yield_test"]["fg_slowdown_pct"] = (
                round(100.0 * (1.0 - fg_with / fg_alone), 1) if fg_alone else None
            )
        result["rows"] = [asdict(r) for r in rec.rows]
    finally:
        await registry.stop()
        pool.shutdown(wait=True)
        if not args.keep_file:
            with contextlib.suppress(OSError):
                os.remove(path)
    return result


def report(result: dict[str, Any]) -> None:
    if result.get("sweep"):
        sweep = Table(title="open-loop sweep: fixed parallelism, no limit")
        for col in ("workers", "queue", "busy", "latency us", "IOPS", "MB/s", "rho %", "Q_eff"):
            sweep.add_column(col, justify="right")
        for s in result["sweep"]:
            sweep.add_row(
                *(str(s[k]) for k in ("n", "queue", "busy", "lat_us", "iops", "disk_mbps", "rho", "q_eff"))
            )
        console.print(sweep)
    base = result.get("baseline", {})
    console.print(
        f"baseline before the scan: whole-disk load {base.get('total_mean')} % "
        f"(active time {base.get('active_mean')} %)"
    )
    table = Table(title="I/O governor, real closed loop (steady state = after the first 3 s)")
    for col in (
        "level",
        "limit %",
        "self %",
        "self, own clock %",
        "total %",
        "others %",
        "active %",
        "5 s MA min..max",
        "MA err max",
        "MA > limit",
        "1 s over max",
        "converge s",
        "MB/s",
        "conc",
        "probes",
        "Q_eff",
    ):
        table.add_column(col, justify="right")
    for s in result["levels"]:
        if not s:
            continue
        table.add_row(
            s["level"],
            f"{s['limit']:.0f}",
            f"{s['self_mean']}",
            f"{s['own_mean']}",
            f"{s['total_mean']}",
            f"{s['others_mean']}",
            f"{s['active_mean']}",
            f"{s['ma_min']}..{s['ma_max']}",
            f"{s['ma_err_max']} %",
            f"{s['ma_over_limit']}/{s['ma_windows']}",
            f"{s['point_over_max']:+}",
            f"{s['converge_s']}",
            f"{s['mbps']}",
            f"{s['concurrency']}",
            f"{s['probes']}",
            f"{s['q_eff']}",
        )
    console.print(table)
    if y := result.get("yield_test"):
        yt = Table(title=f"yield experiment ({y['level']}, yield when busy: {y['yield_when_busy']})")
        for col in (
            "limit %",
            "others %",
            "self % (during)",
            "self, own clock %",
            "yielding share",
            "yield after s",
            "resume after s",
            "self back after s",
            "MB/s during",
            "foreground MB/s alone",
            "with scan",
            "slowdown %",
        ):
            yt.add_column(col, justify="right")
        yt.add_row(
            *(
                str(y[k])
                for k in (
                    "limit",
                    "others_mean",
                    "self_during",
                    "own_during",
                    "yield_share",
                    "yield_after_s",
                    "resume_after_s",
                    "self_back_s",
                    "mbps_during",
                    "fg_alone_mbps",
                    "fg_with_scan_mbps",
                    "fg_slowdown_pct",
                )
            )
        )
        console.print(yt)


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--dir", default=tempfile.gettempdir(), help="where the temp file goes (picks the disk)")
    ap.add_argument("--size-mb", type=int, default=1024)
    ap.add_argument("--block-kb", type=int, default=64)
    ap.add_argument("--budget-ms", type=float, default=25.0, help="time budget of one batch")
    ap.add_argument("--workers", type=int, default=32, help="worker processes = max concurrency")
    ap.add_argument("--levels", default="quiet,balanced")
    ap.add_argument("--seconds", type=float, default=30.0)
    ap.add_argument("--baseline", type=float, default=5.0)
    ap.add_argument("--settle", type=float, default=3.0)
    ap.add_argument("--gap", type=float, default=2.0, help="idle pause between levels")
    ap.add_argument("--no-yield", action="store_true", help="turn 'yield when busy' off")
    ap.add_argument("--keep-file", action="store_true")
    ap.add_argument("--json", help="write the full time series here")
    ap.add_argument("--sweep", default="", help="first run fixed parallelism steps, e.g. 1,2,4,8,16,32")
    ap.add_argument("--sweep-seconds", type=float, default=4.0)
    ap.add_argument("--yield-test", default="", help="level for the yield experiment, e.g. balanced")
    ap.add_argument("--others", type=int, default=32, help="foreground readers in the yield experiment")
    args = ap.parse_args()
    logger.remove()
    logger.add(sys.stderr, level="INFO")
    result = asyncio.run(run(args))
    if args.json:
        with open(args.json, "w", encoding="utf-8") as fh:
            json.dump(result, fh, indent=1)
    report(result)


if __name__ == "__main__":
    main()
