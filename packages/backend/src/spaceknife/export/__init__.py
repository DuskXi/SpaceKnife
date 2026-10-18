"""Exports of a scanned tree: SpaceSniffer-compatible templates, CSV / JSON / HTML, and output files.

``resolve(name)`` turns what the user names — a built-in template ("Simple file list"), a format
(``csv`` / ``json`` / ``html``) or a template file — into a target; ``export_chunks`` renders it
piece by piece and ``write_export`` writes the pieces to a file atomically, as UTF-8.
"""

from __future__ import annotations

import os
from collections.abc import Iterable, Iterator
from pathlib import Path

import numpy as np

from ..i18n import Texts, _
from ..store.nodes import NodeStore
from .builtin import BUILTIN_INFO, BUILTIN_TEMPLATES, BuiltinInfo
from .formats import (
    FORMATS,
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
    "render",
    "render_chunks",
    "resolve",
    "write_export",
]

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
    source = BUILTIN_TEMPLATES.get(name)
    if source is None:
        folded = name.casefold()
        name = next((k for k in BUILTIN_TEMPLATES if k.casefold() == folded), "")
        source = BUILTIN_TEMPLATES.get(name)
    return parse_template(source, name) if source is not None else None


def resolve(name: str, *, cwd: Path | None = None) -> ExportTemplate | ExportFormat:
    """A built-in template, then a format key, then a template file (relative to ``cwd``)."""
    found = builtin_template(name)
    if found is not None:
        return found
    fmt = FORMATS.get(name.strip().lower())
    if fmt is not None:
        return fmt
    path = Path(name).expanduser()
    if cwd is not None and not path.is_absolute():
        path = cwd / path
    if not path.is_file():
        raise ExportError(TEXT["not_found"].format(name=name, builtins=_("、").join(BUILTIN_TEMPLATES)))
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
) -> Iterator[str]:
    """The output of ``target`` for the tree below ``root`` (0: every scan root), in pieces."""
    if isinstance(target, ExportTemplate):
        return render_chunks(store, root, target, match=match)
    return target.chunks(store, root, match=match, filter_text=filter_text)


def write_export(path: Path, chunks: Iterable[str], *, bom: bool = False) -> int:
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
                data = chunk.encode("utf-8", "backslashreplace")
                fh.write(data)
                written += len(data)
        os.replace(tmp, path)
    except BaseException:
        tmp.unlink(missing_ok=True)
        raise
    return written
