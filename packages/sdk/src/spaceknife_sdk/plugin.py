"""The declaration API: ``plugin = Plugin()`` and its decorators.

Nothing here does any I/O at import time besides reading the plugin's own ``plugin.toml``: every
declaration is recorded on the :class:`Plugin`; the host asks for :meth:`Plugin.describe` (the
contributions, methods, events, states and streams with their JSON Schemas) and calls the handlers
by kind and name. Decorated functions stay plain functions the plugin can call itself.
"""

from __future__ import annotations

import inspect
import json
import re
import sys
import typing
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Literal, TypeVar, overload

from pydantic import BaseModel, ConfigDict, Field

from spaceknife_protocol.plugins import ActionContext, ReportContext
from spaceknife_protocol.text import Text, as_text

from .errors import Issue, ManifestError
from .manifest import (
    HOOKS,
    KINDS,
    MANIFEST,
    PLUGIN_API,
    AnnotatorSpec,
    BackgroundSpec,
    CleanupSpec,
    ColorModeSpec,
    CommandSpec,
    EventDecl,
    ExportFormatSpec,
    ExportTemplateSpec,
    FieldSpec,
    FileClassSpec,
    FilterPresetSpec,
    FolderRuleSpec,
    HookSpec,
    InsightSpec,
    KeySpec,
    Manifest,
    MethodDecl,
    NodeSetSpec,
    PanelSpec,
    ReportSpec,
    ScanSourceSpec,
    SettingSpec,
    Spec,
    StateDecl,
    StreamDecl,
    VisualizationSpec,
    WatcherSpec,
    load_manifest,
    spec_key,
)

F = TypeVar("F", bound=Callable[..., Any])
C = TypeVar("C", bound=type[BaseModel])

# Set by the runtime while it imports a plugin's entry (the folder and manifest it loads).
_LOADING: list[tuple[Path, Manifest]] = []
_INSTANCES: list[Plugin] = []


def _snake(name: str) -> str:
    s = re.sub(r"(?<!^)(?=[A-Z])", "_", name).lower()
    return re.sub(r"[^a-z0-9_]", "_", s).strip("_")[:24].rstrip("_") or "x"


@dataclass(slots=True)
class Handler:
    """A function the host may call: its kind (``method``, ``report``, …), name and signature."""

    kind: str
    name: str
    fn: Callable[..., Any]
    params: type[BaseModel] | None = None  # the second argument's model (methods, streams)
    result: Any = None  # the return annotation (methods: the result schema)
    arity: int = 2
    is_async: bool = False
    is_gen: bool = False  # an async generator (streams, watchers)


def _defining_locals() -> dict[str, Any]:
    """The local names where a decorator was applied (models defined inside a function, tests)."""
    frame = sys._getframe(1)
    while frame is not None and frame.f_code.co_filename == __file__:
        frame = frame.f_back  # type: ignore[assignment]
    return dict(frame.f_locals) if frame is not None else {}


def _annotations(fn: Callable[..., Any], localns: dict[str, Any] | None = None) -> dict[str, Any]:
    try:
        return typing.get_type_hints(fn, localns=localns)
    except Exception:  # unresolved forward references: the check reports missing types
        return dict(getattr(fn, "__annotations__", {}))


def _arity(fn: Callable[..., Any]) -> int:
    params = [
        p
        for p in inspect.signature(fn).parameters.values()
        if p.kind in (p.POSITIONAL_ONLY, p.POSITIONAL_OR_KEYWORD)
    ]
    return len(params)


class Description(BaseModel):
    """What a plugin adds, as the host registers it (the result of :meth:`Plugin.describe`)."""

    model_config = ConfigDict(extra="forbid", frozen=True)

    id: str
    version: str
    api: int
    contributions: list[dict[str, Any]] = Field(default_factory=list, description="{kind, spec}")
    methods: list[MethodDecl] = Field(default_factory=list)
    events: list[EventDecl] = Field(default_factory=list)
    states: list[StateDecl] = Field(default_factory=list)
    streams: list[StreamDecl] = Field(default_factory=list)
    handlers: list[tuple[str, str]] = Field(default_factory=list, description="(kind, name) the code handles")
    catalog: dict[str, str] = Field(default_factory=dict, description="Chinese -> English of its texts")


class Plugin:
    """A plugin's declarations. ``Plugin()`` reads ``plugin.toml`` next to the calling module."""

    def __init__(self, *, manifest: Manifest | None = None, dir: Path | str | None = None) -> None:
        if manifest is None:
            if _LOADING:
                folder, manifest = _LOADING[-1]
            else:
                folder = Path(dir) if dir is not None else _caller_dir()
                manifest = load_manifest(folder)
            self.dir = folder
        else:
            self.dir = Path(dir) if dir is not None else _caller_dir()
        self.manifest = manifest
        self.declared: list[tuple[str, Spec]] = []
        self.handlers: dict[tuple[str, str], Handler] = {}
        self.methods: dict[str, MethodDecl] = {}
        self.events: dict[str, type[BaseModel]] = {}
        self.states: dict[str, type[BaseModel]] = {}
        self.streams: dict[str, Handler] = {}
        self.issues: list[Issue] = []
        if _LOADING:  # the runtime is importing a plugin's entry: it looks for this instance
            _INSTANCES.append(self)

    @property
    def id(self) -> str:
        return self.manifest.id

    def __repr__(self) -> str:
        return f"Plugin({self.id!r})"

    # ------------------------------------------------------------- internals

    def _declare(self, kind: str, spec: Spec) -> Spec:
        self.declared.append((kind, spec))
        return spec

    def _handler(
        self,
        kind: str,
        name: str,
        fn: Callable[..., Any],
        *,
        params: type[BaseModel] | None = None,
    ) -> Handler:
        if (kind, name) in self.handlers:
            self.issues.append(Issue("duplicate", f"{kind} {name!r} is defined twice"))
        hints = _annotations(fn, _defining_locals())
        arity = _arity(fn)
        if params is None and kind in ("method", "stream", "task") and arity >= 2:
            names = list(inspect.signature(fn).parameters)
            ann = hints.get(names[1])
            if isinstance(ann, type) and issubclass(ann, BaseModel):
                params = ann
            else:
                self.issues.append(
                    Issue(
                        "untyped",
                        f"{kind} {name!r}: annotate the second argument with a Params model",
                        fix="async def find(ctx: Context, p: FindParams) -> FindResult",
                    )
                )
        h = Handler(
            kind=kind,
            name=name,
            fn=fn,
            params=params,
            result=hints.get("return"),
            arity=arity,
            is_async=inspect.iscoroutinefunction(fn),
            is_gen=inspect.isasyncgenfunction(fn),
        )
        self.handlers[(kind, name)] = h
        return h

    def _method(self, name: str, kind: str, timeout: float, latest: bool, public: bool, h: Handler) -> None:
        params_schema = h.params.model_json_schema() if h.params is not None else None
        result_schema = None
        if isinstance(h.result, type) and issubclass(h.result, BaseModel):
            result_schema = h.result.model_json_schema(mode="serialization")
        if name in self.methods:
            self.issues.append(Issue("duplicate", f"method {name!r} is defined twice"))
        text = inspect.getdoc(h.fn) or ""
        self.methods[name] = MethodDecl(
            name=name,
            kind=kind,
            timeout=timeout,
            latest=latest,
            public=public,
            params=params_schema,
            result=result_schema,
            doc=" ".join(text.split("\n\n", 1)[0].split())[:400],
        )

    # ------------------------------------------------------------- communication

    @overload
    def method(self, fn: F, /) -> F: ...
    @overload
    def method(
        self,
        fn: None = None,
        /,
        *,
        name: str | None = None,
        timeout: float = 30.0,
        latest: bool = False,
        public: bool = True,
    ) -> Callable[[F], F]: ...
    def method(
        self,
        fn: F | None = None,
        /,
        *,
        name: str | None = None,
        timeout: float = 30.0,
        latest: bool = False,
        public: bool = True,
    ) -> F | Callable[[F], F]:
        """A request/response method: ``plugin.<id>.<name>`` for frontends and panels."""

        def wrap(f: F) -> F:
            n = name or f.__name__.rstrip("_")
            h = self._handler("method", n, f)
            self._method(n, "method", timeout, latest, public, h)
            return f

        return wrap(fn) if fn is not None else wrap

    @overload
    def event(self, cls: C, /) -> C: ...
    @overload
    def event(self, cls: None = None, /, *, name: str | None = None) -> Callable[[C], C]: ...
    def event(self, cls: C | None = None, /, *, name: str | None = None) -> C | Callable[[C], C]:
        """An event model; ``ctx.emit(Model(...))`` sends it."""

        def wrap(c: C) -> C:
            self.events[name or _snake(c.__name__)] = c
            return c

        return wrap(cls) if cls is not None else wrap

    @overload
    def state(self, cls: C, /) -> C: ...
    @overload
    def state(self, cls: None = None, /, *, name: str | None = None) -> Callable[[C], C]: ...
    def state(self, cls: C | None = None, /, *, name: str | None = None) -> C | Callable[[C], C]:
        """A shared state model (the host keeps the latest value and pushes changes)."""

        def wrap(c: C) -> C:
            self.states[name or _snake(c.__name__)] = c
            return c

        return wrap(cls) if cls is not None else wrap

    @overload
    def stream(self, fn: F, /) -> F: ...
    @overload
    def stream(self, fn: None = None, /, *, name: str | None = None) -> Callable[[F], F]: ...
    def stream(self, fn: F | None = None, /, *, name: str | None = None) -> F | Callable[[F], F]:
        """A subscription stream: an async generator of models or :class:`Columns`."""

        def wrap(f: F) -> F:
            n = name or f.__name__.rstrip("_")
            self.streams[n] = self._handler("stream", n, f)
            return f

        return wrap(fn) if fn is not None else wrap

    # ------------------------------------------------------------- interface

    def command(self, name: str, title: Text | str, *, timeout: float = 30.0, **kw: Any) -> Callable[[F], F]:
        """A command (palette, keys, menus). Used as a decorator without ``action``, it calls the
        decorated function (a method taking an :class:`ActionContext`; ``timeout`` seconds, ≤ 600
        and ≤ the manifest's ``limits.timeout_s`` — longer work goes into a background task)."""
        index = len(self.declared)
        self._declare("command", CommandSpec(name=name, title=as_text(title), **kw))

        def wrap(f: F) -> F:
            method = f.__name__.rstrip("_")
            h = self._handler("method", method, f, params=ActionContext)
            self._method(method, "method", timeout, False, True, h)
            spec = self.declared[index][1]
            assert isinstance(spec, CommandSpec)
            if spec.action is None:
                self.declared[index] = (
                    "command",
                    spec.model_copy(update={"action": {"kind": "call", "method": method, "args": {}}}),
                )
            return f

        return wrap

    def keys(
        self, key: str, command: str, *, when: list[str] | None = None, override: bool = False
    ) -> KeySpec:
        """Bind a key to a command (its own by local name, or any command's full id)."""
        return self._declare("keybinding", KeySpec(key=key, command=command, when=when, override=override))  # type: ignore[return-value]

    def setting(self, name: str, title: Text | str, default: Any, **kw: Any) -> SettingSpec:
        return self._declare("setting", SettingSpec(name=name, title=as_text(title), default=default, **kw))  # type: ignore[return-value]

    def report(
        self,
        title: Text | str,
        *,
        name: str | None = None,
        timeout: float = 30.0,
        **kw: Any,
    ) -> Callable[[F], F]:
        """A report: ``(ctx, c: ReportContext) -> Report``, shown in its slot."""

        def wrap(f: F) -> F:
            n = name or f.__name__.rstrip("_")
            self._declare("report", ReportSpec(name=n, title=as_text(title), **kw))
            h = self._handler("report", n, f, params=ReportContext)
            self._method(n, "report", timeout, True, True, h)
            return f

        return wrap

    def node_field(self, title: Text | str, *, name: str | None = None, **kw: Any) -> Callable[[F], F]:
        """A field of the details panel / hover card: ``(ctx, node) -> FieldValue | str | None``."""

        def wrap(f: F) -> F:
            n = name or f.__name__.rstrip("_")
            self._declare("node_field", FieldSpec(name=n, title=as_text(title), **kw))
            self._handler("field", n, f)
            return f

        return wrap

    @overload
    def insight(self, fn: F, /) -> F: ...
    @overload
    def insight(
        self,
        fn: None = None,
        /,
        *,
        name: str | None = None,
        globs: list[str] | None = None,
        title: Text | str | None = None,
    ) -> Callable[[F], F]: ...
    def insight(
        self,
        fn: F | None = None,
        /,
        *,
        name: str | None = None,
        globs: list[str] | None = None,
        title: Text | str | None = None,
    ) -> F | Callable[[F], F]:
        """A code insight: ``(ctx, node) -> Insight | None``. ``globs``: only nodes whose path
        matches one of them are asked (``"**/node_modules"``) — the details panel stays fast;
        ``title``: what the enable dialog calls it (default: its name)."""

        def wrap(f: F) -> F:
            n = name or f.__name__.rstrip("_")
            spec = InsightSpec(name=n, globs=list(globs or []), title=as_text(title) if title else None)
            self._declare("insight", spec)
            self._handler("insight", n, f)
            return f

        return wrap(fn) if fn is not None else wrap

    def panel(self, name: str, title: Text | str, **kw: Any) -> PanelSpec:
        """A web panel (``web/index.html`` in a sandboxed frame, talking through the host's bridge)."""
        return self._declare("panel", PanelSpec(name=name, title=as_text(title), **kw))  # type: ignore[return-value]

    # ------------------------------------------------------------- visualizations and colours

    @overload
    def visualization(
        self, name: str, title: Text | str, *, size: Literal["code"] | None = None, **kw: Any
    ) -> Callable[[F], F]: ...
    @overload
    def visualization(
        self, name: str, title: Text | str, *, size: Literal["alloc", "size", "files", "items"], **kw: Any
    ) -> VisualizationSpec: ...
    def visualization(
        self, name: str, title: Text | str, *, size: str | None = None, **kw: Any
    ) -> VisualizationSpec | Callable[[F], F]:
        """A built-in engine with another size measure (``size="files"``); used as a decorator, the
        function gives each file a weight: ``(v: VisualInput) -> np.ndarray`` (length ``tree.n``)."""
        if size not in (None, "code"):
            spec = VisualizationSpec.model_validate(
                {"name": name, "title": as_text(title), "size": size, **kw}
            )
            self._declare("visualization", spec)
            return spec

        def wrap(f: F) -> F:
            self._declare(
                "visualization", VisualizationSpec(name=name, title=as_text(title), **{**kw, "size": "code"})
            )
            self._handler("visual", name, f)
            return f

        return wrap

    def layout(
        self,
        name: str,
        title: Text | str,
        *,
        size: Literal["alloc", "size", "files", "items"] = "alloc",
        fill: float = 0.75,
        nested: bool = True,
        max_rows: int = 5000,
        **kw: Any,
    ) -> Callable[[F], F]:
        """A visualization with a layout of its own: ``(v: LayoutInput) -> Shapes``. The host picks
        the blocks (sized by ``size``, merged below the minimum element; ``fill``: how much of its
        share of the canvas a block really gets), the function places them (circles, rectangles, convex
        polygons) and the host draws them like a built-in view (docs/plugin-dev.md 「自带布局」).
        ``nested``: a folder's blocks lie inside its shape; ``max_rows``: blocks per call at most."""

        def wrap(f: F) -> F:
            spec = VisualizationSpec.model_validate(
                {
                    "name": name,
                    "title": as_text(title),
                    "base": "shapes",
                    "size": size,
                    "fill": fill,
                    "nested": nested,
                    "max_rows": max_rows,
                    **kw,
                }
            )
            self._declare("visualization", spec)
            self._handler("layout", name, f)
            return f

        return wrap

    @overload
    def color_mode(
        self,
        name: str,
        title: Text | str,
        *,
        by: Literal["ext", "class", "age", "size", "depth", "name", "set", "insight"],
        **kw: Any,
    ) -> ColorModeSpec: ...
    @overload
    def color_mode(self, name: str, title: Text | str, *, by: None = None, **kw: Any) -> Callable[[F], F]: ...
    def color_mode(
        self, name: str, title: Text | str, *, by: str | None = None, **kw: Any
    ) -> ColorModeSpec | Callable[[F], F]:
        """Declarative with ``by=…`` (ext, class, age, size, depth, name, set, insight); used as a
        decorator: ``(c: ColorInput) -> np.ndarray[uint8]`` palette indices of the drawn rows (the
        older ``(tree, ids)`` form works too)."""
        if by is not None:
            spec = ColorModeSpec.model_validate({"name": name, "title": as_text(title), "by": by, **kw})
            self._declare("color_mode", spec)
            return spec

        def wrap(f: F) -> F:
            self._declare("color_mode", ColorModeSpec(name=name, title=as_text(title), **kw))
            self._handler("color", name, f)
            return f

        return wrap

    def color_legend(self, mode: str) -> Callable[[F], F]:
        """The legend of a colour mode, worked out when it is shown: ``(ctx) -> list[text | (text,
        colour)]``, one entry per palette index (index 0 first). It follows the plugin's settings
        (asked again whenever they change, or after ``ctx.invalidate("color_mode")``)."""

        def wrap(f: F) -> F:
            self._handler("legend", mode, f)
            return f

        return wrap

    # ------------------------------------------------------------- data

    def export_format(self, name: str, title: Text | str, **kw: Any) -> Callable[[F], F]:
        """An export format: ``(x: ExportInput) -> Iterator[str]`` (``bytes`` with binary=True).
        ``options=[{"name": …, "title": …, "default": …}, …]`` (setting fields) are drawn in the
        export dialog; their values arrive in ``x.options``. The older ``(tree, root, match[, info])``
        form works too."""

        def wrap(f: F) -> F:
            self._declare("export_format", ExportFormatSpec(name=name, title=as_text(title), **kw))
            self._handler("export", name, f)
            return f

        return wrap

    def export_template(self, file: str, name: str, title: Text | str, **kw: Any) -> ExportTemplateSpec:
        return self._declare(
            "export_template", ExportTemplateSpec(file=file, name=name, title=as_text(title), **kw)
        )  # type: ignore[return-value]

    def file_class(self, name: str, title: Text | str, **kw: Any) -> FileClassSpec:
        return self._declare("file_class", FileClassSpec(name=name, title=as_text(title), **kw))  # type: ignore[return-value]

    def folder_rule(
        self,
        glob: str,
        cls: str,
        *,
        mode: str = "swallow",
        name: str | None = None,
        priority: int = 0,
        exclude: list[str] | None = None,
    ) -> FolderRuleSpec:
        spec = FolderRuleSpec.model_validate(
            {
                "glob": glob,
                "class": cls,
                "mode": mode,
                "name": name,
                "priority": priority,
                "exclude": list(exclude or []),
            }
        )
        self._declare("folder_rule", spec)
        return spec

    def node_set(self, name: str, title: Text | str, **kw: Any) -> Callable[[F], F]:
        """A node set for the filter (``:set:<name>``): ``(ctx, tree) -> bool mask | node ids``."""

        def wrap(f: F) -> F:
            self._declare("node_set", NodeSetSpec(name=name, title=as_text(title), **kw))
            self._handler("set", name, f)
            return f

        return wrap

    def filter_preset(self, name: str, title: Text | str, expr: str) -> FilterPresetSpec:
        return self._declare("filter_preset", FilterPresetSpec(name=name, title=as_text(title), expr=expr))  # type: ignore[return-value]

    # ------------------------------------------------------------- operations, sources, history

    def cleanup(
        self, title: Text | str, *, name: str | None = None, timeout: float = 120.0, **kw: Any
    ) -> Callable[[F], F]:
        """A cleanup: ``(ctx, c: ReportContext) -> list[Proposal]``; the user reviews the candidates,
        then they go to the delete basket."""

        def wrap(f: F) -> F:
            n = name or f.__name__.rstrip("_")
            self._declare("cleanup", CleanupSpec(name=n, title=as_text(title), **kw))
            h = self._handler("cleanup", n, f, params=ReportContext)
            self._method(n, "cleanup", timeout, True, True, h)
            return f

        return wrap

    def scan_source(self, name: str, title: Text | str, **kw: Any) -> Callable[[F], F]:
        """A scan source: ``(ctx, uri, out: Listing) -> None`` lists what a URI holds."""

        def wrap(f: F) -> F:
            self._declare("scan_source", ScanSourceSpec(name=name, title=as_text(title), **kw))
            self._handler("source", name, f)
            return f

        return wrap

    def watcher(
        self, source: str, *, name: str | None = None, title: Text | str | None = None
    ) -> Callable[[F], F]:
        """Changes of a source's URI: an async generator ``(ctx, uri) -> list[Change]`` batches;
        ``title``: what the enable dialog calls it (default: 「监控 <the source's title>」)."""

        def wrap(f: F) -> F:
            n = name or f.__name__.rstrip("_")
            spec = WatcherSpec(name=n, source=source, title=as_text(title) if title else None)
            self._declare("watcher", spec)
            self._handler("watch", n, f)
            return f

        return wrap

    @overload
    def change_annotator(self, fn: F, /) -> F: ...
    @overload
    def change_annotator(self, fn: None = None, /, *, name: str | None = None) -> Callable[[F], F]: ...
    def change_annotator(self, fn: F | None = None, /, *, name: str | None = None) -> F | Callable[[F], F]:
        """Reasons for changes in a comparison: ``(ctx, items: list[ChangeUnit]) -> list[str | None]``."""

        def wrap(f: F) -> F:
            n = name or f.__name__.rstrip("_")
            self._declare("change_annotator", AnnotatorSpec(name=n))
            self._handler("annotate", n, f)
            return f

        return wrap(fn) if fn is not None else wrap

    def on(self, event: str, *, timeout: float = 30.0) -> Callable[[F], F]:
        """A lifecycle hook: start, stop, scan_done, scan_closed, settings, lang, basket (the delete
        basket changed: what this plugin proposed may be gone). ``timeout``: seconds it may run (≤
        600); work that takes longer goes into a background task (``ctx.tasks.start``)."""

        def wrap(f: F) -> F:
            n = f.__name__.rstrip("_")
            if event not in HOOKS:
                code = getattr(f, "__code__", None)
                self.issues.append(
                    Issue(
                        "unknown_hook",
                        f"@plugin.on({event!r}): there is no such hook",
                        file=Path(code.co_filename).name if code is not None else "",
                        line=code.co_firstlineno if code is not None else 0,
                        fix=f"one of: {', '.join(HOOKS)}",
                    )
                )
                return f
            self._declare("hook", HookSpec(name=n, event=event, timeout=timeout))  # type: ignore[arg-type]
            self._handler("hook", n, f)
            return f

        return wrap

    def background(
        self,
        name: str,
        title: Text | str,
        *,
        start: Literal["manual", "startup"] = "manual",
        restart: bool = False,
        timeout: float | None = None,
    ) -> Callable[[F], F]:
        """A background task: ``(ctx) -> None`` or ``(ctx, p: MyParams) -> None``, as long as it
        likes (``timeout`` seconds, none: no limit). ``ctx.tasks.start(name, params, key=…)`` starts
        it; ``start="startup"`` starts it with the plugin's process. The host shows it with the
        app's other long work (title, ``ctx.progress``, a cancel button), and with ``restart`` starts
        it again when the plugin's process crashed. A plain ``def`` runs on a thread of its own (not
        one of the call threads); ``ctx.check()`` stops it when cancelled."""

        def wrap(f: F) -> F:
            spec = BackgroundSpec(
                name=name, title=as_text(title), start=start, restart=restart, timeout=timeout
            )
            self._declare("background", spec)
            self._handler("task", name, f)
            return f

        return wrap

    # ------------------------------------------------------------- description

    def all_contributions(self) -> list[tuple[str, Spec]]:
        """TOML and Python declarations together; the same item in both is an error."""
        out = list(self.manifest.declared())
        seen = {(kind, spec_key(kind, spec)) for kind, spec in out}
        for kind, spec in self.declared:
            key = (kind, spec_key(kind, spec))
            if key in seen:
                self.issues.append(
                    Issue(
                        "declared_twice",
                        f"{kind} {key[1]!r} is declared in {MANIFEST} and in the code",
                        fix="keep one of the two",
                    )
                )
                continue
            seen.add(key)
            out.append((kind, spec))
        return out

    def catalog(self) -> dict[str, str]:
        """Chinese -> English of every text it declares, plus ``locales/en.json``."""
        out: dict[str, str] = {}
        path = self.dir / "locales" / "en.json"
        if path.is_file():
            try:
                data = json.loads(path.read_text(encoding="utf-8"))
                out.update({str(k): str(v) for k, v in data.items() if v})
            except (OSError, ValueError) as exc:
                self.issues.append(Issue("locale", f"locales/en.json: {exc}", file="locales/en.json"))

        def walk(value: Any) -> None:
            if isinstance(value, Text):
                if value.en:
                    out.setdefault(value.zh, value.en)
            elif isinstance(value, BaseModel):
                for f in type(value).model_fields:
                    walk(getattr(value, f))
            elif isinstance(value, list | tuple):
                for v in value:
                    walk(v)
            elif isinstance(value, dict):
                for v in value.values():
                    walk(v)

        walk(self.manifest)
        for _kind, spec in self.declared:
            walk(spec)
        return out

    def describe(self) -> Description:
        contributions = [
            {"kind": kind, "spec": spec.model_dump(mode="json", by_alias=True, exclude_none=True)}
            for kind, spec in self.all_contributions()
        ]
        if self.issues:
            raise ManifestError(list(self.issues))
        return Description(
            id=self.id,
            version=self.manifest.version,
            api=PLUGIN_API,
            contributions=contributions,
            methods=list(self.methods.values()),
            events=[
                EventDecl.model_validate({"name": n, "schema": m.model_json_schema(mode="serialization")})
                for n, m in self.events.items()
            ],
            states=[
                StateDecl.model_validate(
                    {
                        "name": n,
                        "schema": m.model_json_schema(mode="serialization"),
                        "default": _default_of(m),
                    }
                )
                for n, m in self.states.items()
            ],
            streams=[
                StreamDecl(name=n, params=h.params.model_json_schema() if h.params is not None else None)
                for n, h in self.streams.items()
            ],
            handlers=sorted(self.handlers),
            catalog=self.catalog(),
        )


def _default_of(model: type[BaseModel]) -> dict[str, Any]:
    try:
        return model().model_dump(mode="json")
    except Exception:  # required fields: no default value
        return {}


def _caller_dir() -> Path:
    for frame in inspect.stack()[2:]:
        mod = frame.frame.f_globals.get("__name__", "")
        if not mod.startswith("spaceknife_sdk"):
            return Path(frame.filename).resolve().parent
    return Path.cwd()


def describe_declarative(folder: Path, manifest: Manifest | None = None) -> Description:
    """The description of a plugin without code (or without running its code)."""
    m = manifest or load_manifest(folder)
    return Plugin(manifest=m, dir=folder).describe()


def known_kinds() -> list[str]:
    return list(KINDS)


def loaded(folder: Path, manifest: Manifest) -> _Loading:
    """Context manager for the runtime: ``Plugin()`` inside the entry module uses this folder."""
    return _Loading(folder, manifest)


class _Loading:
    def __init__(self, folder: Path, manifest: Manifest) -> None:
        self.entry = (folder, manifest)
        self.start = 0

    def __enter__(self) -> _Loading:
        _LOADING.append(self.entry)
        self.start = len(_INSTANCES)
        return self

    def __exit__(self, *exc: object) -> None:
        _LOADING.remove(self.entry)
        self.made = _INSTANCES[self.start :]
        del _INSTANCES[self.start :]

    def instance(self) -> Plugin | None:
        made = getattr(self, "made", None)
        if made is None:
            made = _INSTANCES[self.start :]
        return made[-1] if made else None


__all__ = ["Description", "Handler", "Plugin", "describe_declarative", "loaded"]
