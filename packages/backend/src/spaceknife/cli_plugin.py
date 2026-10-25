"""``spaceknife plugin …``: write, check, try and manage plugins (docs/plugin-dev.md 「命令行」).

    spaceknife plugin new example.hello --with report,command     a working skeleton, built
    spaceknife plugin check [DIR] [--strict] [--json]              every problem, with the fix
    spaceknife plugin build [DIR] [--zip]                          generated files; the package
    spaceknife plugin call DIR METHOD '{"a": 1}' [--scan PATH]     a method's result, events, logs
    spaceknife plugin render DIR report:NAME [--scan PATH]         a report / field / cleanup / export as text
    spaceknife plugin dev [DIR] [--scan PATH]                      a backend with the plugin, reloaded on save
    spaceknife plugin install DIR|ZIP / uninstall ID / list / enable ID / disable ID
    spaceknife plugin schema [--manifest | DIR]                    JSON Schema of plugin.toml or of a plugin
    spaceknife plugin docs [--section ANCHOR]                      the author docs, or one section

``call`` and ``render`` run the plugin in a :class:`~spaceknife_sdk.testing.MockHost` (a server
without a socket, its data in a temporary folder) — in a plugin process, as the app does, unless
``--inprocess``. ``install``, ``list``, ``enable`` … change the real data folder (``SPACEKNIFE_HOME``
relocates it); a running app sees the change after its plugins page is refreshed or a restart.
"""

from __future__ import annotations

import asyncio
import contextlib
import json
import os
import sys
from collections.abc import Iterator
from contextlib import contextmanager
from pathlib import Path
from typing import Annotated, Any

import typer
from rich.console import Console
from rich.markup import escape
from typer.core import TyperCommand

plugin_app = typer.Typer(
    help="Write, check, try and manage plugins (docs/plugin-dev.md).",
    no_args_is_help=True,
    add_completion=False,
)
for _stream in (sys.stdout, sys.stderr):  # line by line, also into a file (plugin dev > log.txt)
    with contextlib.suppress(AttributeError, ValueError):
        _stream.reconfigure(line_buffering=True)  # type: ignore[union-attr]
out = Console(highlight=False, soft_wrap=True)
err = Console(stderr=True, highlight=False, soft_wrap=True)

DirArg = Annotated[Path, typer.Argument(help="The plugin's folder (default: the current folder).")]


def _quiet() -> None:
    from .log import LogFormat, setup_logging

    setup_logging(os.environ.get("SPACEKNIFE_LOG_LEVEL", "WARNING").upper(), LogFormat.PLAIN)


def _folder(target: str | Path) -> Path:
    """A plugin folder from a path, or the folder of an installed / shipped plugin by id."""
    p = Path(target).expanduser()
    if (p / "plugin.toml").is_file():
        return p.resolve()
    from spaceknife_protocol.plugins import valid_plugin_id

    from .plugins import paths as ppaths

    name = str(target)
    if valid_plugin_id(name) or valid_plugin_id(name, builtin=True):
        for base in (ppaths.plugins_dir(), ppaths.builtin_dir()):
            if (base / name / "plugin.toml").is_file():
                return (base / name).resolve()
    err.print(f"[red]no plugin.toml in {p} (and no installed plugin {name!r})[/red]")
    raise typer.Exit(2)


def _fail(message: str, code: int = 1) -> typer.Exit:
    err.print(f"[red]{message}[/red]")
    return typer.Exit(code)


# --------------------------------------------------------------------------- writing


class _NewCommand(TyperCommand):
    """``plugin new``: the help of ``--with`` lists the scaffold's parts from the one list
    (``spaceknife_sdk.scaffold.PARTS``, also docs/plugin-quick.md's), read only when the help is
    shown or the command runs: other commands do not import the SDK."""

    def get_params(self, ctx: Any) -> list[Any]:
        params: list[Any] = super().get_params(ctx)
        for p in params:
            text = getattr(p, "help", None)
            if isinstance(text, str) and "{parts}" in text:
                from spaceknife_sdk.scaffold import DEFAULT, PARTS

                p.help = text.format(parts=" ".join(PARTS), default=",".join(DEFAULT))
        return params


@plugin_app.command("new", cls=_NewCommand)
def new(
    plugin_id: Annotated[str, typer.Argument(metavar="ID", help="The plugin id, e.g. example.hello.")],
    with_: Annotated[
        list[str] | None,
        typer.Option(
            "--with",
            help="Parts, comma-separated: {parts} (default: {default}).",
        ),
    ] = None,
    dir: Annotated[Path | None, typer.Option(help="Where to create it (default: ./ID).")] = None,
    title: Annotated[
        str | None,
        typer.Option(
            help="The plugin's name (default: from the id); a Chinese one keeps the id's as English."
        ),
    ] = None,
) -> None:
    """Create a plugin skeleton that passes `plugin check --strict` and its own tests as it is."""
    from spaceknife_sdk import scaffold

    from .plugins.build import build as build_plugin

    _quiet()
    folder = (dir or Path.cwd() / plugin_id).resolve()
    try:
        files = scaffold.write(folder, plugin_id, with_ or [], title=title)
    except (ValueError, FileExistsError) as exc:
        raise _fail(str(exc), 2) from None
    result = build_plugin(folder)
    for i in result.issues:
        err.print(f"[yellow]{i.text()}[/yellow]")
    out.print(f"created {folder}")
    for f in sorted({*(str(p.relative_to(folder)) for p in files), *result.written}):
        out.print(f"  {f}")
    out.print("next:")
    for line in scaffold.next_steps(folder, with_ or []):
        out.print(f"  {line}")


@plugin_app.command("check")
def check(
    folder: DirArg = Path("."),
    as_json: Annotated[
        bool, typer.Option("--json", help="One JSON document (for tools and agents).")
    ] = False,
    strict: Annotated[bool, typer.Option(help="Warnings count as problems too.")] = False,
    contracts: Annotated[
        bool, typer.Option(help="Run the code on synthetic trees (--no-contracts: files only).")
    ] = True,
    inprocess: Annotated[bool, typer.Option(help="Run the contracts in this process.")] = False,
) -> None:
    """Every check (docs/plugin-dev.md 「检查」); exit status 1 when something is wrong."""
    from .plugins.check import main_check

    _quiet()
    code = main_check(
        _folder(folder), strict=strict, as_json=as_json, contracts=contracts, process=not inprocess
    )
    raise typer.Exit(code)


@plugin_app.command("build")
def build(
    folder: DirArg = Path("."),
    zip: Annotated[bool, typer.Option("--zip", help="Also pack dist/<id>-<version>.zip.")] = False,
) -> None:
    """Write generated/schema.json, web/sk-plugin.d.ts, generated/pinyin.json, web/_sk/; list texts
    without English."""
    from .plugins.build import build as build_plugin

    _quiet()
    result = build_plugin(_folder(folder), zip=zip)
    for i in result.issues:
        err.print(f"[red]{i.text()}[/red]")
    if not result.ok:
        raise typer.Exit(1)
    for rel in result.written:
        out.print(f"wrote {rel}")
    if not result.written:
        out.print("everything is up to date")
    if result.missing_en:
        out.print(f"{len(result.missing_en)} text(s) without English:")
        for t in result.missing_en:
            out.print(f"  {t}")
    if result.zip is not None:
        out.print(f"packed {result.zip}")


# --------------------------------------------------------------------------- trying


@contextmanager
def _plugin_logs(plugin: str) -> Iterator[list[str]]:
    from loguru import logger

    lines: list[str] = []

    def sink(message: Any) -> None:
        rec = message.record
        if rec["extra"].get("source") == f"plugin:{plugin}" or plugin in str(rec["message"])[:200]:
            lines.append(f"{rec['level'].name}: {rec['message']}")

    handle = logger.add(sink, level="DEBUG", format="{message}")
    try:
        yield lines
    finally:
        logger.remove(handle)


def _settings(items: list[str] | None) -> dict[str, Any]:
    """``--setting name=value`` (repeatable): JSON values (``true``, ``3``, ``"x"``, ``[…]``,
    ``{…}``), ``name=@file.json`` (the value from a file), or plain text."""
    from .cliargs import json_value

    out: dict[str, Any] = {}
    for item in items or []:
        name, sep, raw = item.partition("=")
        if not sep or not name.strip():
            raise _fail(f"--setting {item!r}: write name=value", 2)
        try:
            out[name.strip()] = json_value(
                name.strip(), raw, warn=lambda m: err.print(f"[yellow]--setting {m}[/yellow]")
            )
        except ValueError as exc:
            raise _fail(f"--setting {exc}", 2) from None
    return out


def _json_arg(text: str, what: str) -> Any:
    """A JSON argument, or ``@file.json`` for one in a file (no shell quoting to get wrong)."""
    if text.startswith("@") and len(text) > 1:
        path = Path(text[1:]).expanduser()
        try:
            text = path.read_text(encoding="utf-8-sig")
        except OSError as exc:
            raise _fail(f"{what}: cannot read {path}: {exc.strerror or exc}", 2) from None
    try:
        return json.loads(text)
    except ValueError as exc:
        raise _fail(f"{what} are not JSON: {exc} (a file is safest: @file.json)", 2) from None


def _history(files: list[Path]) -> None:
    """``--history FILE``: snapshots copied into the temporary backend's history, where
    ``ctx.snapshots.list()`` finds them (as the app's saved snapshots)."""
    import shutil

    from .store import snapshot as snap

    for f in files:
        try:
            header = snap.read_header(f)
        except (OSError, snap.SnapshotError) as exc:
            raise _fail(f"--history {f}: {exc}", 2) from None
        target = snap.auto_path(list(header.roots), header.created_at)  # a free name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(f, target)


async def _mock(
    folder: Path,
    scan: Path | None,
    snapshot: Path | None,
    inprocess: bool,
    lang: str | None,
    settings: dict[str, Any] | None = None,
    history: list[Path] | None = None,
) -> Any:
    from spaceknife_sdk.testing import MockHost

    from .plugins.check import _trees

    if lang:
        os.environ["SPACEKNIFE_LANGUAGE"] = "en" if lang.startswith("en") else "zh-CN"
    host = MockHost.load(folder, process=not inprocess)
    try:
        await host.start()
    except RuntimeError as exc:  # the host refused it (it does not import, a contribution is wrong)
        await host.close()
        err.print(f"[red]{exc}[/red]")
        err.print("[dim]spaceknife plugin check shows where[/dim]")
        raise typer.Exit(1) from None
    if history:
        _history(history)
    for name, value in (settings or {}).items():
        try:
            await host.set_setting(name, value)
        except Exception as exc:
            await host.close()
            raise _fail(f"--setting {name}: {exc}", 2) from None
    if scan is not None:
        folder_ = await asyncio.to_thread(lambda: scan.expanduser().resolve())
        if not await asyncio.to_thread(folder_.is_dir):  # say so (a traceback, or a long wait, before)
            await host.close()
            raise _fail(f"--scan {scan}: no such folder", 2)
        await host.scan_dir(folder_)
    elif snapshot is not None:
        from .store import snapshot as snap

        store, _header = snap.load(snapshot)
        host.scan(store)
    elif (folder / "fixtures" / "tree").is_dir():
        await host.scan_dir(folder / "fixtures" / "tree")
    else:
        host.scan(_trees()[0])
        err.print("[dim](a synthetic tree; --scan PATH scans a real folder)[/dim]")
    return host


def _print_side_effects(host: Any, logs: list[str]) -> None:
    for name, payloads in host.events.items():
        for p in payloads:
            err.print(f"event {name}: {json.dumps(p, ensure_ascii=False)}")
    for t in host.toasts:
        err.print(f"toast: {t}")
    for line in logs:
        err.print(f"log {line}")


@plugin_app.command("call")
def call(
    target: Annotated[str, typer.Argument(metavar="DIR|ID", help="The plugin's folder or id.")],
    method: Annotated[str, typer.Argument(help="A method's local name.")],
    params: Annotated[str, typer.Argument(help="Its params as JSON, or @file.json.")] = "{}",
    scan: Annotated[Path | None, typer.Option(help="Scan this folder first.")] = None,
    snapshot: Annotated[Path | None, typer.Option(help="Use this snapshot as the scan.")] = None,
    history: Annotated[
        list[Path] | None,
        typer.Option("--history", help="A snapshot for ctx.snapshots.list() (repeatable)."),
    ] = None,
    inprocess: Annotated[bool, typer.Option(help="Run the plugin in this process.")] = False,
    lang: Annotated[str | None, typer.Option(help="zh or en.")] = None,
    setting: Annotated[
        list[str] | None,
        typer.Option(
            "--setting", help="name=value (JSON, or @file.json): one of the plugin's settings (repeatable)."
        ),
    ] = None,
) -> None:
    """Call a method: prints its JSON result; events, toasts and the plugin's log to stderr."""
    folder = _folder(target)
    _quiet()
    args = _json_arg(params, "params")
    if not isinstance(args, dict):
        raise _fail("params must be a JSON object", 2)

    async def run() -> int:
        from .server.rpc import RpcError

        host = await _mock(folder, scan, snapshot, inprocess, lang, _settings(setting), history)
        try:
            with _plugin_logs(host.plugin_id) as logs:
                try:
                    value, frames = await host.call_frames(method, **args)
                except RpcError as exc:
                    _print_side_effects(host, logs)
                    err.print(f"[red]error {exc.code}: {exc.message}[/red]")
                    if exc.data:
                        err.print(json.dumps(exc.data, ensure_ascii=False))
                    if exc.code == -32601:
                        err.print(f"[dim]{_not_a_method_hint(host, method, str(folder))}[/dim]")
                    return 1
                out.print(json.dumps(value, ensure_ascii=False, indent=1, default=str))
                for f in frames:
                    shown = {k: (v[:20].tolist(), f"{len(v)} values") for k, v in f.items()}
                    out.print(f"columns: {shown}")
                _print_side_effects(host, logs)
                return 0
        finally:
            await host.close()

    raise typer.Exit(asyncio.run(run()))


def _not_a_method_hint(host: Any, name: str, folder: str) -> str:
    """What to run instead when ``name`` is a contribution, not a method (plugin call)."""
    rec = host.server.plugins.records.get(host.plugin_id)
    desc = rec.description if rec is not None else None
    kinds = {str(c.get("kind")): c for c in (desc.contributions if desc else []) if c.get("name") == name}
    look = {
        "report": "report",
        "export_format": "export",
        "node_set": "set",
        "visualization": "visual",
        "color_mode": "color",
        "cleanup": "cleanup",
        "scan_source": "source",
    }
    for kind, word in look.items():
        if kind in kinds:
            return f"{name!r} is a {kind}: spaceknife plugin render {folder} {word}:{name}"
    if "command" in kinds:
        return f"{name!r} is a command: its method is the decorated function's name"
    methods = sorted(m.name for m in (desc.methods if desc else []))
    return "methods: " + (", ".join(methods) or "(none)")


def _calls(items: list[str] | None) -> list[tuple[str, dict[str, Any]]]:
    """``--call NAME`` or ``--call NAME={json params}`` (repeatable)."""
    out: list[tuple[str, dict[str, Any]]] = []
    for item in items or []:
        name, sep, raw = item.partition("=")
        try:
            params = json.loads(raw) if sep else {}
        except ValueError as exc:
            raise _fail(f"--call {item!r}: the params are not JSON ({exc})", 2) from None
        if not isinstance(params, dict) or not name.strip():
            raise _fail(f"--call {item!r}: write NAME or NAME={{json object}}", 2)
        out.append((name.strip(), params))
    return out


@plugin_app.command("render")
def render(
    target: Annotated[str, typer.Argument(metavar="DIR|ID", help="The plugin's folder or id.")],
    what: Annotated[
        str,
        typer.Argument(
            help="report:NAME, field[:NAME], insight, cleanup:NAME, export:NAME, set:NAME, "
            "visual:NAME, color:NAME, source:NAME (with --uri)."
        ),
    ],
    scan: Annotated[Path | None, typer.Option(help="Scan this folder first.")] = None,
    snapshot: Annotated[Path | None, typer.Option(help="Use this snapshot as the scan.")] = None,
    history: Annotated[
        list[Path] | None,
        typer.Option("--history", help="A snapshot for ctx.snapshots.list() (repeatable)."),
    ] = None,
    node: Annotated[str | None, typer.Option(help="The node, relative to the scan's root (a/b.txt).")] = None,
    format: Annotated[str, typer.Option(help="text, json or md.")] = "text",
    inprocess: Annotated[bool, typer.Option(help="Run the plugin in this process.")] = False,
    lang: Annotated[str | None, typer.Option(help="zh or en.")] = None,
    setting: Annotated[
        list[str] | None,
        typer.Option(
            "--setting", help="name=value (JSON, or @file.json): one of the plugin's settings (repeatable)."
        ),
    ] = None,
    uri: Annotated[str | None, typer.Option(help="source:NAME — what to list (a file, a URL …).")] = None,
    output: Annotated[
        Path | None, typer.Option("--output", "-o", help="export:NAME — write the file (with its BOM).")
    ] = None,
    first: Annotated[
        list[str] | None,
        typer.Option(
            "--call",
            help="Call this method first, in the same backend (NAME or NAME={json}); repeatable: "
            "a field or set that needs a result computed before shows it.",
        ),
    ] = None,
    option: Annotated[
        list[str] | None,
        typer.Option(
            "--option", help="export:NAME — name=value of one of the format's options (repeatable)."
        ),
    ] = None,
    filter: Annotated[
        str | None, typer.Option(help="export:NAME — only what this filter expression matches.")
    ] = None,
) -> None:
    """Show what a contribution produces, without the app: a report as text, fields, the cleanup's
    candidates, an export's output, a node set, a visualization's or colour mode's frame (json:
    each row's node, size and palette index), what a scan source lists. --node: the selected
    node (an export: only it)."""
    folder = _folder(target)
    _quiet()
    calls = _calls(first)
    options = _settings(option)
    if uri and "://" not in uri and not os.path.isabs(uri):
        uri = str(Path(uri).resolve())  # the plugin's process has another working folder
    kind, _, name = what.partition(":")
    kinds = ("report", "field", "insight", "cleanup", "export", "set", "visual", "color", "source")
    if kind not in kinds:
        raise _fail(f"unknown {kind!r}: {', '.join(kinds)}", 2)
    if kind in ("report", "cleanup", "export", "set", "visual", "color", "source") and not name:
        raise _fail(f"{kind}:NAME — which one?", 2)
    if kind == "source" and not uri:
        raise _fail("source:NAME needs --uri (what to list)", 2)

    async def run() -> int:
        from .server.rpc import RpcError

        host = await _mock(folder, scan, snapshot, inprocess, lang, _settings(setting), history)
        try:
            with _plugin_logs(host.plugin_id) as logs:
                try:
                    node_id = host.find(node) if node else None
                    for method, params in calls:
                        await host.call(method, **params)
                    export_kw: dict[str, Any] = {"options": options, "filter": filter}
                    if node_id is not None:
                        export_kw["selection"] = [node_id]
                    if kind == "source":
                        text = await _render_source(host, name, str(uri))
                    elif kind == "export" and output is not None:
                        text = await _write_export(host, name, output, export_kw)
                    elif kind == "export":
                        data = await host.export(name, **export_kw)
                        text = data if isinstance(data, str) else data.decode("utf-8", "replace")
                    else:
                        text = await _render(host, kind, name, node_id, format)
                except (RpcError, KeyError, AssertionError, RuntimeError) as exc:
                    _print_side_effects(host, logs)
                    message = getattr(exc, "message", None) or str(exc) or type(exc).__name__
                    if isinstance(exc, KeyError):
                        message = f"no {node!r} in the scan"
                    err.print(f"[red]{message}[/red]")
                    return 1
                sys.stdout.write(text if text.endswith("\n") else text + "\n")
                _print_side_effects(host, logs)
                return 0
        finally:
            await host.close()

    raise typer.Exit(asyncio.run(run()))


async def _render_source(host: Any, name: str, uri: str) -> str:
    """What a scan source lists for ``uri``: its tree, the largest first (as the app would show it)."""
    from . import fmt as sizes

    await host.source(name, uri)
    store = host.store
    lines = []
    c = store.cols
    for i in range(1, min(len(store), 401)):
        kind = "dir " if int(c["kind"][i]) == 1 else "file"
        alloc = int(c["tot_alloc"][i]) if kind == "dir " else int(c["alloc"][i])
        lines.append(f"{kind} {sizes.size(alloc):>10}  {store.path(i)}")
    more = len(store) - 1 - len(lines)
    lines.append(f"{len(store) - 1} item(s)" + (f" ({more} not shown)" if more > 0 else ""))
    return "\n".join(lines)


async def _write_export(host: Any, name: str, path: Path, kw: dict[str, Any] | None = None) -> str:
    """An export format's output written as the app writes it (its BOM, UTF-8)."""
    from .export import ExportFormat, resolve, write_export

    data = await host.export(name, **(kw or {}))
    target = resolve(f"{host.plugin_id}.{name}")
    bom = isinstance(target, ExportFormat) and target.bom
    written = write_export(path, [data], bom=bom)
    return f"wrote {path} ({written} bytes{', with a BOM' if bom else ''})"


def _field_text(value: Any, unit: str) -> str:
    """A field's value as the details panel shows it (sizes, counts, dates)."""
    from . import fmt as f

    if value is None:
        return "—"
    try:
        if unit == "size" and isinstance(value, int | float):
            return f.size(int(value))
        if unit == "count" and isinstance(value, int | float):
            return f.count(int(value))
        if unit == "pct" and isinstance(value, int | float):
            return f"{float(value) * 100:.1f}%"
        if unit in ("date", "ago") and isinstance(value, int | float) and value:
            import datetime as dt

            return dt.datetime.fromtimestamp(float(value), dt.UTC).strftime("%Y-%m-%d %H:%M")
    except (TypeError, ValueError, OverflowError):
        pass
    return str(value)


async def _render(host: Any, kind: str, name: str, node: int | None, fmt: str) -> str:
    def dump(value: Any) -> str:
        return json.dumps(value, ensure_ascii=False, indent=1, default=str)

    if kind == "report":
        rep = await host.report(name, node=node)
        if fmt == "json":
            return dump(rep.model_dump(mode="json"))
        text = str(host.text(rep))
        return text if fmt != "md" else text.replace("\n", "  \n")
    if kind in ("field", "insight"):
        if node is None:
            node = next(iter(host.store.roots()))
        items = await (host.fields(node=node) if kind == "field" else host.insights(node=node))
        rows = [i.model_dump(mode="json") for i in items]
        if name:
            rows = [r for r in rows if str(r.get("id", "")).endswith("." + name)]
        if fmt == "json":
            return dump(rows)
        if kind == "field":
            text = "\n".join(
                f"{r.get('title')}: {_field_text(r.get('value'), str(r.get('unit', '')))}" for r in rows
            )
        else:
            text = "\n".join(f"{r.get('title')}: {r.get('reason', '')}" for r in rows)
        return text or "(nothing for this node)"
    if kind == "cleanup":
        items = await host.cleanup(name)
        if fmt == "json":
            import dataclasses

            return dump([dataclasses.asdict(i) for i in items])
        lines = [f"{i.path}\t{i.alloc}\t{i.reason}" + ("" if i.checked else "\t(unchecked)") for i in items]
        return "\n".join([*lines, f"{len(items)} candidate(s), {sum(i.alloc for i in items)} bytes"])
    if kind == "export":
        data = await host.export(name)
        return data if isinstance(data, str) else data.decode("utf-8", "replace")
    if kind == "set":
        mask = await host.node_set(name)
        if mask is None:
            return "(empty)"
        import numpy as np

        ids = np.flatnonzero(mask) if mask.dtype == np.bool_ else mask
        store = host.store
        return "\n".join(store.path(int(i)) for i in ids[:1000]) + (
            f"\n… {len(ids) - 1000} more" if len(ids) > 1000 else ""
        )
    frame = await (host.visual(name) if kind == "visual" else host.color(name))
    draw = frame.draw
    if fmt == "json":  # each drawn row: its node, name, size (bytes, count or weight), palette index
        rows = [
            {
                "node": int(draw.node[i]),
                "name": draw.names[i] if i < len(draw.names) else "",
                "size": int(draw.size[i]),
                **({"pal": int(frame.pal[i])} if frame.pal is not None else {}),
                # a plugin's own layout: where each block went (physical px)
                **(
                    {k: int(draw.geometry[k][i]) for k in ("x", "y", "w", "h", "r", "poly")}
                    if draw.layout == "shapes.v1"
                    else {}
                ),
            }
            for i in range(len(draw))
        ]
        return dump({"unit": frame.size_unit, "root_size": int(frame.root.size), "rows": rows})
    lines = [f"{len(draw)} rows drawn (the row contract holds)"]
    meta = draw.meta
    if draw.layout == "shapes.v1" and meta is not None:
        import numpy as np

        from spaceknife_protocol import view as pv

        assert isinstance(meta, pv.ShapesGeometry)
        g = draw.geometry
        poly = g["poly"] > 0
        circle = ~poly & (2 * g["r"] >= np.minimum(g["w"], g["h"])) & (g["w"] == g["h"])
        lines.append(
            f"shapes: {int(circle.sum())} circles, {int((~poly & ~circle).sum())} rectangles, "
            f"{int(poly.sum())} polygons; {int((draw.label > 0).sum())} labelled; "
            f"canvas {draw.width} × {draw.height} px; line {meta.stroke:g} px; "
            + (
                "no root shape"
                if meta.root is None
                else f"root {meta.root.w} × {meta.root.h} at {meta.root.x}, {meta.root.y}"
            )
        )
        if meta.message:
            lines.append(f"message: {meta.message}")
    if kind == "color" and frame.pal is not None:
        import numpy as np

        counts = np.bincount(frame.pal.astype(np.int64))
        lines.append("palette index: rows " + ", ".join(f"{i}: {int(c)}" for i, c in enumerate(counts) if c))
    return "\n".join(lines)


# --------------------------------------------------------------------------- developing


@plugin_app.command("dev")
def dev(
    folder: DirArg = Path("."),
    scan: Annotated[Path | None, typer.Option(help="Start a scan of this folder.")] = None,
    port: Annotated[int, typer.Option(help="The backend's port (0: a free one).")] = 0,
    ui: Annotated[
        Path | None,
        typer.Option(help="A built renderer (apps/electron/out/renderer) to serve in browser mode."),
    ] = None,
    home: Annotated[
        Path | None,
        typer.Option(help="Data folder (default: a temporary one; your settings stay untouched)."),
    ] = None,
    allow_origin: Annotated[list[str] | None, typer.Option(help="Extra allowed WebSocket origin.")] = None,
) -> None:
    """Run a backend with the plugin linked; on every save it is described and started again and the
    frontends are told (extensions.changed). With a built renderer (--ui, SPACEKNIFE_UI, the
    repository's), also serves the app in a browser; says so when that build is older than its sources."""
    from .devui import run_dev

    target = _folder(folder)
    raise typer.Exit(run_dev(target, scan=scan, port=port, ui=ui, home=home, origins=allow_origin or []))


@plugin_app.command("shot")
def shot(
    folder: DirArg = Path("."),
    scan: Annotated[
        Path | None, typer.Option(help="Scan this folder (default: a small synthetic one).")
    ] = None,
    open_: Annotated[
        str | None,
        typer.Option(
            "--open",
            help="report:<name> | panel:<name> | export:<format> (the export dialog on it) | settings.",
        ),
    ] = None,
    do: Annotated[
        list[str] | None,
        typer.Option(
            "--do",
            help="Run a command first (repeatable): <name> (the plugin's), visual:<name>, color:<name>, "
            "export (the export dialog), or any command id.",
        ),
    ] = None,
    select: Annotated[
        str | None, typer.Option(help="Select this node, relative to the root (a/b.txt).")
    ] = None,
    theme: Annotated[str | None, typer.Option(help="light or dark.")] = None,
    lang: Annotated[str | None, typer.Option(help="zh or en.")] = None,
    size: Annotated[str, typer.Option(help="The window: WIDTHxHEIGHT.")] = "1400x900",
    out: Annotated[
        Path | None, typer.Option("--out", "-o", help="The PNG (default: ./shot-<id>.png).")
    ] = None,
    wait: Annotated[float, typer.Option(help="Seconds to let it settle before the picture.")] = 1.0,
    text: Annotated[bool, typer.Option("--text", help="Also print the page's visible text.")] = False,
    eval_: Annotated[
        str | None,
        typer.Option("--eval", help="Print what this JavaScript expression gives, just before the picture."),
    ] = None,
    step: Annotated[
        list[str] | None,
        typer.Option(
            "--step",
            help="An action before the picture (repeatable, in order): click: rclick: dblclick: hover: "
            "<css or text=…>, node=<a/b> (where the view draws it) or @x,y; set:<element>=<value>, "
            "type:<text>, key:<Key>, select:<a/b>, do:<command>, open:<target>, wait:<s>, eval:<js>; "
            "panel:<step> inside the plugin's panel.",
        ),
    ] = None,
    setting: Annotated[
        list[str] | None,
        typer.Option(
            "--setting", help="name=value (JSON, or @file.json): one of the plugin's settings (repeatable)."
        ),
    ] = None,
    history: Annotated[
        list[Path] | None,
        typer.Option(
            "--history", help="A snapshot for ctx.snapshots.list() and the comparisons (repeatable)."
        ),
    ] = None,
    source: Annotated[
        str | None, typer.Option("--source", help="<name>:<uri>: scan with the plugin's scan source instead.")
    ] = None,
    ui: Annotated[
        Path | None,
        typer.Option(help="A built interface (out/ or out/renderer of pnpm build; SPACEKNIFE_UI)."),
    ] = None,
) -> None:
    """A picture of the real interface with the plugin enabled, in a headless browser (nothing opens
    on the desktop): its own backend, temporary data folder and port. Prints the PNG's path and
    what the page and the plugin logged as errors or warnings."""
    from .devshot import ShotOptions, StepError, parse_step, run_shot

    target = _folder(folder)
    from spaceknife_sdk.manifest import load_manifest

    pid = load_manifest(target).id
    try:
        w, h = (int(x) for x in size.lower().split("x"))
    except ValueError:
        raise _fail(f"--size {size!r}: write WIDTHxHEIGHT", 2) from None

    def command_word(d: str) -> str:
        kind, _, name = d.partition(":")
        if kind == "visual" and name:
            return f"view.visual.{name if '.' in name else f'{pid}.{name}'}"
        if kind == "color" and name:
            return f"view.color.{name if '.' in name else f'{pid}.{name}'}"
        if d == "export":
            return "report.export"
        return d if "." in d else f"{pid}.{d}"

    def open_word(o: str) -> str:
        if ":" in o:
            what, _, name = o.partition(":")
            if what in ("report", "panel", "export") and "." not in name:
                return f"{what}:{pid}.{name}"
        return f"settings:{pid}" if o == "settings" else o

    words = [command_word(d) for d in do or []]
    target_open = open_word(open_) if open_ else None
    steps: list[str] = []
    for s in step or []:
        try:
            _in_panel, kind, arg = parse_step(s)
        except StepError as exc:
            raise _fail(f"--step {exc}", 2) from None
        if kind == "do":
            s = f"do:{command_word(arg)}"
        elif kind == "open":
            s = f"open:{open_word(arg)}"
        steps.append(s)
    src = None
    if source:
        name, sep, uri = source.partition(":")
        if not sep or not name:
            raise _fail(f"--source {source!r}: write <name>:<uri>", 2)
        src = f"{name if '.' in name else f'{pid}.{name}'}:{uri}"
    opts = ShotOptions(
        scan=scan,
        ui=ui,
        width=w,
        height=h,
        out=out,
        theme=theme,
        lang=("en" if lang.startswith("en") else "zh-CN") if lang else None,
        select=select,
        run=words,
        open=target_open,
        wait=wait,
        text=text,
        eval=eval_,
        steps=steps,
        settings=_settings(setting),
        history=list(history or []),
        source=src,
    )
    raise typer.Exit(run_shot(target, opts, say=lambda line: out_print(line)))


def out_print(line: str) -> None:
    sys.stdout.write(line + "\n")
    sys.stdout.flush()


# --------------------------------------------------------------------------- managing


@contextmanager
def _host() -> Iterator[Any]:
    """The real data folder's plugin host, without a server listening."""
    from .server.app import Server, ServerConfig

    _quiet()
    loop = asyncio.new_event_loop()
    server = Server(ServerConfig(token="cli-plugin-token-0123456789", hello_timeout=1.0, ping_interval=None))
    try:
        loop.run_until_complete(server.plugins.start())
        yield loop, server.plugins
    finally:
        loop.run_until_complete(server.plugins.close())
        loop.run_until_complete(server.scans.close())
        loop.close()


def _rpc(fn: Any) -> Any:
    from .server.rpc import RpcError

    try:
        return fn()
    except RpcError as exc:
        raise _fail(exc.message) from None


@plugin_app.command("install")
def install(
    source: Annotated[Path, typer.Argument(metavar="DIR|ZIP", help="A plugin folder or a .zip.")],
    replace: Annotated[bool, typer.Option(help="Replace an installed plugin with the same id.")] = False,
) -> None:
    """Install into the data folder's plugins/ (it stays off until enabled)."""
    with _host() as (loop, host):
        res = _rpc(lambda: loop.run_until_complete(host.install(str(source.expanduser().resolve()), replace)))
        if res.conflict:
            raise _fail(f"{res.id} {res.existing_version} is installed: --replace to install {res.version}")
        out.print(f"installed {res.id} {res.version}; enable it: spaceknife plugin enable {res.id}")


@plugin_app.command("uninstall")
def uninstall(
    plugin: Annotated[str, typer.Argument(metavar="ID")],
    data: Annotated[bool, typer.Option("--data", help="Remove its data folder too.")] = False,
) -> None:
    """Move an installed plugin to the recycle bin."""
    with _host() as (loop, host):
        _rpc(lambda: loop.run_until_complete(host.uninstall(plugin, data)))
        out.print(f"uninstalled {plugin}")


@plugin_app.command("list")
def list_(as_json: Annotated[bool, typer.Option("--json")] = False) -> None:
    """The plugins: shipped, installed and linked, with their state."""
    with _host() as (_loop, host):
        res = host.list_plugins()
    if as_json:
        out.print_json(json.dumps(res.model_dump(mode="json"), ensure_ascii=False))
        return
    out.print(f"folder: {res.folder}" + ("  (developer mode)" if res.dev_mode else ""))
    for p in res.plugins:
        tag = "built-in" if p.builtin else "linked" if p.dev else ""
        out.print(f"  {p.id:<28} {p.version:<10} {p.state:<14} {tag} {p.name}")


@plugin_app.command("enable")
def enable(
    plugin: Annotated[str, typer.Argument(metavar="ID")],
    yes: Annotated[bool, typer.Option("--yes", help="Do not ask (scripts).")] = False,
) -> None:
    """Enable a plugin: shows what it asks for and asks you to confirm."""
    with _host() as (loop, host):
        desc = _rpc(lambda: loop.run_until_complete(host.describe(plugin)))
        info = desc.plugin
        out.print(f"{info.id} {info.version}: {info.name}")
        out.print("permissions:" + ("" if info.permissions else " none"))
        added = set(desc.added)
        for perm in info.permissions:
            mark = " (new)" if perm.id in added else ""
            warn = " [!]" if perm.warn else ""
            out.print(f"  {perm.id}{mark}{warn}: {perm.text}")
        if desc.groups:
            out.print("adds:")
            for line in desc.summary:  # the same groups as the enable dialog
                out.print(f"  {escape(line)}")
        if desc.keys:
            out.print("keys:")
            for k in desc.keys:
                taken = f"  [yellow](already {escape(k.conflict)})[/yellow]" if k.conflict else ""
                out.print(f"  {escape(k.key)} → {escape(k.title)}{taken}")
        if desc.unsigned:
            out.print("not signed; the audit hook is not an operating-system sandbox")
        if not yes and not typer.confirm("enable it?"):
            raise typer.Exit(1)
        rec = host.records[info.id]
        res = _rpc(lambda: loop.run_until_complete(host.enable(info.id, rec.hash, rec.requested)))
        out.print(f"{res.id}: {res.state}")


@plugin_app.command("disable")
def disable(plugin: Annotated[str, typer.Argument(metavar="ID")]) -> None:
    """Turn a plugin off (its settings and data stay)."""
    with _host() as (loop, host):
        res = _rpc(lambda: loop.run_until_complete(host.disable(plugin)))
        out.print(f"{res.id}: {res.state}")


# --------------------------------------------------------------------------- reference


@plugin_app.command("schema")
def schema(
    folder: Annotated[Path | None, typer.Argument(help="A plugin's folder: its own protocol schema.")] = None,
    manifest: Annotated[bool, typer.Option("--manifest", help="The JSON Schema of plugin.toml.")] = False,
) -> None:
    """Print the JSON Schema of plugin.toml (--manifest), or of a plugin's methods, events and states."""
    if manifest or folder is None:
        from spaceknife_sdk.manifest import Manifest

        sys.stdout.write(json.dumps(Manifest.model_json_schema(), indent=1, ensure_ascii=False) + "\n")
        return
    from spaceknife_sdk.schema import dumps, export_plugin_schema

    from .plugins.build import describe

    _quiet()
    desc, issues = describe(_folder(folder))
    if desc is None:
        raise _fail("; ".join(i.text() for i in issues))
    sys.stdout.write(dumps(export_plugin_schema(desc)))


@plugin_app.command("docs")
def docs(
    section: Annotated[
        str | None, typer.Option(help="One section by its anchor (kind-report, ctx-tree, api-cache …).")
    ] = None,
    search: Annotated[
        str | None, typer.Option(help="The lines that mention a word, with their sections' anchors.")
    ] = None,
    list_sections: Annotated[bool, typer.Option("--list", help="The anchors of all three docs.")] = False,
    guide: Annotated[bool, typer.Option("--guide", help="The whole guide (docs/plugin-dev.md).")] = False,
    api: Annotated[bool, typer.Option("--api", help="The whole API reference (docs/plugin-api.md).")] = False,
) -> None:
    """Print the quick reference (docs/plugin-quick.md: read it first); --section ANCHOR: one section of any
    of the docs; --search the lines with a word; --guide / --api a whole doc."""
    from spaceknife_sdk import docs as sdk_docs

    name = sdk_docs.API if api else sdk_docs.NAME if guide else sdk_docs.QUICK
    text = sdk_docs.text(name)
    if text is None:
        raise _fail(f"the author docs are not part of this installation (docs/{name} in the repository)")
    if list_sections:
        for doc_name in sdk_docs.ALL:
            doc = sdk_docs.text(doc_name)
            for anchor, title in sdk_docs.anchors(doc or ""):
                out.print(f"{anchor:<28} {title}")
        return
    if section:
        part = sdk_docs.find(section)
        if part is None:
            raise _fail(f"no section {section!r}: spaceknife plugin docs --list")
        sys.stdout.write(part)
        return
    if search:
        hits = sdk_docs.search(search)
        if not hits:
            raise _fail(f"no line mentions {search!r}")
        for doc_name, anchor, line_no, line in hits:
            sys.stdout.write(f"{anchor or doc_name:<24} {doc_name}:{line_no}: {line[:160]}\n")
        sys.stdout.write("read one: spaceknife plugin docs --section <anchor>\n")
        return
    sys.stdout.write(text)


__all__ = ["plugin_app"]
