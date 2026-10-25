"""Every kind of plugin call carries the context of the place it comes from (plugins/where.py).

Two scans are open: the first (A), which every call below is about, and the second (B), the scan
in front. Each call records what it got: ``ctx.tree`` (or why it has none), ``ctx.where``, the
epoch of its node ids and its language. A call about A must read A, never fall back to B; a call
about no scan reads the scan in front; a call without a scan tree (a ``scan_closed`` hook, a call
from a comparison, a scan source listing) says so instead of reading another tree. A's epoch is
moved on first, so a call pinned to A's node ids shows A's epoch.

Every kind of call the SDK knows (``CALL_KINDS``) has a case here (``test_every_call_kind_has_a_case``):
a new kind fails until it gets its place in where.py and a case below. Each case runs in the
plugin's process and in-process.
"""

from __future__ import annotations

import asyncio
import time
from collections.abc import Awaitable, Callable, Iterator
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import pytest

from spaceknife import i18n
from spaceknife.plugins import handlers as handlers_mod
from spaceknife.plugins import host as host_mod
from spaceknife.plugins.host import RawParams
from spaceknife_protocol import compare as pc
from spaceknife_protocol import palette as ppal
from spaceknife_protocol import plugins as pp
from spaceknife_protocol import view as pv
from spaceknife_protocol.methods import PALETTE_CHECK
from spaceknife_sdk.plugin import CALL_KINDS
from spaceknife_sdk.testing import MockHost, make_tree

pytestmark = pytest.mark.timeout(280)

PID = "t.where"

TOML = """id = "t.where"
version = "0.1.0"
name = { zh = "调用的上下文", en = "Call context" }
entry = "plugin.py"
permissions = ["tree.read"]

[limits]
threads = 4
"""

CODE = """from __future__ import annotations

import asyncio
from collections.abc import AsyncIterator, Iterator
from typing import Any

import numpy as np
from pydantic import BaseModel, ConfigDict

from spaceknife_sdk import (
    ActionContext, ChangeUnit, ColorInput, Context, ExportInput, LayoutInput, Listing, Node, Payload,
    Plugin, PluginError, Proposal, Report, ReportContext, ScanInfo, T, TreeChange, VisualInput,
)
from spaceknife_sdk.models import Change

plugin = Plugin()
SEEN: list[dict[str, Any]] = []


def note(ctx: Context, label: str, **extra: Any) -> None:
    try:
        tree = ctx.tree.scan_id
    except PluginError:
        tree = "none:" + ctx.call_info.tree
    where = getattr(ctx, "where", None)  # (the test runs on hosts without ctx.where too)
    SEEN.append(
        {
            "label": label,
            "tree": tree,
            "epoch": ctx.call_info.epoch,
            "lang": ctx.lang,
            "where": where.model_dump(mode="json") if where is not None else None,
            **extra,
        }
    )


class Seen(Payload):
    seen: list[dict[str, Any]]


class PanelParams(BaseModel):
    model_config = ConfigDict(extra="forbid")  # a panel's params: nothing of the host's in them
    x: int = 0


class StreamParams(Payload):
    scan_id: str | None = None


class Item(Payload):
    n: int


@plugin.method
def seen(ctx: Context) -> Seen:
    return Seen(seen=list(SEEN))


@plugin.method
def reset(ctx: Context) -> Seen:
    SEEN.clear()
    return Seen(seen=[])


@plugin.command("where_cmd", T("在哪", "Where"), check="check_here")
def where_cmd(ctx: Context, a: ActionContext) -> None:
    note(ctx, "method", given=a.model_dump(mode="json"))


@plugin.method
def check_here(ctx: Context, a: ActionContext) -> str | None:
    note(ctx, "check", given=a.model_dump(mode="json"))
    return None


@plugin.method
def panel_m(ctx: Context, p: PanelParams) -> None:
    note(ctx, "panel", x=p.x)


@plugin.command("start_bg", T("后台", "Background"))
def start_bg(ctx: Context, a: ActionContext) -> None:
    ctx.tasks.start("bg", replace=True)


@plugin.background("bg", T("后台", "Background"))
def bg(ctx: Context) -> None:
    note(ctx, "task")


@plugin.report(T("报告", "Report"))
def rep(ctx: Context, c: ReportContext) -> Report:
    note(ctx, "report")
    return Report(title="r")


@plugin.cleanup(T("清理", "Cleanup"))
def cl(ctx: Context, c: ReportContext) -> list[Proposal]:
    note(ctx, "cleanup")
    return []


@plugin.node_field(T("所在", "Where"))
def fld(ctx: Context, node: Node) -> str:
    note(ctx, "field", slot=ctx.slot, node=node.id)
    return "x"


@plugin.insight
def ins(ctx: Context, node: Node) -> None:
    note(ctx, "insight", node=node.id)


@plugin.node_set("s", T("集合", "Set"))
def node_set(ctx: Context, tree: Any) -> np.ndarray:
    note(ctx, "set")
    return np.zeros(len(tree), dtype=bool)


@plugin.visualization("v", T("权重", "Weights"))
def weights(v: VisualInput) -> np.ndarray:
    assert v.ctx is not None
    note(v.ctx, "visual")
    return np.ones(len(v.tree))


@plugin.layout("shapes", T("形状", "Shapes"), compare=True)
def shapes(v: LayoutInput) -> Any:
    from spaceknife_sdk import shapes as sh

    assert v.ctx is not None
    note(v.ctx, "layout")
    return sh.pack(v)


@plugin.color_mode("c", T("颜色", "Colours"), palette=["#888888", "#cc4444"])
def colours(c: ColorInput) -> np.ndarray:
    assert c.ctx is not None
    note(c.ctx, "color")
    return np.zeros(len(c.ids), dtype=np.uint8)


@plugin.color_legend("c")
def legend(ctx: Context) -> list[str]:
    note(ctx, "legend")
    return ["a", "b"]


@plugin.export_format("exp", T("导出", "Export"), extension="txt")
def exp(x: ExportInput) -> Iterator[str]:
    assert x.ctx is not None
    note(x.ctx, "export", root=x.root)
    yield "x"


@plugin.scan_source("lst", T("清单", "List"), watch=True)
def lst(ctx: Context, uri: str, out: Listing) -> None:
    note(ctx, "source")
    out.file("a.bin", 100)
    out.file("d/b.bin", 200)


@plugin.watcher("lst")
async def watch_lst(ctx: Context, uri: str) -> AsyncIterator[list[Change]]:
    note(ctx, "watch")
    while True:
        await asyncio.sleep(0.05)
        if ctx.cancelled:
            return
        if False:
            yield []


@plugin.stream
async def where_stream(ctx: Context, p: StreamParams) -> AsyncIterator[Item]:
    note(ctx, "stream")
    yield Item(n=1)


@plugin.change_annotator
def ann(ctx: Context, items: list[ChangeUnit]) -> list[str | None]:
    note(ctx, "annotate")
    return [None for _ in items]


@plugin.on("scan_done")
def done(ctx: Context, scan: ScanInfo) -> None:
    note(ctx, "hook:scan_done")


@plugin.on("scan_closed")
def closed(ctx: Context, scan: ScanInfo) -> None:
    note(ctx, "hook:scan_closed")


@plugin.on("tree_changed")
def changed(ctx: Context, change: TreeChange) -> None:
    note(ctx, "hook:tree_changed")


@plugin.on("settings")
def settings(ctx: Context, changed: list[str]) -> None:
    note(ctx, "hook:settings")


plugin.setting("level", T("级别", "Level"), 1)
"""

TREE_A = make_tree({"a": {"x.bin": 5000, "y.bin": 300}, "same.txt": 10})
TREE_A2 = {"a": {"x.bin": 9000, "z.bin": 700}, "same.txt": 10}  # A some time before: a comparison
TREE_B = make_tree({"b": {"z.bin": 7000}, "c": {"w.bin": 10}, "other.txt": 20})
EPOCH_A = 5  # A's tree was reorganised a few times: its node ids are of this epoch


@dataclass
class Case:
    """A call, the label its handler records, and what it must have got (``tree``: a scan's
    name in the scenario, ``front``, or ``none:<why>``; ``epoch``: ``A`` or None; ``where``: fields
    of ``ctx.where``, by name, values may name scenario things)."""

    name: str
    kind: str
    label: str
    tree: str
    epoch: str | None = None
    where: dict[str, Any] = field(default_factory=dict)
    lang: str = ""  # "" the interface language
    extra: dict[str, Any] = field(default_factory=dict)


ON_VIEW = {"scan_id": "A", "view_id": "vZ", "selection": ["n"], "zoom_node": "zZ", "filter": "*.bin"}

CASES = [
    Case(
        "call_action",
        "method",
        "method",
        "A",
        "A",
        {
            "scan_id": "A",
            "view_id": "vA",
            "node": "n",
            "selection": ["n"],
            "zoom_node": "zA",
            "filter": "*.bin",
            "args": {"k": 1},
            "compare_id": None,
        },
    ),
    Case(
        "panel_call",
        "method",
        "panel",
        "A",
        "A",
        {"scan_id": "A", "view_id": "vA", "node": "n"},
        extra={"x": 1},
    ),
    Case(
        "check",
        "method",
        "check",
        "A",
        "A",
        {"scan_id": "A", "view_id": "vA", "node": "n", "selection": ["n"], "zoom_node": "zA", "filter": ""},
    ),
    # from a comparison: its node ids are the change tree's, no scan tree; the scan goes along
    Case(
        "call_in_compare_view",
        "method",
        "method",
        "none:compare",
        None,
        {"scan_id": "A", "compare_id": "cmp"},
    ),
    Case(
        "check_in_compare_view",
        "method",
        "check",
        "none:compare",
        None,
        {"scan_id": "A", "compare_id": "cmp", "view_id": "vC"},
    ),
    Case(
        "report_in_compare_panel",  # the comparison panel names the tab's scan and the compare view
        "report",
        "report",
        "none:compare",
        None,
        {"scan_id": "A", "compare_id": "cmp", "view_id": "vC"},
    ),
    Case("report", "report", "report", "A", "A", {"scan_id": "A", "node": "n"}),
    # a request that names only a view (a method, a report, a stream): where the user is in that
    # view (its zoom root, filter, selection) comes along; a view without a scan is the view's scan
    Case("call_on_view", "method", "method", "A", "A", {**ON_VIEW, "node": "n"}),
    Case("report_on_view", "report", "report", "A", "A", {**ON_VIEW, "node": "n"}),
    Case("stream_on_view", "stream", "stream", "A", None, ON_VIEW),
    # a check without a view (a scan tab's details panel): the node the commands are about
    Case(
        "check_without_view",
        "method",
        "check",
        "A",
        "A",
        {"scan_id": "A", "view_id": None, "node": "n", "selection": ["n"]},
    ),
    Case("cleanup", "cleanup", "cleanup", "A", "A", {"scan_id": "A", "node": "n"}),
    Case("field", "field", "field", "A", "A", {"scan_id": "A", "node": "n"}, extra={"slot": "inspector"}),
    Case("insight", "insight", "insight", "A", "A", {"scan_id": "A", "node": "n"}),
    Case("node_set", "set", "set", "A", "A", {"scan_id": "A"}),
    Case("visual", "visual", "visual", "A", "A", {"scan_id": "A"}),
    Case("layout", "layout", "layout", "A", "A", {"scan_id": "A"}),
    Case(
        "layout_in_compare_view",
        "layout",
        "layout",
        "none:compare",
        None,
        {"scan_id": "A", "compare_id": "cmp"},
    ),
    Case("color", "color", "color", "A", "A", {"scan_id": "A", "view_id": "vF"}),
    Case("legend", "legend", "legend", "B", None, {"scan_id": None}, lang="en"),
    Case("export", "export", "export", "A", "A", {"scan_id": "A", "filter": "*.bin", "selection": ["n"]}),
    Case("hook_scan_done", "hook", "hook:scan_done", "A", "A", {"scan_id": "A"}),
    Case("hook_tree_changed", "hook", "hook:tree_changed", "A", "A", {"scan_id": "A"}),
    Case("hook_scan_closed", "hook", "hook:scan_closed", "none:closed", None, {"scan_id": "A"}),
    Case("hook_settings", "hook", "hook:settings", "B", None, {"scan_id": None}),
    Case("task", "task", "task", "A", None, {"scan_id": "A"}),
    Case("stream", "stream", "stream", "A", None, {"scan_id": "A"}),
    Case("panel_stream", "stream", "stream", "A", None, {"scan_id": "A", "view_id": "vA"}),
    Case("source", "source", "source", "none:listing", None, {"scan_id": "S"}),
    Case("watch", "watch", "watch", "S", None, {"scan_id": "S"}),
    Case("annotate", "annotate", "annotate", "A", None, {"scan_id": "A", "compare_id": "cmp"}),
]


def test_every_call_kind_has_a_case() -> None:
    """A new kind of call (CALL_KINDS, the SDK's) needs its place (where.py) and a case here."""
    assert {c.kind for c in CASES} == set(CALL_KINDS)
    assert len({c.name for c in CASES}) == len(CASES)


# --------------------------------------------------------------------------- the scenario


@dataclass
class Got:
    names: dict[str, Any]  # what "A", "vA", "n" … stand for
    seen: dict[str, dict[str, Any] | str]  # case -> its record, or why there is none


async def _until(cond: Callable[[], Awaitable[bool]], limit: float = 20.0) -> bool:
    end = time.monotonic() + limit
    while time.monotonic() < end:
        if await cond():
            return True
        await asyncio.sleep(0.05)
    return False


async def _scenario(folder: Path, process: bool) -> Got:
    async with MockHost.load(folder, process=process) as host:
        first = host.scan(TREE_A)
        root_a = host.files_root
        second = host.scan(TREE_B)
        older = host.scan(make_tree(TREE_A2, root=str(root_a)))
        host.scan_id = first
        await host.settled()
        hp = host.server.plugins
        hp.api.active_scan = lambda: second  # the user looks at B: the scan in front
        server = host.server
        views = server.views
        store_a = server.scans.scans[first].store
        store_a.epoch = EPOCH_A
        node = host.find("a/x.bin")
        session: Any = host.session

        def view(scan_id: str, **settings: Any) -> Any:
            vhost = views.open(
                session, pv.ViewOpenParams(scan_id=scan_id, viewport=pv.Viewport(width=800, height=600))
            )
            if settings:
                from spaceknife.server.views import apply_settings

                apply_settings(vhost.view, pv.ViewSettingsParams(**settings), session.capabilities)
            return vhost.view

        async def render(v: Any) -> None:
            from spaceknife.server.compute import compute

            await compute(v.render)

        here = view(first)
        names: dict[str, Any] = {
            "A": first,
            "B": second,
            "vA": here.id,
            "n": node,
            "zA": int(here.zoom.node),
        }
        # a view of A zoomed into a, filtered, x.bin selected: requests that name only the view
        zoomed = view(first)
        names["zZ"] = host.find("a")
        assert zoomed.zoom_to_node(names["zZ"])
        assert zoomed.set_filter("*.bin").normal is not None
        zoomed.select(node)
        names["vZ"] = zoomed.id

        # a comparison of A with an older tree of it (its change tree: a compare view)
        host.scan_id = older
        snap = host.snapshot()
        host.scan_id = first
        cmp_id = server.compares.open(
            session, pc.CompareOpenParams(scan_id=first, base=str(snap), target=pc.NOW)
        ).compare_id

        async def ready() -> bool:
            return server.compares.lookup(cmp_id) is not None

        assert await _until(ready), "the comparison did not get ready"
        names["cmp"] = cmp_id

        async def seen() -> list[dict[str, Any]]:
            out: list[dict[str, Any]] = (await host.call("seen"))["seen"]
            return out

        def route(name: str) -> Any:
            r = hp.route(f"plugin.{PID}.{name}")
            assert r is not None
            return r.handler

        async def call_action() -> None:
            body = {
                "scan_id": first,
                "view_id": here.id,
                "node": node,
                "selection": [node],
                "zoom_node": names["zA"],
                "filter": "*.bin",
                "args": {"k": 1},
                "compare_id": None,
            }
            await route("where_cmd")(session, RawParams.model_validate(body))

        async def panel_call() -> None:
            ctx = {"scan_id": first, "view_id": here.id, "node": node, "selection": [node]}
            await route("panel_m")(session, RawParams.model_validate({"x": 1, "$where": ctx}))

        async def check(view_id: str | None, node_id: int | None) -> None:
            handler = server.router.get(PALETTE_CHECK.name)
            assert handler is not None
            context = ppal.PaletteContext(tab="scan", scan_id=first, view_id=view_id, node=node_id)
            await handler.handler(session, ppal.PaletteCheckParams(id=f"{PID}.where_cmd", context=context))

        async def call_in_compare_view() -> None:
            names["vC"] = names.get("vC") or view(cmp_id).id
            body = {"scan_id": cmp_id, "view_id": names["vC"], "compare_id": cmp_id}
            await route("where_cmd")(session, RawParams.model_validate(body))

        async def check_in_compare_view() -> None:
            names["vC"] = names.get("vC") or view(cmp_id).id
            await check(names["vC"], None)

        async def report_in_compare_panel() -> None:
            names["vC"] = names.get("vC") or view(cmp_id).id
            body = {"scan_id": first, "view_id": names["vC"], "node": 1, "compare_id": cmp_id}
            await route("rep")(session, RawParams.model_validate(body))

        async def on_view(method: str) -> None:
            body = {"scan_id": first, "view_id": names["vZ"], "node": node}
            await route(method)(session, RawParams.model_validate(body))

        async def report() -> None:
            await route("rep")(session, RawParams.model_validate({"scan_id": first, "node": node}))

        async def cleanup() -> None:
            await route("cl")(session, RawParams.model_validate({"scan_id": first, "node": node}))

        async def field_() -> None:
            await handlers_mod.node_fields(hp, session, pp.PluginFieldsParams(scan_id=first, node=node))

        async def insight() -> None:
            await handlers_mod.insight_cards(hp, session, pp.InsightNodeParams(scan_id=first, node=node))

        async def node_set() -> None:
            await asyncio.to_thread(hp.set_masks, first, store_a, [f"{PID}.s"])

        async def frame(scan_id: str, **settings: Any) -> None:
            v = view(scan_id, **settings)
            if settings.get("color_mode"):
                names["vF"] = v.id
            try:
                await render(v)
            finally:
                views.close(v.id)

        async def legend() -> None:
            from spaceknife.extensions import EXTENSIONS

            c = EXTENSIONS.get("color_mode", f"{PID}.c")
            hp.legend_of(c, "en")
            await host.settled()

        async def export() -> None:
            host.scan_id = first
            await host.export("exp", filter="*.bin", selection=[node])

        async def hook(event: str) -> None:
            if event == "tree_changed":
                hp.live.tree_changed(first, {node: 0})
            elif event == "settings":
                await host.set_setting("level", 2)
            else:
                hp.fire(event, scan=hp.api.scan_info(server.scans.scans[first]))
            await host.settled()

        async def task() -> None:
            await route("start_bg")(session, RawParams.model_validate({"scan_id": first}))

        async def stream(params: dict[str, Any]) -> None:
            sub = await hp.open_topic(session, f"plugin.{PID}.where_stream", params, None)
            assert sub is not None
            await asyncio.sleep(0.3)
            sub.close()

        async def source() -> None:
            names["S"] = await host.source("lst", "mem://list")
            host.scan_id = first

        async def annotate() -> None:
            items = [
                pc.ChangeItem(
                    node=1, path="/a", label="a", kind="file", delta=3, base=0, target=3, status="new"
                )
            ]
            await asyncio.to_thread(hp.annotate_changes, cmp_id, items)

        runs: dict[str, Callable[[], Awaitable[None]]] = {
            "call_action": call_action,
            "panel_call": panel_call,
            "check": lambda: check(here.id, node),
            "call_in_compare_view": call_in_compare_view,
            "check_in_compare_view": check_in_compare_view,
            "report_in_compare_panel": report_in_compare_panel,
            "report": report,
            "call_on_view": lambda: on_view("where_cmd"),
            "report_on_view": lambda: on_view("rep"),
            "stream_on_view": lambda: stream({"$where": {"view_id": names["vZ"]}}),
            "check_without_view": lambda: check(None, node),
            "cleanup": cleanup,
            "field": field_,
            "insight": insight,
            "node_set": node_set,
            "visual": lambda: frame(first, visual=f"{PID}.v"),
            "layout": lambda: frame(first, visual=f"{PID}.shapes"),
            "layout_in_compare_view": lambda: frame(cmp_id, visual=f"{PID}.shapes"),
            "color": lambda: frame(first, visual="treemap", color_mode=f"{PID}.c"),
            "legend": legend,
            "export": export,
            "hook_scan_done": lambda: hook("scan_done"),
            "hook_tree_changed": lambda: hook("tree_changed"),
            "hook_scan_closed": lambda: hook("scan_closed"),
            "hook_settings": lambda: hook("settings"),
            "task": task,
            "stream": lambda: stream({"scan_id": first}),
            "panel_stream": lambda: stream({"$where": {"scan_id": first, "view_id": here.id}}),
            "source": source,
            "watch": lambda: asyncio.sleep(0),  # the watcher of the source's scan (started by it)
            "annotate": annotate,
        }
        got: dict[str, dict[str, Any] | str] = {}
        for case in CASES:
            if case.name != "watch":
                await host.call("reset")
            try:
                await runs[case.name]()
            except Exception as exc:  # the case says what went wrong; the others still run
                got[case.name] = f"{type(exc).__name__}: {exc}"
                continue
            got[case.name] = await _record(seen, case.label)
        return Got(names, got)


async def _record(seen: Callable[[], Awaitable[list[dict[str, Any]]]], label: str) -> dict[str, Any] | str:
    """The first call the plugin recorded under ``label`` (waiting for it), or why there is none."""
    found: list[dict[str, Any]] = []

    async def recorded() -> bool:
        found[:] = [r for r in await seen() if r["label"] == label]
        return bool(found)

    return found[0] if await _until(recorded, 10.0) else f"no {label} call recorded"


@pytest.fixture(scope="module", params=[False, True], ids=["inprocess", "process"])
def scenario(request: pytest.FixtureRequest, tmp_path_factory: pytest.TempPathFactory) -> Iterator[Got]:
    tmp = tmp_path_factory.mktemp("where")
    folder = tmp / PID
    folder.mkdir()
    (folder / "plugin.toml").write_text(TOML, encoding="utf-8")
    (folder / "plugin.py").write_text(CODE, encoding="utf-8")
    with pytest.MonkeyPatch.context() as mp:
        mp.setenv("SPACEKNIFE_MOCK_TREES", str(tmp / "trees"))
        mp.setenv("SPACEKNIFE_HOME", str(tmp / "home"))
        # a loaded test machine: the calls on the render path answer (their time limits are short)
        for name in ("CHECK_TIMEOUT", "COLOR_TIMEOUT", "ANNOTATE_TIMEOUT"):
            mp.setattr(host_mod, name, 5.0)
        mp.setattr(handlers_mod, "FIELD_TIMEOUT_S", 5.0)
        yield asyncio.run(_scenario(folder, bool(request.param)))


def _named(value: Any, names: dict[str, Any]) -> Any:
    if isinstance(value, str) and value in names:
        return names[value]
    if isinstance(value, list):
        return [_named(v, names) for v in value]
    return value


@pytest.mark.parametrize("case", CASES, ids=[c.name for c in CASES])
def test_each_call_gets_the_context_of_the_scan_it_is_for(scenario: Got, case: Case) -> None:
    rec = scenario.seen[case.name]
    assert isinstance(rec, dict), rec
    names = scenario.names
    tree = names.get(case.tree, case.tree)
    assert rec["tree"] == tree, f"ctx.tree of {case.name}: {rec['tree']} (A={names['A']}, B={names['B']})"
    assert rec["epoch"] == (EPOCH_A if case.epoch == "A" else None)
    assert rec["lang"] == (case.lang or i18n.language())
    where = rec["where"]
    assert where is not None, "no ctx.where"
    for key, want in case.where.items():
        assert where[key] == _named(want, names), f"ctx.where.{key} of {case.name}: {where[key]!r}"
    for key, want in case.extra.items():
        assert rec[key] == want
    if case.name in ("call_action", "check"):  # a command's method gets ctx.where as its ActionContext
        assert {k: rec["given"][k] for k in case.where} == {k: where[k] for k in case.where}
    if case.name == "export":  # the tree below the export's root
        assert where["node"] == rec["root"]
