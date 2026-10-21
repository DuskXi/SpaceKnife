"""Filter engine against an independent node-by-node reference evaluator on random trees."""

from __future__ import annotations

import random
from functools import cache
from pathlib import PurePath

import numpy as np
import pytest
from hypothesis import given, settings
from hypothesis import strategies as st

from spaceknife.classes import DEFAULT_CLASSES
from spaceknife.filter import FilterContext, evaluate
from spaceknife.filter.engine import ATTR_FLAGS, TAG_BITS
from spaceknife.store.nodes import Flag, Kind, NodeStore
from spaceknife_protocol.filter import FilterNormal
from spaceknife_protocol.filter_syntax import parse

NOW = 1_800_000_000
DAY = 86400
ROOT = str(PurePath("/data/Scan Root")) if not PurePath("C:/").drive else "C:\\Data\\Scan Root"

STEMS = ["a", "B", "notes", "Temp", "temp", "x", "Ä", "ä", "视频", "Program Files", "tmp", "abc", "Abc"]
EXTS = ["", ".txt", ".TXT", ".jpg", ".mp4", ".log", ".tar.gz", ".", ".Mp3", ".zip", ".ts", ".MTS"]
ATTRS = [Flag.HIDDEN, Flag.READONLY, Flag.SYSTEM, Flag.ARCHIVE, Flag.COMPRESSED, Flag.SPARSE]


def ext_of(name: str) -> str:
    dot = name.rfind(".")
    if dot <= 0 or len(name) - dot > 17:
        return ""
    return name[dot + 1 :].lower()


def build_random_store(seed: int, max_nodes: int = 2500) -> tuple[NodeStore, np.ndarray]:
    rng = random.Random(seed)
    store = NodeStore()
    root = store.add_root(ROOT)
    queue = [(root, 1)]
    while queue and len(store) < max_nodes:
        d, depth = queue.pop(0)
        count = rng.randint(4, 12) if depth < 7 else rng.randint(0, 4)
        names: list[bytes] = []
        cols: dict[str, list[int]] = {
            k: [] for k in ("kind", "flags", "size", "alloc", "mtime", "ctime", "atime")
        }
        exts: list[int] = []
        for slot in range(count):
            r = rng.random()
            if depth < 4 and slot < 2:
                r = 0.0  # keep the tree branching near the top
            kind = Kind.DIR if r < 0.35 and depth < 7 else (Kind.LINK if r > 0.95 else Kind.FILE)
            stem = rng.choice(STEMS)
            name = stem if kind == Kind.DIR else (rng.choice(["", "."]) if rng.random() < 0.05 else "") + stem
            if kind != Kind.DIR:
                name += rng.choice(EXTS)
            flags = 0
            for f in ATTRS:
                if rng.random() < 0.2:
                    flags |= f
            if kind == Kind.FILE and rng.random() < 0.05:
                flags |= Flag.HARDLINK_DUP
            size = (
                0 if kind == Kind.DIR else rng.choice([0, 10, 4000, 4096, 5000, 1 << 20, 3 << 20, 50 << 20])
            )
            names.append(name.encode("utf-8"))
            cols["kind"].append(int(kind))
            cols["flags"].append(flags)
            cols["size"].append(size)
            cols["alloc"].append((size + 4095) // 4096 * 4096)
            for t in ("mtime", "ctime", "atime"):
                cols[t].append(
                    0 if rng.random() < 0.05 else NOW - rng.choice([1, 2, 10, 40, 400, 800]) * DAY - 7
                )
            exts.append(store.ext_id(ext_of(name)) if kind == Kind.FILE else 0)
        entries = {k: np.array(v, dtype=np.int64) for k, v in cols.items()}
        entries["ino"] = np.zeros(count, dtype=np.uint64)
        entries["nlink"] = np.ones(count, dtype=np.uint16)
        first = store.ingest_names(d, names, entries, np.array(exts, dtype=np.uint16))
        for i in range(first, first + count):
            if store.cols["kind"][i] == Kind.DIR:
                queue.append((i, depth + 1))
    tags = np.zeros(len(store), dtype=np.uint8)
    for i in rng.sample(range(1, len(store)), k=min(40, len(store) - 1)):
        tags[i] = rng.choice([1, 2, 4, 8])
    return store, tags


@cache
def random_store(seed: int) -> tuple[NodeStore, np.ndarray]:
    store, tags = build_random_store(seed)
    assert len(store) > 1500 and int(store.cols["depth"][: len(store)].max()) >= 5
    return store, tags


# --------------------------------------------------------------------------- reference


def ref_glob(pattern: str, name: str) -> bool:
    toks: list[str | None] = []  # None = '*', '' = '?', else a literal character
    i = 0
    while i < len(pattern):
        ch = pattern[i]
        if ch == "\\" and i + 1 < len(pattern):
            toks.append(pattern[i + 1])
            i += 2
            continue
        toks.append(None if ch == "*" else "" if ch == "?" else ch)
        i += 1

    @cache
    def go(ti: int, ni: int) -> bool:
        if ti == len(toks):
            return ni == len(name)
        tok = toks[ti]
        if tok is None:
            return go(ti + 1, ni) or (ni < len(name) and go(ti, ni + 1))
        if ni == len(name):
            return False
        if tok == "":
            return go(ti + 1, ni + 1)
        variants = {v for v in (tok, tok.lower(), tok.upper(), tok.title()) if len(v) == 1}
        return name[ni] in variants and go(ti + 1, ni + 1)

    return go(0, 0)


def attrs_hold(flags: int, req: list[str], forb: list[str]) -> bool:
    has = [bool(flags & ATTR_FLAGS[a]) for a in req]  # type: ignore[index]
    lacks = [not flags & ATTR_FLAGS[a] for a in forb]  # type: ignore[index]
    return all(has) and all(lacks)


def reference(store: NodeStore, nf: FilterNormal, now: int, tags: np.ndarray) -> np.ndarray:
    c = store.cols
    n = len(store)
    out = np.zeros(n, dtype=bool)
    for i in range(1, n):
        if c["kind"][i] == Kind.DIR:
            continue
        name = store.name(i)
        chain = list(store.ancestors(i))  # parent … root
        folder_names = [PurePath(store.name(a)).name if c["depth"][a] == 1 else store.name(a) for a in chain]
        ext = store.ext_table[int(c["ext"][i])]
        classes = set(DEFAULT_CLASSES.file_classes(ext, int(c["size"][i]))) or {"other"}
        own_tags = 0
        for a in [i, *chain]:
            own_tags |= int(tags[a])
        flags = int(c["flags"][i])

        ok = True
        if nf.masks or nf.classes:
            ok &= any(ref_glob(m, name) for m in nf.masks) or bool(classes & set(nf.classes))
        ok &= not any(ref_glob(m, name) for m in nf.not_masks)
        ok &= not (classes & set(nf.not_classes))
        if nf.folders:
            ok &= any(ref_glob(f, fn) for f in nf.folders for fn in folder_names)
        ok &= not any(ref_glob(f, fn) for f in nf.not_folders for fn in folder_names)
        if nf.tags is not None:
            ok &= bool(own_tags & sum(TAG_BITS[t] for t in nf.tags))
        ok &= not (own_tags & sum(TAG_BITS[t] for t in nf.not_tags))
        for r in nf.sizes:
            v = int(c["alloc" if r.field == "disk" else "size"][i])
            ok &= (r.lo is None or v >= r.lo) and (r.hi is None or v <= r.hi)
        for r in nf.ages:
            t = int(c[{"modify": "mtime", "create": "ctime", "access": "atime"}[r.field]][i])
            age = now - t
            ok &= t != 0 and (r.lo is None or age >= r.lo) and (r.hi is None or age <= r.hi)
        if nf.attrs is not None:
            ok &= attrs_hold(flags, nf.attrs.required, nf.attrs.forbidden)
        for s in nf.not_attrs:
            ok &= not attrs_hold(flags, s.required, s.forbidden)
        out[i] = ok
    return out


def check(store: NodeStore, tags: np.ndarray, text: str) -> None:
    nf = parse(text).normal
    assert nf is not None, text
    res = evaluate(store, nf, FilterContext(now=NOW, tags=tags))
    want = reference(store, nf, NOW, tags)
    diff = np.nonzero(res.match != want)[0]
    assert diff.size == 0, (text, [(int(i), store.path(int(i))) for i in diff[:5]])
    # bottom-up aggregation: every directory's numbers equal a direct sum over its subtree
    c = store.cols
    counted = (c["flags"][: len(store)] & np.uint32(Flag.HARDLINK_DUP)) == 0
    for d in [0, *store.roots(), *list(np.nonzero(c["kind"][: len(store)] == Kind.DIR)[0][:30])]:
        d = int(d)
        sub = [d]
        k = 0
        while k < len(sub):
            sub.extend(store.children(sub[k]))
            k += 1
        ids = np.array(sub, dtype=np.int64)
        m = want[ids]
        assert res.count[d] == m.sum()
        assert res.alloc[d] == c["alloc"][ids][m & counted[ids]].sum()
        assert res.size[d] == c["size"][ids][m & counted[ids]].sum()


# --------------------------------------------------------------------------- tests

EXPRESSIONS = [
    "",
    "*",
    "*.txt",
    "*.TXT;*.jpg",
    "a*",
    "*a*",
    "?",
    "?*",
    "*.tar.gz",
    "*.",
    ".txt",
    "ä*",
    "Ä.txt",
    "视频*",
    "|*.txt",
    "|*.txt;|*.jpg;|a*",
    ">1mb",
    ">1mb;<10mb",
    "|>1mb",
    "s>4000b;s<=4096b",
    ">=4kb",
    "<5d",
    "m>30d;m<500d",
    "c<5d",
    "a>1y",
    "|<5d",
    ":attr:h",
    ":attr:h+r-s",
    "|:attr:a",
    ":attr:c;|:attr:sp",
    ":red",
    ":tag:r+y",
    "|:all",
    ":all",
    ":class:video",
    ":class:other",
    "|:class:doc",
    "*.log;:class:image",
    "\\temp",
    "\\*temp*",
    "|\\temp",
    "\\scan root",
    "\\*",
    "\\temp;\\program files",
    "\\temp;|*.zip",
    "*.txt;\\temp;>1kb",
    "*.jpg;>1mb;\\a;:attr:-h",
    "\\x;|\\a;:tag:all-red;c>10d",
]


@pytest.mark.parametrize("seed", [1, 2, 3])
@pytest.mark.parametrize("text", EXPRESSIONS)
def test_engine_matches_reference(seed: int, text: str) -> None:
    store, tags = random_store(seed)
    check(store, tags, text)


WORDS = st.sampled_from([w for e in EXPRESSIONS for w in e.split(";") if w])


@settings(max_examples=150, deadline=None)
@given(st.lists(WORDS, min_size=1, max_size=5), st.sampled_from([1, 2, 3]))
def test_engine_matches_reference_on_random_combinations(words: list[str], seed: int) -> None:
    store, tags = random_store(seed)
    check(store, tags, ";".join(words))


def test_growing_store_reuses_the_name_index() -> None:
    full, _ = build_random_store(7, max_nodes=1500)
    # replay the same tree into a fresh store in two halves, evaluating in between
    store = NodeStore()
    store.add_root(full.name(1))
    c = full.cols
    dirs = [int(i) for i in np.nonzero(c["kind"][: len(full)] == Kind.DIR)[0] if i > 0]
    mapping = {1: 1}
    for step, d in enumerate(dirs):
        kids = list(full.children(d))
        if not kids:
            store.ingest_names(mapping[d], [], {k: np.zeros(0, dtype=np.int64) for k in _ENTRY}, None)
            continue
        entries = {k: np.asarray(c[k][kids[0] : kids[-1] + 1]) for k in _ENTRY}
        names = [full.name_bytes(k) for k in kids]
        exts = np.array([store.ext_id(full.ext_table[int(c["ext"][k])]) for k in kids], dtype=np.uint16)
        first = store.ingest_names(mapping[d], names, entries, exts)
        for off, k in enumerate(kids):
            mapping[k] = first + off
        if step == len(dirs) // 2:
            check(store, np.zeros(len(store), dtype=np.uint8), "*a*;\\temp")
    check(store, np.zeros(len(store), dtype=np.uint8), "*a*;\\temp")
    check(store, np.zeros(len(store), dtype=np.uint8), "*.txt;|\\x")


_ENTRY = ("kind", "flags", "size", "alloc", "mtime", "ctime", "atime", "ino", "nlink")


def test_extension_fast_path_handles_dotfiles_links_and_case() -> None:
    store = NodeStore()
    root = store.add_root(ROOT)
    names = ["a.TXT", ".txt", "b.txt.bak", "link.txt", "c.Txt", "txt", "d.tXt"]
    kinds = [Kind.FILE, Kind.FILE, Kind.FILE, Kind.LINK, Kind.FILE, Kind.FILE, Kind.DIR]
    n = len(names)
    entries = {k: np.zeros(n, dtype=np.int64) for k in _ENTRY}
    entries["kind"] = np.array([int(k) for k in kinds])
    exts = np.array(
        [store.ext_id(ext_of(nm)) if k == Kind.FILE else 0 for nm, k in zip(names, kinds, strict=True)],
        dtype=np.uint16,
    )
    first = store.ingest_names(root, [nm.encode() for nm in names], entries, exts)
    nf = parse("*.txt").normal
    assert nf is not None
    res = evaluate(store, nf, FilterContext(now=NOW))
    got = sorted(store.name(int(i)) for i in np.nonzero(res.match)[0])
    assert got == [".txt", "a.TXT", "c.Txt", "link.txt"]  # the directory "d.tXt" never matches
    assert first == 2


def test_hardlink_duplicates_match_but_add_no_bytes() -> None:
    store = NodeStore()
    root = store.add_root(ROOT)
    entries = {k: np.zeros(2, dtype=np.int64) for k in _ENTRY}
    entries["size"] = np.array([1000, 1000])
    entries["alloc"] = np.array([4096, 4096])
    entries["flags"] = np.array([0, int(Flag.HARDLINK_DUP)])
    bin_ext = store.ext_id("bin")
    store.ingest_names(root, [b"a.bin", b"b.bin"], entries, np.full(2, bin_ext, dtype=np.uint16))
    nf = parse("*.bin").normal
    assert nf is not None
    res = evaluate(store, nf, FilterContext(now=NOW))
    assert res.totals(root) == (2, 4096, 1000)


def test_unknown_timestamps_satisfy_no_age_condition() -> None:
    store = NodeStore()
    root = store.add_root(ROOT)
    entries = {k: np.zeros(2, dtype=np.int64) for k in _ENTRY}
    entries["mtime"] = np.array([0, NOW - 3 * DAY])
    store.ingest_names(root, [b"unknown", b"known"], entries, np.zeros(2, dtype=np.uint16))
    for text, want in [
        ("<5d", ["known"]),
        (">5d", []),
        ("|<5d", []),
        ("|>5d", ["known"]),
        ("*", ["known", "unknown"]),
    ]:
        nf = parse(text).normal
        assert nf is not None
        res = evaluate(store, nf, FilterContext(now=NOW))
        assert sorted(store.name(int(i)) for i in np.nonzero(res.match)[0]) == want, text


def test_folder_hits_mark_bold_folders_including_the_root() -> None:
    store, tags = random_store(1)
    nf = parse("\\scan root;\\temp").normal
    assert nf is not None
    res = evaluate(store, nf, FilterContext(now=NOW, tags=tags))
    hits = {store.name(int(i)) for i in np.nonzero(res.folder_hit)[0]}
    assert ROOT in hits  # stored under its full path, matched by its own name
    assert hits <= {ROOT, "Temp", "temp"}
