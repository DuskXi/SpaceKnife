//! Settings (W7): one backend-owned document, described by metadata so every frontend renders the
//! same page from it (UI-W7-05, UI-W7-27, UI-W7-28).
//!
//! Values are flat `{key: value}`; keys are dotted (`scan.cross_mounts`). The backend validates
//! every write, saves atomically, and pushes `settings.changed` to all connected sessions.

use std::borrow::Cow;
use std::collections::BTreeMap;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::base::yes;
use crate::filter::FilterClassInfo;
use crate::macros::str_enum;
use crate::validate::{self as v, Validate, ValidationResult};

str_enum! {
    pub enum SettingCategory {
        General = "general",
        Appearance = "appearance",
        Treemap = "treemap",
        Scan = "scan",
        Classes = "classes",
        Tags = "tags",
        Keys = "keys",
        Advanced = "advanced",
        About = "about",
    }
}

// `folders`: a list of folders, kept as a string with one absolute path per line
// (treemap.hidden_folders); the page edits it as a list.
str_enum! {
    pub enum SettingControl {
        Switch = "switch",
        Segmented = "segmented",
        Number = "number",
        Slider = "slider",
        Color = "color",
        Text = "text",
        Folders = "folders",
    }
}

str_enum! {
    pub enum SettingApplies {
        Now = "now",
        NextScan = "next_scan",
        NewViews = "new_views",
    }
}

impl Default for SettingApplies {
    fn default() -> Self {
        SettingApplies::Now
    }
}

/// A setting's value: `bool | int | float | str` (JSON true/false, integers, other numbers, strings).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SettingValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
}

impl JsonSchema for SettingValue {
    fn inline_schema() -> bool {
        true
    }
    fn schema_name() -> Cow<'static, str> {
        "SettingValue".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "anyOf": [{ "type": "boolean" }, { "type": "integer" }, { "type": "number" }, { "type": "string" }]
        })
    }
}

impl From<bool> for SettingValue {
    fn from(v: bool) -> Self {
        SettingValue::Bool(v)
    }
}

impl From<i64> for SettingValue {
    fn from(v: i64) -> Self {
        SettingValue::Int(v)
    }
}

impl From<f64> for SettingValue {
    fn from(v: f64) -> Self {
        SettingValue::Float(v)
    }
}

impl From<&str> for SettingValue {
    fn from(v: &str) -> Self {
        SettingValue::Str(v.to_owned())
    }
}

impl From<String> for SettingValue {
    fn from(v: String) -> Self {
        SettingValue::Str(v)
    }
}

/// Size unit base: 1000 (SI) or 1024 (binary).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnitBase {
    Si,
    Binary,
}

impl UnitBase {
    pub const fn value(self) -> u32 {
        match self {
            UnitBase::Si => 1000,
            UnitBase::Binary => 1024,
        }
    }
}

impl Serialize for UnitBase {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u32(self.value())
    }
}

impl<'de> Deserialize<'de> for UnitBase {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match u32::deserialize(d)? {
            1000 => Ok(UnitBase::Si),
            1024 => Ok(UnitBase::Binary),
            other => Err(serde::de::Error::custom(format!(
                "unit_base must be 1000 or 1024, not {other}"
            ))),
        }
    }
}

impl JsonSchema for UnitBase {
    fn inline_schema() -> bool {
        true
    }
    fn schema_name() -> Cow<'static, str> {
        "UnitBase".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({ "enum": [1000, 1024] })
    }
}

str_enum! {
    pub enum Language {
        ZhCn = "zh-CN",
        En = "en",
    }
}

impl Default for Language {
    fn default() -> Self {
        Language::ZhCn
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SettingOption {
    pub value: SettingValue,
    pub label: String,
    /// Why this option cannot be chosen here ('' = available)
    #[serde(default)]
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SettingMeta {
    pub key: String,
    pub category: SettingCategory,
    pub group: String,
    #[serde(default)]
    pub group_note: String,
    pub name: String,
    pub description: String,
    pub control: SettingControl,
    #[serde(default)]
    pub options: Vec<SettingOption>,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default)]
    pub step: Option<f64>,
    #[serde(default)]
    pub unit: String,
    #[serde(default)]
    pub applies: SettingApplies,
    /// Extra search words (English, abbreviations)
    #[serde(default)]
    pub synonyms: Vec<String>,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Why the setting is disabled (platform, or the milestone that wires it)
    #[serde(default)]
    pub reason: String,
    /// 'key' (a switch that must be on) or 'key=value' for this row to be editable
    #[serde(default)]
    pub depends: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SettingsState {
    /// Bumped by every change; send it back with settings.update
    pub version: i64,
    pub values: BTreeMap<String, SettingValue>,
    pub defaults: BTreeMap<String, SettingValue>,
    pub meta: Vec<SettingMeta>,
    /// The settings file (UI-W7-25)
    pub path: String,
    /// The log folder (W7 高级)
    pub logs: String,
    /// The auto-saved snapshots folder (W1 最近记录)
    pub snapshots: String,
    /// The settings file on disk is invalid; the last valid values are in use
    #[serde(default)]
    pub error: String,
    /// Size unit base after resolving 'system'
    pub unit_base: UnitBase,
    /// Interface language after resolving 'system'
    #[serde(default)]
    pub language: Language,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SettingsUpdateParams {
    /// Keys to change; others keep their value
    pub values: BTreeMap<String, SettingValue>,
    /// Reject the change if the document moved on since
    #[serde(default)]
    pub version: Option<i64>,
}

impl Validate for SettingsUpdateParams {}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SettingsResetParams {
    /// None resets every setting
    #[serde(default)]
    pub category: Option<SettingCategory>,
}

impl Validate for SettingsResetParams {}

/// ``settings.changed``: pushed to every session after any change (also edits of the file).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SettingsChangedEvent {
    pub state: SettingsState,
}

// --------------------------------------------------------------------------- 「高级」 (UI-W7-36)

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SettingsFileParams {
    pub path: String,
}

impl Validate for SettingsFileParams {
    fn validate(&self) -> ValidationResult {
        v::str_len("path", &self.path, 1, 32767)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SettingChange {
    pub key: String,
    pub name: String,
    pub before: SettingValue,
    pub after: SettingValue,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SettingsImportParams {
    pub path: String,
    /// False: only tell what would change (the user confirms)
    #[serde(default)]
    pub apply: bool,
}

impl Validate for SettingsImportParams {
    fn validate(&self) -> ValidationResult {
        v::str_len("path", &self.path, 1, 32767)
    }
}

str_enum! {
    pub enum ImportSource {
        Spaceknife = "spaceknife",
        Spacesniffer = "spacesniffer",
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SettingsImportResult {
    pub source: ImportSource,
    pub changes: Vec<SettingChange>,
    /// The new file class table, if it changes
    #[serde(default)]
    pub classes: Option<Vec<FilterClassInfo>>,
    /// What could not be taken over, and why
    #[serde(default)]
    pub skipped: Vec<String>,
    #[serde(default)]
    pub applied: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MaintenanceInfo {
    pub helper_running: bool,
    pub helper_elevated: bool,
    pub cache_dir: String,
    pub cache_bytes: i64,
    pub snapshots_dir: String,
    pub snapshots_bytes: i64,
    pub snapshot_count: i64,
    /// Scan roots with remembered tags
    pub saved_tags: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setting_values_keep_their_json_type() {
        let v: Vec<SettingValue> = serde_json::from_str(r#"[true, 3, 2.5, "x", 1.0]"#).unwrap();
        assert_eq!(
            v,
            [
                SettingValue::Bool(true),
                SettingValue::Int(3),
                SettingValue::Float(2.5),
                SettingValue::Str("x".into()),
                SettingValue::Float(1.0)
            ]
        );
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            r#"[true,3,2.5,"x",1.0]"#
        );
        assert!(serde_json::from_str::<UnitBase>("1000").is_ok());
        assert!(serde_json::from_str::<UnitBase>("1001").is_err());
    }
}
