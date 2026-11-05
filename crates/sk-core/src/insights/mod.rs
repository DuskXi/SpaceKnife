//! Insights: known application folders and files — what they are and how to reclaim their space.
//!
//! The rules library behind the insight card (UI-MAIN-41/42) and the change annotations of the
//! snapshot comparison (UI-W6-30). Rule format: docs/insights.md; contract: docs/manage.md §5.
//!
//! A rule names path globs or bare names, plus optional conditions — the parent folder's name,
//! platforms, node kinds, a minimum size, exclusions — and says what the item is and how to
//! reclaim its space. Rules are TOML: the built-in `builtin.toml` plus the user's own files,
//! which override built-ins by id.
//!
//! Lookups cost a few hash probes per path segment: every pattern is indexed by its most
//! selective literal part (its last segment, its parent segment, its extension, or the first
//! three characters of its last segment), so only a handful of regexes run per lookup (each is
//! compiled the first time it is needed). A folder rule also covers everything inside the folder
//! (`subtree`). Among the matches of a path and its ancestors the winner has the highest
//! priority, then the deepest match, then the most specific pattern; when the winning rule also
//! matches further up, the outermost match is reported (the container is the unit).
//!
//! A path in Windows form (drive letter, UNC, `\\?\`) is matched against the Windows rules,
//! case-insensitively; any other path against the host's POSIX platform — Linux case-sensitively,
//! macOS case-insensitively — so a snapshot from another platform still gets its own platform's
//! rules. A [`RuleSet`] is immutable once built and safe to share between threads.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, OnceLock};

use parking_lot::RwLock;
use regex::Regex;
use sk_store::{Kind, NodeId, Nodes};
use toml::{Table, Value};

use crate::i18n::{Arg, format, t};

pub const FORMAT_VERSION: i64 = 1;
pub const BUILTIN_SOURCE: &str = "builtin.toml";
const BUILTIN_TEXT: &str = include_str!("builtin.toml");
const MAX_EXPANSION: usize = 256;

/// What an item is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum InsightKind {
    App,
    Cache,
    Build,
    Vm,
    Chat,
    Game,
    System,
}

impl InsightKind {
    pub const ALL: [InsightKind; 7] = [
        InsightKind::App,
        InsightKind::Cache,
        InsightKind::Build,
        InsightKind::Vm,
        InsightKind::Chat,
        InsightKind::Game,
        InsightKind::System,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            InsightKind::App => "app",
            InsightKind::Cache => "cache",
            InsightKind::Build => "build",
            InsightKind::Vm => "vm",
            InsightKind::Chat => "chat",
            InsightKind::Game => "game",
            InsightKind::System => "system",
        }
    }
    pub fn parse(s: &str) -> Option<InsightKind> {
        InsightKind::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// How its space is reclaimed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Cleanup {
    /// delete it
    Safe,
    /// use the app or command
    Tool,
    Review,
    Keep,
}

impl Cleanup {
    pub const ALL: [Cleanup; 4] = [Cleanup::Safe, Cleanup::Tool, Cleanup::Review, Cleanup::Keep];
    pub fn as_str(self) -> &'static str {
        match self {
            Cleanup::Safe => "safe",
            Cleanup::Tool => "tool",
            Cleanup::Review => "review",
            Cleanup::Keep => "keep",
        }
    }
    pub fn parse(s: &str) -> Option<Cleanup> {
        Cleanup::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// What a matched item is (contract: docs/manage.md §5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Insight {
    pub rule: String,
    pub title: String,
    pub reason: String,
    pub kind: InsightKind,
    /// short annotation after a name (W6 change reason), e.g. 「游戏更新」
    pub note: String,
    /// the owning application, for the subtitle line
    pub app: String,
    pub cleanup: Cleanup,
    /// the cleaning command when there is one: shown, never run
    pub command: String,
    /// the matched item: the path itself, or the folder it is inside
    pub path: String,
    /// matched through an ancestor folder (`path` is that folder)
    pub inside: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleText {
    pub title: String,
    pub reason: String,
    pub note: String,
    pub app: String,
}

/// A validated rule, before compilation (patterns are normalised globs).
#[derive(Clone, Debug)]
pub struct Rule {
    pub id: String,
    pub kind: InsightKind,
    pub text: RuleText,
    /// language tag → field → text
    pub i18n: BTreeMap<String, BTreeMap<String, String>>,
    pub cleanup: Cleanup,
    pub command: String,
    pub patterns: Vec<String>,
    pub parents: Vec<String>,
    pub exclude: Vec<String>,
    pub platforms: HashSet<&'static str>,
    pub nodes: HashSet<&'static str>,
    pub min_size: i64,
    pub priority: i64,
    pub subtree: bool,
    pub source: String,
}

impl Rule {
    /// Texts in `lang` (`en`, `en-US`, `zh-TW` …), falling back field by field to the primary
    /// language subtag and then to the rule's own texts.
    pub fn texts(&self, lang: Option<&str>) -> RuleText {
        let Some(lang) = lang.filter(|l| !l.is_empty()) else {
            return self.text.clone();
        };
        if self.i18n.is_empty() {
            return self.text.clone();
        }
        let tag = lang.replace('_', "-").to_lowercase();
        let primary = tag.split('-').next().unwrap_or("").to_string();
        let Some(tr) = self.i18n.get(&tag).or_else(|| self.i18n.get(&primary)) else {
            return self.text.clone();
        };
        if tr.is_empty() {
            return self.text.clone();
        }
        let base = &self.text;
        let title = tr
            .get("title")
            .cloned()
            .unwrap_or_else(|| base.title.clone());
        let note = match tr.get("note").filter(|n| !n.is_empty()) {
            Some(n) => n.clone(),
            None if tr.contains_key("title") => title.clone(),
            None => base.note.clone(),
        };
        RuleText {
            reason: tr
                .get("reason")
                .cloned()
                .unwrap_or_else(|| base.reason.clone()),
            app: tr.get("app").cloned().unwrap_or_else(|| base.app.clone()),
            title,
            note,
        }
    }
}

/// A file or rule that was skipped (or partly ignored), with a readable reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleProblem {
    pub source: String,
    /// the rule id, `#n` (1-based entry number) when it has none, "" for the file
    pub rule: String,
    pub message: String,
}

#[derive(Debug, thiserror::Error)]
pub enum InsightsError {
    #[error("unknown node kind {0:?} (file / dir / link / other)")]
    UnknownKind(String),
    #[error("{0} and paths differ in length")]
    Length(&'static str),
    #[error("unknown host platform {0:?}")]
    Host(String),
}

/// A node kind by name: `file` / `dir` / `link` / `other` (any case).
pub fn node_kind(name: &str) -> Result<Kind, InsightsError> {
    match name.to_lowercase().as_str() {
        "file" => Ok(Kind::File),
        "dir" => Ok(Kind::Dir),
        "link" => Ok(Kind::Link),
        "other" => Ok(Kind::Other),
        _ => Err(InsightsError::UnknownKind(name.to_string())),
    }
}

// --------------------------------------------------------------------------- texts

fn msg(key: &str) -> &'static str {
    t(match key {
        "file_unreadable" => "无法读取洞察规则文件 {source}：{error}",
        "file_syntax" => "洞察规则文件 {source} 不是有效的 TOML：{error}",
        "file_version" => {
            "洞察规则文件 {source} 的格式版本 {version} 不受支持（支持 {supported}），已跳过"
        }
        "file_keys" => "洞察规则文件 {source} 有未知的顶层字段 {keys}，已忽略",
        "file_rules" => "洞察规则文件 {source} 的 rule 应为表数组（[[rule]]）",
        "rule_skipped" => "跳过洞察规则 {rule}（{source}）：{error}",
        "rule_duplicate" => "洞察规则 {rule} 在 {source} 中出现多次，以最后一条为准",
        "rule_override" => "用户规则 {rule}（{source}）覆盖了同名规则",
        "rules_loaded" => "已加载 {count} 条洞察规则（来自 {files} 个文件，跳过 {skipped} 处问题）",
        _ => "",
    })
}

fn err(key: &str) -> &'static str {
    t(match key {
        "not_table" => "规则应为表",
        "missing" => "缺少字段 {key}",
        "type" => "字段 {key} 应为{expected}",
        "unknown" => "未知字段 {keys}",
        "bad_id" => "id {value!r} 无效：只能用字母、数字和 . _ -，并以字母或数字开头",
        "bad_choice" => "字段 {key} 的值 {value!r} 无效，可选 {choices}",
        "no_matcher" => "至少要有一条 paths 或 names",
        "name_sep" => "名称 {value!r} 不能为空或包含路径分隔符（多层路径请写进 paths）",
        "empty_choice" => "字段 {key} 不能为空列表",
        "bad_glob" => "通配符 {value!r} 无效：{error}",
        "empty_glob" => "通配符不能为空",
        "unclosed" => "[ 没有配对的 ]",
        "unclosed_brace" => "{ 没有配对的 }",
        "too_many" => "花括号展开后的模式超过 {limit} 个",
        "tilde" => "不支持 ~，请改用相对模式（如 .cache/pip，可匹配任意层级）",
        "bad_size" => "无法识别的大小 {value!r}（例：1048576、\"500 MB\"、\"2 GiB\"）",
        "bad_priority" => "priority 应在 -1000 到 1000 之间",
        "bad_lang" => "i18n 的语言代码 {value!r} 无效（例：en、zh-tw）",
        "bad_i18n" => "i18n.{lang} 只能包含 title、reason、note、app",
        _ => "",
    })
}

fn type_name(key: &str) -> &'static str {
    t(match key {
        "str" => "字符串",
        "strs" => "字符串或字符串列表",
        "int" => "整数",
        "bool" => "true 或 false",
        "table" => "表",
        _ => "",
    })
}

/// Python's `repr` of a string (single quotes unless it holds one).
fn repr(s: &str) -> String {
    if s.contains('\'') && !s.contains('"') {
        format!("\"{s}\"")
    } else {
        format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
    }
}

/// A template with `{value!r}` (given already quoted) and `{name}` fields.
fn fill(template: &str, value: Option<&str>, fields: &[(&str, Arg)]) -> String {
    let t = match value {
        Some(v) => template.replace("{value!r}", &v.replace('{', "{{").replace('}', "}}")),
        None => template.to_string(),
    };
    format(&t, fields)
}

/// A rule that cannot be used, and why.
#[derive(Debug)]
struct Invalid(String);

fn invalid(template: &str, fields: &[(&str, Arg)]) -> Invalid {
    Invalid(fill(template, None, fields))
}

fn invalid_value(template: &str, value: &str, fields: &[(&str, Arg)]) -> Invalid {
    Invalid(fill(template, Some(&repr(value)), fields))
}

fn type_error(key: &str, expected: &str) -> Invalid {
    invalid(
        err("type"),
        &[
            ("key", key.into()),
            ("expected", type_name(expected).into()),
        ],
    )
}

// --------------------------------------------------------------------------- globs

/// Brace alternatives, nested allowed: `a/{b,c/d}` → `a/b`, `a/c/d` (`[{]` is a literal).
fn expand(pattern: &str) -> Result<Vec<String>, Invalid> {
    let mut out: Vec<String> = Vec::new();
    let mut pending: Vec<String> = vec![pattern.to_string()];
    while let Some(p) = pending.pop() {
        let chars: Vec<char> = p.chars().collect();
        let n = chars.len();
        let (mut depth, mut open_at) = (0usize, None::<usize>);
        let mut i = 0;
        while i < n {
            let ch = chars[i];
            if ch == '[' {
                // a character class: braces inside are literal
                let mut j = i + 1;
                if j < n && (chars[j] == '!' || chars[j] == '^') {
                    j += 1;
                }
                if j < n && chars[j] == ']' {
                    j += 1;
                }
                let close = (j..n).find(|&k| chars[k] == ']');
                i = match close {
                    Some(c) if c > 0 => c + 1,
                    _ => n,
                };
                continue;
            }
            if ch == '{' {
                if depth == 0 {
                    open_at = Some(i);
                }
                depth += 1;
            } else if ch == '}' && depth > 0 {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            i += 1;
        }
        match open_at {
            None => out.push(p),
            Some(_) if depth > 0 => return Err(Invalid(err("unclosed_brace").to_string())),
            Some(open) => {
                let body = &chars[open + 1..i];
                let (mut alts, mut level, mut start) = (Vec::new(), 0i32, 0usize);
                for (j, &ch) in body.iter().enumerate() {
                    if ch == '{' {
                        level += 1;
                    } else if ch == '}' {
                        level -= 1;
                    } else if ch == ',' && level == 0 {
                        alts.push(body[start..j].iter().collect::<String>());
                        start = j + 1;
                    }
                }
                alts.push(body[start..].iter().collect::<String>());
                let head: String = chars[..open].iter().collect();
                let tail: String = chars[i + 1..].iter().collect();
                for alt in alts.iter().rev() {
                    pending.push(format!("{head}{alt}{tail}"));
                }
            }
        }
        if out.len() + pending.len() > MAX_EXPANSION {
            return Err(invalid(err("too_many"), &[("limit", MAX_EXPANSION.into())]));
        }
    }
    Ok(out)
}

fn expand_all(patterns: &[String]) -> Result<Vec<String>, Invalid> {
    let mut out: Vec<String> = Vec::new();
    for p in patterns {
        for e in expand(p)? {
            if !out.contains(&e) {
                out.push(e);
            }
        }
    }
    Ok(out)
}

/// Segments of a glob, with `**` prepended unless it is anchored; and whether it is.
fn split_glob(pattern: &str) -> Result<(Vec<String>, bool), Invalid> {
    let p = pattern.trim().replace('\\', "/");
    if p.is_empty() {
        return Err(Invalid(err("empty_glob").into()));
    }
    if p.starts_with('~') {
        return Err(Invalid(err("tilde").into()));
    }
    let absolute = p.starts_with('/');
    let mut segs: Vec<String> = p
        .split('/')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    if absolute {
        segs.insert(0, String::new());
    }
    if segs.is_empty() || (segs.len() == 1 && segs[0].is_empty()) {
        return Err(Invalid(err("empty_glob").into()));
    }
    let anchored = absolute || segs[0].ends_with(':');
    let mut collapsed: Vec<String> = Vec::new();
    for seg in segs {
        if seg == "**" && collapsed.last().is_some_and(|l| l == "**") {
            continue;
        }
        collapsed.push(seg);
    }
    if !anchored && collapsed[0] != "**" {
        collapsed.insert(0, "**".into());
    }
    Ok((collapsed, anchored))
}

fn class_escape(body: &str) -> String {
    let mut out = String::new();
    for c in body.chars() {
        if "\\[]&~|^".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// (regex, literal characters, single-character wildcards) of one glob segment.
fn segment(seg: &str) -> Result<(String, usize, usize), Invalid> {
    let chars: Vec<char> = seg.chars().collect();
    let n = chars.len();
    let (mut out, mut lit, mut single) = (String::new(), 0, 0);
    let mut i = 0;
    while i < n {
        let ch = chars[i];
        if ch == '*' {
            while i < n && chars[i] == '*' {
                i += 1;
            }
            out.push_str("[^/]*");
            continue;
        }
        if ch == '?' {
            out.push_str("[^/]");
            single += 1;
            i += 1;
            continue;
        }
        if ch == '[' {
            let mut j = i + 1;
            let negate = j < n && (chars[j] == '!' || chars[j] == '^');
            if negate {
                j += 1;
            }
            let start = j;
            if j < n && chars[j] == ']' {
                j += 1; // a leading ] is literal
            }
            while j < n && chars[j] != ']' {
                j += 1;
            }
            if j >= n {
                return Err(Invalid(err("unclosed").into()));
            }
            let body: String = chars[start..j].iter().collect();
            out.push_str(if negate { "[^/" } else { "[" });
            out.push_str(&class_escape(&body));
            out.push(']');
            single += 1;
            i = j + 1;
            continue;
        }
        out.push_str(&regex::escape(&ch.to_string()));
        lit += 1;
        i += 1;
    }
    Ok((out, lit, single))
}

/// Regex for a split glob (matched whole against a `/`-joined path) and its specificity.
fn glob_regex(segs: &[String]) -> Result<(String, i64), Invalid> {
    let mut parts = String::new();
    let mut score = 0i64;
    let last = segs.len() - 1;
    for (k, seg) in segs.iter().enumerate() {
        if seg == "**" {
            parts.push_str(if k == last { ".*" } else { "(?:.*/)?" });
            continue;
        }
        let (rx, lit, single) = segment(seg)?;
        parts.push_str(&rx);
        if k != last {
            parts.push('/');
        }
        let whole = lit > 0 && single == 0 && lit == seg.chars().count();
        score += 2 * lit as i64 + single as i64 + if whole { 4 } else { 0 };
    }
    Ok((parts, score))
}

fn literal(seg: &str) -> bool {
    !seg.contains(['*', '?', '['])
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Anchor {
    Name(String, Option<String>),
    Parent(String),
    Ext(String),
    Prefix(String),
}

/// The index key of a pattern: its most selective literal part, or None (always tested).
fn anchor_of(segs: &[String]) -> Option<Anchor> {
    let last = segs.last()?;
    if last == "**" {
        return None;
    }
    let parent =
        (segs.len() >= 2 && segs[segs.len() - 2] != "**" && literal(&segs[segs.len() - 2]))
            .then(|| segs[segs.len() - 2].to_lowercase());
    if literal(last) {
        return Some(Anchor::Name(last.to_lowercase(), parent));
    }
    if let Some(p) = parent {
        return Some(Anchor::Parent(p));
    }
    if let Some(dot) = last.rfind('.') {
        let ext = &last[dot..];
        if literal(ext) && !ext.contains(']') {
            return Some(Anchor::Ext(ext.to_lowercase()));
        }
    }
    let cut = last
        .find(['*', '?', '['])
        .map_or(last.chars().count(), |b| last[..b].chars().count());
    if cut >= 3 {
        let low: String = last.chars().take(cut).collect::<String>().to_lowercase();
        return Some(Anchor::Prefix(low.chars().take(3).collect()));
    }
    None
}

/// A whole-path (or whole-name) pattern, compiled when first used, in both cases.
struct Lazy {
    source: String,
    cs: OnceLock<Regex>,
    ci: OnceLock<Regex>,
}

impl Lazy {
    fn new(alternatives: &[String]) -> Option<Lazy> {
        if alternatives.is_empty() {
            return None;
        }
        Some(Lazy {
            source: format!("^(?s:(?:{}))$", alternatives.join("|")),
            cs: OnceLock::new(),
            ci: OnceLock::new(),
        })
    }

    fn get(&self, fold: bool) -> &Regex {
        let (cell, flags) = if fold {
            (&self.ci, "(?i)")
        } else {
            (&self.cs, "")
        };
        cell.get_or_init(|| {
            Regex::new(&format!("{flags}{}", self.source))
                .unwrap_or_else(|_| Regex::new("^\\b\\B$").unwrap())
        })
    }

    fn full_match(&self, fold: bool, s: &str) -> bool {
        self.get(fold).is_match(s)
    }
}

// --------------------------------------------------------------------------- parsing

const TEXT_FIELDS: [&str; 4] = ["title", "reason", "note", "app"];
const RULE_KEYS: [&str; 19] = [
    "title",
    "reason",
    "note",
    "app",
    "paths",
    "names",
    "parents",
    "exclude",
    "platforms",
    "node",
    "min_size",
    "id",
    "kind",
    "cleanup",
    "command",
    "priority",
    "subtree",
    "enabled",
    "i18n",
];
const PLATFORMS: [&str; 3] = ["windows", "linux", "macos"];
const NODE_NAMES: [&str; 4] = ["file", "dir", "link", "other"];

static ID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._-]*$").unwrap());
static LANG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z]{2,3}(?:-[a-z0-9]{2,8})*$").unwrap());
static SIZE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^\s*(\d+(?:\.\d+)?)\s*([kmgtp]?)(?:i?b)?\s*$").unwrap());

fn get_str(raw: &Table, key: &str, default: Option<&str>) -> Result<String, Invalid> {
    match raw.get(key) {
        None => match default {
            Some(d) => Ok(d.trim().to_string()),
            None => Err(invalid(err("missing"), &[("key", key.into())])),
        },
        Some(Value::String(s)) => Ok(s.trim().to_string()),
        Some(_) => Err(type_error(key, "str")),
    }
}

fn get_strs(raw: &Table, key: &str) -> Result<Vec<String>, Invalid> {
    match raw.get(key) {
        None => Ok(Vec::new()),
        Some(Value::String(s)) => Ok(vec![s.trim().to_string()]),
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| match v {
                Value::String(s) => Ok(s.trim().to_string()),
                _ => Err(type_error(key, "strs")),
            })
            .collect(),
        Some(_) => Err(type_error(key, "strs")),
    }
}

fn choices(
    raw: &Table,
    key: &str,
    allowed: &[(&str, &[&'static str])],
) -> Result<HashSet<&'static str>, Invalid> {
    let values = get_strs(raw, key)?;
    if values.is_empty() {
        return Err(invalid(err("empty_choice"), &[("key", key.into())]));
    }
    let mut out = HashSet::new();
    for value in values {
        let lower = value.to_lowercase();
        let Some((_, names)) = allowed.iter().find(|(k, _)| *k == lower) else {
            let all: Vec<&str> = allowed.iter().map(|(k, _)| *k).collect();
            return Err(invalid_value(
                err("bad_choice"),
                &value,
                &[("key", key.into()), ("choices", all.join(" / ").into())],
            ));
        };
        out.extend(names.iter().copied());
    }
    Ok(out)
}

/// The value as Python would show it (for error messages).
fn show(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Boolean(b) => {
            if *b {
                "True".into()
            } else {
                "False".into()
            }
        }
        other => other.to_string(),
    }
}

/// Bytes from `1048576` or `"500 MB"` / `"2 GiB"` / `"10m"` (1 KB = 1024 B).
pub fn parse_size(value: &Value) -> Result<i64, String> {
    parse_size_inner(value).map_err(|e| e.0)
}

fn parse_size_inner(value: &Value) -> Result<i64, Invalid> {
    let bad = || match value {
        Value::String(s) => invalid_value(err("bad_size"), s, &[]),
        other => Invalid(fill(err("bad_size"), Some(&show(other)), &[])),
    };
    match value {
        Value::Integer(i) if *i >= 0 => Ok(*i),
        Value::String(s) => {
            let c = SIZE_RE.captures(s).ok_or_else(bad)?;
            let num: f64 = c[1].parse().map_err(|_| bad())?;
            let power = match c[2].to_lowercase().as_str() {
                "" => 0,
                "k" => 1,
                "m" => 2,
                "g" => 3,
                "t" => 4,
                _ => 5,
            };
            Ok((num * 1024f64.powi(power)) as i64)
        }
        _ => Err(bad()),
    }
}

fn parse_i18n(raw: &Table) -> Result<BTreeMap<String, BTreeMap<String, String>>, Invalid> {
    let table = match raw.get("i18n") {
        None => return Ok(BTreeMap::new()),
        Some(Value::Table(t)) => t,
        Some(_) => return Err(type_error("i18n", "table")),
    };
    let mut out = BTreeMap::new();
    for (lang, fields) in table {
        let tag = lang.replace('_', "-").to_lowercase();
        if !LANG_RE.is_match(&tag) {
            return Err(invalid_value(err("bad_lang"), lang, &[]));
        }
        let Value::Table(fields) = fields else {
            return Err(invalid(err("bad_i18n"), &[("lang", lang.into())]));
        };
        if fields.keys().any(|k| !TEXT_FIELDS.contains(&k.as_str())) {
            return Err(invalid(err("bad_i18n"), &[("lang", lang.into())]));
        }
        let mut texts = BTreeMap::new();
        for (field, value) in fields {
            let Value::String(s) = value else {
                return Err(type_error(&format!("i18n.{lang}.{field}"), "str"));
            };
            texts.insert(field.clone(), s.trim().to_string());
        }
        out.insert(tag, texts);
    }
    Ok(out)
}

/// (id, rule), or (id, None) for `enabled = false`.
fn parse_rule(raw: &Value, source: &str) -> Result<(String, Option<Rule>), Invalid> {
    let Value::Table(raw) = raw else {
        return Err(Invalid(err("not_table").into()));
    };
    let id = get_str(raw, "id", None)?;
    if !ID_RE.is_match(&id) {
        return Err(invalid_value(err("bad_id"), &id, &[]));
    }
    let enabled = match raw.get("enabled") {
        None => true,
        Some(Value::Boolean(b)) => *b,
        Some(_) => return Err(type_error("enabled", "bool")),
    };
    let mut unknown: Vec<&str> = raw
        .keys()
        .map(String::as_str)
        .filter(|k| !RULE_KEYS.contains(k))
        .collect();
    if !unknown.is_empty() {
        unknown.sort();
        return Err(invalid(
            err("unknown"),
            &[("keys", unknown.join(t("、")).into())],
        ));
    }
    if !enabled {
        return Ok((id, None));
    }
    let kind_s = get_str(raw, "kind", None)?.to_lowercase();
    let all_kinds: Vec<&str> = InsightKind::ALL.iter().map(|k| k.as_str()).collect();
    let kind = InsightKind::parse(&kind_s).ok_or_else(|| {
        invalid_value(
            err("bad_choice"),
            &kind_s,
            &[
                ("key", "kind".into()),
                ("choices", all_kinds.join(" / ").into()),
            ],
        )
    })?;
    let cleanup_s = get_str(raw, "cleanup", Some("review"))?.to_lowercase();
    let all_cleanups: Vec<&str> = Cleanup::ALL.iter().map(|k| k.as_str()).collect();
    let cleanup = Cleanup::parse(&cleanup_s).ok_or_else(|| {
        invalid_value(
            err("bad_choice"),
            &cleanup_s,
            &[
                ("key", "cleanup".into()),
                ("choices", all_cleanups.join(" / ").into()),
            ],
        )
    })?;
    let title = get_str(raw, "title", None)?;
    if title.is_empty() {
        return Err(invalid(err("missing"), &[("key", "title".into())]));
    }
    let reason = get_str(raw, "reason", None)?;
    if reason.is_empty() {
        return Err(invalid(err("missing"), &[("key", "reason".into())]));
    }
    let note = get_str(raw, "note", Some(""))?;
    let text = RuleText {
        note: if note.is_empty() { title.clone() } else { note },
        app: get_str(raw, "app", Some(""))?,
        title,
        reason,
    };
    let names = expand_all(&get_strs(raw, "names")?)?;
    let parents = expand_all(&get_strs(raw, "parents")?)?;
    for value in names.iter().chain(&parents) {
        if value.is_empty() || value.contains(['/', '\\']) {
            return Err(invalid_value(err("name_sep"), value, &[]));
        }
        let checked = segment(value).and_then(|(rx, _, _)| {
            Regex::new(&format!("^(?s:{rx})$"))
                .map(|_| ())
                .map_err(|e| Invalid(e.to_string()))
        });
        if let Err(e) = checked {
            return Err(invalid_value(
                err("bad_glob"),
                value,
                &[("error", e.0.into())],
            ));
        }
    }
    let mut all = get_strs(raw, "paths")?;
    all.extend(names.iter().map(|n| format!("**/{n}")));
    let patterns = expand_all(&all)?;
    if patterns.is_empty() {
        return Err(Invalid(err("no_matcher").into()));
    }
    let exclude = expand_all(&get_strs(raw, "exclude")?)?;
    for value in patterns.iter().chain(&exclude) {
        let checked = split_glob(value)
            .and_then(|(segs, _)| glob_regex(&segs))
            .and_then(|(rx, _)| {
                Regex::new(&format!("^(?s:{rx})$"))
                    .map(|_| ())
                    .map_err(|e| Invalid(e.to_string()))
            });
        if let Err(e) = checked {
            return Err(invalid_value(
                err("bad_glob"),
                value,
                &[("error", e.0.into())],
            ));
        }
    }
    let platform_aliases: &[(&str, &[&'static str])] = &[
        ("windows", &["windows"]),
        ("linux", &["linux"]),
        ("macos", &["macos"]),
        ("posix", &["linux", "macos"]),
        ("all", &PLATFORMS),
    ];
    let platforms = if raw.contains_key("platforms") {
        choices(raw, "platforms", platform_aliases)?
    } else {
        PLATFORMS.into_iter().collect()
    };
    let node_choices: &[(&str, &[&'static str])] = &[
        ("file", &["file"]),
        ("dir", &["dir"]),
        ("link", &["link"]),
        ("other", &["other"]),
        ("any", &NODE_NAMES),
    ];
    let nodes = if raw.contains_key("node") {
        choices(raw, "node", node_choices)?
    } else {
        NODE_NAMES.into_iter().collect()
    };
    let min_size = parse_size_inner(raw.get("min_size").unwrap_or(&Value::Integer(0)))?;
    let priority = match raw.get("priority") {
        None => 0,
        Some(Value::Integer(i)) => *i,
        Some(_) => return Err(type_error("priority", "int")),
    };
    if !(-1000..=1000).contains(&priority) {
        return Err(Invalid(err("bad_priority").into()));
    }
    let subtree = match raw.get("subtree") {
        None => true,
        Some(Value::Boolean(b)) => *b,
        Some(_) => return Err(type_error("subtree", "bool")),
    };
    let rule = Rule {
        id: id.clone(),
        kind,
        text,
        i18n: parse_i18n(raw)?,
        cleanup,
        command: get_str(raw, "command", Some(""))?,
        patterns,
        parents,
        exclude,
        platforms,
        nodes,
        min_size,
        priority,
        subtree,
        source: source.to_string(),
    };
    Ok((id, Some(rule)))
}

type Parsed = (Vec<(String, Option<Rule>)>, Vec<RuleProblem>);

fn parse_file(text: &str, source: &str) -> Parsed {
    let mut problems = Vec::new();
    let file_problem = |message: String| RuleProblem {
        source: source.to_string(),
        rule: String::new(),
        message,
    };
    let data: Table = match text.parse::<Table>() {
        Ok(d) => d,
        Err(e) => {
            let detail = e.to_string().trim().replace('\n', " ");
            let m = format(
                msg("file_syntax"),
                &[("source", source.into()), ("error", detail.into())],
            );
            return (Vec::new(), vec![file_problem(m)]);
        }
    };
    let version = data
        .get("version")
        .cloned()
        .unwrap_or(Value::Integer(FORMAT_VERSION));
    let ok_version = match &version {
        Value::Integer(i) => *i == FORMAT_VERSION,
        Value::Float(f) => *f == FORMAT_VERSION as f64,
        _ => false,
    };
    if !ok_version {
        let m = format(
            msg("file_version"),
            &[
                ("source", source.into()),
                ("version", show(&version).into()),
                ("supported", FORMAT_VERSION.into()),
            ],
        );
        return (Vec::new(), vec![file_problem(m)]);
    }
    let mut extra: Vec<&str> = data
        .keys()
        .map(String::as_str)
        .filter(|k| *k != "version" && *k != "rule")
        .collect();
    if !extra.is_empty() {
        extra.sort();
        let m = format(
            msg("file_keys"),
            &[
                ("source", source.into()),
                ("keys", extra.join(t("、")).into()),
            ],
        );
        problems.push(file_problem(m));
    }
    let entries = match data.get("rule") {
        None => Vec::new(),
        Some(Value::Array(a)) => a.clone(),
        Some(_) => {
            problems.push(file_problem(format(
                msg("file_rules"),
                &[("source", source.into())],
            )));
            return (Vec::new(), problems);
        }
    };
    let mut parsed = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for (n, raw) in entries.iter().enumerate() {
        let label = match raw {
            Value::Table(t) => match t.get("id") {
                Some(Value::String(s)) => s.clone(),
                _ => format!("#{}", n + 1),
            },
            _ => format!("#{}", n + 1),
        };
        match parse_rule(raw, source) {
            Err(e) => {
                let m = format(
                    msg("rule_skipped"),
                    &[
                        ("rule", label.clone().into()),
                        ("source", source.into()),
                        ("error", e.0.into()),
                    ],
                );
                problems.push(RuleProblem {
                    source: source.to_string(),
                    rule: label,
                    message: m,
                });
            }
            Ok((id, rule)) => {
                if seen.contains(&id) {
                    let m = format(
                        msg("rule_duplicate"),
                        &[("rule", id.clone().into()), ("source", source.into())],
                    );
                    problems.push(RuleProblem {
                        source: source.to_string(),
                        rule: id.clone(),
                        message: m,
                    });
                }
                seen.insert(id.clone());
                parsed.push((id, rule));
            }
        }
    }
    (parsed, problems)
}

// --------------------------------------------------------------------------- the compiled rule set

const WIN: u32 = 1;
const LINUX: u32 = 2;
const MAC: u32 = 4;
const FOLD: u32 = WIN | MAC; // case-insensitive file systems by default

fn pmask_of(platform: &str) -> u32 {
    match platform {
        "windows" => WIN,
        "linux" => LINUX,
        _ => MAC,
    }
}

fn kind_bit(kind: Kind) -> u32 {
    1 << (kind as u32)
}

fn name_bit(name: &str) -> u32 {
    match name {
        "file" => kind_bit(Kind::File),
        "dir" => kind_bit(Kind::Dir),
        "link" => kind_bit(Kind::Link),
        _ => kind_bit(Kind::Other),
    }
}

/// Per-rule filters, precomputed for the lookup loop.
struct Compiled {
    rule: Rule,
    pmask: u32,
    nmask: u32,
    min_size: i64,
    prio: i64,
    parents: Option<Lazy>,
    exclude: Option<Lazy>,
}

struct Matcher {
    /// index into `RuleSet::compiled`
    rule: usize,
    pattern: Lazy,
    anchor: Option<Anchor>,
    specificity: i64,
    order: usize,
    rank: usize,
}

/// (matcher, index of the path segment it is tested at)
type Cand = (usize, usize);

/// Matchers keyed by their anchor.
#[derive(Default)]
struct Index {
    name: HashMap<String, HashMap<Option<String>, Vec<usize>>>,
    parent: HashMap<String, Vec<usize>>,
    ext: HashMap<String, Vec<usize>>,
    prefix: HashMap<String, Vec<usize>>,
    generic: Vec<usize>,
}

impl Index {
    fn add(&mut self, m: usize, anchor: &Option<Anchor>) {
        match anchor {
            None => self.generic.push(m),
            Some(Anchor::Name(k, p)) => self
                .name
                .entry(k.clone())
                .or_default()
                .entry(p.clone())
                .or_default()
                .push(m),
            Some(Anchor::Parent(k)) => self.parent.entry(k.clone()).or_default().push(m),
            Some(Anchor::Ext(k)) => self.ext.entry(k.clone()).or_default().push(m),
            Some(Anchor::Prefix(k)) => self.prefix.entry(k.clone()).or_default().push(m),
        }
    }
}

/// A path prepared for lookups.
struct PathInfo {
    /// `/`-separated, no trailing separator
    norm: String,
    segs: Vec<String>,
    low: Vec<String>,
    /// byte offset where segment i ends: `norm[..ends[i]]` is the path of segment i
    ends: Vec<usize>,
    pmask: u32,
    win: bool,
}

impl PathInfo {
    fn upto(&self, i: usize) -> &str {
        &self.norm[..self.ends[i]]
    }
}

/// Subtree matches on the folders containing an item, shared by siblings in `match_many`.
struct Ancestors {
    /// sorted by cand_key
    cands: Vec<Cand>,
    best: Option<Cand>,
    /// rule → its shallowest match
    outer: HashMap<usize, Option<Cand>>,
}

static DRIVE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z]:(?:[\\/]|$)").unwrap());

/// This system's platform name (`windows` / `linux` / `macos`).
pub fn host_platform() -> &'static str {
    if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

/// The node kinds of a [`RuleSet::match_many`] call.
#[derive(Clone, Copy, Debug)]
pub enum Kinds<'a> {
    One(Kind),
    Each(&'a [Kind]),
}

/// Compiled rules: [`RuleSet::match_path`] / [`RuleSet::match_many`] / [`RuleSet::match_node`].
pub struct RuleSet {
    compiled: Vec<Compiled>,
    pub problems: Vec<RuleProblem>,
    pub host: String,
    by_id: HashMap<String, usize>,
    win_host: bool,
    posix_mask: u32,
    matchers: Vec<Matcher>,
    own: Index,
    sub: Index,
    /// patterns tested on every lookup (keep at 0)
    pub unanchored: usize,
}

impl std::fmt::Debug for RuleSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuleSet")
            .field("rules", &self.compiled.len())
            .field("host", &self.host)
            .field("problems", &self.problems.len())
            .finish()
    }
}

impl RuleSet {
    /// `rules` in tie-break order (earlier wins on equal priority, depth and specificity);
    /// `host` (`windows` / `linux` / `macos`, default: this one) decides how POSIX-form paths
    /// are matched.
    pub fn new(
        rules: Vec<Rule>,
        problems: Vec<RuleProblem>,
        host: Option<&str>,
    ) -> Result<RuleSet, InsightsError> {
        let host = host.map_or_else(|| host_platform().to_string(), str::to_string);
        if !PLATFORMS.contains(&host.as_str()) {
            return Err(InsightsError::Host(host));
        }
        let win_host = host == "windows";
        let posix_mask = if win_host {
            LINUX | MAC
        } else {
            pmask_of(&host)
        };
        let mut compiled: Vec<Compiled> = Vec::with_capacity(rules.len());
        let mut matchers: Vec<Matcher> = Vec::new();
        for (order, rule) in rules.into_iter().enumerate() {
            let parents: Vec<String> = rule
                .parents
                .iter()
                .filter_map(|n| segment(n).ok().map(|s| s.0))
                .collect();
            let exclude: Vec<String> = rule
                .exclude
                .iter()
                .filter_map(|p| {
                    split_glob(p)
                        .and_then(|(s, _)| glob_regex(&s))
                        .ok()
                        .map(|r| r.0)
                })
                .collect();
            for pattern in &rule.patterns {
                let Ok((segs, anchored)) = split_glob(pattern) else {
                    continue;
                };
                let Ok((rx, score)) = glob_regex(&segs) else {
                    continue;
                };
                matchers.push(Matcher {
                    rule: order,
                    pattern: Lazy::new(&[rx]).unwrap(),
                    anchor: anchor_of(&segs),
                    specificity: score + if anchored { 8 } else { 0 },
                    order,
                    rank: 0,
                });
            }
            compiled.push(Compiled {
                pmask: rule
                    .platforms
                    .iter()
                    .map(|p| pmask_of(p))
                    .fold(0, |a, b| a | b),
                nmask: rule.nodes.iter().map(|n| name_bit(n)).fold(0, |a, b| a | b),
                min_size: rule.min_size,
                prio: -rule.priority,
                parents: Lazy::new(&parents),
                exclude: Lazy::new(&exclude),
                rule,
            });
        }
        matchers.sort_by_key(|m| (compiled[m.rule].prio, -m.specificity, m.order));
        let (mut own, mut sub) = (Index::default(), Index::default());
        for (rank, m) in matchers.iter_mut().enumerate() {
            m.rank = rank;
            own.add(rank, &m.anchor);
            let c = &compiled[m.rule];
            if c.rule.subtree && c.nmask & kind_bit(Kind::Dir) != 0 {
                sub.add(rank, &m.anchor);
            }
        }
        let by_id = compiled
            .iter()
            .enumerate()
            .map(|(i, c)| (c.rule.id.clone(), i))
            .collect();
        Ok(RuleSet {
            unanchored: own.generic.len(),
            compiled,
            problems,
            host,
            by_id,
            win_host,
            posix_mask,
            matchers,
            own,
            sub,
        })
    }

    pub fn len(&self) -> usize {
        self.compiled.len()
    }

    pub fn is_empty(&self) -> bool {
        self.compiled.is_empty()
    }

    /// The rules in tie-break order.
    pub fn rules(&self) -> impl Iterator<Item = &Rule> {
        self.compiled.iter().map(|c| &c.rule)
    }

    pub fn get(&self, id: &str) -> Option<&Rule> {
        self.by_id.get(id).map(|&i| &self.compiled[i].rule)
    }

    // ------------------------------------------------------------------ lookups

    /// The insight for `path` (a `kind` node; `size` in bytes enables `min_size`), with texts in
    /// `lang` (None: the interface language; `Some("")`: the rule's own texts).
    pub fn match_path(
        &self,
        path: &str,
        kind: Kind,
        size: Option<i64>,
        lang: Option<&str>,
    ) -> Option<Insight> {
        let p = self.prepare(path);
        let hit = self.find(&p, kind_bit(kind), size, None)?;
        Some(self.insight(&p, hit, &text_language(lang)))
    }

    /// [`RuleSet::match_path`] over a list; siblings share the work on their common ancestors.
    pub fn match_many<S: AsRef<str>>(
        &self,
        paths: &[S],
        kinds: Kinds<'_>,
        sizes: Option<&[Option<i64>]>,
        lang: Option<&str>,
    ) -> Result<Vec<Option<Insight>>, InsightsError> {
        let n = paths.len();
        if let Kinds::Each(k) = kinds
            && k.len() != n
        {
            return Err(InsightsError::Length("kinds"));
        }
        if sizes.is_some_and(|s| s.len() != n) {
            return Err(InsightsError::Length("sizes"));
        }
        let lang = text_language(lang);
        let mut cache: HashMap<(u32, String), Ancestors> = HashMap::new();
        Ok(paths
            .iter()
            .enumerate()
            .map(|(j, path)| {
                let kind = match kinds {
                    Kinds::One(k) => k,
                    Kinds::Each(k) => k[j],
                };
                let p = self.prepare(path.as_ref());
                let size = sizes.and_then(|s| s[j]);
                self.find(&p, kind_bit(kind), size, Some(&mut cache))
                    .map(|hit| self.insight(&p, hit, &lang))
            })
            .collect())
    }

    /// [`RuleSet::match_path`] for a store node (its path, kind and allocated size).
    pub fn match_node(&self, nodes: &Nodes, node: NodeId, lang: Option<&str>) -> Option<Insight> {
        let kind = nodes.kind(node);
        let size = if kind == Kind::Dir {
            nodes.tot_alloc(node)
        } else {
            nodes.alloc(node)
        };
        self.match_path(&nodes.path_lossy(node), kind, Some(size), lang)
    }

    // ------------------------------------------------------------------ internals

    fn prepare(&self, path: &str) -> PathInfo {
        let mut p = path.to_string();
        let win;
        if p.starts_with("\\\\") {
            win = true;
            if p.get(2..4).is_some_and(|s| s == "?\\" || s == ".\\") {
                p = p[4..].to_string(); // \\?\C:\… and \\?\UNC\server\share\…
                if p.get(..4).is_some_and(|s| s.eq_ignore_ascii_case("UNC\\")) {
                    p = format!("\\\\{}", &p[4..]);
                }
            }
        } else {
            win = DRIVE.is_match(&p) || (self.win_host && !p.starts_with('/'));
        }
        if win {
            p = p.replace('\\', "/");
        }
        if p.len() > 1 && p.ends_with('/') {
            let trimmed = p.trim_end_matches('/');
            p = if trimmed.is_empty() {
                "/".into()
            } else {
                trimmed.to_string()
            };
        }
        let segs: Vec<String> = p.split('/').map(String::from).collect();
        let mut ends = Vec::with_capacity(segs.len());
        let mut at = 0;
        for (i, s) in segs.iter().enumerate() {
            at += s.len() + usize::from(i > 0);
            ends.push(at);
        }
        let low = p
            .to_lowercase()
            .split('/')
            .map(String::from)
            .collect::<Vec<_>>();
        // lower-casing may change byte lengths: keep segments aligned with `segs`
        let low = if low.len() == segs.len() {
            low
        } else {
            segs.iter().map(|s| s.to_lowercase()).collect()
        };
        PathInfo {
            norm: p,
            segs,
            low,
            ends,
            pmask: if win { WIN } else { self.posix_mask },
            win,
        }
    }

    fn collect(
        &self,
        index: &Index,
        low: &[String],
        start: usize,
        stop: usize,
        pmask: u32,
        out: &mut Vec<Cand>,
    ) {
        let push = |list: &[usize], i: usize, out: &mut Vec<Cand>| {
            for &m in list {
                if self.compiled[self.matchers[m].rule].pmask & pmask != 0 {
                    out.push((m, i));
                }
            }
        };
        let mut prev: Option<&str> = if start > 0 {
            Some(&low[start - 1])
        } else {
            None
        };
        for (i, seg) in low.iter().enumerate().take(stop).skip(start) {
            if let Some(buckets) = index.name.get(seg) {
                if let Some(found) = buckets.get(&None) {
                    push(found, i, out);
                }
                if let Some(p) = prev
                    && let Some(found) = buckets.get(&Some(p.to_string()))
                {
                    push(found, i, out);
                }
            }
            if let Some(p) = prev
                && let Some(found) = index.parent.get(p)
            {
                push(found, i, out);
            }
            if let Some(dot) = seg.rfind('.')
                && let Some(found) = index.ext.get(&seg[dot..])
            {
                push(found, i, out);
            }
            let first3: String = seg.chars().take(3).collect();
            if let Some(found) = index.prefix.get(&first3) {
                push(found, i, out);
            }
            if !index.generic.is_empty() {
                push(&index.generic, i, out);
            }
            prev = Some(seg);
        }
    }

    /// priority, then depth, then specificity / order
    fn cand_key(&self, c: Cand) -> (i64, i64, usize) {
        let m = &self.matchers[c.0];
        (self.compiled[m.rule].prio, -(c.1 as i64), m.rank)
    }

    fn accepts(&self, c: Cand, p: &PathInfo, kbit: u32, size: Option<i64>) -> bool {
        let (m, i) = c;
        let m = &self.matchers[m];
        let r = &self.compiled[m.rule];
        let pm = r.pmask & p.pmask;
        if pm == 0 || r.nmask & kbit == 0 {
            return false;
        }
        if size.is_some_and(|s| s < r.min_size) {
            return false;
        }
        let fold = pm & FOLD != 0;
        let prefix = p.upto(i);
        if !m.pattern.full_match(fold, prefix) {
            return false;
        }
        if let Some(parents) = &r.parents
            && (i == 0 || !parents.full_match(fold, &p.segs[i - 1]))
        {
            return false;
        }
        r.exclude
            .as_ref()
            .is_none_or(|x| !x.full_match(fold, prefix))
    }

    /// Subtree matches among segments `0 .. upto` (the folders containing the item).
    fn ancestors(&self, p: &PathInfo, upto: usize) -> Ancestors {
        let mut cands = Vec::new();
        self.collect(&self.sub, &p.low, 0, upto, p.pmask, &mut cands);
        let mut best = None;
        if !cands.is_empty() {
            cands.sort_by_key(|&c| self.cand_key(c));
            best = cands
                .iter()
                .copied()
                .find(|&c| self.accepts(c, p, kind_bit(Kind::Dir), None));
        }
        Ancestors {
            cands,
            best,
            outer: HashMap::new(),
        }
    }

    /// The shallowest ancestor match of `rule` (memoised on `anc`).
    fn outermost(&self, anc: &mut Ancestors, rule: usize, p: &PathInfo) -> Option<Cand> {
        if let Some(hit) = anc.outer.get(&rule) {
            return *hit;
        }
        let mut mine: Vec<Cand> = anc
            .cands
            .iter()
            .copied()
            .filter(|c| self.matchers[c.0].rule == rule)
            .collect();
        mine.sort_by_key(|c| (c.1, self.matchers[c.0].rank));
        let hit = mine
            .into_iter()
            .find(|&c| self.accepts(c, p, kind_bit(Kind::Dir), None));
        anc.outer.insert(rule, hit);
        hit
    }

    fn find(
        &self,
        p: &PathInfo,
        kbit: u32,
        size: Option<i64>,
        cache: Option<&mut HashMap<(u32, String), Ancestors>>,
    ) -> Option<Cand> {
        let last = p.segs.len() - 1;
        let mut local;
        let anc: &mut Ancestors = match cache {
            Some(cache) => {
                let key = (
                    p.pmask,
                    if last > 0 {
                        p.upto(last - 1).to_string()
                    } else {
                        String::new()
                    },
                );
                cache.entry(key).or_insert_with(|| self.ancestors(p, last))
            }
            None => {
                local = self.ancestors(p, last);
                &mut local
            }
        };
        let mut best = anc.best;
        let mut cands = Vec::new();
        self.collect(&self.own, &p.low, last, last + 1, p.pmask, &mut cands);
        if !cands.is_empty() {
            cands.sort_by_key(|&c| self.cand_key(c));
            let bound = best.map(|b| self.cand_key(b));
            for c in cands {
                if bound.is_some_and(|b| self.cand_key(c) > b) {
                    break;
                }
                if self.accepts(c, p, kbit, size) {
                    best = Some(c);
                    break;
                }
            }
        }
        let best = best?;
        if anc.cands.is_empty() {
            return Some(best);
        }
        // the same rule further up: the container
        let outer = self.outermost(anc, self.matchers[best.0].rule, p);
        match outer {
            Some(o) if o.1 < best.1 => Some(o),
            _ => Some(best),
        }
    }

    fn native(&self, p: &PathInfo, i: usize) -> String {
        let prefix = p.upto(i);
        if !p.win {
            return if prefix.is_empty() {
                "/".into()
            } else {
                prefix.to_string()
            };
        }
        let prefix = prefix.replace('/', "\\");
        if prefix.ends_with(':') {
            format!("{prefix}\\")
        } else {
            prefix
        }
    }

    fn insight(&self, p: &PathInfo, hit: Cand, lang: &str) -> Insight {
        let (m, i) = hit;
        let rule = &self.compiled[self.matchers[m].rule].rule;
        let t = rule.texts(Some(lang));
        Insight {
            rule: rule.id.clone(),
            title: t.title,
            reason: t.reason,
            kind: rule.kind,
            note: t.note,
            app: t.app,
            cleanup: rule.cleanup,
            command: rule.command.clone(),
            path: self.native(p, i),
            inside: i < p.segs.len() - 1,
        }
    }
}

/// The language of a lookup's texts: `lang`, else the interface language.
fn text_language(lang: Option<&str>) -> String {
    match lang {
        Some(l) => l.to_string(),
        None => crate::i18n::language().as_str().to_string(),
    }
}

// --------------------------------------------------------------------------- loading

/// A rule set from TOML texts `(source name, text)`; a rule in a later source replaces the
/// earlier rule with the same id (and wins ties), `enabled = false` removes it. Problems are
/// logged as warnings and kept in [`RuleSet::problems`].
pub fn compile_rules(
    sources: &[(String, String)],
    host: Option<&str>,
    problems: Vec<RuleProblem>,
) -> Result<RuleSet, InsightsError> {
    let mut merged: HashMap<String, (usize, usize, Rule)> = HashMap::new();
    let mut found = problems;
    for (s_idx, (source, text)) in sources.iter().enumerate() {
        let (parsed, probs) = parse_file(text, source);
        found.extend(probs);
        for (pos, (id, rule)) in parsed.into_iter().enumerate() {
            let previous = merged.remove(&id);
            if let Some(prev) = &previous
                && prev.0 != s_idx
                && rule.is_some()
            {
                tracing::debug!(
                    "{}",
                    format(
                        msg("rule_override"),
                        &[("rule", id.clone().into()), ("source", source.into())]
                    )
                );
            }
            if let Some(rule) = rule {
                merged.insert(id, (s_idx, pos, rule));
            }
        }
    }
    for problem in &found {
        tracing::warn!("{}", problem.message);
    }
    let mut ordered: Vec<(usize, usize, Rule)> = merged.into_values().collect();
    ordered.sort_by_key(|e| (std::cmp::Reverse(e.0), e.1));
    let skipped = found.len();
    let set = RuleSet::new(ordered.into_iter().map(|e| e.2).collect(), found, host)?;
    tracing::debug!(
        "{}",
        format(
            msg("rules_loaded"),
            &[
                ("count", set.len().into()),
                ("files", sources.len().into()),
                ("skipped", skipped.into())
            ]
        )
    );
    Ok(set)
}

/// The built-in rules' TOML text.
pub fn builtin_text() -> &'static str {
    BUILTIN_TEXT
}

/// Where the user's own rule files live (`*.toml`; `SPACEKNIFE_HOME` relocates it).
pub fn user_rules_dir() -> PathBuf {
    crate::paths::config_dir().join("insights")
}

static ACTIVE: RwLock<Option<Arc<RuleSet>>> = RwLock::new(None);

/// Built-in rules plus every `*.toml` in `extra_dir` (by file name; user rules override
/// built-ins by id), made the active set for the module-level lookups.
pub fn load_rules(
    extra_dir: Option<&Path>,
    host: Option<&str>,
) -> Result<Arc<RuleSet>, InsightsError> {
    let mut sources: Vec<(String, String)> =
        vec![(BUILTIN_SOURCE.to_string(), BUILTIN_TEXT.to_string())];
    let mut problems = Vec::new();
    if let Some(dir) = extra_dir {
        let mut files: Vec<PathBuf> = match std::fs::read_dir(dir) {
            Ok(it) => it
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "toml") && p.is_file())
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                let source = dir.to_string_lossy().into_owned();
                let m = format(
                    msg("file_unreadable"),
                    &[
                        ("source", source.clone().into()),
                        ("error", e.to_string().into()),
                    ],
                );
                problems.push(RuleProblem {
                    source,
                    rule: String::new(),
                    message: m,
                });
                Vec::new()
            }
        };
        files.sort_by_key(|f| f.file_name().map(|n| n.to_os_string()));
        for f in files {
            let source = f.to_string_lossy().into_owned();
            match std::fs::read(&f).map(String::from_utf8) {
                Ok(Ok(text)) => sources.push((source, text)),
                Ok(Err(e)) => {
                    let m = format(
                        msg("file_unreadable"),
                        &[
                            ("source", source.clone().into()),
                            ("error", e.to_string().into()),
                        ],
                    );
                    problems.push(RuleProblem {
                        source,
                        rule: String::new(),
                        message: m,
                    });
                }
                Err(e) => {
                    let m = format(
                        msg("file_unreadable"),
                        &[
                            ("source", source.clone().into()),
                            ("error", e.to_string().into()),
                        ],
                    );
                    problems.push(RuleProblem {
                        source,
                        rule: String::new(),
                        message: m,
                    });
                }
            }
        }
    }
    let set = Arc::new(compile_rules(&sources, host, problems)?);
    *ACTIVE.write() = Some(set.clone());
    Ok(set)
}

/// The active rule set (the built-in rules until [`load_rules`] is called).
pub fn active_rules() -> Arc<RuleSet> {
    if let Some(set) = ACTIVE.read().as_ref() {
        return set.clone();
    }
    let mut slot = ACTIVE.write();
    if let Some(set) = slot.as_ref() {
        return set.clone();
    }
    let set = Arc::new(
        compile_rules(
            &[(BUILTIN_SOURCE.to_string(), BUILTIN_TEXT.to_string())],
            None,
            Vec::new(),
        )
        .expect("the host platform is known"),
    );
    *slot = Some(set.clone());
    set
}

/// Forget the active rule set (the next lookup loads the built-in rules again).
pub fn reset_active() {
    *ACTIVE.write() = None;
}

pub fn match_path(
    path: &str,
    kind: Kind,
    size: Option<i64>,
    lang: Option<&str>,
) -> Option<Insight> {
    active_rules().match_path(path, kind, size, lang)
}

pub fn match_many<S: AsRef<str>>(
    paths: &[S],
    kinds: Kinds<'_>,
    sizes: Option<&[Option<i64>]>,
    lang: Option<&str>,
) -> Result<Vec<Option<Insight>>, InsightsError> {
    active_rules().match_many(paths, kinds, sizes, lang)
}

pub fn match_node(nodes: &Nodes, node: NodeId, lang: Option<&str>) -> Option<Insight> {
    active_rules().match_node(nodes, node, lang)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn braces_expand() {
        assert_eq!(expand("a/{b,c/d}").unwrap(), ["a/b", "a/c/d"]);
        assert_eq!(expand("[{]a}").unwrap(), ["[{]a}"]);
        assert!(expand("foo{a,b").is_err());
    }

    #[test]
    fn anchors() {
        let (segs, _) = split_glob("Code/Cache").unwrap();
        assert_eq!(
            anchor_of(&segs),
            Some(Anchor::Name("cache".into(), Some("code".into())))
        );
        let (segs, _) = split_glob("*.tar.gz").unwrap();
        assert_eq!(anchor_of(&segs), Some(Anchor::Ext(".gz".into())));
        let (segs, _) = split_glob("pre*").unwrap();
        assert_eq!(anchor_of(&segs), Some(Anchor::Prefix("pre".into())));
    }
}
