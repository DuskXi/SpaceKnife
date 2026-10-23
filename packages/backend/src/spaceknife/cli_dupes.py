"""``spaceknife dupes``: find duplicate files from the command line, and measure the engine.

It scans each path the way ``spaceknife scan`` does (read-only), then runs the duplicate-file job
(docs/dupes.md) on the result and prints what each stage did: candidates, metadata and data
opens, bytes read per stage against the design's lower bound, cache hits, time, CPU, peak memory,
permits taken from the disk-load governor, skip reasons and access-time checks. ``--json`` prints
the same as one JSON object (``--groups`` adds every verified group with its paths), which the
Verify measurements and the reference checks read.

Nothing is deleted or moved: this command only reads.
"""

from __future__ import annotations

import asyncio
import json
import sys
import time
from typing import Annotated, Any

import typer
from rich.console import Console
from rich.table import Table

from . import fmt
from .dupes import Job, JobOptions, KeepOptions, Results, ScanInput, Scope
from .dupes import codes as C
from .dupes.select import plan, select
from .i18n import tr

console = Console(stderr=True)


def _parse_size(text: str) -> int:
    t = text.strip().upper().replace("IB", "").replace("B", "")
    mult = 1
    for suffix, m in (("K", 1 << 10), ("M", 1 << 20), ("G", 1 << 30), ("T", 1 << 40)):
        if t.endswith(suffix):
            t, mult = t[:-1], m
            break
    return int(float(t) * mult)


async def _scan(paths: list[str], workers: int | None) -> list[ScanInput]:
    from .cli_scan import _run_scan

    out: list[ScanInput] = []
    for i, p in enumerate(paths):
        scanner, _ = await _run_scan([p], workers, False)
        out.append(ScanInput(f"scan-{i}", scanner.store, scanner.roots))
    return out


def _report(
    job: Job, res: Results, sel_plan: dict[str, Any], scans: list[ScanInput], *, groups: bool
) -> dict[str, Any]:
    reader = job.reader_counters()
    sel = job.sel
    n_cand = len(sel) if sel is not None else 0
    n_groups, n_extra, reclaim = res.totals()
    out: dict[str, Any] = {
        "state": job.state,
        "error": job.error,
        "ops": job.ops.name,
        "elapsed_s": round(job.elapsed, 3),
        "cpu_s": round(job.cpu_s, 3),
        "peak_rss": job.peak_rss,
        "rss_at_start": job.rss0,
        "plan": sel_plan,
        "funnel": job.funnel.as_dict(),
        "counters": job.counters.as_dict(),
        "reader": reader,
        "skipped_by_reason": job.skipped_by_reason,
        "candidate_bytes": C.CAND_DTYPE.itemsize,
        "candidate_table_bytes": n_cand * C.CAND_DTYPE.itemsize,
        "member_bytes": C.MEMBER_DTYPE.itemsize,
        "members": res.n,
        "groups": n_groups,
        "extra_files": n_extra,
        "reclaim": reclaim,
        "lower_bound_bytes": job.counters.lower_bound,
        "volumes": [
            {
                "index": v.index,
                "name": v.name,
                "fs": v.fs_type,
                "trust": v.trust,
                "clonable": v.clonable,
                "notes": v.notes,
            }
            for v in job.vols
        ],
        "queues": [
            {"key": q.key, "media": q.media, "threads": q.pool._max_workers} for q in job.queues.values()
        ],
    }
    if groups:
        ids = [s.scan_id for s in scans]
        out["group_list"] = [
            {
                "id": g.id,
                "size": g.size,
                "shared": g.shared,
                "digest": g.digest.hex() if g.digest else None,
                "reclaim": g.reclaim,
                "members": [res.member_dict(r, ids) for r in range(g.start, g.start + g.count)],
            }
            for g in res.valid_groups()
        ]
    return out


def _table(rep: dict[str, Any]) -> Table:
    t = Table(title=tr("重复文件"), show_header=False)
    f, c, r = rep["funnel"], rep["counters"], rep["reader"]

    def n(x: int) -> str:
        return f"{x:,}"

    rows = [
        (tr("状态"), f"{rep['state']} {rep['error']}".strip()),
        (tr("读法"), rep["ops"]),
        (tr("候选（同大小）"), n(f["candidates"])),
        (
            tr("物理文件"),
            tr("{n}（别名 {a} · 块克隆 {c}）", n=n(f["physical"]), a=n(f["aliases"]), c=n(f["clones"])),
        ),
        (tr("头尾读取"), tr("{n} 个文件 · {size}", n=n(f["probed"]), size=fmt.size(c["bytes_probe"]))),
        (tr("全文读取"), tr("{n} 个文件 · {size}", n=n(f["verifying"]), size=fmt.size(c["bytes_full"]))),
        (tr("内容相同"), tr("{g} 组 · {n} 个文件", g=n(f["confirmed_groups"]), n=n(f["confirmed_files"]))),
        (tr("可腾出"), tr("{size}（{n} 份多余）", size=fmt.size(rep["reclaim"]), n=n(rep["extra_files"]))),
        (tr("数据打开"), tr("{n}（租期重开 {r}）", n=n(r.get("data_opens", 0)), r=n(r.get("reopens", 0)))),
        (tr("元数据查询"), tr("{n} · 块映射 {e}", n=n(c["meta_ops"]), e=n(c["extent_ops"]))),
        (
            tr("缓存"),
            tr(
                "命中 {h} · 认定相同 {j} · 分开 {s}",
                h=n(c["cache_hits"]),
                j=n(c["cache_joined"]),
                s=n(c["cache_separated"]),
            ),
        ),
        (tr("许可 / IO 调用"), f"{n(c['permits'])} / {n(c['io_calls'])}"),
        (
            tr("访问时间"),
            tr(
                "核对 {c} · 被别人改了 {m} · 保留不了 {u}",
                c=n(c["atime_checked"]),
                m=n(c["atime_moved"]),
                u=n(c["atime_unkept"]),
            ),
        ),
        (tr("跳过"), ", ".join(f"{k} {v}" for k, v in rep["skipped_by_reason"].items()) or "—"),
        (tr("用时 / CPU"), f"{rep['elapsed_s']:.2f} s / {rep['cpu_s']:.2f} s"),
        (
            tr("候选表"),
            tr("{n} B（{per} B/个）", n=n(rep["candidate_table_bytes"]), per=rep["candidate_bytes"]),
        ),
    ]
    for k, v in rows:
        t.add_row(k, v)
    return t


def dupes(
    paths: Annotated[list[str], typer.Argument(help="Folders or volumes to scan and search (read-only).")],
    min_size: Annotated[
        str, typer.Option("--min-size", help="Smallest file compared (e.g. 1M, 64K).")
    ] = "1M",
    cross: Annotated[
        bool, typer.Option("--cross", help="Compare across all given paths (cross-volume).")
    ] = False,
    load: Annotated[str, typer.Option(help="Disk-load level: idle, quiet, balanced, max.")] = "quiet",
    budget_bytes: Annotated[
        str | None, typer.Option("--budget-bytes", help="Stop after reading this much.")
    ] = None,
    max_seconds: Annotated[float | None, typer.Option("--max-seconds", help="Stop after this long.")] = None,
    no_cache: Annotated[
        bool, typer.Option("--no-cache", help="Neither read nor write the hash cache.")
    ] = False,
    include_system: Annotated[
        bool, typer.Option("--include-system", help="Also system and program folders.")
    ] = False,
    include_network: Annotated[bool, typer.Option("--include-network", help="Also network volumes.")] = False,
    keep_atime: Annotated[
        str, typer.Option("--keep-atime", help="skip / read: when access times cannot be kept.")
    ] = "skip",
    stop_on_atime: Annotated[
        bool, typer.Option("--stop-on-atime", help="Stop reading a volume once access times moved.")
    ] = False,
    python: Annotated[
        bool, typer.Option("--python", help="Use the Python reader even when the native one is built.")
    ] = False,
    threads: Annotated[int | None, typer.Option(help="Reader threads per disk (default: by media).")] = None,
    workers: Annotated[int | None, typer.Option(help="Scan workers (0: in-process).")] = None,
    as_json: Annotated[bool, typer.Option("--json", help="Print one JSON object.")] = False,
    groups: Annotated[bool, typer.Option("--groups", help="With --json: every group and its paths.")] = False,
) -> None:
    """Find files whose whole contents are identical (read-only; deletes nothing)."""
    from .dupes.reads import FallbackOps, default_ops

    if keep_atime not in ("skip", "read"):
        raise typer.BadParameter("--keep-atime: skip or read")
    if load not in ("idle", "quiet", "balanced", "max"):
        raise typer.BadParameter("--load: idle, quiet, balanced or max")
    ops = FallbackOps() if python else default_ops()

    async def main() -> list[dict[str, Any]]:
        from .io.registry import IoRegistry

        scans = await _scan(paths, workers)
        registry = IoRegistry(max_workers=8)
        await registry.start()
        reports: list[dict[str, Any]] = []
        try:
            sets = [scans] if cross else [[s] for s in scans]
            for part in sets:
                scope = Scope(
                    part,
                    min_size=_parse_size(min_size),
                    include_system=include_system,
                    include_network=include_network,
                )
                t0 = time.monotonic()
                sel_plan = plan(select(scope), n_scans=len(part))
                sel_plan["plan_s"] = round(time.monotonic() - t0, 4)
                res = Results(part, KeepOptions())
                job = Job(
                    scope,
                    options=JobOptions(
                        load=load,  # type: ignore[arg-type]
                        keep_atime=keep_atime,  # type: ignore[arg-type]
                        budget_bytes=_parse_size(budget_bytes) if budget_bytes else None,
                        max_seconds=max_seconds,
                        use_cache=not no_cache,
                        stop_on_atime_moved=stop_on_atime,
                        threads=threads,
                    ),
                    ops=ops,
                    registry=registry,
                    sink=res,
                )
                await job.run()
                reports.append(_report(job, res, sel_plan, part, groups=groups))
        finally:
            await registry.stop()
        return reports

    reports = asyncio.run(main())
    if as_json:
        sys.stdout.write(
            json.dumps(reports if len(reports) > 1 else reports[0], ensure_ascii=False, indent=1) + "\n"
        )
        return
    for rep in reports:
        console.print(_table(rep))


__all__ = ["dupes"]
