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
MAX_TEXT = 4096


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
        """Fill ``str.format`` placeholders in both languages; the result is still a :class:`Text`."""
        return Text(zh=self.zh.format(**fields), en=self.en.format(**fields) if self.en else "")

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

__all__ = ["CATALOG", "LANG", "LocalStr", "LocalText", "T", "Text", "TextLike", "as_text", "localize"]
