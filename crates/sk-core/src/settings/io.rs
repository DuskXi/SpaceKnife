//! Settings files in and out (W7 「高级」, UI-W7-36): SpaceKnife's own export, SpaceSniffer's config.
//!
//! * Export: `{"format": "spaceknife-settings", "version": 1, "values": {...}, "classes": [...],
//!   "class_folders": [...]}` — the settings that differ from the defaults, the file class table
//!   when the user edited it, the folder class choices when there are any.
//! * Import reads such a file, or a `SpaceSnifferConfig.xml` (SpaceSniffer 2.0.5.18 writes one
//!   element per setting, colours as `0x00BBGGRR`), and returns a *plan*: the changes it would
//!   make, what it cannot map, for the user to confirm.
//!
//! SpaceSniffer mapping, reasonably: settings with a SpaceKnife counterpart are taken over, scaled
//! where the scales differ; its theme colours have none and are listed as skipped. File groups
//! become file classes with SpaceSniffer's names and extension lists; a group known to SpaceKnife
//! keeps its id (and its old names as filter aliases); SpaceSniffer's default primary colours give
//! way to the theme's, a colour the user picked there is kept. SpaceKnife classes SpaceSniffer has
//! no group for (文档) stay. Export templates equal to the built-in ones need nothing; custom ones
//! are listed.
//!
//! What lives in other modules comes in through [`ImportContext`] / the export arguments (the
//! active class table, the folder class choices of `pathclass`, the built-in export template
//! names of `export`): the caller (the server) passes them.

use std::path::Path;
use std::sync::Arc;

use serde_json::{Map, Value, json};
use sk_protocol::classes::{ClassEdit, OTHER, builtin_classes, localized};
use sk_protocol::filter::FilterClassInfo;
use sk_protocol::filter_syntax::casefold;
use sk_protocol::settings::{ImportSource, SettingChange, SettingValue};

use super::{Values, by_key, defaults, validate};
use crate::classes::{self, ClassTable};
use crate::i18n::{self, t};
use crate::tr;

pub const FORMAT: &str = "spaceknife-settings";
pub const MAX_BYTES: u64 = 4 << 20;

/// SpaceSniffer element -> SpaceKnife key (switches).
const SS_BOOL: &[(&str, &str)] = &[
    ("showFreeSpace", "treemap.show_free"),
    ("showUnknownSpace", "treemap.show_unknown"),
    ("showFileTooltipDateTime", "general.hover_dates"),
    ("showFileTooltipAge", "general.hover_age"),
    ("useDynaSmooth", "treemap.dynasmooth"),
    ("useSorting", "treemap.sort_by_size"),
    ("scanADS", "scan.ads"),
    ("watchDog", "scan.monitor"),
    ("logPopup", "general.log_auto_open"),
    ("logWarning", "general.log_scan_warnings"),
    ("flashElement", "treemap.flash_changes"),
    ("flashWindow", "general.flash_on_done"),
    ("keepTooltip", "general.hover_keep_open"),
    ("mouseHover", "appearance.hover_highlight"),
    ("mouseTrail", "appearance.mouse_trail"),
    ("hilightShadow", "appearance.selection_shadow"),
];
const SS_DEFAULT_COLORS: &[&str] = &[
    "0x000000ff",
    "0x0000ffff",
    "0x00ff0000",
    "0x0000ff00",
    "0x00c080ff",
];
/// SpaceSniffer's default label size (pt): SpaceKnife's 100 %.
const SS_FONT_SIZE: f64 = 7.0;
/// SpaceSniffer's default border contrast: SpaceKnife's 10.
const SS_BORDER: f64 = 30.0;

/// What an import would change.
#[derive(Clone, Debug)]
pub struct ImportPlan {
    pub source: ImportSource,
    /// Only what changes
    pub values: Values,
    /// The keys of `values` in the order they were read: the settings' page order for a
    /// SpaceKnife export, the file's for SpaceSniffer's (how 0.3.4 listed the changes)
    order: Vec<String>,
    /// A new class table, if it changes
    pub classes: Option<Arc<ClassTable>>,
    /// New folder choices (UI-W7-38) as (path, class id), if they change
    pub folders: Option<Vec<(String, String)>>,
    /// Notes, in the language of the moment
    pub skipped: Vec<String>,
}

impl ImportPlan {
    fn new(source: ImportSource) -> ImportPlan {
        ImportPlan {
            source,
            values: Values::new(),
            order: Vec::new(),
            classes: None,
            folders: None,
            skipped: Vec::new(),
        }
    }

    /// The changes as `settings.import` lists them (names in the current language), in the order
    /// they were read ([`ImportPlan::order`]).
    pub fn changes(&self, current: &Values) -> Vec<SettingChange> {
        let rest = self.values.keys().filter(|k| !self.order.contains(k));
        self.order
            .iter()
            .chain(rest)
            .filter_map(|k| self.values.get_key_value(k))
            .map(|(k, v)| SettingChange {
                key: k.clone(),
                name: by_key(k).map_or_else(|| k.clone(), |s| t(s.name).to_owned()),
                before: current.get(k).cloned().unwrap_or_else(|| v.clone()),
                after: v.clone(),
            })
            .collect()
    }
}

/// A file that cannot be imported (the message is in the current language).
#[derive(Clone, Debug, thiserror::Error)]
#[error("{0}")]
pub struct ImportError(pub String);

/// The rest of the backend an import compares against.
#[derive(Clone, Copy)]
pub struct ImportContext<'a> {
    /// The active class table ([`classes::active`])
    pub active: &'a ClassTable,
    /// The folder class choices now (`pathclass::folders()`), as (path, class id)
    pub folders: &'a [(String, String)],
    /// Names of the built-in export templates (`export::BUILTIN_TEMPLATES`)
    pub builtin_templates: &'a [&'a str],
}

// --------------------------------------------------------------------------- export

/// The export document.
pub fn export_document(values: &Values, table: &ClassTable, folders: &[(String, String)]) -> Value {
    let changed: Map<String, Value> = values
        .iter()
        .filter(|(k, v)| defaults().get(*k) != Some(*v))
        .map(|(k, v)| (k.clone(), serde_json::to_value(v).unwrap_or(Value::Null)))
        .collect();
    let classes = if table.custom() {
        json!(table.classes())
    } else {
        Value::Null
    };
    let folders = if folders.is_empty() {
        Value::Null
    } else {
        Value::Array(
            folders
                .iter()
                .map(|(p, c)| json!({"path": p, "class": c}))
                .collect(),
        )
    };
    json!({
        "format": FORMAT,
        "version": 1,
        "values": changed,
        "classes": classes,
        "class_folders": folders,
    })
}

/// Write the export document to `path` (atomically).
pub fn export_to(
    path: &Path,
    values: &Values,
    table: &ClassTable,
    folders: &[(String, String)],
) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(&export_document(values, table, folders))
        .map_err(std::io::Error::other)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.tmp"));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

// --------------------------------------------------------------------------- import

/// What importing `path` would change, compared with `current` and `ctx`.
pub fn read_plan(
    path: &Path,
    current: &Values,
    ctx: &ImportContext<'_>,
) -> Result<ImportPlan, ImportError> {
    let unreadable = |e: std::io::Error| {
        ImportError(tr!(
            "读不了 {path}：{reason}",
            path = path,
            reason = e.to_string()
        ))
    };
    let size = std::fs::metadata(path).map_err(unreadable)?.len();
    if size > MAX_BYTES {
        return Err(ImportError(t("文件太大，不像是设置文件").to_owned()));
    }
    let raw = std::fs::read(path).map_err(unreadable)?;
    let start = raw
        .iter()
        .position(|b| !matches!(b, 0xEF | 0xBB | 0xBF | b' ' | b'\t' | b'\r' | b'\n'))
        .unwrap_or(raw.len());
    match raw.get(start) {
        Some(b'{') => plan_spaceknife(&raw, current, ctx),
        Some(b'<') => plan_spacesniffer(&raw, current, ctx),
        _ => Err(unknown_format()),
    }
}

fn unknown_format() -> ImportError {
    ImportError(
        t("不认识这个文件：既不是 SpaceKnife 导出的设置，也不是 SpaceSniffer 的 SpaceSnifferConfig.xml")
            .to_owned(),
    )
}

fn to_setting(v: &Value) -> Option<SettingValue> {
    serde_json::from_value::<SettingValue>(v.clone()).ok()
}

/// Keep the values that are valid, settable and different from now.
fn finish(plan: &mut ImportPlan, wanted: Vec<(String, Value)>, current: &Values) {
    for (key, value) in wanted {
        let Some(spec) = by_key(&key) else {
            plan.skipped.push(tr!(
                "未知设置 {key}（来自其他版本），已跳过",
                key = key.as_str()
            ));
            continue;
        };
        let setting = to_setting(&value);
        if !spec.reason.is_empty() {
            if setting.as_ref() != current.get(&key) {
                plan.skipped
                    .push(tr!("当前版本不可修改，已跳过：{name}", name = t(spec.name)));
            }
            continue;
        }
        let checked = setting.and_then(|s| {
            let mut merged = current.clone();
            merged.insert(key.clone(), s);
            validate(&merged).ok().and_then(|mut v| v.remove(&key))
        });
        let Some(checked) = checked else {
            let shown = match &value {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            plan.skipped.push(tr!(
                "设置值不合法，已跳过：{key} = {value}",
                key = key.as_str(),
                value = shown
            ));
            continue;
        };
        let changes = current.get(&key) != Some(&checked);
        if changes && plan.values.insert(key.clone(), checked).is_none() {
            plan.order.push(key);
        }
    }
}

fn plan_spaceknife(
    raw: &[u8],
    current: &Values,
    ctx: &ImportContext<'_>,
) -> Result<ImportPlan, ImportError> {
    let bad_json =
        |reason: String| ImportError(tr!("设置文件不是有效的 JSON：{reason}", reason = reason));
    let text = std::str::from_utf8(raw).map_err(|e| bad_json(e.to_string()))?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let doc: Value = serde_json::from_str(text).map_err(|e| bad_json(e.to_string()))?;
    let Some(doc) = doc
        .as_object()
        .filter(|d| d.get("format").and_then(Value::as_str) == Some(FORMAT))
    else {
        return Err(unknown_format());
    };
    let version = doc.get("version").and_then(Value::as_i64).unwrap_or(1);
    if version > 1 {
        return Err(ImportError(tr!(
            "这份设置来自更新版本的 SpaceKnife（格式 {version}），无法导入",
            version = version
        )));
    }
    let mut plan = ImportPlan::new(ImportSource::Spaceknife);
    let empty = Map::new();
    let values = doc
        .get("values")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    // an export lists only non-defaults: everything else goes back to its default
    let mut wanted: Vec<(String, Value)> = super::registry()
        .iter()
        .map(|s| {
            let v = values
                .get(s.key)
                .cloned()
                .unwrap_or_else(|| serde_json::to_value(&s.default).unwrap_or(Value::Null));
            (s.key.to_owned(), v)
        })
        .collect();
    wanted.extend(
        values
            .iter()
            .filter(|(k, _)| by_key(k).is_none())
            .map(|(k, v)| (k.clone(), v.clone())),
    );
    finish(&mut plan, wanted, current);

    // an export from before the light theme has no color_light: its colours are the user's for
    // both themes, as in classes.json v1
    let table = match doc
        .get("classes")
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty())
    {
        None => ClassTable::builtin(),
        Some(items) => {
            let classes = items
                .iter()
                .map(|c| {
                    let c = if c.get("color_light").is_some() {
                        c.clone()
                    } else {
                        classes::colors_for_both(c.clone())
                    };
                    serde_json::from_value::<FilterClassInfo>(c)
                })
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| bad_json(e.to_string()))?;
            Arc::new(ClassTable::new(classes).map_err(bad_json)?)
        }
    };
    if table.classes() != ctx.active.classes() {
        plan.classes = Some(table);
    }
    let folders: Option<Vec<(String, String)>> = match doc.get("class_folders") {
        None | Some(Value::Null) => Some(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|f| {
                let path = f.get("path")?;
                let class = f.get("class")?;
                let s = |v: &Value| v.as_str().map_or_else(|| v.to_string(), str::to_owned);
                Some((s(path), s(class)))
            })
            .collect(),
        Some(_) => None,
    };
    let folders = folders.unwrap_or_else(|| {
        plan.skipped
            .push(t("文件夹归类的格式不对，已忽略").to_owned());
        Vec::new()
    });
    let mut folders = folders;
    folders.sort();
    let mut now = ctx.folders.to_vec();
    now.sort();
    if folders != now {
        plan.folders = Some(folders);
    }
    Ok(plan)
}

fn child<'a>(el: roxmltree::Node<'a, 'a>, tag: &str) -> Option<roxmltree::Node<'a, 'a>> {
    el.children().find(|c| c.has_tag_name(tag))
}

fn text_of(el: Option<roxmltree::Node<'_, '_>>, tag: &str) -> Option<String> {
    let c = child(el?, tag)?;
    c.text().map(|t| t.trim().to_owned())
}

fn clamp(v: i64, key: &str) -> i64 {
    let spec = by_key(key).expect("a registered key");
    let lo = spec.min.map_or(v, |m| m as i64);
    let hi = spec.max.map_or(v, |m| m as i64);
    v.min(hi).max(lo)
}

/// `0x00BBGGRR` (a Delphi TColor) -> `#RRGGBB`; SpaceSniffer's own defaults -> theme ("").
fn ss_color(text: Option<&str>) -> String {
    let Some(text) = text.map(str::trim).filter(|t| !t.is_empty()) else {
        return String::new();
    };
    let low = text.to_lowercase();
    if SS_DEFAULT_COLORS.contains(&low.as_str()) {
        return String::new();
    }
    let parsed = match low.strip_prefix("0x") {
        Some(hex) => i64::from_str_radix(hex, 16),
        None => low.parse::<i64>(),
    };
    let Ok(v) = parsed else { return String::new() };
    let (r, g, b) = (v & 0xFF, (v >> 8) & 0xFF, (v >> 16) & 0xFF);
    format!("#{r:02X}{g:02X}{b:02X}")
}

fn decode_xml(raw: &[u8]) -> Result<String, String> {
    if raw.starts_with(&[0xFF, 0xFE]) || raw.starts_with(&[0xFE, 0xFF]) {
        let le = raw[0] == 0xFF;
        let units: Vec<u16> = raw[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&c| {
                if le {
                    u16::from_le_bytes(c)
                } else {
                    u16::from_be_bytes(c)
                }
            })
            .collect();
        return String::from_utf16(&units).map_err(|e| e.to_string());
    }
    let text = std::str::from_utf8(raw).map_err(|e| e.to_string())?;
    Ok(text.strip_prefix('\u{feff}').unwrap_or(text).to_owned())
}

fn plan_spacesniffer(
    raw: &[u8],
    current: &Values,
    ctx: &ImportContext<'_>,
) -> Result<ImportPlan, ImportError> {
    let bad_xml = |reason: String| {
        ImportError(tr!(
            "SpaceSniffer 配置不是有效的 XML：{reason}",
            reason = reason
        ))
    };
    let text = decode_xml(raw).map_err(bad_xml)?;
    let doc = roxmltree::Document::parse(&text).map_err(|e| bad_xml(e.to_string()))?;
    let root = doc.root_element();
    if !root.has_tag_name("SpaceSnifferConfig") {
        return Err(unknown_format());
    }
    let mut plan = ImportPlan::new(ImportSource::Spacesniffer);
    let colors = child(root, "colors");
    let mut wanted: Vec<(String, Value)> = Vec::new();
    let flag = |tag: &str, where_: Option<roxmltree::Node<'_, '_>>| {
        text_of(where_, tag).map(|v| v.eq_ignore_ascii_case("true"))
    };
    let number = |tag: &str, where_: Option<roxmltree::Node<'_, '_>>| {
        text_of(where_, tag).and_then(|v| v.parse::<i64>().ok())
    };
    for &(tag, key) in SS_BOOL {
        let v = if child(root, tag).is_some() {
            flag(tag, Some(root))
        } else {
            flag(tag, colors)
        };
        if let Some(v) = v {
            wanted.push((key.to_owned(), Value::Bool(v)));
        }
    }
    if let Some(n) = number("minElementSizePx", Some(root)) {
        wanted.push(("treemap.min_element".into(), json!(n)));
    }
    if let Some(n) = number("initialDetailLevel", Some(root)) {
        wanted.push(("treemap.initial_detail".into(), json!(n)));
    }
    if let Some(n) = number("fontSize", Some(root)) {
        let scaled = (n as f64 / SS_FONT_SIZE * 100.0).round_ties_even() as i64;
        wanted.push((
            "treemap.font_scale".into(),
            json!(clamp(scaled, "treemap.font_scale")),
        ));
    }
    match text_of(Some(root), "initialStyle").as_deref() {
        Some("flat") => wanted.push(("appearance.default_color_mode".into(), json!("flat"))),
        Some("fileClasses") => {
            wanted.push(("appearance.default_color_mode".into(), json!("class")))
        }
        _ => {}
    }
    if let Some(n) = number("levelContrast", colors) {
        wanted.push((
            "appearance.level_contrast".into(),
            json!(clamp(n, "appearance.level_contrast")),
        ));
    }
    if let Some(n) = number("borderContrast", colors) {
        let scaled = (n as f64 * 10.0 / SS_BORDER).round_ties_even() as i64;
        wanted.push((
            "appearance.border_contrast".into(),
            json!(clamp(scaled, "appearance.border_contrast")),
        ));
    }
    if let Some(n) = number("hilightHaloLevels", colors) {
        wanted.push((
            "appearance.halo_levels".into(),
            json!(clamp(n, "appearance.halo_levels")),
        ));
    }
    finish(&mut plan, wanted, current);
    if colors.is_some_and(|c| {
        ["driveColor", "folderColor", "fileColor"]
            .iter()
            .any(|t| child(c, t).is_some())
    }) {
        plan.skipped.push(
            t("SpaceSniffer 的配色（磁盘 / 文件夹 / 文件 / 可用 / 未知空间颜色）：SpaceKnife 的树图配色固定，不导入")
                .to_owned(),
        );
    }
    for (tag, note) in [
        (
            "refreshDelayMs",
            "刷新间隔 refreshDelayMs：SpaceKnife 自动调节刷新，不需要",
        ),
        (
            "zoomTimeMs",
            "缩放时长 zoomTimeMs：单位与 SpaceKnife 不同，不导入（可在「树图 › 缩放动画时长」里调）",
        ),
        (
            "useRealZoom",
            "实时缩放 useRealZoom（DynaZoom）：还没有实现",
        ),
        (
            "proportions",
            "元素比例 proportions：两边的刻度不同，不导入（可在「树图 › 元素比例」里调）",
        ),
    ] {
        if child(root, tag).is_some() {
            plan.skipped.push(t(note).to_owned());
        }
    }
    if let Some(groups) = child(root, "fileGroups") {
        let table = classes_from(groups, &mut plan).map_err(ImportError)?;
        if table.classes() != ctx.active.classes() {
            plan.classes = Some(Arc::new(table));
        }
    }
    for cfg in root
        .descendants()
        .filter(|n| n.has_tag_name("exportConfig"))
    {
        let name = text_of(Some(cfg), "description").unwrap_or_default();
        if !name.is_empty() && !ctx.builtin_templates.contains(&name.as_str()) {
            plan.skipped.push(tr!(
                "自定义导出模板「{name}」：SpaceKnife 还不能保存自定义模板",
                name = name.as_str()
            ));
        }
    }
    Ok(plan)
}

fn classes_from(
    groups: roxmltree::Node<'_, '_>,
    plan: &mut ImportPlan,
) -> Result<ClassTable, String> {
    let find_builtin = |name: &str| {
        let folded = casefold(name);
        builtin_classes().iter().find(|c| {
            std::iter::once(&c.id)
                .chain(std::iter::once(&c.name))
                .chain(&c.aliases)
                .any(|s| casefold(s) == folded)
        })
    };
    let mut edits: Vec<ClassEdit> = Vec::new();
    let mut used: Vec<String> = Vec::new();
    for g in groups.descendants().filter(|n| n.has_tag_name("fileGroup")) {
        let name = text_of(Some(g), "name").unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        let cid = find_builtin(&name)
            .filter(|k| !used.contains(&k.id) && k.id != OTHER)
            .map(|k| k.id.clone());
        if let Some(c) = &cid {
            used.push(c.clone());
        }
        // SpaceSniffer has one set of colours: they go to both themes
        let color = ss_color(text_of(Some(g), "color").as_deref());
        edits.push(ClassEdit {
            id: cid,
            name,
            extensions: vec![text_of(Some(g), "extensions").unwrap_or_default()],
            color: color.clone(),
            color_light: color,
        });
    }
    let lang = i18n::language();
    for c in builtin_classes() {
        // SpaceKnife-only classes stay; 「其他」 last
        if !used.contains(&c.id) && c.id != OTHER {
            edits.push(ClassEdit {
                id: Some(c.id.clone()),
                name: c.name.clone(),
                extensions: c.extensions.clone(),
                color: String::new(),
                color_light: String::new(),
            });
            plan.skipped.push(tr!(
                "保留 SpaceKnife 的「{name}」类别（SpaceSniffer 没有这一类）",
                name = localized(c, lang.as_str()).name
            ));
        }
    }
    let other = builtin_classes()
        .iter()
        .find(|c| c.id == OTHER)
        .expect("an other class");
    edits.push(ClassEdit {
        id: Some(OTHER.to_owned()),
        name: other.name.clone(),
        extensions: Vec::new(),
        color: String::new(),
        color_light: String::new(),
    });
    classes::build(&edits)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SS_DEFAULT: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/SpaceSnifferConfig.default.xml"
    );

    fn ctx(active: &ClassTable) -> ImportContext<'_> {
        ImportContext {
            active,
            folders: &[],
            builtin_templates: &[],
        }
    }

    #[test]
    fn spacesniffer_plan() {
        let _g = crate::i18n::TEST_LOCK.lock();
        let builtin = ClassTable::builtin();
        let plan = read_plan(Path::new(SS_DEFAULT), defaults(), &ctx(&builtin)).unwrap();
        assert_eq!(plan.source, ImportSource::Spacesniffer);
        let text = std::fs::read_to_string(SS_DEFAULT).unwrap();
        let doc = roxmltree::Document::parse(&text).unwrap();
        let classes = plan.classes.as_ref().unwrap();
        let by_name = |n: &str| classes.classes().iter().find(|c| c.name == n).unwrap();
        let mut groups = 0;
        for g in doc.descendants().filter(|n| n.has_tag_name("fileGroup")) {
            let name = text_of(Some(g), "name").unwrap();
            let mut exts: Vec<String> = Vec::new();
            for e in text_of(Some(g), "extensions")
                .unwrap_or_default()
                .split(';')
            {
                if !e.is_empty() && !exts.iter().any(|x| x == e) {
                    exts.push(e.to_owned());
                }
            }
            assert_eq!(by_name(&name).extensions, exts, "{name}");
            assert_eq!(by_name(&name).color, ""); // SpaceSniffer's default primaries give way to the theme
            groups += 1;
        }
        assert_eq!(groups, 5);
        let video = by_name("Video/Movies");
        assert_eq!(video.id, "video");
        assert!(video.aliases.iter().any(|a| a == "视频"));
        assert_eq!(classes.classes().last().unwrap().id, OTHER);
        by_name("文档");
        assert_eq!(plan.values["appearance.default_color_mode"], "flat".into());
        assert_eq!(plan.values["treemap.initial_detail"], SettingValue::Int(3));
        assert!(!plan.values.contains_key("treemap.min_element")); // 10 in both
        assert!(plan.skipped.iter().any(|s| s.contains("zoomTimeMs")));
        let changes = plan.changes(defaults());
        assert!(
            changes
                .iter()
                .any(|c| c.key == "treemap.initial_detail" && c.before == SettingValue::Int(5))
        );
    }

    #[test]
    fn spacesniffer_custom_colour() {
        let _g = crate::i18n::TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        let text = std::fs::read_to_string(SS_DEFAULT).unwrap().replacen(
            "<color>0x000000FF</color>",
            "<color>0x00336699</color>",
            1,
        );
        let file = tmp.path().join("c.xml");
        std::fs::write(&file, text).unwrap();
        let builtin = ClassTable::builtin();
        let plan = read_plan(&file, defaults(), &ctx(&builtin)).unwrap();
        let classes = plan.classes.unwrap();
        let video = classes
            .classes()
            .iter()
            .find(|c| c.name == "Video/Movies")
            .unwrap();
        assert_eq!(
            (video.color.as_str(), video.color_light.as_str()),
            ("#996633", "#996633")
        );
    }

    #[test]
    fn export_from_before_themes_keeps_colours_in_both() {
        let tmp = tempfile::tempdir().unwrap();
        let mut rows: Vec<Value> = builtin_classes()
            .iter()
            .map(|c| serde_json::to_value(c).unwrap())
            .collect();
        rows[1]["color"] = json!("#123456");
        let old: Vec<Value> = rows
            .iter()
            .map(|r| {
                let mut r = r.clone();
                r.as_object_mut().unwrap().remove("color_light");
                r
            })
            .collect();
        for (name, items) in [("old.json", &old), ("new.json", &rows)] {
            let doc = json!({"format": FORMAT, "version": 1, "values": {}, "classes": items});
            std::fs::write(tmp.path().join(name), doc.to_string()).unwrap();
        }
        let builtin = ClassTable::builtin();
        let image = |file: &str| {
            let plan = read_plan(&tmp.path().join(file), defaults(), &ctx(&builtin)).unwrap();
            let c = plan.classes.unwrap().by_id("image").unwrap().clone();
            (c.color, c.color_light)
        };
        assert_eq!(image("old.json"), ("#123456".into(), "#123456".into()));
        assert_eq!(image("new.json"), ("#123456".into(), "".into()));
    }

    #[test]
    fn bad_files() {
        let tmp = tempfile::tempdir().unwrap();
        let builtin = ClassTable::builtin();
        for (name, body) in [("a.json", "{nope"), ("b.xml", "<x/>"), ("c.txt", "hello")] {
            std::fs::write(tmp.path().join(name), body).unwrap();
            assert!(
                read_plan(&tmp.path().join(name), defaults(), &ctx(&builtin)).is_err(),
                "{name}"
            );
        }
        assert!(read_plan(&tmp.path().join("missing.json"), defaults(), &ctx(&builtin)).is_err());
    }

    #[test]
    fn export_round_trip() {
        let _g = crate::i18n::TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("mine.json");
        let mut values = defaults().clone();
        values.insert("treemap.min_element".into(), SettingValue::Int(14));
        let folders = vec![("C:\\games".to_owned(), "game".to_owned())];
        export_to(&out, &values, &ClassTable::builtin(), &folders).unwrap();
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        assert_eq!(doc["format"], FORMAT);
        assert_eq!(doc["values"], json!({"treemap.min_element": 14}));
        assert_eq!(doc["classes"], Value::Null);
        // importing it into the defaults: that one value, those folders, no class change
        let builtin = ClassTable::builtin();
        let plan = read_plan(&out, defaults(), &ctx(&builtin)).unwrap();
        assert_eq!(plan.source, ImportSource::Spaceknife);
        assert_eq!(plan.values.len(), 1);
        assert!(plan.classes.is_none());
        assert_eq!(plan.folders, Some(folders.clone()));
        // into the values it came from: nothing
        let c = ImportContext {
            active: &builtin,
            folders: &folders,
            builtin_templates: &[],
        };
        let plan = read_plan(&out, &values, &c).unwrap();
        assert!(plan.values.is_empty() && plan.folders.is_none() && plan.skipped.is_empty());
    }

    #[test]
    fn changes_are_listed_in_the_settings_order() {
        // the import dialog shows the changes as settings.import lists them: 0.3.4 listed a
        // SpaceKnife export's in the settings' own order (page by page), a SpaceSniffer file's in
        // the order they were read from it; never A→Z by key
        let _g = crate::i18n::TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("mine.json");
        let mut values = defaults().clone();
        for (k, v) in [
            ("treemap.initial_detail", SettingValue::Int(7)),
            ("appearance.theme", SettingValue::Str("dark".into())),
            ("general.update_channel", SettingValue::Str("stable".into())),
            ("tags.name_red", SettingValue::Str("Delete me".into())),
        ] {
            assert!(by_key(k).is_some(), "{k}");
            values.insert(k.to_owned(), v);
        }
        export_to(&out, &values, &ClassTable::builtin(), &[]).unwrap();
        let builtin = ClassTable::builtin();
        let plan = read_plan(&out, defaults(), &ctx(&builtin)).unwrap();
        let keys: Vec<String> = plan
            .changes(defaults())
            .into_iter()
            .map(|c| c.key)
            .collect();
        let page_order: Vec<String> = super::super::registry()
            .iter()
            .map(|s| s.key.to_owned())
            .filter(|k| keys.contains(k))
            .collect();
        assert_eq!(keys.len(), 4, "{keys:?}");
        assert_eq!(keys, page_order);

        let plan = read_plan(Path::new(SS_DEFAULT), defaults(), &ctx(&builtin)).unwrap();
        let keys: Vec<String> = plan
            .changes(defaults())
            .into_iter()
            .map(|c| c.key)
            .collect();
        let at = |k: &str| keys.iter().position(|x| x == k).unwrap();
        // initialDetailLevel is read before initialStyle
        assert!(
            at("treemap.initial_detail") < at("appearance.default_color_mode"),
            "{keys:?}"
        );
    }
}
