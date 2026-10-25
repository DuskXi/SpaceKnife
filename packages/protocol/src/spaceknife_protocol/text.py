"""Bilingual text for plugin contributions and documents (docs/plugins.md, 「文案」).

A plugin writes every text the user sees once, in both languages, where it declares it:
``T("重复文件", "Duplicate files")`` in Python, ``{ zh = "重复文件", en = "Duplicate files" }`` in
TOML. Models that carry such text to the frontends use :data:`LocalStr`: a plain ``string`` in the
schema, written in the language of the request (:data:`LANG`, set by the host and the plugin SDK
before each call) when the model is serialized, not when it is made: a column defined once at a
module's top level follows the language of every later call. A bare Chinese string is looked up in
the plugin's own English catalog (:data:`CATALOG`, its ``locales/en.json``) and stays as it is when
there is no entry.
"""

from __future__ import annotations

import string
import time
from collections.abc import Mapping
from contextvars import ContextVar
from typing import TYPE_CHECKING, Annotated, Any

from pydantic import (
    BaseModel,
    BeforeValidator,
    ConfigDict,
    Field,
    PlainSerializer,
    ValidatorFunctionWrapHandler,
    WrapValidator,
)

LANG: ContextVar[str] = ContextVar("spaceknife_lang", default="zh-CN")
CATALOG: ContextVar[Mapping[str, str] | None] = ContextVar("spaceknife_catalog", default=None)
UNIT_BASE: ContextVar[int] = ContextVar("spaceknife_unit_base", default=1024)  # the user's 1000 / 1024
MAX_TEXT = 4096
_UNITS = ("B", "KB", "MB", "GB", "TB", "PB")


def fmt_size(n: float, base: int | None = None, *, whole: bool = False) -> str:
    """Bytes as the app writes them: ``"63.6 GB"`` (the user's unit base, as the interface)."""
    b = base or UNIT_BASE.get()
    v, i = float(n), 0
    while abs(v) >= b and i < len(_UNITS) - 1:
        v /= b
        i += 1
    if i == 0:
        return f"{round(v)} B"
    if abs(round(v, 1)) >= b and i < len(_UNITS) - 1:  # 1048575 B is "1.0 MB", not "1024.0 KB"
        v /= b
        i += 1
    text = str(round(v)) if whole and abs(v - round(v)) < 0.05 else f"{v:.1f}"
    return f"{text} {_UNITS[i]}"


def fmt_count(n: float) -> str:
    """``"1,234,567"``."""
    return f"{round(n):,}"


def fmt_date(ts: float) -> str:
    """Unix seconds as ``YYYY-MM-DD HH:MM`` in local time (as the interface); ``—`` for 0."""
    if not ts:
        return "—"
    return time.strftime("%Y-%m-%d %H:%M", time.localtime(ts))


def fmt_ago(ts: float, lang: str | None = None, now: float | None = None) -> str:
    """``"3 天前"`` / ``"3 days ago"`` (minutes, hours, days, months, years)."""
    en = (lang or LANG.get()) == "en"
    if not ts:
        return "unknown" if en else "未知"
    s = max(0.0, (time.time() if now is None else now) - ts)
    if s < 60:
        return "just now" if en else "刚刚"
    for size, zh, one in (
        (31_536_000, "年", "year"),
        (2_592_000, "个月", "month"),
        (86_400, "天", "day"),
        (3_600, "小时", "hour"),
        (60, "分钟", "minute"),
    ):
        if s >= size:
            k = int(s // size)
            return (f"{k} {one}{'' if k == 1 else 's'} ago") if en else f"{k} {zh}前"
    return "just now" if en else "刚刚"


class _Formatter(string.Formatter):
    """``str.format`` plus the app's ways of writing numbers: ``{n:size}`` ``{n:count}``
    ``{t:date}`` ``{t:ago}`` (as the interface writes them, in this formatter's language)."""

    def __init__(self, lang: str) -> None:
        self.lang = lang

    def format_field(self, value: Any, format_spec: str) -> str:
        if format_spec == "size":
            return fmt_size(float(value))
        if format_spec == "count":
            return fmt_count(float(value))
        if format_spec == "date":
            return fmt_date(float(value))
        if format_spec == "ago":
            return fmt_ago(float(value), self.lang)
        return str(super().format_field(value, format_spec))


def format_text(template: str, lang: str = "zh-CN", /, **fields: Any) -> str:
    """``template.format(**fields)`` with ``:size`` ``:count`` ``:date`` ``:ago``."""
    return _Formatter(lang).format(template, **fields)


class Text(BaseModel):
    """One text in Chinese (the source) and English; an empty English text falls back to the catalog."""

    model_config = ConfigDict(extra="forbid", frozen=True)

    zh: str = Field(min_length=1, max_length=MAX_TEXT)
    en: str = Field(default="", max_length=MAX_TEXT)

    def get(self, lang: str | None = None) -> str:
        """The text in ``lang`` (default: the language of the current request)."""
        if (lang or LANG.get()) != "en":
            return self.zh
        if self.en:
            return self.en
        catalog = CATALOG.get()
        return catalog.get(self.zh, self.zh) if catalog else self.zh

    def format(self, **fields: Any) -> Text:
        """Fill ``str.format`` placeholders in both languages; the result is still a :class:`Text`.
        Besides the usual specs: ``{n:size}`` (bytes as the app writes them: 1.2 GB), ``{n:count}``
        (1,234), ``{t:date}`` (unix seconds: 2026-10-24 21:30), ``{t:ago}`` (3 天前 / 3 days ago)."""
        return Text(
            zh=format_text(self.zh, "zh-CN", **fields),
            en=format_text(self.en, "en", **fields) if self.en else "",
        )

    def plural(self, n: int, one: str, /, **fields: Any) -> Text:
        """``T("{n} 组", "{n} groups").plural(n, "1 group")``: the English text for one when ``n``
        is 1 (Chinese has one form); ``n`` is a field too."""
        en = one if n == 1 else self.en
        return Text(zh=self.zh, en=en).format(n=n, **fields)

    def __str__(self) -> str:
        return self.get()


def T(zh: str, en: str = "") -> Text:
    """``T("中文", "English")``: a text in both languages."""
    return Text(zh=zh, en=en)


def localize(value: Any, lang: str | None = None) -> Any:
    """A :class:`Text` (or its ``{zh, en}`` mapping) or a Chinese string -> the string in ``lang``.

    Anything else is returned unchanged (the model's own validation then rejects it).
    """
    if isinstance(value, Text):
        return value.get(lang)
    if isinstance(value, Mapping) and "zh" in value and set(value) <= {"zh", "en"}:
        return Text.model_validate(dict(value)).get(lang)
    if isinstance(value, str) and (lang or LANG.get()) == "en":
        catalog = CATALOG.get()
        if catalog:
            return catalog.get(value, value)
    return value


def as_text(value: Any) -> Any:
    """Coerce a declared text (``Text``, ``{zh, en}``, a plain string) to a :class:`Text`."""
    if isinstance(value, Text):
        return value
    if isinstance(value, str):
        return Text(zh=value) if value else value
    if isinstance(value, Mapping):
        return Text.model_validate(dict(value))
    return value


class LocalText(str):
    """The value of a :data:`LocalStr` field made from a :class:`Text`: a ``str`` (in the language
    of the moment it was made) that keeps the text, so it is written again in the language of the
    request that sends it."""

    text: Text

    def __new__(cls, value: str, text: Text) -> LocalText:
        out = super().__new__(cls, value)
        out.text = text
        return out

    def __reduce__(self) -> Any:
        return (LocalText, (str(self), self.text))


def _local(value: Any, handler: ValidatorFunctionWrapHandler) -> Any:
    text: Text | None = None
    if isinstance(value, LocalText):
        text = value.text
    elif isinstance(value, Text):
        text = value
    elif isinstance(value, Mapping) and "zh" in value and set(value) <= {"zh", "en"}:
        text = Text.model_validate(dict(value))
    if text is None:
        return handler(value)
    return LocalText(handler(text.get()), text)


def _write(value: Any) -> Any:
    if isinstance(value, LocalText):
        return value.text.get()
    return localize(value)


if TYPE_CHECKING:
    # what type checkers see: a field takes a Text too (``Report(title=T("…", "…"))``); the value
    # read back is a str at run time
    LocalStr = str | Text
else:
    LocalStr = Annotated[
        str, WrapValidator(_local), PlainSerializer(_write, return_type=str, when_used="always")
    ]
"""A text field of a model sent to the frontends: ``Text`` in, the request's language out (chosen
when the model is serialized, so a model made once serves every language)."""

TextLike = Annotated[Text, BeforeValidator(as_text)]
"""A declared text: ``T(...)``, ``{zh, en}`` (TOML) or a Chinese string (English from the catalog)."""

__all__ = [
    "CATALOG",
    "LANG",
    "UNIT_BASE",
    "LocalStr",
    "LocalText",
    "T",
    "Text",
    "TextLike",
    "as_text",
    "fmt_ago",
    "fmt_count",
    "fmt_date",
    "fmt_size",
    "format_text",
    "localize",
]
