"""Exports of a scanned tree: SpaceSniffer-compatible templates, CSV / JSON / HTML, and output files.

``resolve(name)`` turns what the user names — a built-in template ("Simple file list"), a format
(``csv`` / ``json`` / ``html``), a plugin's template or format (by full id) or a template file — into
a target; ``export_chunks`` renders it piece by piece and ``write_export`` writes the pieces to a
file atomically, as UTF-8.

The built-in templates and formats are the shipped plugin ``builtin.export``
(``plugins/builtin/builtin.export/``), registered in the extension registry like any plugin's:
``BUILTIN_INFO``, ``BUILTIN_TEMPLATES`` and ``FORMATS`` are views of what it registered.
"""

from __future__ import annotations

import os
from collections.abc import Iterable, Iterator
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import numpy as np

from spaceknife_protocol.text import Text

from ..i18n import Texts, _, language
from ..store.nodes import NodeStore
from .formats import (
    ExportFormat,
    export_csv,
    export_html,
    export_json,
    iter_csv,
    iter_html,
    iter_json,
)
from .template import (
    ExportTemplate,
    ExportTree,
    Ordering,
    TemplateIssue,
    check,
    parse_template,
    render,
    render_chunks,
)

__all__ = [
    "BUILTIN_INFO",
    "BUILTIN_TEMPLATES",
    "FORMATS",
    "BuiltinInfo",
    "ExportError",
    "ExportFormat",
    "ExportTemplate",
    "ExportTree",
    "Ordering",
    "TemplateIssue",
    "builtin_targets",
    "builtin_template",
    "check",
    "export_chunks",
    "export_csv",
    "export_html",
    "export_json",
    "iter_csv",
    "iter_html",
    "iter_json",
    "parse_template",
    "plugin_targets",
    "render",
    "render_chunks",
    "resolve",
    "write_export",
]


@dataclass(frozen=True, slots=True)
class BuiltinInfo:
    """A built-in template as the UI lists it (``export.targets``)."""

    name: str  # SpaceSniffer's name (command line, settings); never translated
    file: str  # in the plugin folder: templates/<file>.sktpl
    text: Text  # title in both languages
    desc: Text  # description in both languages
    spacesniffer: bool  # also built into SpaceSniffer

    @property
    def title_id(self) -> str:
        return self.text.zh

    @property
    def description_id(self) -> str:
        return self.desc.zh

    @property
    def title(self) -> str:
        """The title in the current interface language."""
        return self.text.get(language())

    @property
    def description(self) -> str:
        """The description in the current interface language."""
        return self.desc.get(language())


def _shipped(kind: str) -> list[Any]:
    from ..extensions import EXTENSIONS
    from ..plugins import builtins

    builtins.ensure()
    return [c for c in EXTENSIONS.of(kind) if c.shipped and c.impl is not None]


def builtin_targets() -> tuple[dict[str, BuiltinInfo], dict[str, ExportTemplate], dict[str, ExportFormat]]:
    """The shipped templates (by their names) and formats (by key), in the order the UI lists them."""
    info: dict[str, BuiltinInfo] = {}
    templates: dict[str, ExportTemplate] = {}
    for c in _shipped("export_template"):
        tpl: ExportTemplate = c.impl
        spec = c.spec
        info[tpl.name] = BuiltinInfo(
            tpl.name, spec.file, spec.title, spec.description or Text(zh=""), bool(spec.spacesniffer)
        )
        templates[tpl.name] = tpl
    formats = {c.impl.key: c.impl for c in _shipped("export_format")}
    return info, templates, formats


def __getattr__(name: str) -> Any:
    """``BUILTIN_INFO`` (name -> info), ``BUILTIN_TEMPLATES`` (name -> source), ``FORMATS`` (key ->
    format): read from the registry when used (the shipped plugin registers on first use)."""
    if name in ("BUILTIN_INFO", "BUILTIN_TEMPLATES", "FORMATS"):
        info, _templates, formats = builtin_targets()
        if name == "BUILTIN_INFO":
            return info
        if name == "FORMATS":
            return formats
        from ..plugins import builtins

        loaded = builtins.get("builtin.export")
        folder = loaded.folder if loaded is not None else None
        return {
            n: (folder / i.file).read_text(encoding="utf-8") if folder is not None else ""
            for n, i in info.items()
        }
    raise AttributeError(name)


# User-facing text of this module, translated when read (M7.5).
TEXT = Texts(
    {
        "not_found": "找不到导出模板「{name}」：它既不是内置模板（{builtins}），也不是 csv / json / html，"
        "也没有这个模板文件",
        "too_big": "模板文件 {path} 太大（超过 {limit} KB），不像是模板",
        "not_utf8": "模板文件 {path} 不是 UTF-8 编码",
        "unreadable": "无法读取模板文件 {path}：{reason}",
    }
)
MAX_TEMPLATE_BYTES = 1 << 20


class ExportError(Exception):
    """A user-facing export problem (the message is in the interface language)."""


def builtin_template(name: str) -> ExportTemplate | None:
    """A built-in template by its SpaceSniffer name; case does not matter (SpaceSniffer: it does)."""
    _info, templates, _formats = builtin_targets()
    found = templates.get(name)
    if found is None:
        folded = name.casefold()
        found = next((t for k, t in templates.items() if k.casefold() == folded), None)
    return found


def plugin_targets() -> list[tuple[str, ExportTemplate | ExportFormat]]:
    """The enabled plugins' templates, then their formats, by full id (export.targets, the CLI);
    the shipped ones too (``builtin.export.csv``), besides their own names."""
    from ..extensions import EXTENSIONS
    from ..plugins import builtins

    builtins.ensure()
    out: list[tuple[str, ExportTemplate | ExportFormat]] = []
    for kind in ("export_template", "export_format"):
        for c in EXTENSIONS.of(kind):
            if c.impl is not None:
                out.append((c.id, c.impl))
    return out


def resolve(name: str, *, cwd: Path | None = None) -> ExportTemplate | ExportFormat:
    """A built-in template, then a format key, then a plugin's template or format (by full id), then
    a template file (relative to ``cwd``)."""
    found = builtin_template(name)
    if found is not None:
        return found
    fmt = builtin_targets()[2].get(name.strip().lower())
    if fmt is not None:
        return fmt
    for key, target in plugin_targets():
        if key == name.strip():
            return target
    path = Path(name).expanduser()
    if cwd is not None and not path.is_absolute():
        path = cwd / path
    if not path.is_file():
        raise ExportError(TEXT["not_found"].format(name=name, builtins=_("、").join(builtin_targets()[0])))
    try:
        if path.stat().st_size > MAX_TEMPLATE_BYTES:
            raise ExportError(TEXT["too_big"].format(path=path, limit=MAX_TEMPLATE_BYTES >> 10))
        source = path.read_bytes().decode("utf-8")
    except UnicodeDecodeError as exc:
        raise ExportError(TEXT["not_utf8"].format(path=path)) from exc
    except OSError as exc:
        raise ExportError(TEXT["unreadable"].format(path=path, reason=exc.strerror or exc)) from exc
    return parse_template(source, path.stem)


def export_chunks(
    target: ExportTemplate | ExportFormat,
    store: NodeStore,
    root: int,
    *,
    match: np.ndarray | None = None,
    filter_text: str = "",
    options: dict[str, Any] | None = None,
    selection: list[int] | None = None,
) -> Iterator[str | bytes]:
    """The output of ``target`` for the tree below ``root`` (0: every scan root), in pieces.

    ``options``: values of a plugin format's own options (the export dialog's fields); ``selection``:
    the nodes the export was asked for (``match`` already keeps to them)."""
    if isinstance(target, ExportTemplate):
        return render_chunks(store, root, target, match=match)
    if target.plugin is not None and not target.shipped:
        return target.chunks(
            store,
            root,
            match=match,
            filter_text=filter_text,
            options=options or {},
            selection=selection or [],
        )
    return target.chunks(store, root, match=match, filter_text=filter_text)


def subtree_mask(store: NodeStore, nodes: Iterable[int]) -> np.ndarray:
    """True for ``nodes`` and everything below them."""
    n = len(store)
    c = store.cols
    mask = np.zeros(n, dtype=bool)
    front = np.unique(np.asarray([x for x in nodes if 0 < int(x) < n], dtype=np.int64))
    while len(front):
        front = front[~mask[front]]
        if not len(front):
            break
        mask[front] = True
        cnt = c["child_count"][front].astype(np.int64)
        st = c["child_start"][front].astype(np.int64)
        ok = (cnt > 0) & (st >= 1) & (st + cnt <= n)
        cnt, st = cnt[ok], st[ok]
        if not len(cnt):
            break
        total = int(cnt.sum())
        starts = np.repeat(st, cnt)
        offs = np.arange(total, dtype=np.int64) - np.repeat(np.cumsum(cnt) - cnt, cnt)
        front = starts + offs
    return mask


def common_root(store: NodeStore, nodes: list[int]) -> int:
    """The nearest folder that holds all of ``nodes`` (0: they are under different roots)."""
    if not nodes:
        return 0
    if len(nodes) == 1:
        return int(nodes[0])
    chains = []
    for x in nodes:
        chain = [int(x), *store.ancestors(int(x))]
        chains.append(chain[::-1])
    out = 0
    for level in zip(*chains, strict=False):
        if all(v == level[0] for v in level):
            out = level[0]
        else:
            break
    return out


def write_export(path: Path, chunks: Iterable[str | bytes], *, bom: bool = False) -> int:
    """Write ``chunks`` to ``path`` as UTF-8, atomically (a temporary file, then a rename).

    Names that are not valid Unicode (a lone surrogate from a Windows name, a POSIX name that is
    not UTF-8) are written as ``\\udcXX`` escapes — in JSON that is the string's own escape.
    Returns the bytes written. The folder must exist.
    """
    tmp = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    written = 0
    try:
        with open(tmp, "wb") as fh:
            if bom:
                fh.write(b"\xef\xbb\xbf")
                written += 3
            for chunk in chunks:
                data = chunk if isinstance(chunk, bytes) else chunk.encode("utf-8", "backslashreplace")
                fh.write(data)
                written += len(data)
        os.replace(tmp, path)
    except BaseException:
        tmp.unlink(missing_ok=True)
        raise
    return written
