"""M2 体验台启动器：后端 + 静态页面，一条命令。

    uv run python tools/demo/m2_demo.py [--port 0] [--path C:\\Users]

启动后端（``--token-stdin``，随本进程退出而退出），在 127.0.0.1 上提供 ``tools/demo/m2`` 页面和
``design/build`` 令牌，并打印带 token 的地址。数据目录用 ``SPACEKNIFE_HOME``（未设置时为仓库外的
``SpaceKnife-demos/M2/.home``），不会碰真实的用户数据。
"""

from __future__ import annotations

import argparse
import functools
import http.server
import json
import os
import secrets
import subprocess
import sys
import threading
from pathlib import Path
from urllib.parse import quote

from rich.console import Console
from rich.panel import Panel

ROOT = Path(__file__).resolve().parents[2]
console = Console()


class _Quiet(http.server.SimpleHTTPRequestHandler):
    def log_message(self, format: str, *args: object) -> None:
        pass

    def end_headers(self) -> None:
        self.send_header("Cache-Control", "no-store")
        super().end_headers()


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=0, help="页面端口；0 表示自动选一个空闲端口")
    ap.add_argument("--path", default="", help="预填的扫描路径")
    args = ap.parse_args()
    in_demo_copy = ROOT.parent.name == "SpaceKnife-demos"
    default_home = ROOT / ".home" if in_demo_copy else ROOT.parent / "SpaceKnife-demos" / "M2" / ".home"
    home = os.environ.get("SPACEKNIFE_HOME") or str(default_home)
    env = {**os.environ, "SPACEKNIFE_HOME": home, "PYTHONIOENCODING": "utf-8"}
    handler = functools.partial(_Quiet, directory=str(ROOT))
    httpd = http.server.ThreadingHTTPServer(("127.0.0.1", args.port), handler)  # bind first: we need the port
    origin = f"http://127.0.0.1:{httpd.server_address[1]}"
    token = secrets.token_urlsafe(24)
    backend = subprocess.Popen(
        [
            sys.executable,
            "-X",
            "utf8",
            "-m",
            "spaceknife",
            "serve",
            "--token-stdin",
            "--allow-origin",
            origin,
        ],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        env=env,
        cwd=ROOT,
    )
    assert backend.stdin is not None
    assert backend.stdout is not None
    backend.stdin.write((token + "\n").encode())
    backend.stdin.flush()
    ready = json.loads(backend.stdout.readline())
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    url = (
        f"{origin}/tools/demo/m2/index.html#ws={quote(ready['url'])}&token={quote(token)}"
        f"&path={quote(args.path)}"
    )
    console.print(
        Panel.fit(
            f"后端 {ready['url']}（pid {ready['pid']}）\n数据目录 {home}\n\n[bold]{url}[/bold]\n\n按 Ctrl+C 结束。",
            title="SpaceKnife M2 体验台",
        )
    )
    (Path(home) / "url.txt").parent.mkdir(parents=True, exist_ok=True)
    (Path(home) / "url.txt").write_text(url, encoding="utf-8")
    try:
        backend.wait()
    except KeyboardInterrupt:
        pass
    finally:
        httpd.shutdown()
        if backend.poll() is None:
            backend.stdin.close()  # stdin EOF: the backend shuts down gracefully
            try:
                backend.wait(timeout=10)
            except subprocess.TimeoutExpired:
                backend.kill()
    return 0


if __name__ == "__main__":
    sys.exit(main())
