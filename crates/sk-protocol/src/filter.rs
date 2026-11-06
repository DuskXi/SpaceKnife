//! Filter language models: tokens, conditions, diagnostics and the normal form.
//!
//! The language itself is specified in `docs/filter-syntax.md`; the reference parser is
//! [`crate::filter_syntax`]. All character offsets are **UTF-16 code units**, the unit the frontends'
//! text widgets use, so spans can be applied to the input box directly.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::macros::{const_str, str_enum};
use crate::validate::{self as v, Validate, ValidationResult};

str_enum! {
    pub enum TagColor {
        Red = "red",
        Yellow = "yellow",
        Green = "green",
        Blue = "blue",
    }
}

/// The four tag colours in their canonical order.
pub const TAG_COLORS: [TagColor; 4] = [
    TagColor::Red,
    TagColor::Yellow,
    TagColor::Green,
    TagColor::Blue,
];

str_enum! {
    pub enum AttrName {
        Archive = "archive",
        System = "system",
        Readonly = "readonly",
        Hidden = "hidden",
        Compressed = "compressed",
        Encrypted = "encrypted",
        Offline = "offline",
        Temporary = "temporary",
        Notindexed = "notindexed",
        Sparse = "sparse",
        Ads = "ads",
    }
}

/// Every attribute in its canonical order.
pub const ATTR_NAMES: [AttrName; 11] = [
    AttrName::Archive,
    AttrName::System,
    AttrName::Readonly,
    AttrName::Hidden,
    AttrName::Compressed,
    AttrName::Encrypted,
    AttrName::Offline,
    AttrName::Temporary,
    AttrName::Notindexed,
    AttrName::Sparse,
    AttrName::Ads,
];

str_enum! {
    pub enum SizeField {
        Disk = "disk",
        Logical = "logical",
    }
}

str_enum! {
    pub enum AgeField {
        Modify = "modify",
        Create = "create",
        Access = "access",
    }
}

str_enum! {
    pub enum Category {
        Name = "name",
        Class = "class",
        Folder = "folder",
        Tag = "tag",
        Size = "size",
        Age = "age",
        Attr = "attr",
    }
}

str_enum! {
    pub enum TokenKind {
        Negate = "negate",
        Separator = "separator",
        Mask = "mask",
        Folder = "folder",
        Compare = "compare",
        Keyword = "keyword",
        Value = "value",
    }
}

str_enum! {
    /// Diagnostic codes of the filter language.
    pub enum FilterErrorCode {
        /// A condition is missing its required part (":class:", ">", "|", "\\")
        Incomplete = "incomplete",
        UnterminatedQuote = "unterminated_quote",
        TextAfterQuote = "text_after_quote",
        /// "\\" at the very end of a quoted mask
        BadEscape = "bad_escape",
        BackslashInMask = "backslash_in_mask",
        UnknownKeyword = "unknown_keyword",
        /// A character a bare mask cannot contain ('|', '<', '>'): quote the mask
        BadChar = "bad_char",
        UnknownClass = "unknown_class",
        UnknownTag = "unknown_tag",
        UnknownAttr = "unknown_attr",
        /// Malformed ":tag:" / ":attr:" list ("h++r", trailing "+")
        BadList = "bad_list",
        UnknownPrefix = "unknown_prefix",
        /// "=", "=>", "<>", ">>" …
        BadOperator = "bad_operator",
        BadNumber = "bad_number",
        MissingUnit = "missing_unit",
        UnknownUnit = "unknown_unit",
        /// A size prefix with a time unit or vice versa
        UnitMismatch = "unit_mismatch",
        OutOfRange = "out_of_range",
        /// Warning: valid, but this platform does not record the attribute/time
        Unsupported = "unsupported",
    }
}

str_enum! {
    pub enum Severity {
        Error = "error",
        Warning = "warning",
    }
}

impl Default for Severity {
    fn default() -> Self {
        Severity::Error
    }
}

str_enum! {
    pub enum BoundOp {
        Lt = "<",
        Le = "<=",
        Gt = ">",
        Ge = ">=",
    }
}

str_enum! {
    pub enum CompletionContext {
        Keyword = "keyword",
        Class = "class",
        Tag = "tag",
        Attr = "attr",
        Unit = "unit",
        Prefix = "prefix",
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FilterToken {
    pub kind: TokenKind,
    pub start: u32,
    pub end: u32,
}

/// An error (the expression is not applied) or, with `severity="warning"`, a hint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FilterDiagnostic {
    pub code: FilterErrorCode,
    #[serde(default)]
    pub severity: Severity,
    /// UTF-16 offset of the offending part
    pub start: u32,
    pub end: u32,
    /// UTF-16 span of the whole condition
    pub cond_start: u32,
    pub cond_end: u32,
    /// The offending part as typed
    pub text: String,
    /// Valid spellings at this position
    #[serde(default)]
    pub expected: Vec<String>,
}

/// One side of a range, in its canonical spelling (``>`` ``500`` ``mb``).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Bound {
    pub op: BoundOp,
    pub number: String,
    pub unit: String,
}

const_str!(pub struct NameKind = "name";);
const_str!(pub struct FolderKind = "folder";);
const_str!(pub struct ClassKind = "class";);
const_str!(pub struct TagKind = "tag";);
const_str!(pub struct AttrKind = "attr";);
const_str!(pub struct SizeKind = "size";);
const_str!(pub struct AgeKind = "age";);

macro_rules! cond {
    ($(#[$m:meta])* pub struct $name:ident { kind: $kind:ty, $($(#[$fm:meta])* pub $f:ident : $t:ty,)* }) => {
        $(#[$m])*
        #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
        pub struct $name {
            pub start: u32,
            pub end: u32,
            #[serde(default)]
            pub negated: bool,
            /// Canonical spelling of this single condition
            pub text: String,
            // The union's discriminator: required on the wire, so the variants never blur.
            pub kind: $kind,
            $($(#[$fm])* pub $f: $t,)*
        }
    };
}

cond! {
    pub struct MaskCond {
        kind: NameKind,
        /// Normalised glob: lower case, '*' '?' wildcards, '\' escapes
        pub pattern: String,
    }
}

cond! {
    pub struct FolderCond {
        kind: FolderKind,
        pub pattern: String,
    }
}

cond! {
    pub struct ClassCond {
        kind: ClassKind,
        pub class_id: String,
    }
}

cond! {
    pub struct TagCond {
        kind: TagKind,
        pub colors: Vec<TagColor>,
    }
}

cond! {
    pub struct AttrCond {
        kind: AttrKind,
        pub required: Vec<AttrName>,
        pub forbidden: Vec<AttrName>,
    }
}

cond! {
    pub struct SizeCond {
        kind: SizeKind,
        pub field: SizeField,
        /// Inclusive lower bound in bytes (after applying negation)
        pub lo: Option<i64>,
        /// Inclusive upper bound in bytes (after applying negation)
        pub hi: Option<i64>,
    }
}

cond! {
    pub struct AgeCond {
        kind: AgeKind,
        pub field: AgeField,
        /// Inclusive lower bound of the age in seconds
        pub lo: Option<i64>,
        /// Inclusive upper bound of the age in seconds
        pub hi: Option<i64>,
    }
}

/// One condition of a filter, tagged by `kind`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
#[schemars(inline)]
pub enum FilterCond {
    Name(MaskCond),
    Folder(FolderCond),
    Class(ClassCond),
    Tag(TagCond),
    Attr(AttrCond),
    Size(SizeCond),
    Age(AgeCond),
}

impl FilterCond {
    pub fn start(&self) -> u32 {
        self.common().0
    }
    pub fn end(&self) -> u32 {
        self.common().1
    }
    pub fn negated(&self) -> bool {
        self.common().2
    }
    pub fn text(&self) -> &str {
        self.common().3
    }
    pub fn category(&self) -> Category {
        match self {
            FilterCond::Name(_) => Category::Name,
            FilterCond::Folder(_) => Category::Folder,
            FilterCond::Class(_) => Category::Class,
            FilterCond::Tag(_) => Category::Tag,
            FilterCond::Attr(_) => Category::Attr,
            FilterCond::Size(_) => Category::Size,
            FilterCond::Age(_) => Category::Age,
        }
    }
    fn common(&self) -> (u32, u32, bool, &str) {
        match self {
            FilterCond::Name(c) => (c.start, c.end, c.negated, &c.text),
            FilterCond::Folder(c) => (c.start, c.end, c.negated, &c.text),
            FilterCond::Class(c) => (c.start, c.end, c.negated, &c.text),
            FilterCond::Tag(c) => (c.start, c.end, c.negated, &c.text),
            FilterCond::Attr(c) => (c.start, c.end, c.negated, &c.text),
            FilterCond::Size(c) => (c.start, c.end, c.negated, &c.text),
            FilterCond::Age(c) => (c.start, c.end, c.negated, &c.text),
        }
    }
}

/// The unfinished condition under the cursor: not an error, not evaluated (dashed chip).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PendingCond {
    pub category: Option<Category>,
    pub start: u32,
    pub end: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Range {
    pub field: String,
    pub lo: Option<i64>,
    pub hi: Option<i64>,
    pub lower: Option<Bound>,
    pub upper: Option<Bound>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AttrSet {
    pub required: Vec<AttrName>,
    pub forbidden: Vec<AttrName>,
}

/// The normal form: what the expression means, independent of how it was written.
///
/// Two expressions with the same normal form select exactly the same files; ``canonical`` is the
/// unique spelling of this normal form.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FilterNormal {
    #[serde(default)]
    pub masks: Vec<String>,
    #[serde(default)]
    pub classes: Vec<String>,
    #[serde(default)]
    pub folders: Vec<String>,
    /// None = no tag condition
    #[serde(default)]
    pub tags: Option<Vec<TagColor>>,
    #[serde(default)]
    pub sizes: Vec<Range>,
    #[serde(default)]
    pub ages: Vec<Range>,
    #[serde(default)]
    pub attrs: Option<AttrSet>,
    #[serde(default)]
    pub not_masks: Vec<String>,
    #[serde(default)]
    pub not_classes: Vec<String>,
    #[serde(default)]
    pub not_folders: Vec<String>,
    #[serde(default)]
    pub not_tags: Vec<TagColor>,
    #[serde(default)]
    pub not_attrs: Vec<AttrSet>,
    #[serde(default)]
    pub canonical: String,
}

impl FilterNormal {
    /// The expression selects everything (no conditions).
    pub fn is_empty(&self) -> bool {
        self.canonical.is_empty()
    }
}

/// What can be inserted at the cursor; ``start``/``end`` is the span a candidate replaces.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Completion {
    pub context: CompletionContext,
    pub start: u32,
    pub end: u32,
    pub typed: String,
    pub candidates: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FilterParse {
    pub text: String,
    pub tokens: Vec<FilterToken>,
    pub conditions: Vec<FilterCond>,
    #[serde(default)]
    pub pending: Option<PendingCond>,
    #[serde(default)]
    pub diagnostics: Vec<FilterDiagnostic>,
    /// None while there are errors
    #[serde(default)]
    pub normal: Option<FilterNormal>,
    #[serde(default)]
    pub completion: Option<Completion>,
}

impl FilterParse {
    /// No error diagnostics (warnings allowed).
    pub fn ok(&self) -> bool {
        !self
            .diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FilterClassInfo {
    pub id: String,
    pub name: String,
    pub aliases: Vec<String>,
    pub extensions: Vec<String>,
    /// Colour (#RRGGBB) chosen by the user for the dark theme; empty: the dark theme's colour for a built-in id, the light choice for a class of the user's
    #[serde(default)]
    pub color: String,
    /// Colour (#RRGGBB) chosen by the user for the light theme; empty: the light theme's colour for a built-in id, the dark choice for a class of the user's
    #[serde(default)]
    pub color_light: String,
}

/// Everything the parser needs besides the text; served by ``filter.vocab``.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FilterVocab {
    pub classes: Vec<FilterClassInfo>,
    #[serde(default)]
    pub unsupported_attrs: Vec<AttrName>,
    #[serde(default)]
    pub unsupported_ages: Vec<AgeField>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FilterParseParams {
    pub text: String,
    /// UTF-16 offset of the caret
    #[serde(default)]
    pub cursor: Option<u32>,
}

impl Validate for FilterParseParams {
    fn validate(&self) -> ValidationResult {
        v::str_len("text", &self.text, 0, 8192)
    }
}
