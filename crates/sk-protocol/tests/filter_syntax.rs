//! SKF 1 grammar: examples, equivalences, diagnostics, editing state and the five properties.
//!
//! The properties checked here are the ones docs/filter-syntax.md promises:
//!
//! * totality — every string parses without panicking, spans are well-formed;
//! * unambiguity — parsing is a function (same text → same result);
//! * canonical uniqueness — equivalent spellings share one canonical text;
//! * idempotence — `canonical(canonical(x)) == canonical(x)`;
//! * completeness — every normal form is reachable: `parse(canonical(x))` has `x`'s normal form.

use std::collections::BTreeSet;

use sk_protocol::filter::*;
use sk_protocol::filter_syntax::{canonical, default_vocab, normalize_glob, parse};

const BS: &str = "\\";
const MIB: i64 = 1 << 20;

fn canon(text: &str) -> String {
    canonical(text, None).unwrap_or_else(|| {
        panic!(
            "{text:?}: {:?}",
            parse(text, None, None)
                .diagnostics
                .iter()
                .map(|d| d.code)
                .collect::<Vec<_>>()
        )
    })
}

fn codes(text: &str, cursor: Option<u32>) -> Vec<&'static str> {
    parse(text, cursor, None)
        .diagnostics
        .iter()
        .map(|d| d.code.as_str())
        .collect()
}

/// Python `text[start:end]` with UTF-16 offsets (the test texts are BMP-only unless stated).
fn slice16(text: &str, start: u32, end: u32) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    String::from_utf16(&units[start as usize..end as usize]).unwrap()
}

fn len16(text: &str) -> u32 {
    text.encode_utf16().count() as u32
}

// --------------------------------------------------------------------------- examples

#[test]
fn canonical_examples() {
    let cases: Vec<(String, String)> = [
        ("", ""),
        (" ; ;", ""),
        ("*.txt", "*.txt"),
        ("*.TXT; *.jpg", "*.jpg;*.txt"),
        ("README", "readme"),
        ("|*.txt;|*.jpg", "|*.jpg;|*.txt"),
        (">1mb;<10mb", ">1mb;<10mb"),
        (">10mb;<1mb", ">10mb;<1mb"), // empty range: kept, matches nothing
        (">1mb;>2mb", ">2mb"),
        ("|>1mb", "<=1mb"),
        ("|>=1mb", "<1mb"),
        (">=1mb", ">=1mb"),
        (">0b", ">0b"),
        (">=0b", ""), // every size is >= 0
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
        (
            ":class:Video/Movies;:class:Pictures/Images",
            ":class:video;:class:image",
        ),
        ("\\temp", "\\temp"),
        ("|\\*windows*", "|\\*windows*"),
        ("\"a;b.txt\"", "\"a;b.txt\""),
        ("\"lit\\*star\"", "\"lit\\*star\""),
        ("\"  padded  \"", "\"  padded  \""),
        ("*\"*", "*\"*"),
        ("**.txt", "*.txt"),
        ("*?*x", "?*x"),
        ("| *.txt", "|*.txt"),
        ("\\ temp", "\\temp"),
        (
            "*.txt;\\temp;>1kb;:attr:h;:red;|*.log",
            "*.txt;\\temp;:red;>1kb;:attr:h;|*.log",
        ),
    ]
    .into_iter()
    .map(|(a, b)| (a.to_owned(), b.to_owned()))
    .collect();
    for (text, expected) in cases {
        assert_eq!(canon(&text), expected, "{text:?}");
    }
}

#[test]
fn equivalent_spellings_share_one_canonical() {
    let groups: &[&[&str]] = &[
        &[
            ":attr:a+h-s",
            ":attr:a,h,-s",
            ":ATTR:hidden+archive-system",
            ":attrs:a;:attr:h;:attr:-s",
        ],
        &[
            ":red;:yellow",
            ":tag:r+y",
            ":tag:red+yellow",
            ":tag:yellow;:tag:red",
        ],
        &[
            ">1mb;<10mb",
            "<10mb;>1mb",
            "> 1 mb ; < 10 MB",
            "d>1mb;disksize<10mb",
            "clustersize>1024kb;<10240kb",
        ],
        &["|>1mb", "<=1mb", "<1048577b", "|>1024kb"],
        &["m<5d", "<5days", "modify<120h", "MODIFIED<7200min", "<5 d"],
        &["s>1mb", "size>1mb", "filesize>1mb", "logicalsize>1024kb"],
        &["*.TXT", "*.txt", "**.txt", "*.Txt;*.tXt"],
        &[
            ":class:Audio/Music",
            ":class:音频",
            ":CLASS:music",
            ":class: audio ",
        ],
        &[">1.5gb", ">1536mb", ">1572864kb"],
        &["c<1w", "create<7d", "created<168h"],
        &["a>2y", "access>730d", "accessed>2years"],
        &[">1min", ">1m", ">60s", ">1minute", ">1minutes"],
    ];
    for group in groups {
        let set: BTreeSet<String> = group.iter().map(|t| canon(t)).collect();
        assert_eq!(
            set.len(),
            1,
            "{:?}",
            group.iter().map(|t| (t, canon(t))).collect::<Vec<_>>()
        );
    }
}

#[test]
fn conditions_carry_exact_integer_bounds() {
    let one = |t: &str| {
        let r = parse(t, None, None);
        assert_eq!(r.conditions.len(), 1, "{t}");
        r.conditions[0].clone()
    };
    let FilterCond::Size(c) = one(">1mb") else {
        panic!()
    };
    assert_eq!(
        (c.field, c.lo, c.hi),
        (SizeField::Disk, Some(MIB + 1), None)
    );
    let FilterCond::Size(c) = one("s<=1.5kb") else {
        panic!()
    };
    assert_eq!(
        (c.field, c.lo, c.hi),
        (SizeField::Logical, None, Some(1536))
    );
    let FilterCond::Size(c) = one(">0.3b") else {
        panic!()
    }; // x = 0.3 → value > 0.3 ⇔ value >= 1
    assert_eq!(c.lo, Some(1));
    let FilterCond::Age(c) = one("|c<5d") else {
        panic!()
    };
    assert_eq!(
        (c.field, c.lo, c.hi),
        (AgeField::Create, Some(5 * 86400), None)
    );
}

#[test]
fn text_without_comparison_operator_is_a_name_mask() {
    // "=", "!" and digits are legal in file names; only "<" and ">" make a comparison.
    assert_eq!(canon("=1mb"), "=1mb");
    assert!(matches!(
        parse("=1mb", None, None).conditions[0],
        FilterCond::Name(_)
    ));
}

#[test]
fn masks_are_lower_cased_globs() {
    let r = parse("ABC?*.Mp4", None, None);
    let FilterCond::Name(c) = &r.conditions[0] else {
        panic!()
    };
    assert_eq!(c.pattern, "abc?*.mp4");
    assert_eq!(normalize_glob("A**?b"), "a?*b");
}

// --------------------------------------------------------------------------- diagnostics

#[test]
fn errors_point_at_the_offending_text() {
    let s = |x: &str| x.to_owned();
    let cases: Vec<(String, &str, String)> = vec![
        (s(">500xb"), "unknown_unit", s("xb")),
        (s(">1k"), "unknown_unit", s("k")),
        (s("d<16k"), "unknown_unit", s("k")),
        (s(">500"), "missing_unit", s("")),
        (s("<5"), "missing_unit", s("")),
        (s(">"), "incomplete", s(">")),
        (s("s>"), "incomplete", s("s>")),
        (s(">abc"), "bad_number", s("abc")),
        (s(">1.mb"), "bad_number", s("1.")),
        (s(">.5mb"), "bad_number", s(".5mb")),
        (s(">1,5mb"), "bad_number", s("1,5mb")),
        (s(">-1mb"), "bad_number", s("-1mb")),
        (s("=>1mb"), "bad_operator", s("=>")),
        (s("<>1mb"), "bad_operator", s("<>")),
        (s("x>1mb"), "unknown_prefix", s("x")),
        (s("s<5d"), "unit_mismatch", s("d")),
        (s("m>1mb"), "unit_mismatch", s("mb")),
        (s("d<5d"), "unit_mismatch", s("d")),
        (s(">1mb x"), "unknown_unit", s("mb x")),
        (s(">99999999pb"), "out_of_range", s("99999999pb")),
        (s(":foo"), "unknown_keyword", s(":foo")),
        (s(":foo:bar"), "unknown_keyword", s(":foo:")),
        (s(":"), "incomplete", s(":")),
        (s(":class:"), "incomplete", s(":class:")),
        (s(":class:nosuch"), "unknown_class", s("nosuch")),
        (s(":tag:purple"), "unknown_tag", s("purple")),
        (s(":tag:+red"), "bad_list", s("+")),
        (s(":tag:red+"), "bad_list", s("+")),
        (s(":attr:z"), "unknown_attr", s("z")),
        (s(":attr:h++r"), "bad_list", s("+")),
        (s(":attr:h--r"), "bad_list", s("-")),
        (s(":attr:h r"), "bad_list", s("r")),
        (s(":attr:h,"), "bad_list", s(",")),
        (s("|"), "incomplete", s("|")),
        (s(BS), "incomplete", s(BS)),
        (format!("{BS}temp{BS}x"), "backslash_in_mask", s(BS)),
        (format!("a{BS}b"), "backslash_in_mask", s(BS)),
        (s("a|b"), "bad_char", s("|")),
        (format!("{BS}a>b"), "bad_char", s(">")),
        (s("\"abc"), "unterminated_quote", s("\"abc")),
        (s("\"abc\"x"), "text_after_quote", s("x")),
        (s("\"\""), "incomplete", s("\"\"")),
    ];
    for (text, code, span) in cases {
        let result = parse(&text, None, None);
        assert!(result.normal.is_none(), "{text:?}");
        assert_eq!(
            result.diagnostics.len(),
            1,
            "{text:?}: {:?}",
            result.diagnostics
        );
        let d = &result.diagnostics[0];
        assert_eq!(d.code.as_str(), code, "{text:?}");
        assert_eq!(slice16(&text, d.start, d.end), span, "{text:?}");
        assert_eq!(
            (d.cond_start, d.cond_end),
            (0, len16(text.trim())),
            "{text:?}"
        );
        assert_eq!(d.text, span);
    }
}

#[test]
fn every_error_lists_what_would_be_valid() {
    assert_eq!(
        parse(">500xb", None, None).diagnostics[0].expected[..6],
        ["b", "kb", "mb", "gb", "tb", "pb"]
    );
    assert_eq!(
        parse("m>1x", None, None).diagnostics[0].expected,
        ["s", "min", "h", "d", "w", "month", "y"]
    );
    assert!(
        parse(":class:nosuch", None, None).diagnostics[0]
            .expected
            .contains(&"视频".to_owned())
    );
    assert!(
        parse(":attr:z", None, None).diagnostics[0]
            .expected
            .contains(&"hidden".to_owned())
    );
}

#[test]
fn errors_in_one_condition_do_not_hide_the_others() {
    let result = parse(">1xb;*.txt;:foo", None, None);
    assert_eq!(
        codes(">1xb;*.txt;:foo", None),
        ["unknown_unit", "unknown_keyword"]
    );
    assert_eq!(result.conditions.len(), 1);
}

#[test]
fn spans_are_utf16_code_units() {
    let text = "😀*.txt;>1xb"; // the emoji is two UTF-16 code units
    let result = parse(text, None, None);
    assert_eq!(result.diagnostics.len(), 1);
    let d = &result.diagnostics[0];
    assert_eq!((d.start, d.end), (10, 12));
    let mask = result
        .tokens
        .iter()
        .find(|t| t.kind == TokenKind::Mask)
        .unwrap();
    assert_eq!((mask.start, mask.end), (0, 7));
    assert_eq!(result.conditions.len(), 1);
    assert_eq!(
        (result.conditions[0].start(), result.conditions[0].end()),
        (0, 7)
    );
}

// --------------------------------------------------------------------------- editing state

#[test]
fn unfinished_condition_at_the_cursor_is_pending_not_an_error() {
    let text = "*.mp4;*.mkv;*.mov;>500mb;:class:";
    let result = parse(text, Some(len16(text)), None);
    assert!(result.diagnostics.is_empty());
    let pending = result.pending.as_ref().unwrap();
    assert_eq!(
        (pending.category, pending.start, pending.end),
        (Some(Category::Class), 25, 32)
    );
    assert_eq!(
        result.normal.as_ref().unwrap().canonical,
        "*.mkv;*.mov;*.mp4;>500mb"
    );
    let completion = result.completion.as_ref().unwrap();
    assert_eq!(completion.context, CompletionContext::Class);
    assert_eq!(completion.candidates[..2], ["视频", "图片"]);
    // the same text with the caret elsewhere: the unfinished condition is an error
    assert_eq!(codes(text, Some(0)), ["incomplete"]);
}

#[test]
fn prefix_of_something_valid_is_pending() {
    let cases: &[(&str, Option<Category>)] = &[
        (":class:Vi", Some(Category::Class)),
        (":tag:re", Some(Category::Tag)),
        (":attr:hid", Some(Category::Attr)),
        (":attr:h+", Some(Category::Attr)),
        (":cla", None),
        (">5", None), // size or age: unknown until the unit is typed
        ("s>5", Some(Category::Size)),
        ("m<5", Some(Category::Age)),
        (">5mo", Some(Category::Age)),
        (">1.", None),
        ("\"abc", Some(Category::Name)),
        ("|", None),
    ];
    for &(text, category) in cases {
        let result = parse(text, Some(len16(text)), None);
        assert!(
            result.diagnostics.is_empty(),
            "{text:?}: {:?}",
            result.diagnostics
        );
        let pending = result
            .pending
            .unwrap_or_else(|| panic!("{text:?} not pending"));
        assert_eq!(pending.category, category, "{text:?}");
    }
    // complete already: 5 minutes
    let result = parse(">5m", Some(3), None);
    assert!(result.pending.is_none() && result.diagnostics.is_empty());
}

#[test]
fn completion_contexts() {
    let ctx = |text: &str| {
        parse(text, Some(len16(text)), None)
            .completion
            .map(|c| (c.context.as_str(), c.candidates))
    };
    let v = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(
        ctx(":"),
        Some((
            "keyword",
            v(&[
                ":red", ":yellow", ":green", ":blue", ":all", ":tag:", ":attr:", ":class:"
            ])
        ))
    );
    assert_eq!(ctx(":t"), Some(("keyword", v(&[":tag:"]))));
    assert_eq!(ctx(":tag:g"), Some(("tag", v(&["green"]))));
    assert_eq!(ctx(":attr:h+r"), Some(("attr", v(&["readonly"]))));
    assert_eq!(
        ctx(">5"),
        Some((
            "unit",
            v(&[
                "b", "kb", "mb", "gb", "tb", "pb", "s", "min", "h", "d", "w", "month", "y"
            ])
        ))
    );
    assert_eq!(
        ctx("s>5"),
        Some(("unit", v(&["b", "kb", "mb", "gb", "tb", "pb"])))
    );
    assert_eq!(ctx("m>5mo"), Some(("unit", v(&["month"]))));
    assert_eq!(ctx("*.txt;").unwrap().0, "prefix");
}

#[test]
fn tokens_cover_each_part_once() {
    let text = "|*.mp4;>1gb;:class:video;\\temp";
    let result = parse(text, None, None);
    let kinds: Vec<(&str, String)> = result
        .tokens
        .iter()
        .map(|t| (t.kind.as_str(), slice16(text, t.start, t.end)))
        .collect();
    let expected: Vec<(&str, String)> = [
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
    .into_iter()
    .map(|(k, s)| (k, s.to_owned()))
    .collect();
    assert_eq!(kinds, expected);
}

#[test]
fn platform_warnings_do_not_block() {
    let mut vocab = default_vocab();
    vocab.unsupported_attrs = vec![AttrName::System];
    vocab.unsupported_ages = vec![AgeField::Create];
    let result = parse(":attr:s;c<5d", None, Some(&vocab));
    assert!(result.normal.is_some());
    let got: Vec<(&str, Severity, Vec<String>)> = result
        .diagnostics
        .iter()
        .map(|d| (d.code.as_str(), d.severity, d.expected.clone()))
        .collect();
    assert_eq!(
        got,
        [
            ("unsupported", Severity::Warning, vec!["system".to_owned()]),
            ("unsupported", Severity::Warning, vec!["create".to_owned()])
        ]
    );
}

// --------------------------------------------------------------------------- SpaceSniffer help examples

#[test]
fn spacesniffer_help_examples_parse() {
    for text in [
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
    ] {
        assert!(parse(text, None, None).normal.is_some(), "{text:?}");
    }
}

#[test]
fn spacesniffer_help_example_d16k_is_rejected_like_spacesniffer_does() {
    // SpaceSniffer's help shows "d<16k", but SpaceSniffer 2.0.5.18 itself rejects it.
    assert_eq!(codes("d<16k", None), ["unknown_unit"]);
}

#[test]
fn json_shape_of_a_parse() {
    let r = parse("|*.TXT;>1mb", None, None);
    let v = serde_json::to_value(&r).unwrap();
    assert_eq!(v["conditions"][0]["kind"], "name");
    assert_eq!(v["conditions"][0]["pattern"], "*.txt");
    assert_eq!(v["conditions"][0]["negated"], true);
    assert_eq!(v["conditions"][1]["kind"], "size");
    assert_eq!(v["conditions"][1]["field"], "disk");
    assert_eq!(v["conditions"][1]["hi"], serde_json::Value::Null);
    assert_eq!(v["normal"]["tags"], serde_json::Value::Null);
    assert_eq!(
        v["normal"]["sizes"][0]["lower"],
        serde_json::json!({"op": ">", "number": "1", "unit": "mb"})
    );
    assert_eq!(v["pending"], serde_json::Value::Null);
    assert_eq!(v["completion"], serde_json::Value::Null);
    // and back
    let again: FilterParse = serde_json::from_value(v).unwrap();
    assert_eq!(again, r);
}

// --------------------------------------------------------------------------- properties

/// xorshift64*: deterministic, dependency-free random strings.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn coin(&mut self) -> bool {
        self.next() & 1 == 1
    }
}

const ALPHABET: &[char] = &[
    '*', '?', ';', '|', '\\', ':', '"', '<', '>', '=', '!', '.', ',', '+', '-', ' ', '\t', 'a',
    'b', 'c', 'd', 'g', 'h', 'k', 'm', 'n', 'r', 's', 't', 'w', 'y', 'A', 'B', 'C', 'M', 'S', '0',
    '1', '2', '3', '4', '5', '9', 'é', '视', '😀',
];

fn random_text(rng: &mut Rng) -> String {
    let n = rng.below(41);
    (0..n)
        .map(|_| ALPHABET[rng.below(ALPHABET.len())])
        .collect()
}

fn check_well_formed(text: &str, result: &FilterParse) {
    let n16 = len16(text);
    let mut prev = 0;
    for tok in &result.tokens {
        assert!(tok.start < tok.end && tok.end <= n16, "{text:?} {tok:?}");
        assert!(tok.start >= prev, "{text:?} overlapping tokens");
        prev = tok.end;
    }
    for d in &result.diagnostics {
        assert!(
            d.cond_start <= d.start && d.start <= d.end && d.end <= d.cond_end && d.cond_end <= n16,
            "{text:?} {d:?}"
        );
    }
    if let Some(p) = &result.pending {
        assert!(p.start <= p.end && p.end <= n16, "{text:?}");
    }
}

#[test]
fn parse_is_total() {
    let mut rng = Rng(0x5eed_1234_abcd_0001);
    for _ in 0..4000 {
        let text = random_text(&mut rng);
        let cursor = if rng.coin() {
            None
        } else {
            Some(rng.below(46) as u32)
        };
        let result = parse(&text, cursor, None);
        check_well_formed(&text, &result);
        assert_eq!(
            result.normal.is_none(),
            result
                .diagnostics
                .iter()
                .any(|d| d.severity == Severity::Error),
            "{text:?}"
        );
        assert_eq!(
            parse(&text, cursor, None),
            result,
            "{text:?}: not a function of its input"
        );
    }
}

#[test]
fn canonical_is_idempotent_and_complete() {
    let mut rng = Rng(0x0dd_ba11_cafe_f00d);
    let mut checked = 0;
    for _ in 0..6000 {
        let text = random_text(&mut rng);
        let result = parse(&text, None, None);
        let Some(normal) = result.normal else {
            continue;
        };
        checked += 1;
        let c = normal.canonical.clone();
        let again = parse(&c, None, None);
        assert!(
            again
                .diagnostics
                .iter()
                .all(|d| d.severity == Severity::Warning),
            "{text:?} -> {c:?}: {:?}",
            again.diagnostics
        );
        assert_eq!(again.normal.as_ref(), Some(&normal), "{text:?} -> {c:?}");
        assert_eq!(canonical(&c, None).as_deref(), Some(c.as_str()), "{text:?}");
    }
    assert!(checked > 500, "only {checked} valid samples");
}

const WORDS: &[&str] = &[
    "*.txt",
    "*.JPG",
    "a?c",
    "x*y*",
    "\"a;b\"",
    "\"q\\\"x\"",
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
];

#[test]
fn order_and_repetition_do_not_matter() {
    let mut rng = Rng(0xfeed_beef_0bad_c0de);
    for _ in 0..1500 {
        let n = rng.below(9);
        let parts: Vec<String> = (0..n)
            .map(|_| {
                let neg = if rng.coin() { "|" } else { "" };
                format!("{neg}{}", WORDS[rng.below(WORDS.len())])
            })
            .collect();
        let text = parts.join(";");
        let mut shuffled: Vec<String> = parts
            .iter()
            .chain(parts[..parts.len() / 2].iter())
            .cloned()
            .collect();
        for i in (1..shuffled.len()).rev() {
            let j = rng.below(i + 1);
            shuffled.swap(i, j);
        }
        assert_eq!(canon(&text), canon(&shuffled.join(";")), "{text:?}");
    }
}
