//! `machine.disks` (the 「本机磁盘」 page) and `machine.read_health` (the disks' own records that
//! need administrator rights; Python `server/machine_handlers.py`).
//!
//! The disks and the volumes' place on them come from [`sk_core::disks::probe`]; what is known of
//! each volume comes from a scan of its root open in this backend (counts, and once it is done the
//! largest file classes and top-level folders), else from what was kept of its last complete scan
//! ([`crate::volume_summaries`]), else from the newest snapshot of it (counts only, the header).
//! The headers of the volume's complete snapshots give its history. Records read as root are kept
//! for the life of the backend; while the elevated helper still runs later reads ask it again.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use sk_core::disks::Probe;
use sk_core::helper::{HelperClient, HelperError, HelperErrorKind, Scalars, SharedHelper};
use sk_core::scan::ScanState;
use sk_core::volume::{VolumeInfo, list_volumes};
use sk_protocol::machine::{
    ClassTotal, DiskHealth, DiskInterface, DiskPart, FolderStat, HealthNote, MachineDisks, PhysicalDisk,
    ReadHealthOutcome, ReadHealthResult, SnapshotPoint, VolumeOverview, VolumeScan, VolumeScanSource,
};
use sk_protocol::methods::{MachineDisksMethod, MachineReadHealth};
use sk_protocol::scan::{ClassStat, ScanMethodUsed, ScanSource, TreeMethod};
use sk_store::{Kind, SortBy};

use crate::app::Server;
use crate::compute::Latest;
use crate::rpc::{RpcResult, Router};
use crate::scans::{ScanHandle, saved_method, saved_state};
use crate::topics::TopicRegistry;
use crate::volume_summaries::{Kept, identity};

pub const TOP_CLASSES: usize = 5;
pub const TOP_FOLDERS: usize = 3;
/// Snapshots of a volume for its growth.
pub const HISTORY: usize = 12;
/// A snapshot saved this long after a kept scan finished is from a later scan.
pub fn later() -> Duration {
    Duration::minutes(10)
}

fn norm(path: &str) -> String {
    let n = sk_store::names::normpath(path.as_bytes());
    String::from_utf8_lossy(&sk_store::names::normcase(&n)).into_owned()
}

/// The mount point a scan covers whole (one root, the volume's), else None.
pub fn whole_root(h: &ScanHandle) -> Option<String> {
    let roots = h.roots.read();
    if roots.len() != 1 {
        return None;
    }
    let r = &roots[0];
    let v = r.volume.as_ref()?;
    (r.whole_volume || norm(&r.path.to_string_lossy()) == norm(&v.mount_point)).then(|| norm(&v.mount_point))
}

/// A scan of the disk beats a loaded snapshot; then the newest.
fn rank(h: &ScanHandle) -> (bool, i64) {
    let started = h.progress().started_at.map_or(i64::MIN, |t| t.timestamp_micros());
    (h.source == ScanSource::Scan, started)
}

/// Whether the scan is still going (its tree incomplete). The finish of a scan (where the keeper
/// runs) counts as done.
fn still_running(h: &ScanHandle) -> bool {
    h.running() && matches!(h.progress().state, ScanState::Queued | ScanState::Scanning)
}

/// What an open scan shows, and every class with bytes (largest first; [] while it runs).
/// `Err` (the reason) when its summary lacks the class totals or the folder classes: partial
/// totals are never shown (or kept) as the volume's make-up.
pub fn live_full(server: &Server, h: &ScanHandle) -> Result<(VolumeScan, Vec<ClassStat>), String> {
    let st = h.status(server);
    let running = still_running(h);
    let mut every = Vec::new();
    let mut folders = Vec::new();
    if !running && let Some(node) = h.roots.read().first().map(|r| r.node) {
        let summary = crate::handlers::scan::summary_of(h, node, 0)?;
        if let Some(gap) = summary
            .gaps
            .iter()
            .find(|g| matches!(g.part, sk_protocol::scan::SummaryPart::Contents | sk_protocol::scan::SummaryPart::Classes))
        {
            return Err(gap.reason.clone());
        }
        every = summary.classes.into_iter().filter(|c| c.bytes > 0).collect();
        let nodes = h.store.read();
        folders = nodes
            .sorted_children(node, SortBy::Size, true)
            .into_iter()
            .filter(|&i| nodes.kind(i) == Kind::Dir)
            .take(TOP_FOLDERS)
            .map(|i| FolderStat { name: nodes.name(i).into_owned(), bytes: nodes.tot_alloc(i) })
            .collect();
    }
    let method = match st.method {
        ScanMethodUsed::Mft => Some(TreeMethod::Mft),
        ScanMethodUsed::Hard => Some(TreeMethod::Hard),
        ScanMethodUsed::Snapshot => None,
    };
    let scan = VolumeScan {
        source: if h.source == ScanSource::Snapshot { VolumeScanSource::Snapshot } else { VolumeScanSource::Scan },
        scan_id: Some(h.id.clone()),
        snapshot: h.st.lock().snapshot_path.clone(),
        running,
        at: st.finished_at.or(st.started_at),
        files: st.files,
        dirs: Some(st.dirs),
        bytes_alloc: st.bytes_alloc,
        method,
        classes: every.iter().take(TOP_CLASSES).cloned().collect(),
        folders,
    };
    Ok((scan, every))
}

/// What to keep of a complete scan of a whole volume; None for anything else.
fn keep_entry(server: &Server, h: &ScanHandle, scan: &VolumeScan, every: &[ClassStat]) -> Option<Kept> {
    let volume = h.roots.read().first().and_then(|r| r.volume.clone())?;
    server.summaries.whole_root(h)?;
    if scan.running || h.progress().state != ScanState::Done {
        return None; // a stopped scan's counts are partial
    }
    Some(Kept {
        mount: volume.mount_point.clone(),
        identity: identity(&volume),
        at: scan.at?,
        seen: Utc::now(),
        files: scan.files,
        dirs: scan.dirs,
        bytes_alloc: scan.bytes_alloc,
        method: scan.method,
        snapshot: scan.snapshot.clone(),
        classes: every.iter().map(|c| ClassStat { parts: Vec::new(), ..c.clone() }).collect(),
        folders: scan.folders.clone(),
    })
}

/// What to keep of an open scan (it sums the scan's classes: a blocking thread).
pub fn kept_of(server: &Server, h: &ScanHandle) -> Option<Kept> {
    server.summaries.whole_root(h)?;
    if still_running(h) {
        return None;
    }
    let (scan, every) = live_full(server, h).ok()?;
    keep_entry(server, h, &scan, &every)
}

/// The newest complete snapshot of a whole volume and its history.
pub type Saved = (Option<VolumeScan>, Vec<SnapshotPoint>);

/// The newest complete snapshot of a whole volume and the history of its complete snapshots,
/// newest first (headers only; stopped scans saved by hand are left out).
pub fn saved_scans(mount_point: &str, base: Option<&Path>) -> Saved {
    let base = base.map_or_else(sk_core::paths::snapshots_dir, Path::to_path_buf);
    let folder = base.join(sk_store::snapshot::root_key(&[mount_point.to_owned()]));
    let Ok(rd) = std::fs::read_dir(&folder) else { return (None, Vec::new()) };
    let mut files: Vec<PathBuf> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == sk_store::snapshot::SUFFIX))
        .collect();
    files.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    let mut newest = None;
    let mut history = Vec::new();
    for file in files {
        if history.len() >= HISTORY {
            break;
        }
        let Ok(header) = sk_store::snapshot::read_header(&file) else { continue };
        let stats = &header.stats;
        if saved_state(stats) != ScanState::Done {
            continue;
        }
        let files_n = stats.get("files").and_then(|v| v.as_i64()).unwrap_or(0);
        let alloc = stats.get("bytes_alloc").and_then(|v| v.as_i64()).unwrap_or(0);
        let at = DateTime::from_timestamp(header.created_at as i64, 0).unwrap_or_default();
        if newest.is_none() {
            newest = Some(VolumeScan {
                source: VolumeScanSource::Snapshot,
                scan_id: None,
                snapshot: Some(file.display().to_string()),
                running: false,
                at: Some(at),
                files: files_n,
                dirs: stats.get("dirs").and_then(|v| v.as_i64()),
                bytes_alloc: alloc,
                method: saved_method(stats),
                classes: Vec::new(),
                folders: Vec::new(),
            });
        }
        history.push(SnapshotPoint { at, bytes_alloc: alloc, files: files_n });
    }
    (newest, history)
}

/// What was kept of a volume's last complete scan.
pub type KeptOf<'a> = dyn Fn(&VolumeInfo) -> Option<Kept> + 'a;

/// The page's data (on a blocking thread: summing an open scan's classes reads its columns).
///
/// `kept` gives what was kept of a volume's last complete scan; `fresh` collects what to keep of
/// the complete scans open now.
pub fn overview(
    server: &Server,
    volumes: &[VolumeInfo],
    found: &Probe,
    saved: &HashMap<String, Saved>,
    kept: Option<&KeptOf<'_>>,
    mut fresh: Option<&mut Vec<Kept>>,
) -> MachineDisks {
    let mut open: HashMap<String, Arc<ScanHandle>> = HashMap::new();
    for h in server.scans.list() {
        let Some(root) = server.summaries.whole_root(&h) else { continue };
        let better = open.get(&root).is_none_or(|best| rank(&h) > rank(best));
        if better {
            open.insert(root, h);
        }
    }
    let mut out_volumes = Vec::new();
    let mut totals: BTreeMap<String, (i64, i64)> = BTreeMap::new();
    let mut covered = Vec::new();
    let mut covered_bytes = 0i64;
    for v in volumes {
        let model = v.to_protocol();
        let (newest, history) = saved.get(&v.mount_point).cloned().unwrap_or((None, Vec::new()));
        let k = kept.and_then(|f| f(v));
        let mut every: Vec<ClassStat> = Vec::new();
        let mut mix_bytes = 0;
        let live = open.get(&norm(&v.mount_point)).and_then(|h| match live_full(server, h) {
            Ok(got) => Some((h, got)),
            Err(reason) => {
                tracing::debug!(scan_id = %h.id, "volume make-up left out: {reason}");
                None
            }
        });
        let scan = if let Some((h, (scan, all))) = live {
            every = all;
            mix_bytes = scan.bytes_alloc;
            if let Some(fresh) = fresh.as_deref_mut()
                && let Some(entry) = keep_entry(server, h, &scan, &every)
            {
                fresh.push(entry);
            }
            if scan.running
                && let Some(k) = &k
            {
                // scanned again: its last mix counts meanwhile
                every = k.classes.clone();
                mix_bytes = k.bytes_alloc;
            }
            Some(scan)
        } else if let Some(k) = k.as_ref().filter(|k| match &newest {
            None => true,
            Some(n) => n.at.is_none() || n.snapshot == k.snapshot || n.at.is_some_and(|a| a <= k.at + later()),
        }) {
            every = k.classes.clone();
            mix_bytes = k.bytes_alloc;
            Some(k.volume_scan(TOP_CLASSES))
        } else {
            newest
        };
        if !every.is_empty() {
            for c in &every {
                let t = totals.entry(c.class_id.clone()).or_default();
                t.0 += c.bytes;
                t.1 += c.files;
            }
            covered.push(v.mount_point.clone());
            covered_bytes += mix_bytes;
        }
        let mut ids: Vec<String> = Vec::new();
        for (disk, _) in found.parts.get(&v.mount_point).map(Vec::as_slice).unwrap_or(&[]) {
            if !ids.contains(disk) {
                ids.push(disk.clone());
            }
        }
        out_volumes.push(VolumeOverview { volume: model, disks: ids, scan, history });
    }
    // each disk's volumes, in disk order (those with no known offset last)
    type Placed<'a> = Vec<(i64, &'a str, Option<i64>)>;
    let mut on_disk: HashMap<&str, Placed> = HashMap::new();
    for (mount, parts) in &found.parts {
        let start = found.offsets.get(mount).copied().unwrap_or(1 << 62);
        for (disk, size) in parts {
            on_disk.entry(disk.as_str()).or_default().push((start, mount.as_str(), *size));
        }
    }
    let disks = found
        .disks
        .iter()
        .map(|d| {
            let mut list = on_disk.get(d.id.as_str()).cloned().unwrap_or_default();
            list.sort();
            PhysicalDisk {
                parts: list.into_iter().map(|(_, m, b)| DiskPart { mount_point: m.to_owned(), bytes: b }).collect(),
                ..d.clone()
            }
        })
        .collect();
    let classes = (!covered.is_empty()).then(|| {
        let mut list: Vec<(String, (i64, i64))> = totals.into_iter().filter(|(_, (b, _))| *b > 0).collect();
        list.sort_by(|a, b| b.1.0.cmp(&a.1.0).then_with(|| a.0.cmp(&b.0)));
        ClassTotal {
            classes: list
                .into_iter()
                .map(|(class_id, (bytes, files))| ClassStat { class_id, bytes, files, parts: Vec::new() })
                .collect(),
            bytes: covered_bytes,
            volumes: covered,
        }
    });
    MachineDisks { disks, volumes: out_volumes, collected_at: Utc::now(), classes }
}

// --------------------------------------------------------------------------- records read as root

/// (the record or None when the disk didn't answer, the note then, when it was read)
pub type Elevated = (Option<DiskHealth>, HealthNote, DateTime<Utc>);

/// An elevated helper that reads disk records (the real one, or a test's).
pub trait HealthClient: Send + Sync {
    fn elevated(&self) -> bool;
    fn disk_health(&self, disk: &str) -> Result<Scalars, HelperError>;
}

/// Where elevated helpers come from.
pub trait HealthHelper: Send + Sync {
    /// The running helper, if any (no prompt).
    fn current(&self) -> Option<Arc<dyn HealthClient>>;
    /// The running elevated helper, or a new one (the prompt). Blocks.
    fn get(&self, elevate: bool) -> Result<Arc<dyn HealthClient>, HelperError>;
}

impl HealthClient for HelperClient {
    fn elevated(&self) -> bool {
        HelperClient::elevated(self)
    }

    fn disk_health(&self, disk: &str) -> Result<Scalars, HelperError> {
        HelperClient::disk_health(self, disk)
    }
}

/// The backend's shared helper as a [`HealthHelper`].
pub struct SharedHealth(pub Arc<SharedHelper>);

impl HealthHelper for SharedHealth {
    fn current(&self) -> Option<Arc<dyn HealthClient>> {
        self.0.current().map(|c| c as Arc<dyn HealthClient>)
    }

    fn get(&self, elevate: bool) -> Result<Arc<dyn HealthClient>, HelperError> {
        self.0.get(elevate).map(|c| c as Arc<dyn HealthClient>)
    }
}

/// A kernel name can move to another disk (hot-plug): the serial pins it.
fn record_key(d: &PhysicalDisk) -> String {
    format!("{}|{}", d.id, d.serial)
}

/// The disk with what an elevated read found: the record (keeping the live temperature), or the
/// reason it has none.
pub fn with_record(d: &PhysicalDisk, read: Option<&Elevated>) -> PhysicalDisk {
    let Some((record, note, at)) = read else { return d.clone() };
    let Some(record) = record else {
        return PhysicalDisk { health_note: *note, ..d.clone() };
    };
    let live = d.health.as_ref().and_then(|h| h.temperature_c);
    let merged = DiskHealth { temperature_c: live.or(record.temperature_c), read_at: Some(*at), ..record.clone() };
    PhysicalDisk { health: Some(merged), health_note: HealthNote::None, ..d.clone() }
}

pub fn failed_note(d: &PhysicalDisk) -> HealthNote {
    if d.interface == DiskInterface::Usb { HealthNote::Usb } else { HealthNote::Unsupported }
}

fn pages_of(s: &Scalars) -> BTreeMap<String, String> {
    s.iter().filter_map(|(k, v)| v.as_str().map(|t| (k.clone(), t.to_owned()))).collect()
}

/// Ask the elevated helper for each disk's pages; the cache gets the record or the reason. An
/// unavailable helper ends it (the error); a disk that doesn't answer doesn't stop the rest.
pub fn read_records(
    client: &dyn HealthClient,
    targets: &[PhysicalDisk],
    cache: &mut HashMap<String, Elevated>,
) -> Result<(), HelperError> {
    for d in targets {
        let at = Utc::now();
        match client.disk_health(&d.id) {
            Ok(pages) => {
                let record = sk_core::disks::health::from_pages(&pages_of(&pages));
                let note = if record.is_some() { HealthNote::None } else { failed_note(d) };
                cache.insert(record_key(d), (record, note, at));
            }
            Err(e) if e.kind == HelperErrorKind::Error => {
                tracing::info!("disk {} has no record for the helper: {}", d.id, e.message);
                cache.insert(record_key(d), (None, failed_note(d), at));
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// The volumes and the disks (blocking).
fn probe(server: &Server) -> (Vec<VolumeInfo>, Probe) {
    let volumes = list_volumes();
    for v in volumes.iter().filter(|v| v.reason.is_empty()) {
        let _ = sk_core::media::media_of(&v.mount_point, &v.device, v.is_remote); // warm the cache
    }
    let found = server.summaries.probe(&volumes);
    (volumes, found)
}

/// Read the kept records again while the elevated helper still runs (no prompt). Blocking.
fn refresh(server: &Server, found: &Probe) {
    let helper = server.summaries.helper(server);
    let mut cache = server.summaries.elevated.lock();
    if cache.is_empty() {
        return;
    }
    let Some(client) = helper.current().filter(|c| c.elevated()) else { return };
    let again: Vec<PhysicalDisk> = found
        .disks
        .iter()
        .filter(|d| cache.get(&record_key(d)).is_some_and(|e| e.0.is_some()))
        .cloned()
        .collect();
    let _ = read_records(&*client, &again, &mut cache);
}

/// The page (blocking): records merged, snapshots read, open scans summed; what to keep is kept.
fn page(server: &Server, volumes: &[VolumeInfo], mut found: Probe) -> MachineDisks {
    {
        let cache = server.summaries.elevated.lock();
        found.disks = found.disks.iter().map(|d| with_record(d, cache.get(&record_key(d)))).collect();
    }
    let saved: HashMap<String, Saved> = volumes
        .iter()
        .filter(|v| v.reason.is_empty())
        .map(|v| (v.mount_point.clone(), saved_scans(&v.mount_point, None)))
        .collect();
    let store = server.summaries.store();
    let get = |v: &VolumeInfo| store.get(v);
    let mut fresh = Vec::new();
    let out = overview(server, volumes, &found, &saved, Some(&get), Some(&mut fresh));
    if !fresh.is_empty() {
        store.put(fresh);
    }
    store.seen(volumes, None);
    out
}

async fn machine_disks(server: Arc<Server>, latest: Arc<Latest<String>>, session: String) -> RpcResult<MachineDisks> {
    let srv = server.clone();
    let (volumes, found) = crate::compute::blocking(move || {
        let (volumes, found) = probe(&srv);
        refresh(&srv, &found);
        (volumes, found)
    })
    .await?;
    latest.run(format!("machine.disks:{session}"), move || page(&server, &volumes, found)).await
}

pub fn install(r: &mut Router, _t: &mut TopicRegistry) {
    let latest: Arc<Latest<String>> = Arc::new(Latest::new());
    let l = latest.clone();
    r.add::<MachineDisksMethod, _, _>(move |call, _| machine_disks(call.server.clone(), l.clone(), call.session.id.clone()));

    r.add::<MachineReadHealth, _, _>(move |call, p| {
        let server = call.server.clone();
        async move {
            crate::compute::blocking(move || {
                let (volumes, found) = probe(&server);
                let wanted: Vec<String> = p.disks.clone();
                let targets: Vec<PhysicalDisk> = {
                    let cache = server.summaries.elevated.lock();
                    found
                        .disks
                        .iter()
                        .map(|d| with_record(d, cache.get(&record_key(d)))) // read once is enough
                        .filter(|d| d.health_note == HealthNote::NeedsAdmin && (wanted.is_empty() || wanted.contains(&d.id)))
                        .collect()
                };
                let mut outcome = ReadHealthOutcome::Nothing;
                let mut message = String::new();
                if !targets.is_empty() {
                    let helper = server.summaries.helper(&server);
                    let res = helper.get(true).and_then(|client| {
                        let mut cache = server.summaries.elevated.lock();
                        read_records(&*client, &targets, &mut cache)
                    });
                    match res {
                        Ok(()) => outcome = ReadHealthOutcome::Read,
                        Err(e) if e.kind == HelperErrorKind::Denied => {
                            outcome = ReadHealthOutcome::Denied;
                            message = e.message;
                        }
                        Err(e) => {
                            tracing::info!("disk records need administrator rights, not available: {}", e.message);
                            outcome = ReadHealthOutcome::Unavailable;
                            message = e.message;
                        }
                    }
                }
                let machine = page(&server, &volumes, found);
                ReadHealthResult { outcome, message, machine }
            })
            .await
        }
    });
}
