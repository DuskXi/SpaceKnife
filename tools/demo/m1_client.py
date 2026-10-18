"""M1 体验客户端：通过协议驱动扫描核心——卷列表、扫描进度推送、目录树浏览、快照。

用法::

    uv run spaceknife serve --ready-file .demo/ready.json     # 终端 A：后端
    uv run python tools/demo/m1_client.py .demo/ready.json     # 终端 B：本客户端
"""

from __future__ import annotations

import asyncio
import json
import sys
import time
from pathlib import Path
from typing import Any

from rich.console import Console
from rich.panel import Panel
from rich.progress import BarColumn, Progress, SpinnerColumn, TextColumn, TimeElapsedColumn
from rich.prompt import Prompt
from rich.table import Table
from rich.text import Text

from spaceknife_client.connection import Connection, RemoteError
from spaceknife_protocol import scan as p
from spaceknife_protocol.base import Empty
from spaceknife_protocol.methods import (
    APP_SHUTDOWN,
    EVENT_SCAN_PROGRESS,
    SCAN_LIST,
    SCAN_START,
    SNAPSHOT_LIST,
    SNAPSHOT_LOAD,
    TREE_CHILDREN,
    TREE_NODE,
    VOLUME_LIST,
)
from spaceknife_protocol.session import ClientInfo

console = Console()
CLIENT = ClientInfo(name="m1-demo", version="0.1.0", kind="cli")
BASE = 1000 if sys.platform == "darwin" else 1024


def size(n: int) -> str:
    v, units = float(n), ["B", "KB", "MB", "GB", "TB"]
    i = 0
    while v >= BASE and i < len(units) - 1:
        v /= BASE
        i += 1
    return f"{int(v)} B" if i == 0 else f"{v:.{0 if v >= 100 else 1}f} {units[i]}"


def bar(frac: float, width: int = 14) -> Text:
    k = max(0, min(width, round(frac * width)))
    return Text("█" * k, style="#8B9CFF") + Text("░" * (width - k), style="#2A2E37")


def load_ready(path: Path) -> dict[str, Any]:
    for _ in range(100):
        if path.exists():
            data: dict[str, Any] = json.loads(path.read_text(encoding="utf-8"))
            return data
        time.sleep(0.1)
    console.print(f"[red]找不到 {path}，请先在另一个终端启动后端。[/red]")
    sys.exit(1)


async def volumes(conn: Connection) -> None:
    res = await conn.call(VOLUME_LIST, Empty())
    t = Table(title="卷（W1 启动页的数据源）", title_justify="left", box=None, padding=(0, 1))
    for col in ("卷", "名称", "文件系统", "总容量", "已用", "", "可用", "类型"):
        t.add_column(col, justify="right" if col in ("总容量", "已用", "可用") else "left")
    for v in res.volumes:
        kind = "网络" if v.is_remote else ("可移动" if v.is_removable else "本地")
        t.add_row(
            v.mount_point,
            v.label or "-",
            v.fs_type or "?",
            size(v.total),
            size(v.used),
            bar(v.used / v.total if v.total else 0),
            size(v.free),
            kind,
        )
    console.print(t)


async def run_scan(conn: Connection) -> str | None:
    default = str(Path.home())
    target = Prompt.ask("要扫描的路径（整盘写 C:\\ 会显示可用/未知空间与百分比）", default=default)
    try:
        status = await conn.call(SCAN_START, p.ScanStartParams(roots=[target]))
    except RemoteError as exc:
        console.print(f"[red]无法开始：{exc.message}[/red]")
        return None
    done: asyncio.Future[p.ScanStatus] = asyncio.get_running_loop().create_future()
    progress = Progress(
        SpinnerColumn(style="#8B9CFF"),
        TextColumn("[bold]扫描中[/bold]"),
        BarColumn(bar_width=26, complete_style="#8B9CFF"),
        TextColumn("{task.fields[pct]}"),
        TextColumn("{task.fields[items]} 项 · {task.fields[size]} · 每秒 {task.fields[rate]} 项"),
        TimeElapsedColumn(),
        TextColumn("[#8F95A2]{task.fields[path]}"),
        console=console,
        transient=True,
    )
    task = progress.add_task("s", total=None, pct="", items="0", size="0 B", rate="0", path="")
    events = 0

    def on_event(ev: p.ScanProgressEvent) -> None:
        nonlocal events
        s = ev.status
        if s.scan_id != status.scan_id:
            return
        events += 1
        path = s.current_path if len(s.current_path) < 56 else "…" + s.current_path[-54:]
        progress.update(
            task,
            total=1.0 if s.progress is not None else None,
            completed=s.progress or 0,
            pct=f"{s.progress * 100:4.1f}%" if s.progress is not None else "",
            items=f"{s.items:,}",
            size=size(s.bytes_alloc),
            rate=f"{s.rate:,.0f}",
            path=path,
        )
        if (
            s.state in ("done", "cancelled", "failed")
            and (s.snapshot or s.state != "done")
            and not done.done()
        ):
            done.set_result(s)

    conn.on_event(EVENT_SCAN_PROGRESS, on_event)
    sub = await conn.subscribe(p.TOPIC_SCAN, p.ScanTopicParams(scan_id=status.scan_id))
    with progress:
        final = await done
    await conn.unsubscribe(sub)
    lines = [
        f"状态 {final.state} · 用时 {final.elapsed_s:.1f} 秒 · 每秒 {final.rate:,.0f} 项 · 收到进度推送 {events} 次",
        f"{final.items:,} 项（文件 {final.files:,} · 文件夹 {final.dirs:,}）· 占用 {size(final.bytes_alloc)}",
    ]
    if final.free is not None and final.unknown is not None:
        lines.append(f"可用空间 {size(final.free)} · 未知空间 {size(final.unknown)}")
    if final.errors:
        lines.append(f"[#F5C542]{final.errors} 个文件夹无法访问，例如 {final.denied[0]}[/#F5C542]")
    if final.snapshot:
        lines.append(f"[#8F95A2]已自动保存快照：{final.snapshot}[/#8F95A2]")
    console.print(
        Panel("\n".join(lines), title=f"扫描 {final.scan_id}", title_align="left", border_style="#2A2E37")
    )
    return status.scan_id


async def browse(conn: Connection, scan_id: str) -> None:
    status = (await conn.call(SCAN_LIST, Empty())).scans
    match = next((s for s in status if s.scan_id == scan_id), None)
    if match is None:
        console.print("[red]找不到这个扫描[/red]")
        return
    node = match.roots[0].node
    stack: list[int] = []
    while True:
        res = await conn.call(TREE_CHILDREN, p.TreeChildrenParams(scan_id=scan_id, node=node, limit=20))
        path = (await conn.call(TREE_NODE, p.TreeNodeParams(scan_id=scan_id, node=node))).path
        parent = res.parent
        t = Table(
            title=f"{path}  ·  {size(parent.alloc)} · {parent.files:,} 个文件",
            title_justify="left",
            box=None,
            padding=(0, 1),
        )
        t.add_column("#", justify="right", style="#8F95A2")
        t.add_column("名称", max_width=48, overflow="ellipsis", no_wrap=True)
        t.add_column("占用", justify="right", style="bold")
        t.add_column("占比", justify="right")
        t.add_column("")
        t.add_column("类型", style="#8F95A2")
        for i, item in enumerate(res.items, 1):
            frac = item.alloc / parent.alloc if parent.alloc else 0
            name = Text(
                ("▸ " if item.kind == "dir" else "  ") + item.name, style="bold" if item.kind == "dir" else ""
            )
            if "access_denied" in item.flags:
                name.append("  无法访问", style="#F5C542")
            t.add_row(str(i), name, size(item.alloc), f"{frac * 100:.1f}%", bar(frac), item.ext or item.kind)
        if res.total > len(res.items):
            t.add_row("", Text(f"…其余 {res.total - len(res.items):,} 项", style="#8F95A2"), "", "", "", "")
        console.print(t)
        choice = Prompt.ask("输入编号进入文件夹，.. 返回上一级，q 退出浏览", default="q")
        if choice == "q":
            return
        if choice == "..":
            if stack:
                node = stack.pop()
            continue
        if choice.isdigit() and 1 <= int(choice) <= len(res.items):
            item = res.items[int(choice) - 1]
            if item.kind == "dir" and item.child_count:
                stack.append(node)
                node = item.id
            else:
                console.print("[#8F95A2]这不是一个可以进入的文件夹[/#8F95A2]")


async def snapshots(conn: Connection) -> str | None:
    res = await conn.call(SNAPSHOT_LIST, Empty())
    if not res.snapshots:
        console.print("还没有快照，先做一次扫描。")
        return None
    t = Table(title="快照（每次扫描完成自动保存）", title_justify="left", box=None, padding=(0, 1))
    for col in ("#", "时间", "扫描对象", "占用", "文件数", "快照大小"):
        t.add_column(col)
    for i, s in enumerate(res.snapshots, 1):
        t.add_row(
            str(i),
            s.created_at.astimezone().strftime("%m-%d %H:%M:%S"),
            "; ".join(s.roots),
            size(s.bytes_alloc),
            f"{s.files:,}",
            size(s.file_bytes),
        )
    console.print(t)
    choice = Prompt.ask("输入编号打开快照（不重新扫描），回车跳过", default="")
    if choice.isdigit() and 1 <= int(choice) <= len(res.snapshots):
        t0 = time.perf_counter()
        status = await conn.call(
            SNAPSHOT_LOAD, p.SnapshotLoadParams(path=res.snapshots[int(choice) - 1].path)
        )
        console.print(
            f"已打开为 {status.scan_id}（{status.items:,} 项，用时 {(time.perf_counter() - t0) * 1000:.0f} 毫秒）"
        )
        return status.scan_id
    return None


async def session(ready: dict[str, Any]) -> None:
    async with await Connection.open(ready["url"], ready["token"], CLIENT) as conn:
        current: str | None = None
        while True:
            console.print()
            console.print("  [bold]1[/bold]  卷列表")
            console.print("  [bold]2[/bold]  开始扫描（实时进度推送）")
            console.print(
                "  [bold]3[/bold]  浏览扫描结果"
                + (f"（当前 {current}）" if current else "（先扫描或打开快照）")
            )
            console.print("  [bold]4[/bold]  快照列表 / 打开快照")
            console.print("  [bold]5[/bold]  关闭后端")
            console.print("  [bold]0[/bold]  退出")
            choice = Prompt.ask("选择", choices=["1", "2", "3", "4", "5", "0"], default="2")
            console.rule()
            if choice == "0":
                return
            if choice == "1":
                await volumes(conn)
            elif choice == "2":
                current = await run_scan(conn) or current
            elif choice == "3":
                if current:
                    await browse(conn, current)
            elif choice == "4":
                current = await snapshots(conn) or current
            elif choice == "5":
                await conn.call(APP_SHUTDOWN, Empty())
                console.print("已请求关闭，请看终端 A。")
                return


def main() -> None:
    ready = load_ready(Path(sys.argv[1] if len(sys.argv) > 1 else ".demo/ready.json"))
    console.print(
        Panel.fit(f"已找到后端：{ready['url']}（pid {ready['pid']}）", title="SpaceKnife M1 体验客户端")
    )
    try:
        asyncio.run(session(ready))
    except (OSError, ConnectionError) as exc:
        console.print(f"[red]连接失败：{exc}[/red]")


if __name__ == "__main__":
    main()
