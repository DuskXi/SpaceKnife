//! Reference parser, normaliser and formatter of the SpaceKnife filter language (SKF 1).
//!
//! Specified in `docs/filter-syntax.md`; a line-by-line port of the Python reference
//! (`spaceknife_protocol.filter_syntax`).
//!
//! * [`parse`] is total: every string yields a [`FilterParse`] (conditions, tokens for syntax
//!   colouring, diagnostics with UTF-16 spans, the pending condition under the caret, completion
//!   context, and the normal form when there are no errors). It never panics.
//! * [`canonical`] gives the unique spelling of an expression's normal form; it is idempotent and
//!   `parse(canonical(x))` has the same normal form as `x`.
//!
//! A backend that parses often keeps a [`CompiledVocab`] (the class lookup tables built once).

use std::collections::{BTreeSet, HashMap};
use std::sync::LazyLock;

use crate::classes::builtin_classes;
use crate::filter::*;

// --------------------------------------------------------------------------- vocabulary

pub const KIB: i64 = 1024;
const DAY: i64 = 86400;
const HOUR: i64 = 3600;
const MINUTE: i64 = 60;

/// Size units and their factors (the spellings the parser accepts).
pub const SIZE_UNITS: &[(&str, i64)] = &[
    ("b", 1),
    ("kb", KIB),
    ("kib", KIB),
    ("mb", KIB * KIB),
    ("mib", KIB * KIB),
    ("gb", KIB * KIB * KIB),
    ("gib", KIB * KIB * KIB),
    ("tb", KIB * KIB * KIB * KIB),
    ("tib", KIB * KIB * KIB * KIB),
    ("pb", KIB * KIB * KIB * KIB * KIB),
    ("pib", KIB * KIB * KIB * KIB * KIB),
];

/// Time units and their factors in seconds.
pub const TIME_UNITS: &[(&str, i64)] = &[
    ("s", 1),
    ("sec", 1),
    ("secs", 1),
    ("second", 1),
    ("seconds", 1),
    ("m", MINUTE),
    ("min", MINUTE),
    ("mins", MINUTE),
    ("minute", MINUTE),
    ("minutes", MINUTE),
    ("h", HOUR),
    ("hr", HOUR),
    ("hrs", HOUR),
    ("hour", HOUR),
    ("hours", HOUR),
    ("d", DAY),
    ("day", DAY),
    ("days", DAY),
    ("w", 7 * DAY),
    ("wk", 7 * DAY),
    ("wks", 7 * DAY),
    ("week", 7 * DAY),
    ("weeks", 7 * DAY),
    ("mon", 30 * DAY),
    ("month", 30 * DAY),
    ("months", 30 * DAY),
    ("y", 365 * DAY),
    ("yr", 365 * DAY),
    ("yrs", 365 * DAY),
    ("year", 365 * DAY),
    ("years", 365 * DAY),
];

/// Canonical size spellings, largest first (also the completion candidates).
pub const SIZE_CANON: &[(&str, i64)] = &[
    ("pb", KIB * KIB * KIB * KIB * KIB),
    ("tb", KIB * KIB * KIB * KIB),
    ("gb", KIB * KIB * KIB),
    ("mb", KIB * KIB),
    ("kb", KIB),
    ("b", 1),
];

/// Canonical time spellings, largest first.
pub const TIME_CANON: &[(&str, i64)] = &[
    ("y", 365 * DAY),
    ("month", 30 * DAY),
    ("w", 7 * DAY),
    ("d", DAY),
    ("h", HOUR),
    ("min", MINUTE),
    ("s", 1),
];

pub const SIZE_PREFIXES: &[(&str, SizeField)] = &[
    ("d", SizeField::Disk),
    ("disk", SizeField::Disk),
    ("disksize", SizeField::Disk),
    ("clustersize", SizeField::Disk),
    ("s", SizeField::Logical),
    ("size", SizeField::Logical),
    ("filesize", SizeField::Logical),
    ("logicalsize", SizeField::Logical),
];

pub const AGE_PREFIXES: &[(&str, AgeField)] = &[
    ("m", AgeField::Modify),
    ("modify", AgeField::Modify),
    ("modified", AgeField::Modify),
    ("c", AgeField::Create),
    ("create", AgeField::Create),
    ("created", AgeField::Create),
    ("a", AgeField::Access),
    ("access", AgeField::Access),
    ("accessed", AgeField::Access),
];

pub const SIZE_FIELDS: [SizeField; 2] = [SizeField::Disk, SizeField::Logical];
pub const AGE_FIELDS: [AgeField; 3] = [AgeField::Modify, AgeField::Create, AgeField::Access];

const fn size_prefix_canon(f: SizeField) -> &'static str {
    match f {
        SizeField::Disk => "",
        SizeField::Logical => "s",
    }
}

const fn age_prefix_canon(f: AgeField) -> &'static str {
    match f {
        AgeField::Modify => "",
        AgeField::Create => "c",
        AgeField::Access => "a",
    }
}

pub const TAG_WORDS: &[(&str, TagColor)] = &[
    ("red", TagColor::Red),
    ("r", TagColor::Red),
    ("yellow", TagColor::Yellow),
    ("y", TagColor::Yellow),
    ("green", TagColor::Green),
    ("g", TagColor::Green),
    ("blue", TagColor::Blue),
    ("b", TagColor::Blue),
];

pub const ATTR_WORDS: &[(&str, AttrName)] = &[
    ("a", AttrName::Archive),
    ("archive", AttrName::Archive),
    ("s", AttrName::System),
    ("system", AttrName::System),
    ("r", AttrName::Readonly),
    ("readonly", AttrName::Readonly),
    ("h", AttrName::Hidden),
    ("hidden", AttrName::Hidden),
    ("c", AttrName::Compressed),
    ("compressed", AttrName::Compressed),
    ("e", AttrName::Encrypted),
    ("encrypted", AttrName::Encrypted),
    ("o", AttrName::Offline),
    ("offline", AttrName::Offline),
    ("t", AttrName::Temporary),
    ("temporary", AttrName::Temporary),
    ("ni", AttrName::Notindexed),
    ("notindexed", AttrName::Notindexed),
    ("sp", AttrName::Sparse),
    ("sparse", AttrName::Sparse),
    ("ads", AttrName::Ads),
    ("alternatedatastream", AttrName::Ads),
    ("alternatedatastreams", AttrName::Ads),
];

/// The short spelling of an attribute (`h` for hidden).
pub const fn attr_letter(a: AttrName) -> &'static str {
    match a {
        AttrName::Archive => "a",
        AttrName::System => "s",
        AttrName::Readonly => "r",
        AttrName::Hidden => "h",
        AttrName::Compressed => "c",
        AttrName::Encrypted => "e",
        AttrName::Offline => "o",
        AttrName::Temporary => "t",
        AttrName::Notindexed => "ni",
        AttrName::Sparse => "sp",
        AttrName::Ads => "ads",
    }
}

pub const VALUE_KEYWORDS: &[&str] = &["class", "tag", "attr", "attrs"];
pub const TAG_KEYWORDS: &[&str] = &["red", "yellow", "green", "blue", "all"];
pub const KEYWORD_CANDIDATES: &[&str] = &[
    ":red", ":yellow", ":green", ":blue", ":all", ":tag:", ":attr:", ":class:",
];
pub const OPERATORS: &[&str] = &["<", ">", "<=", ">="];
pub const STARTERS: &[&str] = &[">", "<", "s>", "s<", "m<", "m>", "c<", "a<", ":", "\\", "|"];

pub const MAX_BYTES: i64 = 1 << 62;
pub const MAX_SECONDS: i64 = 1000 * 365 * DAY;
const OP_CHARS: &[char] = &['<', '>', '=', '!'];
const GLOB_SPECIAL: &[char] = &['*', '?', '\\'];

fn lookup<T: Copy>(table: &[(&str, T)], key: &str) -> Option<T> {
    table.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

fn strings<'a>(it: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    it.into_iter().map(str::to_owned).collect()
}

fn prefix_spellings() -> Vec<String> {
    SIZE_PREFIXES
        .iter()
        .map(|(k, _)| *k)
        .chain(AGE_PREFIXES.iter().map(|(k, _)| *k))
        .map(str::to_owned)
        .collect()
}

/// The vocabulary of the built-in classes, no platform limits.
pub fn default_vocab() -> FilterVocab {
    FilterVocab {
        classes: builtin_classes().to_vec(),
        unsupported_attrs: Vec::new(),
        unsupported_ages: Vec::new(),
    }
}

// --------------------------------------------------------------------------- text helpers

/// Python's `str.isspace` for one character.
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Python's `str.casefold` (full case folding for the characters where it differs from lower case).
pub fn casefold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            'ß' | 'ẞ' => out.push_str("ss"),
            'ς' => out.push('σ'),
            'ſ' => out.push('s'),
            'µ' => out.push('μ'),
            'ﬀ' => out.push_str("ff"),
            'ﬁ' => out.push_str("fi"),
            'ﬂ' => out.push_str("fl"),
            'ﬃ' => out.push_str("ffi"),
            'ﬄ' => out.push_str("ffl"),
            'ﬅ' | 'ﬆ' => out.push_str("st"),
            c if c.is_ascii() => out.push(c.to_ascii_lowercase()),
            c => out.extend(c.to_lowercase()),
        }
    }
    out
}

fn lower(chars: &[char]) -> String {
    chars.iter().collect::<String>().to_lowercase()
}

#[derive(Clone, Debug)]
struct ClassIndex {
    lookup: HashMap<String, String>,
    order: HashMap<String, usize>,
    spellings: Vec<(String, String, String)>,
    display: Vec<String>,
}

impl ClassIndex {
    fn new(vocab: &FilterVocab) -> Self {
        let mut lookup = HashMap::new();
        let mut spellings = Vec::new();
        for c in &vocab.classes {
            for name in std::iter::once(&c.name)
                .chain(c.aliases.iter())
                .chain(std::iter::once(&c.id))
            {
                let key = casefold(name);
                lookup.entry(key.clone()).or_insert_with(|| c.id.clone());
                spellings.push((key, c.id.clone(), c.name.clone()));
            }
        }
        let mut order = HashMap::new();
        for (i, c) in vocab.classes.iter().enumerate() {
            order.insert(c.id.clone(), i);
        }
        Self {
            lookup,
            order,
            spellings,
            display: vocab.classes.iter().map(|c| c.name.clone()).collect(),
        }
    }
}

/// A vocabulary with its lookup tables built: parse with it many times.
#[derive(Clone, Debug)]
pub struct CompiledVocab {
    vocab: FilterVocab,
    index: ClassIndex,
}

static DEFAULT: LazyLock<CompiledVocab> = LazyLock::new(|| CompiledVocab::new(default_vocab()));

impl CompiledVocab {
    pub fn new(vocab: FilterVocab) -> Self {
        let index = ClassIndex::new(&vocab);
        Self { vocab, index }
    }

    /// The built-in vocabulary (shared).
    pub fn builtin() -> &'static CompiledVocab {
        &DEFAULT
    }

    pub fn vocab(&self) -> &FilterVocab {
        &self.vocab
    }

    /// The class id a spelling names (case-insensitive: name, alias or id).
    pub fn class_id(&self, spelling: &str) -> Option<&str> {
        self.index
            .lookup
            .get(&casefold(spelling))
            .map(String::as_str)
    }

    /// Parse a filter expression. `cursor` (UTF-16 offset) enables pending/completion handling.
    pub fn parse(&self, text: &str, cursor: Option<u32>) -> FilterParse {
        let mut p = Parser::new(text, cursor, self);
        p.run();
        p.warnings();
        p.tokens.sort_by_key(|t| t.start);
        let has_error = p.diags.iter().any(|d| d.severity == Severity::Error);
        let mut diagnostics = p.diags;
        diagnostics.sort_by_key(|d| (d.start, d.end));
        let normal = if has_error {
            None
        } else {
            Some(self.normalize(&p.conds))
        };
        FilterParse {
            text: text.to_owned(),
            tokens: p.tokens,
            conditions: p.conds,
            pending: p.pending,
            diagnostics,
            normal,
            completion: p.completion,
        }
    }

    /// Canonical spelling of `text`'s normal form, or None if it has errors.
    pub fn canonical(&self, text: &str) -> Option<String> {
        self.parse(text, None).normal.map(|n| n.canonical)
    }

    /// Merge conditions into the normal form (see the grammar document, section 6).
    pub fn normalize(&self, conds: &[FilterCond]) -> FilterNormal {
        normalize_with(conds, &self.index.order)
    }
}

/// Parse a filter expression with `vocab` (default: the built-in classes).
pub fn parse(text: &str, cursor: Option<u32>, vocab: Option<&FilterVocab>) -> FilterParse {
    match vocab {
        None => CompiledVocab::builtin().parse(text, cursor),
        Some(v) => CompiledVocab::new(v.clone()).parse(text, cursor),
    }
}

/// Canonical spelling of `text`'s normal form, or None if it has errors.
pub fn canonical(text: &str, vocab: Option<&FilterVocab>) -> Option<String> {
    parse(text, None, vocab).normal.map(|n| n.canonical)
}

/// Merge conditions into the normal form.
pub fn normalize(conds: &[FilterCond], vocab: Option<&FilterVocab>) -> FilterNormal {
    match vocab {
        None => CompiledVocab::builtin().normalize(conds),
        Some(v) => normalize_with(conds, &ClassIndex::new(v).order),
    }
}

// --------------------------------------------------------------------------- offsets

/// Code-point index <-> UTF-16 offset (identity unless the text has astral characters).
struct U16 {
    cum: Option<Vec<u32>>,
}

impl U16 {
    fn new(chars: &[char]) -> Self {
        if !chars.iter().any(|&c| c as u32 > 0xFFFF) {
            return Self { cum: None };
        }
        let mut cum = Vec::with_capacity(chars.len() + 1);
        cum.push(0u32);
        let mut acc = 0u32;
        for &c in chars {
            acc += if c as u32 > 0xFFFF { 2 } else { 1 };
            cum.push(acc);
        }
        Self { cum: Some(cum) }
    }

    fn to16(&self, i: usize) -> u32 {
        match &self.cum {
            None => i as u32,
            Some(cum) => cum[i],
        }
    }

    fn from16(&self, u: u32) -> usize {
        match &self.cum {
            None => u as usize,
            // bisect_right(cum, u) - 1, at least 0
            Some(cum) => cum.partition_point(|&x| x <= u).saturating_sub(1),
        }
    }
}

// --------------------------------------------------------------------------- globs

/// Lower-case a glob and normalise wildcard runs (`*?*` -> `?*`); escapes are kept.
pub fn normalize_glob(pattern: &str) -> String {
    let p: Vec<char> = pattern.chars().collect();
    normalize_glob_chars(&p)
}

fn normalize_glob_chars(p: &[char]) -> String {
    let mut out = String::with_capacity(p.len());
    let (mut i, n) = (0, p.len());
    while i < n {
        let ch = p[i];
        if ch == '\\' && i + 1 < n {
            out.push(ch);
            out.push(p[i + 1]);
            i += 2;
            continue;
        }
        if ch == '*' || ch == '?' {
            let mut j = i;
            let (mut q, mut star) = (0, false);
            while j < n && (p[j] == '*' || p[j] == '?') {
                if p[j] == '?' {
                    q += 1;
                } else {
                    star = true;
                }
                j += 1;
            }
            out.extend(std::iter::repeat_n('?', q));
            if star {
                out.push('*');
            }
            i = j;
            continue;
        }
        out.extend(ch.to_lowercase());
        i += 1;
    }
    out
}

/// One part of a normalised glob.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GlobPart {
    /// Literal text (escapes resolved).
    Lit(String),
    /// `*`
    Any,
    /// `?`
    One,
}

/// Split a normalised glob into literal, `*` and `?` parts.
pub fn glob_parts(pattern: &str) -> Vec<GlobPart> {
    let p: Vec<char> = pattern.chars().collect();
    let mut parts = Vec::new();
    let mut lit = String::new();
    let (mut i, n) = (0, p.len());
    while i < n {
        let ch = p[i];
        if ch == '\\' && i + 1 < n {
            lit.push(p[i + 1]);
            i += 2;
            continue;
        }
        if ch == '*' || ch == '?' {
            if !lit.is_empty() {
                parts.push(GlobPart::Lit(std::mem::take(&mut lit)));
            }
            parts.push(if ch == '*' {
                GlobPart::Any
            } else {
                GlobPart::One
            });
        } else {
            lit.push(ch);
        }
        i += 1;
    }
    if !lit.is_empty() {
        parts.push(GlobPart::Lit(lit));
    }
    parts
}

fn bare_ok(pattern: &str) -> bool {
    if pattern.is_empty() || pattern.contains('\\') || pattern.contains(';') {
        return false;
    }
    let first = pattern.chars().next().expect("not empty");
    let last = pattern.chars().next_back().expect("not empty");
    if "|\\:\"".contains(first) || is_space(first) || is_space(last) {
        return false;
    }
    !pattern.chars().any(|c| "|<>".contains(c))
}

/// Spelling of a normalised glob: bare when possible, otherwise quoted with `\` escapes.
pub fn format_mask(pattern: &str) -> String {
    if bare_ok(pattern) {
        return pattern.to_owned();
    }
    format!("\"{}\"", pattern.replace('"', "\\\""))
}

// --------------------------------------------------------------------------- bounds

/// (rank, spelling) of `n` in the largest unit that divides it exactly (rank: larger = coarser).
///
/// Zero is the roundest number of all and is written in the smallest unit (`>0b`).
fn best_unit(n: i64, canon: &[(&str, i64)]) -> (usize, String) {
    if n == 0 {
        return (canon.len() + 1, format!("0{}", canon[canon.len() - 1].0));
    }
    for (rank, (unit, factor)) in canon.iter().enumerate() {
        if n % factor == 0 {
            return (canon.len() - rank, format!("{}{unit}", n / factor));
        }
    }
    unreachable!("unit 1 divides everything")
}

fn split_bound(text: &str) -> (String, String) {
    let i = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    (text[..i].to_owned(), text[i..].to_owned())
}

fn pick(options: Vec<(usize, u8, &'static str, String)>) -> Bound {
    let (_, _, op, text) = options.into_iter().max().expect("a bound has a spelling");
    let (number, unit) = split_bound(&text);
    let op = BoundOp::from_str(op).expect("operator");
    Bound { op, number, unit }
}

/// Canonical spelling of `value >= lo`: the rounder of `>lo-1` and `>=lo` (ties: `>`).
pub fn format_lower(lo: i64, canon: &[(&str, i64)]) -> Bound {
    let mut options = Vec::with_capacity(2);
    if lo > 0 {
        let (rank, text) = best_unit(lo - 1, canon);
        options.push((rank, 1, ">", text));
    }
    if lo >= 0 {
        let (rank, text) = best_unit(lo, canon);
        options.push((rank, 0, ">=", text));
    }
    if options.is_empty() {
        // Unreachable from parsed conditions (lower bounds are never negative).
        return Bound {
            op: BoundOp::Ge,
            number: lo.to_string(),
            unit: canon[canon.len() - 1].0.to_owned(),
        };
    }
    pick(options)
}

/// Canonical spelling of `value <= hi`: the rounder of `<hi+1` and `<=hi` (ties: `<`).
pub fn format_upper(hi: i64, canon: &[(&str, i64)]) -> Bound {
    let mut options = Vec::with_capacity(2);
    if hi + 1 >= 0 {
        let (rank, text) = best_unit(hi + 1, canon);
        options.push((rank, 1, "<", text));
    }
    if hi >= 0 {
        let (rank, text) = best_unit(hi, canon);
        options.push((rank, 0, "<=", text));
    }
    if options.is_empty() {
        return Bound {
            op: BoundOp::Le,
            number: hi.to_string(),
            unit: canon[canon.len() - 1].0.to_owned(),
        };
    }
    pick(options)
}

fn bound_text(prefix: &str, b: &Bound) -> String {
    format!("{prefix}{}{}{}", b.op.as_str(), b.number, b.unit)
}

fn range_texts(
    prefix: &str,
    lo: Option<i64>,
    hi: Option<i64>,
    canon: &[(&str, i64)],
) -> Vec<String> {
    let mut out = Vec::with_capacity(2);
    if let Some(lo) = lo {
        out.push(bound_text(prefix, &format_lower(lo, canon)));
    }
    if let Some(hi) = hi {
        out.push(bound_text(prefix, &format_upper(hi, canon)));
    }
    out
}

fn tags_text(colors: &[TagColor]) -> String {
    let chosen: Vec<TagColor> = TAG_COLORS
        .into_iter()
        .filter(|c| colors.contains(c))
        .collect();
    if chosen.len() == TAG_COLORS.len() {
        return ":all".to_owned();
    }
    if chosen.len() == 1 {
        return format!(":{}", chosen[0].as_str());
    }
    if chosen.is_empty() {
        return format!(":tag:all-{}", TAG_COLORS.map(TagColor::as_str).join("-"));
    }
    format!(
        ":tag:{}",
        chosen
            .iter()
            .map(|c| c.as_str())
            .collect::<Vec<_>>()
            .join("+")
    )
}

fn attrs_text(required: &[AttrName], forbidden: &[AttrName]) -> String {
    let pos: Vec<&str> = ATTR_NAMES
        .into_iter()
        .filter(|a| required.contains(a))
        .map(attr_letter)
        .collect();
    let mut out = String::from(":attr:");
    out.push_str(&pos.join("+"));
    for a in ATTR_NAMES.into_iter().filter(|a| forbidden.contains(a)) {
        out.push('-');
        out.push_str(attr_letter(a));
    }
    out
}

fn sorted_attrs(names: impl IntoIterator<Item = AttrName>) -> Vec<AttrName> {
    let s: BTreeSet<AttrName> = names.into_iter().collect();
    ATTR_NAMES.into_iter().filter(|a| s.contains(a)).collect()
}

fn ordered_tags(colors: &BTreeSet<TagColor>) -> Vec<TagColor> {
    TAG_COLORS
        .into_iter()
        .filter(|c| colors.contains(c))
        .collect()
}

// --------------------------------------------------------------------------- numbers

/// `floor(x)` and whether `x` is an integer, for `x = number * factor` (`number`: ASCII digits
/// with an optional fraction). `None` when `x` exceeds `limit`.
fn scaled(number: &[char], factor: i64, limit: i64) -> Option<(i64, bool)> {
    let (int_part, frac_part) = match number.iter().position(|&c| c == '.') {
        Some(i) => (&number[..i], &number[i + 1..]),
        None => (number, &[][..]),
    };
    let mut int: u128 = 0;
    for &c in int_part {
        int = int.checked_mul(10)?.checked_add(c.to_digit(10)? as u128)?;
    }
    let factor = factor as u128;
    // floor(frac * factor) by long multiplication of the decimal digits
    let (mut carry, mut exact) = (0u128, true);
    for &c in frac_part.iter().rev() {
        let v = c.to_digit(10)? as u128 * factor + carry;
        if !v.is_multiple_of(10) {
            exact = false;
        }
        carry = v / 10;
    }
    let floor = int.checked_mul(factor)?.checked_add(carry)?;
    let limit = limit as u128;
    if floor > limit || (floor == limit && !exact) {
        return None;
    }
    Some((floor as i64, exact))
}

/// Inclusive integer bounds of `value op x` (exact for integer values), negated if asked.
fn bounds(op: &str, floor: i64, exact: bool, neg: bool) -> (Option<i64>, Option<i64>) {
    let ceil = if exact { floor } else { floor + 1 };
    let (mut lo, mut hi) = (None, None);
    match op {
        ">" => lo = Some(floor + 1),
        ">=" => lo = Some(ceil),
        "<" => hi = Some(ceil - 1),
        _ => hi = Some(floor),
    }
    if neg {
        return match (lo, hi) {
            (Some(lo), _) => (None, Some(lo - 1)),
            (None, Some(hi)) => (Some(hi + 1), None),
            (None, None) => (None, None),
        };
    }
    (lo, hi)
}

// --------------------------------------------------------------------------- parser

/// Internal: abandon the current condition (the diagnostic or pending state is recorded).
struct Fail;

type R<T> = Result<T, Fail>;

struct Parser<'v> {
    t: Vec<char>,
    u16: U16,
    cursor: Option<usize>,
    vocab: &'v FilterVocab,
    classes: &'v ClassIndex,
    tokens: Vec<FilterToken>,
    conds: Vec<FilterCond>,
    diags: Vec<FilterDiagnostic>,
    pending: Option<PendingCond>,
    completion: Option<Completion>,
    // per-condition state
    a: usize,
    b: usize,
    us: u32,
    ue: u32,
    at_cursor: bool,
}

impl<'v> Parser<'v> {
    fn new(text: &str, cursor: Option<u32>, vocab: &'v CompiledVocab) -> Self {
        let t: Vec<char> = text.chars().collect();
        let u16 = U16::new(&t);
        let cursor = cursor.map(|c| u16.from16(c).min(t.len()));
        Self {
            t,
            u16,
            cursor,
            vocab: &vocab.vocab,
            classes: &vocab.index,
            tokens: Vec::new(),
            conds: Vec::new(),
            diags: Vec::new(),
            pending: None,
            completion: None,
            a: 0,
            b: 0,
            us: 0,
            ue: 0,
            at_cursor: false,
        }
    }

    // ------------------------------------------------------------- helpers

    fn sub(&self, s: usize, e: usize) -> String {
        self.t[s..e].iter().collect()
    }

    fn ws(&self, mut i: usize, end: usize) -> usize {
        while i < end && is_space(self.t[i]) {
            i += 1;
        }
        i
    }

    fn trim(&self, s: usize, mut e: usize) -> (usize, usize) {
        let s = self.ws(s, e);
        while e > s && is_space(self.t[e - 1]) {
            e -= 1;
        }
        (s, e)
    }

    fn span(&self, s: usize, e: usize) -> (u32, u32) {
        (self.u16.to16(s), self.u16.to16(e))
    }

    fn tok(&mut self, kind: TokenKind, s: usize, e: usize) {
        if e > s {
            let (start, end) = self.span(s, e);
            self.tokens.push(FilterToken { kind, start, end });
        }
    }

    fn cursor_in(&self, s: usize, e: usize) -> bool {
        matches!(self.cursor, Some(c) if s <= c && c <= e)
    }

    fn complete(
        &mut self,
        context: CompletionContext,
        s: usize,
        e: usize,
        candidates: Vec<String>,
    ) {
        if !self.cursor_in(s, e) {
            return;
        }
        let (start, end) = self.span(s, e);
        self.completion = Some(Completion {
            context,
            start,
            end,
            typed: self.sub(s, e),
            candidates,
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn fail_full(
        &mut self,
        code: FilterErrorCode,
        s: usize,
        e: usize,
        expected: Vec<String>,
        pending_ok: bool,
        category: Option<Category>,
    ) -> Fail {
        if pending_ok && self.at_cursor {
            let (start, end) = self.span(self.a, self.b);
            self.pending = Some(PendingCond {
                category,
                start,
                end,
            });
        } else {
            let (start, end) = self.span(s, e);
            let (cond_start, cond_end) = self.span(self.a, self.b);
            self.diags.push(FilterDiagnostic {
                code,
                severity: Severity::Error,
                start,
                end,
                cond_start,
                cond_end,
                text: self.sub(s, e),
                expected,
            });
        }
        Fail
    }

    fn fail(&mut self, code: FilterErrorCode, s: usize, e: usize) -> Fail {
        self.fail_full(code, s, e, Vec::new(), false, None)
    }

    fn find(&self, ch: char, from: usize, to: usize) -> Option<usize> {
        (from..to.min(self.t.len())).find(|&i| self.t[i] == ch)
    }

    // ------------------------------------------------------------- segments

    /// Split at `;` outside quoted masks. A quote opens a mask only at the start of a body.
    fn segments(&mut self) -> Vec<(usize, usize)> {
        let n = self.t.len();
        let mut out = Vec::new();
        let mut i = 0;
        loop {
            let mut j = self.ws(i, n);
            if j < n && self.t[j] == '|' {
                j = self.ws(j + 1, n);
            }
            if j < n && self.t[j] == '\\' {
                j = self.ws(j + 1, n);
            }
            if j < n && self.t[j] == '"' {
                j += 1;
                while j < n && self.t[j] != '"' {
                    j += if self.t[j] == '\\' { 2 } else { 1 };
                }
                j = (j + 1).min(n);
            }
            match self.find(';', j, n) {
                None => {
                    out.push((i, n));
                    return out;
                }
                Some(semi) => {
                    out.push((i, semi));
                    self.tok(TokenKind::Separator, semi, semi + 1);
                    i = semi + 1;
                }
            }
        }
    }

    fn run(&mut self) {
        for (s, e) in self.segments() {
            let (a, b) = self.trim(s, e);
            self.a = a;
            self.b = b;
            (self.us, self.ue) = self.span(a, b);
            self.at_cursor = a < b && self.cursor_in(a, b);
            if a == b {
                if let Some(c) = self.cursor
                    && s <= c
                    && c <= e
                {
                    self.complete(
                        CompletionContext::Prefix,
                        c,
                        c,
                        strings(STARTERS.iter().copied()),
                    );
                }
                continue;
            }
            if let Ok(cond) = self.condition() {
                self.conds.push(cond);
            }
        }
    }

    // ------------------------------------------------------------- conditions

    fn neg_prefix(neg: bool) -> &'static str {
        if neg { "|" } else { "" }
    }

    fn condition(&mut self) -> R<FilterCond> {
        let (a, b) = (self.a, self.b);
        let (mut p, mut neg) = (a, false);
        if self.t[p] == '|' {
            self.tok(TokenKind::Negate, p, p + 1);
            neg = true;
            p = self.ws(p + 1, b);
            if p >= b {
                return Err(self.fail_full(
                    FilterErrorCode::Incomplete,
                    a,
                    b,
                    Vec::new(),
                    true,
                    None,
                ));
            }
        }
        let ch = self.t[p];
        if ch == '\\' {
            return self.folder(p, neg);
        }
        if ch == ':' {
            return self.keyword(p, neg);
        }
        if ch == '"' || !self.t[p..b].iter().any(|&c| c == '<' || c == '>') {
            let pattern = self.mask(p, Category::Name)?;
            self.tok(TokenKind::Mask, p, b);
            let text = format!("{}{}", Self::neg_prefix(neg), format_mask(&pattern));
            return Ok(FilterCond::Name(MaskCond {
                start: self.us,
                end: self.ue,
                negated: neg,
                text,
                kind: NameKind,
                pattern,
            }));
        }
        self.compare(p, neg)
    }

    fn folder(&mut self, p: usize, neg: bool) -> R<FilterCond> {
        let q = self.ws(p + 1, self.b);
        self.tok(TokenKind::Folder, p, self.b);
        if q >= self.b {
            return Err(self.fail_full(
                FilterErrorCode::Incomplete,
                p,
                self.b,
                Vec::new(),
                true,
                Some(Category::Folder),
            ));
        }
        let pattern = self.mask(q, Category::Folder)?;
        let text = format!("{}\\{}", Self::neg_prefix(neg), format_mask(&pattern));
        Ok(FilterCond::Folder(FolderCond {
            start: self.us,
            end: self.ue,
            negated: neg,
            text,
            kind: FolderKind,
            pattern,
        }))
    }

    fn mask(&mut self, p: usize, category: Category) -> R<String> {
        let b = self.b;
        if self.t[p] != '"' {
            for i in p..b {
                if self.t[i] == '\\' {
                    return Err(self.fail(FilterErrorCode::BackslashInMask, i, i + 1));
                }
                if matches!(self.t[i], '|' | '<' | '>') {
                    return Err(self.fail(FilterErrorCode::BadChar, i, i + 1));
                }
            }
            return Ok(normalize_glob_chars(&self.t[p..b]));
        }
        let mut out: Vec<char> = Vec::new();
        let mut i = p + 1;
        while i < b && self.t[i] != '"' {
            if self.t[i] == '\\' {
                if i + 1 >= b {
                    return Err(self.fail_full(
                        FilterErrorCode::BadEscape,
                        i,
                        i + 1,
                        Vec::new(),
                        true,
                        Some(category),
                    ));
                }
                let nxt = self.t[i + 1];
                if GLOB_SPECIAL.contains(&nxt) {
                    out.push('\\');
                }
                out.push(nxt);
                i += 2;
                continue;
            }
            out.push(self.t[i]);
            i += 1;
        }
        if i >= b {
            return Err(self.fail_full(
                FilterErrorCode::UnterminatedQuote,
                p,
                b,
                Vec::new(),
                true,
                Some(category),
            ));
        }
        if self.ws(i + 1, b) < b {
            return Err(self.fail(FilterErrorCode::TextAfterQuote, i + 1, b));
        }
        if out.is_empty() {
            return Err(self.fail_full(
                FilterErrorCode::Incomplete,
                p,
                b,
                Vec::new(),
                true,
                Some(category),
            ));
        }
        Ok(normalize_glob_chars(&out))
    }

    fn keyword(&mut self, p: usize, neg: bool) -> R<FilterCond> {
        let b = self.b;
        let mut j = p + 1;
        while j < b && self.t[j].is_ascii_alphabetic() {
            j += 1;
        }
        let word = lower(&self.t[p + 1..j]);
        if j < b && self.t[j] == ':' {
            let kw_end = j + 1;
            if !VALUE_KEYWORDS.contains(&word.as_str()) {
                let expected = strings(KEYWORD_CANDIDATES.iter().copied());
                return Err(self.fail_full(
                    FilterErrorCode::UnknownKeyword,
                    p,
                    kw_end,
                    expected,
                    false,
                    None,
                ));
            }
            self.tok(TokenKind::Keyword, p, kw_end);
            let vs = self.ws(kw_end, b);
            let category = match word.as_str() {
                "class" => Category::Class,
                "tag" => Category::Tag,
                _ => Category::Attr,
            };
            if vs >= b {
                let ctx = completion_context(category);
                let cands = self.value_candidates(category, "");
                self.complete(ctx, kw_end, kw_end, cands);
                return Err(self.fail_full(
                    FilterErrorCode::Incomplete,
                    p,
                    b,
                    Vec::new(),
                    true,
                    Some(category),
                ));
            }
            self.tok(TokenKind::Value, vs, b);
            return match category {
                Category::Class => self.class(vs, neg),
                Category::Tag => self.tag_list(vs, neg),
                _ => self.attr_list(vs, neg),
            };
        }
        let rest = self.ws(j, b);
        let typed = lower(&self.t[p..j]);
        let cands = strings(
            KEYWORD_CANDIDATES
                .iter()
                .copied()
                .filter(|k| k.starts_with(typed.as_str())),
        );
        self.complete(CompletionContext::Keyword, p, j, cands);
        if TAG_KEYWORDS.contains(&word.as_str()) && rest >= b {
            self.tok(TokenKind::Keyword, p, j);
            let colors = if word == "all" {
                TAG_COLORS.to_vec()
            } else {
                vec![lookup(TAG_WORDS, &word).expect("tag word")]
            };
            let text = format!("{}{}", Self::neg_prefix(neg), tags_text(&colors));
            return Ok(FilterCond::Tag(TagCond {
                start: self.us,
                end: self.ue,
                negated: neg,
                text,
                kind: TagKind,
                colors,
            }));
        }
        self.tok(TokenKind::Keyword, p, b);
        let prefix_of_keyword = rest >= b
            && KEYWORD_CANDIDATES
                .iter()
                .any(|k| k.starts_with(typed.as_str()));
        let code = if word.is_empty() && rest >= b {
            FilterErrorCode::Incomplete
        } else {
            FilterErrorCode::UnknownKeyword
        };
        let expected = strings(KEYWORD_CANDIDATES.iter().copied());
        Err(self.fail_full(code, p, b, expected, prefix_of_keyword, None))
    }

    fn value_candidates(&self, category: Category, typed: &str) -> Vec<String> {
        let key = casefold(typed);
        match category {
            Category::Class => {
                let mut seen: Vec<String> = Vec::new();
                for (spelling, _id, display) in &self.classes.spellings {
                    if spelling.starts_with(&key) && !seen.contains(display) {
                        seen.push(display.clone());
                    }
                }
                seen
            }
            Category::Tag => TAG_COLORS
                .iter()
                .map(|c| c.as_str())
                .chain(std::iter::once("all"))
                .filter(|w| w.starts_with(&key))
                .map(str::to_owned)
                .collect(),
            _ => ATTR_NAMES
                .into_iter()
                .filter(|a| a.as_str().starts_with(&key) || attr_letter(*a).starts_with(&key))
                .map(|a| a.as_str().to_owned())
                .collect(),
        }
    }

    fn class(&mut self, vs: usize, neg: bool) -> R<FilterCond> {
        let name = self.sub(vs, self.b);
        let cands = self.value_candidates(Category::Class, &name);
        self.complete(CompletionContext::Class, vs, self.b, cands);
        let key = casefold(&name);
        let Some(cid) = self.classes.lookup.get(&key).cloned() else {
            let partial = self
                .classes
                .spellings
                .iter()
                .any(|(sp, _, _)| sp.starts_with(&key));
            let expected = self.classes.display.clone();
            return Err(self.fail_full(
                FilterErrorCode::UnknownClass,
                vs,
                self.b,
                expected,
                partial,
                Some(Category::Class),
            ));
        };
        let text = format!("{}:class:{cid}", Self::neg_prefix(neg));
        Ok(FilterCond::Class(ClassCond {
            start: self.us,
            end: self.ue,
            negated: neg,
            text,
            kind: ClassKind,
            class_id: cid,
        }))
    }

    fn word(&self, mut i: usize, stop: &[char]) -> usize {
        while i < self.b && !stop.contains(&self.t[i]) && !is_space(self.t[i]) {
            i += 1;
        }
        i
    }

    fn tag_list(&mut self, vs: usize, neg: bool) -> R<FilterCond> {
        let b = self.b;
        let mut colors: BTreeSet<TagColor> = BTreeSet::new();
        let (mut i, mut first) = (vs, true);
        loop {
            let mut op = '+';
            if !first {
                op = self.t[i];
                if op != '+' && op != '-' {
                    return Err(self.fail(FilterErrorCode::BadList, i, i + 1));
                }
                i = self.ws(i + 1, b);
                if i >= b {
                    return Err(self.fail_full(
                        FilterErrorCode::BadList,
                        i - 1,
                        b,
                        Vec::new(),
                        true,
                        Some(Category::Tag),
                    ));
                }
            }
            let j = self.word(i, &['+', '-']);
            let word = lower(&self.t[i..j]);
            let cands = self.value_candidates(Category::Tag, &word);
            self.complete(CompletionContext::Tag, i, j, cands);
            if word.is_empty() {
                return Err(self.fail(FilterErrorCode::BadList, i, (i + 1).min(b)));
            }
            let items: Vec<TagColor> = if word == "all" {
                TAG_COLORS.to_vec()
            } else if let Some(c) = lookup(TAG_WORDS, &word) {
                vec![c]
            } else {
                let partial = TAG_WORDS
                    .iter()
                    .map(|(w, _)| *w)
                    .chain(std::iter::once("all"))
                    .any(|w| w.starts_with(word.as_str()));
                let expected = strings(
                    TAG_COLORS
                        .iter()
                        .map(|c| c.as_str())
                        .chain(std::iter::once("all")),
                );
                return Err(self.fail_full(
                    FilterErrorCode::UnknownTag,
                    i,
                    j,
                    expected,
                    partial,
                    Some(Category::Tag),
                ));
            };
            if op == '+' {
                colors.extend(items);
            } else {
                for c in items {
                    colors.remove(&c);
                }
            }
            first = false;
            i = self.ws(j, b);
            if i >= b {
                break;
            }
        }
        let ordered = ordered_tags(&colors);
        let text = format!("{}{}", Self::neg_prefix(neg), tags_text(&ordered));
        Ok(FilterCond::Tag(TagCond {
            start: self.us,
            end: self.ue,
            negated: neg,
            text,
            kind: TagKind,
            colors: ordered,
        }))
    }

    fn attr_list(&mut self, vs: usize, neg: bool) -> R<FilterCond> {
        let b = self.b;
        let mut required: BTreeSet<AttrName> = BTreeSet::new();
        let mut forbidden: BTreeSet<AttrName> = BTreeSet::new();
        let (mut i, mut expect_item, mut negate) = (vs, true, false);
        loop {
            i = self.ws(i, b);
            if i >= b {
                if expect_item {
                    let s = vs.max(i.saturating_sub(1));
                    return Err(self.fail_full(
                        FilterErrorCode::BadList,
                        s,
                        b,
                        Vec::new(),
                        true,
                        Some(Category::Attr),
                    ));
                }
                break;
            }
            let ch = self.t[i];
            if expect_item {
                if ch == '-' {
                    if negate {
                        return Err(self.fail(FilterErrorCode::BadList, i, i + 1));
                    }
                    negate = true;
                    i += 1;
                    continue;
                }
                let mut j = i;
                while j < b && self.t[j].is_ascii_alphabetic() {
                    j += 1;
                }
                let word = lower(&self.t[i..j]);
                let cands = self.value_candidates(Category::Attr, &word);
                self.complete(CompletionContext::Attr, i, j, cands);
                if word.is_empty() {
                    return Err(self.fail(FilterErrorCode::BadList, i, i + 1));
                }
                let Some(attr) = lookup(ATTR_WORDS, &word) else {
                    let partial = ATTR_WORDS.iter().any(|(w, _)| w.starts_with(word.as_str()));
                    let expected = strings(ATTR_NAMES.iter().map(|a| a.as_str()));
                    return Err(self.fail_full(
                        FilterErrorCode::UnknownAttr,
                        i,
                        j,
                        expected,
                        partial,
                        Some(Category::Attr),
                    ));
                };
                if negate {
                    forbidden.insert(attr);
                } else {
                    required.insert(attr);
                }
                (expect_item, negate, i) = (false, false, j);
            } else if ch == '+' || ch == ',' {
                (expect_item, i) = (true, i + 1);
            } else if ch == '-' {
                (expect_item, negate, i) = (true, true, i + 1);
            } else {
                return Err(self.fail(FilterErrorCode::BadList, i, i + 1));
            }
        }
        let req = sorted_attrs(required);
        let forb = sorted_attrs(forbidden);
        let text = format!("{}{}", Self::neg_prefix(neg), attrs_text(&req, &forb));
        Ok(FilterCond::Attr(AttrCond {
            start: self.us,
            end: self.ue,
            negated: neg,
            text,
            kind: AttrKind,
            required: req,
            forbidden: forb,
        }))
    }

    fn compare(&mut self, p: usize, neg: bool) -> R<FilterCond> {
        let b = self.b;
        self.tok(TokenKind::Compare, p, b);
        let mut j = p;
        while j < b && self.t[j].is_ascii_alphabetic() {
            j += 1;
        }
        let prefix = lower(&self.t[p..j]);
        let k = self.ws(j, b);
        let mut o = k;
        while o < b && OP_CHARS.contains(&self.t[o]) {
            o += 1;
        }
        if o == k {
            let first_op = [self.find('<', p, b), self.find('>', p, b)]
                .into_iter()
                .flatten()
                .min()
                .unwrap_or(b);
            return Err(self.fail_full(
                FilterErrorCode::UnknownPrefix,
                p,
                first_op,
                prefix_spellings(),
                false,
                None,
            ));
        }
        let op = self.sub(k, o);
        if !OPERATORS.contains(&op.as_str()) {
            let expected = strings(OPERATORS.iter().copied());
            return Err(self.fail_full(FilterErrorCode::BadOperator, k, o, expected, false, None));
        }
        let size_prefix = lookup(SIZE_PREFIXES, &prefix);
        let age_prefix = lookup(AGE_PREFIXES, &prefix);
        if !prefix.is_empty() && size_prefix.is_none() && age_prefix.is_none() {
            return Err(self.fail_full(
                FilterErrorCode::UnknownPrefix,
                p,
                j,
                prefix_spellings(),
                false,
                None,
            ));
        }
        let pkind = if size_prefix.is_some() {
            Some(Category::Size)
        } else if age_prefix.is_some() {
            Some(Category::Age)
        } else {
            None
        };
        let n0 = self.ws(o, b);
        let mut q = n0;
        while q < b && self.t[q].is_ascii_digit() {
            q += 1;
        }
        if q < b && self.t[q] == '.' && q > n0 {
            let mut r = q + 1;
            while r < b && self.t[r].is_ascii_digit() {
                r += 1;
            }
            if r == q + 1 {
                return Err(self.fail_full(
                    FilterErrorCode::BadNumber,
                    n0,
                    r,
                    Vec::new(),
                    true,
                    pkind,
                ));
            }
            q = r;
        }
        if q == n0 {
            if n0 >= b {
                return Err(self.fail_full(
                    FilterErrorCode::Incomplete,
                    p,
                    b,
                    Vec::new(),
                    true,
                    pkind,
                ));
            }
            let w = self.word(n0, &[]);
            return Err(self.fail(FilterErrorCode::BadNumber, n0, w));
        }
        let u0 = self.ws(q, b);
        if u0 < b && !self.t[u0].is_ascii_alphabetic() {
            return Err(self.fail(FilterErrorCode::BadNumber, n0, b));
        }
        let mut u1 = u0;
        while u1 < b && self.t[u1].is_ascii_alphabetic() {
            u1 += 1;
        }
        let unit = lower(&self.t[u0..u1]);
        let units = units_for(pkind);
        let cands = units
            .iter()
            .filter(|u| u.starts_with(unit.as_str()))
            .cloned()
            .collect();
        self.complete(CompletionContext::Unit, u0, u1, cands);
        if self.ws(u1, b) < b {
            return Err(self.fail_full(FilterErrorCode::UnknownUnit, u0, b, units, false, None));
        }
        if unit.is_empty() {
            return Err(self.fail_full(FilterErrorCode::MissingUnit, q, q, units, true, pkind));
        }
        let size_factor = lookup(SIZE_UNITS, &unit);
        let time_factor = lookup(TIME_UNITS, &unit);
        let (ukind, factor) = match (size_factor, time_factor) {
            (Some(f), _) => (Category::Size, f),
            (None, Some(f)) => (Category::Age, f),
            (None, None) => {
                // a partly typed unit: pending, and its category is known once only one kind still fits
                let mut fits: Vec<Category> = Vec::new();
                for (u, _) in SIZE_UNITS.iter() {
                    if u.starts_with(unit.as_str())
                        && kind_ok(pkind, Category::Size)
                        && !fits.contains(&Category::Size)
                    {
                        fits.push(Category::Size);
                    }
                }
                for (u, _) in TIME_UNITS.iter() {
                    if u.starts_with(unit.as_str())
                        && kind_ok(pkind, Category::Age)
                        && !fits.contains(&Category::Age)
                    {
                        fits.push(Category::Age);
                    }
                }
                let category = pkind.or(if fits.len() == 1 { Some(fits[0]) } else { None });
                let pending_ok = !fits.is_empty();
                return Err(self.fail_full(
                    FilterErrorCode::UnknownUnit,
                    u0,
                    u1,
                    units,
                    pending_ok,
                    category,
                ));
            }
        };
        if pkind.is_some() && pkind != Some(ukind) {
            return Err(self.fail_full(FilterErrorCode::UnitMismatch, u0, u1, units, false, None));
        }
        let limit = if ukind == Category::Size {
            MAX_BYTES
        } else {
            MAX_SECONDS
        };
        let Some((floor, exact)) = scaled(&self.t[n0..q], factor, limit) else {
            return Err(self.fail(FilterErrorCode::OutOfRange, n0, u1));
        };
        let (lo, hi) = bounds(&op, floor, exact, neg);
        if ukind == Category::Size {
            let field = size_prefix.unwrap_or(SizeField::Disk);
            let text = range_texts(size_prefix_canon(field), lo, hi, SIZE_CANON).join(";");
            return Ok(FilterCond::Size(SizeCond {
                start: self.us,
                end: self.ue,
                negated: neg,
                text,
                kind: SizeKind,
                field,
                lo,
                hi,
            }));
        }
        let field = age_prefix.unwrap_or(AgeField::Modify);
        let text = range_texts(age_prefix_canon(field), lo, hi, TIME_CANON).join(";");
        Ok(FilterCond::Age(AgeCond {
            start: self.us,
            end: self.ue,
            negated: neg,
            text,
            kind: AgeKind,
            field,
            lo,
            hi,
        }))
    }

    // ------------------------------------------------------------- result

    fn warnings(&mut self) {
        let bad_attrs = &self.vocab.unsupported_attrs;
        let bad_ages = &self.vocab.unsupported_ages;
        let mut extra = Vec::new();
        for c in &self.conds {
            let (hit, start, end, text): (Vec<String>, u32, u32, &str) = match c {
                FilterCond::Attr(c) => (
                    c.required
                        .iter()
                        .chain(c.forbidden.iter())
                        .filter(|a| bad_attrs.contains(a))
                        .map(|a| a.as_str().to_owned())
                        .collect(),
                    c.start,
                    c.end,
                    &c.text,
                ),
                FilterCond::Age(c) if bad_ages.contains(&c.field) => {
                    (vec![c.field.as_str().to_owned()], c.start, c.end, &c.text)
                }
                _ => continue,
            };
            if !hit.is_empty() {
                extra.push(FilterDiagnostic {
                    code: FilterErrorCode::Unsupported,
                    severity: Severity::Warning,
                    start,
                    end,
                    cond_start: start,
                    cond_end: end,
                    text: text.to_owned(),
                    expected: hit,
                });
            }
        }
        self.diags.extend(extra);
    }
}

fn completion_context(c: Category) -> CompletionContext {
    match c {
        Category::Class => CompletionContext::Class,
        Category::Tag => CompletionContext::Tag,
        _ => CompletionContext::Attr,
    }
}

fn kind_ok(pkind: Option<Category>, unit_kind: Category) -> bool {
    match pkind {
        Some(k) => k == unit_kind,
        None => true,
    }
}

fn units_for(pkind: Option<Category>) -> Vec<String> {
    let sizes = SIZE_CANON.iter().rev().map(|(u, _)| *u);
    let times = TIME_CANON.iter().rev().map(|(u, _)| *u);
    match pkind {
        Some(Category::Size) => strings(sizes),
        Some(Category::Age) => strings(times),
        _ => strings(sizes.chain(times)),
    }
}

// --------------------------------------------------------------------------- normal form

fn merge(
    ranges: &mut Vec<(u8, Option<i64>, Option<i64>)>,
    key: u8,
    lo: Option<i64>,
    hi: Option<i64>,
) {
    let idx = match ranges.iter().position(|r| r.0 == key) {
        Some(i) => i,
        None => {
            ranges.push((key, None, None));
            ranges.len() - 1
        }
    };
    let cur = &mut ranges[idx];
    if let Some(lo) = lo {
        cur.1 = Some(cur.1.map_or(lo, |c| c.max(lo)));
    }
    if let Some(hi) = hi {
        cur.2 = Some(cur.2.map_or(hi, |c| c.min(hi)));
    }
}

fn normalize_with(conds: &[FilterCond], order: &HashMap<String, usize>) -> FilterNormal {
    let mut masks: BTreeSet<String> = BTreeSet::new();
    let mut classes: BTreeSet<String> = BTreeSet::new();
    let mut folders: BTreeSet<String> = BTreeSet::new();
    let mut tags: Option<BTreeSet<TagColor>> = None;
    let mut sizes: Vec<(u8, Option<i64>, Option<i64>)> = Vec::new();
    let mut ages: Vec<(u8, Option<i64>, Option<i64>)> = Vec::new();
    let mut attr_req: BTreeSet<AttrName> = BTreeSet::new();
    let mut attr_forb: BTreeSet<AttrName> = BTreeSet::new();
    let mut has_attr = false;
    let mut not_masks: BTreeSet<String> = BTreeSet::new();
    let mut not_classes: BTreeSet<String> = BTreeSet::new();
    let mut not_folders: BTreeSet<String> = BTreeSet::new();
    let mut not_tags: BTreeSet<TagColor> = BTreeSet::new();
    let mut not_attrs: BTreeSet<(Vec<AttrName>, Vec<AttrName>)> = BTreeSet::new();

    for c in conds {
        match c {
            FilterCond::Name(c) => {
                (if c.negated {
                    &mut not_masks
                } else {
                    &mut masks
                })
                .insert(c.pattern.clone());
            }
            FilterCond::Folder(c) => {
                (if c.negated {
                    &mut not_folders
                } else {
                    &mut folders
                })
                .insert(c.pattern.clone());
            }
            FilterCond::Class(c) => {
                (if c.negated {
                    &mut not_classes
                } else {
                    &mut classes
                })
                .insert(c.class_id.clone());
            }
            FilterCond::Tag(c) => {
                if c.negated {
                    not_tags.extend(c.colors.iter().copied());
                } else {
                    tags.get_or_insert_with(BTreeSet::new)
                        .extend(c.colors.iter().copied());
                }
            }
            FilterCond::Attr(c) => {
                if !c.negated {
                    has_attr = true;
                    attr_req.extend(c.required.iter().copied());
                    attr_forb.extend(c.forbidden.iter().copied());
                } else if !c.required.iter().any(|a| c.forbidden.contains(a)) {
                    // |(never true) is always true: dropped
                    not_attrs.insert((c.required.clone(), c.forbidden.clone()));
                }
            }
            FilterCond::Size(c) => merge(&mut sizes, c.field as u8, c.lo, c.hi),
            FilterCond::Age(c) => merge(&mut ages, c.field as u8, c.lo, c.hi),
        }
    }

    let class_key = |k: &String| (order.get(k).copied().unwrap_or(order.len()), k.clone());
    let mut pieces: Vec<String> = Vec::new();
    let out_masks: Vec<String> = masks.into_iter().collect();
    let mut out_classes: Vec<String> = classes.into_iter().collect();
    out_classes.sort_by_key(class_key);
    let out_folders: Vec<String> = folders.into_iter().collect();
    pieces.extend(out_masks.iter().map(|m| format_mask(m)));
    pieces.extend(out_classes.iter().map(|k| format!(":class:{k}")));
    pieces.extend(out_folders.iter().map(|f| format!("\\{}", format_mask(f))));
    let out_tags = tags.as_ref().map(ordered_tags);
    if let Some(t) = &out_tags {
        pieces.push(tags_text(t));
    }
    let mut out_sizes = Vec::new();
    for field in SIZE_FIELDS {
        let Some(&(_, mut lo, hi)) = sizes.iter().find(|r| r.0 == field as u8) else {
            continue;
        };
        if matches!(lo, Some(l) if l <= 0) {
            lo = None; // every size is >= 0
        }
        if lo.is_none() && hi.is_none() {
            continue;
        }
        let lower = lo.map(|l| format_lower(l, SIZE_CANON));
        let upper = hi.map(|h| format_upper(h, SIZE_CANON));
        out_sizes.push(Range {
            field: field.as_str().to_owned(),
            lo,
            hi,
            lower,
            upper,
        });
        pieces.extend(range_texts(size_prefix_canon(field), lo, hi, SIZE_CANON));
    }
    let mut out_ages = Vec::new();
    for field in AGE_FIELDS {
        let Some(&(_, lo, hi)) = ages.iter().find(|r| r.0 == field as u8) else {
            continue;
        };
        let lower = lo.map(|l| format_lower(l, TIME_CANON));
        let upper = hi.map(|h| format_upper(h, TIME_CANON));
        out_ages.push(Range {
            field: field.as_str().to_owned(),
            lo,
            hi,
            lower,
            upper,
        });
        pieces.extend(range_texts(age_prefix_canon(field), lo, hi, TIME_CANON));
    }
    let mut out_attrs = None;
    if has_attr {
        let req = sorted_attrs(attr_req.iter().copied());
        let forb = sorted_attrs(attr_forb.iter().copied());
        pieces.push(attrs_text(&req, &forb));
        out_attrs = Some(AttrSet {
            required: req,
            forbidden: forb,
        });
    }
    let mut neg_attr_texts: Vec<(String, Vec<AttrName>, Vec<AttrName>)> = not_attrs
        .into_iter()
        .map(|(r, f)| (attrs_text(&r, &f), r, f))
        .collect();
    neg_attr_texts.sort_by(|x, y| {
        x.0.cmp(&y.0)
            .then_with(|| {
                x.1.iter()
                    .map(|a| a.as_str())
                    .cmp(y.1.iter().map(|a| a.as_str()))
            })
            .then_with(|| {
                x.2.iter()
                    .map(|a| a.as_str())
                    .cmp(y.2.iter().map(|a| a.as_str()))
            })
    });
    let out_not_masks: Vec<String> = not_masks.into_iter().collect();
    let mut out_not_classes: Vec<String> = not_classes.into_iter().collect();
    out_not_classes.sort_by_key(class_key);
    let out_not_folders: Vec<String> = not_folders.into_iter().collect();
    pieces.extend(out_not_masks.iter().map(|m| format!("|{}", format_mask(m))));
    pieces.extend(out_not_classes.iter().map(|k| format!("|:class:{k}")));
    pieces.extend(
        out_not_folders
            .iter()
            .map(|f| format!("|\\{}", format_mask(f))),
    );
    let out_not_tags = ordered_tags(&not_tags);
    if !out_not_tags.is_empty() {
        pieces.push(format!("|{}", tags_text(&out_not_tags)));
    }
    pieces.extend(neg_attr_texts.iter().map(|(t, _, _)| format!("|{t}")));
    FilterNormal {
        masks: out_masks,
        classes: out_classes,
        folders: out_folders,
        tags: out_tags,
        sizes: out_sizes,
        ages: out_ages,
        attrs: out_attrs,
        not_masks: out_not_masks,
        not_classes: out_not_classes,
        not_folders: out_not_folders,
        not_tags: out_not_tags,
        not_attrs: neg_attr_texts
            .into_iter()
            .map(|(_, r, f)| AttrSet {
                required: r,
                forbidden: f,
            })
            .collect(),
        canonical: pieces.join(";"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaled_is_exact() {
        let c = |s: &str| s.chars().collect::<Vec<_>>();
        assert_eq!(scaled(&c("1.5"), 1024, MAX_BYTES), Some((1536, true)));
        assert_eq!(scaled(&c("0.3"), 1, MAX_BYTES), Some((0, false)));
        assert_eq!(
            scaled(
                &c("1.0000000000000000000000000000000000000000001"),
                1,
                MAX_BYTES
            ),
            Some((1, false))
        );
        assert_eq!(scaled(&c("4"), KIB.pow(5) * 1024 * 4, MAX_BYTES), None);
        assert_eq!(
            scaled(&c("4096"), KIB.pow(5), MAX_BYTES),
            Some((MAX_BYTES, true))
        );
        assert_eq!(scaled(&c("4096.0001"), KIB.pow(5), MAX_BYTES), None);
        assert_eq!(
            scaled(
                &c("99999999999999999999999999999999999999999999"),
                1,
                MAX_BYTES
            ),
            None
        );
    }

    #[test]
    fn casefold_matches_python_where_lower_differs() {
        assert_eq!(casefold("Straße"), "strasse");
        assert_eq!(casefold("AUDIO/Music"), "audio/music");
        assert_eq!(casefold("视频"), "视频");
    }

    #[test]
    fn glob_parts_split() {
        assert_eq!(
            glob_parts("a\\*b?*c"),
            vec![
                GlobPart::Lit("a*b".into()),
                GlobPart::One,
                GlobPart::Any,
                GlobPart::Lit("c".into())
            ]
        );
    }
}
