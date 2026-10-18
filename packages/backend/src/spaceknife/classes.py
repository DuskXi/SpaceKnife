"""File-class lookups over the scanned extension table (``:class:`` filter, treemap colours).

The built-in definitions are shared data in :mod:`spaceknife_protocol.classes`. The user can edit
the table (W7 「文件类别」): it is kept in ``config_dir()/classes.json`` and becomes the *active*
table every component reads through :func:`active` — the filter engine, the layout's colour
classes, the parser vocabulary.
"""

from __future__ import annotations

import json
import re
import threading
from pathlib import Path

import numpy as np
from loguru import logger

from spaceknife_protocol.classes import BUILTIN_CLASSES, OTHER, ClassEdit
from spaceknife_protocol.filter import FilterClassInfo

# User-facing text, kept in one place for the i18n extraction (M7.5).
TEXT = {
    "empty_name": "类别名不能为空",
    "dup_name": "类别名「{name}」重复了（与「{other}」冲突）",
    "bad_name": "类别名里不能有「{chars}」",
    "other_missing": "「其他」类别不能删除",
    "other_exts": "「其他」类别不能填扩展名：它收纳所有未归类的扩展名",
    "bad_ext": "扩展名「{ext}」不合法",
}
_EXT_SPLIT = re.compile(r"[;,\s]+")
_BAD_NAME = re.compile(r'[;:"\\]')  # would break `:class:<name>` in the filter syntax


class ClassTable:
    """Classes in display order (``other`` last), with per-extension membership."""

    def __init__(self, classes: tuple[FilterClassInfo, ...] = BUILTIN_CLASSES) -> None:
        self.classes = classes
        self.by_id = {c.id: c for c in classes}
        if OTHER not in self.by_id:
            raise ValueError("the class table needs an 'other' class")
        self.ids = [c.id for c in classes]
        self.other_index = self.ids.index(OTHER)
        self._ext_classes: dict[str, list[str]] = {}
        for c in classes:
            for ext in c.extensions:
                owners = self._ext_classes.setdefault(ext.lower(), [])
                if c.id not in owners:
                    owners.append(c.id)
        names: dict[str, str] = {}
        for c in classes:
            for name in (c.id, c.name, *c.aliases):
                if names.setdefault(name.casefold(), c.id) != c.id:
                    raise ValueError(f"class name {name!r} is used by two classes")

    def classes_of(self, ext: str) -> list[str]:
        return self._ext_classes.get(ext.lower(), [])

    def color_class(self, ext: str) -> str:
        owners = self._ext_classes.get(ext.lower())
        return owners[0] if owners else OTHER

    def ext_mask(self, class_id: str, ext_table: list[str]) -> np.ndarray:
        """Boolean array over extension-table ids: the extensions that belong to ``class_id``.

        For ``other`` these are the extensions (including "no extension") that are in no class.
        """
        if class_id == OTHER:
            return np.fromiter((not self._ext_classes.get(e) for e in ext_table), bool, len(ext_table))
        return np.fromiter(
            (class_id in self._ext_classes.get(e, ()) for e in ext_table), bool, len(ext_table)
        )

    @property
    def custom(self) -> bool:
        return self.classes != BUILTIN_CLASSES


DEFAULT_CLASSES = ClassTable()

# --------------------------------------------------------------------------- the active table

_lock = threading.Lock()
_active = DEFAULT_CLASSES
_version = 0


def active() -> ClassTable:
    """The table in use now (the user's, else the built-in one)."""
    return _active


def version() -> int:
    """Bumped whenever the active table changes (caches key on it)."""
    return _version


def set_active(table: ClassTable) -> None:
    global _active, _version
    with _lock:
        _active = table
        _version += 1


def _norm_ext(raw: str) -> str:
    return raw.strip().lstrip(".").lower()


def build(edits: list[ClassEdit]) -> ClassTable:
    """Validate the editor's list (UI-W7-33) into a table; raises ``ValueError`` with a Chinese message.

    Extensions: ``;`` separated like SpaceSniffer, spaces and commas accepted, leading dots and
    case dropped. Built-in classes keep their aliases (SpaceSniffer's names keep matching) unless
    renamed onto one of them. ``other`` cannot go, holds no extensions, and stays last.
    """
    out: list[FilterClassInfo] = []
    taken: dict[str, str] = {}
    builtin = {c.id: c for c in BUILTIN_CLASSES}
    used_ids: set[str] = set()
    other: FilterClassInfo | None = None
    for e in edits:
        name = e.name.strip()
        if not name:
            raise ValueError(TEXT["empty_name"])
        bad = sorted(set(_BAD_NAME.findall(name)))
        if bad:
            raise ValueError(TEXT["bad_name"].format(chars="".join(bad)))
        exts: list[str] = []
        for chunk in e.extensions:
            for raw in _EXT_SPLIT.split(chunk):
                ext = _norm_ext(raw)
                if not ext:
                    continue
                if "/" in ext or "\\" in ext or len(ext) > 32:
                    raise ValueError(TEXT["bad_ext"].format(ext=raw))
                if ext not in exts:
                    exts.append(ext)
        cid = e.id if e.id else _new_id(name, used_ids)
        if cid == OTHER and exts:
            raise ValueError(TEXT["other_exts"])
        used_ids.add(cid)
        base = builtin.get(cid)
        aliases = [a for a in (base.aliases if base else []) if a.casefold() != name.casefold()]
        if base is not None and base.name.casefold() != name.casefold():
            aliases.append(base.name)  # renamed: `:class:<old name>` keeps working
        for spelling in (cid, name, *aliases):
            owner = taken.get(spelling.casefold())
            if owner is not None and owner != cid:
                raise ValueError(TEXT["dup_name"].format(name=spelling, other=_display(out, owner)))
        info = FilterClassInfo(id=cid, name=name, aliases=aliases, extensions=exts, color=e.color.upper())
        for spelling in (cid, name, *aliases):
            taken[spelling.casefold()] = cid
        if cid == OTHER:
            other = info
        else:
            out.append(info)
    if other is None:
        raise ValueError(TEXT["other_missing"])
    return ClassTable((*out, other))


def _display(classes: list[FilterClassInfo], cid: str) -> str:
    return next((c.name for c in classes if c.id == cid), cid)


def _new_id(name: str, used: set[str]) -> str:
    slug = re.sub(r"[^a-z0-9]+", "-", name.casefold()).strip("-") or "class"
    cid = f"u-{slug}"[:40]
    n = 2
    while cid in used or cid in {c.id for c in BUILTIN_CLASSES}:
        cid = f"u-{slug}-{n}"[:40]
        n += 1
    return cid


# --------------------------------------------------------------------------- the file


def load(path: Path) -> ClassTable:
    """The table saved in ``path``; the built-in one when there is none or it is broken."""
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
        classes = tuple(FilterClassInfo.model_validate(c) for c in raw["classes"])
        return ClassTable(classes)
    except FileNotFoundError:
        return DEFAULT_CLASSES
    except (OSError, ValueError, KeyError, TypeError) as exc:
        logger.warning("类别表 {} 读不出来，改用内置类别：{}", path, exc)
        return DEFAULT_CLASSES


def user_file() -> Path:
    from .paths import config_dir

    return config_dir() / "classes.json"


def load_user() -> None:
    """Make the user's saved table active (server start, the command line)."""
    set_active(load(user_file()))


def save(table: ClassTable, path: Path) -> None:
    """Write atomically; the built-in table removes the file (nothing to remember)."""
    if not table.custom:
        path.unlink(missing_ok=True)
        return
    path.parent.mkdir(parents=True, exist_ok=True)
    doc = {"version": 1, "classes": [c.model_dump(mode="json") for c in table.classes]}
    tmp = path.with_suffix(".tmp")
    tmp.write_text(json.dumps(doc, ensure_ascii=False, indent=1), encoding="utf-8")
    tmp.replace(path)
