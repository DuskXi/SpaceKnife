"""Replay SpaceSniffer 2.0.5.18's recorded filter results against SpaceKnife (Windows only).

``data/spacesniffer_filter_oracle.json`` holds what SpaceSniffer returned for every probe on the
fixture of ``filter_fixture.py`` (recorded by ``tools/spacesniffer_oracle.py``). SpaceKnife must
return the same files, or SpaceSniffer must have rejected the expression where SpaceKnife does
too — except for the deliberate differences listed in :data:`DIFFERENCES`, each with the section
of docs/filter-syntax.md (§9) that justifies it.

The fixture is scanned with SpaceKnife's scanner. SpaceSniffer counts every non-empty file as at
least one cluster, while SpaceKnife reports NTFS-resident tiny files as taking no clusters
(a scanner difference, §9.4); the allocation column is set to SpaceSniffer's model before
comparing, so this test measures the filter language only.
"""

from __future__ import annotations

import asyncio
import json
import sys
import time
from dataclasses import dataclass
from pathlib import Path

import numpy as np
import pytest

from spaceknife.filter import FilterContext, evaluate
from spaceknife.scan.scanner import Scanner
from spaceknife.store.nodes import Kind
from spaceknife_protocol.filter_syntax import parse

from . import filter_fixture as fx

pytestmark = pytest.mark.skipif(
    sys.platform != "win32", reason="the fixture needs Windows attributes and times"
)

ORACLE = json.loads(
    (Path(__file__).parent / "data" / "spacesniffer_filter_oracle.json").read_text(encoding="utf-8")
)
ALL = frozenset(f.path for f in fx.FILES)
TXT = frozenset(p for p in ALL if p.lower().endswith(".txt"))
BIG = frozenset(
    ("Program Files/x/setup.exe", "big.txt", "music.mp3", "photo.jpg", "sub/movie.mp4", "temp/data.zip")
)


@dataclass(frozen=True)
class Diff:
    section: str  # docs/filter-syntax.md §9 entry
    error: bool = False  # SpaceKnife rejects the expression
    files: frozenset[str] | None = None  # SpaceKnife's result, when it is spelled out
    minus: frozenset[str] = frozenset()  # …or: SpaceSniffer's result without these
    plus: frozenset[str] = frozenset()  # …and with these

    def expected(self, ss: list[str] | None) -> set[str] | None:
        if self.error:
            return None
        if self.files is not None:
            return set(self.files)
        assert ss is not None
        return (set(ss) - self.minus) | self.plus


S = frozenset
DIFFERENCES: dict[str, Diff] = {
    # §9.2 extensions: SpaceSniffer reports an error, SpaceKnife gives the form a meaning
    "m<5d;m>1.5d": Diff("E1 decimals", files=S(("exact4000.bin", "notes.txt", "sub/movie.mp4"))),
    "m>1.5d": Diff("E1 decimals", files=ALL - {"temp/cache.txt"}),
    ">=1mb": Diff("E2 >= and <=", files=BIG),
    ">1tb": Diff("E3 unit spellings", files=S()),
    ">1kib": Diff("E3 unit spellings", files=ALL - {"zero.dat"}),
    ">1mon": Diff("E3 unit spellings", files=S(("app.log", "photo.jpg", "temp/data.zip"))),
    ">1minute": Diff("E3 unit spellings", files=ALL),
    ">1second": Diff("E3 unit spellings", files=ALL),
    ":class:Pictures": Diff("E4 class aliases", files=S(("photo.jpg", "sub/tiny.jpg"))),
    '"notes.txt"': Diff("E5 quoted masks", files=S(("notes.txt",))),
    # §9.3 SpaceSniffer behaviour SpaceKnife deliberately does not copy
    "s>4000b": Diff("D1 strict comparisons", minus=S(("exact4000.bin",))),
    "s<4000b": Diff("D1 strict comparisons", minus=S(("exact4000.bin",))),
    "s>4000b;s<4000b": Diff("D1 strict comparisons", files=S()),
    ">0b": Diff("D1 strict comparisons", minus=S(("zero.dat",))),
    "|>1mb": Diff("D2 negated ranges", minus=BIG),
    "|<5d": Diff(
        "D2 negated ranges", minus=S(("exact4000.bin", "notes.txt", "sub/movie.mp4", "temp/cache.txt"))
    ),
    ">1mb;|>10mb": Diff("D2 negated ranges", minus=S(("photo.jpg", "sub/movie.mp4"))),
    ">1mb;>2mb": Diff("D3 ranges intersect", minus=S(("Program Files/x/setup.exe",))),
    "| *.txt": Diff("D4 spaces after | and \\", minus=TXT),
    "\\ temp": Diff(
        "D4 spaces after | and \\", plus=S(("sub/deeper/temp/x.txt", "temp/cache.txt", "temp/data.zip"))
    ),
    "created<5d": Diff(
        "D5 every prefix spelling works",
        plus=S(("exact4000.bin", "notes.txt", "sub/tiny.jpg", "temp/cache.txt", "temp/data.zip")),
    ),
    "\\fixture": Diff("D6 the scan root by its own name", files=ALL),
    "|*.exe;|*.dll;\\*temp*;|\\*windows*": Diff(
        "D6 the scan root by its own name",
        files=S(("sub/deeper/temp/x.txt", "temp/cache.txt", "temp/data.zip")),
    ),
    "\\temp\\x": Diff("D7 forms that can never match are errors", error=True),
    ":foo": Diff("D7 forms that can never match are errors", error=True),
}


def _scan(root: Path) -> tuple[object, int]:
    scanner = Scanner([str(root)])
    asyncio.run(scanner.run())
    store = scanner.store
    c, n = store.cols, len(store)
    leaf = c["kind"][:n] != Kind.DIR
    c["alloc"][:n] = np.where(
        leaf, (c["size"][:n] + 4095) // 4096 * 4096, c["alloc"][:n]
    )  # SpaceSniffer's model
    return scanner, scanner.roots[0].node


@pytest.fixture(scope="module")
def trees(tmp_path_factory: pytest.TempPathFactory) -> dict[str, tuple[object, int, int]]:
    base = tmp_path_factory.mktemp("ss-compat")
    now = time.time()
    fx.build(base / "fixture", now)
    fx.build(base / "units", now, fx.UNIT_FILES)
    # SpaceSniffer evaluated some seconds after building; never exactly on an age boundary
    later = int(now) + 30
    out = {}
    for name in ("fixture", "units"):
        scanner, root = _scan(base / name)
        out[name] = (scanner, root, later)
    return out


def ours(trees: dict[str, tuple[object, int, int]], tree: str, text: str) -> set[str] | None:
    scanner, root, now = trees[tree]
    store = scanner.store  # type: ignore[attr-defined]
    nf = parse(text).normal
    if nf is None:
        return None
    res = evaluate(store, nf, FilterContext(now=now))
    prefix = len(store.path(root)) + 1
    return {store.path(int(i))[prefix:].replace("\\", "/") for i in np.nonzero(res.match)[0]}


CASES = [("fixture", e) for e in ORACLE["results"]] + [("units", e) for e in ORACLE.get("unit_results", {})]


def test_oracle_is_for_this_fixture() -> None:
    assert ORACLE["fixture_version"] == fx.VERSION
    assert set(ORACLE["results"]) >= set(fx.PROBES) | set(fx.RECHECK)
    assert set(DIFFERENCES) <= set(ORACLE["results"])


@pytest.mark.parametrize(("tree", "text"), CASES, ids=[f"{t}:{e}" for t, e in CASES])
def test_same_result_as_spacesniffer(trees: dict[str, tuple[object, int, int]], tree: str, text: str) -> None:
    ss = ORACLE["results" if tree == "fixture" else "unit_results"][text]
    got = ours(trees, tree, text)
    diff = DIFFERENCES.get(text) if tree == "fixture" else None
    if diff is not None:
        assert got == diff.expected(ss), diff.section
        assert got != (None if ss is None else set(ss)), (
            f"{text!r} matches SpaceSniffer now: drop it from DIFFERENCES"
        )
        return
    if ss is None:
        assert got is None, (
            f"SpaceSniffer rejects {text!r} ({ORACLE['dialogs'].get(text)}) but SpaceKnife accepts it"
        )
    else:
        assert got == set(ss)
