//! Bounded concurrency for file operations (docs/manage.md §2.5, 并发).
//!
//! How many calls an operation keeps in flight on one device follows its media
//! ([`crate::media`], the probe the scanner uses too): NVMe takes many requests at once, a SATA SSD
//! or a network share a few, a spinning disk, a USB stick or an SD card serves them one after the
//! other — more threads there only make it seek. Independent steps of an operation (one item's
//! checks, one item, one folder of a tree being deleted) run on that many threads; what depends on
//! something else waits for it (a folder is deleted after everything in it).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::media::Media;

/// Test seams.
#[doc(hidden)]
pub mod seams {
    use std::sync::atomic::AtomicUsize;

    /// Threads every operation uses whatever the device (0: by its media).
    pub static WORKERS: AtomicUsize = AtomicUsize::new(0);
}

/// Threads for a device of these media.
pub fn workers_for_media(media: Media) -> usize {
    let n = match media {
        Media::NvmeSsd => 8,
        Media::SataSsd | Media::Ssd | Media::UsbSsd | Media::Virtual | Media::Network => 4,
        Media::Hdd | Media::UsbHdd | Media::UsbFlash | Media::SdCard | Media::Unknown => 2,
    };
    let cpus = std::thread::available_parallelism().map_or(2, |c| c.get());
    n.min(cpus.max(2))
}

/// The media of the volume holding `path` (Unknown when it cannot be told).
pub fn media_of_path(path: &Path) -> Media {
    match crate::volume::volume_for(path) {
        Ok(v) if v.reason.is_empty() => {
            crate::media::media_of(&v.mount_point, &v.device, v.is_remote)
        }
        _ => Media::Unknown,
    }
}

/// Threads for work on the volume holding `path` (the test seam first).
pub fn workers_for(path: &Path) -> usize {
    match seams::WORKERS.load(Ordering::Relaxed) {
        0 => workers_for_media(media_of_path(path)),
        n => n,
    }
}

/// Threads for work on the volumes holding `paths`: the fewest any of them takes (one probe per
/// volume: by the drive or share they name on Windows, by their folder's device elsewhere).
pub fn workers_for_all<P: AsRef<Path>>(paths: &[P]) -> usize {
    if let n @ 1.. = seams::WORKERS.load(Ordering::Relaxed) {
        return n;
    }
    let mut seen: Vec<String> = Vec::new();
    let mut folders: HashMap<PathBuf, String> = HashMap::new();
    let mut fewest = usize::MAX;
    for p in paths {
        let p = p.as_ref();
        let key = volume_key(p, &mut folders);
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        fewest = fewest.min(workers_for_media(media_of_path(p)));
        if seen.len() >= 8 {
            break; // enough to tell; the rest share one of these as a rule
        }
    }
    if fewest == usize::MAX { 1 } else { fewest }
}

/// What tells volumes apart cheaply: the drive or share on Windows; elsewhere the device of the
/// path's folder (one look per folder: two mounts under one top folder, `/media/lenny/NVME` and
/// `/media/lenny/STICK`, are two volumes).
fn volume_key(p: &Path, folders: &mut HashMap<PathBuf, String>) -> String {
    let s = p.to_string_lossy();
    if cfg!(windows) {
        let (drive, _, _) = super::paths::nt_splitroot(&s);
        return drive.to_lowercase();
    }
    let folder = p.parent().unwrap_or(p).to_path_buf();
    folders
        .entry(folder)
        .or_insert_with_key(|f| {
            device_of(f).unwrap_or_else(|| s.split('/').nth(1).unwrap_or("").to_string())
        })
        .clone()
}

#[cfg(unix)]
fn device_of(folder: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    super::probe::call(super::probe::Call::Stat);
    std::fs::metadata(folder)
        .ok()
        .map(|m| format!("dev:{}", m.dev()))
}

#[cfg(not(unix))]
fn device_of(_folder: &Path) -> Option<String> {
    None
}

/// `f(i)` for every `i` below `n` on at most `workers` threads (the calling one among them), in no
/// particular order; returns when all ran (or `stop` said so: the rest are skipped). A thread
/// the system refuses to start leaves the work to those there are.
pub fn for_each(
    n: usize,
    workers: usize,
    stop: &(dyn Fn() -> bool + Sync),
    f: impl Fn(usize) + Sync,
) {
    let workers = workers.clamp(1, n.max(1));
    let next = AtomicUsize::new(0);
    // `stop` asked once before each `f` (and not after the last)
    let work = || {
        loop {
            let i = next.fetch_add(1, Ordering::Relaxed);
            if i >= n || stop() {
                return;
            }
            f(i);
        }
    };
    std::thread::scope(|s| {
        for k in 1..workers {
            let started = std::thread::Builder::new()
                .name(format!("spaceknife-ops-{k}"))
                .spawn_scoped(s, work); // (a copy: it holds references only)
            if started.is_err() {
                break;
            }
        }
        work();
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn media_decide_the_threads() {
        assert!(workers_for_media(Media::NvmeSsd) >= workers_for_media(Media::SataSsd));
        assert_eq!(workers_for_media(Media::Hdd), 2);
        assert_eq!(workers_for_media(Media::UsbFlash), 2);
        assert_eq!(workers_for_media(Media::SdCard), 2);
        assert_eq!(workers_for_media(Media::Unknown), 2);
    }

    /// Two mounts under one top folder are two volumes (`/dev` and the tmpfs at `/dev/shm`);
    /// paths in one folder are one.
    #[cfg(target_os = "linux")]
    #[test]
    fn volumes_are_told_apart_by_their_device() {
        use std::os::unix::fs::MetadataExt;
        let (Ok(dev), Ok(shm)) = (std::fs::metadata("/dev"), std::fs::metadata("/dev/shm")) else {
            return;
        };
        if dev.dev() == shm.dev() {
            return;
        }
        let mut folders = HashMap::new();
        let a = volume_key(Path::new("/dev/null"), &mut folders);
        let b = volume_key(Path::new("/dev/shm/x"), &mut folders);
        let c = volume_key(Path::new("/dev/zero"), &mut folders);
        assert_ne!(a, b);
        assert_eq!(a, c);
        assert_eq!(folders.len(), 2); // one look per folder
    }

    #[test]
    fn every_index_once_and_stop_skips_the_rest() {
        let seen = Mutex::new(Vec::new());
        for_each(100, 4, &|| false, |i| seen.lock().unwrap().push(i));
        let mut got = seen.into_inner().unwrap();
        got.sort();
        assert_eq!(got, (0..100).collect::<Vec<_>>());
        let ran = AtomicUsize::new(0);
        for_each(100, 3, &|| ran.load(Ordering::SeqCst) >= 10, |_| {
            ran.fetch_add(1, Ordering::SeqCst);
        });
        // a thread may have started one more before it looked
        assert!(ran.load(Ordering::SeqCst) <= 13, "{ran:?}");
    }
}
