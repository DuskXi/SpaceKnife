//! File-class lookups over the scanned extension table (`:class:` filter, treemap colours).
//!
//! The built-in definitions are shared data in [`sk_protocol::classes`]. The user can edit the
//! table (W7 「文件类别」): it is kept in `config_dir()/classes.json` and becomes the *active* table
//! every component reads through [`active`] — the filter engine, the layout's colour classes, the
//! parser vocabulary.
//!
//! M7.5: frontends see a built-in class the user has not renamed under its name in the interface
//! language ([`sk_protocol::classes::localized`]); [`build`] takes that name back as the built-in
//! one, so saving the editor in English is no rename.
//!
//! A file's class comes from its extension, except for the extensions in [`STREAM_SPLITS`], which
//! two kinds of file share: there the file's size decides ([`ClassTable::file_classes`]; the column
//! forms are [`ClassTable::splits`] and [`ClassTable::node_mask`]).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};

use parking_lot::RwLock;
use serde_json::Value;
use sk_protocol::classes::{ClassEdit, OTHER, builtin_classes, localized};
use sk_protocol::filter::FilterClassInfo;
use sk_protocol::filter_syntax::casefold;

use crate::i18n::{self, SOURCE, t};
use crate::tr;

/// Extensions a recording and a source file share, told apart by size alone (no content is read).
/// An MPEG transport stream is a run of fixed-size packets, 188 bytes (`.ts`) or 192 bytes (AVCHD /
/// Blu-ray `.mts`), so a recording's size is a whole number of packets; TypeScript sources (`.ts`,
/// `.mts`) are small text files. (extension, class of a recording, packet size)
pub const STREAM_SPLITS: &[(&str, &str, i64)] = &[("ts", "video", 188), ("mts", "video", 192)];
/// A whole number of packets this large is a recording.
pub const STREAM_ALIGNED_MIN: i64 = 1 << 20;
/// Anything this large is a recording: no source file is.
pub const STREAM_ANY_MIN: i64 = 16 << 20;

/// Classes a table may hold: the folder class column stores a class index below 127, and the view
/// rows' `cls` keeps 255 for "in no one class".
pub const MAX_CLASSES: usize = 127;

/// classes.json: version 2 has a colour per theme (`color` dark, `color_light` light). A version 1
/// file was written when there was only the dark theme: its colours are the user's for both themes.
pub const FILE_VERSION: i64 = 2;

/// A file this large is a transport-stream recording, not source code.
#[inline]
pub fn looks_like_stream(size: i64, packet: i64) -> bool {
    size >= STREAM_ANY_MIN || (size >= STREAM_ALIGNED_MIN && size % packet == 0)
}

/// A [`STREAM_SPLITS`] extension of one extension table, under one class table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Split {
    /// Index in the extension table
    pub ext_id: usize,
    pub packet: i64,
    /// The class of a recording
    pub stream: String,
    /// The classes of any other file with this extension (none: 「其他」)
    pub rest: Vec<String>,
}

/// Classes in display order (`other` last), with per-extension membership.
#[derive(Clone, Debug)]
pub struct ClassTable {
    classes: Vec<FilterClassInfo>,
    ids: Vec<String>,
    other_index: usize,
    /// lower-case extension -> indices of its classes, in table order
    ext_classes: HashMap<String, Vec<u8>>,
}

impl PartialEq for ClassTable {
    fn eq(&self, other: &Self) -> bool {
        self.classes == other.classes
    }
}

static DEFAULT: LazyLock<Arc<ClassTable>> = LazyLock::new(|| {
    Arc::new(ClassTable::new(builtin_classes().to_vec()).expect("the built-in class table is valid"))
});

impl ClassTable {
    /// A table from its classes; an error (English, for the log) when it is not a valid one.
    pub fn new(classes: Vec<FilterClassInfo>) -> Result<ClassTable, String> {
        if classes.len() > MAX_CLASSES {
            return Err(format!("a class table holds at most {MAX_CLASSES} classes"));
        }
        let ids: Vec<String> = classes.iter().map(|c| c.id.clone()).collect();
        let other_index = ids
            .iter()
            .position(|i| i == OTHER)
            .ok_or_else(|| "the class table needs an 'other' class".to_owned())?;
        let mut ext_classes: HashMap<String, Vec<u8>> = HashMap::new();
        for (i, c) in classes.iter().enumerate() {
            for ext in &c.extensions {
                let owners = ext_classes.entry(ext.to_lowercase()).or_default();
                if !owners.contains(&(i as u8)) {
                    owners.push(i as u8);
                }
            }
        }
        let mut names: HashMap<String, &str> = HashMap::new();
        for c in &classes {
            for name in std::iter::once(&c.id).chain(std::iter::once(&c.name)).chain(&c.aliases) {
                let owner = *names.entry(casefold(name)).or_insert(&c.id);
                if owner != c.id {
                    return Err(format!("class name {name:?} is used by two classes"));
                }
            }
        }
        Ok(ClassTable { classes, ids, other_index, ext_classes })
    }

    /// The built-in table.
    pub fn builtin() -> Arc<ClassTable> {
        DEFAULT.clone()
    }

    pub fn classes(&self) -> &[FilterClassInfo] {
        &self.classes
    }

    pub fn ids(&self) -> &[String] {
        &self.ids
    }

    pub fn other_index(&self) -> usize {
        self.other_index
    }

    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.ids.iter().position(|i| i == id)
    }

    pub fn by_id(&self, id: &str) -> Option<&FilterClassInfo> {
        self.index_of(id).map(|i| &self.classes[i])
    }

    /// Indices of the classes `ext` belongs to, in table order (empty: 「其他」).
    pub fn class_indices_of(&self, ext: &str) -> &[u8] {
        let found = match self.ext_classes.get(ext) {
            Some(v) => Some(v),
            None if ext.chars().any(char::is_uppercase) => self.ext_classes.get(&ext.to_lowercase()),
            None => None,
        };
        found.map_or(&[], Vec::as_slice)
    }

    /// Ids of the classes `ext` belongs to (empty: 「其他」).
    pub fn classes_of(&self, ext: &str) -> Vec<&str> {
        self.class_indices_of(ext).iter().map(|&i| self.ids[i as usize].as_str()).collect()
    }

    /// The class that colours `ext`: its first class, else `other`.
    pub fn color_class(&self, ext: &str) -> &str {
        self.class_indices_of(ext)
            .first()
            .map_or(OTHER, |&i| self.ids[i as usize].as_str())
    }

    /// Per extension-table id: the extension belongs to `class_id`. For `other` these are the
    /// extensions (including "no extension") that are in no class.
    pub fn ext_mask<E: AsRef<str>>(&self, class_id: &str, ext_table: &[E]) -> Vec<bool> {
        if class_id == OTHER {
            return ext_table.iter().map(|e| self.class_indices_of(e.as_ref()).is_empty()).collect();
        }
        let Some(idx) = self.index_of(class_id) else {
            return vec![false; ext_table.len()];
        };
        ext_table
            .iter()
            .map(|e| self.class_indices_of(e.as_ref()).contains(&(idx as u8)))
            .collect()
    }

    /// (class of a recording, packet size, other classes) when `ext` is split by size here: only
    /// while the recording's class still lists the extension.
    fn split_of(&self, ext: &str) -> Option<(&str, i64, Vec<String>)> {
        let lower = ext.to_lowercase();
        let &(_, stream, packet) = STREAM_SPLITS.iter().find(|(e, _, _)| *e == lower)?;
        let owners = self.classes_of(&lower);
        if !owners.contains(&stream) {
            return None;
        }
        let rest = owners.iter().filter(|o| **o != stream).map(|s| (*s).to_owned()).collect();
        Some((stream, packet, rest))
    }

    /// The classes one file belongs to (empty: 「其他」); the reference for the column forms.
    pub fn file_classes(&self, ext: &str, size: i64) -> Vec<String> {
        match self.split_of(ext) {
            None => self.classes_of(ext).into_iter().map(str::to_owned).collect(),
            Some((stream, packet, rest)) => {
                if looks_like_stream(size, packet) {
                    vec![stream.to_owned()]
                } else {
                    rest
                }
            }
        }
    }

    /// The extensions of `ext_table` whose files are split by size.
    pub fn splits<E: AsRef<str>>(&self, ext_table: &[E]) -> Vec<Split> {
        let mut out = Vec::new();
        for (i, e) in ext_table.iter().enumerate() {
            let e = e.as_ref();
            if e.is_empty() {
                continue;
            }
            if let Some((stream, packet, rest)) = self.split_of(e) {
                out.push(Split { ext_id: i, packet, stream: stream.to_owned(), rest });
            }
        }
        out
    }

    /// Per file (its extension id and logical size): it belongs to `class_id`.
    pub fn node_mask<E: AsRef<str>>(
        &self,
        class_id: &str,
        ext_table: &[E],
        ext: &[u16],
        size: &[i64],
    ) -> Vec<bool> {
        let by_ext = self.ext_mask(class_id, ext_table);
        let splits = self.splits(ext_table);
        if splits.is_empty() {
            return ext.iter().map(|&e| by_ext.get(e as usize).copied().unwrap_or(false)).collect();
        }
        // per extension id: None (by extension) or Some((packet, stream matches, plain matches))
        let mut rule: Vec<Option<(i64, bool, bool)>> = vec![None; ext_table.len()];
        for sp in &splits {
            let plain = sp.rest.iter().any(|r| r == class_id) || (class_id == OTHER && sp.rest.is_empty());
            rule[sp.ext_id] = Some((sp.packet, class_id == sp.stream, plain));
        }
        ext.iter()
            .zip(size)
            .map(|(&e, &s)| match rule.get(e as usize).copied().flatten() {
                Some((packet, stream, plain)) => {
                    if looks_like_stream(s, packet) {
                        stream
                    } else {
                        plain
                    }
                }
                None => by_ext.get(e as usize).copied().unwrap_or(false),
            })
            .collect()
    }

    /// The table differs from the built-in one.
    pub fn custom(&self) -> bool {
        self.classes.as_slice() != builtin_classes()
    }
}

// --------------------------------------------------------------------------- the active table

static ACTIVE: LazyLock<RwLock<Arc<ClassTable>>> = LazyLock::new(|| RwLock::new(ClassTable::builtin()));
static VERSION: AtomicU64 = AtomicU64::new(0);

/// The table in use now (the user's, else the built-in one).
pub fn active() -> Arc<ClassTable> {
    ACTIVE.read().clone()
}

/// Bumped whenever the active table changes (caches key on it).
pub fn version() -> u64 {
    VERSION.load(Ordering::Acquire)
}

pub fn set_active(table: Arc<ClassTable>) {
    let mut g = ACTIVE.write();
    *g = table;
    VERSION.fetch_add(1, Ordering::AcqRel);
}

/// The table's classes as the interface shows them (built-in names in its language, M7.5).
pub fn shown(table: &ClassTable) -> Vec<FilterClassInfo> {
    let lang = i18n::language();
    table.classes.iter().map(|c| localized(c, lang.as_str())).collect()
}

/// A built-in class's name as shown in a translated interface -> its own name (not a rename).
fn own_name(cid: Option<&str>, name: &str) -> String {
    let lang = i18n::language();
    if lang != SOURCE
        && let Some(base) = builtin_classes().iter().find(|c| Some(c.id.as_str()) == cid)
        && name == localized(base, lang.as_str()).name
    {
        return base.name.clone();
    }
    name.to_owned()
}

fn norm_ext(raw: &str) -> String {
    raw.trim().trim_start_matches('.').to_lowercase()
}

fn display(classes: &[FilterClassInfo], cid: &str) -> String {
    let lang = i18n::language();
    classes
        .iter()
        .find(|c| c.id == cid)
        .map_or_else(|| cid.to_owned(), |c| localized(c, lang.as_str()).name)
}

fn new_id(name: &str, used: &HashSet<String>) -> String {
    let folded = casefold(name);
    let mut slug = String::new();
    let mut dash = false;
    for c in folded.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            slug.push(c);
            dash = false;
        } else if !dash {
            slug.push('-');
            dash = true;
        }
    }
    let slug = slug.trim_matches('-');
    let slug = if slug.is_empty() { "class" } else { slug };
    let cut = |s: String| s.chars().take(40).collect::<String>();
    let taken = |id: &str| used.contains(id) || builtin_classes().iter().any(|c| c.id == id);
    let mut cid = cut(format!("u-{slug}"));
    let mut n = 2;
    while taken(&cid) {
        cid = cut(format!("u-{slug}-{n}"));
        n += 1;
    }
    cid
}

/// Validate the editor's list (UI-W7-33) into a table; the error is in the current language.
///
/// Extensions: `;` separated like SpaceSniffer, spaces and commas accepted, leading dots and case
/// dropped. Built-in classes keep their aliases (SpaceSniffer's names keep matching) unless renamed
/// onto one of them. `other` cannot go, holds no extensions, and stays last.
pub fn build(edits: &[ClassEdit]) -> Result<ClassTable, String> {
    if edits.len() > MAX_CLASSES {
        return Err(tr!("类别最多 {n} 个", n = MAX_CLASSES));
    }
    let mut out: Vec<FilterClassInfo> = Vec::new();
    let mut taken: HashMap<String, String> = HashMap::new();
    let mut used: HashSet<String> = HashSet::new();
    let mut other: Option<FilterClassInfo> = None;
    for e in edits {
        let name = own_name(e.id.as_deref(), e.name.trim());
        if name.is_empty() {
            return Err(t("类别名不能为空").to_owned());
        }
        let mut bad: Vec<char> = name.chars().filter(|c| matches!(c, ';' | ':' | '"' | '\\')).collect();
        if !bad.is_empty() {
            bad.sort_unstable();
            bad.dedup();
            return Err(tr!("类别名里不能有「{chars}」", chars = bad.into_iter().collect::<String>()));
        }
        let mut exts: Vec<String> = Vec::new();
        for chunk in &e.extensions {
            for raw in chunk.split(|c: char| c == ';' || c == ',' || c.is_whitespace()) {
                let ext = norm_ext(raw);
                if ext.is_empty() {
                    continue;
                }
                if ext.contains('/') || ext.contains('\\') || ext.chars().count() > 32 {
                    return Err(tr!("扩展名「{ext}」不合法", ext = raw));
                }
                if !exts.contains(&ext) {
                    exts.push(ext);
                }
            }
        }
        let cid = match &e.id {
            Some(id) if !id.is_empty() => id.clone(),
            _ => new_id(&name, &used),
        };
        if cid == OTHER && !exts.is_empty() {
            return Err(t("「其他」类别不能填扩展名：它收纳所有未归类的扩展名").to_owned());
        }
        used.insert(cid.clone());
        let base = builtin_classes().iter().find(|c| c.id == cid);
        let name_fold = casefold(&name);
        let mut aliases: Vec<String> = base
            .map(|b| b.aliases.iter().filter(|a| casefold(a) != name_fold).cloned().collect())
            .unwrap_or_default();
        if let Some(b) = base
            && casefold(&b.name) != name_fold
        {
            aliases.push(b.name.clone()); // renamed: `:class:<old name>` keeps working
        }
        let spellings: Vec<String> =
            std::iter::once(cid.clone()).chain(std::iter::once(name.clone())).chain(aliases.clone()).collect();
        for spelling in &spellings {
            if let Some(owner) = taken.get(&casefold(spelling))
                && *owner != cid
            {
                return Err(tr!(
                    "类别名「{name}」重复了（与「{other}」冲突）",
                    name = spelling.as_str(),
                    other = display(&out, owner)
                ));
            }
        }
        let info = FilterClassInfo {
            id: cid.clone(),
            name,
            aliases,
            extensions: exts,
            color: e.color.to_uppercase(),
            color_light: e.color_light.to_uppercase(),
        };
        for spelling in &spellings {
            taken.insert(casefold(spelling), cid.clone());
        }
        if cid == OTHER {
            other = Some(info);
        } else {
            out.push(info);
        }
    }
    let Some(other) = other else {
        return Err(t("「其他」类别不能删除").to_owned());
    };
    out.push(other);
    ClassTable::new(out)
}

// --------------------------------------------------------------------------- the file

/// A class written before themes (classes.json version 1, a settings export without
/// `color_light`), with its colour for both themes.
pub fn colors_for_both(raw: Value) -> Value {
    match raw {
        Value::Object(mut m) if m.contains_key("color") => {
            let color = m["color"].clone();
            m.insert("color_light".into(), color);
            Value::Object(m)
        }
        other => other,
    }
}

fn parse_file(text: &str) -> Result<ClassTable, String> {
    let raw: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let items = raw.get("classes").and_then(Value::as_array).ok_or("no 'classes' list")?.clone();
    let version = raw.get("version").and_then(Value::as_i64).unwrap_or(1);
    let items: Vec<Value> = if version < 2 { items.into_iter().map(colors_for_both).collect() } else { items };
    let classes = items
        .into_iter()
        .map(serde_json::from_value::<FilterClassInfo>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    ClassTable::new(classes)
}

/// The table saved in `path`; the built-in one when there is none or it is broken.
pub fn load(path: &Path) -> Arc<ClassTable> {
    match std::fs::read_to_string(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => ClassTable::builtin(),
        Err(e) => {
            tracing::warn!("{}", tr!("类别表 {} 读不出来，改用内置类别：{}", path = path, reason = e.to_string()));
            ClassTable::builtin()
        }
        Ok(text) => match parse_file(&text) {
            Ok(t) => Arc::new(t),
            Err(e) => {
                tracing::warn!("{}", tr!("类别表 {} 读不出来，改用内置类别：{}", path = path, reason = e));
                ClassTable::builtin()
            }
        },
    }
}

pub fn user_file() -> PathBuf {
    crate::paths::config_dir().join("classes.json")
}

/// Make the user's saved table active (server start, the command line).
pub fn load_user() {
    set_active(load(&user_file()));
}

/// Write atomically; the built-in table removes the file (nothing to remember).
pub fn save(table: &ClassTable, path: &Path) -> std::io::Result<()> {
    if !table.custom() {
        return match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let doc = serde_json::json!({ "version": FILE_VERSION, "classes": table.classes });
    let mut buf = Vec::new();
    let fmt = serde_json::ser::PrettyFormatter::with_indent(b" ");
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, fmt);
    serde::Serialize::serialize(&doc, &mut ser).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, buf)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sk_protocol::classes::ENGLISH_NAMES;

    const TS_REC: i64 = 188 * 5600; // 1.0 MiB of whole 188-byte packets (not whole 192-byte ones)
    const MTS_REC: i64 = 192 * 5500;

    fn sizes() -> Vec<i64> {
        vec![
            0, 1, 4096, 65_536, STREAM_ALIGNED_MIN - 188, TS_REC, TS_REC + 1, MTS_REC, MTS_REC + 1,
            3 << 20, STREAM_ANY_MIN - 1, STREAM_ANY_MIN, 5 * STREAM_ANY_MIN + 7,
        ]
    }

    #[test]
    fn sizes_are_what_they_claim() {
        const { assert!(TS_REC >= STREAM_ALIGNED_MIN && TS_REC % 188 == 0 && TS_REC % 192 != 0) };
        const { assert!(MTS_REC >= STREAM_ALIGNED_MIN && MTS_REC % 192 == 0 && MTS_REC % 188 != 0) };
    }

    #[test]
    fn only_deliberate_extensions_are_in_two_classes() {
        let t = ClassTable::builtin();
        let shared: HashSet<&str> = builtin_classes()
            .iter()
            .flat_map(|c| c.extensions.iter())
            .filter(|e| t.classes_of(e).len() > 1)
            .map(String::as_str)
            .collect();
        assert_eq!(shared, HashSet::from(["3gp", "ts", "mts"]));
    }

    #[test]
    fn builtin_extensions_are_clean() {
        for c in builtin_classes() {
            let set: HashSet<_> = c.extensions.iter().collect();
            assert_eq!(set.len(), c.extensions.len(), "{}", c.id);
            for e in &c.extensions {
                assert!(e.chars().all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit()), "{e}");
            }
        }
        assert!(builtin_classes().iter().find(|c| c.id == OTHER).unwrap().extensions.is_empty());
        assert_eq!(builtin_classes().last().unwrap().id, OTHER);
    }

    #[test]
    fn every_builtin_class_has_an_english_name_among_its_aliases() {
        assert_eq!(ENGLISH_NAMES.len(), builtin_classes().len());
        for c in builtin_classes() {
            let en = ENGLISH_NAMES.iter().find(|(k, _)| *k == c.id).unwrap().1;
            assert!(c.aliases.iter().any(|a| a == en), "{}", c.id);
        }
    }

    #[test]
    fn file_classes_by_size() {
        let t = ClassTable::builtin();
        for (ext, size, want) in [
            ("ts", 12_000, vec!["code"]),
            ("ts", TS_REC, vec!["video"]),
            ("ts", TS_REC + 1, vec!["code"]),
            ("ts", MTS_REC, vec!["code"]),
            ("ts", STREAM_ALIGNED_MIN - 188, vec!["code"]),
            ("ts", STREAM_ANY_MIN, vec!["video"]),
            ("TS", STREAM_ANY_MIN + 1, vec!["video"]),
            ("mts", 4000, vec!["code"]),
            ("mts", MTS_REC, vec!["video"]),
            ("mts", TS_REC, vec!["code"]),
            ("m2ts", 4000, vec!["video"]),
            ("mp4", 10, vec!["video"]),
            ("3gp", 10, vec!["video", "audio"]),
        ] {
            assert_eq!(t.file_classes(ext, size), want, "{ext} {size}");
        }
    }

    fn edited(changes: &[(&str, Option<Vec<&str>>)]) -> ClassTable {
        let mut out = Vec::new();
        for c in builtin_classes() {
            let mut c = c.clone();
            if let Some((_, exts)) = changes.iter().find(|(id, _)| *id == c.id) {
                match exts {
                    None => continue,
                    Some(e) => c.extensions = e.iter().map(|s| (*s).to_owned()).collect(),
                }
            }
            out.push(c);
        }
        ClassTable::new(out).unwrap()
    }

    #[test]
    fn user_edits_decide_the_split() {
        let video: Vec<&str> = builtin_classes()[0].extensions.iter().map(String::as_str).filter(|e| *e != "ts").collect();
        let no_ts = edited(&[("video", Some(video))]);
        assert_eq!(no_ts.file_classes("ts", STREAM_ANY_MIN), ["code"]);
        assert!(no_ts.splits(&["", "ts"]).is_empty());
        let no_code = edited(&[("code", None)]);
        assert!(no_code.file_classes("ts", 3000).is_empty());
        assert_eq!(no_code.file_classes("ts", TS_REC), ["video"]);
    }

    #[test]
    fn node_mask_matches_file_classes() {
        let ext_table = ["", "ts", "mts", "mp4", "py", "m2ts", "3gp", "zzz"];
        let sizes = sizes();
        let mut ext = Vec::new();
        let mut size = Vec::new();
        for e in 0..ext_table.len() {
            for &s in &sizes {
                ext.push(e as u16);
                size.push(s);
            }
        }
        for table in [ClassTable::builtin().as_ref().clone(), edited(&[("code", None)])] {
            for cid in table.ids() {
                let got = table.node_mask(cid, &ext_table, &ext, &size);
                let want: Vec<bool> = ext
                    .iter()
                    .zip(&size)
                    .map(|(&e, &s)| {
                        let fc = table.file_classes(ext_table[e as usize], s);
                        if fc.is_empty() { cid == OTHER } else { fc.iter().any(|c| c == cid) }
                    })
                    .collect();
                assert_eq!(got, want, "{cid}");
            }
        }
    }

    #[test]
    fn a_table_leaves_room_for_no_class() {
        let _g = crate::i18n::TEST_LOCK.lock();
        let edit = |id: Option<&str>, name: String, exts: Vec<String>| ClassEdit {
            id: id.map(str::to_owned),
            name,
            extensions: exts,
            color: String::new(),
            color_light: String::new(),
        };
        let many: Vec<ClassEdit> =
            (0..MAX_CLASSES).map(|i| edit(None, format!("c{i}"), vec![format!("e{i}")])).collect();
        let mut most = many[..MAX_CLASSES - 1].to_vec();
        most.push(edit(Some(OTHER), "其他".into(), vec![]));
        assert_eq!(build(&most).unwrap().classes().len(), MAX_CLASSES);
        let mut too_many = many.clone();
        too_many.push(edit(Some(OTHER), "其他".into(), vec![]));
        assert!(build(&too_many).unwrap_err().contains(&MAX_CLASSES.to_string()));
        const { assert!(MAX_CLASSES < 255) };
    }

    #[test]
    fn build_validates_and_keeps_aliases() {
        let _g = crate::i18n::TEST_LOCK.lock();
        let edit = |id: Option<&str>, name: &str, exts: &[&str]| ClassEdit {
            id: id.map(str::to_owned),
            name: name.into(),
            extensions: exts.iter().map(|s| (*s).to_owned()).collect(),
            color: "#abcdef".into(),
            color_light: String::new(),
        };
        let t = build(&[
            edit(Some("video"), "Films", &[".MP4; mkv,avi"]),
            edit(None, "My Stuff", &["xyz"]),
            edit(Some(OTHER), "其他", &[]),
        ])
        .unwrap();
        let video = t.by_id("video").unwrap();
        assert_eq!(video.extensions, ["mp4", "mkv", "avi"]);
        assert!(video.aliases.iter().any(|a| a == "视频")); // renamed: the old name stays an alias
        assert_eq!(video.color, "#ABCDEF");
        assert_eq!(t.ids(), ["video", "u-my-stuff", OTHER]);
        assert!(build(&[edit(Some(OTHER), "其他", &["x"])]).is_err());
        assert!(build(&[edit(None, "a:b", &[]), edit(Some(OTHER), "其他", &[])]).is_err());
        assert!(build(&[edit(None, "x", &[])]).is_err()); // no other
        assert!(build(&[edit(None, "Video", &[]), edit(Some("video"), "视频", &[]), edit(Some(OTHER), "o", &[])]).is_err());
    }

    #[test]
    fn save_and_load() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("classes.json");
        save(&ClassTable::builtin(), &file).unwrap();
        assert!(!file.exists());
        assert_eq!(*load(&file), *ClassTable::builtin());
        let t = edited(&[("code", None)]);
        save(&t, &file).unwrap();
        assert_eq!(*load(&file), t);
        // version 1: colours count for both themes
        std::fs::write(&file, r##"{"classes": [{"id": "other", "name": "其他", "aliases": [], "extensions": [], "color": "#123456"}]}"##).unwrap();
        assert_eq!(load(&file).classes()[0].color_light, "#123456");
        std::fs::write(&file, "{ broken").unwrap();
        assert_eq!(*load(&file), *ClassTable::builtin());
    }
}
