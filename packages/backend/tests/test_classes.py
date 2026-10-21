"""Built-in file classes, and ``ts`` / ``mts`` told apart by size (classes.STREAM_SPLITS)."""

from __future__ import annotations

import numpy as np
import pytest

from spaceknife.classes import DEFAULT_CLASSES, STREAM_ALIGNED_MIN, STREAM_ANY_MIN, ClassTable
from spaceknife.filter import FilterContext, evaluate
from spaceknife.layout.basis import Basis
from spaceknife.store.subtree import summarize
from spaceknife_protocol.classes import BUILTIN_CLASSES, ENGLISH_NAMES, OTHER
from spaceknife_protocol.filter_syntax import parse

from .store_builder import build, find

TS_REC = 188 * 5600  # 1.0 MiB of whole 188-byte packets (not whole 192-byte ones)
MTS_REC = 192 * 5500  # 1.0 MiB of whole 192-byte packets (not whole 188-byte ones)
SIZES = [0, 1, 4096, 65_536, STREAM_ALIGNED_MIN - 188, TS_REC, TS_REC + 1, MTS_REC, MTS_REC + 1]
SIZES += [3 << 20, STREAM_ANY_MIN - 1, STREAM_ANY_MIN, 5 * STREAM_ANY_MIN + 7]


def test_sizes_are_what_they_claim() -> None:
    assert TS_REC >= STREAM_ALIGNED_MIN and TS_REC % 188 == 0 and TS_REC % 192
    assert MTS_REC >= STREAM_ALIGNED_MIN and MTS_REC % 192 == 0 and MTS_REC % 188


def test_only_deliberate_extensions_are_in_two_classes() -> None:
    shared = {e for c in BUILTIN_CLASSES for e in c.extensions if len(DEFAULT_CLASSES.classes_of(e)) > 1}
    assert shared == {"3gp", "ts", "mts"}


def test_builtin_extensions_are_clean() -> None:
    for c in BUILTIN_CLASSES:
        assert len(c.extensions) == len(set(c.extensions)), c.id
        for e in c.extensions:
            assert e == e.lower() and e.isascii() and e.isalnum(), (c.id, e)
    assert next(c for c in BUILTIN_CLASSES if c.id == OTHER).extensions == []
    assert BUILTIN_CLASSES[-1].id == OTHER


def test_every_builtin_class_has_an_english_name_among_its_aliases() -> None:
    assert set(ENGLISH_NAMES) == {c.id for c in BUILTIN_CLASSES}
    for c in BUILTIN_CLASSES:
        assert ENGLISH_NAMES[c.id] in c.aliases, c.id


@pytest.mark.parametrize(
    ("ext", "size", "want"),
    [
        ("ts", 12_000, ["code"]),  # a TypeScript source
        ("ts", TS_REC, ["video"]),  # whole packets
        ("ts", TS_REC + 1, ["code"]),
        ("ts", MTS_REC, ["code"]),  # 192-byte packets are .mts / .m2ts
        ("ts", STREAM_ALIGNED_MIN - 188, ["code"]),  # whole packets, but too small to tell
        ("ts", STREAM_ANY_MIN, ["video"]),  # no source file is this large
        ("TS", STREAM_ANY_MIN + 1, ["video"]),
        ("mts", 4000, ["code"]),  # TypeScript ES module
        ("mts", MTS_REC, ["video"]),  # AVCHD
        ("mts", TS_REC, ["code"]),
        ("m2ts", 4000, ["video"]),  # only video: no split
        ("mp4", 10, ["video"]),
        ("3gp", 10, ["video", "audio"]),
    ],
)
def test_file_classes(ext: str, size: int, want: list[str]) -> None:
    assert DEFAULT_CLASSES.file_classes(ext, size) == want


def _edited(**changes: list[str] | None) -> ClassTable:
    """The built-in table with some classes' extensions replaced (None: class removed)."""
    out = []
    for c in BUILTIN_CLASSES:
        if c.id in changes:
            exts = changes[c.id]
            if exts is None:
                continue
            c = c.model_copy(update={"extensions": exts})
        out.append(c)
    return ClassTable(tuple(out))


def test_user_edits_decide_the_split() -> None:
    video = next(c for c in BUILTIN_CLASSES if c.id == "video").extensions
    # ts taken out of video: the extension decides again
    no_ts = _edited(video=[e for e in video if e != "ts"])
    assert no_ts.file_classes("ts", STREAM_ANY_MIN) == ["code"]
    assert no_ts.splits(["", "ts"]) == []
    # no code class: a source is in no class (「其他」), a recording still video
    no_code = _edited(code=None)
    assert no_code.file_classes("ts", 3000) == []
    assert no_code.file_classes("ts", TS_REC) == ["video"]


@pytest.mark.parametrize("table", [DEFAULT_CLASSES, _edited(code=None)], ids=["builtin", "no-code"])
def test_node_mask_matches_file_classes(table: ClassTable) -> None:
    ext_table = ["", "ts", "mts", "mp4", "py", "m2ts", "3gp", "zzz"]
    ext = np.repeat(np.arange(len(ext_table)), len(SIZES))
    size = np.tile(np.array(SIZES, dtype=np.int64), len(ext_table))
    for cid in table.ids:
        got = table.node_mask(cid, ext_table, ext, size)
        want = [
            cid in (table.file_classes(ext_table[e], int(s)) or [OTHER])
            for e, s in zip(ext.tolist(), size.tolist(), strict=True)
        ]
        assert got.tolist() == want, cid


SPEC = {
    "src": {"app.ts": 3000, "types.d.ts": 1_500_000, "mod.mts": 2000, "main.py": 100},
    "rec": {"show.ts": TS_REC, "clip.mts": MTS_REC, "huge.ts": STREAM_ANY_MIN, "v.mp4": 5000},
}
WANT = {
    "src/app.ts": "code",
    "src/types.d.ts": "code",
    "src/mod.mts": "code",
    "src/main.py": "code",
    "rec/show.ts": "video",
    "rec/clip.mts": "video",
    "rec/huge.ts": "video",
    "rec/v.mp4": "video",
}


def test_colours_summary_and_filter_agree() -> None:
    store = build(SPEC)
    basis = Basis(store, DEFAULT_CLASSES)
    ids = np.array([find(store, p) for p in WANT], dtype=np.int64)
    got = [basis.class_ids[i] for i in basis.cls_of(ids).tolist()]
    assert dict(zip(WANT, got, strict=True)) == WANT

    root = next(iter(store.roots()))
    s = summarize(store, root, basis.cls_of, len(basis.class_ids), 3)
    files = dict(zip(basis.class_ids, s.class_files.tolist(), strict=True))
    assert files["video"] == 4 and files["code"] == 4 and sum(files.values()) == 8

    ctx = FilterContext(now=0, classes=DEFAULT_CLASSES)
    for cid in ("video", "code"):
        nf = parse(f":class:{cid}").normal
        assert nf is not None
        res = evaluate(store, nf, ctx)
        matched = {p for p, i in zip(WANT, ids.tolist(), strict=True) if res.match[i]}
        assert matched == {p for p, c in WANT.items() if c == cid}, cid
