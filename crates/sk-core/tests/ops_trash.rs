//! ops::trash (port of tests/test_ops_trash.py): recycle bin / freedesktop trash round trips,
//! refusals, permanent deletion.
//!
//! Only files these tests create in temporary folders are touched. Everything a test moves to the
//! real recycle bin is restored by the test itself (and, should an assertion fail first, by the
//! `Trashed` guard), so the bin is left as it was. On Linux the home trash is pointed into a
//! temporary folder through `XDG_DATA_HOME`.

use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use parking_lot::{Mutex, MutexGuard};
use sk_core::ops::trash::{
    self, OpCode, OpOptions, OpResult, delete_permanently, move_to_trash, parse_recycle_info,
    parse_trashinfo, rename_noreplace, restore, trash_available, trash_status,
};

const WIN: bool = cfg!(windows);
const MAC: bool = cfg!(target_os = "macos");

mod exit_cleanup;
mod fs_caps;

/// One SpaceKnife home (and on Linux, one XDG data home) for this binary, removed when it exits;
/// tests that change the process-wide seams run one at a time.
fn setup() -> (&'static Path, MutexGuard<'static, ()>) {
    static BASE: OnceLock<PathBuf> = OnceLock::new();
    static LOCK: Mutex<()> = Mutex::new(());
    let base = BASE.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap().keep();
        exit_cleanup::remove_at_exit(dir.clone());
        // SAFETY: set once, before the tests of this binary read the environment
        unsafe {
            std::env::set_var("SPACEKNIFE_HOME", dir.join("skhome"));
            if !WIN {
                std::env::set_var("XDG_DATA_HOME", dir.join("xdg"));
            }
        }
        dir
    });
    (base, LOCK.lock())
}

/// Refs a test put into the real trash; whatever is still there at the end goes back.
struct Trashed(Vec<String>);

impl Drop for Trashed {
    fn drop(&mut self) {
        let left: Vec<String> = self.0.iter().filter(|r| !r.is_empty()).cloned().collect();
        if !left.is_empty() {
            restore(&left);
        }
    }
}

fn held_open(path: &Path) -> File {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0x1)
            .open(path)
            .unwrap()
    }
    #[cfg(not(windows))]
    File::open(path).unwrap()
}

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn text(key: &str) -> String {
    // the source texts (the tests run in the source language)
    match key {
        "trashed" => "已移到回收站",
        "restored" => "已还原",
        "deleted" => "已永久删除",
        "exists" => "原位置已有同名项目，未覆盖",
        "bad_ref" => "不是可还原的回收站条目",
        "no_bin_nuke" => "这个卷的回收站设置为「不移到回收站，直接删除」，只能永久删除",
        "too_long" => "路径过长，回收站放不下，只能永久删除",
        "refused" => "系统准备直接删除而不是移到回收站，已阻止，未做任何改动",
        "cancelled_partly" => "已取消，其中一部分已删除",
        "no_trash_dir" => "这个卷上没有可用的回收站目录，只能永久删除",
        _ => unreachable!(),
    }
    .to_string()
}

fn make(dir: &Path) -> (PathBuf, PathBuf) {
    let data = dir.join("data");
    std::fs::create_dir_all(data.join("folder a").join("sub")).unwrap();
    let f = data.join("报告 final.txt");
    std::fs::write(&f, b"hello").unwrap();
    std::fs::write(
        data.join("folder a").join("sub").join("x.bin"),
        b"x".repeat(10),
    )
    .unwrap();
    (f, data.join("folder a"))
}

/// `p` as the shell records it in a `$I` file: 8.3 short names (CI's `RUNNER~1`) expanded.
fn shell_name(p: &Path) -> String {
    #[cfg(windows)]
    if let Some(long) = sk_core::ops::win::long_names(&s(p)) {
        return long;
    }
    s(p)
}

fn same(a: &str, b: &str) -> bool {
    let key = |p: &str| {
        if WIN {
            sk_core::ops::paths::nt_normcase(&sk_core::ops::paths::nt_normpath(p))
        } else {
            sk_core::ops::paths::posix_normpath(p)
        }
    };
    key(a) == key(b)
}

fn codes(res: &[OpResult]) -> Vec<OpCode> {
    res.iter().map(|r| r.code).collect()
}

fn trash_all(paths: &[String]) -> Vec<OpResult> {
    move_to_trash(paths, OpOptions::default())
}

/// One thread for every operation while it lives (what a test of the order of things
/// counts on), as many as the device takes again when dropped.
struct Threads;

impl Threads {
    fn set(n: usize) -> Threads {
        sk_core::ops::par::seams::WORKERS.store(n, Ordering::SeqCst);
        Threads
    }
}

impl Drop for Threads {
    fn drop(&mut self) {
        sk_core::ops::par::seams::WORKERS.store(0, Ordering::SeqCst);
    }
}

/// Whether the volume `dir` is on has no trash (a network share, a removable disk, a volume set
/// to delete at once …): then moving something there must be refused with the reason the user is
/// shown, which offers a permanent deletion, and nothing may change; the test that needs a trash
/// is skipped (said so).
fn without_trash(dir: &Path, what: &str) -> bool {
    let Some(reason) = fs_caps::no_trash(dir) else {
        return false;
    };
    let probe = dir.join("no-trash-probe.txt");
    std::fs::write(&probe, b"p").unwrap();
    let r = trash_all(&[s(&probe)]).remove(0);
    assert_eq!(
        (r.code, r.message.as_str()),
        (OpCode::NoTrash, reason.as_str()),
        "{r:?}"
    );
    assert!(r.message.contains("永久删除"), "{r:?}");
    assert_eq!(std::fs::read(&probe).unwrap(), b"p");
    std::fs::remove_file(&probe).unwrap();
    fs_caps::skip(what, dir, &format!("no trash here: {reason}"));
    true
}

// --------------------------------------------------------------------------- round trips

#[test]
fn trash_and_restore() {
    if MAC {
        return; // macOS trash: implemented best effort, not tested here
    }
    let (base, _g) = setup();
    let dir = tempfile::tempdir().unwrap();
    if without_trash(dir.path(), "trash_and_restore") {
        return;
    }
    let (f, d) = make(dir.path());
    let recorded = [shell_name(&f), shell_name(&d)]; // before they leave
    let mut seen: Vec<(usize, usize, OpCode)> = Vec::new();
    let mut progress = |done: usize, total: usize, r: &OpResult| seen.push((done, total, r.code));
    let res = move_to_trash(
        &[s(&f), s(&d)],
        OpOptions {
            progress: Some(&mut progress),
            ..OpOptions::default()
        },
    );
    let mut trashed = Trashed(res.iter().map(|r| r.trash_ref.clone()).collect());
    let got: Vec<(OpCode, bool, String)> = res
        .iter()
        .map(|r| (r.code, r.ok, r.message.clone()))
        .collect();
    assert_eq!(got, vec![(OpCode::Ok, true, text("trashed")); 2]);
    assert_eq!(
        res.iter().map(|r| r.path.clone()).collect::<Vec<_>>(),
        [s(&f), s(&d)]
    );
    assert!(!f.exists() && !d.exists());
    assert_eq!(seen, [(1, 2, OpCode::Ok), (2, 2, OpCode::Ok)]);
    if WIN {
        for (r, (original, size)) in res.iter().zip([(&recorded[0], 5), (&recorded[1], 10)]) {
            let info = Path::new(&r.trash_ref);
            assert!(
                info.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("$I")
            );
            // `$Recycle.Bin\<SID>`, or `$Recycle.Bin` itself on FAT and exFAT (no folder per user)
            let name_of = |p: &Path| p.file_name().map(|n| n.to_string_lossy().to_lowercase());
            let folder = info.parent().unwrap();
            let shared = name_of(folder).as_deref() == Some("$recycle.bin");
            let bin = if shared {
                folder
            } else {
                folder.parent().unwrap()
            };
            assert_eq!(name_of(bin).as_deref(), Some("$recycle.bin"), "{info:?}");
            #[cfg(windows)] // (`if WIN` is decided at run time: the rest compiles everywhere)
            {
                let root = sk_core::ops::win::volume_root(&s(&f)).unwrap();
                assert_eq!(shared, !sk_core::ops::win::per_user_bin(&root));
            }
            let (got_size, when, path) = parse_recycle_info(&std::fs::read(info).unwrap()).unwrap();
            let short = if size == 5 { s(&f) } else { s(&d) };
            assert!(
                (same(&path, original) || same(&path, &short)) && got_size == size && when > 1.7e9,
                "{path} {got_size} {when}"
            );
        }
    } else {
        let info = PathBuf::from(&res[0].trash_ref);
        assert_eq!(
            info.parent().unwrap(),
            base.join("xdg").join("Trash").join("info")
        );
        let body = std::fs::read_to_string(&info).unwrap();
        assert!(body.starts_with("[Trash Info]\n"));
        let quoted = sk_core::ops::paths::quote_path(s(&f).as_bytes());
        assert!(body.contains(&format!("Path={quoted}\n")));
        assert!(body.lines().any(|l| l.starts_with("DeletionDate=")
            && l.len() == "DeletionDate=2026-01-01T00:00:00".len()));
        let landed = base
            .join("xdg")
            .join("Trash")
            .join("files")
            .join(f.file_name().unwrap());
        assert_eq!(std::fs::read(landed).unwrap(), b"hello");
    }
    let back = restore(&res.iter().map(|r| r.trash_ref.clone()).collect::<Vec<_>>());
    let got: Vec<(OpCode, String)> = back.iter().map(|b| (b.code, b.message.clone())).collect();
    assert_eq!(got, vec![(OpCode::Ok, text("restored")); 2]);
    trashed.0.clear();
    // back where they were, under either spelling (the $I file may hold the long one)
    let home = |got: &str, p: &Path, long: &str| same(got, &s(p)) || same(got, long);
    assert!(
        home(&back[0].path, &f, &recorded[0]) && home(&back[1].path, &d, &recorded[1]),
        "{back:?}"
    );
    assert_eq!(std::fs::read(&f).unwrap(), b"hello");
    assert_eq!(
        std::fs::read(d.join("sub").join("x.bin")).unwrap(),
        b"x".repeat(10)
    );
}

#[test]
fn restore_never_overwrites() {
    if MAC {
        return;
    }
    let (_, _g) = setup();
    let dir = tempfile::tempdir().unwrap();
    if without_trash(dir.path(), "restore_never_overwrites") {
        return;
    }
    let f = dir.path().join("gone").join("a.txt");
    std::fs::create_dir(f.parent().unwrap()).unwrap();
    std::fs::write(&f, b"old").unwrap();
    let r = trash_all(&[s(&f)]).remove(0);
    let mut trashed = Trashed(vec![r.trash_ref.clone()]);
    std::fs::write(&f, b"new").unwrap(); // something new took the name
    let back = restore(std::slice::from_ref(&r.trash_ref)).remove(0);
    assert_eq!((back.code, back.message), (OpCode::Exists, text("exists")));
    assert_eq!(std::fs::read(&f).unwrap(), b"new");
    std::fs::remove_file(&f).unwrap();
    std::fs::remove_dir(f.parent().unwrap()).unwrap(); // the folder is gone too: made again
    let back = restore(std::slice::from_ref(&r.trash_ref)).remove(0);
    assert_eq!(back.code, OpCode::Ok);
    trashed.0.clear();
    assert_eq!(std::fs::read(&f).unwrap(), b"old");
    let again = restore(std::slice::from_ref(&r.trash_ref)).remove(0);
    assert_eq!(again.code, OpCode::Missing);
}

#[test]
fn prechecks_skip_items() {
    if MAC {
        return;
    }
    let (base, _g) = setup();
    let own = base.join("skhome").join("data").join("x.sks");
    std::fs::create_dir_all(own.parent().unwrap()).unwrap();
    std::fs::write(&own, b"x").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let held = dir.path().join("held.txt");
    std::fs::write(&held, b"x").unwrap();
    let paths = [s(&dir.path().join("nope")), s(&own), s(&held)];
    // (Windows looks at holders after the bin: without one, that is what the item is told)
    let bin_first = WIN && fs_caps::no_trash(dir.path()).is_some();
    let handle = held_open(&held);
    let res = trash_all(&paths);
    let _trashed = Trashed(res.iter().map(|r| r.trash_ref.clone()).collect());
    let held_code = if bin_first {
        OpCode::NoTrash
    } else {
        OpCode::Locked
    };
    assert_eq!(
        codes(&res),
        [OpCode::Missing, OpCode::Protected, held_code],
        "{res:?}"
    );
    assert!(bin_first || res[2].message.contains("使用"), "{res:?}");
    let res = delete_permanently(&paths, OpOptions::default());
    assert_eq!(
        codes(&res),
        [OpCode::Missing, OpCode::Protected, OpCode::Locked]
    );
    drop(handle);
    assert!(own.exists() && held.exists());
}

#[test]
fn cancel_midway() {
    if MAC {
        return;
    }
    let (_, _g) = setup();
    let _one = Threads::set(1); // one item after the other: the first is done when it stops
    let dir = tempfile::tempdir().unwrap();
    if without_trash(dir.path(), "cancel_midway") {
        return;
    }
    let files: Vec<String> = (0..3)
        .map(|i| {
            let p = dir.path().join(format!("c{i}.txt"));
            std::fs::write(&p, b"c").unwrap();
            s(&p)
        })
        .collect();
    let seen = AtomicUsize::new(0);
    let mut progress = |_: usize, _: usize, _: &OpResult| {
        seen.fetch_add(1, Ordering::SeqCst);
    };
    let cancel = || seen.load(Ordering::SeqCst) > 0;
    let res = move_to_trash(
        &files,
        OpOptions {
            progress: Some(&mut progress),
            cancel: Some(&cancel),
            ..OpOptions::default()
        },
    );
    let mut trashed = Trashed(res.iter().map(|r| r.trash_ref.clone()).collect());
    assert_eq!(
        codes(&res),
        [OpCode::Ok, OpCode::Cancelled, OpCode::Cancelled]
    );
    let exists: Vec<bool> = files.iter().map(|p| Path::new(p).exists()).collect();
    assert_eq!(exists, [false, true, true]);
    assert_eq!(seen.load(Ordering::SeqCst), 3); // every item gets its final result
    assert_eq!(
        codes(&restore(std::slice::from_ref(&res[0].trash_ref))),
        [OpCode::Ok]
    );
    trashed.0.clear();
}

#[test]
fn trash_status_of_a_folder() {
    if MAC {
        return;
    }
    let (base, _g) = setup();
    let dir = tempfile::tempdir().unwrap();
    let status = trash_status(&s(dir.path()), true);
    if let Some(reason) = fs_caps::no_trash(dir.path()) {
        assert!(!status.available && status.reason == reason, "{status:?}");
        assert!(!reason.is_empty() && !trash_available(&s(dir.path())));
        fs_caps::skip("trash_status_of_a_folder", dir.path(), &reason);
        return;
    }
    assert!(
        status.available && trash_available(&s(dir.path())),
        "{status:?}"
    );
    if WIN {
        assert!(
            status.location.contains("$Recycle.Bin") && status.used >= 0 && status.capacity > 0
        );
    } else {
        assert_eq!(status.location, s(&base.join("xdg").join("Trash")));
    }
}

#[test]
fn bad_refs_are_refused() {
    let (_, _g) = setup();
    let dir = tempfile::tempdir().unwrap();
    let fake = dir.path().join("info").join("x.trashinfo");
    std::fs::create_dir(fake.parent().unwrap()).unwrap();
    std::fs::write(&fake, "[Trash Info]\nPath=/etc/passwd\n").unwrap();
    let res = restore(&[
        s(&fake),
        s(&dir.path().join("$Iabc.txt")),
        "not even a path".into(),
    ]);
    assert_eq!(codes(&res), [OpCode::Error; 3]);
    assert!(res.iter().all(|r| r.message == text("bad_ref")));
}

// --------------------------------------------------------------------------- Windows refusals

#[cfg(windows)]
mod windows {
    use super::*;

    struct Seam;
    impl Drop for Seam {
        fn drop(&mut self) {
            *trash::seams::BIN_SETTINGS.lock() = None;
            trash::seams::MAX_PATH.store(trash::WIN_MAX_PATH, Ordering::Relaxed);
        }
    }

    #[test]
    fn no_trash_on_a_volume_that_destroys() {
        let (_, _g) = setup();
        let _seam = Seam;
        let dir = tempfile::tempdir().unwrap();
        if without_trash(dir.path(), "no_trash_on_a_volume_that_destroys") {
            return;
        }
        let (f, d) = make(dir.path());
        *trash::seams::BIN_SETTINGS.lock() = Some(|_, _| (true, 1 << 40));
        let res = trash_all(&[s(&f), s(&d)]);
        let got: Vec<(OpCode, String)> = res.iter().map(|r| (r.code, r.message.clone())).collect();
        assert_eq!(got, vec![(OpCode::NoTrash, text("no_bin_nuke")); 2]);
        assert!(f.exists() && d.join("sub").join("x.bin").exists());
        assert!(!trash_available(&s(&f)));
        assert_eq!(trash_status(&s(&f), true).reason, text("no_bin_nuke"));
    }

    #[test]
    fn no_trash_when_too_big_for_the_bin() {
        let (_, _g) = setup();
        let _seam = Seam;
        let dir = tempfile::tempdir().unwrap();
        if without_trash(dir.path(), "no_trash_when_too_big_for_the_bin") {
            return;
        }
        let big = dir.path().join("big.bin");
        std::fs::write(&big, b"b".repeat(2000)).unwrap();
        let folder = dir.path().join("many");
        std::fs::create_dir(&folder).unwrap();
        for i in 0..3 {
            std::fs::write(folder.join(format!("{i}.bin")), b"m".repeat(500)).unwrap();
        }
        *trash::seams::BIN_SETTINGS.lock() = Some(|_, _| (false, 1000));
        let sizes = HashMap::from([(s(&folder), 900i64)]); // a hint near the limit: measured
        let res = move_to_trash(
            &[s(&big), s(&folder)],
            OpOptions {
                sizes: Some(&sizes),
                ..OpOptions::default()
            },
        );
        assert_eq!(codes(&res), [OpCode::NoTrash, OpCode::NoTrash]);
        assert!(
            res.iter().all(|r| r.message.starts_with("超出回收站容量")),
            "{res:?}"
        );
        assert_eq!(std::fs::metadata(&big).unwrap().len(), 2000);
        assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 3);
    }

    /// `p` as a path past MAX_PATH must be given (`\\?\`, `\\?\UNC\`).
    fn lp(p: &Path) -> PathBuf {
        PathBuf::from(fs_caps::verbatim(p))
    }

    fn deep_tree(top: &Path) -> PathBuf {
        std::fs::create_dir(top).unwrap();
        let mut p = top.to_path_buf();
        while s(&p).len() < 300 {
            p = p.join("n".repeat(40));
            std::fs::create_dir(lp(&p)).unwrap();
        }
        let leaf = p.join("leaf.txt");
        std::fs::write(lp(&leaf), b"x").unwrap();
        leaf
    }

    #[test]
    fn no_trash_when_paths_get_too_long() {
        let (_, _g) = setup();
        let dir = tempfile::tempdir().unwrap();
        if without_trash(dir.path(), "no_trash_when_paths_get_too_long") {
            return;
        }
        let top = dir.path().join("deep");
        let leaf = deep_tree(&top);
        let r = trash_all(&[s(&top)]).remove(0);
        assert_eq!((r.code, r.message), (OpCode::NoTrash, text("too_long")));
        assert!(lp(&leaf).exists());
        std::fs::remove_dir_all(lp(&top)).unwrap();
    }

    /// A folder whose scanned size is far below the bin's capacity is not measured again, but its
    /// paths still have to fit the bin: without the longest path inside as scanned, it is walked
    /// (the shortcut used to skip that check too, leaving it to the engine guard); with it, that
    /// decides without a walk.
    #[test]
    fn a_trusted_size_still_checks_path_lengths() {
        let (_, _g) = setup();
        let dir = tempfile::tempdir().unwrap();
        if without_trash(dir.path(), "a_trusted_size_still_checks_path_lengths") {
            return;
        }
        let top = dir.path().join("deep");
        let leaf = deep_tree(&top);
        let sizes = HashMap::from([(s(&top), 1i64)]); // far below a quarter of any bin
        let r = move_to_trash(
            &[s(&top)],
            OpOptions {
                sizes: Some(&sizes),
                ..OpOptions::default()
            },
        )
        .remove(0);
        let _trashed = Trashed(vec![r.trash_ref.clone()]);
        assert_eq!(
            (r.code, r.message.clone()),
            (OpCode::NoTrash, text("too_long")),
            "{r:?}"
        );
        assert!(lp(&leaf).exists());
        std::fs::remove_dir_all(lp(&top)).unwrap();
        // the scan's longest path decides without a walk: here it says too long
        let plain = dir.path().join("plain");
        std::fs::create_dir(&plain).unwrap();
        std::fs::write(plain.join("f.txt"), b"f").unwrap();
        let sizes = HashMap::from([(s(&plain), 1i64)]);
        let longest = HashMap::from([(s(&plain), 400usize)]);
        let r = move_to_trash(
            &[s(&plain)],
            OpOptions {
                sizes: Some(&sizes),
                longest: Some(&longest),
                ..OpOptions::default()
            },
        )
        .remove(0);
        let _trashed = Trashed(vec![r.trash_ref.clone()]);
        assert_eq!(
            (r.code, r.message.clone()),
            (OpCode::NoTrash, text("too_long")),
            "{r:?}"
        );
        assert!(plain.join("f.txt").exists());
    }

    /// With the checks out of the way, the shell itself may want to destroy a tree that cannot
    /// fit the bin (paths too long): the sink refuses, nothing is lost, the next item still goes.
    /// (Newer Windows builds recycle such trees fine: then it must come back whole.)
    #[test]
    fn engine_guard_refuses_destruction() {
        let (_, _g) = setup();
        let _seam = Seam;
        let dir = tempfile::tempdir().unwrap();
        if without_trash(dir.path(), "engine_guard_refuses_destruction") {
            return;
        }
        let top = dir.path().join("deep");
        let leaf = deep_tree(&top);
        let other = dir.path().join("other.txt");
        std::fs::write(&other, b"o").unwrap();
        trash::seams::MAX_PATH.store(100_000, Ordering::Relaxed); // only the guard is left
        let res = trash_all(&[s(&top), s(&other)]);
        let mut trashed = Trashed(res.iter().map(|r| r.trash_ref.clone()).collect());
        assert_eq!(
            (res[1].code, res[1].message.clone()),
            (OpCode::Ok, text("trashed"))
        );
        match res[0].code {
            OpCode::NoTrash => assert_eq!(res[0].message, text("refused")),
            OpCode::Ok => {
                assert_eq!(
                    codes(&restore(std::slice::from_ref(&res[0].trash_ref))),
                    [OpCode::Ok]
                );
            }
            other => panic!("{other:?}: {res:?}"),
        }
        assert!(lp(&leaf).exists());
        assert_eq!(
            codes(&restore(std::slice::from_ref(&res[1].trash_ref))),
            [OpCode::Ok]
        );
        trashed.0.clear();
        assert_eq!(std::fs::read(&other).unwrap(), b"o");
        std::fs::remove_dir_all(lp(&top)).unwrap();
    }

    /// What a rename into the bin would move although something uses it: a file another program
    /// holds open while sharing its deletion (NTFS renames it), a folder with a running program
    /// in it (its image is mapped, no handle is open: the folder renames). Both are reported
    /// held, by the program's name, and stay where they are, as before the bin was written to
    /// directly (the Restart Manager asked about every item then).
    #[test]
    fn items_in_use_that_a_rename_would_move_stay() {
        use std::os::windows::fs::OpenOptionsExt;
        let (_, _g) = setup();
        let dir = tempfile::tempdir().unwrap();
        if without_trash(dir.path(), "items_in_use_that_a_rename_would_move_stay") {
            return;
        }
        let shared = dir.path().join("log.txt");
        std::fs::write(&shared, b"l").unwrap();
        let handle = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0x7) // read, write, delete
            .open(&shared)
            .unwrap();
        let app = dir.path().join("app");
        std::fs::create_dir(&app).unwrap();
        let exe = app.join("waiter.exe");
        let system = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        std::fs::copy(Path::new(&system).join("System32").join("PING.EXE"), &exe).unwrap();
        std::fs::write(app.join("readme.txt"), b"r").unwrap();
        let mut running = std::process::Command::new(&exe)
            .args(["-n", "60", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        let res = trash_all(&[s(&shared), s(&app)]);
        let trashed = Trashed(res.iter().map(|r| r.trash_ref.clone()).collect());
        let _ = running.kill();
        let _ = running.wait();
        drop(handle);
        drop(trashed);
        assert_eq!(codes(&res), [OpCode::Locked, OpCode::Locked], "{res:?}");
        assert!(res.iter().all(|r| r.message.contains("使用")), "{res:?}");
        assert!(shared.exists() && exe.exists() && app.join("readme.txt").exists());
    }

    /// A link goes into the bin of the volume it is on, whatever it points to, and never makes its
    /// folder's other items look as if they were on its target's volume: a junction to another
    /// volume (dangling here: nothing there is touched) first, then a file beside it, both go
    /// into this volume's bin and come back.
    #[test]
    fn a_link_to_another_volume_does_not_move_its_neighbours_there() {
        use sk_core::ops::win;
        let (_, _g) = setup();
        let _one = Threads::set(1); // the link is checked first
        let dir = tempfile::tempdir().unwrap();
        if without_trash(
            dir.path(),
            "a_link_to_another_volume_does_not_move_its_neighbours_there",
        ) {
            return;
        }
        if !fs_caps::links(dir.path()) {
            fs_caps::skip("a_link_to_another_volume", dir.path(), "no junctions");
            return;
        }
        let here = win::volume_root(&s(dir.path())).unwrap();
        if win::own_bin(&here).is_none() {
            // (a fresh account: the shell makes the bin folder first; a dangling link is its call)
            eprintln!("skipped: this volume's bin is the shell's to write to");
            return;
        }
        let Some(other) = ('C'..='Z')
            .map(|c| format!("{c}:\\"))
            .find(|r| !r.eq_ignore_ascii_case(&here) && win::drive_type(r) == win::DRIVE_FIXED)
        else {
            eprintln!("skipped: no other fixed volume");
            return;
        };
        let link = dir.path().join("elsewhere");
        let ok = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(format!("{other}sk-no-such-folder-{}", std::process::id()))
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "mklink /J");
        let file = dir.path().join("beside.txt");
        std::fs::write(&file, b"b").unwrap();
        let res = trash_all(&[s(&link), s(&file)]);
        let mut trashed = Trashed(res.iter().map(|r| r.trash_ref.clone()).collect());
        assert_eq!(codes(&res), [OpCode::Ok, OpCode::Ok], "{res:?}");
        for r in &res {
            assert!(
                r.trash_ref.to_lowercase().starts_with(&here.to_lowercase()),
                "{r:?}"
            );
        }
        assert_eq!(codes(&restore(&trashed.0)), [OpCode::Ok, OpCode::Ok]);
        trashed.0.clear();
        assert!(std::fs::symlink_metadata(&link).is_ok() && file.exists());
        let _ = std::fs::remove_dir(&link); // the junction only
    }

    /// Moved into the bin by a rename of its own (the user's own bin folder, room in it): the
    /// `$I` record is what Windows 10 writes — version 2, the size the scan gave, the original
    /// path — beside its `$R`, and a restore puts it back.
    #[test]
    fn a_folder_goes_into_the_bin_by_a_rename() {
        let (_, _g) = setup();
        let dir = tempfile::tempdir().unwrap();
        if without_trash(dir.path(), "a_folder_goes_into_the_bin_by_a_rename") {
            return;
        }
        let folder = dir.path().join("photos.2024");
        std::fs::create_dir_all(folder.join("sub")).unwrap();
        std::fs::write(folder.join("sub").join("a.jpg"), b"jpeg").unwrap();
        let recorded = shell_name(&folder);
        let key = s(&folder);
        let sizes = HashMap::from([(key.clone(), 4096i64)]);
        let longest = HashMap::from([(key.clone(), 10usize)]);
        let bytes = HashMap::from([(key.clone(), 4i64)]);
        let r = move_to_trash(
            &[s(&folder)],
            OpOptions {
                sizes: Some(&sizes),
                longest: Some(&longest),
                bytes: Some(&bytes),
                ..OpOptions::default()
            },
        )
        .remove(0);
        let mut trashed = Trashed(vec![r.trash_ref.clone()]);
        assert_eq!((r.code, r.message.clone()), (OpCode::Ok, text("trashed")));
        assert!(!folder.exists());
        let info = PathBuf::from(&r.trash_ref);
        let name = info.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("$I") && name.ends_with(".2024"), "{name}");
        let data = std::fs::read(&info).unwrap();
        assert_eq!(i64::from_le_bytes(data[..8].try_into().unwrap()), 2);
        let (size, when, path) = parse_recycle_info(&data).unwrap();
        assert!(same(&path, &recorded), "{path} / {recorded}");
        assert_eq!(size, 4); // what the scan measured, not walked again
        assert!(when > 1.7e9);
        let payload = info.with_file_name(format!("$R{}", &name[2..]));
        assert_eq!(
            std::fs::read(payload.join("sub").join("a.jpg")).unwrap(),
            b"jpeg"
        );
        assert_eq!(codes(&restore(&trashed.0)), [OpCode::Ok]);
        trashed.0.clear();
        assert_eq!(
            std::fs::read(folder.join("sub").join("a.jpg")).unwrap(),
            b"jpeg"
        );
    }

    /// What the bin holds is told from its `$I` records: one item more and its bytes after a
    /// move, as before once it is back.
    #[test]
    fn the_bin_is_sized_from_its_records() {
        let (_, _g) = setup();
        let _one = Threads::set(1);
        let dir = tempfile::tempdir().unwrap();
        if without_trash(dir.path(), "the_bin_is_sized_from_its_records") {
            return;
        }
        let root = sk_core::ops::win::volume_root(&s(dir.path())).unwrap();
        let f = dir.path().join("five.bin");
        std::fs::write(&f, b"12345").unwrap();
        let before = sk_core::ops::win::bin_usage(&root);
        assert!(before.0 >= 0 && before.1 >= 0, "{before:?}");
        let r = trash_all(&[s(&f)]).remove(0);
        let mut trashed = Trashed(vec![r.trash_ref.clone()]);
        assert_eq!(r.code, OpCode::Ok, "{r:?}");
        let during = sk_core::ops::win::bin_usage(&root);
        assert_eq!((during.0 - before.0, during.1 - before.1), (5, 1));
        assert_eq!(codes(&restore(&trashed.0)), [OpCode::Ok]);
        trashed.0.clear();
        assert_eq!(sk_core::ops::win::bin_usage(&root), before);
    }

    /// A folder with a file open inside cannot be renamed: it is reported held (by this test
    /// process, the Restart Manager says) and stays where it is, whole.
    #[test]
    fn a_folder_with_an_open_file_stays_whole() {
        let (_, _g) = setup();
        let dir = tempfile::tempdir().unwrap();
        if without_trash(dir.path(), "a_folder_with_an_open_file_stays_whole") {
            return;
        }
        let folder = dir.path().join("busy");
        std::fs::create_dir(&folder).unwrap();
        let held = folder.join("doc.docx");
        std::fs::write(&held, b"d").unwrap();
        std::fs::write(folder.join("free.txt"), b"f").unwrap();
        let handle = held_open(&held);
        let r = trash_all(&[s(&folder)]).remove(0);
        let _trashed = Trashed(vec![r.trash_ref.clone()]);
        drop(handle);
        assert_eq!(r.code, OpCode::Locked, "{r:?}");
        assert!(r.message.contains("使用"), "{r:?}");
        assert!(held.exists() && folder.join("free.txt").exists());
    }

    #[test]
    fn engine_reports_a_held_file() {
        let (_, _g) = setup();
        let dir = tempfile::tempdir().unwrap();
        if without_trash(dir.path(), "engine_reports_a_held_file") {
            return;
        }
        let held = dir.path().join("held.txt");
        std::fs::write(&held, b"h").unwrap();
        let handle = held_open(&held);
        // the shell meets the open handle itself
        let r = move_to_trash(
            &[s(&held)],
            OpOptions {
                check_locks: false,
                ..OpOptions::default()
            },
        )
        .remove(0);
        let _trashed = Trashed(vec![r.trash_ref.clone()]);
        drop(handle);
        assert_eq!(r.code, OpCode::Locked, "{r:?}");
        assert!(held.exists());
    }

    /// The junction only (never what it points to).
    struct Unmount(PathBuf);
    impl Drop for Unmount {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir(&self.0);
        }
    }

    /// `link` mounting the volume that holds `near` (what `mountvol` writes: a junction to
    /// `\\?\Volume{…}\`; made with `mklink /J`, it needs no privilege). None: the volume has no
    /// GUID.
    fn mount_volume_at(link: &Path, near: &Path) -> Option<Unmount> {
        let root = sk_core::ops::win::volume_root(&s(near)).unwrap();
        let guid = sk_core::ops::win::volume_guid(&root);
        if guid.is_empty() {
            return None;
        }
        let ok = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(format!("\\\\?\\Volume{guid}\\"))
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "mklink /J to the volume");
        Some(Unmount(link.to_path_buf()))
    }

    /// A folder that mounts a volume is never removed: not as an item (protected), not inside a
    /// deleted folder (`mount_inside`): either would quietly unmount the volume from it. Nothing
    /// on the mounted volume is touched.
    #[test]
    fn folder_mount_points_are_never_removed() {
        let (_, _g) = setup();
        let dir = tempfile::tempdir().unwrap();
        let holder = dir.path().join("holder");
        std::fs::create_dir(&holder).unwrap();
        std::fs::write(holder.join("x.txt"), b"x").unwrap();
        let mnt = holder.join("mnt");
        if !fs_caps::links(dir.path()) {
            fs_caps::skip("folder_mount_points", dir.path(), "no junctions");
            return;
        }
        let Some(_mounted) = mount_volume_at(&mnt, dir.path()) else {
            fs_caps::skip("folder_mount_points", dir.path(), "the volume has no GUID");
            return;
        };
        assert!(sk_core::ops::paths::is_mount(&mnt));
        assert_eq!(sk_core::ops::paths::protected_key(&s(&mnt)), Some("mount"));
        let r = delete_permanently(&[s(&mnt)], OpOptions::default()).remove(0);
        assert_eq!(r.code, OpCode::Protected, "{r:?}");
        let r = trash_all(&[s(&mnt)]).remove(0);
        let _trashed = Trashed(vec![r.trash_ref.clone()]);
        assert_eq!(r.code, OpCode::Protected, "{r:?}");
        assert!(std::fs::symlink_metadata(&mnt).is_ok());
        let r = delete_permanently(&[s(&holder)], OpOptions::default()).remove(0);
        assert_eq!(r.code, OpCode::Error, "{r:?}");
        assert!(r.message.contains("挂载点"), "{r:?}");
        assert!(sk_core::ops::paths::is_mount(&mnt), "still mounted");
        assert!(!holder.join("x.txt").exists()); // the rest went
        // a junction to a plain folder is only a name: it may go
        let plain = dir.path().join("plain");
        make_link(&holder, &plain);
        assert!(!sk_core::ops::paths::is_mount(&plain));
        assert_eq!(sk_core::ops::paths::protected_key(&s(&plain)), None);
    }

    use fs_caps::short_name;

    /// Asked through a short name (CI's temp folder is `C:\Users\RUNNER~1\…`), reported by the
    /// shell under the long one: still matched to the request.
    #[test]
    fn trash_through_a_short_name() {
        let (_, _g) = setup();
        let dir = tempfile::tempdir().unwrap();
        if without_trash(dir.path(), "trash_through_a_short_name") {
            return;
        }
        let folder = dir.path().join("a folder with a long name");
        std::fs::create_dir(&folder).unwrap();
        let file = folder.join("some long file name.txt");
        std::fs::write(&file, b"x").unwrap();
        let Some(short) = short_name(&file) else {
            fs_caps::skip("trash_through_a_short_name", dir.path(), "no 8.3 names");
            return;
        };
        let r = move_to_trash(&[short.as_str()], OpOptions::default()).remove(0);
        let _trashed = Trashed(vec![r.trash_ref.clone()]);
        assert_eq!(
            (r.code, r.path.as_str()),
            (OpCode::Ok, short.as_str()),
            "{r:?}"
        );
        assert!(!file.exists());
    }

    /// Names that are not Unicode asked through a short name, as in CI's temp folder: the shell
    /// reports the items under their long names, which are found from the exact names asked for
    /// (U+FFFD in place of the lone surrogate names nothing). Before, they went to the bin and
    /// were reported cancelled, without a trash reference.
    #[test]
    fn names_that_are_not_unicode_come_back_through_a_short_name() {
        let (_, _g) = setup();
        let dir = tempfile::tempdir().unwrap();
        if without_trash(
            dir.path(),
            "names_that_are_not_unicode_come_back_through_a_short_name",
        ) {
            return;
        }
        let long = dir.path().join("a folder with a long name");
        std::fs::create_dir(&long).unwrap();
        let Some(short) = short_name(&long) else {
            fs_caps::skip("names_through_a_short_name", dir.path(), "no 8.3 names");
            return;
        };
        super::odd_names_round_trip(Path::new(&short));
    }
}

// --------------------------------------------------------------------------- freedesktop details

#[cfg(all(unix, not(target_os = "macos")))]
mod freedesktop {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use trash::fd::seams;

    struct Seam;
    impl Drop for Seam {
        fn drop(&mut self) {
            *seams::EXISTING_DEV.lock() = None;
            *seams::MOUNT_TOP.lock() = None;
            *seams::USABLE_DIR.lock() = None;
        }
    }

    #[test]
    fn same_names_get_numbered() {
        let (_, _g) = setup();
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("one").join("same.txt");
        let b = dir.path().join("two").join("same.txt");
        for (p, body) in [(&a, b"1"), (&b, b"2")] {
            std::fs::create_dir(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        let res = trash_all(&[s(&a), s(&b)]);
        let names: Vec<String> = res
            .iter()
            .map(|r| {
                Path::new(&r.trash_ref)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(names, ["same.txt.trashinfo", "same.2.txt.trashinfo"]);
        let back = restore(&res.iter().map(|r| r.trash_ref.clone()).collect::<Vec<_>>());
        assert_eq!(codes(&back), [OpCode::Ok, OpCode::Ok]);
        assert_eq!(
            (std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap()),
            (b"1".to_vec(), b"2".to_vec())
        );
    }

    static VOL: Mutex<Option<PathBuf>> = Mutex::new(None);

    fn topdir_case(admin: bool) {
        let (_, _g) = setup();
        let _seam = Seam;
        let dir = tempfile::tempdir().unwrap();
        let vol = dir.path().join("vol");
        let f = vol.join("dir").join("a b.txt");
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(&f, b"t").unwrap();
        if admin {
            std::fs::create_dir(vol.join(".Trash")).unwrap();
            std::fs::set_permissions(vol.join(".Trash"), std::fs::Permissions::from_mode(0o1777))
                .unwrap();
        }
        *VOL.lock() = Some(vol.clone());
        *seams::EXISTING_DEV.lock() = Some(|_| Ok(u64::MAX)); // the home trash is elsewhere
        *seams::MOUNT_TOP.lock() = Some(|_, _| VOL.lock().clone().unwrap());
        let r = trash_all(&[s(&f)]).remove(0);
        // SAFETY: always succeeds
        let uid = unsafe { libc::getuid() };
        let root = if admin {
            vol.join(".Trash").join(uid.to_string())
        } else {
            vol.join(format!(".Trash-{uid}"))
        };
        assert_eq!(r.code, OpCode::Ok, "{r:?}");
        assert_eq!(
            PathBuf::from(&r.trash_ref),
            root.join("info").join("a b.txt.trashinfo")
        );
        assert_eq!(
            std::fs::metadata(&root).unwrap().permissions().mode() & 0o7777,
            0o700
        );
        assert!(
            std::fs::read_to_string(&r.trash_ref)
                .unwrap()
                .contains("Path=dir/a%20b.txt\n")
        );
        assert_eq!(
            codes(&restore(std::slice::from_ref(&r.trash_ref))),
            [OpCode::Ok]
        );
        assert_eq!(std::fs::read(&f).unwrap(), b"t");
    }

    #[test]
    fn topdir_trash() {
        topdir_case(false);
    }

    #[test]
    fn topdir_admin_trash() {
        topdir_case(true);
    }

    #[test]
    fn no_usable_trash_dir() {
        let (_, _g) = setup();
        let _seam = Seam;
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("vol").join("a.txt");
        std::fs::create_dir(f.parent().unwrap()).unwrap();
        std::fs::write(&f, b"a").unwrap();
        *VOL.lock() = Some(dir.path().join("vol"));
        *seams::EXISTING_DEV.lock() = Some(|_| Ok(u64::MAX));
        *seams::MOUNT_TOP.lock() = Some(|_, _| VOL.lock().clone().unwrap());
        *seams::USABLE_DIR.lock() = Some(|_, _, _| false);
        let r = trash_all(&[s(&f)]).remove(0);
        assert_eq!((r.code, r.message), (OpCode::NoTrash, text("no_trash_dir")));
        assert!(f.exists() && !trash_available(&s(&f)));
    }

    #[test]
    fn directorysizes_lines() {
        use trash::fd::{DirSize, format_directorysizes, parse_directorysizes};
        let entries = vec![
            DirSize {
                size: 4096,
                mtime: 1_290_678_378,
                name: b"Untitled Folder".to_vec(),
            },
            DirSize {
                size: 12,
                mtime: 7,
                name: "报告 %".as_bytes().to_vec(),
            },
        ];
        let text = format_directorysizes(&entries);
        assert!(text.starts_with(b"4096 1290678378 Untitled%20Folder\n"));
        assert_eq!(parse_directorysizes(&text), entries);
        // malformed lines and names with a slash are left out
        let odd = b"x 1 a\n10 2\n5 6 a%2Fb\n3 4 ok\n";
        assert_eq!(
            parse_directorysizes(odd),
            [DirSize {
                size: 3,
                mtime: 4,
                name: b"ok".to_vec()
            }]
        );
    }

    /// A folder moved to the trash with its scanned size gets its `directorysizes` line; the
    /// trash's size is told from that line without walking the folder (while the `.trashinfo`
    /// is unchanged), and from a walk once it is not (the line is written again).
    #[test]
    fn trashed_folders_are_sized_from_the_cache() {
        let (base, _g) = setup();
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("big folder");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("x.bin"), vec![b'x'; 10_000]).unwrap();
        let sizes = HashMap::from([(s(&folder), 777_777i64)]);
        let r = move_to_trash(
            &[s(&folder)],
            OpOptions {
                sizes: Some(&sizes),
                ..OpOptions::default()
            },
        )
        .remove(0);
        let mut trashed = Trashed(vec![r.trash_ref.clone()]);
        assert_eq!(r.code, OpCode::Ok, "{r:?}");
        let trash_root = base.join("xdg").join("Trash");
        let lines = trash::fd::parse_directorysizes(
            &std::fs::read(trash_root.join("directorysizes")).unwrap(),
        );
        let mine = lines
            .iter()
            .find(|e| e.name == b"big folder")
            .expect("a line for the folder");
        assert_eq!(mine.size, 777_777);
        let used = trash_status(&s(dir.path()), true).used;
        assert!(used >= 777_777, "{used}"); // the line's size, not the folder's 10 kB
        // the .trashinfo changed: walked again, the line rewritten with what is there
        let info = PathBuf::from(&r.trash_ref);
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&info)
            .unwrap()
            .set_modified(later)
            .unwrap();
        let used = trash_status(&s(dir.path()), true).used;
        assert!((10_000..777_777).contains(&used), "{used}");
        let lines = trash::fd::parse_directorysizes(
            &std::fs::read(trash_root.join("directorysizes")).unwrap(),
        );
        let mine = lines.iter().find(|e| e.name == b"big folder").unwrap();
        assert!(mine.size < 777_777 && mine.size >= 10_000, "{mine:?}");
        assert_eq!(codes(&restore(&trashed.0)), [OpCode::Ok]);
        trashed.0.clear();
        assert!(folder.join("x.bin").exists());
        // gone from the trash: its line goes too
        trash_status(&s(dir.path()), true);
        let lines = trash::fd::parse_directorysizes(
            &std::fs::read(trash_root.join("directorysizes")).unwrap(),
        );
        assert!(!lines.iter().any(|e| e.name == b"big folder"), "{lines:?}");
    }
}

// --------------------------------------------------------------------------- pure helpers

#[test]
fn parse_recycle_info_versions() {
    let path = r"C:\Users\u\报告.txt";
    let filetime: i64 = 133_000_000_000_000_000;
    let units: Vec<u8> = path
        .encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    let mut v2 = Vec::new();
    for x in [2i64, 1234, filetime] {
        v2.extend(x.to_le_bytes());
    }
    v2.extend(((path.encode_utf16().count() + 1) as i32).to_le_bytes());
    v2.extend(&units);
    let mut v1 = Vec::new();
    for x in [1i64, 99, filetime] {
        v1.extend(x.to_le_bytes());
    }
    let mut padded = units.clone();
    padded.resize(520, 0);
    v1.extend(padded);
    let want = (filetime - 116_444_736_000_000_000) as f64 / 1e7;
    assert_eq!(
        parse_recycle_info(&v2).unwrap(),
        (1234, want, path.to_string())
    );
    let got = parse_recycle_info(&v1).unwrap();
    assert_eq!((got.0, got.2), (99, path.to_string()));
    let mut bad7 = Vec::new();
    for x in [7i64, 0, 0] {
        bad7.extend(x.to_le_bytes());
    }
    let mut empty2 = Vec::new();
    for x in [2i64, 0, 0] {
        empty2.extend(x.to_le_bytes());
    }
    empty2.extend(1i32.to_le_bytes());
    empty2.extend([0, 0]);
    for bad in [Vec::new(), bad7, empty2] {
        assert!(parse_recycle_info(&bad).unwrap_err().contains("record"));
    }
}

#[test]
fn parse_trashinfo_paths() {
    let body = "[Trash Info]\nPath=/home/u/a%20b/%E6%96%87.txt\nDeletionDate=2026-10-18T12:00:00\n";
    assert_eq!(
        parse_trashinfo(body).unwrap(),
        "/home/u/a b/文.txt".as_bytes()
    );
    assert_eq!(
        parse_trashinfo("[Other]\nPath=/x\n[Trash Info]\nPath=rel/y\n").unwrap(),
        b"rel/y"
    );
    assert!(
        parse_trashinfo("[Other]\nPath=/x\n")
            .unwrap_err()
            .contains("no Path")
    );
}

#[test]
fn rename_without_replacing() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b, c) = (
        dir.path().join("a"),
        dir.path().join("b"),
        dir.path().join("c"),
    );
    std::fs::write(&a, b"a").unwrap();
    std::fs::write(&b, b"b").unwrap();
    let e = rename_noreplace(&a, &b).unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(
        (std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap()),
        (b"a".to_vec(), b"b".to_vec())
    );
    rename_noreplace(&a, &c).unwrap();
    assert_eq!(std::fs::read(&c).unwrap(), b"a");
    assert!(!a.exists());
}

// --------------------------------------------------------------------------- permanent deletion

fn make_link(target: &Path, link: &Path) {
    assert!(fs_caps::dir_link(target, link), "link {}", link.display());
}

#[test]
fn delete_permanently_trees_links_files() {
    let (_, _g) = setup();
    let dir = tempfile::tempdir().unwrap();
    let outside = dir.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("keep.txt"), b"k").unwrap();
    let links = fs_caps::links(dir.path());
    if !links {
        fs_caps::skip("deleting links", dir.path(), "no links");
    }
    let tree = dir.path().join("tree");
    std::fs::create_dir_all(tree.join("a b").join("深")).unwrap();
    std::fs::write(tree.join("a b").join("深").join("x.txt"), b"x").unwrap();
    if fs_caps::read_only_files(dir.path()) {
        let ro = tree.join("readonly.txt");
        std::fs::write(&ro, b"r").unwrap();
        let mut perm = std::fs::metadata(&ro).unwrap().permissions();
        perm.set_readonly(true);
        std::fs::set_permissions(&ro, perm).unwrap();
    } else {
        fs_caps::skip(
            "deleting a read-only file",
            dir.path(),
            "no read-only files",
        );
    }
    let top_link = dir.path().join("top-link");
    let mut asked = vec![s(&tree)];
    if links {
        make_link(&outside, &tree.join("a b").join("link")); // inside the tree: removed, never followed
        make_link(&outside, &top_link);
        asked.push(s(&top_link));
    }
    let single = dir.path().join("single.bin");
    std::fs::write(&single, b"s").unwrap();
    asked.push(s(&single));
    let mut seen: Vec<OpCode> = Vec::new();
    let mut progress = |_: usize, _: usize, r: &OpResult| seen.push(r.code);
    let res = delete_permanently(
        &asked,
        OpOptions {
            progress: Some(&mut progress),
            ..OpOptions::default()
        },
    );
    let got: Vec<(OpCode, String)> = res.iter().map(|r| (r.code, r.message.clone())).collect();
    assert_eq!(got, vec![(OpCode::Ok, text("deleted")); asked.len()]);
    assert_eq!(seen, vec![OpCode::Ok; asked.len()]);
    assert!(!tree.exists() && std::fs::symlink_metadata(&top_link).is_err() && !single.exists());
    assert_eq!(std::fs::read(outside.join("keep.txt")).unwrap(), b"k");
}

#[test]
fn delete_permanently_cancel() {
    let (_, _g) = setup();
    let _one = Threads::set(1); // one item after the other: the first is done when it stops
    let dir = tempfile::tempdir().unwrap();
    let files: Vec<String> = (0..3)
        .map(|i| {
            let p = dir.path().join(format!("d{i}.txt"));
            std::fs::write(&p, b"d").unwrap();
            s(&p)
        })
        .collect();
    let seen = AtomicUsize::new(0);
    let mut progress = |_: usize, _: usize, _: &OpResult| {
        seen.fetch_add(1, Ordering::SeqCst);
    };
    let cancel = || seen.load(Ordering::SeqCst) > 0;
    let res = delete_permanently(
        &files,
        OpOptions {
            progress: Some(&mut progress),
            cancel: Some(&cancel),
            ..OpOptions::default()
        },
    );
    assert_eq!(
        codes(&res),
        [OpCode::Ok, OpCode::Cancelled, OpCode::Cancelled]
    );
    let exists: Vec<bool> = files.iter().map(|p| Path::new(p).exists()).collect();
    assert_eq!(exists, [false, true, true]);
}

#[test]
fn delete_inside_a_folder_can_be_cancelled() {
    let (_, _g) = setup();
    let _one = Threads::set(1); // the polls counted below are one thread's
    let dir = tempfile::tempdir().unwrap();
    let tree = dir.path().join("t");
    for i in 0..3 {
        std::fs::create_dir_all(tree.join(format!("s{i}"))).unwrap();
        std::fs::write(tree.join(format!("s{i}")).join("f.txt"), b"f").unwrap();
    }
    // the item, the tree, its first folder; then stop
    let polls = AtomicUsize::new(0);
    let cancel = || polls.fetch_add(1, Ordering::SeqCst) >= 3;
    let r = delete_permanently(
        &[s(&tree)],
        OpOptions {
            cancel: Some(&cancel),
            check_locks: false,
            ..OpOptions::default()
        },
    )
    .remove(0);
    assert_eq!(
        (r.code, r.message),
        (OpCode::Cancelled, text("cancelled_partly"))
    );
    assert!(tree.exists());
}

// --------------------------------------------------------------------------- several threads

/// `folders` folders of `files` files below `top`, two levels deep.
fn wide_tree(top: &Path, folders: usize, files: usize) {
    for a in 0..folders {
        for b in 0..2 {
            let d = top.join(format!("a{a}")).join(format!("b{b}"));
            std::fs::create_dir_all(&d).unwrap();
            for f in 0..files {
                std::fs::write(d.join(format!("f{f}.txt")), b"w").unwrap();
            }
        }
        std::fs::write(top.join(format!("a{a}")).join("top.txt"), b"t").unwrap();
    }
}

/// Several items and the folders inside them go on several threads at once: all of it, each
/// item reported once, as itself.
#[test]
fn trees_are_deleted_on_several_threads() {
    let (_, _g) = setup();
    let _four = Threads::set(4);
    let dir = tempfile::tempdir().unwrap();
    let items: Vec<PathBuf> = (0..5)
        .map(|i| dir.path().join(format!("item{i}")))
        .collect();
    for it in &items {
        wide_tree(it, 12, 15);
    }
    let file = dir.path().join("single.txt");
    std::fs::write(&file, b"s").unwrap();
    let mut asked: Vec<String> = items.iter().map(|p| s(p)).collect();
    asked.insert(2, s(&file));
    let seen = std::sync::Mutex::new(Vec::new());
    let mut progress = |done: usize, total: usize, r: &OpResult| {
        seen.lock().unwrap().push((done, total, r.path.clone()));
    };
    let res = delete_permanently(
        &asked,
        OpOptions {
            progress: Some(&mut progress),
            ..OpOptions::default()
        },
    );
    assert_eq!(codes(&res), vec![OpCode::Ok; 6], "{res:?}");
    assert_eq!(
        res.iter().map(|r| r.path.clone()).collect::<Vec<_>>(),
        asked
    );
    let seen = seen.into_inner().unwrap();
    assert_eq!(
        seen.iter().map(|s| s.0).collect::<Vec<_>>(),
        (1..=6).collect::<Vec<_>>()
    );
    let mut reported: Vec<String> = seen.into_iter().map(|s| s.2).collect();
    reported.sort();
    let mut want = asked.clone();
    want.sort();
    assert_eq!(reported, want);
    assert!(items.iter().all(|p| std::fs::symlink_metadata(p).is_err()) && !file.exists());
}

/// What cannot be deleted (held open on Windows, in a folder without write permission
/// elsewhere) stays with each folder above it; on several threads the rest of the item goes.
#[test]
fn what_cannot_go_stays_with_its_folders_the_rest_goes() {
    let (_, _g) = setup();
    let _four = Threads::set(4);
    let dir = tempfile::tempdir().unwrap();
    if !WIN && !fs_caps::folder_permissions(dir.path()) {
        let why = "a read-only folder does not keep this process out (root, or no permissions)";
        fs_caps::skip("what_cannot_go_stays_with_its_folders", dir.path(), why);
        return;
    }
    let top = dir.path().join("sel");
    wide_tree(&top, 8, 10);
    let stuck_dir = top.join("a3").join("b1");
    let stuck = stuck_dir.join("f4.txt");
    #[cfg(windows)]
    let handle = held_open(&stuck);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stuck_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    }
    let r = delete_permanently(&[s(&top)], no_locks()).remove(0);
    #[cfg(windows)]
    drop(handle);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stuck_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    assert_eq!(r.code, OpCode::Error, "{r:?}");
    assert!(r.message.starts_with("部分内容未能删除"), "{r:?}");
    assert!(stuck.exists());
    for a in (0..8).filter(|&a| a != 3) {
        assert!(!top.join(format!("a{a}")).exists(), "a{a} left");
    }
    assert!(!top.join("a3").join("b0").exists() && !top.join("a3").join("top.txt").exists());
}

/// An item inside another one (never from the basket, but the call allows it) is deleted first,
/// then the one around it: both are done, nothing is left.
#[test]
fn an_item_inside_another_is_deleted_first() {
    let (_, _g) = setup();
    let _four = Threads::set(4);
    let dir = tempfile::tempdir().unwrap();
    let outer = dir.path().join("outer");
    wide_tree(&outer, 4, 5);
    let inner = outer.join("a1");
    let res = delete_permanently(&[s(&outer), s(&inner)], no_locks());
    assert_eq!(codes(&res), [OpCode::Ok, OpCode::Ok], "{res:?}");
    assert!(std::fs::symlink_metadata(&outer).is_err());
}

/// On several threads a cancel stops what has not started: an item reported done is gone, one
/// reported cancelled is (partly) still there, and not every item ran.
#[test]
fn a_cancel_stops_deleting_on_several_threads() {
    let (_, _g) = setup();
    let _four = Threads::set(4);
    let dir = tempfile::tempdir().unwrap();
    let items: Vec<PathBuf> = (0..16).map(|i| dir.path().join(format!("c{i}"))).collect();
    for it in &items {
        wide_tree(it, 3, 6);
    }
    let seen = AtomicUsize::new(0);
    let mut progress = |_: usize, _: usize, _: &OpResult| {
        seen.fetch_add(1, Ordering::SeqCst);
    };
    let cancel = || seen.load(Ordering::SeqCst) > 0;
    let asked: Vec<String> = items.iter().map(|p| s(p)).collect();
    let res = delete_permanently(
        &asked,
        OpOptions {
            progress: Some(&mut progress),
            cancel: Some(&cancel),
            check_locks: false,
            ..OpOptions::default()
        },
    );
    assert_eq!(
        seen.load(Ordering::SeqCst),
        16,
        "every item gets its final result"
    );
    let done = res.iter().filter(|r| r.code == OpCode::Ok).count();
    assert!((1..16).contains(&done), "{done} of 16 done");
    for (r, p) in res.iter().zip(&items) {
        match r.code {
            OpCode::Ok => assert!(!p.exists(), "{r:?}"),
            OpCode::Cancelled => assert!(p.exists(), "{r:?}"),
            other => panic!("{other:?}: {r:?}"),
        }
    }
}

#[test]
fn plain_paths_reach_themselves() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("scan");
    std::fs::create_dir_all(root.join("Sub")).unwrap();
    std::fs::write(root.join("Sub").join("a.txt"), b"a").unwrap();
    let mut names = HashMap::new();
    assert!(!trash::reaches_another(
        &s(&root.join("Sub").join("a.txt")),
        &s(&root),
        &mut names
    ));
    assert!(!trash::reaches_another(&s(&root), &s(&root), &mut names)); // the root itself
    assert!(!trash::win32_trims(&s(&root.join("Sub"))));
    assert_eq!(trash::win32_trims(r"C:\odd.\x"), WIN);
}

// --------------------------------------------------------------------------- what a deleted folder holds

/// The deletion seams, unset again when dropped.
struct Walk;

impl Drop for Walk {
    fn drop(&mut self) {
        *trash::seams::ENTERING.lock() = None;
        *trash::seams::LISTED.lock() = None;
        *trash::seams::OPENED_NAME.lock() = None;
        trash::seams::OPEN_FOLDERS.store(0, Ordering::SeqCst);
    }
}

fn named(p: &Path, name: &str) -> bool {
    p.file_name().is_some_and(|n| n == name)
}

fn no_locks() -> OpOptions<'static> {
    OpOptions {
        check_locks: false,
        ..OpOptions::default()
    }
}

/// A name listed in a folder that a path cannot hold (WinBtrfs lists a Linux file `..\x` as it
/// is; joined to its folder's path that is a way out of it) is never joined into a path: nothing
/// outside the folder is touched, the rest of it goes.
#[test]
fn a_listed_name_that_climbs_out_is_never_followed() {
    let (_, _g) = setup();
    let _w = Walk;
    let dir = tempfile::tempdir().unwrap();
    let sel = dir.path().join("sel");
    std::fs::create_dir_all(sel.join("sub")).unwrap();
    std::fs::write(sel.join("sub").join("a.txt"), b"a").unwrap();
    let outside = dir.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("important.txt"), b"keep").unwrap();
    let climb = if WIN { "..\\outside" } else { "../outside" };
    *trash::seams::LISTED.lock() = Some(std::sync::Arc::new(
        move |folder: &Path, names: &mut Vec<std::ffi::OsString>| {
            if named(folder, "sel") {
                names.push(climb.into());
            }
        },
    ));
    let r = delete_permanently(&[s(&sel)], no_locks()).remove(0);
    assert_eq!(
        std::fs::read(outside.join("important.txt")).unwrap(),
        b"keep"
    );
    assert_eq!(r.code, OpCode::Error, "{r:?}");
    assert!(r.message.contains("路径"), "{r:?}");
    assert!(!sel.join("sub").exists()); // the rest went
}

/// The folder about to be listed is replaced by a link to somewhere else: the link is not
/// followed (the old walk listed the target and deleted what it held).
#[test]
fn a_folder_swapped_for_a_link_is_not_entered() {
    let (_, _g) = setup();
    let _w = Walk;
    let dir = tempfile::tempdir().unwrap();
    if !fs_caps::links(dir.path()) {
        fs_caps::skip(
            "a_folder_swapped_for_a_link_is_not_entered",
            dir.path(),
            "no links",
        );
        return;
    }
    let target = dir.path().join("target");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("precious.txt"), b"keep").unwrap();
    let sel = dir.path().join("sel");
    std::fs::create_dir_all(sel.join("sub")).unwrap();
    std::fs::write(sel.join("sub").join("a.txt"), b"a").unwrap();
    let (s_sel, s_target) = (sel.clone(), target.clone());
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let once = done.clone();
    *trash::seams::ENTERING.lock() = Some(std::sync::Arc::new(move |folder: &Path| {
        if named(folder, "sub") && !once.swap(true, Ordering::SeqCst) {
            std::fs::rename(s_sel.join("sub"), s_sel.join("moved")).unwrap();
            make_link(&s_target, &s_sel.join("sub"));
        }
    }));
    let r = delete_permanently(&[s(&sel)], no_locks()).remove(0);
    eprintln!("{r:?}");
    assert!(done.load(Ordering::SeqCst));
    assert_eq!(std::fs::read(target.join("precious.txt")).unwrap(), b"keep");
    assert!(!r.ok, "{r:?}");
}

/// A folder above the one being listed is replaced by a link meanwhile: what is listed and
/// deleted is still the folder the deletion started in, never the link's target.
#[test]
fn a_parent_swapped_for_a_link_is_not_followed() {
    let (_, _g) = setup();
    let _w = Walk;
    let dir = tempfile::tempdir().unwrap();
    if !fs_caps::links(dir.path()) {
        fs_caps::skip(
            "a_parent_swapped_for_a_link_is_not_followed",
            dir.path(),
            "no links",
        );
        return;
    }
    let target = dir.path().join("target");
    std::fs::create_dir_all(target.join("deeper")).unwrap();
    std::fs::write(target.join("deeper").join("precious.txt"), b"keep").unwrap();
    let sel = dir.path().join("sel");
    std::fs::create_dir_all(sel.join("sub").join("deeper")).unwrap();
    std::fs::write(sel.join("sub").join("deeper").join("a.txt"), b"a").unwrap();
    let (s_sel, s_target) = (sel.clone(), target.clone());
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let once = done.clone();
    *trash::seams::ENTERING.lock() = Some(std::sync::Arc::new(move |folder: &Path| {
        if named(folder, "deeper") && !once.swap(true, Ordering::SeqCst) {
            std::fs::rename(s_sel.join("sub"), s_sel.join("moved")).unwrap();
            make_link(&s_target, &s_sel.join("sub"));
        }
    }));
    let r = delete_permanently(&[s(&sel)], no_locks()).remove(0);
    eprintln!("{r:?}");
    assert!(done.load(Ordering::SeqCst));
    assert_eq!(
        std::fs::read(target.join("deeper").join("precious.txt")).unwrap(),
        b"keep"
    );
    assert!(!r.ok, "{r:?}");
}

/// On POSIX a backslash is an ordinary character of a name: such entries are deleted as they
/// are, and nothing a Windows reading of the name would point at is touched.
#[cfg(unix)]
#[test]
fn names_with_a_backslash_are_deleted_as_they_are() {
    let (_, _g) = setup();
    let dir = tempfile::tempdir().unwrap();
    if !fs_caps::backslash_names(dir.path()) {
        fs_caps::skip(
            "names_with_a_backslash",
            dir.path(),
            "no backslash in names",
        );
        return;
    }
    let outside = dir.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("important.txt"), b"keep").unwrap();
    let sel = dir.path().join("sel");
    std::fs::create_dir(&sel).unwrap();
    std::fs::write(sel.join("..\\outside"), b"x").unwrap();
    std::fs::create_dir(sel.join("a\\b")).unwrap();
    std::fs::write(sel.join("a\\b").join("c"), b"c").unwrap();
    let r = delete_permanently(&[s(&sel)], no_locks()).remove(0);
    assert!(r.ok, "{r:?}");
    assert!(!sel.exists());
    assert_eq!(
        std::fs::read(outside.join("important.txt")).unwrap(),
        b"keep"
    );
}

/// Names Win32 would rewrite (a trailing dot or space), names that are not Unicode and paths
/// past MAX_PATH inside a deleted folder are deleted exactly as they are named; the sibling a
/// rewritten name would reach is deleted for itself, not instead.
#[cfg(windows)]
#[test]
fn odd_names_inside_a_folder_are_deleted_exactly() {
    use std::os::windows::ffi::OsStringExt;
    let (_, _g) = setup();
    let dir = tempfile::tempdir().unwrap();
    let sel = dir.path().join("sel");
    std::fs::create_dir(&sel).unwrap();
    let long = fs_caps::verbatim(&sel);
    if fs_caps::trailing_names(dir.path()) {
        std::fs::write(format!("{long}\\odd."), b"dot").unwrap();
        std::fs::write(format!("{long}\\odd"), b"plain").unwrap();
        std::fs::create_dir(format!("{long}\\space ")).unwrap();
        std::fs::write(format!("{long}\\space \\in.txt"), b"in").unwrap();
    } else {
        fs_caps::skip("names ending in a dot or space", dir.path(), "refused");
    }
    if fs_caps::odd_names(dir.path()) {
        let mut w: Vec<u16> = "lone".encode_utf16().collect();
        w.push(0xDC00);
        let lone = sel.join(std::ffi::OsString::from_wide(&w));
        std::fs::write(&lone, b"lone").unwrap();
    } else {
        fs_caps::skip("names that are not Unicode", dir.path(), "refused");
    }
    let mut p = sel.join("deep");
    std::fs::create_dir(&p).unwrap();
    while s(&p).len() < 300 {
        p = p.join("n".repeat(40));
        std::fs::create_dir(fs_caps::verbatim(&p)).unwrap();
    }
    std::fs::write(fs_caps::verbatim(&p.join("leaf.txt")), b"x").unwrap();
    let r = delete_permanently(&[s(&sel)], no_locks()).remove(0);
    assert_eq!(
        (r.code, r.message.as_str()),
        (OpCode::Ok, "已永久删除"),
        "{r:?}"
    );
    assert!(std::fs::symlink_metadata(&long).is_err());
}

// --------------------------------------------------------------------------- names in another case (Windows)

/// Make every handle report its name upper-cased, as an SMB redirector, WinFsp or Dokan may
/// report a name in the case another program opened it with (they keep the first opener's).
#[cfg(windows)]
fn report_names_upper_cased() {
    *trash::seams::OPENED_NAME.lock() = Some(std::sync::Arc::new(|name: &mut Vec<u16>| {
        for u in name.iter_mut() {
            if (u16::from(b'a')..=u16::from(b'z')).contains(u) {
                *u -= 32;
            }
        }
    }));
}

/// A file system that reports the names of what is opened in another case than the one listed
/// does not stop a deletion: each entry is the very one listed, by its file id.
#[cfg(windows)]
#[test]
fn names_reported_in_another_case_are_still_deleted() {
    let (_, _g) = setup();
    let _w = Walk;
    let dir = tempfile::tempdir().unwrap();
    let sel = dir.path().join("sel");
    std::fs::create_dir_all(sel.join("sub").join("Deeper")).unwrap();
    std::fs::write(sel.join("sub").join("Deeper").join("a.txt"), b"a").unwrap();
    std::fs::write(sel.join("b.TXT"), b"b").unwrap();
    report_names_upper_cased();
    let r = delete_permanently(&[s(&sel)], no_locks()).remove(0);
    assert_eq!(
        (r.code, r.message.as_str()),
        (OpCode::Ok, "已永久删除"),
        "{r:?}"
    );
    assert!(!sel.exists());
}

/// Where the reported name matches only in case, the entry must be the one its folder listed:
/// a folder from outside moved in under the deleted folder's name for it is left alone.
#[cfg(windows)]
#[test]
fn a_folder_moved_in_is_not_deleted_for_a_name_in_another_case() {
    let (_, _g) = setup();
    let _w = Walk;
    let dir = tempfile::tempdir().unwrap();
    let sel = dir.path().join("sel");
    std::fs::create_dir_all(sel.join("sub")).unwrap();
    std::fs::write(sel.join("sub").join("x.txt"), b"x").unwrap();
    let outside = dir.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("x.txt"), b"keep").unwrap();
    report_names_upper_cased();
    let (s_sel, s_out) = (sel.clone(), outside.clone());
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let once = done.clone();
    *trash::seams::ENTERING.lock() = Some(std::sync::Arc::new(move |p: &Path| {
        if named(p, "x.txt") && !once.swap(true, Ordering::SeqCst) {
            std::fs::rename(s_sel.join("sub"), s_sel.join("moved")).unwrap();
            std::fs::rename(&s_out, s_sel.join("sub")).unwrap();
        }
    }));
    let r = delete_permanently(&[s(&sel)], no_locks()).remove(0);
    assert!(done.load(Ordering::SeqCst));
    assert!(!r.ok && r.message.contains("换成了别的项"), "{r:?}");
    assert_eq!(
        std::fs::read(sel.join("sub").join("x.txt")).unwrap(),
        b"keep"
    );
}

/// In a case-sensitive folder (NTFS's per-folder flag; WinBtrfs) names that differ only in case
/// are two entries: with names reported in another case, each is deleted as itself (its own
/// file id), neither in place of the other. Skipped where no such folder can be made.
#[cfg(windows)]
#[test]
fn case_twins_are_each_deleted_when_names_are_reported_in_another_case() {
    let (_, _g) = setup();
    let _w = Walk;
    let dir = tempfile::tempdir().unwrap();
    let sel = dir.path().join("sel");
    let cs = sel.join("cs");
    std::fs::create_dir_all(&cs).unwrap();
    let _ = std::process::Command::new("fsutil")
        .args(["file", "setCaseSensitiveInfo"])
        .arg(&cs)
        .arg("enable")
        .output();
    std::fs::write(cs.join("a"), b"a").unwrap();
    if std::fs::write(cs.join("A"), b"A").is_err() || std::fs::read(cs.join("a")).unwrap() != b"a" {
        eprintln!("skipped: {} is not case-sensitive", cs.display());
        return;
    }
    for d in ["d", "D"] {
        std::fs::create_dir(cs.join(d)).unwrap();
        std::fs::write(cs.join(d).join("f.txt"), d).unwrap();
    }
    report_names_upper_cased();
    let r = delete_permanently(&[s(&sel)], no_locks()).remove(0);
    assert_eq!(r.code, OpCode::Ok, "{r:?}");
    assert!(!sel.exists());
}

// --------------------------------------------------------------------------- deep trees (POSIX)

/// Make `levels` nested folders named `name` below `top` (relative to each folder's descriptor:
/// deeper than PATH_MAX too), with a file in every hundredth one and at the bottom.
#[cfg(unix)]
fn nested(top: &Path, levels: usize, name: &str) {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    std::fs::create_dir(top).unwrap();
    let c = |s: &str| CString::new(s).unwrap();
    let (dir, file) = (c(name), c("f.txt"));
    let top_c = CString::new(top.as_os_str().as_bytes()).unwrap();
    // SAFETY: NUL-terminated names; every descriptor opened here is closed here.
    unsafe {
        let mut fd = libc::open(top_c.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY);
        assert!(fd >= 0);
        for level in 1..=levels {
            assert_eq!(libc::mkdirat(fd, dir.as_ptr(), 0o755), 0, "level {level}");
            let next = libc::openat(fd, dir.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY);
            assert!(next >= 0, "level {level}");
            libc::close(fd);
            fd = next;
            if level % 100 == 0 || level == levels {
                let f = libc::openat(fd, file.as_ptr(), libc::O_WRONLY | libc::O_CREAT, 0o644);
                assert!(f >= 0, "level {level}");
                libc::close(f);
            }
        }
        libc::close(fd);
    }
}

/// A tree a thousand and more folders deep (longer than PATH_MAX) is deleted, all of it, by a
/// process allowed 128 open files (macOS gives programs 256): the deletion does not keep a
/// descriptor open per level. The work runs in a child process with that limit (the limit is
/// the process's: lowering it here would starve the tests beside this one).
#[cfg(unix)]
#[test]
fn deep_trees_are_deleted_within_a_few_descriptors() {
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "deep_tree_child_process",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{text}");
    assert!(text.contains("1 passed"), "{text}");
}

/// The child process of `deep_trees_are_deleted_within_a_few_descriptors`.
#[cfg(unix)]
#[test]
#[ignore = "run in a child process with few open files allowed by deep_trees_are_deleted_within_a_few_descriptors"]
fn deep_tree_child_process() {
    let (_, _g) = setup();
    // SAFETY: zeroed is a valid rlimit; this process runs this one test.
    unsafe {
        let mut lim: libc::rlimit = std::mem::zeroed();
        assert_eq!(libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim), 0);
        lim.rlim_cur = 128.min(lim.rlim_max);
        assert_eq!(libc::setrlimit(libc::RLIMIT_NOFILE, &lim), 0);
    }
    let dir = tempfile::tempdir().unwrap();
    let top = dir.path().join("deep");
    nested(&top, 1200, "level-dir"); // 1200 × 10 bytes: past PATH_MAX (4096)
    let r = delete_permanently(&[s(&top)], no_locks()).remove(0);
    assert_eq!(
        (r.code, r.message.as_str()),
        (OpCode::Ok, "已永久删除"),
        "{r:?}"
    );
    assert!(std::fs::symlink_metadata(&top).is_err());
}

/// With one folder's descriptor open at a time, a tree with folders beside each other at every
/// level is deleted whole: a folder whose descriptor was closed is reached again.
#[cfg(unix)]
#[test]
fn a_tree_is_deleted_with_one_descriptor_kept_open() {
    let (_, _g) = setup();
    let _w = Walk;
    trash::seams::OPEN_FOLDERS.store(1, Ordering::SeqCst);
    let dir = tempfile::tempdir().unwrap();
    let sel = dir.path().join("sel");
    for p in ["a/b/c", "a/b/c2", "a/b2", "a2/x/y"] {
        std::fs::create_dir_all(sel.join(p)).unwrap();
        std::fs::write(sel.join(p).join("f.txt"), b"f").unwrap();
    }
    std::fs::write(sel.join("a").join("g.txt"), b"g").unwrap();
    let r = delete_permanently(&[s(&sel)], no_locks()).remove(0);
    assert_eq!(r.code, OpCode::Ok, "{r:?}");
    assert!(!sel.exists());
}

/// A folder whose descriptor was closed while the deletion went deeper is reached again by its
/// identity (device and inode), never by its name alone: when the folders above were moved
/// away and another folder now has their names, the other folder is not touched.
#[cfg(unix)]
#[test]
fn a_folder_reached_again_must_be_the_one_left() {
    let (_, _g) = setup();
    let _w = Walk;
    trash::seams::OPEN_FOLDERS.store(2, Ordering::SeqCst);
    let dir = tempfile::tempdir().unwrap();
    let sel = dir.path().join("sel");
    std::fs::create_dir_all(sel.join("a/b/c/d")).unwrap();
    std::fs::write(sel.join("a/b/c/d/e.txt"), b"e").unwrap();
    std::fs::write(sel.join("a/z.txt"), b"z").unwrap();
    let outside = dir.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    // every folder lists its names in order: `b` before `z.txt`
    *trash::seams::LISTED.lock() = Some(std::sync::Arc::new(
        |_: &Path, names: &mut Vec<std::ffi::OsString>| names.sort(),
    ));
    let (s_sel, s_out) = (sel.clone(), outside.clone());
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let once = done.clone();
    // deep in d (a and b closed): c goes elsewhere, a too, and a new `a` holds what is not ours
    *trash::seams::ENTERING.lock() = Some(std::sync::Arc::new(move |p: &Path| {
        if named(p, "e.txt") && !once.swap(true, Ordering::SeqCst) {
            std::fs::rename(s_sel.join("a/b/c"), s_out.join("c")).unwrap();
            std::fs::rename(s_sel.join("a"), s_out.join("a")).unwrap();
            std::fs::create_dir_all(s_sel.join("a/b")).unwrap();
            std::fs::write(s_sel.join("a/z.txt"), b"keep").unwrap();
            std::fs::write(s_sel.join("a/b/keep.txt"), b"keep").unwrap();
        }
    }));
    let r = delete_permanently(&[s(&sel)], no_locks()).remove(0);
    assert!(done.load(Ordering::SeqCst));
    assert!(!r.ok && r.message.contains("换成了别的项"), "{r:?}");
    assert_eq!(std::fs::read(sel.join("a/z.txt")).unwrap(), b"keep");
    assert_eq!(std::fs::read(sel.join("a/b/keep.txt")).unwrap(), b"keep");
}

/// A bind mount of a folder on the same file system inside a deleted folder is a mount point
/// like any other: protected, and not entered (its source keeps everything). Needs root (CI's
/// Linux job: passwordless sudo); skipped otherwise.
#[cfg(target_os = "linux")]
#[test]
fn bind_mounts_inside_are_not_entered() {
    let (_, _g) = setup();
    let Some(root) = privileged() else {
        eprintln!("skipped: mount --bind needs root or passwordless sudo");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir(&src).unwrap();
    std::fs::write(src.join("data.txt"), b"keep").unwrap();
    let sel = dir.path().join("sel");
    let mnt = sel.join("mnt");
    std::fs::create_dir_all(&mnt).unwrap();
    std::fs::write(sel.join("own.txt"), b"x").unwrap();
    let Some(_bound) = BindMount::new(&root, &src, &mnt) else {
        eprintln!("skipped: mount --bind failed (no CAP_SYS_ADMIN here)");
        return;
    };
    assert_eq!(sk_core::ops::paths::protected_key(&s(&mnt)), Some("mount"));
    let r = delete_permanently(&[s(&mnt)], no_locks()).remove(0);
    assert_eq!(r.code, OpCode::Protected, "{r:?}");
    let r = delete_permanently(&[s(&sel)], no_locks()).remove(0);
    assert_eq!(r.code, OpCode::Error, "{r:?}");
    assert!(r.message.contains("挂载点"), "{r:?}");
    assert_eq!(std::fs::read(src.join("data.txt")).unwrap(), b"keep");
    assert!(!sel.join("own.txt").exists()); // the rest went
}

/// The command prefix that runs as root (empty: already root), or None.
#[cfg(target_os = "linux")]
fn privileged() -> Option<Vec<&'static str>> {
    // SAFETY: always succeeds
    if unsafe { libc::geteuid() } == 0 {
        return Some(Vec::new());
    }
    let sudo = std::process::Command::new("sudo")
        .args(["-n", "true"])
        .output()
        .is_ok_and(|o| o.status.success());
    sudo.then(|| vec!["sudo", "-n"])
}

/// `mount --bind src at`, undone when dropped.
#[cfg(target_os = "linux")]
struct BindMount {
    prefix: Vec<&'static str>,
    at: PathBuf,
}

#[cfg(target_os = "linux")]
impl BindMount {
    fn new(prefix: &[&'static str], src: &Path, at: &Path) -> Option<BindMount> {
        let run = |args: &[&std::ffi::OsStr]| {
            let (cmd, rest) = match prefix.split_first() {
                Some((c, r)) => (*c, r),
                None => (args[0].to_str().unwrap(), &[][..]),
            };
            let mut c = std::process::Command::new(cmd);
            c.args(rest);
            if prefix.is_empty() {
                c.args(&args[1..]);
            } else {
                c.args(args);
            }
            c.output().is_ok_and(|o| o.status.success())
        };
        run(&[
            "mount".as_ref(),
            "--bind".as_ref(),
            src.as_os_str(),
            at.as_os_str(),
        ])
        .then(|| BindMount {
            prefix: prefix.to_vec(),
            at: at.to_path_buf(),
        })
    }

    /// A small tmpfs (a device of its own, anyone may write to it) mounted at `at`.
    fn tmpfs(prefix: &[&'static str], at: &Path) -> Option<BindMount> {
        let mut c = match prefix.split_first() {
            Some((cmd, rest)) => {
                let mut c = std::process::Command::new(cmd);
                c.args(rest).arg("mount");
                c
            }
            None => std::process::Command::new("mount"),
        };
        c.args(["-t", "tmpfs", "-o", "size=4m,mode=0777", "tmpfs"])
            .arg(at);
        c.output()
            .is_ok_and(|o| o.status.success())
            .then(|| BindMount {
                prefix: prefix.to_vec(),
                at: at.to_path_buf(),
            })
    }
}

/// Two bind mounts of one file system (one device) are two top directories: an item of each
/// goes into the `.Trash-$uid` of its own (the trash found for the first is not the second's: a
/// rename into it would cross mounts and fail). Needs root, as above.
#[cfg(target_os = "linux")]
#[test]
fn two_mounts_of_one_device_have_a_trash_each() {
    let (_, _g) = setup();
    let Some(root) = privileged() else {
        eprintln!("skipped: mount needs root or passwordless sudo");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let fs = dir.path().join("fs");
    std::fs::create_dir(&fs).unwrap();
    let Some(_tmpfs) = BindMount::tmpfs(&root, &fs) else {
        eprintln!("skipped: mount -t tmpfs failed (no CAP_SYS_ADMIN here)");
        return;
    };
    let (a, b) = (fs.join("a"), fs.join("b"));
    let (m1, m2) = (dir.path().join("m1"), dir.path().join("m2"));
    for d in [&a, &b, &m1, &m2] {
        std::fs::create_dir(d).unwrap();
    }
    let (Some(_m1), Some(_m2)) = (
        BindMount::new(&root, &a, &m1),
        BindMount::new(&root, &b, &m2),
    ) else {
        eprintln!("skipped: mount --bind failed");
        return;
    };
    let (x, y) = (m1.join("x.txt"), m2.join("y.txt"));
    std::fs::write(&x, b"x").unwrap();
    std::fs::write(&y, b"y").unwrap();
    let res = trash_all(&[s(&x), s(&y)]);
    let mut trashed = Trashed(res.iter().map(|r| r.trash_ref.clone()).collect());
    assert_eq!(codes(&res), [OpCode::Ok, OpCode::Ok], "{res:?}");
    assert!(res[0].trash_ref.starts_with(&s(&m1)), "{res:?}");
    assert!(res[1].trash_ref.starts_with(&s(&m2)), "{res:?}");
    assert_eq!(codes(&restore(&trashed.0)), [OpCode::Ok, OpCode::Ok]);
    trashed.0.clear();
    assert!(x.exists() && y.exists());
}

#[cfg(target_os = "linux")]
impl Drop for BindMount {
    fn drop(&mut self) {
        let mut c = match self.prefix.split_first() {
            Some((cmd, rest)) => {
                let mut c = std::process::Command::new(cmd);
                c.args(rest).arg("umount");
                c
            }
            None => std::process::Command::new("umount"),
        };
        if !c.arg(&self.at).output().is_ok_and(|o| o.status.success()) {
            eprintln!("could not unmount {}", self.at.display());
        }
    }
}

// --------------------------------------------------------------------------- names that are not Unicode

/// `stem` with a name no `String` can hold in it: a lone UTF-16 surrogate on Windows, a byte
/// that is not UTF-8 elsewhere; then `ext`.
fn odd_os_name(stem: &str, ext: &str) -> std::ffi::OsString {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        let mut w: Vec<u16> = stem.encode_utf16().collect();
        w.push(0xDC00);
        w.extend(ext.encode_utf16());
        std::ffi::OsString::from_wide(&w)
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        let mut b = stem.as_bytes().to_vec();
        b.push(0xFF);
        b.extend_from_slice(ext.as_bytes());
        std::ffi::OsString::from_vec(b)
    }
}

/// An item whose own name and whose folder's name are not valid Unicode goes to the trash and
/// comes back under exactly those names (not U+FFFD in their place, a new folder beside).
#[test]
fn names_that_are_not_unicode_come_back_as_they_were() {
    if MAC {
        return; // macOS file systems hold UTF-8 names only
    }
    let (_, _g) = setup();
    let dir = tempfile::tempdir().unwrap();
    odd_names_round_trip(dir.path());
}

/// [`names_that_are_not_unicode_come_back_as_they_were`] in `dir` (an empty folder).
fn odd_names_round_trip(dir: &Path) {
    let folder = dir.join(odd_os_name("folder", ""));
    if std::fs::create_dir(&folder).is_err() {
        fs_caps::skip("names that are not Unicode", dir, "refused");
        return;
    }
    if without_trash(dir, "names that are not Unicode in the trash") {
        let _ = std::fs::remove_dir(&folder);
        return;
    }
    // the second one has it in its extension too, which the item keeps in the trash
    let ext = odd_os_name(".b", "");
    let files = [
        folder.join(odd_os_name("file", ".bin")),
        folder.join({
            let mut n = odd_os_name("other", "");
            n.push(&ext);
            n
        }),
    ];
    for (i, f) in files.iter().enumerate() {
        std::fs::write(f, [b'0' + i as u8]).unwrap();
    }
    let res = move_to_trash(&files, OpOptions::default());
    let mut trashed = Trashed(res.iter().map(|r| r.trash_ref.clone()).collect());
    assert!(res.iter().all(|r| r.code == OpCode::Ok), "{res:?}");
    assert!(files.iter().all(|f| !f.exists()));
    let back = restore(&res.iter().map(|r| r.trash_ref.clone()).collect::<Vec<_>>());
    trashed.0.clear();
    assert!(back.iter().all(|b| b.code == OpCode::Ok), "{back:?}");
    assert!(back[0].path.contains('\u{FFFD}'), "{back:?}"); // as shown
    for (i, f) in files.iter().enumerate() {
        assert_eq!(std::fs::read(f).unwrap(), [b'0' + i as u8]);
    }
    let beside: Vec<_> = std::fs::read_dir(dir).unwrap().collect();
    assert_eq!(beside.len(), 1, "{beside:?}");
    assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 2);
}

#[test]
fn trash_refs_keep_names_that_are_not_unicode() {
    let plain = Path::new("/home/lenny/a b.txt");
    assert_eq!(trash::ref_of(plain), "/home/lenny/a b.txt");
    assert_eq!(trash::path_of_ref("/home/lenny/a b.txt"), plain);
    let odd = PathBuf::from("x").join(odd_os_name("y", ".z"));
    let r = trash::ref_of(&odd);
    assert!(r.starts_with('\0'), "{r:?}");
    assert_eq!(trash::path_of_ref(&r), odd);
    // a malformed spelled-out reference is just a (missing) path
    assert_eq!(trash::path_of_ref("\0exact:zz"), Path::new("\0exact:zz"));
}
