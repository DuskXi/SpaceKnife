//! Update checks against the project's GitHub releases (`update.*`, capability `updates.v1`).
//!
//! The backend checks at most once a day (and on request), compares the newest release with the
//! running app's version and tells every session that announced `updates.v1` through
//! `update.changed`. How far behind the app is decides how loudly the front end says so:
//! `patch` (badge only), `minor` (also a card), `major` (also a dialog). `update.dismiss` keeps the
//! card / dialog away for that version; `update.mute` turns off every prompt of a series.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::base::{DateTime, yes};
use crate::macros::str_enum;
use crate::validate::{self as v, Validate, ValidationResult};

str_enum! {
    pub enum UpdateLevel {
        None = "none",
        Patch = "patch",
        Minor = "minor",
        Major = "major",
    }
}

impl Default for UpdateLevel {
    fn default() -> Self {
        UpdateLevel::None
    }
}

str_enum! {
    pub enum InstallKind {
        Nsis = "nsis",
        Portable = "portable",
        Appimage = "appimage",
        Deb = "deb",
        Rpm = "rpm",
        Dmg = "dmg",
        Dev = "dev",
        Unknown = "unknown",
    }
}

str_enum! {
    pub enum UpdateChannel {
        Stable = "stable",
        Preview = "preview",
    }
}

str_enum! {
    pub enum DownloadStateName {
        Idle = "idle",
        Downloading = "downloading",
        Verifying = "verifying",
        Done = "done",
        Failed = "failed",
        Cancelled = "cancelled",
    }
}

impl Default for DownloadStateName {
    fn default() -> Self {
        DownloadStateName::Idle
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct UpdateAsset {
    /// File name, e.g. SpaceKnife_0.4.0_x64-setup.exe (`sk_core::update::asset_template`)
    pub name: String,
    /// Bytes
    pub size: u64,
    /// Its download URL on github.com
    pub url: String,
    /// From the release's SHA256SUMS*.txt; None when it publishes none
    #[serde(default)]
    pub sha256: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct UpdateInfo {
    /// Without the leading v: 0.4.0, 0.4.0-beta.1
    pub version: String,
    pub tag: String,
    /// The release title
    pub name: String,
    #[serde(default)]
    pub published_at: Option<DateTime>,
    /// Marked as a pre-release on GitHub
    pub prerelease: bool,
    /// The release page
    pub url: String,
    /// Release notes (Markdown), at most 16 KiB
    #[serde(default)]
    pub notes: String,
    /// The file for this kind of install; None: only the release page
    #[serde(default)]
    pub asset: Option<UpdateAsset>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct UpdateDownload {
    #[serde(default)]
    pub state: DownloadStateName,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub received: u64,
    #[serde(default)]
    pub total: u64,
    /// The downloaded file, once done
    #[serde(default)]
    pub path: Option<String>,
    /// SHA-256 matched (True), did not (never kept), no checksum published (None)
    #[serde(default)]
    pub verified: Option<bool>,
    /// Why it failed, in the session's language
    #[serde(default)]
    pub error: Option<String>,
}

/// A portable copy's update that was undone after the app had quit: the old version was kept or
/// came back (the swap's note `rollback-<version>.json` in the updates folder).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct UpdateRollback {
    /// The version that was not put in place, or was taken out again
    pub version: String,
    /// What happened, in the session's language
    pub reason: String,
    /// The swap's own words (English), as its log has them
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub at: Option<DateTime>,
    /// The update's log, on the computer SpaceKnife runs on
    #[serde(default)]
    pub log: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct UpdateStatus {
    /// Version of the running app
    pub current: String,
    pub install_kind: InstallKind,
    /// Automatic checks are on (setting general.update_check)
    pub enabled: bool,
    pub channel: UpdateChannel,
    #[serde(default)]
    pub checking: bool,
    /// Last successful check
    #[serde(default)]
    pub checked_at: Option<DateTime>,
    /// The last check failed: why (offline, rate limited)
    #[serde(default)]
    pub error: Option<String>,
    /// How far behind the newest release this app is
    #[serde(default)]
    pub level: UpdateLevel,
    /// The newest release, when it is newer
    #[serde(default)]
    pub latest: Option<UpdateInfo>,
    /// The card / dialog for this version was closed (the badge stays)
    #[serde(default)]
    pub dismissed: bool,
    /// The series of the newest release that update.mute takes: its major.minor for a minor update ("0.4"), its major for a major one ("1"); None for a patch release or no update
    #[serde(default)]
    pub series: Option<String>,
    /// The user turned off the prompts of that series: no card, dialog or badge, no automatic download (the 「关于与更新」 page still shows it)
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub download: UpdateDownload,
    /// When the next automatic check is due; None while automatic checks are off, and where the backend makes none (tests, demos, tools)
    #[serde(default)]
    pub next_check_at: Option<DateTime>,
    /// The last update of this portable copy was undone (to a version newer than this one): shown until update.dismiss, which also removes the note
    #[serde(default)]
    pub rollback: Option<UpdateRollback>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct UpdateReleases {
    /// The recent releases, newest first (at most 30, notes at most 16 KiB each, no asset)
    pub releases: Vec<UpdateInfo>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateVersionParams {
    /// The version shown (latest.version)
    pub version: String,
}

impl Validate for UpdateVersionParams {
    fn validate(&self) -> ValidationResult {
        v::str_len("version", &self.version, 1, 64)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateMuteParams {
    /// A series as UpdateStatus.series names it: "0.4" (every 0.4.x) or "1" (every 1.x)
    pub series: String,
    /// False turns its prompts back on
    #[serde(default = "yes")]
    pub muted: bool,
}

impl Validate for UpdateMuteParams {
    fn validate(&self) -> ValidationResult {
        v::pattern(
            "series",
            v::is_series(&self.series),
            r"^\d{1,6}(\.\d{1,6})?$",
        )
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct UpdateChangedEvent {
    pub status: UpdateStatus,
}
