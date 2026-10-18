"""M0 体验客户端：连接一个正在运行的 ``spaceknife serve``，交互式地演示协议 v1。

用法::

    uv run spaceknife serve --ready-file .demo/ready.json      # 终端 A：后端
    uv run python tools/demo/m0_client.py .demo/ready.json      # 终端 B：本客户端
"""

from __future__ import annotations

import asyncio
import json
import statistics
import sys
import time
from pathlib import Path
from typing import Any

import numpy as np
from rich.console import Console
from rich.panel import Panel
from rich.prompt import Prompt
from rich.table import Table
from websockets.asyncio.client import connect
from websockets.exceptions import ConnectionClosed, InvalidStatus
from websockets.typing import Origin, Subprotocol

from spaceknife_client.connection import Connection, RemoteError
from spaceknife_protocol import PROTOCOL_VERSION, SUBPROTOCOL
from spaceknife_protocol.base import Empty
from spaceknife_protocol.binary import decode_frame, encode_frame
from spaceknife_protocol.methods import APP_SHUTDOWN, EVENT_LOG, SYSTEM_INFO, SYSTEM_PING
from spaceknife_protocol.session import ClientInfo
from spaceknife_protocol.streams import TOPIC_LOG, LogRecordEvent, LogTopicParams
from spaceknife_protocol.system import PingParams

console = Console()
CLIENT = ClientInfo(name="m0-demo", version="0.1.0", kind="cli")
LEVEL_STYLE = {"DEBUG": "cyan", "INFO": "blue", "SUCCESS": "green", "WARNING": "yellow", "ERROR": "red"}


def load_ready(path: Path) -> dict[str, Any]:
    for _ in range(100):
        if path.exists():
            data: dict[str, Any] = json.loads(path.read_text(encoding="utf-8"))
            return data
        time.sleep(0.1)
    console.print(f"[red]找不到 {path}，请先在另一个终端启动后端。[/red]")
    sys.exit(1)


async def show_handshake(ready: dict[str, Any]) -> None:
    async with await Connection.open(ready["url"], ready["token"], CLIENT) as conn:
        h = conn.hello
        info = await conn.call(SYSTEM_INFO, Empty())
        table = Table(title="握手结果（session.hello）", show_header=False, title_justify="left")
        table.add_row("协议版本", h.protocol)
        table.add_row(
            "后端", f"{h.server.name} {h.server.version}（Python {h.server.python}，{h.server.platform}）"
        )
        table.add_row("会话 id", h.session_id)
        table.add_row("协商出的能力", ", ".join(h.capabilities))
        table.add_row("schema 指纹", h.schema_hash)
        table.add_row("入站消息上限", f"{h.limits.max_inbound_message_bytes:,} 字节")
        table.add_row("握手超时", f"{h.limits.hello_timeout_s} 秒")
        table.add_row("后端进程", f"pid {info.pid}，已连接会话 {info.sessions} 个（含本次）")
        table.add_row("平台", f"{info.platform.system} {info.platform.release} {info.platform.machine}")
        console.print(table)


async def latency(ready: dict[str, Any]) -> None:
    async with await Connection.open(ready["url"], ready["token"], CLIENT) as conn:
        samples = []
        for i in range(1000):
            t0 = time.perf_counter()
            await conn.call(SYSTEM_PING, PingParams(payload=str(i)))
            samples.append((time.perf_counter() - t0) * 1000)
        samples.sort()
        t0 = time.perf_counter()
        await asyncio.gather(*(conn.call(SYSTEM_PING, PingParams(payload="x")) for _ in range(1000)))
        burst = time.perf_counter() - t0
    table = Table(title="往返延迟（1000 次 system.ping，逐个发送）", title_justify="left")
    for col in ("p50", "p95", "p99", "最大"):
        table.add_column(col, justify="right")
    pick = lambda q: samples[min(len(samples) - 1, int(q * len(samples)))]  # noqa: E731
    table.add_row(*(f"{v:.3f} ms" for v in (statistics.median(samples), pick(0.95), pick(0.99), samples[-1])))
    console.print(table)
    console.print(f"并发 1000 个请求共用 {burst * 1000:.1f} ms，约 {1000 / burst:,.0f} 次/秒。")


async def stream_logs(ready: dict[str, Any], seconds: float = 12.0) -> None:
    async with await Connection.open(ready["url"], ready["token"], CLIENT) as conn:
        got = 0

        def show(ev: LogRecordEvent) -> None:
            nonlocal got
            got += 1
            style = LEVEL_STYLE.get(ev.level, "")
            stamp = ev.time.strftime("%H:%M:%S.%f")[:-3]
            console.print(
                f"[dim]{stamp}[/dim] [{style}]{ev.level:<7}[/{style}] [magenta]{ev.source}[/magenta]  {ev.message}"
            )

        conn.on_event(EVENT_LOG, show)
        sub = await conn.subscribe(TOPIC_LOG, LogTopicParams(min_level="DEBUG", backlog=5))
        console.print(
            f"[bold]已订阅 log 主题（{sub}）。先回放最近 5 条，然后每 2 秒开关一个额外会话制造日志，持续 {seconds:.0f} 秒…[/bold]"
        )
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            extra = await Connection.open(
                ready["url"], ready["token"], ClientInfo(name="log-trigger", version="0", kind="test")
            )
            await extra.close()
            await asyncio.sleep(2)
        await conn.unsubscribe(sub)
        console.print(f"共收到 {got} 条推送日志。终端 A 里的后端日志应该能和这里一一对上。")


async def _closed_code(ws_ctx: Any, send: str | None) -> str:
    try:
        async with ws_ctx as ws:
            if send is not None:
                await ws.send(send)
                try:
                    reply = json.loads(await ws.recv())
                    err = reply.get("error", {})
                    detail = f"错误 {err.get('code')}「{err.get('message')}」"
                except ConnectionClosed as exc:
                    return f"连接关闭 {exc.rcvd.code if exc.rcvd else '?'}"
            else:
                detail = "未发送任何消息"
            try:
                await ws.recv()
            except ConnectionClosed as exc:
                return f"{detail} → 关闭码 {exc.rcvd.code if exc.rcvd else '?'}"
            return f"{detail} → 连接仍开着（不应如此）"
    except InvalidStatus as exc:
        return f"HTTP {exc.response.status_code} 拒绝升级"


def _hello(token: str, protocol: str = PROTOCOL_VERSION) -> str:
    return json.dumps(
        {
            "jsonrpc": "2.0",
            "id": 0,
            "method": "session.hello",
            "params": {
                "token": token,
                "protocol": protocol,
                "client": {"name": "probe", "version": "0", "kind": "test"},
            },
        }
    )


async def security(ready: dict[str, Any]) -> None:
    url, token = ready["url"], ready["token"]
    sp = [Subprotocol(SUBPROTOCOL)]
    cases = [
        ("错误的 token", connect(url, subprotocols=sp), _hello("x" * 32), "错误 -32001 → 关闭码 4401"),
        (
            "主版本不匹配（2.0）",
            connect(url, subprotocols=sp),
            _hello(token, "2.0"),
            "错误 -32003 → 关闭码 4426",
        ),
        (
            "先发其他请求",
            connect(url, subprotocols=sp),
            '{"jsonrpc":"2.0","id":1,"method":"system.info"}',
            "错误 -32002 → 关闭码 4401",
        ),
        ("5 秒内不发 hello", connect(url, subprotocols=sp), None, "关闭码 4408"),
        ("错误路径 /other", connect(url.removesuffix("/v1") + "/other", subprotocols=sp), None, "HTTP 404"),
        ("不带子协议", connect(url), None, "HTTP 400"),
        (
            "外部网页 Origin",
            connect(url, subprotocols=sp, origin=Origin("https://evil.example")),
            None,
            "HTTP 403",
        ),
    ]
    table = Table(title="安全检查（每项都应被拒绝）", title_justify="left")
    table.add_column("尝试")
    table.add_column("期望")
    table.add_column("实际")
    with console.status("逐项探测中（「不发 hello」要等 5 秒）…"):
        for name, ctx, send, expect in cases:
            got = await _closed_code(ctx, send)
            ok = all(part in got for part in expect.replace("→", "").split())
            table.add_row(name, expect, f"[{'green' if ok else 'red'}]{got}[/]")
    console.print(table)
    try:
        await Connection.open(url, token, CLIENT, origin="app://spaceknife")
        console.print("[green]对照：Electron 的 app://spaceknife Origin + 正确 token 可以正常连接。[/green]")
    except RemoteError as exc:
        console.print(f"[red]对照连接失败：{exc}[/red]")


def binary_codec() -> None:
    n = 200_000
    cols = {
        "id": np.arange(n, dtype=np.uint32),
        "x": np.random.default_rng(0).random(n, dtype=np.float32),
        "y": np.random.default_rng(1).random(n, dtype=np.float32),
        "w": np.random.default_rng(2).random(n, dtype=np.float32),
        "h": np.random.default_rng(3).random(n, dtype=np.float32),
        "color": np.arange(n, dtype=np.uint32),
    }
    t0 = time.perf_counter()
    frame = encode_frame(stream="drawlist", layout="rect.v1", columns=cols)
    t1 = time.perf_counter()
    header, out = decode_frame(frame)
    t2 = time.perf_counter()
    assert all(np.array_equal(out[k], v) for k, v in cols.items())
    mb = len(frame) / 1e6
    console.print(
        Panel.fit(
            f"{n:,} 个矩形 × 6 列 = {mb:.1f} MB 一帧\n"
            f"编码 {(t1 - t0) * 1000:.1f} ms（{mb / (t1 - t0):,.0f} MB/s）  "
            f"解码 {(t2 - t1) * 1000:.3f} ms（零拷贝视图，只解析帧头）\n"
            f"帧头：stream={header.stream} layout={header.layout} count={header.count:,}",
            title="二进制帧（树图绘制列表的数据通道）",
        )
    )


async def shutdown(ready: dict[str, Any]) -> None:
    async with await Connection.open(ready["url"], ready["token"], CLIENT) as conn:
        result = await conn.call(APP_SHUTDOWN, Empty())
    console.print(
        f"app.shutdown → accepted={result.accepted}。请看终端 A：后端应在 1 秒内打印 shutdown 并退出。"
    )


MENU = {
    "1": ("握手信息", show_handshake),
    "2": ("延迟测试", latency),
    "3": ("实时日志推送（W8 日志面板的数据源）", stream_logs),
    "4": ("安全检查", security),
    "5": ("二进制帧编解码", None),
    "6": ("关闭后端（app.shutdown）", shutdown),
}


def main() -> None:
    ready_path = Path(sys.argv[1] if len(sys.argv) > 1 else ".demo/ready.json")
    ready = load_ready(ready_path)
    console.print(
        Panel.fit(f"已找到后端：{ready['url']}（pid {ready['pid']}）", title="SpaceKnife M0 体验客户端")
    )
    while True:
        console.print()
        for key, (label, _) in MENU.items():
            console.print(f"  [bold]{key}[/bold]  {label}")
        console.print("  [bold]0[/bold]  退出")
        choice = Prompt.ask("选择", choices=[*MENU, "0"], default="1")
        if choice == "0":
            return
        label, action = MENU[choice]
        console.rule(label)
        try:
            if action is None:
                binary_codec()
            else:
                asyncio.run(action(ready))
        except (OSError, ConnectionError) as exc:
            console.print(f"[red]连接失败：{exc}（后端是否已关闭？）[/red]")
        if choice == "6":
            return


if __name__ == "__main__":
    main()
