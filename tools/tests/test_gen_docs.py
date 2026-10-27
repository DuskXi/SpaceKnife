from __future__ import annotations

import re
import sys

import pytest

import gen_docs as g
from spaceknife import settings
from spaceknife.palette import commands


def test_generated_docs_are_up_to_date() -> None:
    assert g.main(["--check"]) == 0, "run: uv run python tools/gen_docs.py"


def test_platform_variants_are_read_and_the_host_is_restored() -> None:
    names = {c[0].id: {x.name for x in c} for c in g.command_variants()}
    assert len(names["item.reveal"]) == len(g.PLATFORMS)  # Explorer / Finder / file manager
    specs = {s[0].key: s for s in g.setting_variants()}
    assert len({s.description for s in specs["scan.monitor"]}) == len(g.PLATFORMS)
    assert [not s.reason for s in specs["scan.elevate"]] == [True, False, False]  # Windows only
    assert [not s.reason for s in specs["scan.protected_places"]] == [False, True, False]  # macOS only
    assert (sys.platform == "win32") == settings.IS_WIN
    assert (sys.platform == "darwin") == settings.IS_MAC
    assert commands.sys is sys


def test_pages_match_the_settings_tab() -> None:
    source = g.SETTINGS_PAGE.read_text(encoding="utf-8")
    ids = re.findall(r'id: "(\w+)",', source)
    assert ids[: len(g.PAGES)] == [p[0] for p in g.PAGES]
    for _id, name, note in g.PAGES:
        assert f't("{name}")' in source
        assert f't("{note}")' in source
    assert {s.category for s in settings.REGISTRY} <= {p[0] for p in g.PAGES}


def test_keys_marked_not_wired_are_still_unhandled() -> None:
    app = "".join(f.read_text(encoding="utf-8") for f in g.KEY_HANDLERS)
    listed = {k for c in commands.registry() for k in c.keys}
    for key, handler in g.NOT_WIRED.items():
        assert key in listed, f"{key} is no longer a palette key: drop it from NOT_WIRED"
        assert handler not in app, f"the window handles {key} now: drop it from gen_docs.NOT_WIRED"


def test_english_blocks_have_no_chinese_text() -> None:
    w = g.Words("en", set())
    for _name, (_file, build) in g.BLOCKS.items():
        text = build(w)
        for word in g.SAME_IN_EVERY_LANGUAGE:
            text = text.replace(word, "")
        assert not g.CJK.search(text), g.CJK.findall(text)[:10]
    assert not w.missing


@pytest.mark.parametrize(
    ("keys", "html"),
    [
        ("Ctrl+Shift+F5", "<kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>F5</kbd>"),
        ("Alt+Left", "<kbd>Alt</kbd>+<kbd>←</kbd>"),
        ("Ctrl+`", "<kbd>Ctrl</kbd>+<kbd>&#96;</kbd>"),
        ("Ctrl++", "<kbd>Ctrl</kbd>+<kbd>+</kbd>"),
        ("/", "<kbd>/</kbd>"),
    ],
)
def test_kbd(keys: str, html: str) -> None:
    assert g.kbd(keys) == html


def test_cell_escapes_table_breakers() -> None:
    assert g.cell("a|b <c> *d* `e`") == r"a\|b &lt;c> \*d\* \`e\`"


def test_replace_block() -> None:
    start, end = g.markers("x")
    text = f"intro\n\n{start}\nold\n{end}\n\nmore\n"
    assert g.replace_block(text, "x", "new") == f"intro\n\n{start}\n\nnew\n\n{end}\n\nmore\n"
    with pytest.raises(g.GenError, match="expected one"):
        g.replace_block("no markers", "x", "new")
    with pytest.raises(g.GenError, match="expected one"):
        g.replace_block(text + text, "x", "new")
