"""The SDK on its own: texts, declarations, report builders, listings, the audit rules, imports."""

from __future__ import annotations

from pathlib import Path

import numpy as np
import pytest

from spaceknife_protocol.plugins import NodeRef, Report
from spaceknife_protocol.text import CATALOG, LANG, LocalStr, T, Text
from spaceknife_sdk import Context, Params, Payload, Plugin, r
from spaceknife_sdk._audit import Guard
from spaceknife_sdk.errors import ManifestError
from spaceknife_sdk.imports import check_imports
from spaceknife_sdk.manifest import parse_manifest
from spaceknife_sdk.models import Listing
from spaceknife_sdk.report import render_text

MANIFEST = (
    'id = "example.unit"\nversion = "0.1.0"\nname = { zh = "单元", en = "Unit" }\nentry = "plugin.py"\n'
)


def test_texts_follow_the_language() -> None:
    t = T("找到 {n} 组", "Found {n} groups").format(n=3)
    assert t.get("zh-CN") == "找到 3 组" and t.get("en") == "Found 3 groups"
    from pydantic import BaseModel

    class M(BaseModel):
        label: LocalStr

    token = LANG.set("en")
    try:
        assert M(label=T("你好", "Hello")).label == "Hello"
        assert M(label={"zh": "你好", "en": "Hi"}).label == "Hi"  # type: ignore[arg-type]
        cat = CATALOG.set({"只有中文": "Chinese only"})
        try:
            assert M(label="只有中文").model_dump()["label"] == "Chinese only"  # looked up when written
            assert Text(zh="只有中文").get() == "Chinese only"
        finally:
            CATALOG.reset(cat)
    finally:
        LANG.reset(token)
    assert M(label=T("你好", "Hello")).label == "你好"


def test_declarations_are_recorded_and_described(tmp_path: Path) -> None:
    plugin = Plugin(manifest=parse_manifest(MANIFEST), dir=tmp_path)

    class In(Params):
        n: int = 1

    class Out(Payload):
        n: int

    @plugin.method(timeout=5, latest=True)
    async def double(ctx: Context, p: In) -> Out:
        return Out(n=p.n * 2)

    @plugin.event
    class GotIt(Payload):
        n: int

    plugin.setting("depth", T("深度", "Depth"), 3, min=1, max=9)
    plugin.command("go", T("出发", "Go"), action={"call": "double"})
    desc = plugin.describe()
    assert (
        [m.name for m in desc.methods] == ["double"]
        and desc.methods[0].latest
        and desc.methods[0].timeout == 5
    )
    assert desc.methods[0].params is not None and "n" in desc.methods[0].params["properties"]
    assert [e.name for e in desc.events] == ["got_it"]
    kinds = [c["kind"] for c in desc.contributions]
    assert kinds == ["setting", "command"]
    assert desc.catalog == {"深度": "Depth", "出发": "Go", "单元": "Unit"}


def test_the_same_item_twice_is_an_error(tmp_path: Path) -> None:
    m = parse_manifest(MANIFEST + '[[settings]]\nname = "depth"\ntitle = "深度"\ndefault = 3\n')
    plugin = Plugin(manifest=m, dir=tmp_path)
    plugin.setting("depth", "深度", 4)
    with pytest.raises(ManifestError, match=r"declared in plugin\.toml and in the code"):
        plugin.describe()


def test_untyped_methods_are_reported(tmp_path: Path) -> None:
    plugin = Plugin(manifest=parse_manifest(MANIFEST), dir=tmp_path)

    @plugin.method
    async def loose(ctx, p):  # type: ignore[no-untyped-def]
        return None

    with pytest.raises(ManifestError, match="annotate the second argument"):
        plugin.describe()


def test_report_builders_and_text() -> None:
    ref = NodeRef(scan_id="s", node=3, epoch=0, path="/x/a.log")
    rep = Report(
        title=T("报告", "Report"),
        blocks=[
            r.stats([(T("文件", "Files"), 3, "count"), ("合计", 1536, "size")]),
            r.table(
                [r.col("size", "大小", "size"), r.col("name", "名字")],
                rows=[{"size": 1024, "name": "a", "node": ref}],
            ),
            r.section("更多", [r.md("说明"), r.callout("小心", "warn"), r.bars([("a", 10)])]),
            r.meter(5, 10, "用量"),
            r.nodes([ref]),
        ],
    )
    text = render_text(rep)
    assert "# 报告" in text and "文件: 3 · 合计: 1.5 KB" in text and "1.0 KB | a" in text
    assert "## 更多" in text and "[warn] 小心" in text and "- /x/a.log" in text
    assert Report.model_validate(rep.model_dump(mode="json")) == rep


def test_listing_batches() -> None:
    sent: list[dict[str, object]] = []
    out = Listing(flush=sent.append)
    out.dir("a/b/")
    out.file("a/b/c.txt", size=10, alloc=4096, mtime=5)
    out.file("\\x.bin", size=3)
    with pytest.raises(ValueError, match="not allowed"):
        out.file("../evil", size=1)
    out.flush()
    batch = sent[0]
    assert bytes(batch["_paths"]).decode() == "a/ba/b/c.txtx.bin"  # type: ignore[arg-type]
    assert np.asarray(batch["kind"]).tolist() == [1, 0, 0] and np.asarray(batch["alloc"]).tolist() == [
        0,
        4096,
        3,
    ]


def test_the_guard(tmp_path: Path) -> None:
    storage = tmp_path / "storage"
    scan = tmp_path / "scan"
    for d in (storage, scan):
        d.mkdir()
    (scan / "f.txt").write_text("x", encoding="utf-8")
    g = Guard(
        plugin_dir=str(tmp_path / "plugin"), storage=str(storage), granted=["files.read"], roots=[str(scan)]
    )
    g.write_ok = [g._norm(str(storage))]  # no temp folder for the test
    g("open", (str(storage / "ok.txt"), "w", 0))
    g("open", (str(scan / "f.txt"), "r", 0))
    for event, args in (
        ("open", (str(scan / "f.txt"), "w", 0)),
        ("os.remove", (str(scan / "f.txt"),)),
        ("socket.connect", (None, ("192.0.2.1", 80))),
        ("subprocess.Popen", ("echo", ["echo"], None, None)),
        ("ctypes.dlopen", ("libc",)),
    ):
        with pytest.raises(PermissionError):
            g(event, args)
    g2 = Guard(plugin_dir=str(tmp_path / "plugin"), storage=str(storage), granted=[], roots=[str(scan)])
    g2.read_ok = [g2._norm(str(tmp_path / "plugin"))]  # the test's folders are inside the temp folder
    with pytest.raises(PermissionError):
        g2("open", (str(scan / "f.txt"), "r", 0))
    g3 = Guard(plugin_dir=str(tmp_path), storage=str(storage), granted=["net"], net_hosts=["*.example.com"])
    g3("socket.getaddrinfo", ("api.example.com", 443, 0, 0, 0))
    with pytest.raises(PermissionError):
        g3("socket.getaddrinfo", ("evil.invalid", 443, 0, 0, 0))


def test_imports_are_checked(tmp_path: Path) -> None:
    (tmp_path / "plugin.py").write_text(
        "import json\nimport subprocess\nimport requests\nfrom spaceknife_sdk import Plugin\n",
        encoding="utf-8",
    )
    issues = check_imports(tmp_path)
    assert [(i.code, i.line) for i in issues] == [("forbidden_import", 2), ("unknown_import", 3)]
