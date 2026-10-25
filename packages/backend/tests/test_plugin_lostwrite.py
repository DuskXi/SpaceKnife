"""A file written right after a scan finished reaches the tree and the plugins' ``tree_changed``
hook (round 5's probe ``lostwrite``, docs/plugins.md 「第五轮之后的收尾」).

Linux used to lose it: inotify watches each folder on its own, and they are placed a little after
the scan listed the folder; a file created in between stayed out of the tree until the folder
changed again. Since the merge of dev (a16374e) every folder is checked against the mtime the scan
recorded when its watch is placed (``monitor.poll.listed_changed``) and listed again if it moved;
that listing goes through ``views.flash`` like any other change, so the hook hears of it too.
Windows and macOS watch the whole tree from the scan's start: the same test holds there."""

from __future__ import annotations

import asyncio
import time
from collections.abc import Awaitable, Callable
from pathlib import Path
from typing import Any

import pytest

from spaceknife_sdk.testing import MockHost

pytestmark = pytest.mark.timeout(240)

TOML = """id = "t.lostwrite"
version = "0.1.0"
name = { zh = "扫描后的改动", en = "Changes after a scan" }
entry = "plugin.py"
permissions = ["tree.read"]
"""

CODE = """from __future__ import annotations

from typing import Any

from spaceknife_sdk import Context, Payload, Plugin, TreeChange

plugin = Plugin()
SEEN: list[dict[str, Any]] = []


class Seen(Payload):
    seen: list[dict[str, Any]]


@plugin.method
def seen(ctx: Context) -> Seen:
    return Seen(seen=SEEN)


@plugin.on("tree_changed")
def tree_changed(ctx: Context, change: TreeChange) -> None:
    SEEN.append({"scan_id": change.scan_id, "paths": change.paths, "after": change.after})
"""

FOLDERS = 30  # d<i>/sub/deep each: 91 folders with the root, as the probe had


@pytest.fixture
def plugin(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    monkeypatch.setenv("SPACEKNIFE_HOME", str(tmp_path / "home"))
    folder = tmp_path / "t.lostwrite"
    folder.mkdir()
    (folder / "plugin.toml").write_text(TOML, encoding="utf-8")
    (folder / "plugin.py").write_text(CODE, encoding="utf-8")
    return folder


async def _until(cond: Callable[[], Awaitable[object]], limit: float) -> None:
    end = time.monotonic() + limit
    while not await cond():
        if time.monotonic() > end:
            raise AssertionError("timed out")
        await asyncio.sleep(0.1)


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
async def test_a_file_written_right_after_the_scan_reaches_the_tree_and_the_hook(
    plugin: Path, tmp_path: Path, process: bool
) -> None:
    root = tmp_path / "lost"
    for i in range(FOLDERS):
        deep = root / f"d{i}" / "sub" / "deep"
        deep.mkdir(parents=True)
        (deep / "f.bin").write_bytes(b"x" * 1000)
    async with MockHost.load(plugin, process=process) as host:
        sid = await host.scan_dir(root)
        # at once: the monitor has not placed the watches of most folders yet
        (root / f"d{FOLDERS - 1}" / "sub" / "deep" / "late.bin").write_bytes(b"l" * 500_000)
        late = f"d{FOLDERS - 1}/sub/deep/late.bin"

        async def in_tree() -> bool:
            try:
                host.find(late)
            except KeyError:
                return False
            return True

        await _until(in_tree, 20.0)
        assert host.node(late).size == 500_000
        deep_path = str(root / f"d{FOLDERS - 1}" / "sub" / "deep")
        seen: list[dict[str, Any]] = []

        async def heard() -> bool:
            seen[:] = (await host.call("seen"))["seen"]
            return any(deep_path in s["paths"] for s in seen)

        await _until(heard, 20.0)
        change = next(s for s in seen if deep_path in s["paths"])
        assert change["scan_id"] == sid
        assert change["after"][change["paths"].index(deep_path)] >= 500_000
