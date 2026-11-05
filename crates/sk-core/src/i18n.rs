//! Interface languages (M7.5): Simplified Chinese (the source text) and English.
//!
//! The Chinese text in the code is the message id, gettext style: [`t`]`("开始扫描")` returns it as
//! is in Chinese and looks it up in the English catalog otherwise. The catalog is the JSON files
//! under `crates/sk-core/locales/en/` (one per area, merged; embedded in the binary):
//! `{"中文原文": "English"}`. Placeholders are `{name}` fields (with an optional Python-style format
//! spec, `{n:.1f}`, `{code:#x}`) and stay the same in both languages: `tr!("已导出 {n} 项", n = 3)`.
//!
//! Translate when the text is *shown*, never at start-up: the language can change while the backend
//! runs (setting 「界面语言」).
//!
//! The language: setting `general.language` = `system` | `zh-CN` | `en`; `system` resolves with
//! [`system_language`] — `SPACEKNIFE_LANGUAGE` (tests, forced), then `SPACEKNIFE_SYSTEM_LOCALE` (the
//! launcher passes the OS UI language it sees), then the platform: Windows
//! `GetUserDefaultUILanguage`, macOS `AppleLanguages`, then `LANGUAGE` / `LC_ALL` / `LC_MESSAGES` /
//! `LANG`. Any Chinese locale gives Simplified Chinese, everything else English.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU8, Ordering};

use parking_lot::Mutex;
pub use sk_protocol::settings::Language;

pub const SOURCE: Language = Language::ZhCn;
pub const LANGUAGES: [Language; 2] = [Language::ZhCn, Language::En];

/// The English catalog files, embedded (merged in this order: later files win).
const EN_FILES: &[(&str, &str)] = &[
    ("cli.json", include_str!("../locales/en/cli.json")),
    ("core.json", include_str!("../locales/en/core.json")),
    ("settings.json", include_str!("../locales/en/settings.json")),
    ("update.json", include_str!("../locales/en/update.json")),
];

static LANGUAGE: AtomicU8 = AtomicU8::new(0); // 0 zh-CN, 1 en
static SYSTEM: Mutex<Option<Language>> = Mutex::new(None);
static MISSING: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Default::default);

fn lang_code(l: Language) -> u8 {
    match l {
        Language::ZhCn => 0,
        Language::En => 1,
    }
}

/// A locale tag (`zh_CN.UTF-8`, `zh-Hans`, `en-US`, `C`) -> a supported language.
pub fn normalize(tag: Option<&str>) -> Option<Language> {
    let tag = tag?;
    let t = tag.trim().replace('_', "-");
    let t = t.split('.').next().unwrap_or("");
    let t = t.split('@').next().unwrap_or("").to_lowercase();
    if t.is_empty() || t == "c" || t == "posix" {
        return None;
    }
    Some(if t.starts_with("zh") { Language::ZhCn } else { Language::En })
}

#[cfg(windows)]
fn windows_ui_language() -> Option<Language> {
    // SAFETY: no arguments, no failure mode.
    let langid = unsafe { windows_sys::Win32::Globalization::GetUserDefaultUILanguage() };
    Some(if (langid & 0x3FF) == 0x04 { Language::ZhCn } else { Language::En }) // LANG_CHINESE
}

#[cfg(not(windows))]
fn windows_ui_language() -> Option<Language> {
    None
}

#[cfg(target_os = "macos")]
fn mac_language() -> Option<Language> {
    let out = std::process::Command::new("defaults")
        .args(["read", "-g", "AppleLanguages"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).replace(['(', ')', ','], " ");
    text.split_whitespace()
        .find_map(|tok| normalize(Some(tok.trim_matches('"'))))
}

#[cfg(not(target_os = "macos"))]
fn mac_language() -> Option<Language> {
    None
}

fn env_language(var: &str) -> Option<Language> {
    normalize(std::env::var(var).ok().as_deref())
}

/// The operating system's interface language, reduced to a supported one (cached; see
/// [`clear_system_language_cache`]).
pub fn system_language() -> Language {
    if let Some(l) = *SYSTEM.lock() {
        return l;
    }
    let l = detect_system_language();
    *SYSTEM.lock() = Some(l);
    l
}

/// Forget the cached [`system_language`] (tests, or after the launcher changed the environment).
pub fn clear_system_language_cache() {
    *SYSTEM.lock() = None;
}

fn detect_system_language() -> Language {
    for var in ["SPACEKNIFE_LANGUAGE", "SPACEKNIFE_SYSTEM_LOCALE"] {
        if let Some(l) = env_language(var) {
            return l;
        }
    }
    if let Some(l) = windows_ui_language().or_else(mac_language) {
        return l;
    }
    for var in ["LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(value) = std::env::var(var)
            && !value.is_empty()
            && let Some(l) = normalize(value.split(':').next())
        {
            return l;
        }
    }
    Language::En
}

/// The value of `general.language` -> the language to use.
pub fn resolve(setting: Option<&str>) -> Language {
    if std::env::var("SPACEKNIFE_LANGUAGE").is_ok_and(|v| !v.is_empty()) {
        return system_language(); // forced (tests, diagnostics)
    }
    match setting {
        Some(s) if !s.is_empty() && s != "system" => normalize(Some(s)).unwrap_or(Language::En),
        _ => system_language(),
    }
}

/// The current interface language.
pub fn language() -> Language {
    if LANGUAGE.load(Ordering::Relaxed) == 1 {
        Language::En
    } else {
        Language::ZhCn
    }
}

pub fn set_language(lang: Language) {
    LANGUAGE.store(lang_code(lang), Ordering::Relaxed);
}

static EN_CATALOG: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    let mut out = HashMap::new();
    for (name, text) in EN_FILES {
        // parsed once and kept for the life of the process (about 100 KB)
        match serde_json::from_str::<HashMap<String, String>>(text) {
            Ok(map) => {
                for (k, v) in map {
                    if !v.is_empty() {
                        out.insert(&*Box::leak(k.into_boxed_str()), &*Box::leak(v.into_boxed_str()));
                    }
                }
            }
            Err(e) => tracing::warn!("translation catalog {name} is unreadable: {e}"),
        }
    }
    out
});

static EMPTY: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(HashMap::new);

/// All entries of a language (the source language has none).
pub fn catalog(lang: Language) -> &'static HashMap<&'static str, &'static str> {
    match lang {
        Language::En => &EN_CATALOG,
        Language::ZhCn => &EMPTY,
    }
}

/// `text` in the current language (the Chinese source text when there is no translation).
pub fn t(text: &str) -> &str {
    t_in(language(), text)
}

/// `text` in `lang`.
pub fn t_in(lang: Language, text: &str) -> &str {
    if lang == SOURCE {
        return text;
    }
    match catalog(lang).get(text) {
        Some(found) => found,
        None => {
            let mut missing = MISSING.lock();
            if !missing.contains(text) {
                missing.insert(text.to_owned());
                drop(missing);
                tracing::debug!("no {} translation for {:?}", lang.as_str(), text);
            }
            text
        }
    }
}

/// A value for a `{name}` placeholder.
#[derive(Clone, Debug, PartialEq)]
pub enum Arg {
    Int(i128),
    Float(f64),
    Str(String),
}

macro_rules! arg_int {
    ($($t:ty),*) => {$(impl From<$t> for Arg { fn from(v: $t) -> Self { Arg::Int(v as i128) } })*};
}
arg_int!(i8, i16, i32, i64, i128, u8, u16, u32, u64, usize, isize);

impl From<f64> for Arg {
    fn from(v: f64) -> Self {
        Arg::Float(v)
    }
}
impl From<f32> for Arg {
    fn from(v: f32) -> Self {
        Arg::Float(v as f64)
    }
}
impl From<&str> for Arg {
    fn from(v: &str) -> Self {
        Arg::Str(v.to_owned())
    }
}
impl From<String> for Arg {
    fn from(v: String) -> Self {
        Arg::Str(v)
    }
}
impl From<&String> for Arg {
    fn from(v: &String) -> Self {
        Arg::Str(v.clone())
    }
}
impl From<std::borrow::Cow<'_, str>> for Arg {
    fn from(v: std::borrow::Cow<'_, str>) -> Self {
        Arg::Str(v.into_owned())
    }
}
impl From<&std::path::Path> for Arg {
    fn from(v: &std::path::Path) -> Self {
        Arg::Str(v.display().to_string())
    }
}
impl From<&std::path::PathBuf> for Arg {
    fn from(v: &std::path::PathBuf) -> Self {
        Arg::Str(v.display().to_string())
    }
}

fn group_thousands(digits: &str) -> String {
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    let n = digits.len();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (n - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Format one value with a Python format spec subset: `[0][width][,][.prec][d|x|X|f|s]`, `#x`.
fn format_arg(arg: &Arg, spec: &str) -> String {
    if spec.is_empty() {
        return match arg {
            Arg::Int(i) => i.to_string(),
            Arg::Float(f) => python_float(*f),
            Arg::Str(s) => s.clone(),
        };
    }
    let mut rest = spec;
    let alt = rest.starts_with('#');
    if alt {
        rest = &rest[1..];
    }
    let zero = rest.starts_with('0');
    let wend = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    let width: usize = rest[..wend].parse().unwrap_or(0);
    rest = &rest[wend..];
    let comma = rest.starts_with(',');
    if comma {
        rest = &rest[1..];
    }
    let mut prec = None;
    if let Some(r) = rest.strip_prefix('.') {
        let pend = r.find(|c: char| !c.is_ascii_digit()).unwrap_or(r.len());
        prec = r[..pend].parse::<usize>().ok();
        rest = &r[pend..];
    }
    let body = match (arg, rest) {
        (Arg::Int(i), "x") => format!("{}{:x}", if alt { "0x" } else { "" }, i),
        (Arg::Int(i), "X") => format!("{}{:X}", if alt { "0X" } else { "" }, i),
        (Arg::Int(i), _) if prec.is_none() && rest != "f" => {
            let s = i.unsigned_abs().to_string();
            let s = if comma { group_thousands(&s) } else { s };
            if *i < 0 { format!("-{s}") } else { s }
        }
        (Arg::Int(i), _) => format_float(*i as f64, prec.unwrap_or(6), comma),
        (Arg::Float(f), _) => format_float(*f, prec.unwrap_or(6), comma),
        (Arg::Str(s), _) => match prec {
            Some(p) => s.chars().take(p).collect(),
            None => s.clone(),
        },
    };
    let len = body.chars().count();
    if len >= width {
        return body;
    }
    let pad = width - len;
    match arg {
        Arg::Str(_) => format!("{body}{}", " ".repeat(pad)),
        _ if zero => {
            let (sign, digits) = body.strip_prefix('-').map_or(("", body.as_str()), |d| ("-", d));
            format!("{sign}{}{digits}", "0".repeat(pad))
        }
        _ => format!("{}{body}", " ".repeat(pad)),
    }
}

fn format_float(f: f64, prec: usize, comma: bool) -> String {
    let s = format!("{:.*}", prec, f.abs());
    let s = if comma {
        let (int, frac) = s.split_once('.').map_or((s.as_str(), None), |(a, b)| (a, Some(b)));
        let g = group_thousands(int);
        frac.map_or(g.clone(), |fr| format!("{g}.{fr}"))
    } else {
        s
    };
    if f.is_sign_negative() && f != 0.0 { format!("-{s}") } else { s }
}

/// `str(float)` as Python writes it (`1.0`, `2.5`).
fn python_float(f: f64) -> String {
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 1e16 {
        format!("{f:.1}")
    } else {
        f.to_string()
    }
}

/// `str.format(**fields)` for `{name}` / `{name:spec}` placeholders (`{}`: the next field in order); `{{` and `}}` are braces. An
/// unknown field is left as it is.
pub fn format(template: &str, fields: &[(&str, Arg)]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    let mut auto = 0;
    while let Some(pos) = rest.find(['{', '}']) {
        out.push_str(&rest[..pos]);
        let tail = &rest[pos..];
        if tail.starts_with("{{") || tail.starts_with("}}") {
            out.push_str(&tail[..1]);
            rest = &tail[2..];
            continue;
        }
        if let Some(after) = tail.strip_prefix('}') {
            out.push('}');
            rest = after;
            continue;
        }
        let Some(end) = tail.find('}') else {
            out.push_str(tail);
            return out;
        };
        let inner = &tail[1..end];
        let (name, spec) = inner.split_once(':').unwrap_or((inner, ""));
        let found = if name.is_empty() {
            auto += 1;
            fields.get(auto - 1)
        } else {
            fields.iter().find(|(k, _)| *k == name)
        };
        match found {
            Some((_, v)) => {
                let _ = write!(out, "{}", format_arg(v, spec));
            }
            None => out.push_str(&tail[..=end]),
        }
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    out
}

/// `format(t(text), fields)` (no fields: just the translation).
pub fn tr_with(text: &str, fields: &[(&str, Arg)]) -> String {
    let out = t(text);
    if fields.is_empty() { out.to_owned() } else { format(out, fields) }
}

/// `tr(text, fields)`, except one of something in English: `one` (an English literal), e.g.
/// `plural(n, "{count} 个快照", "1 snapshot", &[("count", n.into())])`.
pub fn plural(count: i64, text: &str, one: &str, fields: &[(&str, Arg)]) -> String {
    if count == 1 && language() == Language::En {
        return if fields.is_empty() { one.to_owned() } else { format(one, fields) };
    }
    tr_with(text, fields)
}

/// `tr!("已导出 {n} 项", n = 3)`: the text in the current language with its fields filled in.
#[macro_export]
macro_rules! tr {
    ($text:expr $(,)?) => { $crate::i18n::t($text).to_owned() };
    ($text:expr, $($k:ident = $v:expr),+ $(,)?) => {
        $crate::i18n::tr_with($text, &[$((stringify!($k), $crate::i18n::Arg::from($v))),+])
    };
}

/// Serialises tests that switch the process-wide language.
#[doc(hidden)]
pub static TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_tags() {
        for (tag, want) in [
            (Some("zh_CN.UTF-8"), Some(Language::ZhCn)),
            (Some("zh-Hans-CN"), Some(Language::ZhCn)),
            (Some("zh-TW"), Some(Language::ZhCn)),
            (Some("en_US.UTF-8"), Some(Language::En)),
            (Some("de-DE"), Some(Language::En)),
            (Some("C"), None),
            (Some("POSIX"), None),
            (Some(""), None),
            (None, None),
        ] {
            assert_eq!(normalize(tag), want, "{tag:?}");
        }
    }

    #[test]
    fn formatting() {
        assert_eq!(format("没有这个词{n}", &[("n", 3.into())]), "没有这个词3");
        assert_eq!(format("{a:.1f} {b:,} {c:#x} {d:02x} {{x}} {e}", &[
            ("a", 2.25f64.into()),
            ("b", 1234567.into()),
            ("c", 255.into()),
            ("d", 5.into()),
        ]), "2.2 1,234,567 0xff 05 {x} {e}");
        assert_eq!(format("{f}", &[("f", 1.0f64.into())]), "1.0");
    }

    #[test]
    fn translation_helpers() {
        let _g = TEST_LOCK.lock();
        assert_eq!(t("未知空间"), "未知空间");
        set_language(Language::En);
        assert!(!catalog(Language::En).is_empty(), "the English catalog is empty");
        if let Some(english) = catalog(Language::En).get("未知空间") {
            assert_eq!(t("未知空间"), *english);
        }
        assert_eq!(t("没有这个词{n}"), "没有这个词{n}"); // no translation: the source text
        assert_eq!(tr!("没有这个词{n}", n = 3), "没有这个词3");
        assert_eq!(plural(1, "{count} 个快照", "1 snapshot", &[("count", 1.into())]), "1 snapshot");
        set_language(Language::ZhCn);
        assert_eq!(plural(1, "{count} 个快照", "1 snapshot", &[("count", 1.into())]), "1 个快照");
    }
}
