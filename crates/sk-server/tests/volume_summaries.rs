//! Port of tests/test_volume_summaries.py: what the 「本机磁盘」 page keeps of each volume's last
//! complete scan.

mod common;

use chrono::{DateTime, Duration, TimeZone, Utc};
use sk_core::volume::VolumeInfo;
use sk_protocol::machine::FolderStat;
use sk_protocol::scan::{ClassStat, TreeMethod};
use sk_server::volume_summaries::{KEEP, Kept, SummaryStore, forget, identity};

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 21, 12, 0, 0).unwrap()
}

fn volume(mount: &str, serial: &str, total: i64) -> VolumeInfo {
    VolumeInfo {
        mount_point: mount.into(),
        device: mount.into(),
        fs_type: "NTFS".into(),
        label: "SSDData".into(),
        total,
        free: total / 2,
        cluster_size: 4096,
        serial: serial.into(),
        ..VolumeInfo::default()
    }
}

fn d() -> VolumeInfo {
    volume("D:\\", "7A3C-11F0", 800_000_000_000)
}

fn kept(v: &VolumeInfo, at: DateTime<Utc>) -> Kept {
    Kept {
        mount: v.mount_point.clone(),
        identity: identity(v),
        at,
        seen: at,
        files: 42_533,
        dirs: Some(3_100),
        bytes_alloc: 280_000_000_000,
        method: Some(TreeMethod::Mft),
        snapshot: None,
        classes: vec![
            ClassStat { class_id: "game".into(), bytes: 211_000_000_000, files: 9_000, parts: vec![] },
            ClassStat { class_id: "archive".into(), bytes: 62_000_000_000, files: 400, parts: vec![] },
        ],
        folders: vec![FolderStat { name: "Koikatsu Party".into(), bytes: 211_000_000_000 }],
    }
}

#[test]
fn kept_across_a_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("data").join("volume_summaries.json");
    let store = SummaryStore::open(Some(file.clone()));
    assert!(store.get(&d()).is_none());
    store.put([kept(&d(), now())]);
    let again = SummaryStore::open(Some(file.clone())); // a new backend reads the same file
    let k = again.get(&d()).unwrap();
    assert_eq!(k, kept(&d(), now()));
    assert_eq!(k.classes.iter().map(|c| c.class_id.as_str()).collect::<Vec<_>>(), ["game", "archive"]);
    assert_eq!(k.folders[0].name, "Koikatsu Party");
    assert!(!file.with_extension("tmp").exists());
}

#[test]
fn only_the_same_volume() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SummaryStore::open(Some(tmp.path().join("s.json")));
    store.put([kept(&d(), now())]);
    assert!(store.get(&volume("D:\\", "0000-0001", 800_000_000_000)).is_none()); // another drive on D:
    assert!(store.get(&volume("E:\\", "7A3C-11F0", 800_000_000_000)).is_none());
    let no_serial = volume("D:\\", "", 800_000_000_000);
    assert_eq!(identity(&no_serial), "ntfs:800000000000");
    store.put([kept(&no_serial, now())]);
    assert!(store.get(&no_serial).is_some() && store.get(&volume("D:\\", "", 1)).is_none());
    assert_eq!(store.len(), 2); // both drives that were on D: keep their entries
}

#[test]
fn a_newer_scan_replaces_an_older_one() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SummaryStore::open(Some(tmp.path().join("s.json")));
    store.put([Kept { files: 1, ..kept(&d(), now()) }]);
    store.put([Kept { files: 2, ..kept(&d(), now() - Duration::hours(1)) }]); // older: ignored
    assert_eq!(store.get(&d()).unwrap().files, 1);
    store.put([Kept { files: 3, ..kept(&d(), now() + Duration::hours(1)) }]);
    assert_eq!(store.get(&d()).unwrap().files, 3);
}

#[test]
fn no_write_when_nothing_changed() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("s.json");
    let store = SummaryStore::open(Some(file.clone()));
    store.put([kept(&d(), now())]);
    let stamp = std::fs::metadata(&file).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    store.put([Kept { seen: now() + Duration::minutes(5), ..kept(&d(), now()) }]); // the same scan read again
    store.seen(&[d()], Some(now() + Duration::hours(1))); // seen within the day
    assert_eq!(std::fs::metadata(&file).unwrap().modified().unwrap(), stamp);
}

#[test]
fn bounded_and_forgotten() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SummaryStore::open(Some(tmp.path().join("s.json")));
    let recent = Utc::now();
    let drives: Vec<VolumeInfo> =
        (0..KEEP + 5).map(|i| volume(&format!("/media/drive{i}"), &format!("S{i}"), 1 << 40)).collect();
    store.put(drives.iter().enumerate().map(|(i, v)| Kept { seen: recent - Duration::minutes(i as i64), ..kept(v, recent) }));
    assert_eq!(store.len(), KEEP);
    assert!(store.get(&drives[0]).is_some() && store.get(drives.last().unwrap()).is_none()); // the least recently seen go
    // an unplugged drive keeps its entry for a while, then goes; a present one stays
    let later = recent + forget() + Duration::days(1);
    store.seen(&drives[..1], Some(recent + forget() - Duration::days(1)));
    store.seen(&[], Some(later));
    assert_eq!(store.len(), 1);
    assert!(store.get(&drives[0]).is_some());
}

#[test]
fn unreadable_file_starts_over() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("s.json");
    std::fs::write(&file, "{not json").unwrap();
    let store = SummaryStore::open(Some(file.clone()));
    assert_eq!(store.len(), 0);
    store.put([kept(&d(), now())]);
    assert!(SummaryStore::open(Some(file)).get(&d()).is_some());
}

#[test]
fn what_the_page_shows_of_it() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = tmp.path().join("D-0198a2ef3ad2").join("2026-10-21.sks");
    std::fs::create_dir_all(snap.parent().unwrap()).unwrap();
    std::fs::write(&snap, b"x").unwrap();
    let k = Kept { snapshot: Some(snap.display().to_string()), ..kept(&d(), now()) };
    let scan = k.volume_scan(1);
    assert_eq!(scan.source.as_str(), "kept");
    assert!(scan.scan_id.is_none() && !scan.running);
    assert_eq!(scan.files, 42_533);
    assert_eq!(scan.snapshot.as_deref(), Some(snap.display().to_string().as_str()));
    assert_eq!(scan.classes.iter().map(|c| c.class_id.as_str()).collect::<Vec<_>>(), ["game"]);
    assert_eq!(scan.folders, k.folders);
    std::fs::remove_file(&snap).unwrap();
    assert!(k.volume_scan(5).snapshot.is_none()); // the snapshot is gone: nothing to open
}
