"""``spaceknife scan`` and ``spaceknife snapshot …``: terminal front end for the scan core."""

from __future__ import annotations

import asyncio
import json
import multiprocessing
import os
import time
from collections.abc import Callable
from concurrent.futures import Executor, ThreadPoolExecutor
from dataclasses import dataclass, field
from pathlib import Path
from typing import TYPE_CHECKING, Annotated, Any

import numpy as np
import typer
from rich.console import Console, Group
from rich.panel import Panel
from rich.table import Table
from rich.text import Text

from . import fmt, i18n
from .i18n import tr
from .io.priority import set_low_io_priority
from .paths import config_dir, snapshots_dir
from .scan import worker
from .scan.scanner import (
    NATIVE_BATCH,
    NATIVE_BUDGET_S,
    Scanner,
    ScanProgress,
    ScanState,
    bulk_listing,
    default_workers,
    listing_max,
    make_pool,
    native_slicer,
    native_threads,
)
from .store.nodes import Flag, Kind, NodeStore
from .store.subtree import largest

if TYPE_CHECKING:
    from .io import IoRegistry

console = Console()
# set once ``scan`` has written all its output and saved its snapshot: cli.main() may then end the
# process without the interpreter's shutdown (cli._end_now)
output_complete = False
snapshot_app = typer.Typer(help="Inspect saved snapshots.", no_args_is_help=True)

KIND_ICON = {Kind.DIR: "▸", Kind.FILE: " ", Kind.LINK: "↪", Kind.OTHER: "·"}
WARN = "#F5C542"


@dataclass
class ScanMethod:
    """How ``spaceknife scan`` read the tree: ``--mft`` asked for the MFT fast scan; ``mft`` it was
    read that way; ``fallback`` why it was scanned the hard way instead; the MFT phases' seconds."""

    asked: bool = False
    mft: bool = False
    fallback: str = ""
    phases: dict[str, float] = field(default_factory=dict)


def mft_helper() -> Any:
    """The read-only helper that reads a volume's $MFT (the app's, one per command; tests replace it).
    Imported here: a plain ``scan`` does not load the helper's code."""
    from .helper import SharedHelper

    return SharedHelper()


def mft_duty(limit: tuple[str, float | None] | None) -> float:
    """The $MFT reader's busy share for ``--disk-limit``, mapped as the app maps its load levels
    (``server.scans.mft_duty``): no limit and max read flat out, idle at 0.2."""
    from .io.governor import level_limit  # loads the protocol's models: only with --mft

    if limit is None or limit[0] == "max":
        return 1.0
    if limit[0] == "idle":
        return 0.2
    return max(0.05, min(1.0, level_limit(limit[0], limit[1])))  # type: ignore[arg-type]


def apply_language() -> None:
    """The interface language of a command-line program (M7.5): the user's setting
    (``general.language``) when the settings file can be read, else the system's."""
    setting = "system"
    if not os.environ.get("SPACEKNIFE_LANGUAGE"):  # forced (tests): no need to read the settings
        try:
            setting = stored_language()
        except Exception:  # an unreadable settings file must not stop the command
            setting = "system"
    i18n.set_language(i18n.resolve(setting))


def stored_language(file: Path | None = None) -> str:
    """``general.language`` as :class:`~spaceknife.settings.SettingsStore` reads it. The store
    validates the whole file and falls back to the defaults when anything in it is invalid, so a
    value other than the default "system" needs the store itself; but without a file, with a file
    that cannot be read or parsed, or one that does not set another language, every outcome is
    "system", and the settings registry with the protocol's pydantic models need not load (a
    good part of the command-line scan's start-up; docs/packaging.md, 命令行扫描的启动)."""
    file = file or config_dir() / "settings.json"
    try:
        doc = json.loads(file.read_text(encoding="utf-8"))
    except (OSError, ValueError):  # missing, unreadable or not JSON: the store keeps its defaults
        return "system"
    values = doc.get("values") if isinstance(doc, dict) else None
    if not isinstance(values, dict) or values.get("general.language", "system") == "system":
        return "system"
    from .settings import SettingsStore

    return str(SettingsStore(file).get("general.language"))


def _bar(fraction: float, width: int = 12) -> Text:
    filled = max(0, min(width, round(fraction * width)))
    return Text("█" * filled, style="#8B9CFF") + Text("░" * (width - filled), style="#2A2E37")


def top_table(store: NodeStore, node: int, limit: int, depth: int, title: str) -> Table:
    _, total = store.effective_size(node)
    table = Table(title=title, title_justify="left", expand=False, pad_edge=False, box=None, padding=(0, 1))
    table.add_column(tr("名称"), overflow="ellipsis", max_width=44, no_wrap=True, ratio=1)
    table.add_column(tr("占用"), justify="right", style="bold", min_width=8, no_wrap=True)
    table.add_column(tr("占比"), justify="right", min_width=6, no_wrap=True)
    table.add_column("", no_wrap=True, min_width=12, max_width=18)
    table.add_column(tr("文件数"), justify="right", style="#8F95A2", no_wrap=True)

    def add(parent: int, level: int) -> None:
        children = store.sorted_children(parent)
        shown = children[:limit]
        for child in shown.tolist():
            v = store.view(child)
            share = v.tot_alloc / total if total else 0.0
            label = Text("  " * level + KIND_ICON.get(v.kind, " ") + " ")
            name_style = "bold" if v.kind == Kind.DIR else ""
            if v.flags & Flag.ACCESS_DENIED:
                name_style = "#F5C542"
            label.append(v.name, style=name_style)
            if v.flags & Flag.ACCESS_DENIED:
                label.append("  " + tr("无法访问"), style="#F5C542")
            files = fmt.count(v.tot_files) if v.kind == Kind.DIR else ""
            table.add_row(label, fmt.size(v.tot_alloc), f"{share * 100:.1f}%", _bar(share), files)
            if level + 1 < depth and v.kind == Kind.DIR and v.child_count:
                add(child, level + 1)
        rest = children[limit:]
        if len(rest):
            rest_alloc = int(sum(store.effective_size(int(i))[1] for i in rest.tolist()))
            share = rest_alloc / total if total else 0.0
            table.add_row(
                Text("  " * level + "  " + tr("…其余 {count} 项", count=f"{len(rest):,}"), style="#8F95A2"),
                fmt.size(rest_alloc),
                f"{share * 100:.1f}%",
                _bar(share),
                "",
            )

    add(node, 0)
    return table


def summary_panel(
    scanner: Scanner, result: ScanProgress, snap: Path | None, method: ScanMethod | None = None
) -> Panel:
    state = {
        ScanState.DONE: f"[green]{tr('完成')}[/green]",
        ScanState.CANCELLED: f"[yellow]{tr('已停止（结果不完整）')}[/yellow]",
        ScanState.FAILED: f"[red]{tr('失败')}[/red]",
    }.get(result.state, result.state.value)
    lines = [
        tr(
            "状态 {state}    用时 {time}    速度 每秒 {rate} 项",
            state=state,
            time=fmt.duration(result.elapsed_s),
            rate=f"{result.rate:,.0f}",
        ),
        tr(
            "项目 {items}（文件 {files} · 文件夹 {folders}）",
            items=fmt.count(result.items),
            files=fmt.count(result.files),
            folders=fmt.count(result.dirs),
        ),
        tr(
            "占用 {size}    逻辑大小 {logical}",
            size=f"[bold]{fmt.size(result.bytes_alloc)}[/bold]",
            logical=fmt.size(result.bytes_size),
        ),
    ]
    if method is not None and method.asked:  # --mft: say which way it was read, and why not by $MFT
        if method.mft:
            lines.append(tr("方式 MFT 快速扫描（读取卷的 $MFT）"))
        else:
            lines.append(tr("方式 硬扫描（{reason}）", reason=f"[{WARN}]{method.fallback}[/{WARN}]"))
    for root in scanner.roots:
        vol = root.volume
        if vol is None:
            continue
        line = tr(
            "卷 {mount} · {fs} · 共 {total}",
            mount=vol.mount_point,
            fs=vol.fs_type or tr("未知"),
            total=fmt.size(vol.total),
        )
        space = scanner.space_summary(root)
        if space is not None:
            free, unknown = space
            line += " · " + tr(
                "可用 {free} · 未知空间 {unknown}",
                free=f"[green]{fmt.size(free)}[/green]",
                unknown=fmt.size(unknown),
            )
        lines.append(line)
    if result.errors:
        denied = tr("{count} 个文件夹无法访问", count=result.errors)
        lines.append(
            tr(
                "{denied}（已计入未知空间），例如 {example}",
                denied=f"[#F5C542]{denied}[/#F5C542]",
                example=result.denied[0],
            )
        )
    if snap is not None:
        lines.append(f"[#8F95A2]{tr('快照已保存：{path}', path=snap)}[/#8F95A2]")
    if not (method is not None and method.mft) and any(
        r.volume and r.volume.fs_type.upper() == "NTFS" for r in scanner.roots
    ):  # hard scan on Windows: explain the accounting limits honestly
        note = tr(
            "说明：这是硬扫描。NTFS 的硬链接按每个链接计入，占用按簇推算；"
            "扫整个本地 NTFS 卷时加 --mft 改用 MFT 快速扫描，会给出精确值（需要管理员权限）。"
        )
        lines.append(f"[#8F95A2]{note}[/#8F95A2]")
    return Panel("\n".join(lines), title=tr("扫描结果"), title_align="left", border_style="#2A2E37")


def load_limit(text: str) -> tuple[str, float | None]:
    """``--disk-limit``: a level of the app's 「磁盘负载上限」 or a percentage (custom)."""
    value = text.strip().lower().removesuffix("%")
    if value in ("max", "balanced", "quiet", "idle"):
        return value, None
    if value.isdigit() and 1 <= int(value) <= 100:
        return "custom", float(value)
    raise typer.BadParameter(tr("磁盘负载上限只能是 max、balanced、quiet、idle 或 1–100 的百分比"))


async def _read_mft(
    scanner: Scanner, method: ScanMethod, duty: float, show: Callable[[str, float | None], None]
) -> str | None:
    """Read the one root's volume through the read-only helper and take over the tree built from
    its $MFT (what the app's ScanManager._run_mft does). None when done; else why it failed, and
    the scanner is ready for a hard scan."""
    from .helper import HelperError
    from .scan import mft

    root = scanner.roots[0]
    helper = mft_helper()
    phases = method.phases
    started = time.perf_counter()
    scanner.begin_external()
    try:
        show(tr("启动读取 $MFT 的只读助手"), None)
        client = await helper.get(elevate=True)
        reading = time.perf_counter()
        phases["helper"] = reading - started
        sending: list[float] = []

        def on_progress(phase: str, elapsed: float, read: float | None) -> None:
            if phase == "send":
                if not sending:
                    sending.append(time.perf_counter())
                show(tr("接收读到的表"), None)
            else:
                show(tr("读取 $MFT"), read)

        table = await client.scan_mft(root.path[:2], duty=duty, on_progress=on_progress)
        received = time.perf_counter()
        phases["read"] = (sending[0] if sending else received) - reading
        if sending:
            phases["receive"] = received - sending[0]
        show(tr("建树"), None)
        store = await asyncio.to_thread(mft.table_to_store, table, root.path, client.last_scan, consume=True)
        del table
        scanner.store.adopt(store)
        phases["build"] = time.perf_counter() - received
        scanner.finish_external(ScanState.DONE)
        return None
    except (HelperError, OSError, ValueError) as exc:
        scanner.reset_external()
        return exc.message if isinstance(exc, HelperError) else str(exc)
    finally:
        await helper.close()


async def _try_mft(scanner: Scanner, method: ScanMethod, limit: tuple[str, float | None] | None) -> None:
    """``--mft``: the MFT fast scan where the roots allow it, with its own progress line; a note
    on the console why not (method.fallback), and then the caller scans the hard way."""
    from .helper import is_elevated
    from .scan import mft

    reason = mft.mft_unsupported(scanner.roots)
    if reason is not None:
        method.fallback = reason
        console.print(f"[{WARN}]{tr('不支持 MFT 快速扫描：{reason}。改用硬扫描。', reason=reason)}[/{WARN}]")
        return
    if not is_elevated():
        console.print(f"[#8F95A2]{tr('读取 $MFT 需要管理员权限：Windows 会请求一次授权。')}[/#8F95A2]")
    from rich.progress import BarColumn, Progress, SpinnerColumn, TextColumn, TimeElapsedColumn

    progress = Progress(
        SpinnerColumn(style="#8B9CFF"),
        TextColumn(f"[bold]{tr('MFT 快速扫描')}[/bold]"),
        BarColumn(bar_width=28, complete_style="#8B9CFF", finished_style="#3FCB80"),
        TextColumn("{task.fields[pct]}"),
        TimeElapsedColumn(),
        TextColumn("[#8F95A2]{task.fields[step]}"),
        console=console,
        transient=True,
    )
    with progress:
        task = progress.add_task("mft", total=None, pct="", step="")

        def show(step: str, fraction: float | None) -> None:
            progress.update(
                task,
                total=1.0 if fraction is not None else None,
                completed=fraction or 0,
                pct=f"{fraction * 100:4.1f}%" if fraction is not None else "",
                step=step,
            )

        reason = await _read_mft(scanner, method, mft_duty(limit), show)
    if reason is None:
        method.mft = True
        return
    method.fallback = reason
    console.print(f"[{WARN}]{tr('MFT 快速扫描不可用：{reason}；改用硬扫描', reason=reason)}[/{WARN}]")


async def _run_scan(
    paths: list[str],
    workers: int | None,
    cross_mounts: bool,
    limit: tuple[str, float | None] | None = None,
    use_mft: bool = False,
) -> tuple[Scanner, ScanProgress, ScanMethod]:
    slicer = None if workers == 0 or os.environ.get("SPACEKNIFE_NO_NATIVE") else native_slicer()
    pool: Executor | None
    options: dict[str, Any] = {}
    if slicer is not None:  # as in the app: the native walker on listing threads
        threads = workers or native_threads()
        pool = ThreadPoolExecutor(max_workers=threads, thread_name_prefix="spaceknife-list")
        options = {"slicer": slicer, "batch_tasks": NATIVE_BATCH, "budget_s": NATIVE_BUDGET_S}
    else:  # the Python walker in worker processes
        threads = default_workers() if workers is None else workers
        pool = make_pool(threads) if threads > 0 else None
    registry: IoRegistry | None = None
    low_io = False
    try:
        scanner = Scanner(
            paths, executor=pool, concurrency=max(1, threads), cross_mounts=cross_mounts, **options
        )
        method = ScanMethod(asked=use_mft)
        if use_mft:
            await _try_mft(scanner, method, limit)
            if method.mft:
                return scanner, scanner.progress(), method
        if limit is not None and pool is not None:  # paced like the app's scans
            from .io import IoRegistry  # only here: the governor loads the protocol's models

            most = listing_max() if slicer is not None else max(1, threads)  # in listings, or processes
            registry = IoRegistry(max_workers=most)
            await registry.start()
            gov = await registry.governor_for_path(scanner.roots[0].path)
            gov.set_max_workers(most)
            gov.set_level(limit[0], limit[1])  # type: ignore[arg-type]
            scanner.governor = gov
            low_io = gov.low_io_priority and set_low_io_priority(None, True)
        if not console.is_terminal or console.is_dumb_terminal:
            # rich draws the live progress only on a terminal; with the output redirected (scripts,
            # benchmarks) it would show nothing: no display, no refresh thread, no import
            try:
                result = await scanner.run()
            except asyncio.CancelledError:
                scanner.cancel()
                raise
            finally:
                if not console.is_interactive:  # the empty line rich's Progress.stop() prints there
                    console.print()
            return scanner, result, method
        from rich.progress import BarColumn, Progress, SpinnerColumn, TextColumn, TimeElapsedColumn

        progress = Progress(
            SpinnerColumn(style="#8B9CFF"),
            TextColumn(f"[bold]{tr('扫描中')}[/bold]"),
            BarColumn(bar_width=28, complete_style="#8B9CFF", finished_style="#3FCB80"),
            TextColumn("{task.fields[pct]}"),
            TextColumn(tr("{task.fields[items]} 项 · {task.fields[size]} · 每秒 {task.fields[rate]} 项")),
            TimeElapsedColumn(),
            TextColumn("[#8F95A2]{task.fields[path]}"),
            console=console,
            transient=True,
        )
        with progress:
            task = progress.add_task("scan", total=None, pct="", items="0", size="0 B", rate="0", path="")

            def update(p: ScanProgress) -> None:
                path = p.current_path if len(p.current_path) < 60 else "…" + p.current_path[-58:]
                progress.update(
                    task,
                    total=1.0 if p.progress is not None else None,
                    completed=p.progress or 0,
                    pct=f"{p.progress * 100:4.1f}%" if p.progress is not None else "",
                    items=f"{p.items:,}",
                    size=fmt.size(p.bytes_alloc),
                    rate=f"{p.rate:,.0f}",
                    path=path,
                )

            try:
                result = await scanner.run(on_progress=update, progress_interval=0.1)
            except asyncio.CancelledError:
                scanner.cancel()
                raise
        return scanner, result, method
    finally:
        if low_io:
            set_low_io_priority(None, False)
        if registry is not None:
            await registry.stop()
        if pool is not None:
            pool.shutdown(wait=False, cancel_futures=True)


def scan(
    paths: Annotated[list[str], typer.Argument(help="Folders or drives to scan, e.g. C:\\ or ~/Downloads.")],
    workers: Annotated[
        int | None,
        typer.Option(
            help="Threads that hand folders to the native walker, which decides how many it lists at "
            "once (worker processes without the native extension); 0 scans in-process with the Python "
            "walker. Default: as in the app (SPACEKNIFE_SCAN_THREADS fixes both there too)."
        ),
    ] = None,
    top: Annotated[int, typer.Option(help="Rows per level in the summary.")] = 15,
    depth: Annotated[int, typer.Option(help="Levels to expand in the summary.")] = 2,
    cross_mounts: Annotated[
        bool, typer.Option(help="Descend into filesystems mounted below the roots.")
    ] = False,
    save: Annotated[Path | None, typer.Option(help="Also save the snapshot to this file.")] = None,
    no_snapshot: Annotated[bool, typer.Option("--no-snapshot", help="Do not auto-save a snapshot.")] = False,
    profile: Annotated[bool, typer.Option("--profile", help="Also show where the scan's time went.")] = False,
    disk_limit: Annotated[
        str | None,
        typer.Option(
            help="Limit the disk load as the app does: balanced (70 %), quiet (35 %), idle, max, "
            "or a percentage. Default: no limit."
        ),
    ] = None,
    use_mft: Annotated[
        bool,
        typer.Option(
            "--mft",
            help="Windows: read a whole local NTFS drive's $MFT (the app's MFT fast scan; asks for "
            "administrator rights once) instead of listing every folder. Off by default. A path it "
            "cannot read this way is scanned the usual way, with a note why.",
        ),
    ] = False,
) -> None:
    """Scan folders or drives and show where the space goes."""
    from .log import LogFormat, setup_logging

    setup_logging("WARNING", LogFormat.RICH)
    apply_language()
    limit = load_limit(disk_limit) if disk_limit is not None else None
    try:
        scanner, result, method = asyncio.run(_run_scan(paths, workers, cross_mounts, limit, use_mft))
    except KeyboardInterrupt:
        console.print(f"[yellow]{tr('已中断')}[/yellow]")
        raise typer.Exit(130) from None
    except Exception as exc:
        console.print(f"[red]{tr('无法扫描：{reason}', reason=exc)}[/red]")
        raise typer.Exit(2) from exc
    snap: Path | None = None
    if result.state == ScanState.DONE and not no_snapshot:
        from .store import snapshot  # pydantic: only when a snapshot is saved

        snap = snapshot.auto_path([r.path for r in scanner.roots])
        _save(scanner, snap, method)
        snapshot.prune(snap.parent)
    if save is not None:
        _save(scanner, save, method)
    console.print(summary_panel(scanner, result, snap, method))
    if profile:
        console.print(mft_profile_panel(method) if method.mft else profile_panel(scanner))
    store = scanner.store
    for root in scanner.roots:
        console.print(top_table(store, root.node, top, depth, tr("{path} 里最大的项目", path=root.path)))
        console.print()
    _largest_files(store, top)
    global output_complete
    # worker processes of the Python walker still stopping (shutdown without waiting): let the
    # interpreter's exit wait for them as before
    output_complete = not multiprocessing.active_children()


def profile_panel(scanner: Scanner) -> Panel:
    """Where the scan's time went: listing (system calls) or folding results into the tree."""
    p = scanner.profile()
    walker = tr("Python 遍历器") if scanner.slicer is worker.scan_slice else tr("原生遍历器")
    lines = [
        tr("{walker} · 列目录并发 {threads}", walker=walker, threads=p.threads),
        tr(
            "列目录线程平均忙碌 {busy}（系统调用；接近 100% 说明瓶颈在磁盘或文件系统）",
            busy=f"[bold]{p.busy:.0%}[/bold]",
        ),
        tr(
            "主线程合并结果 {ingest}（接近 100% 说明瓶颈在合并，加线程无用）",
            ingest=f"[bold]{p.ingest:.0%}[/bold]",
        ),
        tr("共 {slices} 批", slices=fmt.count(p.slices)),
    ]
    if p.width:
        lines.insert(1, tr("列目录宽度 {width}（同时在列的文件夹数，遍历器按读盘和速度调整）", width=p.width))
    if scanner.governor is not None:
        lines.append(
            tr("等磁盘负载上限放行 {limited}（没有任何目录在列）", limited=f"[bold]{p.limited:.0%}[/bold]")
        )
    bulk = bulk_listing()
    if bulk is not None:
        lines.append(tr("批量列目录（getattrlistbulk）：{state}", state=bulk))
    return Panel("\n".join(lines), title=tr("耗时分解"), title_align="left", border_style="#2A2E37")


def mft_profile_panel(method: ScanMethod) -> Panel:
    """Where an MFT fast scan's time went (``--mft --profile``)."""
    names = {
        "helper": tr("启动只读助手（含管理员授权）"),
        "read": tr("读取并解析 $MFT"),
        "receive": tr("接收读到的表"),
        "build": tr("建树"),
    }
    lines = [tr("MFT 快速扫描")] + [
        f"{names[key]}  [bold]{fmt.duration(seconds)}[/bold]"
        for key, seconds in method.phases.items()
        if key in names
    ]
    return Panel("\n".join(lines), title=tr("耗时分解"), title_align="left", border_style="#2A2E37")


def _largest_files(store: NodeStore, limit: int) -> None:
    kind = store.col("kind")
    alloc = store.col("alloc")
    files = np.nonzero(kind == Kind.FILE)[0]
    if not len(files):
        return
    if 0 <= limit < len(files):  # the same rows without sorting a million files (~30 ms)
        order, _ = largest(files, alloc[files], limit)
    else:
        order = files[np.argsort(-alloc[files], kind="stable")[:limit]]
    table = Table(title=tr("最大的文件"), title_justify="left", box=None, padding=(0, 1))
    table.add_column(tr("占用"), justify="right", style="bold")
    table.add_column(tr("修改于"), justify="right", style="#8F95A2")
    table.add_column(tr("路径"), overflow="fold")
    for i in order.tolist():
        v = store.view(i)
        table.add_row(fmt.size(v.alloc), fmt.age(v.mtime), store.path(i))
    console.print(table)


def _save(scanner: Scanner, path: Path, method: ScanMethod | None = None) -> None:
    from .store import snapshot

    pr = scanner.progress()
    snapshot.save(
        scanner.store,
        path,
        roots=[r.path for r in scanner.roots],
        volumes=[r.volume.to_dict() if r.volume else {} for r in scanner.roots],
        stats={
            "items": pr.items,
            "files": pr.files,
            "dirs": pr.dirs,
            "bytes_alloc": pr.bytes_alloc,
            "bytes_size": pr.bytes_size,
            "errors": pr.errors,
            "elapsed_s": pr.elapsed_s,
            "started_at": pr.started_at.isoformat() if pr.started_at else None,
            "finished_at": pr.finished_at.isoformat() if pr.finished_at else None,
            "denied": pr.denied,
            "state": pr.state.value,
            "roots": [{"path": r.path, "whole_volume": r.whole_volume} for r in scanner.roots],
            "method": "mft" if method is not None and method.mft else "hard",
        },
    )


@snapshot_app.command("list")
def snapshot_list() -> None:
    """List auto-saved snapshots, newest first."""
    from .store import snapshot

    apply_language()
    found = snapshot.list_snapshots()
    if not found:
        console.print(tr("还没有快照。先运行 spaceknife scan <路径>。"))
        return
    table = Table(box=None, padding=(0, 1))
    table.add_column(tr("时间"), no_wrap=True)
    table.add_column(tr("扫描对象"), overflow="fold", ratio=1)
    table.add_column(tr("占用"), justify="right", no_wrap=True)
    table.add_column(tr("项目"), justify="right", no_wrap=True)
    table.add_column(tr("快照文件"), style="#8F95A2", no_wrap=True)
    for path, header in found:
        table.add_row(
            header.created_at.astimezone().strftime("%Y-%m-%d %H:%M"),
            "; ".join(header.roots),
            fmt.size(int(header.stats.get("bytes_alloc", 0))),
            fmt.count(header.node_count - 1),
            path.name,
        )
    console.print(table)
    total = tr("共 {count} 个快照，位于 {folder}", count=len(found), folder=snapshots_dir())
    console.print(f"[#8F95A2]{total}[/#8F95A2]")


@snapshot_app.command("show")
def snapshot_show(
    file: Annotated[Path, typer.Argument(help="Snapshot file (.sks).")],
    top: Annotated[int, typer.Option(help="Rows per level.")] = 15,
    depth: Annotated[int, typer.Option(help="Levels to expand.")] = 2,
) -> None:
    """Show where the space went in a saved snapshot (no rescan)."""
    from .store import snapshot

    apply_language()
    try:
        store, header = snapshot.load(file)
    except (OSError, snapshot.SnapshotError) as exc:
        console.print(f"[red]{tr('无法打开快照：{reason}', reason=exc)}[/red]")
        raise typer.Exit(2) from exc
    created = header.created_at.astimezone().strftime("%Y-%m-%d %H:%M")
    console.print(
        Panel(
            Group(
                Text(
                    tr(
                        "保存于 {time} · {items} 项 · {size}",
                        time=created,
                        items=fmt.count(header.node_count - 1),
                        size=fmt.size(int(store.cols["tot_alloc"][0])),
                    )
                ),
            ),
            title=tr("快照 {name}", name=file.name),
            title_align="left",
            border_style="#2A2E37",
        )
    )
    for root in store.roots():
        console.print(top_table(store, root, top, depth, tr("{path} 里最大的项目", path=store.name(root))))
