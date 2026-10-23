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
import os
import shutil
import tempfile
import time
from collections.abc import Mapping, Sequence
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from spaceknife_protocol.plugins import Report

from .manifest import MANIFEST
from .report import render_text


@dataclass(frozen=True, slots=True)
class Tree:
    """A synthetic tree for :meth:`MockHost.scan` (from :func:`make_tree`)."""

    spec: Mapping[str, Any]
    root: str | None = None


def make_tree(spec: Mapping[str, Any], *, root: str | None = None) -> Tree:
    """``{name: size | (size, mtime) | bytes | {…}}``: a file (size, modified time as unix seconds or
    ``"YYYY-MM-DD"``, or real contents) or a folder."""
    return Tree(spec, root)


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
            await self.server.plugins.close()
            await self.server.scans.close()
        for key, value in self._env.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value
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
            self.files_root = Path(
                tempfile.mkdtemp(prefix="sk-mocktree-", dir=self.home if self.home.exists() else None)
            )
            root = tree.root or str(self.files_root)
            store, contents = from_spec(tree.spec, root, now)
            for rel, data in contents:
                target = Path(root) / rel
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(data)
        else:
            store = tree
        scan_id = f"scan-mock{len(self._stores) + 1}"
        self._stores.append((scan_id, store))
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
                        started_at=None,
                        finished_at=None,
                    ),
                )
                await self.server.plugins.push_update({"roots": self.server.plugins.api.scan_roots()})
        if self._settings_pending:  # set_setting: the plugin sees the value before the next call
            self._settings_pending = False
            await self.server.plugins.push_update({"settings": True})
        return self.server.plugins

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
        """The node of a path relative to the root (``"a/x.bin"``)."""
        store = self.store
        node = next(iter(store.roots()))
        for part in [p for p in rel.replace("\\", "/").split("/") if p]:
            node = next(ch for ch in store.children(node) if store.name(ch) == part)
        return int(node)

    # ------------------------------------------------------------- calling the plugin

    def _ctx(self, node: int | None = None) -> dict[str, Any]:
        out: dict[str, Any] = {"scan_id": self.scan_id}
        if node is not None:
            out["node"] = node
        return out

    async def call(self, name: str, /, **params: Any) -> Any:
        """A method: its JSON result (a dict)."""
        host = await self._ready()
        value, _frames = await host.call(
            self.plugin_id, "method", name, params, session=self.session, ctx=self._ctx()
        )
        return value

    async def call_frames(self, name: str, /, **params: Any) -> tuple[Any, list[Any]]:
        """A method with binary results: (JSON, decoded frames as dicts of arrays)."""
        from spaceknife_protocol.binary import decode_frame

        host = await self._ready()
        value, frames = await host.call(
            self.plugin_id, "method", name, params, session=self.session, ctx=self._ctx()
        )
        return value, [decode_frame(f)[1] for f in frames]

    async def report(self, name: str, /, node: int | None = None, **ctx: Any) -> Report:
        host = await self._ready()
        params = {"scan_id": self.scan_id, "node": node, **ctx}
        value, _ = await host.call(
            self.plugin_id, "report", name, params, session=self.session, ctx=self._ctx(node)
        )
        return Report.model_validate(host.api.checked_report(value))

    def text(self, report: Report | Mapping[str, Any]) -> str:
        return render_text(report)

    async def fields(self, slot: str = "inspector", *, node: int) -> list[Any]:
        from spaceknife.plugins.handlers import node_fields
        from spaceknife_protocol.plugins import PluginFieldsParams

        host = await self._ready()
        assert self.scan_id is not None
        session: Any = self.session
        params = PluginFieldsParams(
            scan_id=self.scan_id, node=node, slot="hover" if slot == "hover" else "inspector"
        )
        res = await node_fields(host, session, params)
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
        value, _ = await host.call(
            self.plugin_id, "cleanup", name, params, session=self.session, ctx=self._ctx()
        )
        rec = host.enabled(self.plugin_id)
        res = await host.api.cleanup_result(rec, name, params, value)
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
        return frame

    async def export(self, name: str, *, root: int = 0) -> str | bytes:
        """The output of the plugin's export format (or template) for the tree below ``root``."""
        from spaceknife.export import export_chunks, resolve
        from spaceknife.server.compute import compute

        await self._ready()
        target = resolve(f"{self.plugin_id}.{name}")

        def run() -> list[str | bytes]:
            return list(export_chunks(target, self.store, root))

        parts = await compute(run)
        if parts and isinstance(parts[0], bytes):
            return b"".join(p for p in parts if isinstance(p, bytes))
        return "".join(p for p in parts if isinstance(p, str))

    async def source(self, name: str, uri: str) -> str:
        """Scan ``uri`` with the plugin's scan source; returns the scan id (now in front)."""
        from spaceknife_protocol.scan import ScanStartParams

        await self._ready()
        handle = await self.server.scans.start(ScanStartParams(source=f"{self.plugin_id}.{name}", uri=uri))
        if handle.task is not None:
            await handle.task
        self.scan_id = handle.id
        return str(handle.id)

    async def hook(self, event: str, **args: Any) -> None:
        host = await self._ready()
        await host._hook(host.enabled(self.plugin_id), event, **args)

    async def annotate(self, items: Sequence[Mapping[str, Any]], name: str | None = None) -> list[str | None]:
        """A change annotator's reasons for comparison items (``path``, ``kind``, ``status``,
        ``delta``, ``base``, ``target``; missing ones take ChangeUnit's defaults)."""
        host = await self._ready()
        if name is None:
            rec = host.records[self.plugin_id]
            name = next(
                h[1] for h in (rec.description.handlers if rec.description else []) if h[0] == "annotate"
            )
        value, _ = await host.call(self.plugin_id, "annotate", name, {"items": [dict(i) for i in items]})
        return [None if v is None else str(v) for v in value]

    def snapshot(self, *, when: Any = None) -> str:
        """Save the scan in front as a snapshot (where the app keeps them); returns its path. For
        plugins that read history (``ctx.snapshots``); ``when`` is a datetime for the file name."""
        from spaceknife.store import snapshot as snap

        store = self.store
        roots = [store.name(r) for r in store.roots()]
        path = snap.auto_path(roots, when)
        path.parent.mkdir(parents=True, exist_ok=True)
        snap.save(store, path, roots=roots)
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

    def state(self, name: str) -> dict[str, Any]:
        rec = self.server.plugins.records[self.plugin_id]
        return dict(rec.states.get(name, {}))

    @property
    def basket(self) -> list[Any]:
        return list(self.server.ops.basket.items())

    def set_setting(self, name: str, value: Any) -> None:
        """Change one of the plugin's settings (as the settings page would)."""
        self.server.settings.update({f"plugin.{self.plugin_id}.{name}": value})
        self._settings_pending = True

    def set_lang(self, lang: str) -> None:
        from spaceknife import i18n

        i18n.set_language("en" if lang.startswith("en") else "zh-CN")

    # ------------------------------------------------------------- checks

    async def check(self, *, strict: bool = False) -> None:
        """What ``plugin check`` checks, on this plugin as it runs here: the files and the
        description, then every code contribution once on synthetic trees. Raises AssertionError
        listing every problem (warnings too with ``strict``)."""
        from spaceknife.plugins.check import run_contracts, static_issues
        from spaceknife_sdk.manifest import load_manifest

        host = await self._ready()
        rec = host.records[self.plugin_id]
        assert rec.description is not None
        issues = static_issues(self.folder, load_manifest(self.folder), rec.description)
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


def _relative(store: Any, ids: list[int]) -> list[str]:
    root = store.name(next(iter(store.roots())))
    return sorted(os.path.relpath(store.path(int(i)), root).replace(os.sep, "/") for i in ids)


__all__ = ["MockHost", "Proposed", "Tree", "make_tree"]
