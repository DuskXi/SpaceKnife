//! The file-system matrix (docs/manage.md §2.5, 「各文件系统」): moving to the trash and deleting
//! permanently on the file system that holds the temporary folder, end to end the way the backend
//! runs a basket — scanned, put into the basket, its targets executed with the backend's options —
//! with an exact result per item. CI's job `fs-matrix` runs this (and the other ops tests) with
//! the temporary folder on FAT32, exFAT, NTFS and ReFS (Windows), ext4, XFS, Btrfs, vfat, exFAT
//! and ntfs3 (Linux), APFS, HFS+, FAT32 and ExFAT (macOS).
//!
//! The case that started it: nineteen folders deleted permanently from the basket on an exFAT
//! USB disk, each reported 「操作失败：参数错误。」 and none deleted (a folder deleted without
//! POSIX semantics only goes once its handle is closed, and its children's handles were still
//! open).
//!
//! The real trash is used: on Windows the recycle bin of the volume (what the tests put there they
//! take back); on Linux and macOS only with `SPACEKNIFE_FS_MATRIX=1` (CI), as a temporary folder on
//! another volume than the home folder gets that volume's own trash, made on it
//! (`$topdir/.Trash-$uid`, `.Trashes/$uid`) — else that part says it is skipped.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use parking_lot::{Mutex, MutexGuard};
use sk_core::ops::basket::{Basket, Target};
use sk_core::ops::trash::{OpCode, OpOptions, OpResult};

mod exit_cleanup;
mod fs_caps;

const ITEMS: usize = 19;

/// One SpaceKnife home for this binary (protected-path checks look at it), removed when it
/// exits; the tests run one at a time (the recycle bin, the basket's seams).
fn setup() -> MutexGuard<'static, ()> {
    static HOME: OnceLock<PathBuf> = OnceLock::new();
    static LOCK: Mutex<()> = Mutex::new(());
    HOME.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap().keep();
        exit_cleanup::remove_at_exit(dir.clone());
        // SAFETY: set once, before the tests of this binary read the environment
        unsafe { std::env::set_var("SPACEKNIFE_HOME", dir.join("skhome")) };
        dir
    });
    LOCK.lock()
}

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Nineteen folders below `top`, each with folders and files inside (a name in Chinese, one a
/// level deeper); a read-only file in the first where the file system keeps the flag.
fn make_items(top: &Path) -> Vec<PathBuf> {
    std::fs::create_dir_all(top).unwrap();
    let read_only = fs_caps::read_only_files(top);
    (0..ITEMS)
        .map(|i| {
            let item = top.join(format!("item{i:02}"));
            for sub in ["a", "b"] {
                let d = item.join(sub).join("deeper");
                std::fs::create_dir_all(&d).unwrap();
                std::fs::write(item.join(sub).join("报告.txt"), format!("{i}{sub}")).unwrap();
                std::fs::write(item.join(sub).join("data.bin"), vec![b'd'; 100 + i]).unwrap();
                std::fs::write(d.join("x.txt"), b"x").unwrap();
            }
            if i == 0 && read_only {
                let ro = item.join("readonly.txt");
                std::fs::write(&ro, b"r").unwrap();
                let mut perm = std::fs::metadata(&ro).unwrap().permissions();
                perm.set_readonly(true);
                std::fs::set_permissions(&ro, perm).unwrap();
            }
            if i == 1 && cfg!(windows) {
                // a folder with the read-only attribute (Explorer marks customised folders so;
                // FAT refuses to delete it as it is)
                let ro = item.join("a");
                let mut perm = std::fs::metadata(&ro).unwrap().permissions();
                perm.set_readonly(true);
                std::fs::set_permissions(&ro, perm).unwrap();
            }
            item
        })
        .collect()
}

/// Every file below `top` with its contents, by its path below `top`.
fn contents(top: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![top.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if e.file_type().unwrap().is_dir() {
                stack.push(p);
            } else {
                let rel = p.strip_prefix(top).unwrap().to_string_lossy().into_owned();
                out.insert(rel, std::fs::read(&p).unwrap());
            }
        }
    }
    out
}

/// `top` scanned and each of `items` put into a basket, as the interface does; the targets the
/// backend would execute, the basket, and the options it gives the operation.
struct Queued {
    basket: Basket,
    targets: Vec<Target>,
    sizes: HashMap<String, i64>,
    longest: HashMap<String, usize>,
    bytes: HashMap<String, i64>,
    roots: HashMap<String, String>,
}

fn queue(top: &Path, items: &[PathBuf]) -> Queued {
    let (scanner, _) =
        sk_core::scan::scan_path(top, sk_core::scan::ScanOptions::default()).unwrap();
    let store = scanner.store().clone();
    let basket = Basket::new(None);
    {
        let nodes = store.read();
        let ids: Vec<i64> = items
            .iter()
            .map(|p| i64::from(nodes.find_path(p).expect("scanned")))
            .collect();
        let r = basket.add(&nodes, "s1", &ids, None).unwrap();
        assert!(
            r.refused.is_empty() && r.added.len() == items.len(),
            "{r:?}"
        );
    }
    let targets = basket.targets(None);
    assert_eq!(targets.len(), items.len());
    Queued {
        sizes: targets.iter().map(|t| (t.path.clone(), t.size)).collect(),
        longest: targets
            .iter()
            .filter_map(|t| t.longest.map(|l| (t.path.clone(), l)))
            .collect(),
        bytes: targets
            .iter()
            .filter_map(|t| t.bytes.map(|b| (t.path.clone(), b)))
            .collect(),
        roots: targets
            .iter()
            .map(|t| (t.path.clone(), t.root.clone()))
            .collect(),
        targets,
        basket,
    }
}

impl Queued {
    fn paths(&self) -> Vec<PathBuf> {
        self.targets.iter().map(|t| t.os_path.clone()).collect()
    }

    /// Execute it as the backend does (`ops_service`): a progress callback, a cancel that never
    /// says stop, the scan's sizes for a move to the trash.
    fn run(&self, trash: bool) -> Vec<OpResult> {
        let mut seen = 0usize;
        let mut progress = |n: usize, _: usize, _: &OpResult| seen = n;
        let never = || false;
        let opts = OpOptions {
            progress: Some(&mut progress),
            cancel: Some(&never),
            sizes: trash.then_some(&self.sizes),
            longest: trash.then_some(&self.longest),
            bytes: trash.then_some(&self.bytes),
            roots: Some(&self.roots),
            ..OpOptions::default()
        };
        let paths = self.paths();
        let results = if trash {
            sk_core::ops::move_to_trash(&paths, opts)
        } else {
            sk_core::ops::delete_permanently(&paths, opts)
        };
        assert_eq!(seen, paths.len(), "every item reported");
        assert_eq!(results.len(), paths.len());
        for (r, t) in results.iter().zip(&self.targets) {
            assert_eq!(r.path, t.path);
        }
        results
    }
}

/// The user's case: nineteen folders deleted permanently from the basket, every one of them
/// reported deleted, nothing left, the basket empty afterwards.
#[test]
fn nineteen_folders_deleted_permanently() {
    let _g = setup();
    let dir = tempfile::tempdir().unwrap();
    let top = dir.path().join("scan");
    let items = make_items(&top);
    std::fs::write(top.join("keep.txt"), b"k").unwrap();
    let q = queue(&top, &items);
    let results = q.run(false);
    let got: Vec<(OpCode, &str)> = results
        .iter()
        .map(|r| (r.code, r.message.as_str()))
        .collect();
    assert_eq!(
        got,
        vec![(OpCode::Ok, "已永久删除"); ITEMS],
        "{} {results:#?}",
        fs_caps::fs_name(dir.path())
    );
    assert!(items.iter().all(|p| std::fs::symlink_metadata(p).is_err()));
    let left: Vec<_> = std::fs::read_dir(&top).unwrap().flatten().collect();
    assert_eq!(left.len(), 1, "{left:?}"); // keep.txt
    q.basket.apply_results(q.targets.iter().zip(&results));
    assert!(q.basket.is_empty(), "{:?}", q.basket.items());
}

/// Whether moving to the real trash may be tried here: always on Windows (the tests take back
/// what they put into the recycle bin), elsewhere only when the matrix asks for it.
fn real_trash_allowed() -> bool {
    cfg!(windows) || std::env::var("SPACEKNIFE_FS_MATRIX").is_ok_and(|v| v == "1")
}

/// Where a trash reference says the item went: the folder its trash entry is in.
fn landed(r: &OpResult) -> PathBuf {
    if cfg!(target_os = "macos") {
        let v: serde_json::Value = serde_json::from_str(&r.trash_ref).unwrap();
        PathBuf::from(v["trashed"].as_str().unwrap())
    } else {
        sk_core::ops::trash::path_of_ref(&r.trash_ref)
    }
}

/// Whether `p` lies in the folder `trash` (the trash's own folder: `$Recycle.Bin\<SID>`, a
/// freedesktop trash holding `info/` and `files/`, `.Trashes/<uid>`), as the file system compares
/// names.
fn in_trash(p: &Path, trash: &str) -> bool {
    let norm = |x: &str| {
        let x = x.trim_end_matches(['\\', '/']).to_string();
        if cfg!(windows) || cfg!(target_os = "macos") {
            x.to_lowercase()
        } else {
            x
        }
    };
    let (p, trash) = (norm(&s(p)), norm(trash));
    p.starts_with(&trash) && p[trash.len()..].starts_with(['\\', '/'])
}

/// Nineteen folders moved to the trash from the basket and put back: each lands in the trash of
/// its own volume (where `trash_status` says), and comes back whole. Where the volume has no
/// trash, each is refused with the reason the user is shown (which offers a permanent deletion)
/// and nothing changes.
#[test]
fn nineteen_folders_to_the_trash_and_back() {
    let _g = setup();
    let dir = tempfile::tempdir().unwrap();
    let top = dir.path().join("scan");
    let items = make_items(&top);
    let _cleanup = Cleanup(top.clone());
    let before = contents(&top);
    let status = sk_core::ops::trash_status(&s(&top), false);
    if status.available && !real_trash_allowed() {
        fs_caps::skip(
            "nineteen_folders_to_the_trash_and_back",
            dir.path(),
            "the real trash is used only with SPACEKNIFE_FS_MATRIX=1 here",
        );
        return;
    }
    let q = queue(&top, &items);
    let results = q.run(true);
    if !status.available {
        for r in &results {
            assert_eq!(
                (r.code, r.message.as_str()),
                (OpCode::NoTrash, status.reason.as_str()),
                "{r:?}"
            );
            assert!(r.message.contains("永久删除"), "{r:?}");
        }
        assert_eq!(contents(&top), before);
        fs_caps::skip(
            "nineteen_folders_to_the_trash_and_back",
            dir.path(),
            &format!("no trash here: {}", status.reason),
        );
        return;
    }
    let refs: Vec<String> = results.iter().map(|r| r.trash_ref.clone()).collect();
    let back = || sk_core::ops::restore(&refs);
    let codes: Vec<OpCode> = results.iter().map(|r| r.code).collect();
    if codes != vec![OpCode::Ok; ITEMS] {
        back();
        panic!("{} {results:#?}", fs_caps::fs_name(dir.path()));
    }
    assert!(items.iter().all(|p| std::fs::symlink_metadata(p).is_err()));
    for r in &results {
        let at = landed(r);
        assert!(
            in_trash(&at, &status.location),
            "{} landed in {}, not in {}",
            r.path,
            at.display(),
            status.location
        );
    }
    eprintln!(
        "{}: trashed into {}",
        fs_caps::fs_name(dir.path()),
        status.location
    );
    let restored = back();
    assert!(
        restored.iter().all(|r| r.code == OpCode::Ok),
        "{restored:#?}"
    );
    assert_eq!(contents(&top), before);
}

/// Deletes the test tree when dropped, read-only entries too (what the temporary folder's own
/// removal does not take on every file system).
struct Cleanup(PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        if std::fs::symlink_metadata(&self.0).is_ok() {
            sk_core::ops::delete_permanently(&[&self.0], OpOptions::default());
        }
    }
}

/// Windows: a file deleted while another program holds it open sharing its deletion (an
/// antivirus scanner, a preview, a sync program). Where the file system deletes with POSIX
/// semantics (NTFS) its name goes at once and so does the folder; elsewhere (FAT, exFAT, network
/// shares) the name stays until that program closes it: the folder is asked again for a moment,
/// and goes once it is closed — or, still held, says so (not a bare 「目录不是空的」).
#[cfg(windows)]
#[test]
fn a_file_held_open_by_another_program_sharing_its_deletion() {
    use std::os::windows::fs::OpenOptionsExt;
    let _g = setup();
    let dir = tempfile::tempdir().unwrap();
    let held_open = |p: &Path| {
        std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0x7) // read, write, delete
            .open(p)
            .unwrap()
    };
    let no_locks = || OpOptions {
        check_locks: false,
        ..OpOptions::default()
    };
    // closed a little later: the folder goes
    let sel = dir.path().join("sel");
    std::fs::create_dir_all(sel.join("sub")).unwrap();
    std::fs::write(sel.join("sub").join("scanned.bin"), b"s").unwrap();
    let h = held_open(&sel.join("sub").join("scanned.bin"));
    let closer = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(150));
        drop(h);
    });
    let r = sk_core::ops::delete_permanently(&[s(&sel)], no_locks()).remove(0);
    closer.join().unwrap();
    assert_eq!(
        (r.code, r.message.as_str()),
        (OpCode::Ok, "已永久删除"),
        "{r:?}"
    );
    assert!(std::fs::symlink_metadata(&sel).is_err());
    // held all along: gone (POSIX semantics) or told why it stays
    let sel = dir.path().join("sel2");
    std::fs::create_dir_all(&sel).unwrap();
    std::fs::write(sel.join("held.bin"), b"h").unwrap();
    let h = held_open(&sel.join("held.bin"));
    let r = sk_core::ops::delete_permanently(&[s(&sel)], no_locks()).remove(0);
    drop(h);
    if r.ok {
        assert!(std::fs::symlink_metadata(&sel).is_err());
    } else {
        let pending = "里面的内容已删除，但还有程序（如杀毒软件、预览或同步程序）打开着其中的文件，这个文件系统要等它们关闭后才真正删掉，文件夹因此没能删除；稍后再删一次即可";
        assert_eq!(
            (r.code, r.message.as_str()),
            (
                OpCode::Error,
                format!("部分内容未能删除：{pending}").as_str()
            ),
            "{r:?}"
        );
        eprintln!(
            "{}: deletions wait for the files to be closed (no POSIX semantics)",
            fs_caps::fs_name(dir.path())
        );
        // closed now: it goes
        let r = sk_core::ops::delete_permanently(&[s(&sel)], no_locks()).remove(0);
        assert!(r.ok, "{r:?}");
    }
}

/// A file another program holds open without sharing its deletion: the item is reported held,
/// partly done (the rest of it went), with the reason in words.
#[cfg(windows)]
#[test]
fn a_file_held_open_without_sharing_stops_only_its_folders() {
    use std::os::windows::fs::OpenOptionsExt;
    let _g = setup();
    let dir = tempfile::tempdir().unwrap();
    let sel = dir.path().join("sel");
    std::fs::create_dir_all(sel.join("busy")).unwrap();
    std::fs::create_dir_all(sel.join("free")).unwrap();
    std::fs::write(sel.join("busy").join("doc.docx"), b"d").unwrap();
    std::fs::write(sel.join("free").join("f.txt"), b"f").unwrap();
    let h = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0x1)
        .open(sel.join("busy").join("doc.docx"))
        .unwrap();
    let r = sk_core::ops::delete_permanently(
        &[s(&sel)],
        OpOptions {
            check_locks: false,
            ..OpOptions::default()
        },
    )
    .remove(0);
    drop(h);
    assert_eq!(
        (r.code, r.message.as_str()),
        (
            OpCode::Error,
            "部分内容未能删除：正被其他程序使用，需先关闭"
        ),
        "{r:?}"
    );
    assert!(!sel.join("free").exists() && sel.join("busy").join("doc.docx").exists());
}

/// Linux, Btrfs: a subvolume inside a deleted folder (another device number on the same mount,
/// a snapshot often) is not entered, and the item says it is a subvolume — not a mount point;
/// the rest of the folder goes.
#[cfg(target_os = "linux")]
#[test]
fn a_btrfs_subvolume_inside_is_named_as_one() {
    let _g = setup();
    let dir = tempfile::tempdir().unwrap();
    let sel = dir.path().join("sel");
    std::fs::create_dir_all(sel.join("plain")).unwrap();
    std::fs::write(sel.join("plain").join("f.txt"), b"f").unwrap();
    let sub = sel.join("snap");
    let made = std::process::Command::new("btrfs")
        .args(["subvolume", "create"])
        .arg(&sub)
        .output()
        .is_ok_and(|o| o.status.success());
    if !made {
        let why = "no Btrfs subvolume can be made here (not Btrfs, or no btrfs tool)";
        fs_caps::skip("a_btrfs_subvolume_inside_is_named_as_one", dir.path(), why);
        return;
    }
    std::fs::write(sub.join("in.txt"), b"i").unwrap();
    let r = sk_core::ops::delete_permanently(
        &[s(&sel)],
        OpOptions {
            check_locks: false,
            ..OpOptions::default()
        },
    )
    .remove(0);
    let want = "部分内容未能删除：其中有一个 Btrfs 子卷（例如快照），未进入；它要用 btrfs subvolume delete 删除";
    assert_eq!((r.code, r.message.as_str()), (OpCode::Error, want), "{r:?}");
    assert!(sub.join("in.txt").exists() && !sel.join("plain").exists());
    // (an empty subvolume goes as a folder does, for its owner, since Linux 4.18)
    let _ = std::fs::remove_file(sub.join("in.txt"));
    if std::fs::remove_dir(&sub).is_err() {
        let _ = std::process::Command::new("sudo")
            .args(["-n", "btrfs", "subvolume", "delete"])
            .arg(&sub)
            .output();
    }
}
