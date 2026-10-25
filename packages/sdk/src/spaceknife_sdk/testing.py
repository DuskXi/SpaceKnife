"""Testing a plugin without the app: the real host components, no socket, no window.

>>> host = MockHost.load(__file__)            # the plugin folder above this test file
>>> host.scan(make_tree({"a.iso": (200 << 20, "2020-01-01"), "b.txt": 10}))
>>> rep = await host.report("files")          # validated Report
>>> "a.iso" in host.text(rep)

``MockHost`` runs a backend server object (not listening), its plugin host, and the plugin in the
host's own process (``process=True``: in a plugin process, as in the app). Its data folders are
temporary. Needs the SpaceKnife backend installed (``spaceknife-backend``).
"""

from __future__ import annotations

import asyncio
import contextlib
import os
import shutil
import tempfile
import time
from collections.abc import Mapping, Sequence
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Literal

from spaceknife_protocol.plugins import Report

from .context import TaskInfo
from .manifest import MANIFEST
from .report import render_text
from .tree import Node


@dataclass(frozen=True, slots=True)
class Tree:
    """A synthetic tree for :meth:`MockHost.scan` (from :func:`make_tree`)."""

    spec: Mapping[str, Any]
    root: str | None = None


def make_tree(spec: Mapping[str, Any], *, root: str | None = None) -> Tree:
    """``{name: size | (size, mtime) | bytes | (bytes, mtime) | {…}}``: a file (size, modified time
    as unix seconds or ``"YYYY-MM-DD"``, real contents, or both) or a folder.

    ``root``: the folder the tree stands for (its files are written there when they have contents).
    By default a new folder under ``~/.spaceknife-mock/`` (removed by ``close()``): not under the
    temporary folder, so the app's own rules for temporary folders do not cover what a test
    looks at."""
    return Tree(spec, root)


def _mock_trees() -> Path:
    """Where synthetic trees with contents go: ``$SPACEKNIFE_MOCK_TREES``, else
    ``~/.spaceknife-mock`` (else the temporary folder when the home folder is not writable)."""
    base = os.environ.get("SPACEKNIFE_MOCK_TREES")
    if base:
        Path(base).mkdir(parents=True, exist_ok=True)
        return Path(base)
    home = Path.home() / ".spaceknife-mock"
    try:
        home.mkdir(parents=True, exist_ok=True)
        return home
    except OSError:
        return Path(tempfile.gettempdir())


class _Settled:
    """What ``set_setting`` / ``set_lang`` return: ``await`` it to wait for the plugin's hooks (the
    next call waits for them anyway)."""

    def __init__(self, host: MockHost) -> None:
        self._host = host

    def __await__(self) -> Any:
        return self._host._settle().__await__()


@dataclass(slots=True)
class Proposed:
    node: int
    path: str
    reason: str
    checked: bool
    alloc: int


class _Session:
    """Records what the host would send to a frontend."""

    def __init__(self) -> None:
        from spaceknife_protocol import KNOWN_CAPABILITIES

        self.id = "mock-session"
        self.capabilities = frozenset(KNOWN_CAPABILITIES)
        self.client = None
        self.sent: list[tuple[str, Any]] = []
        self.binary: list[bytes] = []
        self.subs: dict[str, Any] = {}

    def emit(self, spec: Any, payload: Any) -> None:
        self.sent.append((spec.name, payload.model_dump(mode="json")))

    def emit_raw(self, name: str, payload: dict[str, Any]) -> None:
        self.sent.append((name, payload))

    def send_binary(self, frame: bytes) -> None:
        self.binary.append(frame)

    def add_subscription(self, sub: Any) -> None:
        self.subs[sub.id] = sub

    def remove_subscription(self, sub_id: str) -> bool:
        sub = self.subs.pop(sub_id, None)
        if sub is not None:
            sub.close()
        return sub is not None


def _plugin_folder(path: str | os.PathLike[str]) -> Path:
    p = Path(path).resolve()
    for folder in [p, *p.parents]:
        if (folder / MANIFEST).is_file():
            return folder
    raise FileNotFoundError(f"no {MANIFEST} in {p} or above")


def plugin_module(path: str | os.PathLike[str]) -> Any:
    """The plugin's own code as a module, for testing its helpers directly::

        mod = plugin_module(__file__)        # the plugin.py above this test file
        assert mod.group_by_size([...]) == ...

    Imported once per plugin (its ``lib/`` and folder on ``sys.path``, as the host does), apart
    from the copy :class:`MockHost` runs; ``from plugin import x`` in a test may find another
    plugin's ``plugin.py`` first, this does not."""
    import sys

    from ._runtime import load_plugin
    from .manifest import load_manifest

    folder = _plugin_folder(path)
    manifest = load_manifest(folder)
    if manifest.entry is None:
        raise ValueError(f"{manifest.id} is declarative: it has no code to import")
    name = "skplugin_test_" + manifest.id.replace(".", "_").replace("-", "_")
    if name in sys.modules:
        return sys.modules[name]
    load_plugin(folder, manifest)  # imports it as the runtime would, as skplugin_<id>
    module = sys.modules.pop("skplugin_" + manifest.id.replace(".", "_").replace("-", "_"))
    sys.modules[name] = module  # under its own name: MockHost imports a copy of its own
    return module


@dataclass
class MockHost:
    folder: Path
    grant: Sequence[str] | str = "requested"
    process: bool = False
    home: Path = field(default_factory=lambda: Path(tempfile.mkdtemp(prefix="sk-mockhost-")))
    server: Any = None
    session: _Session = field(default_factory=_Session)
    scan_id: str | None = None
    files_root: Path | None = None
    _env: dict[str, str | None] = field(default_factory=dict)
    _started: bool = False
    _settings_pending: bool = False
    _stores: list[tuple[str, Any]] = field(default_factory=list)
    _trees: list[Path] = field(default_factory=list)
    _made: dict[str, float] = field(default_factory=dict)  # synthetic scan -> when it was made

    # ------------------------------------------------------------- setting up

    @classmethod
    def load(
        cls, path: str | os.PathLike[str], *, grant: Sequence[str] | str = "requested", process: bool = False
    ) -> MockHost:
        """The plugin whose folder holds ``path`` (a test file inside it works)."""
        return cls(_plugin_folder(path), grant, process)

    def _set_env(self) -> None:
        for key, value in (
            ("SPACEKNIFE_HOME", str(self.home)),
            ("SPACEKNIFE_LANGUAGE", os.environ.get("SPACEKNIFE_LANGUAGE") or "zh-CN"),
        ):
            self._env.setdefault(key, os.environ.get(key))
            os.environ[key] = value

    async def start(self) -> MockHost:
        if self._started:
            return self
        self._set_env()
        from spaceknife.server.app import Server, ServerConfig

        self.server = Server(
            ServerConfig(token="mock-host-token-0123456789", hello_timeout=1.0, ping_interval=None)
        )
        host = self.server.plugins
        host.force_runtime = "process" if self.process else "inprocess"
        await host.start()
        rec = await host.link_dev(self.folder)
        if rec.state != "enabled":
            raise RuntimeError(
                f"{rec.id} cannot be enabled: {'; '.join(i.text() for i in rec.issues) or rec.state}"
            )
        if self.grant != "requested":
            rec.granted = frozenset(self.grant)
            if rec.runtime is not None:
                await rec.runtime.update({"granted": sorted(rec.granted)})
        self.server.sessions.add(self.session)
        self._started = True
        return self

    @property
    def plugin_id(self) -> str:
        from spaceknife_sdk.manifest import load_manifest

        return load_manifest(self.folder).id

    async def close(self) -> None:
        if self.server is not None:
            self.server.sessions.discard(self.session)
            for handle in list(self.server.scans.scans.values()):  # sources' watchers, as the app
                monitor = getattr(handle, "monitor", None)
                if monitor is not None:
                    with contextlib.suppress(Exception):
                        await monitor.stop()
                    handle.monitor = None
            await self.server.plugins.close()
            await self.server.scans.close()
        for tree in self._trees:
            shutil.rmtree(tree, ignore_errors=True)
        for key, value in self._env.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value
        with contextlib.suppress(Exception):
            from spaceknife import i18n

            i18n.system_language.cache_clear()
        shutil.rmtree(self.home, ignore_errors=True)

    async def __aenter__(self) -> MockHost:
        return await self.start()

    async def __aexit__(self, *exc: object) -> None:
        await self.close()

    # ------------------------------------------------------------- scans

    def scan(self, tree: Tree | Any) -> str:
        """Use ``tree`` (from :func:`make_tree`, or a NodeStore) as the scan in front."""
        from spaceknife.plugins.listing import from_spec

        if not self._started:
            self._set_env()
        now = int(time.time())
        if isinstance(tree, Tree):
            if tree.root is None:
                self.files_root = Path(tempfile.mkdtemp(prefix="tree-", dir=_mock_trees()))
                self._trees.append(self.files_root)
            else:
                self.files_root = Path(tree.root)
            root = str(self.files_root)
            store, contents = from_spec(tree.spec, root, now)
            for rel, data in contents:
                target = Path(root) / rel
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(data)
        else:
            store = tree
        scan_id = f"scan-mock{len(self._stores) + 1}"
        self._stores.append((scan_id, store))
        self._made[scan_id] = time.time()
        self.scan_id = scan_id
        return scan_id

    async def _ready(self) -> Any:
        await self.start()
        for scan_id, store in self._stores:
            if scan_id not in self.server.scans.scans:
                from spaceknife.scan.scanner import RootInfo, ScanProgress, ScanState
                from spaceknife.server.scans import ScanHandle

                self.server.scans.scans[scan_id] = ScanHandle(
                    id=scan_id,
                    store=store,
                    roots=[
                        RootInfo(path=store.name(r), node=r, volume=None, whole_volume=False)
                        for r in store.roots()
                    ],
                    source="snapshot",
                    saved_progress=ScanProgress(
                        state=ScanState.DONE,
                        items=len(store) - 1,
                        files=int(store.cols["tot_files"][0]),
                        dirs=int(store.cols["tot_dirs"][0]),
                        bytes_alloc=int(store.cols["tot_alloc"][0]),
                        bytes_size=int(store.cols["tot_size"][0]),
                        errors=0,
                        pending_dirs=0,
                        current_path="",
                        elapsed_s=0.0,
                        rate=0.0,
                        progress=1.0,
                        started_at=_when(self._made.get(scan_id)),
                        finished_at=_when(self._made.get(scan_id)),
                    ),
                )
                await self.server.plugins.push_update({"roots": self.server.plugins.api.scan_roots()})
        await self._settle()
        return self.server.plugins

    async def _settle(self) -> None:
        """The plugin has the new settings and language, and its hooks for them have run."""
        if self._settings_pending:  # set_setting: the plugin sees the value before the next call
            self._settings_pending = False
            await self.server.plugins.push_update({"settings": True})
        busy = [t for t in list(self.server.plugins._background) if not t.done()]
        if busy:
            await asyncio.wait(busy, timeout=35.0)

    async def settled(self) -> None:
        """Wait until what the host does in the background is done: hover fields asked for late
        (``fields("hover")`` is empty the first time, as in the app), hooks, settings and language
        changes. ``fields``, ``report`` … called after this see the results."""
        await self._ready()
        for _ in range(100):
            busy = [t for t in list(self.server.plugins._background) if not t.done()]
            if not busy and not self._settings_pending:
                return
            await self._settle()
            await asyncio.sleep(0)

    async def scan_dir(self, path: str | os.PathLike[str]) -> str:
        """Scan a real folder (a hard scan in this process), wait for it, use it."""
        from spaceknife_protocol.scan import ScanStartParams

        await self.start()
        handle = await self.server.scans.start(
            ScanStartParams(roots=[str(path)], workers=0, auto_snapshot=False)
        )
        if handle.task is not None:
            await handle.task
        self.scan_id = handle.id
        await self.server.plugins.push_update({"roots": self.server.plugins.api.scan_roots()})
        return str(handle.id)

    @property
    def store(self) -> Any:
        assert self.scan_id is not None, "scan something first"
        for sid, s in self._stores:
            if sid == self.scan_id:
                return s
        return self.server.scans.scans[self.scan_id].store

    def find(self, rel: str) -> int:
        """The node of a path relative to the root (``"a/x.bin"``); KeyError when there is none."""
        store = self.store
        node = next(iter(store.roots()))
        for part in [p for p in rel.replace("\\", "/").split("/") if p]:
            found = next((ch for ch in store.children(node) if store.name(ch) == part), None)
            if found is None:
                raise KeyError(rel)
            node = found
        return int(node)

    def node(self, rel: str = "") -> Node:
        """The node at a path relative to the root, as ``ctx.tree.node`` gives it (``size`` ``alloc``
        ``mtime`` …; folders: subtree totals); KeyError when there is none."""
        from spaceknife.layout.basis import Basis
        from spaceknife.plugins import query

        store = self.store
        return Node.from_dict(query.node_dict(store, Basis(store), self.find(rel)))

    async def legend(self, name: str) -> list[str]:
        """The texts of the legend of the plugin's colour mode ``name``, as the colour menu shows
        them now (the one of ``@plugin.color_legend`` once it answered: after :meth:`settled`)."""
        from spaceknife import i18n
        from spaceknife.extensions import EXTENSIONS

        host = await self._ready()
        c = EXTENSIONS.get("color_mode", f"{self.plugin_id}.{name}")
        if c is None:
            raise KeyError(f"no colour mode {name!r}")
        lang = i18n.language()
        host.legend_of(c, lang)  # asks the plugin (in the background) when it has a legend function
        await self.settled()
        return [str(text) for text, _colour in host.legend_of(c, lang)]

    def path(self, rel: str = "") -> str:
        """The path of a node as the app shows it (the system's form: ``\\`` on Windows), by its
        place relative to the root (``"logs/a.txt"``). Compare paths a plugin writes with this,
        not with ``files_root / "…"`` joined by hand."""
        return str(self.store.path(self.find(rel)))

    # ------------------------------------------------------------- calling the plugin

    def _place(self, host: Any, params: Mapping[str, Any]) -> Any:
        """The context the app gives a call from the frontend with these params (``scan_id``,
        ``node``, ``selection`` … of an ``ActionContext``), on the scan in front by default."""
        return host.places.request({"scan_id": self.scan_id, **params})

    async def call(self, name: str, /, **params: Any) -> Any:
        """A method: its JSON result (a dict). Params that name a context (``scan_id``, ``node``
        …) are its ``ctx.where`` as in the app; ``ctx.tree`` is the scan in front by default."""
        host = await self._ready()
        value, _frames = await host.call(
            self.plugin_id, "method", name, params, where=self._place(host, params), session=self.session
        )
        return value

    async def call_frames(self, name: str, /, **params: Any) -> tuple[Any, list[Any]]:
        """A method with binary results: (JSON, decoded frames as dicts of arrays)."""
        from spaceknife_protocol.binary import decode_frame

        host = await self._ready()
        value, frames = await host.call(
            self.plugin_id, "method", name, params, where=self._place(host, params), session=self.session
        )
        return value, [decode_frame(f)[1] for f in frames]

    async def report(self, name: str, /, node: int | None = None, **ctx: Any) -> Report:
        host = await self._ready()
        params = {"scan_id": self.scan_id, "node": node, **ctx}
        value, _ = await host.call(
            self.plugin_id, "report", name, params, where=self._place(host, params), session=self.session
        )
        return Report.model_validate(host.api.checked_report(value))

    def text(self, report: Report | Mapping[str, Any]) -> str:
        return render_text(report)

    async def fields(self, slot: str = "inspector", *, node: int) -> list[Any]:
        """The fields the details panel (``"inspector"``) or the hover card (``"hover"``: empty the
        first time, as in the app; :meth:`settled` then ask again) shows for ``node``: the host's
        ``PluginField`` (``id plugin title value unit tone node action``). A field of this plugin
        that raised raises here (``RuntimeError`` with its message); the app leaves it out."""
        from spaceknife.plugins.handlers import node_fields
        from spaceknife_protocol.plugins import PluginFieldsParams

        host = await self._ready()
        assert self.scan_id is not None
        session: Any = self.session
        params = PluginFieldsParams(
            scan_id=self.scan_id, node=node, slot="hover" if slot == "hover" else "inspector"
        )
        mine = f"{self.plugin_id}."
        for key in [k for k in host.field_errors if k[0].startswith(mine) and k[1] == node]:
            host.field_errors.pop(key, None)
        res = await node_fields(host, session, params)
        failed = [
            (k[0], why) for k, why in host.field_errors.items() if k[0].startswith(mine) and k[1] == node
        ]
        if failed:
            raise RuntimeError("; ".join(f"field {fid} failed: {why}" for fid, why in failed))
        return list(res.fields)

    async def insights(self, *, node: int) -> list[Any]:
        from spaceknife.plugins.handlers import insight_cards
        from spaceknife_protocol.plugins import InsightNodeParams

        host = await self._ready()
        assert self.scan_id is not None
        session: Any = self.session
        return list(
            (await insight_cards(host, session, InsightNodeParams(scan_id=self.scan_id, node=node))).cards
        )

    async def cleanup(self, name: str, /, **ctx: Any) -> list[Proposed]:
        host = await self._ready()
        params = {"scan_id": self.scan_id, **ctx}
        where = self._place(host, params)
        value, _ = await host.call(self.plugin_id, "cleanup", name, params, where=where, session=self.session)
        rec = host.enabled(self.plugin_id)
        res = await host.api.cleanup_result(rec, name, where, value)
        return [
            Proposed(
                int(i["node"]["node"]), str(i["path"]), str(i["reason"]), bool(i["checked"]), int(i["alloc"])
            )
            for i in res["items"]
        ]

    async def node_set(self, name: str) -> Any:
        host = await self._ready()
        full = f"{self.plugin_id}.{name}"
        masks = await asyncio.to_thread(host.set_masks, self.scan_id, self.store, [full])
        return masks.get(full)

    async def filter(self, expr: str) -> list[str]:
        """The files a filter expression matches (with the plugin's node sets), as relative paths."""
        from spaceknife.filter.engine import FilterContext, evaluate
        from spaceknife.filter.vocab import platform_vocab
        from spaceknife_protocol.filter_syntax import parse

        host = await self._ready()
        parsed = parse(expr, None, platform_vocab())
        if parsed.normal is None:
            raise ValueError([d.code for d in parsed.diagnostics])
        store = self.store
        ids = [*parsed.normal.sets, *parsed.normal.not_sets]
        sets = await asyncio.to_thread(host.set_masks, self.scan_id, store, ids) if ids else None
        res = evaluate(store, parsed.normal, FilterContext.current(sets=sets))
        return _relative(store, res.match.nonzero()[0].tolist())

    async def visual(self, name: str, *, width: int = 1280, height: int = 800) -> Any:
        """A frame of the plugin's visualization (the draw list, checked with the row contract)."""
        return await self._frame({"visual": f"{self.plugin_id}.{name}"}, width, height)

    async def color(self, name: str, *, visual: str = "treemap", width: int = 1280, height: int = 800) -> Any:
        """A frame in the plugin's colour mode: its ``pal`` column is the palette index per row."""
        return await self._frame({"visual": visual, "color_mode": f"{self.plugin_id}.{name}"}, width, height)

    async def _frame(self, settings: dict[str, Any], width: int, height: int) -> Any:
        from spaceknife.layout.contract import check_draw
        from spaceknife.server.views import apply_settings
        from spaceknife_protocol import view as pv

        await self._ready()
        assert self.scan_id is not None
        views = self.server.views
        vhost = views.open(
            self.session,
            pv.ViewOpenParams(scan_id=self.scan_id, viewport=pv.Viewport(width=width, height=height)),
        )
        apply_settings(vhost.view, pv.ViewSettingsParams(**settings), self.session.capabilities)
        from spaceknife.layout import registry
        from spaceknife.server.compute import compute

        view = vhost.view
        frame = await compute(view.render)
        vis = registry.get(view.visual)
        sizes = vis.sizes(view.basis) if vis.sizes is not None else None
        problems = check_draw(frame.draw, view.store, view._root_spec(vis), full=True, sizes=sizes)
        views.close(view.id)
        if problems:
            raise AssertionError("the frame breaks the row contract: " + "; ".join(problems[:5]))
        meta = frame.draw.meta
        if isinstance(meta, pv.ShapesGeometry) and meta.message:  # @plugin.layout failed
            raise AssertionError(f"the layout drew nothing: {meta.message}")
        return frame

    async def export(
        self,
        name: str,
        *,
        root: int = 0,
        filter: str | None = None,
        options: Mapping[str, Any] | None = None,
        selection: Sequence[int] | None = None,
    ) -> str | bytes:
        """The output of the plugin's export format (or template) for the tree below ``root``, as
        the export dialog makes it: ``filter`` (an expression: only what it matches), ``options``
        (the format's options; the rest take their defaults), ``selection`` (only these nodes).
        Option values are checked as the app checks them: an unknown name, a value that is not one
        of a select's or a multi's choices, a number out of range raise ``RpcError``."""
        from spaceknife.export import common_root, export_chunks, resolve, subtree_mask
        from spaceknife.extensions import EXTENSIONS
        from spaceknife.filter.engine import FilterContext, evaluate
        from spaceknife.filter.vocab import platform_vocab
        from spaceknife.server.compute import compute
        from spaceknife.server.export_handlers import check_options
        from spaceknife_protocol.filter_syntax import parse

        host = await self._ready()
        target = resolve(f"{self.plugin_id}.{name}")
        fmt = EXTENSIONS.get("export_format", f"{self.plugin_id}.{name}")
        given = dict(options or {})
        if fmt is not None and getattr(fmt.spec, "options", None) is not None:
            given = check_options(list(fmt.spec.options), given)
        store = self.store
        match = None
        if filter:
            parsed = parse(filter, None, platform_vocab())
            if parsed.normal is None:
                raise ValueError([d.code for d in parsed.diagnostics])
            ids = [*parsed.normal.sets, *parsed.normal.not_sets]
            sets = await asyncio.to_thread(host.set_masks, self.scan_id, store, ids) if ids else None
            match = evaluate(store, parsed.normal, FilterContext.current(sets=sets)).match
        picked = [int(x) for x in selection or []]
        if picked:
            root = common_root(store, picked)
            if len(picked) > 1:
                inside = subtree_mask(store, picked)
                match = inside if match is None else (match & inside)

        def run() -> list[str | bytes]:
            return list(
                export_chunks(
                    target,
                    store,
                    root,
                    match=match,
                    filter_text=filter or "",
                    options=given,
                    selection=picked,
                )
            )

        parts = await compute(run)
        if parts and isinstance(parts[0], bytes):
            return b"".join(p for p in parts if isinstance(p, bytes))
        return "".join(p for p in parts if isinstance(p, str))

    async def export_text(self, name: str, **kw: Any) -> str:
        """:meth:`export` of a text format (str)."""
        out = await self.export(name, **kw)
        if not isinstance(out, str):
            raise TypeError(f"{name!r} wrote bytes: use export_bytes")
        return out

    async def export_bytes(self, name: str, **kw: Any) -> bytes:
        """:meth:`export` of a binary format (``binary = true``)."""
        out = await self.export(name, **kw)
        if not isinstance(out, bytes):
            raise TypeError(f"{name!r} wrote text: use export_text")
        return out

    async def source(self, name: str, uri: str) -> str:
        """Scan ``uri`` with the plugin's scan source; returns the scan id (now in front). Raises
        RuntimeError with the plugin's message when the listing failed (as the app's status bar
        shows it); :meth:`status` tells how a scan stands."""
        from spaceknife_protocol.scan import ScanStartParams

        await self._ready()
        handle = await self.server.scans.start(ScanStartParams(source=f"{self.plugin_id}.{name}", uri=uri))
        if handle.task is not None:
            await handle.task
        self.scan_id = handle.id
        st = handle.status()
        if st.state == "failed":
            raise RuntimeError(f"scan source {name!r} failed on {uri}: {st.message}")
        return str(handle.id)

    def status(self, scan_id: str | None = None) -> Any:
        """A scan's status (``scan.status``: state, items, message, monitor …)."""
        sid = scan_id or self.scan_id
        assert sid is not None, "scan something first"
        return self.server.scans.scans[sid].status()

    # ------------------------------------------------------------- what the plugin registered

    def _owned(self, kind: str) -> list[Any]:
        from spaceknife.extensions import EXTENSIONS

        return [c for c in EXTENSIONS.of(kind) if c.owner == self.plugin_id]

    def commands(self) -> dict[str, Any]:
        """The plugin's commands as the palette has them: full id -> the command (``.name``,
        ``.keys``, ``.action``, ``.menus`` …); filter presets are among them. Run one with
        :meth:`command`, ask whether it can run with :meth:`command_available`."""
        return {c.id: c.impl for c in self._owned("command")}

    def _command(self, name: str) -> Any:
        cid = name if "." in name else f"{self.plugin_id}.{name}"
        cmd = self.commands().get(cid)
        if cmd is None:
            raise KeyError(f"no command {name!r}: the plugin has {sorted(self.commands())}")
        return cmd

    def _where(
        self, node: int | None, selection: Sequence[int] | None, zoom_node: int | None, filter: str
    ) -> dict[str, Any]:
        sel = [int(n) for n in selection] if selection is not None else ([node] if node is not None else [])
        return {
            "scan_id": self.scan_id,
            "view_id": None,
            "node": node,
            "selection": sel,
            "zoom_node": zoom_node,
            "filter": filter,
            "compare_id": None,
        }

    async def command(
        self,
        name: str,
        /,
        *,
        node: int | None = None,
        selection: Sequence[int] | None = None,
        zoom_node: int | None = None,
        filter: str = "",
        **args: Any,
    ) -> Any:
        """Run one of the plugin's commands as the app does on ``node`` (a context menu's block, or
        the selection; its ``when`` and ``check`` are not asked: :meth:`command_available` does).
        ``args`` are added to the action's own, as a panel's ``sk.host.command(id, args)`` does. A
        ``call`` action calls its method with the :class:`ActionContext` and returns what it
        returned; a ``cleanup`` action runs the cleanup and returns its candidates (as
        :meth:`cleanup`); any other action is returned as a dict with the args it would carry
        (``{"kind": "open", "target": …, "args": …}``)."""
        cmd = self._command(name)
        action = getattr(cmd, "action", None)
        if action is None:
            return None
        kind = getattr(action, "kind", "")
        merged = {**(getattr(action, "args", None) or {}), **args}
        if kind == "cleanup":
            owner, _, local = action.cleanup.rpartition(".")
            if owner != self.plugin_id:
                raise ValueError(f"{name!r} runs another plugin's cleanup ({action.cleanup})")
            return await self.cleanup(local, **self._where(node, selection, zoom_node, filter), args=merged)
        if kind != "call":
            out = action.model_dump(mode="json") if hasattr(action, "model_dump") else dict(action)
            if "args" in out:
                out["args"] = merged
            return out
        host = await self._ready()
        ctx = self._where(node, selection, zoom_node, filter) | {"args": merged}
        method = action.method.rsplit(".", 1)[-1]
        value, _ = await host.call(
            self.plugin_id, "method", method, ctx, where=host.places.request(ctx), session=self.session
        )
        return value

    async def command_available(self, name: str, /, *, node: int | None = None) -> str | None:
        """Why the command cannot run with ``node`` selected in a view of the scan in front (its
        ``when``, then its ``check`` method, as the palette and the menus ask), or None when it can."""
        return await self._available(self._command(name), node)

    async def visual_available(self, name: str, /) -> str | None:
        """Why the plugin's visualization ``name`` cannot be picked in a view of the scan in front
        (its ``when``, as the visualization switch and 「可视化：…」 ask), or None when it can."""
        from spaceknife.palette import commands as pal

        await self._ready()
        cid = f"view.visual.{self.plugin_id}.{name}"
        cmd = next((c for c in pal.registry() if c.id == cid), None)
        if cmd is None:
            raise KeyError(f"no visualization {name!r}")
        return await self._available(cmd, None)

    async def _available(self, cmd: Any, node: int | None) -> str | None:
        from spaceknife.palette.commands import Context as PaletteContext
        from spaceknife_protocol import view as pv

        host = await self._ready()
        assert self.scan_id is not None, "scan something first"
        vhost = self.server.views.open(
            self.session, pv.ViewOpenParams(scan_id=self.scan_id, viewport=pv.Viewport(width=800, height=600))
        )
        view = vhost.view
        view.selected = node
        handle = self.server.scans.scans.get(self.scan_id)
        # as the palette asks (a when of "source:<id>"): the full id of the scan source, else ""
        source = str(getattr(handle, "plugin_source", None) or "") if handle is not None else ""
        pc = PaletteContext(
            tab="scan",
            has_scan=True,
            view=view,
            settings=self.server.settings,
            plugins=frozenset(host.enabled_ids()),
            scan_source=source,
            caps=frozenset(self.session.capabilities),
            scan_id=self.scan_id,
        )

        def ask() -> str | None:
            for need in cmd.needs:
                why = need(pc)
                if why:
                    return str(why)
            return None

        try:
            return await asyncio.to_thread(ask)
        finally:
            self.server.views.close(view.id)

    def keys(self) -> dict[str, str]:
        """Key -> command id, for every key the plugin binds (its commands' keys and keybindings)."""
        out = {k: cid for cid, cmd in self.commands().items() for k in getattr(cmd, "keys", ())}
        for c in self._owned("keybinding"):
            cmd = c.spec.command
            out[c.spec.key] = cmd if "." in cmd else f"{self.plugin_id}.{cmd}"
        return out

    def presets(self) -> dict[str, str]:
        """Filter presets: name -> expression."""
        return {c.local: c.spec.expr for c in self._owned("filter_preset")}

    async def hook(self, event: str, **args: Any) -> None:
        """Run the plugin's hooks for ``event`` as the host would, raising what they raise.
        ``scan_done`` / ``scan_closed`` take ``scan`` (default: the scan in front),
        ``tree_changed`` takes ``change`` (a :class:`TreeChange` or dict; default: the root of the
        scan in front changed), ``snapshots`` takes ``change`` (a :class:`SnapshotsChange` or
        dict), ``settings`` takes ``changed`` (the names of its settings that changed), the others
        nothing. ``ctx.tree`` of ``scan_done`` and ``tree_changed`` is that scan; ``scan_closed``
        has none (reading it raises ``PluginError``: in the app the scan is gone). A hook that needs
        a permission the plugin lacks (``snapshots``: ``snapshots.read``) is PermissionError: the
        app would leave the plugin out."""
        from spaceknife.extensions import EXTENSIONS
        from spaceknife_sdk.manifest import HOOK_PERMISSIONS, HOOKS

        if event not in HOOKS:
            raise ValueError(f"unknown hook event {event!r}: one of {', '.join(HOOKS)}")
        allowed = {
            "scan_done": {"scan"},
            "scan_closed": {"scan"},
            "settings": {"changed"},
            "tree_changed": {"change"},
            "snapshots": {"change"},
        }.get(event, set())
        extra = set(args) - allowed
        if extra:
            raise TypeError(f"hook {event!r} takes {sorted(allowed) or 'nothing'}, not {sorted(extra)}")
        host = await self._ready()
        need = HOOK_PERMISSIONS.get(event)
        if need is not None and need not in host.records[self.plugin_id].granted:
            raise PermissionError(f"the {event!r} hook needs {need!r}: the app leaves this plugin out")
        if event in ("scan_done", "scan_closed") and "scan" not in args:
            args["scan"] = {"scan_id": self.scan_id, "roots": [self.path()], "state": "done"}
        if event == "settings":
            args.setdefault("changed", [])
        if event == "tree_changed" and "change" not in args:
            store = self.store
            roots = [int(r) for r in store.roots()]
            alloc = [int(store.cols["tot_alloc"][r]) for r in roots]
            args["change"] = {
                "scan_id": self.scan_id,
                "epoch": int(store.epoch),
                "version": int(store.version),
                "folders": roots,
                "paths": [str(store.path(r)) for r in roots],
                "before": alloc,
                "after": alloc,
                "changes": 1,
            }
        if event == "snapshots":
            args.setdefault("change", {})
        for k in ("scan", "change"):
            if k in args and hasattr(args[k], "model_dump"):
                args[k] = args[k].model_dump(mode="json")
        where = host.places.hook(event, args)  # as the app: scan_closed has no tree, its scan is gone
        for c in EXTENSIONS.owned(self.plugin_id):
            if c.kind == "hook" and c.spec.event == event:
                await host.call(
                    self.plugin_id,
                    "hook",
                    c.local,
                    {"event": event, **args},
                    where=where,
                    session=self.session,
                )

    async def changed(self, *nodes: int | str, scan_id: str | None = None) -> None:
        """The tree of the scan in front (or ``scan_id``) changed at these nodes (ids, or paths
        relative to its root; files or folders), as when the app applies file changes: the
        plugin's ``tree_changed`` hook runs the way it does in the app (the changes of about a
        second together, the folders where they happened). Waits until it ran. The tree itself
        stays as it is (each folder's ``before`` is its ``after``): to see sizes change, change
        files in a folder from :meth:`scan_dir`, whose monitor applies them as the app does."""
        from spaceknife.plugins.live import changed_folders, sizes_now

        host = await self._ready()
        sid = scan_id or self.scan_id
        assert sid is not None, "scan something first"
        ids = [self.find(n) if isinstance(n, str) else int(n) for n in nodes]
        changes = {i: 0 for i in ids}
        store = host.server.scans.scans[sid].store
        host.live.tree_changed(sid, changes, sizes_now(store, changed_folders(store, changes)))
        await self.settled()

    async def task(
        self,
        name: str,
        /,
        params: Any = None,
        *,
        key: str = "",
        wait: bool = True,
        timeout: float = 60.0,  # noqa: ASYNC109 — how long a test waits for the task, at most
    ) -> TaskInfo:
        """Start the background task ``name`` as ``ctx.tasks.start`` would and (``wait``) wait until
        it ends; returns its :class:`TaskInfo` (``state`` is done, failed or cancelled, ``error`` why
        it failed, ``done total text`` its last progress). ``timeout``: seconds to wait at most."""
        host = await self._ready()
        rec = host.records[self.plugin_id]
        body = params.model_dump(mode="json") if hasattr(params, "model_dump") else dict(params or {})
        t = host.tasks.start(rec, name, body, key, scan_id=self.scan_id)  # ctx.tree: the scan in front
        if wait and t.runner is not None:
            _done, pending = await asyncio.wait({t.runner}, timeout=timeout)
            if pending:
                raise TimeoutError(f"the task {name!r} still runs after {timeout:g} s")
        return TaskInfo.model_validate(host.tasks.info(t).model_dump())

    def tasks(self) -> list[TaskInfo]:
        """This plugin's background tasks (:class:`TaskInfo`): running ones and the last that ended."""
        if self.server is None:
            return []
        return [
            TaskInfo.model_validate(t.model_dump()) for t in self.server.plugins.tasks.list(self.plugin_id)
        ]

    async def cancel_task(self, name: str, /, *, key: str | None = None, wait: bool = True) -> int:
        """Cancel the task ``name`` (of ``key`` only, when given), as the cancel button would; with
        ``wait``, until it has ended. Returns how many were cancelled."""
        host = await self._ready()
        running = [
            t
            for t in list(host.tasks._tasks.values())
            if t.plugin == self.plugin_id and t.name == name and (key is None or t.key == key) and t.running
        ]
        n: int = host.tasks.cancel_named(self.plugin_id, name, key)
        runners = {t.runner for t in running if t.runner is not None}
        if wait and runners:
            await asyncio.wait(runners, timeout=10.0)
        return n

    async def annotate(self, items: Sequence[Mapping[str, Any]], name: str | None = None) -> list[str | None]:
        """A change annotator's reasons for comparison items (``path``, ``kind``, ``status``,
        ``delta``, ``base``, ``target``; missing ones take ChangeUnit's defaults). Its ``ctx.tree``
        is the scan in front, as in the app the scan a comparison was opened from."""
        host = await self._ready()
        if name is None:
            rec = host.records[self.plugin_id]
            name = next(
                h[1] for h in (rec.description.handlers if rec.description else []) if h[0] == "annotate"
            )
        value, _ = await host.call(
            self.plugin_id,
            "annotate",
            name,
            {"items": [dict(i) for i in items]},
            where=host.places.comparison(None, self.scan_id),
        )
        return [None if v is None else str(v) for v in value]

    def snapshot(self, *, when: Any = None) -> str:
        """Save the scan in front as a snapshot (where the app keeps them); returns its path. For
        plugins that read history (``ctx.snapshots``); ``when`` (a datetime) is when it was taken:
        its file name and the time in it (``SnapshotInfo.created_at``)."""
        from spaceknife.store import snapshot as snap

        store = self.store
        roots = [store.name(r) for r in store.roots()]
        path = snap.auto_path(roots, when)  # a free name, as the app's automatic snapshots
        path.parent.mkdir(parents=True, exist_ok=True)
        c = store.cols
        files, dirs = int(c["tot_files"][0]), int(c["tot_dirs"][0])
        stats = {  # the totals the app writes in a snapshot's header (snapshots.list() shows them)
            "items": files + dirs,
            "files": files,
            "dirs": dirs,
            "bytes_alloc": int(c["tot_alloc"][0]),
            "bytes_size": int(c["tot_size"][0]),
            "errors": 0,
            "state": "done",
            "roots": [{"path": r, "whole_volume": False} for r in roots],
            "method": "hard",
        }
        snap.save(store, path, roots=roots, stats=stats, created_at=when)  # its time is `when` too
        if self._started:  # as the app: the windows and the plugins' snapshots hooks hear of it
            with contextlib.suppress(RuntimeError):
                self.server.scans._snapshots_changed([str(path)], [])
        return str(path)

    # ------------------------------------------------------------- side effects

    @property
    def events(self) -> dict[str, list[Any]]:
        prefix = f"plugin.{self.plugin_id}."
        out: dict[str, list[Any]] = {}
        for name, payload in self.session.sent:
            if name.startswith(prefix):
                out.setdefault(name[len(prefix) :], []).append(payload)
        return out

    @property
    def toasts(self) -> list[str]:
        return [p["text"] for n, p in self.session.sent if n == "plugin.toast"]

    @property
    def notices(self) -> list[dict[str, Any]]:
        """``ctx.notify`` notices the app showed, oldest first: ``title body tone command button
        args`` (``command`` the full id of the button's command, or None)."""
        return [dict(p) for n, p in self.session.sent if n == "plugin.notice"]

    @property
    def clipboard(self) -> list[str]:
        """What ``ctx.clipboard.write_text`` copied, oldest first (calls of this test's session)."""
        return [p["text"] for n, p in self.session.sent if n == "plugin.clipboard"]

    def state(self, name: str) -> dict[str, Any]:
        rec = self.server.plugins.records[self.plugin_id]
        return dict(rec.states.get(name, {}))

    @property
    def basket(self) -> list[Any]:
        return list(self.server.ops.basket.items())

    def set_setting(self, name: str, value: Any) -> _Settled:
        """Change one of the plugin's settings (as the settings page would): the host drops what it
        cached of the plugin's answers and runs its ``settings`` hook (``await`` the result to wait
        for the hook; the next call waits for it anyway)."""
        if self.server is None:
            raise RuntimeError("start the host first (async with MockHost.load(...) as host)")
        self.server.settings.update({f"plugin.{self.plugin_id}.{name}": value})
        self._settings_pending = True
        return _Settled(self)

    def set_lang(self, lang: str) -> _Settled:
        """Switch the interface language (``"en"`` or ``"zh-CN"``), as the settings page does: the
        plugin's ``lang`` hook runs; texts come in that language from the next call on."""
        from spaceknife import i18n

        code: Literal["en", "zh-CN"] = "en" if lang.startswith("en") else "zh-CN"
        self._env.setdefault("SPACEKNIFE_LANGUAGE", os.environ.get("SPACEKNIFE_LANGUAGE"))
        os.environ["SPACEKNIFE_LANGUAGE"] = code  # what the host's language follows in tests
        i18n.system_language.cache_clear()
        i18n.set_language(code)
        if self.server is not None:
            self.server.settings.update({"general.language": code})  # stays when settings change
        return _Settled(self)

    # ------------------------------------------------------------- checks

    async def check(self, *, strict: bool = False) -> None:
        """What ``plugin check`` checks, on this plugin as it runs here: the files and the
        description, then every code contribution once on synthetic trees. Raises AssertionError
        listing every problem (warnings too with ``strict``)."""
        from spaceknife.plugins.check import check_generated, run_contracts, static_issues
        from spaceknife_sdk.imports import check_imports
        from spaceknife_sdk.manifest import load_manifest

        host = await self._ready()
        rec = host.records[self.plugin_id]
        assert rec.description is not None
        manifest = load_manifest(self.folder)
        issues = static_issues(self.folder, manifest, rec.description)  # as plugin check does
        issues += check_imports(self.folder)
        if not manifest.builtin:
            issues += check_generated(self.folder, rec.description, manifest)
        scan_id = self.scan_id
        try:
            issues += await run_contracts(self, rec.description)
        finally:
            self.scan_id = scan_id
        bad = [i for i in issues if i.severity == "error" or (strict and i.severity == "warning")]
        if bad:
            raise AssertionError("plugin check:\n" + "\n".join("  " + i.text() for i in bad))

    # ------------------------------------------------------------- without async

    def call_sync(self, name: str, /, **params: Any) -> Any:
        async def run() -> Any:
            try:
                return await self.call(name, **params)
            finally:
                await self.close()

        return asyncio.run(run())


def _when(t: float | None) -> Any:
    from datetime import UTC, datetime

    return None if t is None else datetime.fromtimestamp(t, UTC)


def _relative(store: Any, ids: list[int]) -> list[str]:
    root = store.name(next(iter(store.roots())))
    return sorted(os.path.relpath(store.path(int(i)), root).replace(os.sep, "/") for i in ids)


__all__ = ["MockHost", "Proposed", "Tree", "make_tree", "plugin_module"]
