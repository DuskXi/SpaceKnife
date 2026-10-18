"""SKF 1 grammar: examples, equivalences, diagnostics, editing state and the five properties.

The properties checked here are the ones docs/filter-syntax.md promises:

* totality — every string parses without raising, spans are well-formed;
* unambiguity — parsing is a function (same text → same result; one classification per condition);
* canonical uniqueness — equivalent spellings share one canonical text;
* idempotence — ``canonical(canonical(x)) == canonical(x)``;
* completeness — every normal form is reachable: ``parse(canonical(x))`` has ``x``'s normal form.
"""

from __future__ import annotations

import pytest
from hypothesis import given, settings
from hypothesis import strategies as st

from spaceknife_protocol.filter import AgeCond, FilterParse, MaskCond, SizeCond
from spaceknife_protocol.filter_syntax import canonical, normalize_glob, parse

BS = "\\"
MIB = 1 << 20


def canon(text: str) -> str:
    result = canonical(text)
    assert result is not None, [d.code for d in parse(text).diagnostics]
    return result


def codes(text: str, cursor: int | None = None) -> list[str]:
    return [d.code for d in parse(text, cursor).diagnostics]


# --------------------------------------------------------------------------- examples


@pytest.mark.parametrize(
    ("text", "expected"),
    [
        ("", ""),
        (" ; ;", ""),
        ("*.txt", "*.txt"),
        ("*.TXT; *.jpg", "*.jpg;*.txt"),
        ("README", "readme"),
        ("|*.txt;|*.jpg", "|*.jpg;|*.txt"),
        (">1mb;<10mb", ">1mb;<10mb"),
        (">10mb;<1mb", ">10mb;<1mb"),  # empty range: kept, matches nothing
        (">1mb;>2mb", ">2mb"),
        ("|>1mb", "<=1mb"),
        ("|>=1mb", "<1mb"),
        (">=1mb", ">=1mb"),
        (">0b", ">0b"),
        (">=0b", ""),  # every size is >= 0
        (">1.5gb", ">1536mb"),
        ("s>1mb;d<2mb", "<2mb;s>1mb"),
        ("m<5d;m>1.5d", ">36h;<5d"),
        ("<2w", "<2w"),
        ("<14d", "<2w"),
        ("c>1month;a<1y", "c>1month;a<1y"),
        (">1m", ">1min"),
        (">60s", ">1min"),
        ("|<5d", ">=5d"),
        (":attr:a+h-s", ":attr:a+h-s"),
        (":attr:-a", ":attr:-a"),
        (":attr:h;:attr:r", ":attr:r+h"),
        ("|:attr:a", "|:attr:a"),
        (":red;:yellow", ":tag:red+yellow"),
        (":tag:all-red", ":tag:yellow+green+blue"),
        (":tag:r+y+g+b", ":all"),
        ("|:red;|:blue", "|:tag:red+blue"),
        (":class:Audio/Music", ":class:audio"),
        (":class:Video/Movies;:class:Pictures/Images", ":class:video;:class:image"),
        (BS + "temp", BS + "temp"),
        ("|" + BS + "*windows*", "|" + BS + "*windows*"),
        ('"a;b.txt"', '"a;b.txt"'),
        ('"lit' + BS + '*star"', '"lit' + BS + '*star"'),
        ('"  padded  "', '"  padded  "'),
        ('*"*', '*"*'),
        ("**.txt", "*.txt"),
        ("*?*x", "?*x"),
        ("| *.txt", "|*.txt"),
        (BS + " temp", BS + "temp"),
        ("*.txt;\\temp;>1kb;:attr:h;:red;|*.log", "*.txt;" + BS + "temp;:red;>1kb;:attr:h;|*.log"),
    ],
)
def test_canonical_examples(text: str, expected: str) -> None:
    assert canon(text) == expected


@pytest.mark.parametrize(
    "group",
    [
        [":attr:a+h-s", ":attr:a,h,-s", ":ATTR:hidden+archive-system", ":attrs:a;:attr:h;:attr:-s"],
        [":red;:yellow", ":tag:r+y", ":tag:red+yellow", ":tag:yellow;:tag:red"],
        [
            ">1mb;<10mb",
            "<10mb;>1mb",
            "> 1 mb ; < 10 MB",
            "d>1mb;disksize<10mb",
            "clustersize>1024kb;<10240kb",
        ],
        ["|>1mb", "<=1mb", "<1048577b", "|>1024kb"],
        ["m<5d", "<5days", "modify<120h", "MODIFIED<7200min", "<5 d"],
        ["s>1mb", "size>1mb", "filesize>1mb", "logicalsize>1024kb"],
        ["*.TXT", "*.txt", "**.txt", "*.Txt;*.tXt"],
        [":class:Audio/Music", ":class:音频", ":CLASS:music", ":class: audio "],
        [">1.5gb", ">1536mb", ">1572864kb"],
        ["c<1w", "create<7d", "created<168h"],
        ["a>2y", "access>730d", "accessed>2years"],
        [">1min", ">1m", ">60s", ">1minute", ">1minutes"],
    ],
)
def test_equivalent_spellings_share_one_canonical(group: list[str]) -> None:
    assert len({canon(t) for t in group}) == 1, {t: canon(t) for t in group}


def test_conditions_carry_exact_integer_bounds() -> None:
    (c,) = parse(">1mb").conditions
    assert isinstance(c, SizeCond)
    assert (c.field, c.lo, c.hi) == ("disk", MIB + 1, None)
    (c,) = parse("s<=1.5kb").conditions
    assert isinstance(c, SizeCond)
    assert (c.field, c.lo, c.hi) == ("logical", None, 1536)
    (c,) = parse(">0.3b").conditions  # x = 0.3 → value > 0.3 ⇔ value >= 1
    assert isinstance(c, SizeCond)
    assert c.lo == 1
    (c,) = parse("|c<5d").conditions
    assert isinstance(c, AgeCond)
    assert (c.field, c.lo, c.hi) == ("create", 5 * 86400, None)


def test_text_without_comparison_operator_is_a_name_mask() -> None:
    # "=", "!" and digits are legal in file names; only "<" and ">" make a comparison.
    assert canon("=1mb") == "=1mb"
    assert isinstance(parse("=1mb").conditions[0], MaskCond)


def test_masks_are_lower_cased_globs() -> None:
    (c,) = parse("ABC?*.Mp4").conditions
    assert isinstance(c, MaskCond)
    assert c.pattern == "abc?*.mp4"
    assert normalize_glob("A**?b") == "a?*b"


# --------------------------------------------------------------------------- diagnostics


@pytest.mark.parametrize(
    ("text", "code", "span"),
    [
        (">500xb", "unknown_unit", "xb"),
        (">1k", "unknown_unit", "k"),
        ("d<16k", "unknown_unit", "k"),
        (">500", "missing_unit", ""),
        ("<5", "missing_unit", ""),
        (">", "incomplete", ">"),
        ("s>", "incomplete", "s>"),
        (">abc", "bad_number", "abc"),
        (">1.mb", "bad_number", "1."),
        (">.5mb", "bad_number", ".5mb"),
        (">1,5mb", "bad_number", "1,5mb"),
        (">-1mb", "bad_number", "-1mb"),
        ("=>1mb", "bad_operator", "=>"),
        ("<>1mb", "bad_operator", "<>"),
        ("x>1mb", "unknown_prefix", "x"),
        ("s<5d", "unit_mismatch", "d"),
        ("m>1mb", "unit_mismatch", "mb"),
        ("d<5d", "unit_mismatch", "d"),
        (">1mb x", "unknown_unit", "mb x"),
        (">99999999pb", "out_of_range", "99999999pb"),
        (":foo", "unknown_keyword", ":foo"),
        (":foo:bar", "unknown_keyword", ":foo:"),
        (":", "incomplete", ":"),
        (":class:", "incomplete", ":class:"),
        (":class:nosuch", "unknown_class", "nosuch"),
        (":tag:purple", "unknown_tag", "purple"),
        (":tag:+red", "bad_list", "+"),
        (":tag:red+", "bad_list", "+"),
        (":attr:z", "unknown_attr", "z"),
        (":attr:h++r", "bad_list", "+"),
        (":attr:h--r", "bad_list", "-"),
        (":attr:h r", "bad_list", "r"),
        (":attr:h,", "bad_list", ","),
        ("|", "incomplete", "|"),
        (BS, "incomplete", BS),
        (BS + "temp" + BS + "x", "backslash_in_mask", BS),
        ("a" + BS + "b", "backslash_in_mask", BS),
        ("a|b", "bad_char", "|"),
        (BS + "a>b", "bad_char", ">"),
        ('"abc', "unterminated_quote", '"abc'),
        ('"abc"x', "text_after_quote", "x"),
        ('""', "incomplete", '""'),
    ],
)
def test_errors_point_at_the_offending_text(text: str, code: str, span: str) -> None:
    result = parse(text)
    assert result.normal is None
    (d,) = result.diagnostics
    assert d.code == code
    assert text[d.start : d.end] == span
    assert (d.cond_start, d.cond_end) == (0, len(text.strip()))


def test_every_error_lists_what_would_be_valid() -> None:
    assert parse(">500xb").diagnostics[0].expected[:6] == ["b", "kb", "mb", "gb", "tb", "pb"]
    assert parse("m>1x").diagnostics[0].expected == ["s", "min", "h", "d", "w", "month", "y"]
    assert "视频" in parse(":class:nosuch").diagnostics[0].expected
    assert "hidden" in parse(":attr:z").diagnostics[0].expected


def test_errors_in_one_condition_do_not_hide_the_others() -> None:
    result = parse(">1xb;*.txt;:foo")
    assert [d.code for d in result.diagnostics] == ["unknown_unit", "unknown_keyword"]
    assert len(result.conditions) == 1


def test_spans_are_utf16_code_units() -> None:
    text = "😀*.txt;>1xb"  # the emoji is two UTF-16 code units
    result = parse(text)
    (d,) = result.diagnostics
    assert (d.start, d.end) == (10, 12)
    mask = next(t for t in result.tokens if t.kind == "mask")
    assert (mask.start, mask.end) == (0, 7)
    (c,) = result.conditions
    assert (c.start, c.end) == (0, 7)


# --------------------------------------------------------------------------- editing state


def test_unfinished_condition_at_the_cursor_is_pending_not_an_error() -> None:
    text = "*.mp4;*.mkv;*.mov;>500mb;:class:"
    result = parse(text, cursor=len(text))
    assert result.diagnostics == []
    assert result.pending is not None
    assert (result.pending.category, result.pending.start, result.pending.end) == ("class", 25, 32)
    assert result.normal is not None
    assert result.normal.canonical == "*.mkv;*.mov;*.mp4;>500mb"
    assert result.completion is not None
    assert result.completion.context == "class"
    assert result.completion.candidates[:2] == ["视频", "图片"]
    # the same text with the caret elsewhere: the unfinished condition is an error
    assert codes(text, cursor=0) == ["incomplete"]


@pytest.mark.parametrize(
    ("text", "category"),
    [
        (":class:Vi", "class"),
        (":tag:re", "tag"),
        (":attr:hid", "attr"),
        (":attr:h+", "attr"),
        (":cla", None),
        (">5", None),  # size or age: unknown until the unit is typed
        ("s>5", "size"),
        ("m<5", "age"),
        (">5m", None),  # complete already: 5 minutes
        (">5mo", "age"),
        (">1.", None),
        ('"abc', "name"),
        ("|", None),
    ],
)
def test_prefix_of_something_valid_is_pending(text: str, category: str | None) -> None:
    result = parse(text, cursor=len(text))
    if text == ">5m":
        assert result.pending is None and result.diagnostics == []
        return
    assert result.diagnostics == [], result.diagnostics
    assert result.pending is not None
    assert result.pending.category == category


def test_completion_contexts() -> None:
    def ctx(text: str) -> tuple[str, list[str]] | None:
        c = parse(text, cursor=len(text)).completion
        return None if c is None else (c.context, c.candidates)

    assert ctx(":") == (
        "keyword",
        [":red", ":yellow", ":green", ":blue", ":all", ":tag:", ":attr:", ":class:"],
    )
    assert ctx(":t") == ("keyword", [":tag:"])
    assert ctx(":tag:g") == ("tag", ["green"])
    assert ctx(":attr:h+r") == ("attr", ["readonly"])
    assert ctx(">5") == ("unit", ["b", "kb", "mb", "gb", "tb", "pb", "s", "min", "h", "d", "w", "month", "y"])
    assert ctx("s>5") == ("unit", ["b", "kb", "mb", "gb", "tb", "pb"])
    assert ctx("m>5mo") == ("unit", ["month"])
    assert ctx("*.txt;") is not None and ctx("*.txt;")[0] == "prefix"  # type: ignore[index]


def test_tokens_cover_each_part_once() -> None:
    result = parse("|*.mp4;>1gb;:class:video;\\temp")
    kinds = [(t.kind, result.text[t.start : t.end]) for t in result.tokens]
    assert kinds == [
        ("negate", "|"),
        ("mask", "*.mp4"),
        ("separator", ";"),
        ("compare", ">1gb"),
        ("separator", ";"),
        ("keyword", ":class:"),
        ("value", "video"),
        ("separator", ";"),
        ("folder", "\\temp"),
    ]


def test_platform_warnings_do_not_block() -> None:
    from spaceknife_protocol.filter_syntax import default_vocab

    vocab = default_vocab().model_copy(
        update={"unsupported_attrs": ["system"], "unsupported_ages": ["create"]}
    )
    result = parse(":attr:s;c<5d", vocab=vocab)
    assert result.normal is not None
    assert [(d.code, d.severity, d.expected) for d in result.diagnostics] == [
        ("unsupported", "warning", ["system"]),
        ("unsupported", "warning", ["create"]),
    ]


# --------------------------------------------------------------------------- SpaceSniffer help examples


@pytest.mark.parametrize(
    "text",
    [
        "*.jpg;>100kb;>2weeks",
        ":red;:yellow",
        ":tag:red+yellow",
        ":tag:r+y",
        "|*.exe;|*.dll",
        "|*.exe;|*.dll;\\*temp*;|\\*windows*",
        ":attr:a+h-s",
        ":attr:a,h,-s",
        "|:attr:a",
        ":attr:-a",
        "s>1mb",
        "c>1month",
        "create>1month",
        "m<1day",
        "m<10s",
        ":class:Audio/Music",
        "|:class:Audio/Music",
        "\\temp",
        "|\\progr*",
    ],
)
def test_spacesniffer_help_examples_parse(text: str) -> None:
    assert parse(text).normal is not None


def test_spacesniffer_help_example_d16k_is_rejected_like_spacesniffer_does() -> None:
    # SpaceSniffer's help shows "d<16k", but SpaceSniffer 2.0.5.18 itself rejects it.
    assert codes("d<16k") == ["unknown_unit"]


# --------------------------------------------------------------------------- properties

ALPHABET = st.sampled_from([*list('*?;|\\:"<>=!.,+- \tabcdghkmnrstwyABCMS0123459'), "é", "视", "😀"])
TEXT = st.text(ALPHABET, max_size=40)


def _check_well_formed(text: str, result: FilterParse) -> None:
    n16 = len(text.encode("utf-16-le")) // 2
    prev = 0
    for tok in result.tokens:
        assert 0 <= tok.start < tok.end <= n16
        assert tok.start >= prev
        prev = tok.end
    for d in result.diagnostics:
        assert 0 <= d.cond_start <= d.start <= d.end <= d.cond_end <= n16
    if result.pending is not None:
        assert 0 <= result.pending.start <= result.pending.end <= n16


@settings(max_examples=600, deadline=None)
@given(TEXT, st.one_of(st.none(), st.integers(0, 45)))
def test_parse_is_total(text: str, cursor: int | None) -> None:
    result = parse(text, cursor)
    _check_well_formed(text, result)
    assert (result.normal is None) == any(d.severity == "error" for d in result.diagnostics)
    assert parse(text, cursor) == result  # a function of its input


@settings(max_examples=600, deadline=None)
@given(TEXT)
def test_canonical_is_idempotent_and_complete(text: str) -> None:
    result = parse(text)
    if result.normal is None:
        return
    c = result.normal.canonical
    again = parse(c)
    assert again.diagnostics == [] or all(d.severity == "warning" for d in again.diagnostics)
    assert again.normal == result.normal
    assert canonical(c) == c


WORDS = st.sampled_from(
    [
        "*.txt",
        "*.JPG",
        "a?c",
        "x*y*",
        '"a;b"',
        '"q\\"x"',
        "\\temp",
        "\\*Win*",
        ">1mb",
        ">=2kb",
        "<10gb",
        "s>512b",
        "d<=1tb",
        "m<5d",
        "c>1month",
        "a<36h",
        ">1.5y",
        ":attr:h",
        ":attr:a+r-s",
        ":attr:-ni,sp",
        ":red",
        ":all",
        ":tag:r+g",
        ":tag:all-b",
        ":class:video",
        ":class:Audio/Music",
        ":class:其他",
    ]
)


@settings(max_examples=400, deadline=None)
@given(st.lists(st.tuples(st.booleans(), WORDS), max_size=8), st.randoms())
def test_order_and_repetition_do_not_matter(conds: list[tuple[bool, str]], rnd: object) -> None:
    parts = [("|" if neg else "") + w for neg, w in conds]
    text = ";".join(parts)
    shuffled = parts + parts[: len(parts) // 2]
    rnd.shuffle(shuffled)  # type: ignore[attr-defined]
    assert canon(text) == canon(";".join(shuffled))
