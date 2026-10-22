"""Filter language models: tokens, conditions, diagnostics and the normal form.

The language itself is specified in ``docs/filter-syntax.md``; the reference parser is
:mod:`spaceknife_protocol.filter_syntax` (pure Python, shared by the backend and the Python
frontends). All character offsets are **UTF-16 code units**, the unit the three frontends' text
widgets use, so spans can be applied to the input box directly.
"""

from __future__ import annotations

from typing import Annotated, Literal

from pydantic import Field

from .base import Params, Payload

TagColor = Literal["red", "yellow", "green", "blue"]
TAG_COLORS: tuple[TagColor, ...] = ("red", "yellow", "green", "blue")

AttrName = Literal[
    "archive",
    "system",
    "readonly",
    "hidden",
    "compressed",
    "encrypted",
    "offline",
    "temporary",
    "notindexed",
    "sparse",
    "ads",
]
ATTR_NAMES: tuple[AttrName, ...] = (
    "archive",
    "system",
    "readonly",
    "hidden",
    "compressed",
    "encrypted",
    "offline",
    "temporary",
    "notindexed",
    "sparse",
    "ads",
)

SizeField = Literal["disk", "logical"]
AgeField = Literal["modify", "create", "access"]
Category = Literal["name", "class", "folder", "tag", "size", "age", "attr", "set"]

TokenKind = Literal["negate", "separator", "mask", "folder", "compare", "keyword", "value"]

ErrorCode = Literal[
    "incomplete",  # a condition is missing its required part (":class:", ">", "|", "\\")
    "unterminated_quote",
    "text_after_quote",
    "bad_escape",  # "\\" at the very end of a quoted mask
    "backslash_in_mask",
    "unknown_keyword",
    "bad_char",  # a character a bare mask cannot contain ('|', '<', '>'): quote the mask
    "unknown_class",
    "unknown_set",  # a node set (":set:") no enabled plugin offers
    "unknown_tag",
    "unknown_attr",
    "bad_list",  # malformed ":tag:" / ":attr:" list ("h++r", trailing "+")
    "unknown_prefix",
    "bad_operator",  # "=", "=>", "<>", ">>" …
    "bad_number",
    "missing_unit",
    "unknown_unit",
    "unit_mismatch",  # a size prefix with a time unit or vice versa
    "out_of_range",
    "unsupported",  # warning: valid, but this platform does not record the attribute/time
]


class FilterToken(Payload):
    kind: TokenKind
    start: int
    end: int


class FilterDiagnostic(Payload):
    """An error (the expression is not applied) or, with ``severity="warning"``, a hint."""

    code: ErrorCode
    severity: Literal["error", "warning"] = "error"
    start: int = Field(description="UTF-16 offset of the offending part")
    end: int
    cond_start: int = Field(description="UTF-16 span of the whole condition")
    cond_end: int
    text: str = Field(description="The offending part as typed")
    expected: list[str] = Field(default_factory=list, description="Valid spellings at this position")


class Bound(Payload):
    """One side of a range, in its canonical spelling (``>`` ``500`` ``mb``)."""

    op: Literal["<", "<=", ">", ">="]
    number: str
    unit: str


class _Cond(Payload):
    start: int
    end: int
    negated: bool = False
    text: str = Field(description="Canonical spelling of this single condition")


class MaskCond(_Cond):
    kind: Literal["name"] = "name"
    pattern: str = Field(description="Normalised glob: lower case, '*' '?' wildcards, '\\' escapes")


class FolderCond(_Cond):
    kind: Literal["folder"] = "folder"
    pattern: str


class ClassCond(_Cond):
    kind: Literal["class"] = "class"
    class_id: str


class SetCond(_Cond):
    """``:set:<name>``: in a node set a plugin computes (plugins.v1)."""

    kind: Literal["set"] = "set"
    set_id: str


class TagCond(_Cond):
    kind: Literal["tag"] = "tag"
    colors: list[TagColor]


class AttrCond(_Cond):
    kind: Literal["attr"] = "attr"
    required: list[AttrName]
    forbidden: list[AttrName]


class SizeCond(_Cond):
    kind: Literal["size"] = "size"
    field: SizeField
    lo: int | None = Field(description="Inclusive lower bound in bytes (after applying negation)")
    hi: int | None = Field(description="Inclusive upper bound in bytes (after applying negation)")


class AgeCond(_Cond):
    kind: Literal["age"] = "age"
    field: AgeField
    lo: int | None = Field(description="Inclusive lower bound of the age in seconds")
    hi: int | None = Field(description="Inclusive upper bound of the age in seconds")


FilterCond = Annotated[
    MaskCond | FolderCond | ClassCond | SetCond | TagCond | AttrCond | SizeCond | AgeCond,
    Field(discriminator="kind"),
]


class PendingCond(Payload):
    """The unfinished condition under the cursor: not an error, not evaluated (dashed chip)."""

    category: Category | None
    start: int
    end: int


class Range(Payload):
    field: str
    lo: int | None
    hi: int | None
    lower: Bound | None
    upper: Bound | None


class AttrSet(Payload):
    required: list[AttrName]
    forbidden: list[AttrName]


class FilterNormal(Payload):
    """The normal form: what the expression means, independent of how it was written.

    Two expressions with the same normal form select exactly the same files; ``canonical`` is the
    unique spelling of this normal form.
    """

    masks: list[str] = Field(default_factory=list)
    classes: list[str] = Field(default_factory=list)
    folders: list[str] = Field(default_factory=list)
    tags: list[TagColor] | None = Field(default=None, description="None = no tag condition")
    sizes: list[Range] = Field(default_factory=list)
    ages: list[Range] = Field(default_factory=list)
    attrs: AttrSet | None = None
    not_masks: list[str] = Field(default_factory=list)
    not_classes: list[str] = Field(default_factory=list)
    not_folders: list[str] = Field(default_factory=list)
    not_tags: list[TagColor] = Field(default_factory=list)
    not_attrs: list[AttrSet] = Field(default_factory=list)
    sets: list[str] = Field(default_factory=list, description="Node sets, any of them (full ids)")
    not_sets: list[str] = Field(default_factory=list)
    canonical: str = ""

    @property
    def empty(self) -> bool:
        return not self.canonical


class Completion(Payload):
    """What can be inserted at the cursor; ``start``/``end`` is the span a candidate replaces."""

    context: Literal["keyword", "class", "tag", "attr", "unit", "prefix", "set"]
    start: int
    end: int
    typed: str
    candidates: list[str]


class FilterParse(Payload):
    text: str
    tokens: list[FilterToken]
    conditions: list[FilterCond]
    pending: PendingCond | None = None
    diagnostics: list[FilterDiagnostic] = Field(default_factory=list)
    normal: FilterNormal | None = Field(default=None, description="None while there are errors")
    completion: Completion | None = None

    @property
    def ok(self) -> bool:
        return not any(d.severity == "error" for d in self.diagnostics)


class FilterClassInfo(Payload):
    id: str
    name: str
    aliases: list[str]
    extensions: list[str]
    color: str = Field(
        default="", description="#RRGGBB chosen by the user; empty: the theme's colour for the id"
    )


class FilterSetInfo(Payload):
    """A node set of an enabled plugin (``:set:<name>``)."""

    id: str = Field(description="Full id; also accepted after :set:")
    name: str
    aliases: list[str] = Field(default_factory=list)


class FilterVocab(Payload):
    """Everything the parser needs besides the text; served by ``filter.vocab``."""

    classes: list[FilterClassInfo]
    sets: list[FilterSetInfo] = Field(default_factory=list, description="plugins.v1: the node sets")
    unsupported_attrs: list[AttrName] = Field(default_factory=list)
    unsupported_ages: list[AgeField] = Field(default_factory=list)


class FilterParseParams(Params):
    text: str = Field(max_length=8192)
    cursor: int | None = Field(default=None, ge=0, description="UTF-16 offset of the caret")
