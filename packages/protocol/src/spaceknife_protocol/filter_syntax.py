"""Reference parser, normaliser and formatter of the SpaceKnife filter language (SKF 1).

Specified in ``docs/filter-syntax.md``. Pure Python: the backend uses it to evaluate filters,
the Python frontends to colour the input locally; the Electron frontend gets the same result
through ``filter.parse``.

* :func:`parse` is total: every string yields a :class:`FilterParse` (conditions, tokens for
  syntax colouring, diagnostics with UTF-16 spans, the pending condition under the caret,
  completion context, and the normal form when there are no errors). It never raises.
* :func:`canonical` gives the unique spelling of an expression's normal form; it is idempotent
  and ``parse(canonical(x))`` has the same normal form as ``x``.
"""

from __future__ import annotations

import bisect
import math
from collections.abc import Iterable
from dataclasses import dataclass
from fractions import Fraction

from .classes import BUILTIN_CLASSES
from .filter import (
    ATTR_NAMES,
    TAG_COLORS,
    AgeCond,
    AgeField,
    AttrCond,
    AttrName,
    AttrSet,
    Bound,
    Category,
    ClassCond,
    Completion,
    ErrorCode,
    FilterCond,
    FilterDiagnostic,
    FilterNormal,
    FilterParse,
    FilterToken,
    FilterVocab,
    FolderCond,
    MaskCond,
    PendingCond,
    Range,
    SetCond,
    SizeCond,
    SizeField,
    TagColor,
    TagCond,
    TokenKind,
)

# --------------------------------------------------------------------------- vocabulary

KIB = 1024
SIZE_UNITS: dict[str, int] = {
    "b": 1,
    "kb": KIB,
    "kib": KIB,
    "mb": KIB**2,
    "mib": KIB**2,
    "gb": KIB**3,
    "gib": KIB**3,
    "tb": KIB**4,
    "tib": KIB**4,
    "pb": KIB**5,
    "pib": KIB**5,
}
MINUTE, HOUR, DAY = 60, 3600, 86400
TIME_UNITS: dict[str, int] = {
    **dict.fromkeys(("s", "sec", "secs", "second", "seconds"), 1),
    **dict.fromkeys(("m", "min", "mins", "minute", "minutes"), MINUTE),
    **dict.fromkeys(("h", "hr", "hrs", "hour", "hours"), HOUR),
    **dict.fromkeys(("d", "day", "days"), DAY),
    **dict.fromkeys(("w", "wk", "wks", "week", "weeks"), 7 * DAY),
    **dict.fromkeys(("mon", "month", "months"), 30 * DAY),
    **dict.fromkeys(("y", "yr", "yrs", "year", "years"), 365 * DAY),
}
# Canonical spellings, largest first (also the completion candidates).
SIZE_CANON: tuple[tuple[str, int], ...] = (
    ("pb", KIB**5),
    ("tb", KIB**4),
    ("gb", KIB**3),
    ("mb", KIB**2),
    ("kb", KIB),
    ("b", 1),
)
TIME_CANON: tuple[tuple[str, int], ...] = (
    ("y", 365 * DAY),
    ("month", 30 * DAY),
    ("w", 7 * DAY),
    ("d", DAY),
    ("h", HOUR),
    ("min", MINUTE),
    ("s", 1),
)
SIZE_PREFIXES: dict[str, SizeField] = {
    **dict.fromkeys(("d", "disk", "disksize", "clustersize"), "disk"),
    **dict.fromkeys(("s", "size", "filesize", "logicalsize"), "logical"),
}
AGE_PREFIXES: dict[str, AgeField] = {
    **dict.fromkeys(("m", "modify", "modified"), "modify"),
    **dict.fromkeys(("c", "create", "created"), "create"),
    **dict.fromkeys(("a", "access", "accessed"), "access"),
}
SIZE_FIELDS: tuple[SizeField, ...] = ("disk", "logical")
AGE_FIELDS: tuple[AgeField, ...] = ("modify", "create", "access")
SIZE_PREFIX_CANON: dict[SizeField, str] = {"disk": "", "logical": "s"}
AGE_PREFIX_CANON: dict[AgeField, str] = {"modify": "", "create": "c", "access": "a"}

TAG_WORDS: dict[str, TagColor] = {
    "red": "red",
    "r": "red",
    "yellow": "yellow",
    "y": "yellow",
    "green": "green",
    "g": "green",
    "blue": "blue",
    "b": "blue",
}
ATTR_WORDS: dict[str, AttrName] = {
    **dict.fromkeys(("a", "archive"), "archive"),
    **dict.fromkeys(("s", "system"), "system"),
    **dict.fromkeys(("r", "readonly"), "readonly"),
    **dict.fromkeys(("h", "hidden"), "hidden"),
    **dict.fromkeys(("c", "compressed"), "compressed"),
    **dict.fromkeys(("e", "encrypted"), "encrypted"),
    **dict.fromkeys(("o", "offline"), "offline"),
    **dict.fromkeys(("t", "temporary"), "temporary"),
    **dict.fromkeys(("ni", "notindexed"), "notindexed"),
    **dict.fromkeys(("sp", "sparse"), "sparse"),
    **dict.fromkeys(("ads", "alternatedatastream", "alternatedatastreams"), "ads"),
}
ATTR_LETTER: dict[AttrName, str] = {
    "archive": "a",
    "system": "s",
    "readonly": "r",
    "hidden": "h",
    "compressed": "c",
    "encrypted": "e",
    "offline": "o",
    "temporary": "t",
    "notindexed": "ni",
    "sparse": "sp",
    "ads": "ads",
}
VALUE_KEYWORDS = ("class", "tag", "attr", "attrs", "set")
TAG_KEYWORDS = ("red", "yellow", "green", "blue", "all")
KEYWORD_CANDIDATES = (":red", ":yellow", ":green", ":blue", ":all", ":tag:", ":attr:", ":class:")
OPERATORS = ("<", ">", "<=", ">=")
STARTERS = (">", "<", "s>", "s<", "m<", "m>", "c<", "a<", ":", "\\", "|")

MAX_BYTES = 1 << 62
MAX_SECONDS = 1000 * 365 * DAY
_OP_CHARS = "<>=!"
_GLOB_SPECIAL = "*?\\"


def default_vocab() -> FilterVocab:
    return FilterVocab(classes=list(BUILTIN_CLASSES))


@dataclass(frozen=True, slots=True)
class _ClassIndex:
    lookup: dict[str, str]  # casefolded spelling -> id
    order: dict[str, int]  # id -> display order
    spellings: tuple[tuple[str, str, str], ...]  # (casefolded spelling, id, display name)
    display: tuple[str, ...]


_INDEX_CACHE: dict[int, tuple[FilterVocab, _ClassIndex]] = {}


def _class_index(vocab: FilterVocab) -> _ClassIndex:
    hit = _INDEX_CACHE.get(id(vocab))
    if hit is not None and hit[0] is vocab:
        return hit[1]
    lookup: dict[str, str] = {}
    spellings: list[tuple[str, str, str]] = []
    for c in vocab.classes:
        for name in (c.name, *c.aliases, c.id):
            key = name.casefold()
            lookup.setdefault(key, c.id)
            spellings.append((key, c.id, c.name))
    index = _ClassIndex(
        lookup,
        {c.id: i for i, c in enumerate(vocab.classes)},
        tuple(spellings),
        tuple(c.name for c in vocab.classes),
    )
    if len(_INDEX_CACHE) > 32:
        _INDEX_CACHE.clear()
    _INDEX_CACHE[id(vocab)] = (vocab, index)
    return index


# --------------------------------------------------------------------------- offsets


class _U16:
    """Code-point index <-> UTF-16 offset (identity unless the text has astral characters)."""

    def __init__(self, text: str) -> None:
        self.cum: list[int] | None = None
        if any(ord(ch) > 0xFFFF for ch in text):
            cum = [0]
            for ch in text:
                cum.append(cum[-1] + (2 if ord(ch) > 0xFFFF else 1))
            self.cum = cum

    def to16(self, i: int) -> int:
        return i if self.cum is None else self.cum[i]

    def from16(self, u: int) -> int:
        if self.cum is None:
            return u
        return max(0, bisect.bisect_right(self.cum, u) - 1)


# --------------------------------------------------------------------------- globs


def normalize_glob(pattern: str) -> str:
    """Lower-case a glob and normalise wildcard runs (``*?*`` -> ``?*``); escapes are kept."""
    out: list[str] = []
    i, n = 0, len(pattern)
    while i < n:
        ch = pattern[i]
        if ch == "\\" and i + 1 < n:
            out.append(pattern[i : i + 2])
            i += 2
            continue
        if ch in "*?":
            j = i
            while j < n and pattern[j] in "*?":
                j += 1
            run = pattern[i:j]
            out.append("?" * run.count("?") + ("*" if "*" in run else ""))
            i = j
            continue
        out.append(ch.lower())
        i += 1
    return "".join(out)


def glob_parts(pattern: str) -> list[tuple[str, str]]:
    """Split a normalised glob into ``("lit", text)``, ``("any", "")`` and ``("one", "")`` parts."""
    parts: list[tuple[str, str]] = []
    lit: list[str] = []
    i, n = 0, len(pattern)
    while i < n:
        ch = pattern[i]
        if ch == "\\" and i + 1 < n:
            lit.append(pattern[i + 1])
            i += 2
            continue
        if ch in "*?":
            if lit:
                parts.append(("lit", "".join(lit)))
                lit = []
            parts.append(("any" if ch == "*" else "one", ""))
        else:
            lit.append(ch)
        i += 1
    if lit:
        parts.append(("lit", "".join(lit)))
    return parts


def _bare_ok(pattern: str) -> bool:
    if not pattern or "\\" in pattern or ";" in pattern:
        return False
    if pattern[0] in '|\\:"' or pattern[0].isspace() or pattern[-1].isspace():
        return False
    return not any(ch in pattern for ch in "|<>")


def format_mask(pattern: str) -> str:
    """Spelling of a normalised glob: bare when possible, otherwise quoted with ``\\`` escapes."""
    if _bare_ok(pattern):
        return pattern
    return '"' + pattern.replace('"', '\\"') + '"'


# --------------------------------------------------------------------------- bounds


def _best_unit(n: int, canon: tuple[tuple[str, int], ...]) -> tuple[int, str]:
    """(rank, spelling) of ``n`` in the largest unit that divides it exactly (rank: larger = coarser).

    Zero is the roundest number of all and is written in the smallest unit (``>0b``).
    """
    if n == 0:
        return len(canon) + 1, f"0{canon[-1][0]}"
    for rank, (unit, factor) in enumerate(canon):
        if n % factor == 0:
            return len(canon) - rank, f"{n // factor}{unit}"
    raise AssertionError("unit 1 divides everything")


def _split_bound(text: str) -> tuple[str, str]:
    i = 0
    while i < len(text) and text[i].isdigit():
        i += 1
    return text[:i], text[i:]


def format_lower(lo: int, canon: tuple[tuple[str, int], ...]) -> Bound:
    """Canonical spelling of ``value >= lo``: the rounder of ``>lo-1`` and ``>=lo`` (ties: ``>``)."""
    options: list[tuple[int, int, str, str]] = []  # (rank, strict, op, text)
    if lo - 1 >= 0:
        rank, text = _best_unit(lo - 1, canon)
        options.append((rank, 1, ">", text))
    if lo >= 0:
        rank, text = _best_unit(lo, canon)
        options.append((rank, 0, ">=", text))
    _, _, op, text = max(options)
    number, unit = _split_bound(text)
    return Bound(op=op, number=number, unit=unit)  # type: ignore[arg-type]


def format_upper(hi: int, canon: tuple[tuple[str, int], ...]) -> Bound:
    """Canonical spelling of ``value <= hi``: the rounder of ``<hi+1`` and ``<=hi`` (ties: ``<``)."""
    options: list[tuple[int, int, str, str]] = []
    if hi + 1 >= 0:
        rank, text = _best_unit(hi + 1, canon)
        options.append((rank, 1, "<", text))
    if hi >= 0:
        rank, text = _best_unit(hi, canon)
        options.append((rank, 0, "<=", text))
    _, _, op, text = max(options)
    number, unit = _split_bound(text)
    return Bound(op=op, number=number, unit=unit)  # type: ignore[arg-type]


def _bound_text(prefix: str, b: Bound) -> str:
    return f"{prefix}{b.op}{b.number}{b.unit}"


def _range_texts(
    prefix: str, lo: int | None, hi: int | None, canon: tuple[tuple[str, int], ...]
) -> list[str]:
    out = []
    if lo is not None:
        out.append(_bound_text(prefix, format_lower(lo, canon)))
    if hi is not None:
        out.append(_bound_text(prefix, format_upper(hi, canon)))
    return out


def _tags_text(colors: Iterable[TagColor]) -> str:
    chosen = [c for c in TAG_COLORS if c in set(colors)]
    if len(chosen) == len(TAG_COLORS):
        return ":all"
    if len(chosen) == 1:
        return f":{chosen[0]}"
    if not chosen:
        return ":tag:all-" + "-".join(TAG_COLORS)
    return ":tag:" + "+".join(chosen)


def _attrs_text(required: Iterable[AttrName], forbidden: Iterable[AttrName]) -> str:
    req, forb = set(required), set(forbidden)
    pos = "+".join(ATTR_LETTER[a] for a in ATTR_NAMES if a in req)
    neg = "".join("-" + ATTR_LETTER[a] for a in ATTR_NAMES if a in forb)
    return ":attr:" + pos + neg


def _sorted_attrs(names: Iterable[AttrName]) -> list[AttrName]:
    s = set(names)
    return [a for a in ATTR_NAMES if a in s]


# --------------------------------------------------------------------------- parser


class _Fail(Exception):
    """Internal: abandon the current condition (the diagnostic or pending state is recorded)."""


class _Parser:
    def __init__(self, text: str, cursor: int | None, vocab: FilterVocab) -> None:
        self.text = text
        self.u16 = _U16(text)
        self.cursor = None if cursor is None else min(self.u16.from16(cursor), len(text))
        self.vocab = vocab
        self.classes = _class_index(vocab)
        self.sets: dict[str, str] = {}  # spelling (casefolded) -> set id
        self.set_names: list[str] = []
        for s in vocab.sets:
            self.set_names.append(s.name)
            for spelling in (s.id, s.name, *s.aliases):
                self.sets.setdefault(spelling.casefold(), s.id)
        self.keywords = KEYWORD_CANDIDATES + ((":set:",) if vocab.sets else ())
        self.tokens: list[FilterToken] = []
        self.conds: list[FilterCond] = []
        self.diags: list[FilterDiagnostic] = []
        self.pending: PendingCond | None = None
        self.completion: Completion | None = None
        # per-condition state
        self.a = self.b = 0
        self.us = self.ue = 0
        self.at_cursor = False

    # ------------------------------------------------------------- helpers

    def _ws(self, i: int, end: int) -> int:
        t = self.text
        while i < end and t[i].isspace():
            i += 1
        return i

    def _trim(self, s: int, e: int) -> tuple[int, int]:
        s = self._ws(s, e)
        while e > s and self.text[e - 1].isspace():
            e -= 1
        return s, e

    def _span(self, s: int, e: int) -> tuple[int, int]:
        return self.u16.to16(s), self.u16.to16(e)

    def _tok(self, kind: TokenKind, s: int, e: int) -> None:
        if e > s:
            us, ue = self._span(s, e)
            self.tokens.append(FilterToken(kind=kind, start=us, end=ue))

    def _cursor_in(self, s: int, e: int) -> bool:
        return self.cursor is not None and s <= self.cursor <= e

    def _complete(self, context: str, s: int, e: int, candidates: Iterable[str]) -> None:
        if not self._cursor_in(s, e):
            return
        us, ue = self._span(s, e)
        self.completion = Completion(
            context=context,  # type: ignore[arg-type]
            start=us,
            end=ue,
            typed=self.text[s:e],
            candidates=list(candidates),
        )

    def _fail(
        self,
        code: ErrorCode,
        s: int,
        e: int,
        *,
        expected: Iterable[str] = (),
        pending_ok: bool = False,
        category: Category | None = None,
    ) -> _Fail:
        if pending_ok and self.at_cursor:
            us, ue = self._span(self.a, self.b)
            self.pending = PendingCond(category=category, start=us, end=ue)
        else:
            us, ue = self._span(s, e)
            ca, cb = self._span(self.a, self.b)
            self.diags.append(
                FilterDiagnostic(
                    code=code,
                    start=us,
                    end=ue,
                    cond_start=ca,
                    cond_end=cb,
                    text=self.text[s:e],
                    expected=list(expected),
                )
            )
        return _Fail()

    # ------------------------------------------------------------- segments

    def segments(self) -> list[tuple[int, int]]:
        """Split at ``;`` outside quoted masks. A quote opens a mask only at the start of a body."""
        t, n = self.text, len(self.text)
        out: list[tuple[int, int]] = []
        i = 0
        while True:
            j = self._ws(i, n)
            if j < n and t[j] == "|":
                j = self._ws(j + 1, n)
            if j < n and t[j] == "\\":
                j = self._ws(j + 1, n)
            if j < n and t[j] == '"':
                j += 1
                while j < n and t[j] != '"':
                    j += 2 if t[j] == "\\" else 1
                j = min(j + 1, n)
            semi = t.find(";", j)
            end = n if semi < 0 else semi
            out.append((i, end))
            if semi < 0:
                return out
            self._tok("separator", semi, semi + 1)
            i = semi + 1

    def run(self) -> None:
        for s, e in self.segments():
            self.a, self.b = self._trim(s, e)
            self.us, self.ue = self._span(self.a, self.b)
            self.at_cursor = self._cursor_in(self.a, self.b) if self.a < self.b else False
            if self.a == self.b:
                if self.cursor is not None and s <= self.cursor <= e:
                    self._complete("prefix", self.cursor, self.cursor, STARTERS)
                continue
            try:
                cond = self._condition()
            except _Fail:
                continue
            self.conds.append(cond)

    # ------------------------------------------------------------- conditions

    def _condition(self) -> FilterCond:
        t, a, b = self.text, self.a, self.b
        p, neg = a, False
        if t[p] == "|":
            self._tok("negate", p, p + 1)
            neg = True
            p = self._ws(p + 1, b)
            if p >= b:
                raise self._fail("incomplete", a, b, pending_ok=True)
        ch = t[p]
        if ch == "\\":
            return self._folder(p, neg)
        if ch == ":":
            return self._keyword(p, neg)
        if ch == '"' or not any(c in t[p:b] for c in "<>"):
            pattern = self._mask(p, "name")
            self._tok("mask", p, b)
            return MaskCond(
                pattern=pattern,
                text=("|" if neg else "") + format_mask(pattern),
                start=self.us,
                end=self.ue,
                negated=neg,
            )
        return self._compare(p, neg)

    def _folder(self, p: int, neg: bool) -> FolderCond:
        q = self._ws(p + 1, self.b)
        self._tok("folder", p, self.b)
        if q >= self.b:
            raise self._fail("incomplete", p, self.b, pending_ok=True, category="folder")
        pattern = self._mask(q, "folder")
        text = ("|" if neg else "") + "\\" + format_mask(pattern)
        return FolderCond(pattern=pattern, text=text, start=self.us, end=self.ue, negated=neg)

    def _mask(self, p: int, category: Category) -> str:
        t, b = self.text, self.b
        if t[p] != '"':
            for i in range(p, b):
                if t[i] == "\\":
                    raise self._fail("backslash_in_mask", i, i + 1)
                if t[i] in "|<>":
                    raise self._fail("bad_char", i, i + 1)
            return normalize_glob(t[p:b])
        out: list[str] = []
        i = p + 1
        while i < b and t[i] != '"':
            if t[i] == "\\":
                if i + 1 >= b:
                    raise self._fail("bad_escape", i, i + 1, pending_ok=True, category=category)
                nxt = t[i + 1]
                out.append("\\" + nxt if nxt in _GLOB_SPECIAL else nxt)
                i += 2
                continue
            out.append(t[i])
            i += 1
        if i >= b:
            raise self._fail("unterminated_quote", p, b, pending_ok=True, category=category)
        if self._ws(i + 1, b) < b:
            raise self._fail("text_after_quote", i + 1, b)
        if not out:
            raise self._fail("incomplete", p, b, pending_ok=True, category=category)
        return normalize_glob("".join(out))

    def _keyword(self, p: int, neg: bool) -> FilterCond:
        t, b = self.text, self.b
        j = p + 1
        while j < b and t[j].isascii() and t[j].isalpha():
            j += 1
        word = t[p + 1 : j].lower()
        if j < b and t[j] == ":":
            kw_end = j + 1
            if word not in VALUE_KEYWORDS:
                raise self._fail("unknown_keyword", p, kw_end, expected=self.keywords)
            self._tok("keyword", p, kw_end)
            vs = self._ws(kw_end, b)
            category: Category = (
                "class" if word == "class" else "tag" if word == "tag" else "set" if word == "set" else "attr"
            )
            if vs >= b:
                self._complete(category, kw_end, kw_end, self._value_candidates(category, ""))
                raise self._fail("incomplete", p, b, pending_ok=True, category=category)
            self._tok("value", vs, b)
            if word == "class":
                return self._class(vs, neg)
            if word == "set":
                return self._set(vs, neg)
            if word == "tag":
                return self._tag_list(vs, neg)
            return self._attr_list(vs, neg)
        rest = self._ws(j, b)
        self._complete("keyword", p, j, [k for k in self.keywords if k.startswith(t[p:j].lower())])
        if word in TAG_KEYWORDS and rest >= b:
            self._tok("keyword", p, j)
            colors = list(TAG_COLORS) if word == "all" else [TAG_WORDS[word]]
            return TagCond(
                colors=colors,
                text=("|" if neg else "") + _tags_text(colors),
                start=self.us,
                end=self.ue,
                negated=neg,
            )
        self._tok("keyword", p, b)
        prefix_of_keyword = rest >= b and any(k.startswith(t[p:j].lower()) for k in self.keywords)
        raise self._fail(
            "incomplete" if not word and rest >= b else "unknown_keyword",
            p,
            b,
            expected=self.keywords,
            pending_ok=prefix_of_keyword,
        )

    def _value_candidates(self, category: Category, typed: str) -> list[str]:
        key = typed.casefold()
        if category == "class":
            seen: list[str] = []
            for spelling, _cid, display in self.classes.spellings:
                if spelling.startswith(key) and display not in seen:
                    seen.append(display)
            return seen
        if category == "tag":
            return [w for w in (*TAG_COLORS, "all") if w.startswith(key)]
        if category == "set":
            return [n for n in self.set_names if n.casefold().startswith(key)]
        return [a for a in ATTR_NAMES if a.startswith(key) or ATTR_LETTER[a].startswith(key)]

    def _class(self, vs: int, neg: bool) -> ClassCond:
        name = self.text[vs : self.b]
        self._complete("class", vs, self.b, self._value_candidates("class", name))
        cid = self.classes.lookup.get(name.casefold())
        if cid is None:
            partial = any(sp.startswith(name.casefold()) for sp, _, _ in self.classes.spellings)
            raise self._fail(
                "unknown_class",
                vs,
                self.b,
                expected=self.classes.display,
                pending_ok=partial,
                category="class",
            )
        return ClassCond(
            class_id=cid, text=("|" if neg else "") + f":class:{cid}", start=self.us, end=self.ue, negated=neg
        )

    def _set(self, vs: int, neg: bool) -> SetCond:
        name = self.text[vs : self.b].strip()
        self._complete("set", vs, self.b, self._value_candidates("set", name))
        sid = self.sets.get(name.casefold())
        if sid is None:
            partial = any(sp.startswith(name.casefold()) for sp in self.sets)
            raise self._fail(
                "unknown_set", vs, self.b, expected=self.set_names, pending_ok=partial, category="set"
            )
        return SetCond(
            set_id=sid, text=("|" if neg else "") + f":set:{sid}", start=self.us, end=self.ue, negated=neg
        )

    def _word(self, i: int, stop: str) -> int:
        t, b = self.text, self.b
        while i < b and t[i] not in stop and not t[i].isspace():
            i += 1
        return i

    def _tag_list(self, vs: int, neg: bool) -> TagCond:
        t, b = self.text, self.b
        colors: set[TagColor] = set()
        i, first = vs, True
        while True:
            op = "+"
            if not first:
                op = t[i]
                if op not in "+-":
                    raise self._fail("bad_list", i, i + 1)
                i = self._ws(i + 1, b)
                if i >= b:
                    raise self._fail("bad_list", i - 1, b, pending_ok=True, category="tag")
            j = self._word(i, "+-")
            word = t[i:j].lower()
            self._complete("tag", i, j, self._value_candidates("tag", word))
            if not word:
                raise self._fail("bad_list", i, min(i + 1, b))
            if word == "all":
                items: set[TagColor] = set(TAG_COLORS)
            elif word in TAG_WORDS:
                items = {TAG_WORDS[word]}
            else:
                partial = any(w.startswith(word) for w in (*TAG_WORDS, "all"))
                raise self._fail(
                    "unknown_tag", i, j, expected=(*TAG_COLORS, "all"), pending_ok=partial, category="tag"
                )
            colors = colors | items if op == "+" else colors - items
            first = False
            i = self._ws(j, b)
            if i >= b:
                break
        ordered = [c for c in TAG_COLORS if c in colors]
        return TagCond(
            colors=ordered,
            text=("|" if neg else "") + _tags_text(ordered),
            start=self.us,
            end=self.ue,
            negated=neg,
        )

    def _attr_list(self, vs: int, neg: bool) -> AttrCond:
        t, b = self.text, self.b
        required: set[AttrName] = set()
        forbidden: set[AttrName] = set()
        i, expect_item, negate = vs, True, False
        while True:
            i = self._ws(i, b)
            if i >= b:
                if expect_item:
                    raise self._fail("bad_list", max(vs, i - 1), b, pending_ok=True, category="attr")
                break
            ch = t[i]
            if expect_item:
                if ch == "-":
                    if negate:
                        raise self._fail("bad_list", i, i + 1)
                    negate = True
                    i += 1
                    continue
                j = i
                while j < b and t[j].isascii() and t[j].isalpha():
                    j += 1
                word = t[i:j].lower()
                self._complete("attr", i, j, self._value_candidates("attr", word))
                if not word:
                    raise self._fail("bad_list", i, i + 1)
                attr = ATTR_WORDS.get(word)
                if attr is None:
                    partial = any(w.startswith(word) for w in ATTR_WORDS)
                    raise self._fail(
                        "unknown_attr", i, j, expected=ATTR_NAMES, pending_ok=partial, category="attr"
                    )
                (forbidden if negate else required).add(attr)
                expect_item, negate, i = False, False, j
            elif ch in "+,":
                expect_item, i = True, i + 1
            elif ch == "-":
                expect_item, negate, i = True, True, i + 1
            else:
                raise self._fail("bad_list", i, i + 1)
        req, forb = _sorted_attrs(required), _sorted_attrs(forbidden)
        return AttrCond(
            required=req,
            forbidden=forb,
            text=("|" if neg else "") + _attrs_text(req, forb),
            start=self.us,
            end=self.ue,
            negated=neg,
        )

    def _compare(self, p: int, neg: bool) -> FilterCond:
        t, b = self.text, self.b
        self._tok("compare", p, b)
        j = p
        while j < b and t[j].isascii() and t[j].isalpha():
            j += 1
        prefix = t[p:j].lower()
        k = self._ws(j, b)
        o = k
        while o < b and t[o] in _OP_CHARS:
            o += 1
        if o == k:
            first_op = min(i for i in (t.find("<", p, b), t.find(">", p, b)) if i >= 0)
            raise self._fail("unknown_prefix", p, first_op, expected=(*SIZE_PREFIXES, *AGE_PREFIXES))
        op = t[k:o]
        if op not in OPERATORS:
            raise self._fail("bad_operator", k, o, expected=OPERATORS)
        if prefix and prefix not in SIZE_PREFIXES and prefix not in AGE_PREFIXES:
            raise self._fail("unknown_prefix", p, j, expected=(*SIZE_PREFIXES, *AGE_PREFIXES))
        pkind: Category | None = (
            "size" if prefix in SIZE_PREFIXES else "age" if prefix in AGE_PREFIXES else None
        )
        n0 = self._ws(o, b)
        q = n0
        while q < b and t[q].isdigit() and t[q].isascii():
            q += 1
        if q < b and t[q] == "." and q > n0:
            r = q + 1
            while r < b and t[r].isdigit() and t[r].isascii():
                r += 1
            if r == q + 1:
                raise self._fail("bad_number", n0, r, pending_ok=True, category=pkind)
            q = r
        if q == n0:
            if n0 >= b:
                raise self._fail("incomplete", p, b, pending_ok=True, category=pkind)
            raise self._fail("bad_number", n0, self._word(n0, ""))
        number = t[n0:q]
        u0 = self._ws(q, b)
        if u0 < b and not (t[u0].isascii() and t[u0].isalpha()):
            raise self._fail("bad_number", n0, b)
        u1 = u0
        while u1 < b and t[u1].isascii() and t[u1].isalpha():
            u1 += 1
        unit = t[u0:u1].lower()
        units = self._units(pkind)
        self._complete("unit", u0, u1, [u for u in units if u.startswith(unit)])
        if self._ws(u1, b) < b:
            raise self._fail("unknown_unit", u0, b, expected=units)
        if not unit:
            raise self._fail("missing_unit", q, q, expected=units, pending_ok=True, category=pkind)
        ukind: Category | None = "size" if unit in SIZE_UNITS else "age" if unit in TIME_UNITS else None
        if ukind is None:
            # a partly typed unit: pending, and its category is known once only one kind still fits
            fits: set[Category] = set()
            for u in (*SIZE_UNITS, *TIME_UNITS):
                if u.startswith(unit) and self._kind_ok(pkind, u):
                    fits.add("size" if u in SIZE_UNITS else "age")
            category: Category | None = pkind or (next(iter(fits)) if len(fits) == 1 else None)
            raise self._fail("unknown_unit", u0, u1, expected=units, pending_ok=bool(fits), category=category)
        if pkind is not None and ukind != pkind:
            raise self._fail("unit_mismatch", u0, u1, expected=units)
        factor = SIZE_UNITS[unit] if ukind == "size" else TIME_UNITS[unit]
        x = Fraction(number) * factor
        if x > (MAX_BYTES if ukind == "size" else MAX_SECONDS):
            raise self._fail("out_of_range", n0, u1)
        lo, hi = _bounds(op, x, neg)
        if ukind == "size":
            sfield: SizeField = SIZE_PREFIXES.get(prefix, "disk")
            texts = _range_texts(SIZE_PREFIX_CANON[sfield], lo, hi, SIZE_CANON)
            return SizeCond(
                field=sfield, lo=lo, hi=hi, text=";".join(texts), start=self.us, end=self.ue, negated=neg
            )
        afield: AgeField = AGE_PREFIXES.get(prefix, "modify")
        texts = _range_texts(AGE_PREFIX_CANON[afield], lo, hi, TIME_CANON)
        return AgeCond(
            field=afield, lo=lo, hi=hi, text=";".join(texts), start=self.us, end=self.ue, negated=neg
        )

    @staticmethod
    def _kind_ok(pkind: Category | None, unit: str) -> bool:
        if pkind == "size":
            return unit in SIZE_UNITS
        if pkind == "age":
            return unit in TIME_UNITS
        return True

    @staticmethod
    def _units(pkind: Category | None) -> list[str]:
        sizes = [u for u, _ in reversed(SIZE_CANON)]
        times = [u for u, _ in reversed(TIME_CANON)]
        if pkind == "size":
            return sizes
        if pkind == "age":
            return times
        return sizes + times

    # ------------------------------------------------------------- result

    def warnings(self) -> None:
        bad_attrs = set(self.vocab.unsupported_attrs)
        bad_ages = set(self.vocab.unsupported_ages)
        for c in self.conds:
            if isinstance(c, MaskCond) and not c.negated and c.pattern.startswith("!:"):
                # "!:class:x" is a name mask (a name may start with "!"), not a negation: say how
                # the negation is written; the mask still applies as written
                self.diags.append(
                    FilterDiagnostic(
                        code="bang_negation",
                        severity="warning",
                        start=c.start,
                        end=c.start + 1,
                        cond_start=c.start,
                        cond_end=c.end,
                        text="!",
                        expected=["|" + c.pattern[1:]],
                    )
                )
                continue
            hit: list[str] = []
            if isinstance(c, AttrCond):
                hit = [a for a in (*c.required, *c.forbidden) if a in bad_attrs]
            elif isinstance(c, AgeCond) and c.field in bad_ages:
                hit = [c.field]
            if hit:
                self.diags.append(
                    FilterDiagnostic(
                        code="unsupported",
                        severity="warning",
                        start=c.start,
                        end=c.end,
                        cond_start=c.start,
                        cond_end=c.end,
                        text=c.text,
                        expected=hit,
                    )
                )


def _bounds(op: str, x: Fraction, neg: bool) -> tuple[int | None, int | None]:
    """Inclusive integer bounds of ``value op x`` (exact for integer values), negated if asked."""
    lo: int | None = None
    hi: int | None = None
    if op == ">":
        lo = math.floor(x) + 1
    elif op == ">=":
        lo = math.ceil(x)
    elif op == "<":
        hi = math.ceil(x) - 1
    else:
        hi = math.floor(x)
    if neg:
        lo, hi = (None, lo - 1) if lo is not None else ((hi + 1) if hi is not None else None, None)
    return lo, hi


# --------------------------------------------------------------------------- normal form


def normalize(conds: Iterable[FilterCond], vocab: FilterVocab | None = None) -> FilterNormal:
    """Merge conditions into the normal form (see the grammar document, section 6)."""
    order = _class_index(vocab or _DEFAULT_VOCAB).order
    masks: set[str] = set()
    classes: set[str] = set()
    folders: set[str] = set()
    tags: set[TagColor] | None = None
    sizes: dict[SizeField, list[int | None]] = {}
    ages: dict[AgeField, list[int | None]] = {}
    attr_req: set[AttrName] = set()
    attr_forb: set[AttrName] = set()
    has_attr = False
    not_masks: set[str] = set()
    not_classes: set[str] = set()
    not_folders: set[str] = set()
    not_tags: set[TagColor] = set()
    not_attrs: set[tuple[tuple[AttrName, ...], tuple[AttrName, ...]]] = set()
    sets: set[str] = set()
    not_sets: set[str] = set()

    def merge(ranges: dict[str, list[int | None]], key: str, lo: int | None, hi: int | None) -> None:
        cur = ranges.setdefault(key, [None, None])
        if lo is not None:
            cur[0] = lo if cur[0] is None else max(cur[0], lo)
        if hi is not None:
            cur[1] = hi if cur[1] is None else min(cur[1], hi)

    for c in conds:
        if isinstance(c, MaskCond):
            (not_masks if c.negated else masks).add(c.pattern)
        elif isinstance(c, FolderCond):
            (not_folders if c.negated else folders).add(c.pattern)
        elif isinstance(c, ClassCond):
            (not_classes if c.negated else classes).add(c.class_id)
        elif isinstance(c, SetCond):
            (not_sets if c.negated else sets).add(c.set_id)
        elif isinstance(c, TagCond):
            if c.negated:
                not_tags |= set(c.colors)
            else:
                tags = (tags or set()) | set(c.colors)
        elif isinstance(c, AttrCond):
            if not c.negated:
                has_attr = True
                attr_req |= set(c.required)
                attr_forb |= set(c.forbidden)
            elif not set(c.required) & set(c.forbidden):  # |(never true) is always true: drop
                not_attrs.add((tuple(c.required), tuple(c.forbidden)))
        elif isinstance(c, SizeCond):
            merge(sizes, c.field, c.lo, c.hi)  # type: ignore[arg-type]
        elif isinstance(c, AgeCond):
            merge(ages, c.field, c.lo, c.hi)  # type: ignore[arg-type]

    pieces: list[str] = []
    out_masks = sorted(masks)
    out_classes = sorted(classes, key=lambda k: (order.get(k, len(order)), k))
    out_folders = sorted(folders)
    pieces += [format_mask(m) for m in out_masks]
    pieces += [f":class:{k}" for k in out_classes]
    pieces += [f":set:{k}" for k in sorted(sets)]
    pieces += ["\\" + format_mask(f) for f in out_folders]
    out_tags = None if tags is None else [c for c in TAG_COLORS if c in tags]
    if out_tags is not None:
        pieces.append(_tags_text(out_tags))
    out_sizes: list[Range] = []
    for sfield in SIZE_FIELDS:
        if sfield not in sizes:
            continue
        lo, hi = sizes[sfield]
        if lo is not None and lo <= 0:
            lo = None  # every size is >= 0
        if lo is None and hi is None:
            continue
        lower = format_lower(lo, SIZE_CANON) if lo is not None else None
        upper = format_upper(hi, SIZE_CANON) if hi is not None else None
        out_sizes.append(Range(field=sfield, lo=lo, hi=hi, lower=lower, upper=upper))
        pieces += _range_texts(SIZE_PREFIX_CANON[sfield], lo, hi, SIZE_CANON)
    out_ages: list[Range] = []
    for afield in AGE_FIELDS:
        if afield not in ages:
            continue
        lo, hi = ages[afield]
        lower = format_lower(lo, TIME_CANON) if lo is not None else None
        upper = format_upper(hi, TIME_CANON) if hi is not None else None
        out_ages.append(Range(field=afield, lo=lo, hi=hi, lower=lower, upper=upper))
        pieces += _range_texts(AGE_PREFIX_CANON[afield], lo, hi, TIME_CANON)
    out_attrs = None
    if has_attr:
        out_attrs = AttrSet(required=_sorted_attrs(attr_req), forbidden=_sorted_attrs(attr_forb))
        pieces.append(_attrs_text(attr_req, attr_forb))
    neg_attr_texts = sorted((_attrs_text(r, f), r, f) for r, f in not_attrs)
    pieces += ["|" + format_mask(m) for m in sorted(not_masks)]
    pieces += [f"|:class:{k}" for k in sorted(not_classes, key=lambda k: (order.get(k, len(order)), k))]
    pieces += ["|\\" + format_mask(f) for f in sorted(not_folders)]
    out_not_tags = [c for c in TAG_COLORS if c in not_tags]
    if out_not_tags:
        pieces.append("|" + _tags_text(out_not_tags))
    pieces += ["|" + text for text, _, _ in neg_attr_texts]
    pieces += [f"|:set:{k}" for k in sorted(not_sets)]
    return FilterNormal(
        masks=out_masks,
        classes=out_classes,
        folders=out_folders,
        tags=out_tags,
        sizes=out_sizes,
        ages=out_ages,
        attrs=out_attrs,
        not_masks=sorted(not_masks),
        not_classes=sorted(not_classes, key=lambda k: (order.get(k, len(order)), k)),
        not_folders=sorted(not_folders),
        not_tags=out_not_tags,
        not_attrs=[AttrSet(required=list(r), forbidden=list(f)) for _, r, f in neg_attr_texts],
        sets=sorted(sets),
        not_sets=sorted(not_sets),
        canonical=";".join(pieces),
    )


_DEFAULT_VOCAB = default_vocab()


def parse(text: str, cursor: int | None = None, vocab: FilterVocab | None = None) -> FilterParse:
    """Parse a filter expression. ``cursor`` (UTF-16 offset) enables pending/completion handling."""
    vocab = vocab or _DEFAULT_VOCAB
    p = _Parser(text, cursor, vocab)
    p.run()
    p.warnings()
    p.tokens.sort(key=lambda tk: tk.start)
    has_error = any(d.severity == "error" for d in p.diags)
    return FilterParse(
        text=text,
        tokens=p.tokens,
        conditions=p.conds,
        pending=p.pending,
        diagnostics=sorted(p.diags, key=lambda d: (d.start, d.end)),
        normal=None if has_error else normalize(p.conds, vocab),
        completion=p.completion,
    )


def canonical(text: str, vocab: FilterVocab | None = None) -> str | None:
    """Canonical spelling of ``text``'s normal form, or None if it has errors."""
    result = parse(text, None, vocab)
    return result.normal.canonical if result.normal is not None else None
