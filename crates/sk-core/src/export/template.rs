//! SpaceSniffer-compatible export templates: parsing and rendering (docs/export-templates.md).
//!
//! The language has three parts:
//!
//! * tags `<%name%>` — a property of the current element (`pathfile`, `disksize`, …);
//! * brace groups `{…}` — plain grouping, or a function when the group starts with a keyword:
//!   `{if{a}=={b}?{x}:{y}}`, `{leftpad{text}{pad}{length}}`, `{nest{text}}`, `{&br}` …;
//! * plain text, copied as it is (`{script}` drops line breaks and tabs from a section first).
//!
//! Parsing never fails: problems become [`TemplateIssue`]s and the affected part renders a marker
//! (`*BAD TAG*`, `*BAD PAD*`, `*BAD IF*`, `*BAD CALL*`). Rendering is streamed in chunks.

use std::borrow::Cow;
use std::collections::BTreeSet;

use sk_store::{NodeId, Nodes};

use super::util::{fill, size as fmt_size, tr};
use super::tree::{Blocks, Element, ExportTree, Mask, Ordering, SortKey, export_roots, text_of};
use super::{ExportError, Sink};

pub const BAD_TAG: &str = "*BAD TAG*";
pub const BAD_PAD: &str = "*BAD PAD*";
pub const BAD_IF: &str = "*BAD IF*";
pub const BAD_CALL: &str = "*BAD CALL*";
/// Longest leftpad / rightpad result (a typo must not allocate gigabytes per element).
pub const MAX_PAD: usize = 10_000;
/// Default chunk size of [`render_chunks`] (bytes).
pub const CHUNK_BYTES: usize = 1 << 16;

/// Message ids (Chinese source text) of the diagnostics, translated when a template is parsed.
mod msg {
    pub const UNKNOWN_TAG: &str = "未知的标签 <%{name}%>，输出 *BAD TAG*";
    pub const UNCLOSED_TAG: &str = "标签缺少结尾的 %>";
    pub const UNCLOSED_GROUP: &str = "缺少与之配对的 }}";
    pub const STRAY_CLOSE: &str = "多余的 }}，按原样输出（要输出 }} 请写 {{&curlycl}}）";
    pub const UNKNOWN_ENTITY: &str = "未知的转义 {{{name}}}，按原样输出";
    pub const BAD_IF: &str = "if 的写法应为 {usage}：{detail}";
    pub const BAD_CALL: &str = "{name} 的写法应为 {usage}";
    pub const BAD_OPTION: &str = "选项 @{key} 的值 {value!r} 无效，可选：{allowed}";
    pub const UNKNOWN_OPTION: &str = "未知的选项 @{key}，已忽略";
    pub const BAD_LINE: &str = "这一行既不是 @选项=值，也不是段标记（@header / @detail / @footer），已忽略";
    pub const DUPLICATE_SECTION: &str = "@{name} 段出现了不止一次，以最后一次为准";
    pub const IF_OPERAND: &str = "缺少值或值没有用 {{}} 括起来";
    pub const IF_OPERATOR: &str = "缺少比较运算符（== != < <= > >=）";
    pub const IF_QUESTION: &str = "缺少 ?";
    pub const IF_END: &str = "结尾缺少 }}";
    // argument names of the usage hints
    pub const ARG_TEXT: &str = "文字";
    pub const ARG_PAD: &str = "填充";
    pub const ARG_LENGTH: &str = "长度";
    pub const ARG_VALUE1: &str = "值1";
    pub const ARG_VALUE2: &str = "值2";
    pub const ARG_THEN: &str = "结果";
    pub const ARG_ELSE: &str = "否则";
}

const ENTITIES: [(&str, &str); 6] = [
    ("&br", "\n"),
    ("&tab", "\t"),
    ("&curlyop", "{"),
    ("&curlycl", "}"),
    ("&angleop", "<"),
    ("&anglecl", ">"),
];
const FUNCTIONS: [&str; 7] = ["if", "leftpad", "rightpad", "nest", "html", "csv", "json"];
const COMPARISONS: [&str; 6] = ["==", "!=", "<=", ">=", "<", ">"]; // longest first

/// How a function is written, e.g. `{leftpad{文字}{填充}{长度}}` (in the interface language).
pub fn usage(word: &str) -> String {
    if word == "if" {
        return format!(
            "{{if{{{}}}=={{{}}}?{{{}}}:{{{}}}}}",
            tr(msg::ARG_VALUE1),
            tr(msg::ARG_VALUE2),
            tr(msg::ARG_THEN),
            tr(msg::ARG_ELSE)
        );
    }
    let args: &[&str] = match word {
        "leftpad" | "rightpad" => &[msg::ARG_TEXT, msg::ARG_PAD, msg::ARG_LENGTH],
        _ => &[msg::ARG_TEXT],
    };
    let mut out = format!("{{{word}");
    for a in args {
        out.push('{');
        out.push_str(&tr(a));
        out.push('}');
    }
    out.push('}');
    out
}

// ---------------------------------------------------------------------------------- diagnostics

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Options,
    Header,
    Detail,
    Footer,
}

impl Section {
    pub fn as_str(self) -> &'static str {
        match self {
            Section::Options => "options",
            Section::Header => "header",
            Section::Detail => "detail",
            Section::Footer => "footer",
        }
    }
}

/// A problem of a template source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplateIssue {
    pub severity: Severity,
    pub code: &'static str,
    /// for the user, in the interface language at parse time
    pub message: String,
    pub section: Section,
    /// 1-based, in the template source (characters)
    pub line: u32,
    /// 1-based
    pub column: u32,
}

impl From<&TemplateIssue> for sk_protocol::export::ExportIssue {
    fn from(i: &TemplateIssue) -> Self {
        sk_protocol::export::ExportIssue {
            severity: match i.severity {
                Severity::Error => sk_protocol::export::IssueSeverity::Error,
                Severity::Warning => sk_protocol::export::IssueSeverity::Warning,
            },
            code: i.code.to_owned(),
            message: i.message.clone(),
            section: i.section.as_str().to_owned(),
            line: i.line as i64,
            column: i.column as i64,
        }
    }
}

// ---------------------------------------------------------------------------------- tags

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TimeOut {
    Date,
    Clock,
    Age,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tag {
    File,
    FileName,
    FileExt,
    Path,
    PathFile,
    Size,
    SizeBytes,
    DiskSize,
    DiskSizeBytes,
    Time(usize, TimeOut),
    IsFile,
    IsFolder,
    Count,
    LocalCount,
    FileCount,
    LocalFileCount,
    FolderCount,
    LocalFolderCount,
    ContainsFiles,
    ContainsFolders,
    NestingLevel,
}

/// Every tag name (lower case) and what it outputs.
const TAGS: [(&str, Tag); 33] = [
    ("file", Tag::File),
    ("filename", Tag::FileName),
    ("fileext", Tag::FileExt),
    ("path", Tag::Path),
    ("pathfile", Tag::PathFile),
    ("size", Tag::Size),
    ("sizebytes", Tag::SizeBytes),
    ("disksize", Tag::DiskSize),
    ("disksizebytes", Tag::DiskSizeBytes),
    ("filecreatedate", Tag::Time(1, TimeOut::Date)),
    ("filecreatetime", Tag::Time(1, TimeOut::Clock)),
    ("filecreateage", Tag::Time(1, TimeOut::Age)),
    ("filemodifydate", Tag::Time(0, TimeOut::Date)),
    ("filemodifytime", Tag::Time(0, TimeOut::Clock)),
    ("filemodifyage", Tag::Time(0, TimeOut::Age)),
    ("fileaccessdate", Tag::Time(2, TimeOut::Date)),
    ("fileaccesstime", Tag::Time(2, TimeOut::Clock)),
    ("fileaccessage", Tag::Time(2, TimeOut::Age)),
    ("isfile", Tag::IsFile),
    ("isfolder", Tag::IsFolder),
    ("iscontainer", Tag::IsFolder),
    ("count", Tag::Count),
    ("localcount", Tag::LocalCount),
    ("filecount", Tag::FileCount),
    ("localfilecount", Tag::LocalFileCount),
    ("foldercount", Tag::FolderCount),
    ("localfoldercount", Tag::LocalFolderCount),
    ("containercount", Tag::FolderCount),
    ("localcontainercount", Tag::LocalFolderCount),
    ("containsfiles", Tag::ContainsFiles),
    ("containsfolders", Tag::ContainsFolders),
    ("containscontainers", Tag::ContainsFolders),
    ("nestinglevel", Tag::NestingLevel),
];

/// Tags that need the subtree counts (folded over the whole tree once).
const COUNT_TAGS: [&str; 4] = ["count", "filecount", "foldercount", "containercount"];

/// The tag names a template can use.
pub fn tag_names() -> impl Iterator<Item = &'static str> {
    TAGS.iter().map(|(n, _)| *n)
}

fn tag_of(name: &str) -> Option<Tag> {
    TAGS.iter().find(|(n, _)| *n == name).map(|(_, t)| *t)
}

/// `ts` in local time as a strftime `pattern`; "" when unknown (0) or out of range.
pub fn local_time(ts: i64, pattern: &str) -> String {
    use chrono::{LocalResult, TimeZone};
    if ts <= 0 {
        return String::new();
    }
    match chrono::Local.timestamp_opt(ts, 0) {
        LocalResult::Single(t) | LocalResult::Ambiguous(t, _) => t.format(pattern).to_string(),
        LocalResult::None => String::new(),
    }
}

fn bool_text(v: bool) -> &'static str {
    if v { "true" } else { "false" }
}

/// Evaluation context: the tree and the current element.
struct Cx<'t, 'e> {
    tree: &'t ExportTree<'t>,
    el: &'e Element<'e>,
}

impl Tag {
    fn put(self, cx: &Cx<'_, '_>, out: &mut String) {
        let (tree, el) = (cx.tree, cx.el);
        let counts = || if el.is_dir { tree.counts(el.pos) } else { (0, 0) };
        let local = || if el.is_dir { tree.local_counts(el.pos) } else { (0, 0) };
        let num = |out: &mut String, v: u64| {
            use std::fmt::Write;
            let _ = write!(out, "{v}");
        };
        match self {
            Tag::File => out.push_str(&text_of(el.name)),
            Tag::FileName => out.push_str(&text_of(el.stem_ext().0)),
            Tag::FileExt => out.push_str(&text_of(el.stem_ext().1)),
            Tag::Path => out.push_str(&text_of(el.parent_path)),
            Tag::PathFile => out.push_str(&text_of(el.path)),
            Tag::Size => out.push_str(&fmt_size(tree.sizes(el.pos).0)),
            Tag::SizeBytes => out.push_str(&tree.sizes(el.pos).0.to_string()),
            Tag::DiskSize => out.push_str(&fmt_size(tree.sizes(el.pos).1)),
            Tag::DiskSizeBytes => out.push_str(&tree.sizes(el.pos).1.to_string()),
            Tag::Time(which, kind) => {
                let ts = tree.time(el.pos, which);
                match kind {
                    TimeOut::Date => out.push_str(&local_time(ts, "%Y-%m-%d")),
                    TimeOut::Clock => out.push_str(&local_time(ts, "%H:%M:%S")),
                    TimeOut::Age => {
                        if ts > 0 {
                            let days = ((tree.now - ts as f64) / 86400.0).floor().max(0.0);
                            num(out, days as u64);
                        }
                    }
                }
            }
            Tag::IsFile => out.push_str(bool_text(!el.is_dir)),
            Tag::IsFolder => out.push_str(bool_text(el.is_dir)),
            Tag::Count => {
                let (f, d) = counts();
                num(out, f as u64 + d as u64)
            }
            Tag::LocalCount => {
                let (f, d) = local();
                num(out, f as u64 + d as u64)
            }
            Tag::FileCount => num(out, counts().0 as u64),
            Tag::LocalFileCount => num(out, local().0 as u64),
            Tag::FolderCount => num(out, counts().1 as u64),
            Tag::LocalFolderCount => num(out, local().1 as u64),
            Tag::ContainsFiles => out.push_str(bool_text(local().0 > 0)),
            Tag::ContainsFolders => out.push_str(bool_text(local().1 > 0)),
            Tag::NestingLevel => num(out, el.level as u64),
        }
    }
}

// ---------------------------------------------------------------------------------- nodes

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Esc {
    Html,
    Csv,
    Json,
}

#[derive(Clone, Debug)]
enum Node {
    Text(String),
    Tag(Tag),
    Seq(Vec<Node>),
    If(Box<IfNode>),
    Pad { left: bool, args: Box<[Node; 3]> },
    Nest(Box<Node>),
    Escape(Esc, Box<Node>),
}

#[derive(Clone, Debug)]
struct IfNode {
    a: Node,
    op: &'static str,
    b: Node,
    yes: Node,
    no: Option<Node>,
}

/// A sequence with nested sequences flattened, adjacent texts merged and empty texts dropped.
fn seq(items: Vec<Node>) -> Node {
    let mut merged: Vec<Node> = Vec::with_capacity(items.len());
    let push = |merged: &mut Vec<Node>, item: Node| match item {
        Node::Text(t) if t.is_empty() => {}
        Node::Text(t) => {
            if let Some(Node::Text(prev)) = merged.last_mut() {
                prev.push_str(&t);
            } else {
                merged.push(Node::Text(t));
            }
        }
        other => merged.push(other),
    };
    for item in items {
        if let Node::Seq(inner) = item {
            for i in inner {
                push(&mut merged, i);
            }
        } else {
            push(&mut merged, item);
        }
    }
    Node::Seq(merged)
}

impl Node {
    fn put(&self, cx: &Cx<'_, '_>, out: &mut String) {
        match self {
            Node::Text(t) => out.push_str(t),
            Node::Tag(t) => t.put(cx, out),
            Node::Seq(items) => {
                for i in items {
                    i.put(cx, out);
                }
            }
            Node::If(n) => {
                if compare(&n.a.text(cx), n.op, &n.b.text(cx)) {
                    n.yes.put(cx, out);
                } else if let Some(no) = &n.no {
                    no.put(cx, out);
                }
            }
            Node::Pad { left, args } => {
                let text = args[0].text(cx);
                let pad = args[1].text(cx);
                let length = args[2].text(cx);
                let length = length.trim();
                let ok = !pad.is_empty() && !length.is_empty() && length.bytes().all(|b| b.is_ascii_digit());
                let want = if ok { length.parse::<usize>().ok().filter(|&n| n <= MAX_PAD) } else { None };
                let Some(want) = want else {
                    out.push_str(BAD_PAD);
                    return;
                };
                let have = text.chars().count();
                if want <= have {
                    out.push_str(&text);
                    return;
                }
                let fill: String = pad.chars().cycle().take(want - have).collect();
                if *left {
                    out.push_str(&fill);
                    out.push_str(&text);
                } else {
                    out.push_str(&text);
                    out.push_str(&fill);
                }
            }
            Node::Nest(body) => {
                if cx.el.level > 0 {
                    let t = body.text(cx);
                    for _ in 0..cx.el.level {
                        out.push_str(&t);
                    }
                }
            }
            Node::Escape(kind, body) => {
                let t = body.text(cx);
                match kind {
                    Esc::Html => html_escape_into(&t, out),
                    Esc::Csv => out.push_str(&csv_field(&t)),
                    Esc::Json => json_string_into(&t, out),
                }
            }
        }
    }

    fn text(&self, cx: &Cx<'_, '_>) -> Cow<'_, str> {
        match self {
            Node::Text(t) => Cow::Borrowed(t),
            _ => {
                let mut s = String::new();
                self.put(cx, &mut s);
                Cow::Owned(s)
            }
        }
    }
}

/// `html.escape(text, quote=True)`.
pub fn html_escape_into(text: &str, out: &mut String) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            c => out.push(c),
        }
    }
}

pub fn html_escape(text: &str) -> String {
    let mut s = String::with_capacity(text.len());
    html_escape_into(text, &mut s);
    s
}

/// One CSV field (RFC 4180): quoted when it holds a comma, quote, line break or edge space.
pub fn csv_field(text: &str) -> Cow<'_, str> {
    if text.contains([',', '"', '\r', '\n']) || text != text.trim_matches(' ') {
        Cow::Owned(format!("\"{}\"", text.replace('"', "\"\"")))
    } else {
        Cow::Borrowed(text)
    }
}

/// `json.dumps(text, ensure_ascii=False)`.
pub fn json_string_into(text: &str, out: &mut String) {
    out.push('"');
    json_escape_into(text, out);
    out.push('"');
}

/// The body of a JSON string (no quotes), as Python's `encode_basestring` writes it.
pub fn json_escape_into(text: &str, out: &mut String) {
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
}

/// `{if}`: numbers compare as numbers, everything else as case-insensitive text.
pub fn compare(a: &str, op: &str, b: &str) -> bool {
    use std::cmp::Ordering as O;
    if a == b && matches!(op, "==" | "!=" | "<=" | ">=") {
        return op != "!=";
    }
    let (x, y) = (a.trim(), b.trim());
    let ord = match (decimal(x), decimal(y)) {
        (Some(dx), Some(dy)) => dx.cmp(&dy),
        _ => {
            let (fx, fy) = (
                sk_protocol::filter_syntax::casefold(x),
                sk_protocol::filter_syntax::casefold(y),
            );
            fx.cmp(&fy)
        }
    };
    match op {
        "==" => ord == O::Equal,
        "!=" => ord != O::Equal,
        "<" => ord == O::Less,
        "<=" => ord != O::Greater,
        ">" => ord == O::Greater,
        _ => ord != O::Less,
    }
}

/// An exact decimal (`[+-]?(\d+(\.\d*)?|\.\d+)`), normalised so that equal values compare equal.
#[derive(Debug, PartialEq, Eq)]
struct Decimal {
    negative: bool,
    int: String,
    frac: String,
}

fn decimal(s: &str) -> Option<Decimal> {
    let (negative, body) = match s.as_bytes().first() {
        Some(b'+') => (false, &s[1..]),
        Some(b'-') => (true, &s[1..]),
        _ => (false, s),
    };
    let (int, frac) = match body.split_once('.') {
        Some((i, f)) => (i, f),
        None => (body, ""),
    };
    let digits = |t: &str| t.bytes().all(|b| b.is_ascii_digit());
    if !digits(int) || !digits(frac) || (int.is_empty() && frac.is_empty()) {
        return None;
    }
    if int.is_empty() && !body.starts_with('.') {
        return None;
    }
    let int = int.trim_start_matches('0').to_owned();
    let frac = frac.trim_end_matches('0').to_owned();
    let zero = int.is_empty() && frac.is_empty();
    Some(Decimal { negative: negative && !zero, int, frac })
}

impl Ord for Decimal {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering as O;
        match (self.negative, other.negative) {
            (false, true) => return O::Greater,
            (true, false) => return O::Less,
            _ => {}
        }
        let mag = self
            .int
            .len()
            .cmp(&other.int.len())
            .then_with(|| self.int.cmp(&other.int))
            .then_with(|| self.frac.cmp(&other.frac));
        if self.negative { mag.reverse() } else { mag }
    }
}

impl PartialOrd for Decimal {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

// ---------------------------------------------------------------------------------- parsing

fn is_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\r' | '\n')
}

/// Recursive descent over one section; positions map back to the template source.
struct Parser<'a> {
    t: Vec<char>,
    section: Section,
    /// index in `raw` of each character (script mode dropped some)
    origin: Option<Vec<usize>>,
    raw: &'a [char],
    first_line: u32,
    issues: &'a mut Vec<TemplateIssue>,
    tags: BTreeSet<&'static str>,
}

impl Parser<'_> {
    fn issue(&mut self, at: usize, code: &'static str, severity: Severity, message: String) {
        let o = match &self.origin {
            None => at.min(self.raw.len()),
            Some(origin) => origin.get(at).copied().unwrap_or(self.raw.len()),
        };
        let before = &self.raw[..o];
        let line = self.first_line + before.iter().filter(|&&c| c == '\n').count() as u32;
        let line_start = before.iter().rposition(|&c| c == '\n').map_or(0, |k| k + 1);
        self.issues.push(TemplateIssue {
            severity,
            code,
            message,
            section: self.section,
            line,
            column: (o - line_start + 1) as u32,
        });
    }

    fn ws(&self, mut i: usize) -> usize {
        while i < self.t.len() && is_ws(self.t[i]) {
            i += 1;
        }
        i
    }

    fn starts_with(&self, i: usize, s: &str) -> bool {
        (i..).zip(s.chars()).all(|(k, c)| self.t.get(k) == Some(&c))
    }

    fn run(&mut self) -> Node {
        let (items, _) = self.items(0, false);
        seq(items)
    }

    fn items(&mut self, mut i: usize, inner: bool) -> (Vec<Node>, usize) {
        let n = self.t.len();
        let mut out = Vec::new();
        while i < n {
            let mut j = i;
            while j < n && !(self.t[j] == '{' || self.t[j] == '}' || self.starts_with(j, "<%")) {
                j += 1;
            }
            if j > i {
                out.push(Node::Text(self.t[i..j].iter().collect()));
                i = j;
                continue;
            }
            match self.t[i] {
                '{' => {
                    let (node, k) = self.group(i);
                    out.push(node);
                    i = k;
                }
                '}' => {
                    if inner {
                        return (out, i);
                    }
                    self.issue(i, "stray_close", Severity::Warning, fill(&tr(msg::STRAY_CLOSE), &[]));
                    out.push(Node::Text("}".into()));
                    i += 1;
                }
                _ => {
                    // "<%"
                    let end = (i + 2..n.saturating_sub(1)).find(|&k| self.t[k] == '%' && self.t[k + 1] == '>');
                    match end {
                        None => {
                            self.issue(i, "unclosed_tag", Severity::Error, tr(msg::UNCLOSED_TAG));
                            out.push(Node::Text(self.t[i..].iter().collect()));
                            i = n;
                        }
                        Some(end) => {
                            let name: String = self.t[i + 2..end].iter().collect();
                            out.push(self.tag(&name, i));
                            i = end + 2;
                        }
                    }
                }
            }
        }
        (out, i)
    }

    fn tag(&mut self, raw_name: &str, at: usize) -> Node {
        let name = raw_name.trim().to_lowercase();
        match tag_of(&name) {
            None => {
                let m = fill(&tr(msg::UNKNOWN_TAG), &[("name", raw_name.trim())]);
                self.issue(at, "unknown_tag", Severity::Error, m);
                Node::Text(BAD_TAG.into())
            }
            Some(t) => {
                if let Some((n, _)) = TAGS.iter().find(|(n, _)| *n == name) {
                    self.tags.insert(*n);
                }
                Node::Tag(t)
            }
        }
    }

    /// A `{…}` group at `i`: an entity, a function call, or plain grouping.
    fn group(&mut self, i: usize) -> (Node, usize) {
        let n = self.t.len();
        let j = self.ws(i + 1);
        // &?[A-Za-z]+
        let mut end = j;
        if end < n && self.t[end] == '&' {
            end += 1;
        }
        let letters = end;
        while end < n && self.t[end].is_ascii_alphabetic() {
            end += 1;
        }
        let (word, end) = if end > letters {
            (self.t[j..end].iter().collect::<String>().to_ascii_lowercase(), end)
        } else {
            (String::new(), j)
        };
        let after = self.t.get(end).copied();
        if word == "script" || word.starts_with('&') {
            let k = self.ws(end);
            if k < n && self.t[k] == '}' {
                if let Some((_, v)) = ENTITIES.iter().find(|(e, _)| *e == word) {
                    return (Node::Text((*v).into()), k + 1);
                }
                if word == "script" {
                    return (Node::Text(String::new()), k + 1); // the section was already prepared
                }
                let raw: String = self.t[j..end].iter().collect();
                let m = fill(&tr(msg::UNKNOWN_ENTITY), &[("name", &raw)]);
                self.issue(i, "unknown_entity", Severity::Warning, m);
            }
        } else if FUNCTIONS.contains(&word.as_str())
            && after.is_none_or(|c| is_ws(c) || c == '{' || c == '}')
        {
            return self.call(&word, i, end);
        }
        let (items, k) = self.items(i + 1, true);
        if k >= n {
            self.issue(i, "unclosed_group", Severity::Error, fill(&tr(msg::UNCLOSED_GROUP), &[]));
            return (seq(items), n);
        }
        (seq(items), k + 1)
    }

    fn arg(&mut self, i: usize) -> (Option<Node>, usize) {
        let j = self.ws(i);
        if j < self.t.len() && self.t[j] == '{' {
            let (node, k) = self.group(j);
            return (Some(node), k);
        }
        (None, j)
    }

    /// Index after the `}` closing the group that opens at `i` (or the end).
    fn skip_group(&self, i: usize) -> usize {
        let mut depth = 0i64;
        for k in i..self.t.len() {
            match self.t[k] {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return k + 1;
                    }
                }
                _ => {}
            }
        }
        self.t.len()
    }

    fn call(&mut self, word: &str, start: usize, mut i: usize) -> (Node, usize) {
        if word == "if" {
            return self.call_if(start, i);
        }
        let pad = word == "leftpad" || word == "rightpad";
        let arity = if pad { 3 } else { 1 };
        let mut args = Vec::with_capacity(arity);
        for _ in 0..arity {
            let (node, k) = self.arg(i);
            i = k;
            match node {
                Some(n) => args.push(n),
                None => break,
            }
        }
        let k = self.ws(i);
        if args.len() == arity && k < self.t.len() && self.t[k] == '}' {
            let node = if pad {
                let mut it = args.into_iter();
                let a = [it.next().unwrap(), it.next().unwrap(), it.next().unwrap()];
                Node::Pad { left: word == "leftpad", args: Box::new(a) }
            } else {
                let body = Box::new(args.pop().unwrap());
                match word {
                    "nest" => Node::Nest(body),
                    "html" => Node::Escape(Esc::Html, body),
                    "csv" => Node::Escape(Esc::Csv, body),
                    _ => Node::Escape(Esc::Json, body),
                }
            };
            return (node, k + 1);
        }
        let m = fill(&tr(msg::BAD_CALL), &[("name", word), ("usage", &usage(word))]);
        self.issue(start, "bad_call", Severity::Error, m);
        let marker = if pad { BAD_PAD } else { BAD_CALL };
        (Node::Text(marker.into()), self.skip_group(start))
    }

    fn call_if(&mut self, start: usize, i: usize) -> (Node, usize) {
        let fail = |p: &mut Self, detail: &str| -> (Node, usize) {
            let m = fill(&tr(msg::BAD_IF), &[("usage", &usage("if")), ("detail", &fill(&tr(detail), &[]))]);
            p.issue(start, "bad_if", Severity::Error, m);
            (Node::Text(BAD_IF.into()), p.skip_group(start))
        };
        let (a, i) = self.arg(i);
        let Some(a) = a else { return fail(self, msg::IF_OPERAND) };
        let j = self.ws(i);
        let Some(op) = COMPARISONS.iter().copied().find(|o| self.starts_with(j, o)) else {
            return fail(self, msg::IF_OPERATOR);
        };
        let (b, i) = self.arg(j + op.len());
        let Some(b) = b else { return fail(self, msg::IF_OPERAND) };
        let j = self.ws(i);
        if j >= self.t.len() || self.t[j] != '?' {
            return fail(self, msg::IF_QUESTION);
        }
        let (yes, i) = self.arg(j + 1);
        let Some(yes) = yes else { return fail(self, msg::IF_OPERAND) };
        let mut j = self.ws(i);
        let mut no = None;
        if j < self.t.len() && self.t[j] == ':' {
            let (n, i) = self.arg(j + 1);
            let Some(n) = n else { return fail(self, msg::IF_OPERAND) };
            no = Some(n);
            j = self.ws(i);
        }
        if j >= self.t.len() || self.t[j] != '}' {
            return fail(self, msg::IF_END);
        }
        (Node::If(Box::new(IfNode { a, op, b, yes, no })), j + 1)
    }
}

/// `\{[ \t\r\n]*script[ \t\r\n]*\}`, case-insensitively, anywhere in the section.
fn has_script(raw: &[char]) -> bool {
    let n = raw.len();
    for i in 0..n {
        if raw[i] != '{' {
            continue;
        }
        let mut k = i + 1;
        while k < n && is_ws(raw[k]) {
            k += 1;
        }
        if k + 6 > n {
            continue;
        }
        let word: String = raw[k..k + 6].iter().collect();
        if !word.eq_ignore_ascii_case("script") {
            continue;
        }
        k += 6;
        while k < n && is_ws(raw[k]) {
            k += 1;
        }
        if k < n && raw[k] == '}' {
            return true;
        }
    }
    false
}

#[derive(Clone, Debug)]
struct Compiled {
    node: Node,
    tags: BTreeSet<&'static str>,
    /// the output when the section has no tags and no functions
    static_text: Option<String>,
}

fn compile(raw: &str, section: Section, first_line: u32, issues: &mut Vec<TemplateIssue>) -> Compiled {
    let raw: Vec<char> = raw.chars().collect();
    let (text, origin) = if has_script(&raw) {
        // script mode: line breaks and tabs are layout, not output
        let origin: Vec<usize> = (0..raw.len()).filter(|&i| !matches!(raw[i], '\r' | '\n' | '\t')).collect();
        (origin.iter().map(|&i| raw[i]).collect(), Some(origin))
    } else {
        (raw.clone(), None)
    };
    let mut parser = Parser {
        t: text,
        section,
        origin,
        raw: &raw,
        first_line,
        issues,
        tags: BTreeSet::new(),
    };
    let node = parser.run();
    let tags = std::mem::take(&mut parser.tags);
    let static_text = match &node {
        Node::Seq(items) if items.iter().all(|i| matches!(i, Node::Text(_))) => Some(
            items
                .iter()
                .map(|i| if let Node::Text(t) = i { t.as_str() } else { "" })
                .collect(),
        ),
        _ => None,
    };
    Compiled { node, tags, static_text }
}

/// A parsed template file: sections, ordering, display data and diagnostics.
#[derive(Clone, Debug)]
pub struct ExportTemplate {
    pub name: String,
    /// suggested output file name ("" = none)
    pub file_name: String,
    pub ordering: Ordering,
    pub header: String,
    pub detail: String,
    pub footer: String,
    pub issues: Vec<TemplateIssue>,
    compiled: [Compiled; 3],
}

impl ExportTemplate {
    /// No errors (warnings allowed).
    pub fn ok(&self) -> bool {
        !self.issues.iter().any(|i| i.severity == Severity::Error)
    }

    /// The tags the template uses (lower case).
    pub fn tags(&self) -> BTreeSet<&'static str> {
        self.compiled.iter().flat_map(|c| c.tags.iter().copied()).collect()
    }

    pub fn needs_counts(&self) -> bool {
        self.compiled.iter().any(|c| c.tags.iter().any(|t| COUNT_TAGS.contains(t)))
    }
}

fn line_is_section(line: &str) -> Option<Section> {
    let t = line.trim_matches([' ', '\t']);
    let word = t.strip_prefix('@')?;
    match word.to_ascii_lowercase().as_str() {
        "header" => Some(Section::Header),
        "detail" => Some(Section::Detail),
        "footer" => Some(Section::Footer),
        _ => None,
    }
}

/// `[ \t]*@([A-Za-z]+)[ \t]*=(.*)`
fn option_line(line: &str) -> Option<(String, &str)> {
    let t = line.trim_start_matches([' ', '\t']).strip_prefix('@')?;
    let end = t.find(|c: char| !c.is_ascii_alphabetic()).unwrap_or(t.len());
    if end == 0 {
        return None;
    }
    let key = t[..end].to_ascii_lowercase();
    let rest = t[end..].trim_start_matches([' ', '\t']).strip_prefix('=')?;
    Some((key, rest))
}

/// Parse a template file (or a bare detail section); never fails.
///
/// A source whose first line starts with `@` is a template file: `@key=value` option lines
/// (`@name` `@file` `@blocks` `@sort` `@order`), blank lines and `#` comments, then the sections,
/// each introduced by a line `@header`, `@detail` or `@footer`. Anything else is the detail
/// section alone. Line breaks are normalised to `\n`; a section's last line break is not part of it.
pub fn parse_template(source: &str, name: &str) -> ExportTemplate {
    let text = source.strip_prefix('\u{feff}').unwrap_or(source);
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let text = text.strip_suffix('\n').unwrap_or(&text);
    let mut issues: Vec<TemplateIssue> = Vec::new();
    let mut sections: [Option<(String, u32)>; 3] = [None, None, None];
    let idx = |s: Section| match s {
        Section::Header => 0,
        Section::Detail => 1,
        _ => 2,
    };
    let (mut opt_name, mut opt_file) = (None::<String>, None::<String>);
    let mut ordering = Ordering::default();
    if !text.starts_with('@') {
        sections[1] = Some((text.to_owned(), 1));
    } else {
        let mut current: Option<Section> = None;
        let mut body: Vec<&str> = Vec::new();
        let mut start = 0u32;
        let close = |current: Option<Section>, body: &[&str], start: u32, sections: &mut [Option<(String, u32)>; 3], issues: &mut Vec<TemplateIssue>| {
            if let Some(cur) = current {
                if sections[idx(cur)].is_some() {
                    issues.push(TemplateIssue {
                        severity: Severity::Warning,
                        code: "duplicate_section",
                        message: fill(&tr(msg::DUPLICATE_SECTION), &[("name", cur.as_str())]),
                        section: cur,
                        line: start,
                        column: 1,
                    });
                }
                sections[idx(cur)] = Some((body.join("\n"), start + 1));
            }
        };
        for (k, line) in text.split('\n').enumerate() {
            let number = k as u32 + 1;
            if let Some(sec) = line_is_section(line) {
                close(current, &body, start, &mut sections, &mut issues);
                current = Some(sec);
                body.clear();
                start = number;
                continue;
            }
            if current.is_some() {
                body.push(line);
                continue;
            }
            if line.trim().is_empty() || line.trim_start().starts_with('#') {
                continue;
            }
            let warn = |issues: &mut Vec<TemplateIssue>, code: &'static str, message: String| {
                issues.push(TemplateIssue {
                    severity: Severity::Warning,
                    code,
                    message,
                    section: Section::Options,
                    line: number,
                    column: 1,
                })
            };
            let Some((key, value)) = option_line(line) else {
                warn(&mut issues, "bad_line", tr(msg::BAD_LINE));
                continue;
            };
            let value = value.trim();
            let lower = value.to_lowercase();
            match key.as_str() {
                "name" => opt_name = Some(value.to_owned()),
                "file" => opt_file = Some(value.to_owned()),
                "blocks" | "sort" | "order" => {
                    let allowed: &[&str] = match key.as_str() {
                        "blocks" => &["files", "folders", "mixed"],
                        "sort" => &["size", "name", "none"],
                        _ => &["desc", "asc"],
                    };
                    if allowed.contains(&lower.as_str()) || (key == "blocks" && value.is_empty()) {
                        match (key.as_str(), lower.as_str()) {
                            ("blocks", "files") => ordering.blocks = Blocks::Files,
                            ("blocks", "folders") => ordering.blocks = Blocks::Folders,
                            ("blocks", _) => ordering.blocks = Blocks::Mixed,
                            ("sort", "size") => ordering.sort = SortKey::Size,
                            ("sort", "name") => ordering.sort = SortKey::Name,
                            ("sort", _) => ordering.sort = SortKey::None,
                            (_, v) => ordering.descending = v == "desc",
                        }
                    } else {
                        let quoted = format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"));
                        let m = fill(
                            &tr(msg::BAD_OPTION).replace("{value!r}", "{value}"),
                            &[("key", &key), ("value", &quoted), ("allowed", &allowed.join(" / "))],
                        );
                        warn(&mut issues, "bad_option", m);
                    }
                }
                _ => {
                    let m = fill(&tr(msg::UNKNOWN_OPTION), &[("key", &key)]);
                    warn(&mut issues, "unknown_option", m);
                }
            }
        }
        close(current, &body, start, &mut sections, &mut issues);
    }
    let order = [Section::Header, Section::Detail, Section::Footer];
    let compiled = order.map(|s| {
        let (raw, first) = sections[idx(s)].clone().unwrap_or_default();
        compile(&raw, s, first.max(1), &mut issues)
    });
    let raw_of = |s: Section| sections[idx(s)].as_ref().map(|x| x.0.clone()).unwrap_or_default();
    ExportTemplate {
        name: opt_name.unwrap_or_else(|| name.to_owned()),
        file_name: opt_file.unwrap_or_default(),
        ordering,
        header: raw_of(Section::Header),
        detail: raw_of(Section::Detail),
        footer: raw_of(Section::Footer),
        issues,
        compiled,
    }
}

/// Diagnostics of a template source (for the editor and before an export).
pub fn check(source: &str) -> Vec<TemplateIssue> {
    parse_template(source, "").issues
}

// ---------------------------------------------------------------------------------- render

/// Render `template` for the tree below `root` in chunks of at least `chunk_bytes` bytes (the
/// last may be shorter), handed to `sink` as they fill.
///
/// `root` 0 renders every scan root in turn, each as its own export (header, detail, footer).
/// `mask` is a filter mask (a bool per node): only matching files and the folders that contain
/// them are exported, with the sizes and counts of what is visible.
pub fn render_chunks(
    nodes: &Nodes,
    root: NodeId,
    template: &ExportTemplate,
    mask: Mask<'_>,
    now: Option<f64>,
    chunk_bytes: usize,
    sink: &mut Sink<'_>,
) -> Result<(), ExportError> {
    let [header, detail, footer] = &template.compiled;
    let counts = template.needs_counts();
    let mut buf = String::with_capacity(chunk_bytes.min(1 << 20) + 256);
    for r in export_roots(nodes, root) {
        let tree = ExportTree::new(nodes, r, mask, template.ordering, counts, now)?;
        let top = tree.root_element();
        header.node.put(&Cx { tree: &tree, el: &top }, &mut buf);
        if detail.static_text.as_deref() != Some("") {
            tree.walk(|el| -> Result<(), ExportError> {
                detail.node.put(&Cx { tree: &tree, el }, &mut buf);
                if buf.len() >= chunk_bytes {
                    sink(&buf)?;
                    buf.clear();
                }
                Ok(())
            })?;
        }
        footer.node.put(&Cx { tree: &tree, el: &top }, &mut buf);
    }
    if !buf.is_empty() {
        sink(&buf)?;
    }
    Ok(())
}

/// The whole output of [`render_chunks`] as one string.
pub fn render(
    nodes: &Nodes,
    root: NodeId,
    template: &ExportTemplate,
    mask: Mask<'_>,
    now: Option<f64>,
) -> Result<String, ExportError> {
    let mut out = String::new();
    render_chunks(nodes, root, template, mask, now, CHUNK_BYTES, &mut |s| {
        out.push_str(s);
        Ok(())
    })?;
    Ok(out)
}

/// One tag of one element (tests and tools): `None` for an unknown tag name.
pub fn tag_value(tree: &ExportTree<'_>, el: &Element<'_>, name: &str) -> Option<String> {
    let t = tag_of(name)?;
    let mut out = String::new();
    t.put(&Cx { tree, el }, &mut out);
    Some(out)
}
