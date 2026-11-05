//! What the 「本机磁盘」 page knows of each volume from its last complete scan, kept across page
//! switches, closed tabs and restarts (UI-W9-13; Python `server/volume_summaries.py`), and the
//! page's state for the life of the backend (disk records read as root).
//!
//! One JSON file in the data directory, one entry per volume: the scan's time, counts, method and
//! snapshot, every file class with its size, and the largest top-level folders. An entry is written
//! when a scan of the whole volume finishes, before its tab closes, and whenever the page reads an
//! open complete scan; a newer scan of the same volume replaces it. An entry stands only for the
//! volume it was taken of: its mount point and identity (the volume serial, else its file system and
//! size) must both match. At most [`KEEP`] entries; one whose volume hasn't been seen for [`FORGET`]
//! goes. Writes go through a temporary file and an atomic rename.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sk_core::settings::SettingsStore;
use sk_core::volume::VolumeInfo;
use sk_protocol::machine::{FolderStat, VolumeScan, VolumeScanSource};
use sk_protocol::scan::{ClassStat, TreeMethod};

use crate::app::{Config, Server};
use crate::handlers::machine::{Elevated, HealthHelper, SharedHealth};
use crate::scans::ScanHandle;

pub const VERSION: i64 = 1;
pub const KEEP: usize = 64;
/// An entry whose volume was not seen for this long goes.
pub fn forget() -> Duration {
    Duration::days(90)
}
/// "seen" is refreshed at most this often (no write per page read).
pub fn seen_every() -> Duration {
    Duration::days(1)
}

/// One volume's summary from its last complete scan.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Kept {
    pub mount: String,
    pub identity: String,
    /// when the scan finished
    pub at: DateTime<Utc>,
    /// when the volume was last present
    pub seen: DateTime<Utc>,
    #[serde(default)]
    pub files: i64,
    #[serde(default)]
    pub dirs: Option<i64>,
    #[serde(default)]
    pub bytes_alloc: i64,
    #[serde(default)]
    pub method: Option<TreeMethod>,
    #[serde(default)]
    pub snapshot: Option<String>,
    /// all of them, largest first
    #[serde(default)]
    pub classes: Vec<ClassStat>,
    #[serde(default)]
    pub folders: Vec<FolderStat>,
}

impl Kept {
    /// What the page shows of it (the snapshot only while its file is still there).
    pub fn volume_scan(&self, top: usize) -> VolumeScan {
        let snap = self.snapshot.clone().filter(|s| Path::new(s).is_file());
        VolumeScan {
            source: VolumeScanSource::Kept,
            scan_id: None,
            snapshot: snap,
            running: false,
            at: Some(self.at),
            files: self.files,
            dirs: self.dirs,
            bytes_alloc: self.bytes_alloc,
            method: self.method,
            classes: self.classes.iter().take(top).cloned().collect(),
            folders: self.folders.clone(),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct File {
    version: i64,
    volumes: Vec<Kept>,
}

/// Which volume it is, beyond its mount point.
pub fn identity(v: &VolumeInfo) -> String {
    if v.serial.is_empty() {
        format!("{}:{}", v.fs_type.to_lowercase(), v.total)
    } else {
        format!("serial:{}", v.serial)
    }
}

fn norm(mount: &str) -> String {
    let n = sk_store::names::normpath(mount.as_bytes());
    String::from_utf8_lossy(&sk_store::names::normcase(&n)).into_owned()
}

fn key(mount: &str, ident: &str) -> String {
    format!("{}|{ident}", norm(mount))
}

/// The kept summaries, on disk.
pub struct SummaryStore {
    pub file: PathBuf,
    entries: Mutex<HashMap<String, Kept>>,
}

impl SummaryStore {
    pub fn open(file: Option<PathBuf>) -> SummaryStore {
        let file = file.unwrap_or_else(|| sk_core::paths::data_dir().join("volume_summaries.json"));
        let mut entries = HashMap::new();
        match std::fs::read(&file) {
            Ok(bytes) => match serde_json::from_slice::<File>(&bytes) {
                Ok(f) if f.version == VERSION => {
                    entries = f.volumes.into_iter().map(|e| (key(&e.mount, &e.identity), e)).collect();
                }
                Ok(_) => {}
                Err(e) => tracing::warn!("volume summaries unreadable, starting over: {e}"),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!("volume summaries unreadable, starting over: {e}"),
        }
        SummaryStore { file, entries: Mutex::new(entries) }
    }

    fn save(&self, entries: &HashMap<String, Kept>) {
        let mut volumes: Vec<Kept> = entries.values().cloned().collect();
        volumes.sort_by(|a, b| a.mount.cmp(&b.mount));
        let payload = File { version: VERSION, volumes };
        let write = || -> std::io::Result<()> {
            if let Some(dir) = self.file.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = self.file.with_extension("tmp");
            std::fs::write(&tmp, serde_json::to_vec(&payload).map_err(std::io::Error::other)?)?;
            std::fs::rename(&tmp, &self.file)
        };
        if let Err(e) = write() {
            // a summary is a convenience: never fail the page for it
            tracing::warn!("could not save volume summaries: {e}");
        }
    }

    fn prune(entries: &mut HashMap<String, Kept>, now: DateTime<Utc>) {
        let mut live: Vec<Kept> = entries.values().filter(|e| now - e.seen <= forget()).cloned().collect();
        live.sort_by_key(|e| std::cmp::Reverse(e.seen));
        live.truncate(KEEP);
        *entries = live.into_iter().map(|e| (key(&e.mount, &e.identity), e)).collect();
    }

    pub fn get(&self, v: &VolumeInfo) -> Option<Kept> {
        self.entries.lock().get(&key(&v.mount_point, &identity(v))).cloned()
    }

    /// Keep these, each unless an entry of a newer scan of the same volume is kept already.
    pub fn put(&self, entries: impl IntoIterator<Item = Kept>) {
        let mut g = self.entries.lock();
        let mut changed = false;
        for e in entries {
            let k = key(&e.mount, &e.identity);
            if let Some(old) = g.get(&k) {
                let same = Kept { seen: e.seen, ..old.clone() } == e;
                if old.at > e.at || same {
                    continue;
                }
            }
            g.insert(k, e);
            changed = true;
        }
        if changed {
            Self::prune(&mut g, Utc::now());
            self.save(&g);
        }
    }

    /// These volumes are present: their entries stay (refreshed at most once a day).
    pub fn seen(&self, volumes: &[VolumeInfo], now: Option<DateTime<Utc>>) {
        let now = now.unwrap_or_else(Utc::now);
        let mut g = self.entries.lock();
        let mut changed = false;
        for v in volumes {
            if let Some(e) = g.get_mut(&key(&v.mount_point, &identity(v)))
                && now - e.seen >= seen_every()
            {
                e.seen = now;
                changed = true;
            }
        }
        let before = g.len();
        Self::prune(&mut g, now);
        if changed || g.len() != before {
            self.save(&g);
        }
    }

    pub fn len(&self) -> usize {
        self.entries.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Picks the mount point a scan covers whole (tests replace it).
pub type WholeRoot = Arc<dyn Fn(&ScanHandle) -> Option<String> + Send + Sync>;
/// Asks the system for the disks (tests replace it).
pub type ProbeFn = Arc<dyn Fn(&[VolumeInfo]) -> sk_core::disks::Probe + Send + Sync>;

/// The page's state: kept summaries, disk records read as root, test hooks.
pub struct VolumeSummaries {
    store: Mutex<Arc<SummaryStore>>,
    /// disk → what root read; for the life of the backend
    pub elevated: Mutex<HashMap<String, Elevated>>,
    whole_root: Mutex<Option<WholeRoot>>,
    probe: Mutex<Option<ProbeFn>>,
    helper: Mutex<Option<Arc<dyn HealthHelper>>>,
}

impl VolumeSummaries {
    pub fn new(_config: &Config, _settings: &Arc<SettingsStore>) -> VolumeSummaries {
        VolumeSummaries {
            store: Mutex::new(Arc::new(SummaryStore::open(None))),
            elevated: Mutex::new(HashMap::new()),
            whole_root: Mutex::new(None),
            probe: Mutex::new(None),
            helper: Mutex::new(None),
        }
    }

    /// The server is in its `Arc`.
    pub fn started(&self, _server: &Arc<Server>) {}

    pub fn store(&self) -> Arc<SummaryStore> {
        self.store.lock().clone()
    }

    /// Read the summaries file again (tests: as after a restart).
    pub fn reload(&self) {
        let file = self.store().file.clone();
        *self.store.lock() = Arc::new(SummaryStore::open(Some(file)));
    }

    /// A scan finished (or closes): keep what it showed. Runs on a blocking thread.
    pub fn keep(&self, server: &Arc<Server>, handle: &Arc<ScanHandle>) {
        if let Some(entry) = crate::handlers::machine::kept_of(server, handle) {
            self.store().put([entry]);
        }
    }

    pub fn whole_root(&self, h: &ScanHandle) -> Option<String> {
        let f = self.whole_root.lock().clone();
        match f {
            Some(f) => f(h),
            None => crate::handlers::machine::whole_root(h),
        }
    }

    /// Tests: which scans stand for a whole volume.
    pub fn set_whole_root(&self, f: Option<WholeRoot>) {
        *self.whole_root.lock() = f;
    }

    /// Tests: what the disk probe finds.
    pub fn set_probe(&self, f: Option<ProbeFn>) {
        *self.probe.lock() = f;
    }

    pub fn probe(&self, volumes: &[VolumeInfo]) -> sk_core::disks::Probe {
        let f = self.probe.lock().clone();
        match f {
            Some(f) => f(volumes),
            None => sk_core::disks::probe(volumes),
        }
    }

    /// Tests: the elevated helper the records are read through.
    pub fn set_helper(&self, h: Option<Arc<dyn HealthHelper>>) {
        *self.helper.lock() = h;
    }

    pub fn helper(&self, server: &Server) -> Arc<dyn HealthHelper> {
        let h = self.helper.lock().clone();
        h.unwrap_or_else(|| Arc::new(SharedHealth(server.scans.helper.clone())))
    }
}
