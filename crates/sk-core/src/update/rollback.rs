//! The note a portable copy's swap leaves when it kept the old version or brought it back after
//! the app had quit (sk_core::update::swap): `rollback-<version>.json` in the updates folder, next
//! to the update's log. The old version reads it when it starts and says that the update to
//! `<version>` was undone and why (`update.status`'s `rollback`), until the user closes the card
//! (`update.dismiss`, which removes the notes) or runs that version or a newer one (the cleaning of
//! the updates folder removes the note with the rest of that version's update).
//!
//! The note is a contract with every later version: their swap writes it, this version reads it.
//! So it only ever gets fields, and a reason word ([`Rollback::why`]) this version does not know is
//! said in general terms. Version 1:
//!
//! ```json
//! {"v": 1, "version": "0.3.6", "why": "start", "detail": "it ended before it was ready, with exit code 1",
//!  "at": "2026-11-09T12:00:00Z", "log": "/home/lenny/SpaceKnife/data/data/updates/update-0.3.6.log"}
//! ```

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::tr;

pub const NOTE_VERSION: i64 = 1;

/// SpaceKnife did not quit in time: nothing was changed.
pub const WHY_QUIT: &str = "quit";
/// The folders could not be read: nothing was changed.
pub const WHY_READ: &str = "read";
/// The old files stayed held: they were put back.
pub const WHY_BUSY: &str = "busy";
/// The new files could not be moved in: the old ones were put back.
pub const WHY_PLACE: &str = "place";
/// The new version was in place but did not start: it was taken out again.
pub const WHY_START: &str = "start";

fn note_version() -> i64 {
    NOTE_VERSION
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rollback {
    #[serde(default = "note_version")]
    pub v: i64,
    /// The version that was not put in place, or was taken out again
    pub version: String,
    /// Why, as one of the `WHY_*` words
    pub why: String,
    /// The swap's own words (English), as its log has them
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub at: Option<DateTime<Utc>>,
    /// The update's log
    #[serde(default)]
    pub log: Option<String>,
}

impl Rollback {
    pub fn new(version: &str, why: &str, detail: &str, log: Option<&Path>) -> Rollback {
        Rollback {
            v: NOTE_VERSION,
            version: version.to_owned(),
            why: why.to_owned(),
            detail: detail.to_owned(),
            at: Some(Utc::now()),
            log: log.map(|l| l.display().to_string()),
        }
    }

    /// What happened, in the current language.
    pub fn reason(&self) -> String {
        let version = self.version.as_str();
        match self.why.as_str() {
            WHY_QUIT => tr!(
                "SpaceKnife 没有及时退出，没有换上 {version}",
                version = version
            ),
            WHY_READ => tr!(
                "读不了程序所在的文件夹，没有换上 {version}",
                version = version
            ),
            WHY_BUSY => tr!(
                "旧版本的文件一直被占用，没有换上 {version}",
                version = version
            ),
            WHY_PLACE => tr!(
                "{version} 的文件放不进程序文件夹，已换回原来的版本",
                version = version
            ),
            WHY_START => tr!(
                "{version} 换上之后没能启动，已换回原来的版本",
                version = version
            ),
            _ => tr!(
                "更新到 {version} 没有完成，已换回原来的版本",
                version = version
            ),
        }
    }
}

/// The note's file name for `version`.
pub fn file_name(version: &str) -> String {
    format!("rollback-{version}.json")
}

/// Write `note` into `folder` (through a temporary file and a rename); its path.
pub fn write(folder: &Path, note: &Rollback) -> std::io::Result<PathBuf> {
    let file = folder.join(file_name(&note.version));
    let text = serde_json::to_string_pretty(note).map_err(std::io::Error::other)?;
    super::write_atomic(&file, &format!("{text}\n"))?;
    Ok(file)
}

/// Every note in `folder` that reads as one (a broken file is skipped), the latest first.
pub fn read_all(folder: &Path) -> Vec<(PathBuf, Rollback)> {
    let Ok(list) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut out: Vec<(PathBuf, Rollback)> = list
        .filter_map(Result::ok)
        .filter(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.starts_with("rollback-") && name.ends_with(".json")
        })
        .filter_map(|e| {
            let bytes = std::fs::read(e.path()).ok()?;
            let note: Rollback = serde_json::from_slice(&bytes).ok()?;
            (!note.version.is_empty()).then(|| (e.path(), note))
        })
        .collect();
    out.sort_by_key(|(_, note)| std::cmp::Reverse(note.at));
    out
}
