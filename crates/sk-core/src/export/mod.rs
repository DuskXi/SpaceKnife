//! Exports of a scanned tree: SpaceSniffer-compatible templates, CSV / JSON / HTML, and output
//! files (port of `spaceknife.export`; the language is specified in docs/export-templates.md).
//!
//! [`resolve`] turns what the user names — a built-in template ("Simple file list"), a format
//! (`csv` / `json` / `html`) or a template file — into a [`Target`]; [`export_chunks`] renders it
//! piece by piece into a [`Sink`] and [`write_export`] writes the pieces to a file atomically, as
//! UTF-8.
//!
//! Everything reads the store through a `&Nodes` (a [`sk_store::Reader`] guard): take one guard
//! per export request.

mod formats;
mod template;
mod tree;
mod util;

use std::io::Write as _;
use std::path::{Path, PathBuf};

use sk_store::{NodeId, Nodes};

pub use formats::{
    ATTRIBUTES, CSV_COLUMNS, ExportFormat, FORMATS, FormatKey, HtmlOptions, JSON_FORMAT,
    JSON_VERSION, LocalTimes, REPORT_ORDER, attributes, csv_chunks, export_csv, export_html,
    export_json, format, iso_time, json_chunks, kind_name,
};
pub use template::{
    BAD_CALL, BAD_IF, BAD_PAD, BAD_TAG, CHUNK_BYTES, ExportTemplate, MAX_PAD, Section, Severity,
    TemplateIssue, check, compare, csv_field, html_escape, local_time, parse_template, render,
    render_chunks, tag_names, tag_value, usage,
};
pub use tree::{Blocks, Element, ExportTree, Mask, Ordering, SortKey, export_roots, text_of};

use util::{fill, tr};

/// Where rendered output goes, piece by piece (all text is valid UTF-8).
pub type Sink<'a> = dyn FnMut(&str) -> std::io::Result<()> + 'a;

/// Longest template file read.
pub const MAX_TEMPLATE_BYTES: u64 = 1 << 20;

mod text {
    pub const NOT_FOUND: &str = "找不到导出模板「{name}」：它既不是内置模板（{builtins}），也不是 csv / json / html，也没有这个模板文件";
    pub const TOO_BIG: &str = "模板文件 {path} 太大（超过 {limit} KB），不像是模板";
    pub const NOT_UTF8: &str = "模板文件 {path} 不是 UTF-8 编码";
    pub const UNREADABLE: &str = "无法读取模板文件 {path}：{reason}";
    pub const LIST_SEP: &str = "、";
}

/// A user-facing export problem (`Display` is in the interface language), or an I/O failure while
/// writing.
#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("{}", fill(&tr(text::NOT_FOUND), &[("name", .0), ("builtins", &builtin_names())]))]
    NotFound(String),
    #[error("{}", fill(&tr(text::TOO_BIG), &[("path", &.0.display().to_string()), ("limit", &(MAX_TEMPLATE_BYTES >> 10).to_string())]))]
    TooBig(PathBuf),
    #[error("{}", fill(&tr(text::NOT_UTF8), &[("path", &.0.display().to_string())]))]
    NotUtf8(PathBuf),
    #[error("{}", fill(&tr(text::UNREADABLE), &[("path", &.0.display().to_string()), ("reason", &.1.to_string())]))]
    Unreadable(PathBuf, std::io::Error),
    /// The export root is not an element of the store (a programming error of the caller).
    #[error("node {0} is not an element of this store")]
    NotAnElement(NodeId),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

fn builtin_names() -> String {
    BUILTINS
        .iter()
        .map(|b| b.name)
        .collect::<Vec<_>>()
        .join(&tr(text::LIST_SEP))
}

// ---------------------------------------------------------------------------------- built-ins

/// A built-in template: SpaceSniffer 2.0.5.18's four, plus SpaceKnife's folder tree.
#[derive(Debug, PartialEq, Eq)]
pub struct BuiltinInfo {
    /// SpaceSniffer's name (command line, settings); never translated
    pub name: &'static str,
    pub file: &'static str,
    /// the Chinese source text (message id) of [`BuiltinInfo::title`]
    pub title_id: &'static str,
    pub description_id: &'static str,
    /// also built into SpaceSniffer
    pub spacesniffer: bool,
    /// the template text (`builtin/*.sktpl`)
    pub source: &'static str,
}

impl BuiltinInfo {
    /// The title in the current interface language.
    pub fn title(&self) -> String {
        tr(self.title_id)
    }

    /// The description in the current interface language.
    pub fn description(&self) -> String {
        tr(self.description_id)
    }

    pub fn template(&self) -> ExportTemplate {
        parse_template(self.source, self.name)
    }
}

pub static BUILTINS: [BuiltinInfo; 5] = [
    BuiltinInfo {
        name: "Grouped by folder",
        file: "grouped-by-folder.sktpl",
        title_id: "按文件夹分组",
        description_id: "每个直接含有文件的文件夹一段：完整路径和占用，下面列出其中的文件（占用右对齐）。",
        spacesniffer: true,
        source: include_str!("builtin/grouped-by-folder.sktpl"),
    },
    BuiltinInfo {
        name: "First level folders",
        file: "first-level-folders.sktpl",
        title_id: "第一层文件夹",
        description_id: "根下第一层的每个文件夹：完整路径和占用。",
        spacesniffer: true,
        source: include_str!("builtin/first-level-folders.sktpl"),
    },
    BuiltinInfo {
        name: "Simple file list",
        file: "simple-file-list.sktpl",
        title_id: "文件清单",
        description_id: "每个文件的完整路径，一行一个。",
        spacesniffer: true,
        source: include_str!("builtin/simple-file-list.sktpl"),
    },
    BuiltinInfo {
        name: "Summary",
        file: "summary.sktpl",
        title_id: "汇总",
        description_id: "总占用、文件夹数和文件数。",
        spacesniffer: true,
        source: include_str!("builtin/summary.sktpl"),
    },
    BuiltinInfo {
        name: "Folder tree",
        file: "folder-tree.sktpl",
        title_id: "文件夹树",
        description_id: "按层级缩进的文件夹和占用。",
        spacesniffer: false,
        source: include_str!("builtin/folder-tree.sktpl"),
    },
];

/// A built-in template's info by its SpaceSniffer name; case does not matter.
pub fn builtin_info(name: &str) -> Option<&'static BuiltinInfo> {
    BUILTINS.iter().find(|b| b.name == name).or_else(|| {
        let folded = sk_protocol::filter_syntax::casefold(name);
        BUILTINS
            .iter()
            .find(|b| sk_protocol::filter_syntax::casefold(b.name) == folded)
    })
}

/// A built-in template by its SpaceSniffer name; case does not matter (SpaceSniffer: it does).
pub fn builtin_template(name: &str) -> Option<ExportTemplate> {
    builtin_info(name).map(BuiltinInfo::template)
}

// ---------------------------------------------------------------------------------- targets

/// Something to export with.
#[derive(Clone, Debug)]
pub enum Target {
    Template(Box<ExportTemplate>),
    Format(&'static ExportFormat),
}

impl Target {
    /// Write a UTF-8 BOM before the output.
    pub fn bom(&self) -> bool {
        matches!(self, Target::Format(f) if f.bom)
    }
}

fn expand_user(name: &str) -> PathBuf {
    if let Some(rest) = name.strip_prefix('~')
        && (rest.is_empty() || rest.starts_with(['/', '\\']))
    {
        let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" });
        if let Some(home) = home {
            return PathBuf::from(home).join(rest.trim_start_matches(['/', '\\']));
        }
    }
    PathBuf::from(name)
}

/// A built-in template, then a format key, then a template file (relative to `cwd`).
pub fn resolve(name: &str, cwd: Option<&Path>) -> Result<Target, ExportError> {
    if let Some(t) = builtin_template(name) {
        return Ok(Target::Template(Box::new(t)));
    }
    if let Some(f) = format(name) {
        return Ok(Target::Format(f));
    }
    let mut path = expand_user(name);
    if let Some(cwd) = cwd
        && !path.is_absolute()
    {
        path = cwd.join(path);
    }
    if !path.is_file() {
        return Err(ExportError::NotFound(name.to_owned()));
    }
    let meta = std::fs::metadata(&path).map_err(|e| ExportError::Unreadable(path.clone(), e))?;
    if meta.len() > MAX_TEMPLATE_BYTES {
        return Err(ExportError::TooBig(path));
    }
    let bytes = std::fs::read(&path).map_err(|e| ExportError::Unreadable(path.clone(), e))?;
    let source = String::from_utf8(bytes).map_err(|_| ExportError::NotUtf8(path.clone()))?;
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(Target::Template(Box::new(parse_template(&source, &stem))))
}

/// The output of `target` for the tree below `root` (0: every scan root), in pieces.
pub fn export_chunks(
    target: &Target,
    nodes: &Nodes,
    root: NodeId,
    mask: Mask<'_>,
    filter_text: &str,
    sink: &mut Sink<'_>,
) -> Result<(), ExportError> {
    match target {
        Target::Template(t) => render_chunks(nodes, root, t, mask, None, CHUNK_BYTES, sink),
        Target::Format(f) => f.chunks(nodes, root, mask, filter_text, sink),
    }
}

/// Write what `produce` hands its sink to `path` as UTF-8, atomically (a temporary file next to
/// it, then a rename); a failure leaves any old file untouched and no temporary file behind.
/// Returns the bytes written. The folder must exist.
pub fn write_export(
    path: &Path,
    bom: bool,
    produce: impl FnOnce(&mut Sink<'_>) -> Result<(), ExportError>,
) -> Result<u64, ExportError> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    let result = (|| -> Result<u64, ExportError> {
        let file = std::fs::File::create(&tmp)?;
        let mut w = std::io::BufWriter::with_capacity(1 << 16, file);
        let mut written = 0u64;
        if bom {
            w.write_all(b"\xef\xbb\xbf")?;
            written += 3;
        }
        produce(&mut |s: &str| {
            written += s.len() as u64;
            w.write_all(s.as_bytes())
        })?;
        let file = w.into_inner().map_err(|e| e.into_error())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)?;
        Ok(written)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// The diagnostics of a template as the protocol sends them.
pub fn protocol_issues(issues: &[TemplateIssue]) -> Vec<sk_protocol::export::ExportIssue> {
    issues.iter().map(Into::into).collect()
}

/// Every export target as the protocol lists them (`export.targets`): the built-in templates,
/// then the formats.
pub fn protocol_targets() -> Vec<sk_protocol::export::ExportTarget> {
    use sk_protocol::export::{ExportTarget, ExportTargetKind};
    let mut out: Vec<ExportTarget> = BUILTINS
        .iter()
        .map(|b| {
            let t = b.template();
            ExportTarget {
                name: b.name.into(),
                title: b.title(),
                description: b.description(),
                kind: ExportTargetKind::Template,
                extension: "txt".into(),
                file_name: t.file_name,
                spacesniffer: b.spacesniffer,
            }
        })
        .collect();
    out.extend(FORMATS.iter().map(|f| ExportTarget {
        name: f.key.into(),
        title: f.title(),
        description: String::new(),
        kind: ExportTargetKind::Format,
        extension: f.extension.into(), // with the dot, as the Python server sent it
        file_name: String::new(),
        spacesniffer: false,
    }));
    out
}
