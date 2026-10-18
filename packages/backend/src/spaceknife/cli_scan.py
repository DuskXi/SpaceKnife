"""``spaceknife scan`` and ``spaceknife snapshot …``: terminal front end for the scan core."""

from __future__ import annotations

import asyncio
import os
from pathlib import Path
from typing import Annotated

import numpy as np
import typer
from rich.console import Console, Group
from rich.panel import Panel
from rich.progress import BarColumn, Progress, SpinnerColumn, TextColumn, TimeElapsedColumn
from rich.table import Table
from rich.text import Text

from . import fmt, i18n
from .i18n import tr
from .paths import snapshots_dir
from .scan.scanner import Scanner, ScanProgress, ScanState, default_workers, make_pool
from .store import snapshot
from .store.nodes import Flag, Kind, NodeStore

console = Console()
snapshot_app = typer.Typer(help="Inspect saved snapshots.", no_args_is_help=True)

KIND_ICON = {Kind.DIR: "▸", Kind.FILE: " ", Kind.LINK: "↪", Kind.OTHER: "·"}


def apply_language() -> None:
    """The interface language of a command-line program (M7.5): the user's setting
    (``general.language``) when the settings file can be read, else the system's."""
    setting = "system"
    if not os.environ.get("SPACEKNIFE_LANGUAGE"):  # forced (tests): no need to read the settings
        try:
            from .settings import SettingsStore

            setting = str(SettingsStore().get("general.language"))
        except Exception:  # an unreadable settings file must not stop the command
            setting = "system"
    i18n.set_language(i18n.resolve(setting))


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


def summary_panel(scanner: Scanner, result: ScanProgress, snap: Path | None) -> Panel:
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
    if any(
        r.volume and r.volume.fs_type.upper() == "NTFS" for r in scanner.roots
    ):  # hard scan on Windows: explain the accounting limits honestly
        note = tr(
            "说明：这是硬扫描。NTFS 的硬链接按每个链接计入，占用按簇推算；MFT 快速扫描（M4）会给出精确值。"
        )
        lines.append(f"[#8F95A2]{note}[/#8F95A2]")
    return Panel("\n".join(lines), title=tr("扫描结果"), title_align="left", border_style="#2A2E37")


async def _run_scan(paths: list[str], workers: int, cross_mounts: bool) -> tuple[Scanner, ScanProgress]:
    pool = make_pool(workers) if workers > 0 else None
    try:
        scanner = Scanner(paths, executor=pool, concurrency=max(1, workers), cross_mounts=cross_mounts)
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
        return scanner, result
    finally:
        if pool is not None:
            pool.shutdown(wait=False, cancel_futures=True)


def scan(
    paths: Annotated[list[str], typer.Argument(help="Folders or drives to scan, e.g. C:\\ or ~/Downloads.")],
    workers: Annotated[
        int | None, typer.Option(help="Worker processes; 0 scans in-process. Default: CPU count − 1 (max 8).")
    ] = None,
    top: Annotated[int, typer.Option(help="Rows per level in the summary.")] = 15,
    depth: Annotated[int, typer.Option(help="Levels to expand in the summary.")] = 2,
    cross_mounts: Annotated[
        bool, typer.Option(help="Descend into filesystems mounted below the roots.")
    ] = False,
    save: Annotated[Path | None, typer.Option(help="Also save the snapshot to this file.")] = None,
    no_snapshot: Annotated[bool, typer.Option("--no-snapshot", help="Do not auto-save a snapshot.")] = False,
) -> None:
    """Scan folders or drives and show where the space goes."""
    from .log import LogFormat, setup_logging

    setup_logging("WARNING", LogFormat.RICH)
    apply_language()
    n_workers = default_workers() if workers is None else workers
    try:
        scanner, result = asyncio.run(_run_scan(paths, n_workers, cross_mounts))
    except KeyboardInterrupt:
        console.print(f"[yellow]{tr('已中断')}[/yellow]")
        raise typer.Exit(130) from None
    except Exception as exc:
        console.print(f"[red]{tr('无法扫描：{reason}', reason=exc)}[/red]")
        raise typer.Exit(2) from exc
    snap: Path | None = None
    if result.state == ScanState.DONE and not no_snapshot:
        snap = snapshot.auto_path([r.path for r in scanner.roots])
        _save(scanner, snap)
        snapshot.prune(snap.parent)
    if save is not None:
        _save(scanner, save)
    console.print(summary_panel(scanner, result, snap))
    store = scanner.store
    for root in scanner.roots:
        console.print(top_table(store, root.node, top, depth, tr("{path} 里最大的项目", path=root.path)))
        console.print()
    _largest_files(store, top)


def _largest_files(store: NodeStore, limit: int) -> None:
    kind = store.col("kind")
    alloc = store.col("alloc")
    files = np.nonzero(kind == Kind.FILE)[0]
    if not len(files):
        return
    order = files[np.argsort(-alloc[files], kind="stable")[:limit]]
    table = Table(title=tr("最大的文件"), title_justify="left", box=None, padding=(0, 1))
    table.add_column(tr("占用"), justify="right", style="bold")
    table.add_column(tr("修改于"), justify="right", style="#8F95A2")
    table.add_column(tr("路径"), overflow="fold")
    for i in order.tolist():
        v = store.view(i)
        table.add_row(fmt.size(v.alloc), fmt.age(v.mtime), store.path(i))
    console.print(table)


def _save(scanner: Scanner, path: Path) -> None:
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
        },
    )


@snapshot_app.command("list")
def snapshot_list() -> None:
    """List auto-saved snapshots, newest first."""
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
