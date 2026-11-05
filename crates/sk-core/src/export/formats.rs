//! CSV, JSON and HTML exports of a tree, beside the template language.
//!
//! All three walk the tree the way templates do ([`ExportTree`]: depth first, the largest first,
//! a filter mask honoured), so a filtered export shows exactly what the filtered view shows. CSV
//! and JSON stream; the HTML report is bounded (a few levels, the largest children, files and
//! extensions) and self-contained: inline CSS, no scripts, no external assets.
//!
//! The data formats use English keys (a stable schema for tools); the report's text is in the
//! interface language.

use std::collections::HashMap;
use std::fmt::Write as _;

use sk_store::{Flags, Kind, NodeId, Nodes};

use super::util::{UNIT_BASE, fill, language, size as fmt_size, thousands, tr};
use super::template::{html_escape, json_escape_into, local_time};
use super::tree::{Blocks, Element, ExportTree, Mask, Ordering, Piece, SortKey, decode_units, export_roots, text_of};
use super::{ExportError, Sink};

/// Message ids of the HTML report and the format titles.
mod text {
    pub const TITLE: &str = "空间报告";
    pub const ROOTS: &str = "{count} 个位置";
    pub const GENERATED: &str = "生成于 {time}";
    pub const FILTER: &str = "过滤：{expr}";
    pub const DISK: &str = "占用空间";
    pub const LOGICAL: &str = "逻辑大小";
    pub const FILES: &str = "文件";
    pub const FOLDERS: &str = "文件夹";
    pub const MATCHED: &str = "匹配过滤的文件";
    pub const STRUCTURE: &str = "文件夹结构";
    pub const STRUCTURE_NOTE: &str = "每层按占用从大到小，最多 {top} 项、{depth} 层";
    pub const LARGEST: &str = "最大的文件";
    pub const EXTENSIONS: &str = "按扩展名";
    pub const COL_SHARE: &str = "占比";
    pub const COL_DISK: &str = "占用";
    pub const COL_FILES: &str = "文件数";
    pub const COL_MODIFIED: &str = "修改于";
    pub const COL_PATH: &str = "路径";
    pub const COL_EXT: &str = "扩展名";
    pub const NO_EXT: &str = "（无扩展名）";
    pub const REST: &str = "其余 {count} 项";
    pub const EMPTY: &str = "没有内容";
    pub const FOOTER: &str = "由 SpaceKnife {version} 生成 · 大小均为占用空间 · {base}";
    pub const BASE_1024: &str = "1 KB = 1024 字节";
    pub const BASE_1000: &str = "1 KB = 1000 字节";
    pub const CSV: &str = "CSV 表格";
    pub const JSON: &str = "JSON 数据";
    pub const HTML: &str = "HTML 报告";
}

pub const CSV_COLUMNS: [&str; 12] = [
    "path",
    "name",
    "type",
    "depth",
    "size",
    "disk_size",
    "files",
    "folders",
    "modified",
    "created",
    "accessed",
    "attributes",
];

/// Attribute names by flag, in output order.
pub const ATTRIBUTES: [(Flags, &str); 17] = [
    (Flags::READONLY, "readonly"),
    (Flags::HIDDEN, "hidden"),
    (Flags::SYSTEM, "system"),
    (Flags::ARCHIVE, "archive"),
    (Flags::COMPRESSED, "compressed"),
    (Flags::ENCRYPTED, "encrypted"),
    (Flags::SPARSE, "sparse"),
    (Flags::TEMPORARY, "temporary"),
    (Flags::OFFLINE, "offline"),
    (Flags::NOT_INDEXED, "notindexed"),
    (Flags::REPARSE, "reparse"),
    (Flags::CLOUD, "cloud"),
    (Flags::HARDLINK_DUP, "hardlink"),
    (Flags::ACCESS_DENIED, "denied"),
    (Flags::ERROR, "error"),
    (Flags::MOUNT, "mount"),
    (Flags::PENDING, "pending"),
];

/// As the treemap shows it.
pub const REPORT_ORDER: Ordering = Ordering {
    blocks: Blocks::Mixed,
    sort: SortKey::Size,
    descending: true,
};
pub const JSON_FORMAT: &str = "spaceknife-export";
pub const JSON_VERSION: u32 = 1;
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Bytes collected before a piece goes to the sink.
const PIECE_BYTES: usize = 1 << 16;

pub fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::File => "file",
        Kind::Dir => "folder",
        Kind::Link => "link",
        Kind::Other => "other",
    }
}

/// The attribute names of a flag set.
pub fn attributes(flags: Flags) -> impl Iterator<Item = &'static str> {
    ATTRIBUTES.into_iter().filter(move |(f, _)| flags.intersects(*f)).map(|(_, n)| n)
}

fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

fn exact_time(ts: i64) -> String {
    use chrono::{LocalResult, TimeZone};
    match chrono::Local.timestamp_opt(ts, 0) {
        LocalResult::Single(t) | LocalResult::Ambiguous(t, _) => t.format("%Y-%m-%dT%H:%M:%S%:z").to_string(),
        LocalResult::None => chrono::DateTime::from_timestamp(ts, 0)
            .map(|t| t.format("%Y-%m-%dT%H:%M:%S+00:00").to_string())
            .unwrap_or_default(),
    }
}

/// ISO 8601 local time with its offset (`2026-10-18T12:30:00+08:00`); "" when unknown.
pub fn iso_time(ts: i64) -> String {
    if ts > 0 { exact_time(ts) } else { String::new() }
}

/// [`iso_time`] for many timestamps: the zone offset is looked up once per UTC day (at its first
/// and last second); a day on which it changes (a daylight saving switch) is converted one
/// timestamp at a time.
#[derive(Default)]
pub struct LocalTimes {
    days: HashMap<i64, Option<(i64, String)>>,
}

impl LocalTimes {
    pub fn new() -> Self {
        Self::default()
    }

    fn zone(&mut self, day: i64) -> Option<&(i64, String)> {
        self.days
            .entry(day)
            .or_insert_with(|| {
                use chrono::{Offset, TimeZone};
                let at = |ts: i64| match chrono::Local.timestamp_opt(ts, 0) {
                    chrono::LocalResult::Single(t) => Some(t.offset().fix().local_minus_utc() as i64),
                    _ => None,
                };
                let (first, last) = (at(day * 86400)?, at(day * 86400 + 86399)?);
                if first != last {
                    return None;
                }
                let sign = if first < 0 { '-' } else { '+' };
                let a = first.abs();
                Some((first, format!("{sign}{:02}:{:02}", a / 3600, a % 3600 / 60)))
            })
            .as_ref()
    }

    /// The local time of `ts` ("" when unknown), appended to `out`.
    pub fn put(&mut self, ts: i64, out: &mut String) {
        if ts <= 0 {
            return;
        }
        match self.zone(ts.div_euclid(86400)) {
            Some((offset, suffix)) => {
                let suffix = suffix.clone();
                match chrono::DateTime::from_timestamp(ts + offset, 0) {
                    Some(t) => {
                        let _ = write!(out, "{}{suffix}", t.format("%Y-%m-%dT%H:%M:%S"));
                    }
                    None => out.push_str(&exact_time(ts)),
                }
            }
            None => out.push_str(&exact_time(ts)),
        }
    }

    pub fn get(&mut self, ts: i64) -> String {
        let mut s = String::new();
        self.put(ts, &mut s);
        s
    }
}

/// Name bytes as the body of a JSON string: units that are not Unicode as `\uXXXX` escapes.
fn json_name_into(raw: &[u8], out: &mut String) {
    decode_units(raw, |p| match p {
        Piece::Text(s) => json_escape_into(s, out),
        Piece::Unit(u) => {
            let _ = write!(out, "\\u{u:04x}");
        }
    });
}

/// Per element values the data formats write.
struct Row {
    kind: &'static str,
    flags: Flags,
    size: i64,
    alloc: i64,
    files: u32,
    dirs: u32,
}

fn row(tree: &ExportTree<'_>, el: &Element<'_>) -> Row {
    let (size, alloc) = tree.sizes(el.pos);
    let (files, dirs) = tree.counts(el.pos);
    Row {
        kind: kind_name(tree.kind(el.pos)),
        flags: tree.flags(el.pos),
        size,
        alloc,
        files,
        dirs,
    }
}

// ---------------------------------------------------------------------------------- CSV

/// One CSV field as Python's `csv` module writes it (QUOTE_MINIMAL).
fn csv_cell(text: &str, out: &mut String) {
    if text.contains([',', '"', '\r', '\n']) {
        out.push('"');
        out.push_str(&text.replace('"', "\"\""));
        out.push('"');
    } else {
        out.push_str(text);
    }
}

/// RFC 4180 CSV (CRLF rows, a header row), one row per element, depth first.
pub fn csv_chunks(nodes: &Nodes, root: NodeId, mask: Mask<'_>, sink: &mut Sink<'_>) -> Result<(), ExportError> {
    let mut buf = CSV_COLUMNS.join(",");
    buf.push_str("\r\n");
    let mut times = LocalTimes::new();
    for r in export_roots(nodes, root) {
        let tree = ExportTree::new(nodes, r, mask, REPORT_ORDER, true, None)?;
        tree.walk(|el| -> Result<(), ExportError> {
            let v = row(&tree, el);
            csv_cell(&text_of(el.path), &mut buf);
            buf.push(',');
            csv_cell(&text_of(el.name), &mut buf);
            let _ = write!(buf, ",{},{},{},{},", v.kind, el.level, v.size, v.alloc);
            if el.is_dir {
                let _ = write!(buf, "{},{}", v.files, v.dirs);
            } else {
                buf.push(',');
            }
            for which in 0..3 {
                buf.push(',');
                times.put(tree.time(el.pos, which), &mut buf);
            }
            buf.push(',');
            let attrs: Vec<&str> = attributes(v.flags).collect();
            buf.push_str(&attrs.join(" "));
            buf.push_str("\r\n");
            if buf.len() >= PIECE_BYTES {
                sink(&buf)?;
                buf.clear();
            }
            Ok(())
        })?;
    }
    if !buf.is_empty() {
        sink(&buf)?;
    }
    Ok(())
}

pub fn export_csv(nodes: &Nodes, root: NodeId, mask: Mask<'_>) -> Result<String, ExportError> {
    collect(|sink| csv_chunks(nodes, root, mask, sink))
}

fn collect(f: impl FnOnce(&mut Sink<'_>) -> Result<(), ExportError>) -> Result<String, ExportError> {
    let mut out = String::new();
    f(&mut |s: &str| {
        out.push_str(s);
        Ok(())
    })?;
    Ok(out)
}

// ---------------------------------------------------------------------------------- JSON

fn quoted_time(times: &mut LocalTimes, ts: i64, out: &mut String) {
    if ts > 0 {
        out.push('"');
        times.put(ts, out);
        out.push('"');
    } else {
        out.push_str("null");
    }
}

/// A JSON document: metadata and `roots`, each a nested tree (folders carry `children`). One
/// element per line, indented by its level.
pub fn json_chunks(
    nodes: &Nodes,
    root: NodeId,
    mask: Mask<'_>,
    filter_text: &str,
    now: Option<f64>,
    sink: &mut Sink<'_>,
) -> Result<(), ExportError> {
    let mut buf = String::new();
    let generated = iso_time(now.unwrap_or_else(now_secs) as i64);
    let _ = write!(
        buf,
        "{{\"format\": \"{JSON_FORMAT}\", \"version\": {JSON_VERSION}, \"generator\": \"SpaceKnife {VERSION}\", \"generated_at\": \"{generated}\", \"filter\": "
    );
    if filter_text.is_empty() {
        buf.push_str("null");
    } else {
        buf.push('"');
        json_escape_into(filter_text, &mut buf);
        buf.push('"');
    }
    buf.push_str(",\n\"roots\": [");
    let mut times = LocalTimes::new();
    for (index, r) in export_roots(nodes, root).into_iter().enumerate() {
        let tree = ExportTree::new(nodes, r, mask, REPORT_ORDER, true, None)?;
        let mut open: Vec<u32> = Vec::new(); // levels of folders whose children list is open
        let mut later: Vec<bool> = Vec::new(); // a child was written into it already
        tree.walk(|el| -> Result<(), ExportError> {
            let level = el.level;
            while open.last().is_some_and(|&l| l >= level) {
                open.pop();
                later.pop();
                buf.push_str("]}");
            }
            if level == 0 {
                if index > 0 {
                    buf.push(',');
                }
                buf.push_str("\n{\"path\": \"");
                json_name_into(el.path, &mut buf);
                buf.push_str("\", ");
            } else {
                let l = later.last_mut().expect("an open folder");
                buf.push_str(if *l { ",\n" } else { "\n" });
                *l = true;
                for _ in 0..level {
                    buf.push_str("  ");
                }
                buf.push('{');
            }
            let v = row(&tree, el);
            buf.push_str("\"name\": \"");
            json_name_into(el.name, &mut buf);
            let _ = write!(
                buf,
                "\", \"type\": \"{}\", \"size\": {}, \"disk_size\": {}",
                v.kind, v.size, v.alloc
            );
            if el.is_dir {
                let _ = write!(buf, ", \"files\": {}, \"folders\": {}", v.files, v.dirs);
            }
            for (key, which) in [("modified", 0), ("created", 1), ("accessed", 2)] {
                let _ = write!(buf, ", \"{key}\": ");
                quoted_time(&mut times, tree.time(el.pos, which), &mut buf);
            }
            buf.push_str(", \"attributes\": [");
            for (i, a) in attributes(v.flags).enumerate() {
                if i > 0 {
                    buf.push_str(", ");
                }
                let _ = write!(buf, "\"{a}\"");
            }
            buf.push(']');
            if el.is_dir {
                buf.push_str(", \"children\": [");
                open.push(level);
                later.push(false);
            } else {
                buf.push('}');
            }
            if buf.len() >= PIECE_BYTES {
                sink(&buf)?;
                buf.clear();
            }
            Ok(())
        })?;
        for _ in 0..open.len() {
            buf.push_str("]}");
        }
    }
    buf.push_str("\n]}\n");
    sink(&buf)?;
    Ok(())
}

pub fn export_json(nodes: &Nodes, root: NodeId, mask: Mask<'_>, filter_text: &str) -> Result<String, ExportError> {
    collect(|sink| json_chunks(nodes, root, mask, filter_text, None, sink))
}

// ---------------------------------------------------------------------------------- HTML

const CSS: &str = r#"
:root{--bg:#08090B;--panel:#0E1013;--line:#1D2027;--line2:#2A2E37;--text:#F1F2F5;--text2:#AEB3BE;
--text3:#8F95A2;--accent:#8B9CFF;--track:#1A1D23;color-scheme:dark}
@media (prefers-color-scheme:light){:root{--bg:#F6F7F9;--panel:#FFFFFF;--line:#E7E9ED;--line2:#D6D9DF;
--text:#14161B;--text2:#454B57;--text3:#6A7080;--accent:#5464E8;--track:#EDEFF3;color-scheme:light}}
@media print{:root{--bg:#FFFFFF;--panel:#FFFFFF;--line:#DDDDDD;--line2:#CCCCCC;--text:#000000;
--text2:#333333;--text3:#555555;--accent:#4455CC;--track:#EEEEEE;color-scheme:light}}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--text);
font:13px/1.45 Geist,"Noto Sans SC",system-ui,-apple-system,"Segoe UI","Microsoft YaHei",sans-serif}
main{max-width:1080px;margin:0 auto;padding:32px 16px 48px}
.brand{font-size:11px;letter-spacing:.08em;text-transform:uppercase;color:var(--text3)}
h1{font-size:28px;line-height:1.2;font-weight:600;margin:6px 0 8px}
h2{font-size:15px;font-weight:600;margin:0 0 4px}
.mono,.path{font-family:"Geist Mono",ui-monospace,"Cascadia Mono",Menlo,monospace}
.path{color:var(--text2);word-break:break-all}
.meta,.note{color:var(--text3)}
.note{margin:0 0 12px}
.cards{display:grid;grid-template-columns:repeat(auto-fit,minmax(170px,1fr));gap:12px;margin:20px 0 8px}
.card{background:var(--panel);border:1px solid var(--line);border-radius:10px;padding:14px 16px}
.card .k{color:var(--text3);font-size:12px}
.card .v{font-size:22px;font-weight:600;margin-top:4px;font-variant-numeric:tabular-nums}
section{margin-top:32px}
.root+.root{margin-top:56px;padding-top:24px;border-top:1px solid var(--line2)}
.box{background:var(--panel);border:1px solid var(--line);border-radius:10px;padding:6px 0;overflow:hidden}
.row{display:grid;grid-template-columns:minmax(0,1fr) 160px 88px 56px;gap:12px;align-items:center;
padding:4px 14px 4px calc(14px + var(--d,0) * 18px)}
.row:hover{background:var(--track)}
.name{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.dir>.name{font-weight:600}
summary{list-style:none;cursor:pointer}
summary::-webkit-details-marker{display:none}
summary>.name::before{content:"\25B8";display:inline-block;width:14px;color:var(--text3)}
details[open]>summary>.name::before{content:"\25BE"}
.leaf>.name{padding-left:14px}
.bar{height:6px;background:var(--track);border-radius:3px;overflow:hidden}
.bar i{display:block;height:100%;background:var(--accent);border-radius:3px}
.num{text-align:right;font-variant-numeric:tabular-nums;white-space:nowrap}
.muted{color:var(--text3)}
table{width:100%;border-collapse:collapse}
th,td{padding:6px 14px;text-align:left;border-bottom:1px solid var(--line);vertical-align:top}
tr:last-child td{border-bottom:0}
th{color:var(--text3);font-weight:500;font-size:12px}
th.num,td.num{text-align:right}
td.path{word-break:break-all}
footer{margin-top:40px;color:var(--text3);font-size:12px}
@media (max-width:640px){.row{grid-template-columns:minmax(0,1fr) 76px 48px}.row .bar{display:none}
h1{font-size:22px}.hide-sm{display:none}}
"#;

/// Bounds of the HTML report.
#[derive(Clone, Copy, Debug)]
pub struct HtmlOptions {
    /// levels of the folder structure
    pub depth: u32,
    /// largest children per folder
    pub top: usize,
    /// largest files listed
    pub largest: usize,
    /// largest extensions listed
    pub extensions: usize,
    /// unix seconds ("generated at")
    pub now: Option<f64>,
}

impl Default for HtmlOptions {
    fn default() -> Self {
        HtmlOptions { depth: 4, top: 30, largest: 50, extensions: 20, now: None }
    }
}

fn share(part: i64, whole: i64) -> f64 {
    if whole > 0 { part as f64 / whole as f64 } else { 0.0 }
}

fn html_row(out: &mut String, name: &str, alloc: i64, whole: i64, depth: u32, cls: &str, tag: &str) {
    let pct = format!("{:.1}%", share(alloc, whole) * 100.0);
    let name = html_escape(name);
    let _ = write!(
        out,
        "<{tag} class=\"row {cls}\" style=\"--d:{depth}\"><span class=\"name\" title=\"{name}\">{name}</span>\
         <span class=\"bar\"><i style=\"width:{pct}\"></i></span>\
         <span class=\"num\">{}</span><span class=\"num muted\">{pct}</span></{tag}>",
        html_escape(&fmt_size(alloc))
    );
}

fn structure(tree: &ExportTree<'_>, out: &mut String, depth: u32, top: usize) {
    fn folder(tree: &ExportTree<'_>, out: &mut String, pos: u32, level: u32, depth: u32, top: usize) {
        let whole = tree.sizes(pos).1;
        let kids = tree.children(pos);
        let (shown, rest) = kids.split_at(kids.len().min(top));
        for &k in shown {
            let name = text_of(tree.nodes.name_bytes(tree.ids[k as usize]));
            let alloc = tree.alloc_key(k);
            let is_dir = tree.is_dir[k as usize];
            if is_dir && level + 1 < depth && tree.has_children(k) {
                out.push_str("<details>");
                html_row(out, &name, alloc, whole, level, "dir", "summary");
                folder(tree, out, k, level + 1, depth, top);
                out.push_str("</details>");
            } else {
                html_row(out, &name, alloc, whole, level, if is_dir { "dir leaf" } else { "leaf" }, "div");
            }
        }
        if !rest.is_empty() {
            let alloc: i64 = rest.iter().map(|&k| tree.alloc_key(k)).sum();
            let label = fill(&tr(text::REST), &[("count", &thousands(rest.len() as i64))]);
            html_row(out, &label, alloc, whole, level, "leaf muted", "div");
        }
    }
    let all = tree.sizes(0).1;
    out.push_str("<details open>");
    html_row(out, &text_of(&tree.root_name), all, all, 0, "dir", "summary");
    folder(tree, out, 0, 1, depth, top);
    out.push_str("</details>");
}

fn visible_leaves(tree: &ExportTree<'_>) -> Vec<u32> {
    (0..tree.len() as u32)
        .filter(|&p| !tree.is_dir[p as usize] && tree.visible(p))
        .collect()
}

fn largest(tree: &ExportTree<'_>, out: &mut String, limit: usize) {
    let mut leaves: Vec<(i64, u32)> = visible_leaves(tree).into_iter().map(|p| (tree.alloc_key(p), p)).collect();
    if leaves.is_empty() || limit == 0 {
        let _ = write!(out, "<p class=\"note\">{}</p>", tr(text::EMPTY));
        return;
    }
    let by = |a: &(i64, u32), b: &(i64, u32)| b.0.cmp(&a.0).then(a.1.cmp(&b.1));
    if leaves.len() > limit {
        leaves.select_nth_unstable_by(limit - 1, by);
        leaves.truncate(limit);
    }
    leaves.sort_by(by);
    let _ = write!(
        out,
        "<div class=\"box\"><table><thead><tr><th class=\"num\">{}</th><th class=\"num hide-sm\">{}</th><th>{}</th></tr></thead><tbody>",
        tr(text::COL_DISK),
        tr(text::COL_MODIFIED),
        tr(text::COL_PATH)
    );
    for (alloc, pos) in leaves {
        let node = tree.ids[pos as usize];
        let when = local_time(tree.time(pos, 0), "%Y-%m-%d");
        let _ = write!(
            out,
            "<tr><td class=\"num\">{}</td><td class=\"num muted hide-sm\">{when}</td><td class=\"path mono\">{}</td></tr>",
            html_escape(&fmt_size(alloc)),
            html_escape(&text_of(&tree.nodes.path_bytes(node)))
        );
    }
    out.push_str("</tbody></table></div>");
}

fn extensions(tree: &ExportTree<'_>, out: &mut String, limit: usize) {
    let leaves = visible_leaves(tree);
    if leaves.is_empty() || limit == 0 {
        let _ = write!(out, "<p class=\"note\">{}</p>", tr(text::EMPTY));
        return;
    }
    let mut sums: HashMap<u16, (i64, i64)> = HashMap::new(); // ext -> (bytes, files)
    for &p in &leaves {
        let e = sums.entry(tree.nodes.ext(tree.ids[p as usize])).or_default();
        e.0 += tree.alloc_key(p);
        e.1 += 1;
    }
    let whole: i64 = sums.values().map(|v| v.0).sum();
    let mut order: Vec<(u16, i64, i64)> = sums.into_iter().map(|(k, (s, c))| (k, s, c)).collect();
    order.sort_by(|a, b| b.1.cmp(&a.1).then(b.2.cmp(&a.2)).then(a.0.cmp(&b.0)));
    order.truncate(limit);
    let _ = write!(
        out,
        "<div class=\"box\"><table><thead><tr><th>{}</th><th class=\"num\">{}</th><th class=\"num\">{}</th><th class=\"num\">{}</th></tr></thead><tbody>",
        tr(text::COL_EXT),
        tr(text::COL_FILES),
        tr(text::COL_DISK),
        tr(text::COL_SHARE)
    );
    for (ext, sum, count) in order {
        let name = tree.nodes.ext_name(ext);
        let label = if name.is_empty() { tr(text::NO_EXT) } else { format!(".{name}") };
        let _ = write!(
            out,
            "<tr><td class=\"mono\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num muted\">{:.1}%</td></tr>",
            html_escape(&label),
            thousands(count),
            html_escape(&fmt_size(sum)),
            share(sum, whole) * 100.0
        );
    }
    out.push_str("</tbody></table></div>");
}

/// A self-contained HTML report: totals, the folder structure (`depth` levels, `top` largest
/// children each), the `largest` files and the `extensions` largest extensions.
pub fn export_html(
    nodes: &Nodes,
    root: NodeId,
    mask: Mask<'_>,
    filter_text: &str,
    opts: &HtmlOptions,
) -> Result<String, ExportError> {
    let roots = export_roots(nodes, root);
    let now = opts.now.unwrap_or_else(now_secs);
    let stamp = local_time(now as i64, "%Y-%m-%d %H:%M");
    let subject = if roots.len() == 1 {
        text_of(&nodes.path_bytes(roots[0])).into_owned()
    } else {
        fill(&tr(text::ROOTS), &[("count", &roots.len().to_string())])
    };
    let mut out = String::new();
    let _ = write!(
        out,
        "<!doctype html>\n<html lang=\"{}\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <meta name=\"generator\" content=\"SpaceKnife {}\">\
         <title>{} · {}</title><style>{CSS}</style></head><body><main>",
        language(),
        html_escape(VERSION),
        html_escape(&tr(text::TITLE)),
        html_escape(&subject)
    );
    let mut meta = vec![fill(&tr(text::GENERATED), &[("time", &stamp)])];
    if !filter_text.is_empty() {
        meta.push(fill(&tr(text::FILTER), &[("expr", filter_text)]));
    }
    for r in roots {
        let tree = ExportTree::new(nodes, r, mask, REPORT_ORDER, true, Some(now))?;
        let (size, alloc) = tree.sizes(0);
        let (files, folders) = tree.counts(0);
        let cards = [
            (tr(text::DISK), fmt_size(alloc)),
            (tr(text::LOGICAL), fmt_size(size)),
            (tr(if tree.masked { text::MATCHED } else { text::FILES }), thousands(files as i64)),
            (tr(text::FOLDERS), thousands(folders as i64)),
        ];
        let _ = write!(
            out,
            "<div class=\"root\"><header><div class=\"brand\">SpaceKnife</div><h1>{}</h1><p class=\"path\">{}</p>\
             <p class=\"meta\">{}</p></header><div class=\"cards\">",
            html_escape(&tr(text::TITLE)),
            html_escape(&text_of(&tree.root_path)),
            html_escape(&meta.join(" · "))
        );
        for (k, v) in cards {
            let _ = write!(
                out,
                "<div class=\"card\"><div class=\"k\">{}</div><div class=\"v\">{}</div></div>",
                html_escape(&k),
                html_escape(&v)
            );
        }
        let note = fill(
            &tr(text::STRUCTURE_NOTE),
            &[("top", &opts.top.to_string()), ("depth", &opts.depth.to_string())],
        );
        let _ = write!(
            out,
            "</div><section><h2>{}</h2><p class=\"note\">{}</p><div class=\"box\">",
            tr(text::STRUCTURE),
            html_escape(&note)
        );
        structure(&tree, &mut out, opts.depth, opts.top);
        let _ = write!(out, "</div></section><section><h2>{}</h2>", tr(text::LARGEST));
        largest(&tree, &mut out, opts.largest);
        let _ = write!(out, "</section><section><h2>{}</h2>", tr(text::EXTENSIONS));
        extensions(&tree, &mut out, opts.extensions);
        out.push_str("</section></div>");
    }
    let base = tr(if UNIT_BASE == 1000 { text::BASE_1000 } else { text::BASE_1024 });
    let footer = fill(&tr(text::FOOTER), &[("version", VERSION), ("base", &base)]);
    let _ = writeln!(out, "<footer>{}</footer></main></body></html>", html_escape(&footer));
    Ok(out)
}

// ---------------------------------------------------------------------------------- registry

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormatKey {
    Csv,
    Json,
    Html,
}

/// A built-in data format.
#[derive(Debug, PartialEq, Eq)]
pub struct ExportFormat {
    /// "csv" | "json" | "html"
    pub key: &'static str,
    pub which: FormatKey,
    /// the Chinese source text (message id) of [`ExportFormat::title`]
    pub title_id: &'static str,
    pub extension: &'static str,
    pub media_type: &'static str,
    /// write a UTF-8 BOM: Excel reads a CSV without one in the ANSI code page
    pub bom: bool,
}

impl ExportFormat {
    /// User-facing, in the current interface language.
    pub fn title(&self) -> String {
        tr(self.title_id)
    }

    /// The output for the tree below `root` (0: every scan root), in pieces.
    pub fn chunks(
        &self,
        nodes: &Nodes,
        root: NodeId,
        mask: Mask<'_>,
        filter_text: &str,
        sink: &mut Sink<'_>,
    ) -> Result<(), ExportError> {
        match self.which {
            FormatKey::Csv => csv_chunks(nodes, root, mask, sink),
            FormatKey::Json => json_chunks(nodes, root, mask, filter_text, None, sink),
            FormatKey::Html => {
                let page = export_html(nodes, root, mask, filter_text, &HtmlOptions::default())?;
                sink(&page)?;
                Ok(())
            }
        }
    }
}

pub static FORMATS: [ExportFormat; 3] = [
    ExportFormat {
        key: "csv",
        which: FormatKey::Csv,
        title_id: text::CSV,
        extension: ".csv",
        media_type: "text/csv",
        bom: true,
    },
    ExportFormat {
        key: "json",
        which: FormatKey::Json,
        title_id: text::JSON,
        extension: ".json",
        media_type: "application/json",
        bom: false,
    },
    ExportFormat {
        key: "html",
        which: FormatKey::Html,
        title_id: text::HTML,
        extension: ".html",
        media_type: "text/html",
        bom: false,
    },
];

/// A format by its key ("csv", "json", "html"), any case.
pub fn format(key: &str) -> Option<&'static ExportFormat> {
    let k = key.trim().to_lowercase();
    FORMATS.iter().find(|f| f.key == k)
}
