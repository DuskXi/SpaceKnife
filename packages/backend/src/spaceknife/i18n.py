"""Interface languages (M7.5): Simplified Chinese (the source text) and English.

The Chinese text in the code is the message id, gettext style: ``_("开始扫描")`` returns it as is
in Chinese and looks it up in the English catalog otherwise. The catalog is several JSON files
under ``locales/en/`` (one per area of the code, merged when loaded): ``{"中文原文": "English"}``.
Placeholders are ``str.format`` fields and stay the same in both languages
(``tr("已导出 {n} 项", n=3)``).

Translate when the text is *shown*, never at import time: the language can change while the
backend runs (setting 「界面语言」). Module-level tables of user-facing text are :class:`Texts`,
whose items translate on access.

The language: setting ``general.language`` = ``system`` | ``zh-CN`` | ``en``; ``system`` resolves
with :func:`system_language` — ``SPACEKNIFE_LANGUAGE`` (tests, forced), then
``SPACEKNIFE_SYSTEM_LOCALE`` (Electron passes the OS UI language it sees), then the platform:
Windows ``GetUserDefaultUILanguage``, ``LANGUAGE`` / ``LC_ALL`` / ``LC_MESSAGES`` / ``LANG``,
macOS ``AppleLanguages``. Any Chinese locale gives Simplified Chinese, everything else English.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import threading
from collections.abc import Mapping
from functools import cache
from pathlib import Path
from typing import Any, Literal

from loguru import logger

Language = Literal["zh-CN", "en"]
LANGUAGES: tuple[Language, ...] = ("zh-CN", "en")
SOURCE: Language = "zh-CN"
LOCALES = Path(__file__).parent / "locales"

_lock = threading.Lock()
_language: Language = SOURCE
_missing: set[str] = set()


def normalize(tag: str | None) -> Language | None:
    """A locale tag (``zh_CN.UTF-8``, ``zh-Hans``, ``en-US``, ``C``) -> a supported language."""
    if not tag:
        return None
    t = tag.strip().replace("_", "-").split(".")[0].split("@")[0].lower()
    if not t or t in ("c", "posix"):
        return None
    return "zh-CN" if t.startswith("zh") else "en"


def _windows_ui_language() -> Language | None:
    if sys.platform != "win32":
        return None
    import ctypes

    try:
        langid = int(ctypes.windll.kernel32.GetUserDefaultUILanguage())
    except (AttributeError, OSError):
        return None
    return "zh-CN" if (langid & 0x3FF) == 0x04 else "en"  # LANG_CHINESE is primary language 4


def _mac_language() -> Language | None:
    if sys.platform != "darwin":
        return None
    try:
        out = subprocess.run(
            ["defaults", "read", "-g", "AppleLanguages"],
            capture_output=True,
            text=True,
            timeout=2,
            check=False,
        ).stdout
    except (OSError, subprocess.SubprocessError):
        return None
    for token in out.replace("(", " ").replace(")", " ").replace(",", " ").split():
        lang = normalize(token.strip('"'))
        if lang:
            return lang
    return None


@cache
def system_language() -> Language:
    """The operating system's interface language, reduced to a supported one (cached)."""
    for var in ("SPACEKNIFE_LANGUAGE", "SPACEKNIFE_SYSTEM_LOCALE"):
        lang = normalize(os.environ.get(var))
        if lang:
            return lang
    lang = _windows_ui_language() or _mac_language()
    if lang:
        return lang
    for var in ("LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"):
        value = os.environ.get(var, "")
        lang = normalize(value.split(":")[0]) if value else None
        if lang:
            return lang
    return "en"


def resolve(setting: str | None) -> Language:
    """The value of ``general.language`` -> the language to use."""
    if os.environ.get("SPACEKNIFE_LANGUAGE"):  # forced (tests, diagnostics)
        return system_language()
    return normalize(setting) if setting and setting != "system" else system_language()  # type: ignore[return-value]


def language() -> Language:
    return _language


def set_language(lang: Language) -> None:
    global _language
    with _lock:
        _language = lang


@cache
def catalog(lang: Language = "en") -> Mapping[str, str]:
    """All entries of a language, merged from ``locales/<lang>/*.json`` (later files win)."""
    out: dict[str, str] = {}
    folder = LOCALES / lang
    if folder.is_dir():
        for f in sorted(folder.glob("*.json")):
            try:
                data = json.loads(f.read_text(encoding="utf-8"))
            except (OSError, ValueError) as exc:
                logger.warning("translation catalog {} is unreadable: {}", f, exc)
                continue
            out.update({str(k): str(v) for k, v in data.items() if v})
    return out


def _(text: str) -> str:
    """``text`` in the current language (the Chinese source text when there is no translation)."""
    lang = _language
    if lang == SOURCE:
        return text
    found = catalog(lang).get(text)
    if found is None:
        if text not in _missing:
            _missing.add(text)
            logger.debug("no {} translation for {!r}", lang, text)
        return text
    return found


def tr(text: str, /, **fields: Any) -> str:
    """``_(text).format(**fields)``."""
    out = _(text)
    return out.format(**fields) if fields else out


class Texts(dict[str, str]):
    """A module's table of user-facing text: items are translated each time they are read."""

    def __getitem__(self, key: str) -> str:
        return _(super().__getitem__(key))

    def get(self, key: str, default: Any = None) -> Any:
        value = super().get(key)
        return _(value) if isinstance(value, str) else default

    def raw(self, key: str) -> str:
        """The Chinese source text (message id)."""
        return super().__getitem__(key)
