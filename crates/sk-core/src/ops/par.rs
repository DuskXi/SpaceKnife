//! Bounded concurrency for file operations (docs/manage.md §2.5, 并发).
//!
//! How many calls an operation keeps in flight on one device follows its media
//! ([`crate::media`], the probe the scanner uses too): NVMe takes many requests at once, a SATA SSD
//! or a network share a few, a spinning disk, a USB stick or an SD card serves them one after the
//! other — more threads there only make it seek. Independent steps of an operation (one item's
//! checks, one item, one folder of a tree being deleted) run on that many threads; what depends on
//! something else waits for it (a folder is deleted after everything in it).

use std::path::Path;
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
/// volume, by the paths' first component and the drive or mount they name).
pub fn workers_for_all<P: AsRef<Path>>(paths: &[P]) -> usize {
    if let n @ 1.. = seams::WORKERS.load(Ordering::Relaxed) {
        return n;
    }
    let mut seen: Vec<String> = Vec::new();
    let mut fewest = usize::MAX;
    for p in paths {
        let p = p.as_ref();
        let key = volume_key(p);
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

/// What tells volumes apart cheaply: the drive or share on Windows, the top folder elsewhere.
fn volume_key(p: &Path) -> String {
    let s = p.to_string_lossy();
    if cfg!(windows) {
        let (drive, _, _) = super::paths::nt_splitroot(&s);
        drive.to_lowercase()
    } else {
        s.split('/').nth(1).unwrap_or("").to_string()
    }
}

/// `f(i)` for every `i` below `n` on at most `workers` threads, in no particular order; returns
/// when all ran (or `stop` said so: the rest are skipped).
pub fn for_each(
    n: usize,
    workers: usize,
    stop: &(dyn Fn() -> bool + Sync),
    f: impl Fn(usize) + Sync,
) {
    let workers = workers.clamp(1, n.max(1));
    if workers == 1 {
        for i in 0..n {
            if stop() {
                return;
            }
            f(i);
        }
        return;
    }
    let next = AtomicUsize::new(0);
    std::thread::scope(|s| {
        for k in 0..workers {
            let (next, f) = (&next, &f);
            std::thread::Builder::new()
                .name(format!("spaceknife-ops-{k}"))
                .spawn_scoped(s, move || {
                    loop {
                        if stop() {
                            return;
                        }
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        if i >= n {
                            return;
                        }
                        f(i);
                    }
                })
                .expect("a thread for a file operation");
        }
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
