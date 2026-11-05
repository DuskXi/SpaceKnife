//! Settings service (W7): the registry, validation, atomic persistence and change notification.
//!
//! The registry ([`registry()`], [`by_key`], [`defaults`]) is the single source of truth for every
//! frontend (UI-W7-27). Values are validated strictly against it (a bool is a bool, an int an int
//! in range, a segmented value one of its options) and stored as JSON in the config directory
//! (`settings.json`: `{"version": 1, "values": {<only what differs from the defaults>}}`). External
//! edits of the file are picked up by [`SettingsStore::poll`].
//!
//! [`io`] reads and writes settings files (SpaceKnife's export, SpaceSniffer's configuration).

pub mod io;
mod registry;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::SystemTime;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sk_protocol::settings::{
    SettingCategory, SettingControl, SettingMeta, SettingOption, SettingValue, SettingsState, UnitBase,
};

pub use registry::{ACCENTS, Kind, LOAD_LEVELS, Spec};

use crate::i18n::{self, t};
use crate::{paths, tr};

pub const FILE_VERSION: i64 = 1;

/// `{key: value}` for every setting.
pub type Values = BTreeMap<String, SettingValue>;

static REGISTRY: LazyLock<Vec<Spec>> = LazyLock::new(registry::registry);
static BY_KEY: LazyLock<HashMap<&'static str, usize>> =
    LazyLock::new(|| REGISTRY.iter().enumerate().map(|(i, s)| (s.key, i)).collect());
static DEFAULTS: LazyLock<Values> =
    LazyLock::new(|| REGISTRY.iter().map(|s| (s.key.to_owned(), s.default.clone())).collect());

/// Every setting, in page order.
pub fn registry() -> &'static [Spec] {
    &REGISTRY
}

pub fn by_key(key: &str) -> Option<&'static Spec> {
    BY_KEY.get(key).map(|&i| &REGISTRY[i])
}

/// The default of every setting.
pub fn defaults() -> &'static Values {
    &DEFAULTS
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    /// Unknown key, invalid value, a read-only setting (the message is in the language of the moment)
    #[error("{0}")]
    Invalid(String),
    /// The document moved on since the version the client sent (SETTINGS_CONFLICT)
    #[error("{0}")]
    Conflict(String),
    /// The settings file could not be written
    #[error("{0}")]
    Io(String),
}

fn num(v: f64) -> String {
    if v.fract() == 0.0 { format!("{}", v as i64) } else { v.to_string() }
}

fn literal_list(opts: &[(&str, &str, &str)]) -> String {
    let quoted: Vec<String> = opts.iter().map(|o| format!("'{}'", o.0)).collect();
    match quoted.len() {
        0 => String::new(),
        1 => quoted[0].clone(),
        n => format!("{} or {}", quoted[..n - 1].join(", "), quoted[n - 1]),
    }
}

/// One value against its setting: `Err` is a pydantic-style message.
pub fn check_value(spec: &Spec, value: &SettingValue) -> Result<SettingValue, String> {
    if !spec.options.is_empty() && spec.control == SettingControl::Segmented {
        return match value {
            SettingValue::Str(s) if spec.options.iter().any(|o| o.0 == s) => Ok(value.clone()),
            _ => Err(format!("Input should be {}", literal_list(&spec.options))),
        };
    }
    match spec.kind {
        Kind::Bool => match value {
            SettingValue::Bool(_) => Ok(value.clone()),
            _ => Err("Input should be a valid boolean".into()),
        },
        Kind::Int => match value {
            SettingValue::Int(i) => {
                if let Some(min) = spec.min
                    && (*i as f64) < min
                {
                    return Err(format!("Input should be greater than or equal to {}", num(min)));
                }
                if let Some(max) = spec.max
                    && (*i as f64) > max
                {
                    return Err(format!("Input should be less than or equal to {}", num(max)));
                }
                Ok(value.clone())
            }
            _ => Err("Input should be a valid integer".into()),
        },
        Kind::Str => match value {
            SettingValue::Str(s) if spec.control == SettingControl::Color => {
                let ok = s.len() == 7
                    && s.starts_with('#')
                    && s[1..].chars().all(|c| c.is_ascii_hexdigit());
                if ok {
                    Ok(value.clone())
                } else {
                    Err("String should match pattern '^#[0-9A-Fa-f]{6}$'".into())
                }
            }
            SettingValue::Str(s) if s.chars().count() > 64 => {
                Err("String should have at most 64 characters".into())
            }
            SettingValue::Str(_) => Ok(value.clone()),
            _ => Err("Input should be a valid string".into()),
        },
    }
}

/// Full, validated values (unknown keys dropped, missing keys defaulted).
pub fn validate(values: &Values) -> Result<Values, SettingsError> {
    let mut out = Values::new();
    for spec in registry() {
        let v = values.get(spec.key).unwrap_or(&spec.default);
        match check_value(spec, v) {
            Ok(v) => {
                out.insert(spec.key.to_owned(), v);
            }
            Err(msg) => {
                return Err(SettingsError::Invalid(tr!(
                    "「{name}」的值不合法：{msg}",
                    name = t(spec.name),
                    msg = msg
                )));
            }
        }
    }
    Ok(out)
}

/// The settings' metadata in the current language (M7.5); the Chinese name stays searchable.
pub fn meta() -> Vec<SettingMeta> {
    let source = i18n::language() == i18n::SOURCE;
    let tx = |s: &str| if s.is_empty() { String::new() } else { t(s).to_owned() };
    registry()
        .iter()
        .map(|s| {
            let mut synonyms: Vec<String> = s.synonyms.iter().map(|x| (*x).to_owned()).collect();
            if !source {
                synonyms.push(s.name.to_owned());
            }
            SettingMeta {
                key: s.key.to_owned(),
                category: s.category,
                group: t(s.group).to_owned(),
                group_note: tx(s.group_note),
                name: t(s.name).to_owned(),
                description: tx(s.description),
                control: s.control,
                options: s
                    .options
                    .iter()
                    .map(|o| SettingOption {
                        value: SettingValue::Str(o.0.to_owned()),
                        label: tx(o.1),
                        reason: tx(o.2),
                    })
                    .collect(),
                min: s.min,
                max: s.max,
                step: s.step,
                unit: tx(s.unit),
                applies: s.applies,
                synonyms,
                enabled: s.reason.is_empty(),
                reason: tx(s.reason),
                depends: s.depends.map(str::to_owned),
            }
        })
        .collect()
}

/// '跟随系统' (W7 待定 7): Windows 1024, macOS 1000, Linux GNOME 1000, other desktops 1024.
pub fn system_unit_base() -> UnitBase {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    unit_base_for(std::env::consts::OS, &desktop)
}

/// [`system_unit_base`] for an OS (`std::env::consts::OS`) and desktop (`XDG_CURRENT_DESKTOP`).
pub fn unit_base_for(os: &str, desktop: &str) -> UnitBase {
    if os == "macos" || (os == "linux" && desktop.to_lowercase().contains("gnome")) {
        UnitBase::Si
    } else {
        UnitBase::Binary
    }
}

#[derive(Serialize, Deserialize)]
struct SettingsFile {
    #[serde(default = "file_version")]
    version: i64,
    #[serde(default)]
    values: Values,
}

fn file_version() -> i64 {
    FILE_VERSION
}

#[derive(Clone, Copy, Debug)]
enum FileProblem {
    Unreadable,
    Invalid,
}

struct Inner {
    version: i64,
    values: Values,
    /// (what, reason): translated when shown
    error: Option<(FileProblem, String)>,
    mtime: Option<SystemTime>,
}

pub type Listener = Arc<dyn Fn(&SettingsStore) + Send + Sync>;

/// The settings document: thread-safe reads, validated patches, atomic saves, change listeners.
pub struct SettingsStore {
    file: PathBuf,
    inner: Mutex<Inner>,
    listeners: Mutex<Vec<(String, Listener)>>,
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

impl SettingsStore {
    /// The store of `file` (`None`: `config_dir()/settings.json`), loaded.
    pub fn new(file: Option<PathBuf>) -> SettingsStore {
        let file = file.unwrap_or_else(|| paths::config_dir().join("settings.json"));
        let store = SettingsStore {
            file,
            inner: Mutex::new(Inner { version: 0, values: defaults().clone(), error: None, mtime: None }),
            listeners: Mutex::new(Vec::new()),
        };
        {
            let mut g = store.inner.lock();
            store.load(&mut g);
        }
        store
    }

    pub fn file(&self) -> &Path {
        &self.file
    }

    // ------------------------------------------------------------ persistence

    fn load(&self, g: &mut Inner) -> bool {
        let meta = match std::fs::metadata(&self.file) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                g.mtime = None;
                return false;
            }
            Err(e) => {
                g.error = Some((FileProblem::Unreadable, e.to_string()));
                return false;
            }
        };
        g.mtime = meta.modified().ok();
        let parsed = std::fs::read_to_string(&self.file)
            .map_err(|e| e.to_string())
            .and_then(|text| serde_json::from_str::<SettingsFile>(&text).map_err(|e| e.to_string()))
            .and_then(|doc| {
                let mut merged = defaults().clone();
                merged.extend(doc.values);
                validate(&merged).map_err(|e| e.to_string())
            });
        match parsed {
            Ok(values) => {
                let changed = values != g.values;
                g.values = values;
                g.error = None;
                changed
            }
            Err(reason) => {
                tracing::warn!("settings file {} is invalid: {reason}", self.file.display());
                g.error = Some((FileProblem::Invalid, reason));
                false
            }
        }
    }

    fn save(&self, g: &mut Inner) -> Result<(), SettingsError> {
        let io = |e: std::io::Error| SettingsError::Io(format!("{}: {e}", self.file.display()));
        if let Some(parent) = self.file.parent() {
            std::fs::create_dir_all(parent).map_err(io)?;
        }
        let changed: Values = g
            .values
            .iter()
            .filter(|(k, v)| defaults().get(*k) != Some(*v))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let doc = SettingsFile { version: FILE_VERSION, values: changed };
        let text = serde_json::to_string_pretty(&doc).map_err(|e| SettingsError::Io(e.to_string()))?;
        let tmp = self.file.with_extension("tmp");
        std::fs::write(&tmp, text).map_err(io)?;
        std::fs::rename(&tmp, &self.file).map_err(io)?;
        g.mtime = mtime(&self.file);
        Ok(())
    }

    /// Reload after an external edit of the file (UI-W7-25). True if the values changed.
    pub fn poll(&self) -> bool {
        let now = mtime(&self.file);
        let (changed, error) = {
            let mut g = self.inner.lock();
            if now == g.mtime {
                return false;
            }
            let changed = self.load(&mut g);
            if changed {
                g.version += 1;
            }
            (changed, g.error.is_some())
        };
        if changed || error {
            self.notify();
        }
        changed
    }

    /// What is wrong with the settings file, in the current language ("" when nothing is).
    pub fn error(&self) -> String {
        match &self.inner.lock().error {
            None => String::new(),
            Some((FileProblem::Unreadable, r)) => tr!("无法读取设置文件：{reason}", reason = r.as_str()),
            Some((FileProblem::Invalid, r)) => {
                tr!("设置文件不合法，继续使用上一次的有效设置：{reason}", reason = r.as_str())
            }
        }
    }

    // ------------------------------------------------------------ access

    pub fn version(&self) -> i64 {
        self.inner.lock().version
    }

    pub fn values(&self) -> Values {
        self.inner.lock().values.clone()
    }

    pub fn get(&self, key: &str) -> Option<SettingValue> {
        self.inner.lock().values.get(key).cloned()
    }

    pub fn get_bool(&self, key: &str) -> bool {
        matches!(self.get(key), Some(SettingValue::Bool(true)))
    }

    pub fn get_int(&self, key: &str) -> i64 {
        match self.get(key) {
            Some(SettingValue::Int(i)) => i,
            _ => 0,
        }
    }

    pub fn get_str(&self, key: &str) -> String {
        match self.get(key) {
            Some(SettingValue::Str(s)) => s,
            _ => String::new(),
        }
    }

    /// Change some settings (validated; `version`: reject when the document moved on). True when
    /// anything changed (then saved and the listeners told).
    pub fn update(&self, patch: &Values, version: Option<i64>) -> Result<bool, SettingsError> {
        let unknown: Vec<&str> = patch.keys().filter(|k| by_key(k).is_none()).map(String::as_str).collect();
        if !unknown.is_empty() {
            return Err(SettingsError::Invalid(tr!("未知设置：{keys}", keys = unknown.join(", "))));
        }
        {
            let mut g = self.inner.lock();
            let locked: Vec<&str> = patch
                .iter()
                .filter_map(|(k, v)| {
                    let spec = by_key(k)?;
                    (!spec.reason.is_empty() && g.values.get(k) != Some(v)).then(|| t(spec.name))
                })
                .collect();
            if !locked.is_empty() {
                return Err(SettingsError::Invalid(tr!(
                    "这些设置在当前版本不可修改：{names}",
                    names = locked.join(t("、"))
                )));
            }
            if let Some(v) = version
                && v != g.version
            {
                return Err(SettingsError::Conflict(tr!(
                    "设置已被其他窗口修改（版本 {version}）",
                    version = g.version
                )));
            }
            let mut merged = g.values.clone();
            merged.extend(patch.iter().map(|(k, v)| (k.clone(), v.clone())));
            let values = validate(&merged)?;
            if values == g.values {
                return Ok(false);
            }
            g.values = values;
            g.version += 1;
            self.save(&mut g)?;
        }
        self.notify();
        Ok(true)
    }

    /// Back to the defaults: one page (`category`) or everything.
    pub fn reset(&self, category: Option<SettingCategory>) -> Result<bool, SettingsError> {
        {
            let mut g = self.inner.lock();
            let mut values = g.values.clone();
            for s in registry().iter().filter(|s| category.is_none_or(|c| c == s.category)) {
                values.insert(s.key.to_owned(), s.default.clone());
            }
            if values == g.values {
                return Ok(false);
            }
            g.values = values;
            g.version += 1;
            self.save(&mut g)?;
        }
        self.notify();
        Ok(true)
    }

    /// 「大小单位」 after resolving 'system'.
    pub fn unit_base(&self) -> UnitBase {
        match self.get_str("scan.unit_base").as_str() {
            "1000" => UnitBase::Si,
            "1024" => UnitBase::Binary,
            _ => system_unit_base(),
        }
    }

    /// 「界面语言」 after resolving 'system'.
    pub fn language(&self) -> i18n::Language {
        i18n::resolve(Some(&self.get_str("general.language")))
    }

    /// The whole document for `settings.get` / `settings.changed`.
    pub fn state(&self) -> SettingsState {
        let (version, values) = {
            let g = self.inner.lock();
            (g.version, g.values.clone())
        };
        SettingsState {
            version,
            values,
            defaults: defaults().clone(),
            meta: meta(),
            path: self.file.display().to_string(),
            logs: paths::logs_dir().display().to_string(),
            snapshots: paths::snapshots_dir().display().to_string(),
            error: self.error(),
            unit_base: self.unit_base(),
            language: self.language(),
        }
    }

    // ------------------------------------------------------------ listeners

    /// Call `listener` after every change (replaces one of the same name).
    pub fn add_listener(&self, name: &str, listener: impl Fn(&SettingsStore) + Send + Sync + 'static) {
        let mut g = self.listeners.lock();
        g.retain(|(n, _)| n != name);
        g.push((name.to_owned(), Arc::new(listener)));
    }

    pub fn remove_listener(&self, name: &str) {
        self.listeners.lock().retain(|(n, _)| n != name);
    }

    fn notify(&self) {
        let listeners: Vec<Listener> = self.listeners.lock().iter().map(|(_, l)| l.clone()).collect();
        for l in listeners {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| l(self))).is_err() {
                tracing::error!("settings listener failed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sk_protocol::view::{BUILTIN_VISUALS, ColorModeName};

    fn vals(items: &[(&str, SettingValue)]) -> Values {
        items.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect()
    }

    #[test]
    fn registry_is_consistent() {
        assert_eq!(BY_KEY.len(), registry().len());
        assert_eq!(validate(defaults()).unwrap(), *defaults());
        for s in registry() {
            assert!(!s.name.is_empty() && !s.description.is_empty(), "{}", s.key);
            if !s.options.is_empty() && s.control == SettingControl::Segmented {
                assert!(s.options.iter().any(|o| SettingValue::Str(o.0.into()) == s.default), "{}", s.key);
            }
            if let (Some(min), Some(max), SettingValue::Int(d)) = (s.min, s.max, &s.default) {
                assert!(min <= *d as f64 && *d as f64 <= max, "{}", s.key);
            }
        }
    }

    #[test]
    fn every_setting_text_has_an_english_translation() {
        let en = i18n::catalog(i18n::Language::En);
        let mut missing = Vec::new();
        for s in registry() {
            let mut texts = vec![s.group, s.name, s.description, s.group_note, s.unit, s.reason];
            texts.extend(s.options.iter().flat_map(|o| [o.1, o.2]));
            for text in texts {
                let has_han = text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c));
                if has_han && !en.contains_key(text) && text != "简体中文" {
                    missing.push(text);
                }
            }
        }
        assert!(missing.is_empty(), "{missing:?}");
    }

    #[test]
    fn visualization_settings_match_the_protocol() {
        let opts = |k: &str| by_key(k).unwrap().options.iter().map(|o| o.0).collect::<Vec<_>>();
        assert_eq!(opts("appearance.default_visual"), BUILTIN_VISUALS);
        let modes: Vec<&str> = ColorModeName::ALL.iter().map(|m| m.as_str()).collect();
        for key in ["appearance.default_color_mode", "appearance.partition_color_mode"] {
            assert_eq!(opts(key), modes, "{key}");
        }
        assert_eq!(defaults()["appearance.default_visual"], "treemap".into());
        assert_eq!(defaults()["appearance.partition_color_mode"], "branch".into());
        assert_eq!(defaults()["treemap.flame_origin"], "bottom".into());
        let mut v = defaults().clone();
        v.insert("appearance.default_visual".into(), "pie".into());
        assert!(validate(&v).is_err());
        let mut v = defaults().clone();
        v.insert("treemap.flame_origin".into(), "left".into());
        assert!(validate(&v).is_err());
    }

    #[test]
    fn update_validates_and_persists() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("settings.json");
        let st = SettingsStore::new(Some(file.clone()));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s2 = seen.clone();
        st.add_listener("t", move |s| s2.lock().push(s.version()));
        st.update(&vals(&[("scan.unit_base", "1000".into()), ("treemap.min_element", 16.into())]), None)
            .unwrap();
        assert_eq!(st.unit_base(), UnitBase::Si);
        assert_eq!(*seen.lock(), [1]);
        let saved: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(saved["values"], serde_json::json!({"scan.unit_base": "1000", "treemap.min_element": 16}));
        assert_eq!(SettingsStore::new(Some(file.clone())).values(), st.values());
        let bad = |k: &str, v: SettingValue| st.update(&vals(&[(k, v)]), None).unwrap_err();
        assert!(matches!(bad("treemap.min_element", 99.into()), SettingsError::Invalid(_)));
        assert!(matches!(bad("treemap.min_element", 9.5.into()), SettingsError::Invalid(_)));
        assert!(matches!(bad("scan.unit_base", "999".into()), SettingsError::Invalid(_)));
        assert!(matches!(bad("appearance.accent", "red".into()), SettingsError::Invalid(_)));
        assert!(matches!(bad("nope", 1.into()), SettingsError::Invalid(_)));
        assert!(matches!(bad("treemap.dynazoom", true.into()), SettingsError::Invalid(_)));
        assert!(matches!(bad("general.hover_age", 1.into()), SettingsError::Invalid(_)));
        assert!(matches!(
            st.update(&vals(&[("treemap.min_element", 12.into())]), Some(0)),
            Err(SettingsError::Conflict(_))
        ));
        st.update(&vals(&[("treemap.min_element", 12.into())]), Some(st.version())).unwrap();
        assert_eq!(st.get_int("treemap.min_element"), 12);
        assert!(!st.update(&vals(&[("treemap.min_element", 12.into())]), None).unwrap()); // no change
        assert_eq!(*seen.lock(), [1, 2]);
        st.reset(Some(SettingCategory::Treemap)).unwrap();
        assert_eq!(st.get("treemap.min_element"), defaults().get("treemap.min_element").cloned());
        assert_eq!(st.get_str("scan.unit_base"), "1000"); // other pages untouched
        st.reset(None).unwrap();
        assert_eq!(st.values(), *defaults());
    }

    #[test]
    fn external_edits_are_picked_up() {
        let _g = i18n::TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("settings.json");
        let st = SettingsStore::new(Some(file.clone()));
        st.update(&vals(&[("general.hover_age", false.into())]), None).unwrap();
        let touch = |secs: u64| {
            let f = std::fs::File::options().write(true).open(&file).unwrap();
            f.set_modified(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs)).unwrap();
        };
        std::fs::write(&file, r#"{"version": 1, "values": {"general.hover_age": true, "tags.omit_ctrl": false}}"#).unwrap();
        touch(1);
        assert!(st.poll());
        assert!(!st.get_bool("tags.omit_ctrl") && st.error().is_empty());
        std::fs::write(&file, "{ broken").unwrap();
        touch(2);
        assert!(!st.poll());
        assert!(!st.error().is_empty() && !st.get_bool("tags.omit_ctrl")); // last valid values stay
        std::fs::write(&file, r#"{"values": {"treemap.min_element": 1}}"#).unwrap();
        touch(3);
        assert!(!st.poll());
        assert!(st.error().contains("最小元素尺寸"), "{}", st.error());
    }

    #[test]
    fn system_unit_base_by_platform() {
        assert_eq!(unit_base_for("linux", "ubuntu:GNOME"), UnitBase::Si);
        assert_eq!(unit_base_for("linux", "KDE"), UnitBase::Binary);
        assert_eq!(unit_base_for("macos", ""), UnitBase::Si);
        assert_eq!(unit_base_for("windows", "GNOME"), UnitBase::Binary);
    }

    #[test]
    fn meta_is_translated_and_keeps_the_chinese_name_searchable() {
        let _g = i18n::TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        let st = SettingsStore::new(Some(tmp.path().join("s.json")));
        let state = st.state();
        assert_eq!(state.meta.len(), registry().len());
        let lang = state.meta.iter().find(|m| m.key == "general.language").unwrap();
        assert_eq!(lang.name, "界面语言");
        i18n::set_language(i18n::Language::En);
        let meta = meta();
        i18n::set_language(i18n::Language::ZhCn);
        let lang = meta.iter().find(|m| m.key == "general.language").unwrap();
        assert_ne!(lang.name, "界面语言");
        assert!(lang.synonyms.iter().any(|s| s == "界面语言"));
        assert!(!meta.iter().find(|m| m.key == "treemap.dynazoom").unwrap().enabled);
    }
}
