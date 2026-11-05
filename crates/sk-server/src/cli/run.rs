//! `run`: SpaceSniffer-compatible command sequences, without a window (port of `cli_run.py`).
//!
//! ```text
//! spaceknife-cli run scan "D:\Photos" filter "*.jpg;>5mb" export "Simple file list" big.txt autoclose
//! ```
//!
//! The commands are SpaceSniffer 2.0.5.18's (docs/export-templates.md, section 6): `scan`, `load`,
//! `filter`, `save`, `export`, `autoclose` and `help`, plus the meta commands `@delim=` and
//! `@parser=OS`. They run in order; `filter`, `save` and `export` act on the view opened by the
//! preceding `scan` / `load`. The whole sequence is checked before anything runs.
//!
//! SpaceSniffer reads the raw command line with its own parser (parameters in delimiters, no
//! escape character), so `scan "C:\" …` is not mangled by the C runtime's backslash rules. On
//! Windows [`cli`] does the same with `GetCommandLineW`; elsewhere the shell has already split the
//! arguments and the same rules apply to them ([`split_tokens`]). `@parser=OS` switches to the
//! arguments as the system split them.
//!
//! Scans run in this process. Messages go to stderr; without `autoclose` a summary of each view
//! goes to stdout.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use sk_core::export::{self, ExportTree, Ordering, Target};
use sk_core::fmt;
use sk_core::i18n::{self, Arg, t};
use sk_core::scan::{ScanOptions, ScanState, Scanner};
use sk_core::tr;
use sk_store::snapshot;
use sk_store::{NodeId, Store};

use super::rich::{Column, Console, MUTED, Style, Table, Text, WARN};

/// User-facing texts of this module (Chinese source text; translated when shown).
pub mod text {
    pub const UNKNOWN: &str = "未知的命令「{word}」（可用：help scan load filter save export autoclose）";
    pub const INCOMPLETE: &str = "命令 {name} 不完整，写法：{usage}";
    pub const NO_VIEW: &str = "{name} 前面没有 scan 或 load：它要作用于前一个打开的视图";
    pub const UNCLOSED: &str = "参数缺少结尾的分隔符 {delim}";
    pub const BAD_DELIM: &str = "元命令 {word} 无效：@delim= 后面应是一个或两个字符";
    pub const META_AFTER_OS: &str = "@parser=OS 之后不能再用元命令：{word}";
    pub const ERRORS: &str = "命令行有误，没有执行任何命令：";
    pub const HELP_HINT: &str = "运行 spaceknife run help 查看用法。";
    pub const SCAN_FAILED: &str = "无法扫描 {paths}：{reason}";
    pub const SCAN_STOPPED: &str = "扫描没有完成（{state}）：{paths}";
    pub const SCANNED: &str = "已扫描 {paths}：{items} 项，占用 {size}，用时 {time}";
    pub const DENIED: &str = "{count} 个文件夹无法访问，例如 {example}";
    pub const LOAD_FAILED: &str = "无法打开快照 {path}：{reason}";
    pub const LOAD_SNS: &str = "{path} 看起来是 SpaceSniffer 快照（.sns）：这种格式没有公开，SpaceKnife 读不了";
    pub const LOAD_MANY: &str = "一个 load 只能打开一个快照；要打开多个，请写多个 load";
    pub const LOADED: &str = "已打开快照 {path}：{items} 项，占用 {size}";
    pub const FILTER_FAILED: &str = "过滤表达式有误：{expr}";
    pub const FILTER_DIAG: &str = "  第 {start} 个字符起「{text}」：{reason}";
    pub const FILTERED: &str = "过滤 {expr}：{count} 个文件，占用 {size}";
    pub const SAVED: &str = "已保存快照 {path}（{size}）";
    pub const SAVE_FAILED: &str = "无法保存快照 {path}：{reason}";
    pub const TEMPLATE_ERRORS: &str = "模板「{name}」有错误，没有导出：";
    pub const TEMPLATE_ISSUE: &str = "  第 {line} 行第 {column} 列（{section}）：{message}";
    pub const EXPORTED: &str = "已导出（{what}）到 {path}（{size}）";
    pub const EXPORT_FAILED: &str = "无法写入 {path}：{reason}";
    pub const INTERRUPTED: &str = "已中断";
    pub const VIEW_TITLE: &str = "视图 {index}：{label}";
    pub const COL_NAME: &str = "名称";
    pub const COL_DISK: &str = "占用";
    pub const COL_SHARE: &str = "占比";
    pub const VIEW_TOTAL: &str = "共 {files}、{folders}，占用 {size}";
    pub const VIEW_FILES: &str = "{files} 个文件";
    pub const VIEW_FOLDERS: &str = "{folders} 个文件夹";
    pub const VIEW_FILTER: &str = "过滤 {expr}";
    pub const REST: &str = "…其余 {count} 项";
    pub const HELP: &str = "spaceknife run <命令序列>\n\n按顺序执行与 SpaceSniffer 相同的命令，不打开窗口。整个序列先检查一遍，有错就一条也不执行。\n\n  help                       显示本帮助\n  scan \"<路径>\"              扫描文件夹或磁盘，打开一个视图；多个路径用 ; 分隔（同一个视图）\n  load \"<快照>\"              打开 SpaceKnife 快照（.sks），打开一个视图\n  filter \"<表达式>\"          对前一个视图应用过滤（语法见 docs/filter-syntax.md）\n  save \"<文件>\"              把前一个视图的完整结果保存为快照（不受过滤影响）\n  export \"<模板>\" \"<文件>\"   把前一个视图导出到文件（过滤后的内容）；<模板> 是内置模板名、\n                             csv / json / html，或模板文件的路径（语法见 docs/export-templates.md）\n  autoclose                  结束时不显示视图摘要\n\n元命令（必须是一个不带分隔符的词）：\n  @delim=X  @delim=XY        之后的参数改用 X 作分隔符（或 X 开头、Y 结尾），默认是 \"\n  @parser=OS                 改用系统拆分的参数从头再解析一次，之后不再接受元命令\n\n内置模板：{builtins}\n导出格式：{formats}\n\n例：spaceknife run scan \"D:\\Photos\" filter \"*.jpg;>5mb\" export \"Simple file list\" big.txt autoclose\n";
}

/// docs/filter-syntax.md, section 8 (the frontends localise by code).
pub fn filter_reason(code: &str) -> &'static str {
    match code {
        "incomplete" => "条件缺少必需的部分",
        "unterminated_quote" => "引号没有闭合",
        "text_after_quote" => "闭合引号后还有内容",
        "bad_escape" => "引号内末尾的 \\",
        "backslash_in_mask" => "不带引号的掩码里有 \\",
        "bad_char" => "不带引号的掩码里有 | < >",
        "unknown_keyword" => "未知关键字",
        "unknown_class" => "未知类别",
        "unknown_tag" => "未知标签",
        "unknown_attr" => "未知属性",
        "bad_list" => "列表格式不对",
        "unknown_prefix" => "未知前缀",
        "bad_operator" => "运算符应为 < > <= >=",
        "bad_number" => "数字不合法",
        "missing_unit" => "缺少单位",
        "unknown_unit" => "未知单位",
        "unit_mismatch" => "前缀与单位不同类",
        "out_of_range" => "数值超出上限",
        "unsupported" => "当前平台不记录这项属性或时间（警告）",
        _ => "",
    }
}

/// How a command with parameters is written (the others: just the name).
pub fn usage(name: &str) -> &str {
    match name {
        "scan" => "scan \"<路径>\"",
        "load" => "load \"<快照>\"",
        "filter" => "filter \"<表达式>\"",
        "save" => "save \"<文件>\"",
        "export" => "export \"<模板>\" \"<文件>\"",
        other => other,
    }
}

fn arity(name: &str) -> Option<usize> {
    Some(match name {
        "help" | "autoclose" => 0,
        "scan" | "load" | "filter" | "save" => 1,
        "export" => 2,
        _ => return None,
    })
}

/// Native listing calls in flight for a command-line scan.
pub const SCAN_THREADS: usize = 4;
pub const SUMMARY_ROWS: usize = 10;
pub const EXIT_OK: i32 = 0;
pub const EXIT_ERROR: i32 = 2;
pub const EXIT_INTERRUPTED: i32 = 130;

fn f(template: &str, fields: &[(&str, Arg)]) -> String {
    i18n::format(t(template), fields)
}

fn s(v: impl Into<Arg>) -> Arg {
    v.into()
}

// ------------------------------------------------------------------------------------- parsing

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    /// lowercase command name
    pub name: String,
    pub args: Vec<String>,
}

impl Command {
    pub fn new(name: &str, args: &[&str]) -> Command {
        Command { name: name.to_owned(), args: args.iter().map(|a| a.to_string()).collect() }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParserKind {
    Internal,
    Os,
}

#[derive(Clone, Debug)]
pub struct Parsed {
    pub commands: Vec<Command>,
    pub errors: Vec<String>,
    pub parser: ParserKind,
}

enum Meta {
    Os,
    Delim(char, char),
}

/// A meta command: new delimiters, the parser switch, or None (an ordinary word).
fn meta(word: &str, errors: &mut Vec<String>) -> Option<Meta> {
    let low = word.to_lowercase();
    if low == "@parser=os" {
        return Some(Meta::Os);
    }
    if low.starts_with("@delim=") {
        let spec: Vec<char> = word.chars().skip("@delim=".len()).collect();
        if (spec.len() == 1 || spec.len() == 2) && !spec.iter().any(|c| c.is_whitespace()) {
            return Some(Meta::Delim(spec[0], *spec.last().unwrap()));
        }
        errors.push(f(text::BAD_DELIM, &[("word", s(word))]));
    }
    None
}

fn is_delim_word(word: &str) -> bool {
    word.to_lowercase().starts_with("@delim=")
}

/// SpaceSniffer's internal parser over a raw command line: (words, errors, switch to OS).
///
/// Words are separated by white space; a word that starts with the opening delimiter runs to the
/// closing one (no escape character). Meta commands are bare words and change how the rest of the
/// line is read.
pub fn split_line(text: &str) -> (Vec<String>, Vec<String>, bool) {
    let chars: Vec<char> = text.chars().collect();
    let mut words = Vec::new();
    let mut errors = Vec::new();
    let (mut opening, mut closing) = ('"', '"');
    let n = chars.len();
    let mut i = 0;
    while i < n {
        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }
        if chars[i] == opening {
            match chars[i + 1..].iter().position(|&c| c == closing) {
                None => {
                    errors.push(f(text::UNCLOSED, &[("delim", s(closing.to_string()))]));
                    words.push(chars[i + 1..].iter().collect());
                    break;
                }
                Some(k) => {
                    let j = i + 1 + k;
                    words.push(chars[i + 1..j].iter().collect());
                    i = j + 1;
                    continue;
                }
            }
        }
        let mut j = i;
        while j < n && !chars[j].is_whitespace() {
            j += 1;
        }
        let word: String = chars[i..j].iter().collect();
        i = j;
        match meta(&word, &mut errors) {
            Some(Meta::Os) => return (words, errors, true),
            Some(Meta::Delim(o, c)) => {
                opening = o;
                closing = c;
            }
            None if !is_delim_word(&word) => words.push(word),
            None => {}
        }
    }
    (words, errors, false)
}

/// The internal parser over arguments the shell already split (POSIX, or no raw line).
///
/// A token that starts with the opening delimiter runs to the token holding the closing one (the
/// tokens joined by single spaces); whatever follows the closing delimiter is the next token.
pub fn split_tokens<S: AsRef<str>>(tokens: &[S]) -> (Vec<String>, Vec<String>, bool) {
    let mut words = Vec::new();
    let mut errors = Vec::new();
    let (mut opening, mut closing) = ('"', '"');
    let mut queue: VecDeque<String> = tokens.iter().map(|t| t.as_ref().to_owned()).collect();
    while let Some(token) = queue.pop_front() {
        if let Some(rest) = token.strip_prefix(opening) {
            let mut buf = rest.to_owned();
            loop {
                if let Some(k) = buf.find(closing) {
                    words.push(buf[..k].to_owned());
                    let after = &buf[k + closing.len_utf8()..];
                    if !after.is_empty() {
                        queue.push_front(after.to_owned());
                    }
                    break;
                }
                match queue.pop_front() {
                    None => {
                        errors.push(f(text::UNCLOSED, &[("delim", s(closing.to_string()))]));
                        words.push(buf);
                        break;
                    }
                    Some(next) => buf = format!("{buf} {next}"),
                }
            }
            continue;
        }
        match meta(&token, &mut errors) {
            Some(Meta::Os) => return (words, errors, true),
            Some(Meta::Delim(o, c)) => {
                opening = o;
                closing = c;
            }
            None if !is_delim_word(&token) => words.push(token),
            None => {}
        }
    }
    (words, errors, false)
}

/// `@parser=OS`: the arguments as the system split them; the switch itself has no further effect.
fn os_words<S: AsRef<str>>(tokens: &[S]) -> (Vec<String>, Vec<String>) {
    let mut words = Vec::new();
    let mut errors = Vec::new();
    for token in tokens {
        let token = token.as_ref();
        let low = token.to_lowercase();
        if low == "@parser=os" {
            continue;
        }
        if low.starts_with("@delim=") {
            errors.push(f(text::META_AFTER_OS, &[("word", s(token))]));
            continue;
        }
        words.push(token.to_owned());
    }
    (words, errors)
}

/// Words → commands by arity, then the order check (`filter` / `save` / `export` need a view).
pub fn group<S: AsRef<str>>(words: &[S]) -> (Vec<Command>, Vec<String>) {
    let mut commands = Vec::new();
    let mut errors = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let word = words[i].as_ref();
        let name = word.to_lowercase();
        let Some(n) = arity(&name) else {
            errors.push(f(text::UNKNOWN, &[("word", s(word))]));
            i += 1;
            continue;
        };
        if words.len() - i - 1 < n {
            errors.push(f(text::INCOMPLETE, &[("name", s(name.as_str())), ("usage", s(t(usage(&name))))]));
            break;
        }
        let args = words[i + 1..i + 1 + n].iter().map(|w| w.as_ref().to_owned()).collect();
        commands.push(Command { name, args });
        i += 1 + n;
    }
    let mut view = false;
    for cmd in &commands {
        if cmd.name == "scan" || cmd.name == "load" {
            view = true;
        } else if matches!(cmd.name.as_str(), "filter" | "save" | "export") && !view {
            errors.push(f(text::NO_VIEW, &[("name", s(cmd.name.as_str()))]));
        }
    }
    (commands, errors)
}

/// Commands from the arguments after `run` (and, when known, the raw text they came from).
pub fn parse_command_line<S: AsRef<str>>(tokens: &[S], raw: Option<&str>) -> Parsed {
    let (mut words, mut errors, to_os) = match raw {
        Some(r) => split_line(r),
        None => split_tokens(tokens),
    };
    let mut parser = ParserKind::Internal;
    if to_os {
        // "reparses everything from scratch"
        (words, errors) = os_words(tokens);
        parser = ParserKind::Os;
    }
    let (commands, more) = group(&words);
    errors.extend(more);
    Parsed { commands, errors, parser }
}

/// The text after the first bare word `command` of a raw command line, or None.
pub fn command_tail<'a>(raw: &'a str, command: &str) -> Option<&'a str> {
    let b = raw.as_bytes();
    let n = b.len();
    let mut i = 0;
    while i < n {
        let c = raw[i..].chars().next()?;
        if c.is_whitespace() {
            i += c.len_utf8();
            continue;
        }
        if c == '"' {
            i = raw[i + 1..].find('"').map_or(n, |k| i + 1 + k + 1);
            continue;
        }
        let j = raw[i..].find(char::is_whitespace).map_or(n, |k| i + k);
        if raw[i..j].to_lowercase() == command {
            return Some(&raw[j..]);
        }
        i = j;
    }
    None
}

/// The raw tail and the system-split arguments begin with the same word (or are both empty).
pub fn tail_matches<S: AsRef<str>>(tail: &str, tokens: &[S]) -> bool {
    let first = tail.split_whitespace().next();
    match (tokens.first(), first) {
        (None, w) => w.is_none(),
        (Some(_), None) => false,
        (Some(tok), Some(w)) => w.trim_matches('"').to_lowercase() == tok.as_ref().to_lowercase(),
    }
}

/// This process's raw command line after `run` (Windows only; None elsewhere). Only trusted
/// when it starts like `tokens`: the C runtime's backslash rules mangle parameters, never the
/// first command word.
pub fn windows_command_tail<S: AsRef<str>>(tokens: &[S]) -> Option<String> {
    #[cfg(windows)]
    {
        // SAFETY: GetCommandLineW returns a NUL-terminated string owned by the process.
        let raw = unsafe {
            let p = windows_sys::Win32::System::Environment::GetCommandLineW();
            if p.is_null() {
                return None;
            }
            let mut len = 0;
            while *p.add(len) != 0 {
                len += 1;
            }
            String::from_utf16_lossy(std::slice::from_raw_parts(p, len))
        };
        let tail = command_tail(&raw, "run")?;
        tail_matches(tail, tokens).then(|| tail.to_owned())
    }
    #[cfg(not(windows))]
    {
        let _ = tokens;
        None
    }
}

// ------------------------------------------------------------------------------------- running

/// An open view: a scanned or loaded tree, with the filter applied to it.
pub struct View {
    pub label: String,
    pub store: Store,
    pub roots: Vec<String>,
    pub volumes: Vec<serde_json::Value>,
    pub stats: serde_json::Map<String, serde_json::Value>,
    pub checkpoints: serde_json::Map<String, serde_json::Value>,
    pub filter_text: String,
    pub matched: Option<Vec<bool>>,
}

/// A command could not run; the message is for the user (in the interface language).
#[derive(Debug)]
pub struct CommandFailed(pub String);

fn expanduser(p: &str) -> String {
    sk_core::paths::expand_user(p).to_string_lossy().into_owned()
}

/// `a;b` → two paths, unless the whole text names an existing folder (or file) itself.
fn split_paths(arg: &str, folder: bool) -> Vec<String> {
    let whole = expanduser(arg.trim());
    let exists = if folder { Path::new(&whole).is_dir() } else { Path::new(&whole).is_file() };
    if exists || !arg.contains(';') {
        return vec![whole];
    }
    arg.split(';').map(str::trim).filter(|p| !p.is_empty()).map(expanduser).collect()
}

/// `OSError.strerror` as Python's `open()` reports it (the C runtime's errno text).
fn os_reason(e: &std::io::Error) -> String {
    use std::io::ErrorKind as K;
    match e.kind() {
        K::NotFound => "No such file or directory".into(),
        K::PermissionDenied => "Permission denied".into(),
        K::AlreadyExists => "File exists".into(),
        K::IsADirectory => "Is a directory".into(),
        K::NotADirectory => "Not a directory".into(),
        K::StorageFull => "No space left on device".into(),
        _ => sk_core::volume::os_message(e),
    }
}

/// A path as Python's `pathlib.Path` writes it (Windows: backslashes, no `.` parts).
fn py_path(p: &Path) -> String {
    if cfg!(windows) {
        let joined: PathBuf = p.components().collect();
        joined.display().to_string()
    } else {
        p.display().to_string()
    }
}

pub struct Runner<'c> {
    pub out: &'c mut Console,
    pub err: &'c mut Console,
    pub views: Vec<View>,
    pub autoclose: bool,
}

impl Runner<'_> {
    /// Print a text filled in; with a count `n`, English says `one` for a single thing.
    fn say(&mut self, key: &str, style: Style, n: Option<i64>, one: &str, fields: &[(&str, Arg)]) {
        let text = match n {
            Some(n) => i18n::plural(n, key, one, fields),
            None => f(key, fields),
        };
        self.err.text(&Text::styled(&text, style));
    }

    pub fn run(&mut self, commands: &[Command]) -> i32 {
        for cmd in commands {
            let a = &cmd.args;
            let outcome = match cmd.name.as_str() {
                "help" => {
                    self.out.text(&Text::plain(&help_text()));
                    Ok(())
                }
                "autoclose" => {
                    self.autoclose = true;
                    Ok(())
                }
                "scan" => self.cmd_scan(&a[0]),
                "load" => self.cmd_load(&a[0]),
                "filter" => self.cmd_filter(&a[0]),
                "save" => self.cmd_save(&a[0]),
                "export" => self.cmd_export(&a[0], &a[1]),
                _ => Ok(()),
            };
            if let Err(CommandFailed(msg)) = outcome {
                self.err.text(&Text::styled(&msg, Style::RED));
                return EXIT_ERROR;
            }
            if super::interrupt::interrupted() {
                self.err.text(&Text::styled(t(text::INTERRUPTED), Style::YELLOW));
                return EXIT_INTERRUPTED;
            }
        }
        if !self.autoclose {
            for index in 0..self.views.len() {
                self.summary(index + 1, index);
            }
        }
        EXIT_OK
    }

    fn view(&mut self) -> &mut View {
        self.views.last_mut().expect("group() made sure a scan / load came first")
    }

    fn cmd_scan(&mut self, arg: &str) -> Result<(), CommandFailed> {
        let paths = split_paths(arg, true);
        let threads = SCAN_THREADS.min(sk_core::scan::native_threads());
        let scanner = Scanner::new(&paths, ScanOptions { concurrency: threads, ..Default::default() })
            .map_err(|e| CommandFailed(f(text::SCAN_FAILED, &[("paths", s(paths.join("; "))), ("reason", s(e.to_string()))])))?;
        super::interrupt::cooperative(true);
        let mut check = |_: &sk_core::scan::ScanProgress| {
            if super::interrupt::interrupted() {
                scanner.cancel();
            }
        };
        let result = scanner.run(Some(&mut check), std::time::Duration::from_millis(200));
        super::interrupt::cooperative(false);
        if !self.err.interactive() {
            self.err.blank(); // the empty line rich's Progress.stop() prints on a redirected console
        }
        let infos = scanner.roots();
        let roots: Vec<String> = infos.iter().map(|r| r.path.to_string_lossy().into_owned()).collect();
        if super::interrupt::interrupted() {
            return Ok(());
        }
        if result.state != ScanState::Done {
            return Err(CommandFailed(f(
                text::SCAN_STOPPED,
                &[("state", s(result.state.as_str())), ("paths", s(roots.join("; ")))],
            )));
        }
        self.say(
            text::SCANNED,
            MUTED,
            Some(result.items),
            "Scanned {paths}: 1 item, {size} on disk, in {time}",
            &[
                ("paths", s(roots.join("; "))),
                ("items", s(fmt::count(result.items))),
                ("size", s(fmt::size(result.bytes_alloc as f64))),
                ("time", s(fmt::duration(result.elapsed_s))),
            ],
        );
        if result.errors > 0 {
            self.say(
                text::DENIED,
                WARN,
                Some(result.errors),
                "1 folder couldn't be accessed, e.g. {example}",
                &[("count", s(result.errors)), ("example", s(result.denied.first().cloned().unwrap_or_default()))],
            );
        }
        let iso = |d: Option<sk_protocol::DateTime>| {
            d.map_or(serde_json::Value::Null, |d| d.to_rfc3339_opts(chrono::SecondsFormat::Micros, false).into())
        };
        let mut stats = serde_json::Map::new();
        for (k, v) in [
            ("items", result.items.into()),
            ("files", result.files.into()),
            ("dirs", result.dirs.into()),
            ("bytes_alloc", result.bytes_alloc.into()),
            ("bytes_size", result.bytes_size.into()),
            ("errors", result.errors.into()),
            ("elapsed_s", result.elapsed_s.into()),
            ("started_at", iso(result.started_at)),
            ("finished_at", iso(result.finished_at)),
            ("denied", result.denied.clone().into()),
            ("state", result.state.as_str().into()),
            (
                "roots",
                infos
                    .iter()
                    .map(|r| serde_json::json!({"path": r.path.to_string_lossy(), "whole_volume": r.whole_volume}))
                    .collect::<Vec<_>>()
                    .into(),
            ),
        ] {
            stats.insert(k.to_owned(), v);
        }
        let volumes = infos
            .iter()
            .map(|r| r.volume.as_ref().and_then(|v| serde_json::to_value(v.to_protocol()).ok()).unwrap_or_else(|| serde_json::json!({})))
            .collect();
        self.views.push(View {
            label: format!("scan {arg}"),
            store: scanner.store().clone(),
            roots,
            volumes,
            stats,
            checkpoints: Default::default(),
            filter_text: String::new(),
            matched: None,
        });
        Ok(())
    }

    fn cmd_load(&mut self, arg: &str) -> Result<(), CommandFailed> {
        let paths = split_paths(arg, false);
        if paths.len() > 1 {
            return Err(CommandFailed(t(text::LOAD_MANY).to_owned()));
        }
        let path = PathBuf::from(&paths[0]);
        let shown = py_path(&path);
        let (store, header) = snapshot::load(&path).map_err(|e| {
            let sns = path.extension().is_some_and(|x| x.eq_ignore_ascii_case("sns"));
            CommandFailed(match e {
                snapshot::SnapshotError::Io(io) => {
                    f(text::LOAD_FAILED, &[("path", s(shown.as_str())), ("reason", s(os_reason(&io)))])
                }
                _ if sns => f(text::LOAD_SNS, &[("path", s(shown.as_str()))]),
                other => f(text::LOAD_FAILED, &[("path", s(shown.as_str())), ("reason", s(other.to_string()))]),
            })
        })?;
        let items = header.node_count as i64 - 1;
        let total = store.read().tot_alloc(0);
        self.say(
            text::LOADED,
            MUTED,
            Some(items),
            "Opened snapshot {path}: 1 item, {size} on disk",
            &[("path", s(shown.as_str())), ("items", s(fmt::count(items))), ("size", s(fmt::size(total as f64)))],
        );
        self.views.push(View {
            label: format!("load {arg}"),
            store,
            roots: header.roots.clone(),
            volumes: header.volumes.clone(),
            stats: header.stats.clone(),
            checkpoints: header.checkpoints.clone(),
            filter_text: String::new(),
            matched: None,
        });
        Ok(())
    }

    fn cmd_filter(&mut self, expr: &str) -> Result<(), CommandFailed> {
        let table = sk_core::classes::active();
        let lang = i18n::language().as_str();
        let vocab = sk_core::filter::vocab::compiled_vocab(&sk_core::classes::shown(&table), sk_core::classes::version(), lang);
        let parsed = vocab.parse(expr, None);
        let lines: Vec<String> = parsed
            .diagnostics
            .iter()
            .map(|d| {
                let code = d.code.as_str();
                let reason = match filter_reason(code) {
                    "" => code.to_owned(),
                    r => t(r).to_owned(),
                };
                f(text::FILTER_DIAG, &[("start", s(d.start + 1)), ("text", s(d.text.as_str())), ("reason", s(reason))])
            })
            .collect();
        let Some(normal) = parsed.normal else {
            let mut all = vec![f(text::FILTER_FAILED, &[("expr", s(expr))])];
            all.extend(lines);
            return Err(CommandFailed(all.join("\n")));
        };
        for line in &lines {
            self.err.text(&Text::styled(line, WARN));
        }
        let found = {
            let view = self.view();
            let r = view.store.read();
            sk_core::filter::evaluate(&r, &normal, &sk_core::filter::FilterContext::current())
        };
        let (count, alloc, _) = found.totals(0);
        let view = self.view();
        view.matched = Some(found.matched);
        view.filter_text = expr.to_owned();
        self.say(
            text::FILTERED,
            MUTED,
            Some(count),
            "Filter {expr}: 1 file, {size} on disk",
            &[("expr", s(expr)), ("count", s(fmt::count(count))), ("size", s(fmt::size(alloc as f64)))],
        );
        Ok(())
    }

    fn cmd_save(&mut self, arg: &str) -> Result<(), CommandFailed> {
        let path = sk_core::paths::expand_user(arg);
        let shown = py_path(&path);
        let view = self.view();
        let opts = snapshot::SaveOptions {
            roots: view.roots.clone(),
            volumes: view.volumes.clone(),
            stats: view.stats.clone(),
            checkpoints: view.checkpoints.clone(),
            ..Default::default()
        };
        let saved = {
            let r = view.store.read();
            snapshot::save(&r, &path, &opts)
        };
        let fail = |reason: String| CommandFailed(f(text::SAVE_FAILED, &[("path", s(shown.as_str())), ("reason", s(reason))]));
        match saved {
            Ok(_) => {}
            Err(snapshot::SnapshotError::Io(e)) => return Err(fail(os_reason(&e))),
            Err(e) => return Err(fail(e.to_string())),
        }
        let size = std::fs::metadata(&path).map_err(|e| fail(os_reason(&e)))?.len();
        self.say(text::SAVED, MUTED, None, "", &[("path", s(shown.as_str())), ("size", s(fmt::size(size as f64)))]);
        Ok(())
    }

    fn cmd_export(&mut self, name: &str, arg: &str) -> Result<(), CommandFailed> {
        let target = export::resolve(name, None).map_err(|e| CommandFailed(e.to_string()))?;
        let what = match &target {
            Target::Format(fmt) => fmt.title(),
            Target::Template(tpl) => {
                let what = if tpl.name.is_empty() { name.to_owned() } else { tpl.name.clone() };
                let issue = |i: &export::TemplateIssue| {
                    f(
                        text::TEMPLATE_ISSUE,
                        &[
                            ("line", s(i.line)),
                            ("column", s(i.column)),
                            ("section", s(i.section.as_str())),
                            ("message", s(i.message.as_str())),
                        ],
                    )
                };
                if !tpl.ok() {
                    let mut lines = vec![f(text::TEMPLATE_ERRORS, &[("name", s(what.as_str()))])];
                    lines.extend(tpl.issues.iter().map(issue));
                    return Err(CommandFailed(lines.join("\n")));
                }
                for i in &tpl.issues {
                    let line = issue(i);
                    self.err.text(&Text::styled(&line, WARN));
                }
                what
            }
        };
        let path = sk_core::paths::expand_user(arg);
        let shown = py_path(&path);
        let view = self.view();
        let mask = view.matched.as_deref();
        let filter_text = view.filter_text.clone();
        let written = {
            let r = view.store.read();
            export::write_export(&path, target.bom(), |sink| export::export_chunks(&target, &r, 0, mask, &filter_text, sink))
        };
        let written = written.map_err(|e| {
            let reason = match e {
                export::ExportError::Io(io) => os_reason(&io),
                other => other.to_string(),
            };
            CommandFailed(f(text::EXPORT_FAILED, &[("path", s(shown.as_str())), ("reason", s(reason))]))
        })?;
        self.say(
            text::EXPORTED,
            MUTED,
            None,
            "",
            &[("what", s(what)), ("path", s(shown.as_str())), ("size", s(fmt::size(written as f64)))],
        );
        Ok(())
    }

    // ------------------------------------------------------------------ the headless "view"

    fn summary(&mut self, index: usize, at: usize) {
        let view = &self.views[at];
        self.out.text(&Text::styled(&f(text::VIEW_TITLE, &[("index", s(index)), ("label", s(view.label.as_str()))]), Style::BOLD));
        if !view.filter_text.is_empty() {
            self.out.text(&Text::styled(&f(text::VIEW_FILTER, &[("expr", s(view.filter_text.as_str()))]), MUTED));
        }
        let r = view.store.read();
        let order = Ordering { blocks: export::Blocks::Mixed, sort: export::SortKey::Size, descending: true };
        for root in r.roots() {
            let Ok(tree) = ExportTree::new(&r, root, view.matched.as_deref(), order, true, None) else { continue };
            let (_, total) = tree.sizes(0);
            let (files, folders) = tree.counts(0);
            self.out.text(&Text::plain(&export::text_of(&tree.root_path)));
            // position -> (node, folder) of the root's children
            let mut first_level: std::collections::HashMap<u32, (NodeId, bool)> = Default::default();
            let _ = tree.walk(|el| {
                if el.level == 1 {
                    first_level.insert(el.pos, (el.node, el.is_dir));
                }
                Ok::<(), ()>(())
            });
            let mut table = Table::new(None, false);
            table
                .column(Column::new(t(text::COL_NAME)).no_wrap().max(48))
                .column(Column::new(t(text::COL_DISK)).right().style(Style::BOLD).no_wrap())
                .column(Column::new(t(text::COL_SHARE)).right().no_wrap());
            let kids = tree.children(0);
            let share_of = |a: i64| if total != 0 { a as f64 / total as f64 } else { 0.0 };
            for &pos in kids.iter().take(SUMMARY_ROWS) {
                let (_, alloc) = tree.sizes(pos);
                let (node, is_dir) = first_level.get(&pos).copied().unwrap_or((0, false));
                let mut name = r.name(node).into_owned();
                if is_dir {
                    name.push(std::path::MAIN_SEPARATOR);
                }
                table.row(vec![
                    Text::plain(&name),
                    Text::plain(&fmt::size(alloc as f64)),
                    Text::plain(&format!("{:.1}%", share_of(alloc) * 100.0)),
                ]);
            }
            if kids.len() > SUMMARY_ROWS {
                let rest: i64 = kids[SUMMARY_ROWS..].iter().map(|&p| tree.alloc_key(p)).sum();
                let label = f(text::REST, &[("count", s(fmt::count((kids.len() - SUMMARY_ROWS) as i64)))]);
                table.row(vec![
                    Text::styled(&label, MUTED),
                    Text::plain(&fmt::size(rest as f64)),
                    Text::plain(&format!("{:.1}%", share_of(rest) * 100.0)),
                ]);
            }
            self.out.table(&table);
            let files_text = i18n::plural(files as i64, text::VIEW_FILES, "1 file", &[("files", s(fmt::count(files as i64)))]);
            let folders_text =
                i18n::plural(folders as i64, text::VIEW_FOLDERS, "1 folder", &[("folders", s(fmt::count(folders as i64)))]);
            let line = f(text::VIEW_TOTAL, &[("files", s(files_text)), ("folders", s(folders_text)), ("size", s(fmt::size(total as f64)))]);
            self.out.text(&Text::styled(&line, MUTED));
        }
        drop(r);
        self.out.blank();
    }
}

/// `spaceknife run help`.
pub fn help_text() -> String {
    let builtins: Vec<&str> = export::BUILTINS.iter().map(|b| b.name).collect();
    let formats: Vec<&str> = export::FORMATS.iter().map(|f| f.key).collect();
    tr!(text::HELP, builtins = builtins.join(t("、")), formats = formats.join(" / "))
}

/// Run a command sequence; returns the exit code (0, 2 = a command failed, 130 = interrupted).
///
/// `tokens` are the arguments after `run` as the system split them; `raw` the same part of the
/// raw command line, when known (Windows), which the internal parser then reads instead.
pub fn main<S: AsRef<str>>(tokens: &[S], raw: Option<&str>, out: &mut Console, err: &mut Console) -> i32 {
    if tokens.is_empty() && raw.is_none_or(|r| r.trim().is_empty()) {
        out.text(&Text::plain(&help_text()));
        return EXIT_OK;
    }
    let parsed = parse_command_line(tokens, raw);
    if !parsed.errors.is_empty() {
        err.text(&Text::styled(t(text::ERRORS), Style::RED));
        for m in &parsed.errors {
            err.text(&Text::styled(&format!("  {m}"), Style::RED));
        }
        err.text(&Text::styled(t(text::HELP_HINT), MUTED));
        return EXIT_ERROR;
    }
    let mut runner = Runner { out, err, views: Vec::new(), autoclose: false };
    runner.run(&parsed.commands)
}

/// `spaceknife-cli run …` from the command line.
pub fn cli(tokens: &[String]) -> i32 {
    super::init_warnings();
    super::apply_language();
    sk_core::classes::load_user(); // the user's file classes (W7) also apply to :class: here
    sk_core::pathclass::load_user(); // and their folder choices
    super::interrupt::install(String::new());
    let mut out = Console::stdout();
    let mut err = Console::stderr();
    let raw = windows_command_tail(tokens);
    let code = main(tokens, raw.as_deref(), &mut out, &mut err);
    err.flush();
    out.flush();
    code
}
