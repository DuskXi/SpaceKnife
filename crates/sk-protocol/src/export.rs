//! Exports (M6, docs/manage.md §4, docs/export-templates.md): SpaceSniffer templates, CSV / JSON / HTML.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::base::yes;
use crate::macros::str_enum;
use crate::validate::{self as v, Validate, ValidationResult};

str_enum! {
    pub enum ExportTargetKind {
        Template = "template",
        Format = "format",
    }
}

/// Something to export with: a built-in template or a format.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExportTarget {
    /// What export.run takes: the template's SpaceSniffer name, or csv / json / html
    pub name: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub kind: ExportTargetKind,
    /// Suggested file extension, without the dot
    pub extension: String,
    /// The template's own suggested file name
    #[serde(default)]
    pub file_name: String,
    /// Also built into SpaceSniffer
    #[serde(default)]
    pub spacesniffer: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExportTargets {
    pub targets: Vec<ExportTarget>,
}

str_enum! {
    pub enum IssueSeverity {
        Error = "error",
        Warning = "warning",
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExportIssue {
    pub severity: IssueSeverity,
    pub code: String,
    pub message: String,
    pub section: String,
    pub line: i64,
    pub column: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExportCheckParams {
    pub source: String,
}

impl Validate for ExportCheckParams {
    fn validate(&self) -> ValidationResult {
        v::str_len("source", &self.source, 0, 1 << 20)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExportCheckResult {
    pub ok: bool,
    pub issues: Vec<ExportIssue>,
}

str_enum! {
    pub enum ExportScope {
        Zoom = "zoom",
        All = "all",
    }
}

impl Default for ExportScope {
    fn default() -> Self {
        ExportScope::Zoom
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExportRunParams {
    pub view_id: String,
    /// A target name; empty with source
    #[serde(default)]
    pub target: String,
    /// A template's text instead
    #[serde(default)]
    pub source: Option<String>,
    /// The file to write (its folder must exist)
    pub path: String,
    /// The view's zoom root, or every scan root
    #[serde(default)]
    pub scope: ExportScope,
    /// Only what the view's filter matches
    #[serde(default = "yes")]
    pub use_filter: bool,
}

impl Validate for ExportRunParams {
    fn validate(&self) -> ValidationResult {
        v::str_len("view_id", &self.view_id, 1, 64)?;
        v::str_len("target", &self.target, 0, 256)?;
        v::opt_str_len("source", &self.source, 0, 1 << 20)?;
        v::str_len("path", &self.path, 1, 32767)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExportRunResult {
    pub path: String,
    pub bytes: i64,
    /// Warnings of the template
    #[serde(default)]
    pub issues: Vec<ExportIssue>,
}
