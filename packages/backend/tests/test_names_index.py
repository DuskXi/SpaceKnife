"""NameIndex: whole-character ``?`` and concurrent use from several threads."""

from __future__ import annotations

import fnmatch
import random
import re
import sys
import threading
from concurrent.futures import ThreadPoolExecutor

import numpy as np
import pytest

from spaceknife.filter import FilterContext, evaluate
from spaceknife.filter import names as fnames
from spaceknife.filter.names import NameIndex
from spaceknife.store.nodes import ENTRY_COLUMNS, NodeStore
from spaceknife_protocol.filter_syntax import normalize_glob, parse


def flat_store(names: list[str]) -> NodeStore:
    """One root holding ``names`` as files."""
    store = NodeStore()
    root = store.add_root("root")
    entries = {k: np.zeros(len(names), dtype=np.int64) for k in ENTRY_COLUMNS}
    store.ingest_names(root, [nm.encode() for nm in names], entries)
    return store


# ------------------------------------------------------------------ "?" is one whole character

ONE_CHAR_NAMES = ["é", "ab", "中", "中.txt", "中é", "xé"]
ONE_CHAR_CASES = [
    ("?", ["é", "中"]),
    ("??", ["ab", "xé", "中é"]),
    ("???", []),
    ("?.txt", ["中.txt"]),
    ("???.txt", []),
    ("中?", ["中é"]),  # non-ASCII glob: the case-variant regex
    ("中??", []),
]


@pytest.mark.parametrize(("pattern", "expected"), ONE_CHAR_CASES)
def test_question_mark_never_matches_part_of_a_character(pattern: str, expected: list[str]) -> None:
    store = flat_store(ONE_CHAR_NAMES)
    ids = NameIndex(store).match(normalize_glob(pattern), len(store))
    assert sorted(store.name(i) for i in ids.tolist()) == sorted(expected)


@pytest.mark.parametrize(("pattern", "expected"), ONE_CHAR_CASES)
def test_filter_question_mark_counts_characters(pattern: str, expected: list[str]) -> None:
    store = flat_store(ONE_CHAR_NAMES)
    normal = parse(pattern).normal
    assert normal is not None
    res = evaluate(store, normal, FilterContext(now=1_800_000_000))
    assert sorted(store.name(i) for i in np.flatnonzero(res.match).tolist()) == sorted(expected)


# ------------------------------------------------------------------ threads

STEMS = ["f", "F", "file", "Fig", "a9", "x7", "log", "Q", "ff9", "notes"]
EXTS = [".txt", ".TXT", ".log", "", ".f9"]
MASKS = [f"*{d}*.txt" for d in range(10)] + [f"f{d}*" for d in range(10)]  # > the cache size
TIERS = ["f12", "fi9", "x7"]
SEQ = re.compile(rb"f[^\x00]*?9")


def _tiers(names: list[str], q: str) -> list[np.ndarray]:
    def subsequence(name: str) -> bool:
        it = iter(name)
        return all(ch in it for ch in q)

    out: list[list[int]] = [[], [], [], []]
    for i, raw in enumerate(names):
        name = raw.lower()
        if name == q:
            out[0].append(i)
        elif name.startswith(q):
            out[1].append(i)
        elif q in name:
            out[2].append(i)
        elif len(q) >= 2 and subsequence(name):
            out[3].append(i)
    return [np.array(t, dtype=np.int64) for t in out]


def test_concurrent_update_and_queries_stay_complete(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(fnames, "SEGMENT_NODES", 61)  # many segments: many interleavings
    rng = random.Random(7)
    store = flat_store(
        [
            f"{rng.choice(STEMS)}{rng.randrange(10 ** rng.randrange(1, 5))}{rng.choice(EXTS)}"
            for _ in range(12000)
        ]
    )
    total = len(store)
    names = [store.name(i) for i in range(total)]
    lowered = [n.lower() for n in names]
    masks = {
        m: np.array([i for i, n in enumerate(lowered) if fnmatch.fnmatchcase(n, m)], dtype=np.int64)
        for m in MASKS
    }
    seq = np.array([i for i, n in enumerate(lowered) if re.search("f.*9", n)], dtype=np.int64)
    tiers = {q: _tiers(names, q) for q in TIERS}
    index = NameIndex(store)
    workers = 8
    barrier = threading.Barrier(workers)

    def work(t: int) -> int:
        checks = 0
        barrier.wait()
        steps = sorted({*range(200 + 37 * t, total, 900 + 50 * t), total})
        for k, n in enumerate(steps):
            index.update(n)
            mask = MASKS[(t + k) % len(MASKS)]
            got = index.match(mask, n)
            want = masks[mask]
            assert np.array_equal(got, want[want < n]), (mask, n)
            q = TIERS[(t + k) % len(TIERS)]
            for g, w in zip(index.ascii_tiers(q, n), tiers[q], strict=True):
                assert np.array_equal(g, w[w < n]), (q, n)
            got = index.search("seq:f9", SEQ, lower=True, n=n)
            assert np.array_equal(got, seq[seq < n]), n
            checks += 1
        return checks

    old = sys.getswitchinterval()
    sys.setswitchinterval(1e-6)  # switch threads as often as possible
    try:
        with ThreadPoolExecutor(workers) as pool:
            done = [f.result() for f in [pool.submit(work, t) for t in range(workers)]]
    finally:
        sys.setswitchinterval(old)
    assert all(done)
    # every node indexed exactly once, in order
    end = 0
    for first, _, sep in index.segments:
        assert first == end
        end = first + len(sep)
    assert end == index.n == total
    for mask, want in masks.items():
        assert np.array_equal(index.match(mask, total), want)
