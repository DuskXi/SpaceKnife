"""Round 4, scan sources that are whole (docs/plugins.md 「第三轮之后的改进」, gap 7): the source's
``ctx.progress`` is the scan's progress, what it listed so far is shown while it lists, a failure
keeps its reason, and a ``Listing`` carries what a disk scan records (kind, attributes, a file id
for hard links counted once by the host, a folder's own size; size and alloc both kept, the views
measuring by size for ``size = "size"``); ``ScanInfo`` carries the source and its URI."""

from __future__ import annotations

import asyncio
from pathlib import Path
from typing import Any

import numpy as np
import pytest

from spaceknife.plugins import sources
from spaceknife.store.nodes import Flag, Kind
from spaceknife_sdk.testing import MockHost

pytestmark = pytest.mark.timeout(240)

TOML = """id = "t.r4src"
version = "0.1.0"
name = { zh = "第四轮来源", en = "Round four sources" }
entry = "plugin.py"
permissions = ["tree.read"]
"""

CODE = """from __future__ import annotations

import time

from spaceknife_sdk import Context, Listing, Payload, Plugin, PluginError, ScanInfo, T

plugin = Plugin()


@plugin.scan_source("list", T("清单", "List"))
def listing(ctx: Context, uri: str, out: Listing) -> None:
    if uri == "bad":
        out.file("one.bin", 10)
        raise PluginError(T("不是 ncdu 导出：第 2 项之后，约第 239 字节", "not an ncdu export: after 2 entries, near byte 239"))
    if uri == "slow":
        for i in range(6):
            for j in range(10):
                out.file(f"b{i}/f{j}.bin", 100)
            out.flush()
            ctx.progress(i + 1, 6, T("第 {n} 批", "batch {n}").format(n=i + 1))
            time.sleep(0.3)
        out.root_name("slow")
        return
    out.dir("d", mtime=1_700_000_000, size=4096, alloc=4096, attrs=["hidden"])
    out.file("d/a.bin", 1000, 4096, ino=7, nlink=2, dev=1)
    out.file("e/b.bin", 1000, 4096, ino=7, nlink=2, dev=1)  # the same file again: counted once
    out.file("e/c.bin", 500, 4096, ino=8, nlink=2, dev=2)  # another device: another file
    out.file("l", 0, kind="link")
    out.file("p", 0, kind="other")
    out.file("r.txt", 10, 10, attrs=["readonly", "hidden"])
    out.root_name("whole")


@plugin.scan_source("packed", T("压缩包", "Archive"), size="size")
def packed(ctx: Context, uri: str, out: Listing) -> None:
    out.file("zero.bin", 30_000_000, 31_000)  # a zip bomb: tiny packed, big unpacked
    out.file("small.txt", 100, 80)


class Info(Payload):
    info: dict


@plugin.method
def info(ctx: Context) -> Info:
    s = next(x for x in ctx.scans() if x.front)
    return Info(info=s.model_dump())
"""


@pytest.fixture
def plugin(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    monkeypatch.setenv("SPACEKNIFE_MOCK_TREES", str(tmp_path / "trees"))
    monkeypatch.setenv("SPACEKNIFE_HOME", str(tmp_path / "home"))
    folder = tmp_path / "t.r4src"
    folder.mkdir()
    (folder / "plugin.toml").write_text(TOML, encoding="utf-8")
    (folder / "plugin.py").write_text(CODE, encoding="utf-8")
    return folder


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
async def test_a_listing_carries_what_a_disk_scan_records(plugin: Path, process: bool) -> None:
    async with MockHost.load(plugin, process=process) as host:
        await host.source("list", "whole")
        s = host.store
        c = s.cols
        d, a, b, e = host.find("d"), host.find("d/a.bin"), host.find("e/b.bin"), host.find("e/c.bin")
        # hard links: the first name in tree order counts, the other adds nothing (as a disk scan)
        assert not c["flags"][a] & Flag.HARDLINK_DUP and c["flags"][b] & Flag.HARDLINK_DUP
        assert not c["flags"][e] & Flag.HARDLINK_DUP  # same file id on another device
        assert int(c["ino"][a]) == 7 and int(c["nlink"][a]) == 2
        # a folder's own size counts toward its parent, as a disk scan's directory blocks
        assert int(c["alloc"][d]) == 4096 and c["flags"][d] & Flag.HIDDEN
        root = s.roots()[0]
        assert int(c["tot_alloc"][root]) == 4096 + 4096 + 0 + 4096 + 10
        assert int(c["kind"][host.find("l")]) == Kind.LINK and int(c["kind"][host.find("p")]) == Kind.OTHER
        assert c["flags"][host.find("r.txt")] & Flag.READONLY
        assert await host.filter(":attr:r") == ["r.txt"]
        info = (await host.call("info"))["info"]
        assert info["source"] == "t.r4src.list" and info["uri"] == "whole" and info["on_disk"] is False


async def test_flags_a_source_may_not_set_are_dropped(plugin: Path) -> None:
    from spaceknife.plugins.listing import SOURCE_FLAGS, Collector

    col = Collector()
    blob = b"x.bin"
    col.add_batch(
        {
            "paths": np.frombuffer(blob, dtype=np.uint8),
            "path_len": np.array([len(blob)], dtype=np.uint32),
            "kind": np.array([0], dtype=np.uint8),
            "size": np.array([1], dtype=np.int64),
            "alloc": np.array([1], dtype=np.int64),
            "mtime": np.zeros(1, dtype=np.uint32),
            "ctime": np.zeros(1, dtype=np.uint32),
            "atime": np.zeros(1, dtype=np.uint32),
            "flags": np.array([int(Flag.HARDLINK_DUP | Flag.VIRTUAL | Flag.HIDDEN)], dtype=np.uint32),
        }
    )
    assert col.root.files["x.bin"][6] == int(Flag.HIDDEN) & SOURCE_FLAGS
    assert not SOURCE_FLAGS & (Flag.HARDLINK_DUP | Flag.PENDING | Flag.VIRTUAL | Flag.GONE | Flag.MOUNT)


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
async def test_size_and_alloc_both_stay_and_the_views_measure_by_size(plugin: Path, process: bool) -> None:
    from spaceknife.layout.basis import Basis

    async with MockHost.load(plugin, process=process) as host:
        await host.source("packed", "a.zip")
        s = host.store
        z = host.find("zero.bin")
        assert int(s.cols["size"][z]) == 30_000_000 and int(s.cols["alloc"][z]) == 31_000  # as listed
        assert s.measure == "size"
        root = s.roots()[0]
        sizes = Basis.effective_sizes_of(s, np.array([root, z], dtype=np.int64))
        assert sizes.tolist() == [30_000_100, 30_000_000]  # the areas: unpacked sizes
        frame = await host._frame({"visual": "treemap"}, 800, 600)  # what the treemap draws
        assert frame.root.size == 30_000_100


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
async def test_progress_and_what_came_so_far_show_while_it_lists(
    plugin: Path, process: bool, monkeypatch: pytest.MonkeyPatch
) -> None:
    from spaceknife_protocol.scan import ScanStartParams

    monkeypatch.setattr(sources, "PARTIAL_FIRST_S", 0.1)
    monkeypatch.setattr(sources, "PARTIAL_EVERY_S", 0.1)
    monkeypatch.setattr(sources, "PARTIAL_SLOW_S", 0.2)
    async with MockHost.load(plugin, process=process) as host:
        await host.settled()
        handle = await host.server.scans.start(ScanStartParams(source="t.r4src.list", uri="slow"))
        seen: list[Any] = []
        while handle.running:
            st = handle.status()
            seen.append((st.state, st.progress, st.message, len(handle.store) - 1, st.items))
            await asyncio.sleep(0.05)
        assert handle.task is not None
        await handle.task
        scanning = [x for x in seen if x[0] == "scanning"]
        # the source's ctx.progress: a fraction and its text, in the language of the moment
        assert any(x[1] is not None and 0 < x[1] < 1 and x[2].startswith("第 ") for x in scanning)
        # the tree grew while it listed (named after the URI until the source names it)
        shown = sorted({x[3] for x in scanning if x[3] > 1})
        assert len(shown) >= 2 and shown[0] < 67  # 6 folders and 60 files below the root
        assert handle.status().state == "done" and len(handle.store) - 1 == 67
        assert handle.roots[0].path == "slow"


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
async def test_a_failure_keeps_its_reason(plugin: Path, process: bool) -> None:
    async with MockHost.load(plugin, process=process) as host:
        with pytest.raises(RuntimeError, match=r"not an ncdu export|不是 ncdu 导出"):
            await host.source("list", "bad")
        st = host.status()
        assert st.state == "failed" and "239" in st.message
