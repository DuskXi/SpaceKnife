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

/// One SpaceKnife home (and on Linux, one XDG data home) for this binary; tests that change the
/// process-wide seams run one at a time.
fn setup() -> (&'static Path, MutexGuard<'static, ()>) {
    static BASE: OnceLock<PathBuf> = OnceLock::new();
    static LOCK: Mutex<()> = Mutex::new(());
    let base = BASE.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap().keep();
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

// --------------------------------------------------------------------------- round trips

#[test]
fn trash_and_restore() {
    if MAC {
        return; // macOS trash: implemented best effort, not tested here
    }
    let (base, _g) = setup();
    let dir = tempfile::tempdir().unwrap();
    let (f, d) = make(dir.path());
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
        for (r, (original, size)) in res.iter().zip([(&f, 5), (&d, 10)]) {
            let info = Path::new(&r.trash_ref);
            assert!(
                info.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("$I")
            );
            let bin = info
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .file_name()
                .unwrap();
            assert_eq!(bin.to_string_lossy().to_lowercase(), "$recycle.bin");
            let (got_size, when, path) = parse_recycle_info(&std::fs::read(info).unwrap()).unwrap();
            assert!(same(&path, &s(original)) && got_size == size && when > 1.7e9);
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
    assert!(same(&back[0].path, &s(&f)) && same(&back[1].path, &s(&d)));
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
    let handle = held_open(&held);
    let res = trash_all(&paths);
    let _trashed = Trashed(res.iter().map(|r| r.trash_ref.clone()).collect());
    assert_eq!(
        codes(&res),
        [OpCode::Missing, OpCode::Protected, OpCode::Locked]
    );
    assert!(res[2].message.contains("使用"));
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
    let dir = tempfile::tempdir().unwrap();
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

    const LP: &str = "\\\\?\\";

    fn deep_tree(top: &Path) -> PathBuf {
        std::fs::create_dir(top).unwrap();
        let mut p = s(top);
        while p.len() < 300 {
            p = format!("{p}\\{}", "n".repeat(40));
            std::fs::create_dir(format!("{LP}{p}")).unwrap();
        }
        let leaf = format!("{p}\\leaf.txt");
        std::fs::write(format!("{LP}{leaf}"), b"x").unwrap();
        PathBuf::from(leaf)
    }

    #[test]
    fn no_trash_when_paths_get_too_long() {
        let (_, _g) = setup();
        let dir = tempfile::tempdir().unwrap();
        let top = dir.path().join("deep");
        let leaf = deep_tree(&top);
        let r = trash_all(&[s(&top)]).remove(0);
        assert_eq!((r.code, r.message), (OpCode::NoTrash, text("too_long")));
        assert!(Path::new(&format!("{LP}{}", s(&leaf))).exists());
        std::fs::remove_dir_all(format!("{LP}{}", s(&top))).unwrap();
    }

    /// With the checks out of the way, the shell itself may want to destroy a tree that cannot
    /// fit the bin (paths too long): the sink refuses, nothing is lost, the next item still goes.
    /// (Newer Windows builds recycle such trees fine: then it must come back whole.)
    #[test]
    fn engine_guard_refuses_destruction() {
        let (_, _g) = setup();
        let _seam = Seam;
        let dir = tempfile::tempdir().unwrap();
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
        assert!(Path::new(&format!("{LP}{}", s(&leaf))).exists());
        assert_eq!(
            codes(&restore(std::slice::from_ref(&res[1].trash_ref))),
            [OpCode::Ok]
        );
        trashed.0.clear();
        assert_eq!(std::fs::read(&other).unwrap(), b"o");
        std::fs::remove_dir_all(format!("{LP}{}", s(&top))).unwrap();
    }

    #[test]
    fn engine_reports_a_held_file() {
        let (_, _g) = setup();
        let dir = tempfile::tempdir().unwrap();
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

    /// The 8.3 short name of an existing path (cmd's `%~sI`), None when the volume has none.
    fn short_name(p: &Path) -> Option<String> {
        use std::os::windows::process::CommandExt;
        // raw: cmd parses its own command line, and Rust's quoting would escape the quotes
        let out = std::process::Command::new("cmd")
            .raw_arg(format!(
                "/d /c for %I in (\"{}\") do @echo %~sI",
                p.display()
            ))
            .output()
            .ok()?;
        let short = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (short.contains('~') && Path::new(&short).exists()).then_some(short)
    }

    /// Asked through a short name (CI's temp folder is `C:\Users\RUNNER~1\…`), reported by the
    /// shell under the long one: still matched to the request.
    #[test]
    fn trash_through_a_short_name() {
        let (_, _g) = setup();
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("a folder with a long name");
        std::fs::create_dir(&folder).unwrap();
        let file = folder.join("some long file name.txt");
        std::fs::write(&file, b"x").unwrap();
        let Some(short) = short_name(&file) else {
            return; // no 8.3 names on this volume
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
    #[cfg(windows)]
    {
        let ok = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok);
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).unwrap();
}

#[test]
fn delete_permanently_trees_links_files() {
    let (_, _g) = setup();
    let dir = tempfile::tempdir().unwrap();
    let outside = dir.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("keep.txt"), b"k").unwrap();
    let tree = dir.path().join("tree");
    std::fs::create_dir_all(tree.join("a b").join("深")).unwrap();
    std::fs::write(tree.join("a b").join("深").join("x.txt"), b"x").unwrap();
    let ro = tree.join("readonly.txt");
    std::fs::write(&ro, b"r").unwrap();
    let mut perm = std::fs::metadata(&ro).unwrap().permissions();
    perm.set_readonly(true);
    std::fs::set_permissions(&ro, perm).unwrap();
    make_link(&outside, &tree.join("a b").join("link")); // inside the tree: removed, never followed
    let top_link = dir.path().join("top-link");
    make_link(&outside, &top_link);
    let single = dir.path().join("single.bin");
    std::fs::write(&single, b"s").unwrap();
    let mut seen: Vec<OpCode> = Vec::new();
    let mut progress = |_: usize, _: usize, r: &OpResult| seen.push(r.code);
    let res = delete_permanently(
        &[s(&tree), s(&top_link), s(&single)],
        OpOptions {
            progress: Some(&mut progress),
            ..OpOptions::default()
        },
    );
    let got: Vec<(OpCode, String)> = res.iter().map(|r| (r.code, r.message.clone())).collect();
    assert_eq!(got, vec![(OpCode::Ok, text("deleted")); 3]);
    assert_eq!(seen, [OpCode::Ok; 3]);
    assert!(!tree.exists() && std::fs::symlink_metadata(&top_link).is_err() && !single.exists());
    assert_eq!(std::fs::read(outside.join("keep.txt")).unwrap(), b"k");
}

#[test]
fn delete_permanently_cancel() {
    let (_, _g) = setup();
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
