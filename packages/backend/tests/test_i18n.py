"""Interface languages (M7.5): detection, switching, translation, and no untranslated text left."""

from __future__ import annotations

import importlib.util
import sys
from collections.abc import Iterator
from pathlib import Path

import pytest

from spaceknife import i18n
from spaceknife.server import Server
from spaceknife_client.connection import Connection
from spaceknife_protocol import settings as ps
from spaceknife_protocol.base import Empty
from spaceknife_protocol.methods import FILTER_VOCAB, SETTINGS_GET, SETTINGS_UPDATE

ROOT = Path(__file__).resolve().parents[3]


@pytest.fixture
def unforced(monkeypatch: pytest.MonkeyPatch) -> Iterator[None]:
    """The language from the setting, not the test-wide SPACEKNIFE_LANGUAGE."""
    monkeypatch.delenv("SPACEKNIFE_LANGUAGE", raising=False)
    monkeypatch.setenv("SPACEKNIFE_SYSTEM_LOCALE", "zh-CN")
    i18n.system_language.cache_clear()
    yield
    i18n.system_language.cache_clear()
    i18n.set_language("zh-CN")


@pytest.mark.parametrize(
    ("tag", "lang"),
    [
        ("zh_CN.UTF-8", "zh-CN"),
        ("zh-Hans-CN", "zh-CN"),
        ("zh-TW", "zh-CN"),
        ("en_US.UTF-8", "en"),
        ("de-DE", "en"),
        ("C", None),
        ("POSIX", None),
        ("", None),
        (None, None),
    ],
)
def test_normalize(tag: str | None, lang: str | None) -> None:
    assert i18n.normalize(tag) == lang


def test_system_language_order(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("SPACEKNIFE_LANGUAGE", raising=False)
    monkeypatch.setenv("SPACEKNIFE_SYSTEM_LOCALE", "en-GB")
    i18n.system_language.cache_clear()
    try:
        assert i18n.system_language() == "en"
        assert i18n.resolve("system") == "en"
        assert i18n.resolve("zh-CN") == "zh-CN"  # an explicit setting wins over the system
        monkeypatch.setenv("SPACEKNIFE_LANGUAGE", "zh-CN")  # forced (tests, diagnostics)
        i18n.system_language.cache_clear()
        assert i18n.resolve("en") == "zh-CN"
    finally:
        i18n.system_language.cache_clear()


def test_translation_helpers() -> None:
    table = i18n.Texts({"x": "未知空间", "y": "没有这个词{n}"})
    try:
        assert table["x"] == "未知空间" and i18n._("未知空间") == "未知空间"
        i18n.set_language("en")
        assert i18n.catalog("en"), "the English catalog is empty"
        english = i18n.catalog("en").get("未知空间")
        if english:
            assert table["x"] == english == i18n._("未知空间")
        assert table["y"] == "没有这个词{n}"  # no translation: the source text
        assert i18n.tr("没有这个词{n}", n=3) == "没有这个词3"
        assert table.raw("x") == "未知空间"
    finally:
        i18n.set_language("zh-CN")


async def test_switching_the_setting(unforced: None, server: Server, client: Connection) -> None:
    state = await client.call(SETTINGS_GET, Empty())
    assert state.language == "zh-CN"
    zh_name = next(m.name for m in state.meta if m.key == "general.language")
    assert zh_name == "界面语言"
    state = await client.call(SETTINGS_UPDATE, ps.SettingsUpdateParams(values={"general.language": "en"}))
    assert state.language == "en" and i18n.language() == "en"
    meta = {m.key: m for m in state.meta}
    assert meta["general.language"].name != "界面语言"
    assert "界面语言" in meta["general.language"].synonyms  # the Chinese name stays searchable
    vocab = await client.call(FILTER_VOCAB, Empty())
    assert "Video" in [c.name for c in vocab.classes]
    state = await client.call(SETTINGS_UPDATE, ps.SettingsUpdateParams(values={"general.language": "system"}))
    assert state.language == "zh-CN"  # SPACEKNIFE_SYSTEM_LOCALE above


def _checker() -> object:
    spec = importlib.util.spec_from_file_location("i18n_check", ROOT / "tools" / "i18n_check.py")
    assert spec and spec.loader
    mod = importlib.util.module_from_spec(spec)
    sys.modules["i18n_check"] = mod
    spec.loader.exec_module(mod)
    return mod


def test_backend_has_no_untranslated_text() -> None:
    check = _checker()
    ids, problems = check.scan_python(check.backend_files())  # type: ignore[attr-defined]
    assert not problems, "\n".join(map(str, problems[:30]))
    catalog, conflicts = check.load_catalog(check.BACKEND_CATALOG)  # type: ignore[attr-defined]
    assert not conflicts, "\n".join(conflicts[:30])
    missing = check.check_catalog(ids, catalog, "backend")  # type: ignore[attr-defined]
    assert not missing, "\n".join(missing[:30])


def test_electron_has_no_untranslated_text() -> None:
    check = _checker()
    found = check.scan_electron()  # type: ignore[attr-defined]
    if found is None:
        pytest.skip("needs node and apps/electron/node_modules")
    ids, problems = found
    assert not problems, "\n".join(map(str, problems[:30]))
    catalog, conflicts = check.load_catalog(check.ELECTRON_CATALOG)  # type: ignore[attr-defined]
    assert not conflicts, "\n".join(conflicts[:30])
    missing = check.check_catalog(ids, catalog, "electron")  # type: ignore[attr-defined]
    assert not missing, "\n".join(missing[:30])
