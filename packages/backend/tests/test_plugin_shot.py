"""``plugin shot`` without a browser: the link's words, the synthetic folder, which build of the
interface it takes and when it says that build is older than its sources."""

from __future__ import annotations

import os
import time
from pathlib import Path

import pytest

from spaceknife import devshot
from spaceknife.devshot import ShotOptions, link_words, renderer_stale, write_tree


def test_the_link_says_what_to_show() -> None:
    words = link_words(
        ShotOptions(
            theme="light", lang="en", select="logs/a b.txt", open="report:my.tool.r", run=["a.b.c", "x.y"]
        )
    )
    assert words == "&theme=light&lang=en&select=logs/a%20b.txt&open=report%3Amy.tool.r&do=a.b.c%2Cx.y"
    assert link_words(ShotOptions()) == ""


def test_the_synthetic_folder(tmp_path: Path) -> None:
    write_tree({"a.txt": 5, "old.bin": (3, "2020-01-01"), "sub": {"c.dat": b"hi"}}, tmp_path / "t")
    assert (tmp_path / "t" / "a.txt").stat().st_size == 5
    assert (tmp_path / "t" / "sub" / "c.dat").read_bytes() == b"hi"
    assert time.gmtime((tmp_path / "t" / "old.bin").stat().st_mtime).tm_year in (2019, 2020)


def test_an_interface_older_than_its_sources_is_named(tmp_path: Path) -> None:
    build = tmp_path / "renderer"
    build.mkdir()
    (build / "index.html").write_text("<html></html>", encoding="utf-8")
    old = time.time() - 10 * 365 * 86400
    os.utime(build / "index.html", (old, old))
    why = renderer_stale(build)  # this repository's sources are newer than ten years
    assert why is not None and "pnpm -C apps/electron build" in why
    os.utime(build / "index.html", None)  # built now: nothing to say
    assert renderer_stale(build) is None
    assert renderer_stale(tmp_path / "nothing") is not None


def test_the_interface_comes_from_ui_then_the_environment(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    out = tmp_path / "out"
    (out / "renderer").mkdir(parents=True)
    (out / "renderer" / "index.html").write_text("x", encoding="utf-8")
    assert devshot.find_renderer(out) == out / "renderer"  # the out/ folder of a build is fine too
    monkeypatch.setenv("SPACEKNIFE_UI", str(out / "renderer"))
    assert devshot.find_renderer(None) == out / "renderer"


def test_the_browser_can_be_named(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("SPACEKNIFE_BROWSER", "/opt/some/chromium")
    assert devshot.find_browser() == "/opt/some/chromium"
