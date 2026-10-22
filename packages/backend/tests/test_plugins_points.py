"""Every extension point: what a plugin adds reaches its subsystem, and goes when it is turned off."""

from __future__ import annotations

from pathlib import Path

import numpy as np
import pytest

from spaceknife import classes, pathclass
from spaceknife.extensions import EXTENSIONS, capabilities
from spaceknife.filter.vocab import platform_vocab
from spaceknife.insights import active_rules
from spaceknife.layout import registry as visuals
from spaceknife.palette.commands import registry as commands
from spaceknife.palette.keymap import conflicts
from spaceknife.plugins.handlers import extensions_state
from spaceknife_protocol import KNOWN_CAPABILITIES

from .plugin_helpers import mock

pytestmark = pytest.mark.timeout(120)


@pytest.fixture(autouse=True)
def _home(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    monkeypatch.setenv("SPACEKNIFE_HOME", str(tmp_path / "home"))


def _ids(kind: str, owner: str) -> list[str]:
    return [c.id for c in EXTENSIONS.of(kind) if c.owner == owner]


async def test_declarative_contributions_and_their_removal() -> None:
    before_caps = capabilities()
    async with mock("t.decl") as host:
        hp = await host._ready()
        assert hp.records["t.decl"].state == "enabled"
        # commands: palette, keys (own and a key for a built-in command), generated filter preset
        cmds = {c.id: c for c in commands()}
        logs = cmds["t.decl.logs"]
        assert (
            logs.action.kind == "filter"
            and logs.action.expr == "*.log"
            and logs.menus == ("node", "inspector")
        )
        assert "Ctrl+Alt+H" in logs.keys and "Ctrl+Alt+K" in logs.keys
        assert "Ctrl+Alt+J" in cmds["view.rescan"].keys and "F5" in cmds["view.rescan"].keys
        assert cmds["t.decl.about_open"].action.target == "report:t.decl.about"
        assert cmds["t.decl.big"].action.kind == "filter" and cmds["t.decl.big"].name == "过滤：大文件"
        assert cmds["view.visual.t.decl.files_flame"].owner == "t.decl"
        assert cmds["view.color.t.decl.media"].owner == "t.decl"
        assert not conflicts(list(commands()))
        # settings: namespaced, defaults, metadata in category plugins
        st = host.server.settings
        assert st.get("plugin.t.decl.level") == 3 and st.plugin_settings("t.decl") == {
            "level": 3,
            "mode": "a",
            "fast": False,
        }
        meta = {m.key: m for m in st.state().meta}
        assert (
            meta["plugin.t.decl.mode"].category == "plugins" and meta["plugin.t.decl.mode"].plugin == "t.decl"
        )
        assert [o.label for o in meta["plugin.t.decl.mode"].options] == ["甲", "乙"]
        # visualization variant: registered with its base layout; capabilities unchanged
        vis = visuals.get("t.decl.files_flame")
        assert (
            vis.layout == "flame.v1"
            and vis.size_metric == "files"
            and vis.size_unit == "count"
            and vis.plugin == "t.decl"
        )
        assert capabilities() == before_caps
        # classes and folder rules, insight rules, export template, node sets, presets, icons, panels
        assert (
            "t.decl.cad" in classes.active().by_id
            and "dwg" in classes.active().by_id["t.decl.cad"].extensions
        )
        assert pathclass._plugin_rules and pathclass._plugin_rules[0].class_id == "t.decl.cad"
        assert any(r.id == "t-decl-gradle" for r in active_rules().rules)
        state = extensions_state(hp, frozenset(KNOWN_CAPABILITIES))
        assert [r.id for r in state.reports] == ["t.decl.about"] and state.reports[0].static
        assert [p.id for p in state.panels] == ["t.decl.board"]
        assert [c.id for c in state.color_modes] == ["t.decl.media", "t.decl.age"]
        assert [p.expr for p in state.filter_presets] == [">1mb"]
        assert "t.decl:star" in state.icons and "<script" not in state.icons["t.decl:star"]
        assert "t.decl.names" in _ids("export_template", "t.decl")
        # the export template works
        out = await host.export("names")
        assert "movie.mp4" in out and "a.log" in out
        # frames: the variant counts files; the colour mode fills pal
        frame = await host.visual("files_flame")
        assert frame.size_unit == "count" and len(frame.draw)
        colored = await host.color("media")
        names = list(colored.draw.names)
        pal = colored.pal.tolist()
        assert colored.palette == "t.decl.media"
        assert (
            pal[names.index("movie.mp4")] == 1
            and pal[names.index("photo.jpg")] == 2
            and pal[names.index("a.log")] == 0
        )
        aged = await host.color("age")
        assert aged.pal[list(aged.draw.names).index("movie.mp4")] == 3  # older than a year
        # the class of a gradle cache comes from the plugin's folder rule
        store = host.store
        col = pathclass.column(store, classes.active())
        jar = host.find("project/.gradle/caches/x.jar")
        assert col[jar] == classes.active().ids.index("t.decl.cad") + 1
    # off: everything is gone again
    assert not EXTENSIONS.owned("t.decl")
    assert "t.decl.cad" not in classes.active().by_id
    assert not pathclass._plugin_rules
    assert not any(r.id == "t-decl-gradle" for r in active_rules().rules)
    with pytest.raises(KeyError):
        visuals.get("t.decl.files_flame")
    assert "t.decl.logs" not in {c.id for c in commands()}
    assert "Ctrl+Alt+J" not in {c.id: c for c in commands()}["view.rescan"].keys


async def test_code_contributions() -> None:
    async with mock("t.full") as host:
        hp = await host._ready()
        # report: validated, rendered as text
        rep = await host.report("big")
        text = host.text(rep)
        assert "d.bin" in text and "a.log" in text and "b.txt" not in text  # min_size is 100
        assert rep.actions[0].action.kind == "cleanup" and rep.actions[0].action.cleanup == "t.full.logs"
        host.set_setting("min_size", 600)
        await hp.push_update({"settings": True})
        rep = await host.report("big")
        assert "d.bin" in host.text(rep) and "a.log" not in host.text(rep)
        # fields (inspector waits, hover only shows what is cached)
        node = host.find("a.log")
        fields = await host.fields(node=node)
        assert [(f.id, f.value) for f in fields] == [("t.full.name_len", 5), ("t.full.ext", "log")]
        hover = await host.fields("hover", node=host.find("b.txt"))
        assert hover == [] or [f.id for f in hover] == ["t.full.name_len"]
        # insight cards: the plugin's after the rules
        cards = await host.insights(node=node)
        assert cards[-1].source == "plugin" and cards[-1].title == "日志"
        # cleanup -> proposals resolved to nodes with reasons
        props = await host.cleanup("logs")
        assert sorted(p.path.rsplit("/", 1)[-1] for p in props) == ["a.log", "c.log"]
        assert all(p.reason == "日志文件" and p.checked for p in props)
        # node set -> the filter's :set:
        mask = await host.node_set("logs")
        assert mask is not None and int(mask.sum()) == 2
        assert await host.filter(":set:logs") == ["a.log", "sub/c.log"]
        assert await host.filter(":set:日志;|*.txt") == ["a.log", "sub/c.log"]
        vocab = platform_vocab()
        assert [s.id for s in vocab.sets] == ["t.full.logs"]
        # export format streamed from the plugin
        assert (await host.export("lines")).splitlines()[:2] == ["a.log", "b.txt"]
        # code colour mode and code visualization
        frame = await host.color("parity")
        rows = frame.draw.node.astype(np.int64)
        real = rows >= 0
        assert (frame.pal[real] == (1 + rows[real] % 2)).all()
        frame = await host.visual("by_name")
        assert frame.size_unit == "count" and len(frame.draw)
        # command from a decorator: an action calling the function
        cmd = {c.id: c for c in commands()}["t.full.hello"]
        assert cmd.action.kind == "call" and cmd.action.method == "plugin.t.full.hello"
        assert await host.call("hello", scan_id=host.scan_id) == {
            "type": "toast",
            "text": "你好",
            "tone": "ok",
        }


async def test_scan_source_and_hooks(tmp_path: Path) -> None:
    import zipfile

    archive = tmp_path / "data.zip"
    with zipfile.ZipFile(archive, "w") as z:
        z.writestr("docs/readme.txt", "x" * 100)
        z.writestr("docs/deep/notes.md", "y" * 50)
        z.writestr("top.bin", "z" * 10)
    async with mock("t.full") as host:
        await host.scan_dir(tmp_path)  # files.read reaches the zip only inside a scanned folder
        scan_id = await host.source("zip", str(archive))
        handle = host.server.scans.scans[scan_id]
        assert handle.plugin_source == "t.full.zip" and handle.status().state == "done"
        store = handle.store
        assert store.name(1) == "data.zip"
        assert sorted(store.path(i).replace("\\", "/") for i in range(2, len(store))) == [
            "data.zip/docs",
            "data.zip/docs/deep",
            "data.zip/docs/deep/notes.md",
            "data.zip/docs/readme.txt",
            "data.zip/top.bin",
        ]
        assert int(store.cols["tot_size"][1]) == 160
        # not on disk: the basket refuses it
        res = await host.server.ops.add(
            host.session,
            __import__("spaceknife_protocol.ops", fromlist=["x"]).BasketAddParams(scan_id=scan_id, nodes=[2]),
        )
        assert res.added == 0 and res.refused
        # scan_done ran the plugin's hook
        await host.hook("scan_done", scan={"scan_id": scan_id})
        assert host.state("status") == {"runs": -1}
