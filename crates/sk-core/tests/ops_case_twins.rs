//! Case-only siblings and other names a path cannot tell apart, on the deletion side (port of the
//! "deleting" part of tests/test_case_twins.py, docs/manage.md §2.5).
//!
//! Windows opens names case-insensitively, so where a folder stores names case-sensitively
//! (WinBtrfs, NTFS names in the POSIX namespace) the path of `a` can open its sibling `A`. A
//! fake stored-name lookup (`ops::names::seams::FINAL_NAME`) stands in for that, so these tests
//! run on every system; folders that really hold case twins are made where the file system can
//! (Linux, a case-sensitive NTFS folder), and those tests skip elsewhere.
//!
//! Only files these tests create in temporary folders are touched; the home trash on Linux is a
//! temporary folder (`XDG_DATA_HOME`), and nothing here is expected to reach the recycle bin.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use parking_lot::{Mutex, MutexGuard};
use sk_core::ops::basket::Basket;
use sk_core::ops::names::seams as name_seams;
use sk_core::ops::trash::{self, OpCode, OpOptions, OpResult, delete_permanently, move_to_trash};
use sk_store::testing::{Spec, build_with, d, f};
use sk_store::{NodeId, Store};

const WIN: bool = cfg!(windows);

mod exit_cleanup;

/// One SpaceKnife home (and on Linux one XDG data home) for this binary, removed when it exits;
/// the tests change a process-wide seam, so they run one at a time.
fn setup() -> MutexGuard<'static, ()> {
    static BASE: OnceLock<PathBuf> = OnceLock::new();
    static LOCK: Mutex<()> = Mutex::new(());
    BASE.get_or_init(|| {
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
    let g = LOCK.lock();
    *name_seams::FINAL_NAME.lock() = None;
    g
}

/// The stored names a test pretends the file system has (unset again when dropped).
struct Stored;

impl Stored {
    fn set(map: HashMap<String, String>) -> Stored {
        let map = Arc::new(map);
        *name_seams::FINAL_NAME.lock() = Some(Arc::new(move |p: &str| map.get(p).cloned()));
        Stored
    }
}

impl Drop for Stored {
    fn drop(&mut self) {
        *name_seams::FINAL_NAME.lock() = None;
    }
}

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// The source texts (the tests run in the source language).
fn text(key: &str) -> &'static str {
    match key {
        "ambiguous_name" => "另一项与它只差大小写，按名字会打开另一项；没有改动任何东西",
        "trailing_name" => {
            "它或上级文件夹的名字以点或空格结尾，Windows 按路径找到的不是它；没有改动任何东西"
        }
        "basket_trailing" => "它或上级文件夹的名字以点或空格结尾，Windows 按路径找到的不是它",
        "dup_name" => "它或上级文件夹与同一文件夹里的另一项名字完全相同，按路径找到的可能是另一项",
        "bad_name" => "它或上级文件夹的名字里有路径不能有的字符，没有路径能找到它",
        _ => unreachable!(),
    }
}

fn opts(roots: &HashMap<String, String>) -> OpOptions<'_> {
    OpOptions {
        roots: Some(roots),
        check_locks: false,
        ..OpOptions::default()
    }
}

fn one(res: Vec<OpResult>) -> OpResult {
    assert_eq!(res.len(), 1, "{res:?}");
    res.into_iter().next().unwrap()
}

/// Makes `folder` hold names that differ only in case, or says it cannot (the test then skips).
/// On Windows it becomes a case-sensitive NTFS folder (fsutil's exit code says nothing, so the
/// folder is tried); elsewhere the file system decides (APFS folds case by default).
fn case_sensitive(folder: &Path) -> bool {
    std::fs::create_dir_all(folder).unwrap();
    if WIN {
        let _ = std::process::Command::new("fsutil")
            .args(["file", "setCaseSensitiveInfo"])
            .arg(folder)
            .arg("enable")
            .output();
    }
    std::fs::create_dir(folder.join("probe")).unwrap();
    let twin = std::fs::create_dir(folder.join("PROBE")).is_ok();
    std::fs::remove_dir(folder.join("probe")).unwrap();
    if twin {
        std::fs::remove_dir(folder.join("PROBE")).unwrap();
    } else {
        eprintln!(
            "skipped: {} cannot hold names that differ only in case",
            folder.display()
        );
    }
    twin
}

/// A scan root with folders `a` and `A` side by side, each holding a file.
fn twins(tmp: &Path) -> Option<(PathBuf, PathBuf, PathBuf)> {
    let root = tmp.join("root");
    if !case_sensitive(&root) {
        return None;
    }
    let (a, big_a) = (root.join("a"), root.join("A"));
    std::fs::create_dir(&a).unwrap();
    std::fs::create_dir(&big_a).unwrap();
    std::fs::write(a.join("f"), b"a").unwrap();
    std::fs::write(big_a.join("g"), b"A").unwrap();
    Some((root, a, big_a))
}

// --------------------------------------------------------------------------- deleting by path

#[test]
fn a_path_that_opens_another_entry_is_never_deleted() {
    let _g = setup();
    let tmp = tempfile::tempdir().unwrap();
    let Some((root, a, big_a)) = twins(tmp.path()) else {
        return;
    };
    // as on WinBtrfs: the path of `a` opens `A`
    let _stored = Stored::set(HashMap::from([(s(&a), "\\somewhere\\root\\A".to_string())]));
    let roots = HashMap::from([(s(&a), s(&root))]);
    let r = one(delete_permanently(&[s(&a)], opts(&roots)));
    assert_eq!(
        (r.code, r.message.as_str(), r.ok),
        (OpCode::AmbiguousName, text("ambiguous_name"), false)
    );
    let r = one(move_to_trash(&[s(&a)], opts(&roots)));
    assert_eq!(r.code, OpCode::AmbiguousName, "{r:?}");
    assert!(a.join("f").exists() && big_a.join("g").exists());
}

/// A stored name elsewhere (another name of a hard link, a redirect) with no name on the way
/// that another entry has but for case: the path names the item. With such a pair on the way it
/// may not: `A` can be a symlink that won the name of `a` and led elsewhere.
#[test]
fn other_stored_names_do_not_stop_a_deletion() {
    let _g = setup();
    let tmp = tempfile::tempdir().unwrap();
    let Some((root, a, big_a)) = twins(tmp.path()) else {
        return;
    };
    let plain = root.join("plain");
    std::fs::create_dir(&plain).unwrap();
    std::fs::write(plain.join("f"), b"p").unwrap();
    let _stored = Stored::set(HashMap::from([
        (s(&plain), "\\elsewhere\\other".to_string()),
        (s(&a.join("f")), "\\elsewhere\\y\\f".to_string()),
    ]));
    let af = s(&a.join("f"));
    let r = one(delete_permanently(
        std::slice::from_ref(&af),
        opts(&HashMap::from([(af.clone(), s(&root))])),
    ));
    assert_eq!(r.code, OpCode::AmbiguousName, "{r:?}");
    assert!(a.join("f").exists());
    let r = one(delete_permanently(
        &[s(&plain)],
        opts(&HashMap::from([(s(&plain), s(&root))])),
    ));
    assert!(r.ok, "{r:?}");
    assert!(!plain.exists() && a.join("f").exists() && big_a.join("g").exists());
}

#[test]
fn case_twins_on_disk_each_delete_their_own() {
    let _g = setup();
    let tmp = tempfile::tempdir().unwrap();
    let Some((root, a, big_a)) = twins(tmp.path()) else {
        return;
    };
    let roots = HashMap::from([(s(&a), s(&root)), (s(&big_a), s(&root))]);
    let r = one(delete_permanently(&[s(&a)], opts(&roots)));
    assert!(r.ok, "{r:?}");
    assert!(!a.exists() && big_a.join("g").exists());
    let r = one(delete_permanently(&[s(&big_a)], opts(&roots)));
    assert!(r.ok && !big_a.exists(), "{r:?}");
}

/// The scan saw `Photos`, the folder is `photos` now: no entry has the old name, so the path
/// still opens the item (Windows) and is not refused. A stored name somewhere else altogether
/// (another name of a hard link, a redirect) is no sibling either.
#[test]
fn a_name_whose_case_changed_since_the_scan_still_reaches_the_item() {
    let _g = setup();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    std::fs::create_dir_all(root.join("photos")).unwrap();
    std::fs::create_dir(root.join("d1")).unwrap();
    std::fs::create_dir(root.join("d2")).unwrap();
    std::fs::write(root.join("d2").join("file.txt"), b"f").unwrap();
    let stale = s(&root.join("Photos"));
    let linked = s(&root.join("d2").join("file.txt"));
    let mut stored = HashMap::from([
        (s(&root), "\\x\\root".to_string()),
        (stale.clone(), "\\x\\root\\photos".to_string()),
        (linked.clone(), "\\x\\root\\d1\\File.txt".to_string()),
    ]);
    let guard = Stored::set(stored.clone());
    let mut names = HashMap::new();
    assert!(!trash::reaches_another(&stale, &s(&root), &mut names));
    assert!(!trash::reaches_another(&linked, &s(&root), &mut names));
    drop(guard);
    // with both names in the folder, the stored one is another entry's, or a link that one is
    // led elsewhere
    std::fs::create_dir(root.join("x")).unwrap();
    let cs = root.join("x").join("cs");
    if !case_sensitive(&cs) {
        return;
    }
    std::fs::create_dir(cs.join("Photos")).unwrap();
    std::fs::create_dir(cs.join("photos")).unwrap();
    let twin = s(&cs.join("Photos"));
    let below = s(&cs.join("Photos").join("f"));
    stored.insert(twin.clone(), "\\x\\root\\x\\cs\\photos".to_string());
    stored.insert(below.clone(), "\\x\\root\\x\\f".to_string());
    let guard = Stored::set(stored.clone());
    let mut names = HashMap::new();
    assert!(trash::reaches_another(&twin, &s(&root), &mut names));
    assert!(trash::reaches_another(&below, &s(&root), &mut names));
    drop(guard);
    // what it is stored as: nothing to look at
    stored.insert(twin.clone(), "\\x\\root\\x\\cs\\Photos".to_string());
    let _guard = Stored::set(stored);
    let mut names = HashMap::new();
    assert!(!trash::reaches_another(&twin, &s(&root), &mut names));
}

/// A folder on the way that cannot be listed counts as holding a pair.
#[test]
fn a_folder_that_cannot_be_listed_counts_as_a_pair() {
    let _g = setup();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    // the stored name names another item, and the folder `gone` on the way is not there to list
    let p = s(&root.join("gone").join("f"));
    let _stored = Stored::set(HashMap::from([(p.clone(), "\\x\\other\\f".to_string())]));
    let mut names = HashMap::new();
    assert!(trash::reaches_another(&p, &s(&root), &mut names));
    // listed, with no other name like it on the way: the path names the item
    std::fs::create_dir(root.join("gone")).unwrap();
    assert!(!trash::reaches_another(&p, &s(&root), &mut names));
}

#[cfg(windows)]
#[test]
fn windows_a_stale_case_is_deleted_by_its_path() {
    let _g = setup();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    std::fs::create_dir_all(root.join("photos")).unwrap();
    std::fs::write(root.join("photos").join("big.mov"), b"m").unwrap();
    let stale = s(&root.join("Photos").join("big.mov"));
    let r = one(delete_permanently(
        std::slice::from_ref(&stale),
        opts(&HashMap::from([(stale.clone(), s(&root))])),
    ));
    assert!(r.ok, "{r:?}");
    assert!(!root.join("photos").join("big.mov").exists());
}

/// Win32 trims a name's trailing dot or space: by its path the folder `odd.` is `odd`. The scan
/// lists both as they are; deleting or trashing `odd.` would touch `odd`, so it is refused, as is
/// adding such an item to the basket.
#[cfg(windows)]
#[test]
fn windows_names_ending_in_a_dot_are_never_deleted_by_their_path() {
    let _g = setup();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    std::fs::create_dir(&root).unwrap();
    let long_root = format!("\\\\?\\{}", s(&root));
    for name in ["odd", "odd."] {
        std::fs::create_dir(format!("{long_root}\\{name}")).unwrap();
        std::fs::write(format!("{long_root}\\{name}\\in.txt"), name.as_bytes()).unwrap();
    }
    let target = s(&root.join("odd."));
    let roots = HashMap::from([(target.clone(), s(&root))]);
    for (op, r) in [
        (
            "delete",
            one(delete_permanently(
                std::slice::from_ref(&target),
                opts(&roots),
            )),
        ),
        (
            "trash",
            one(move_to_trash(std::slice::from_ref(&target), opts(&roots))),
        ),
    ] {
        assert_eq!(
            (r.code, r.message.as_str()),
            (OpCode::AmbiguousName, text("trailing_name")),
            "{op}"
        );
    }
    let mut listed: Vec<String> = std::fs::read_dir(&long_root)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    listed.sort();
    assert_eq!(listed, ["odd", "odd."]);
    assert!(trash::win32_trims(&s(&root.join("x.").join("f"))));
    assert!(trash::win32_trims(&s(&root.join("sp "))));
    assert!(!trash::win32_trims(&s(&root.join("a.b").join("c"))));
    let store = build_with(&d([("odd.", d([])), ("odd", d([]))]), &s(&root), 1, &[]);
    let (dot, plain) = (
        child(&store, root_of(&store), "odd."),
        child(&store, root_of(&store), "odd"),
    );
    let res = basket()
        .add(&store.read(), "s1", &[dot as i64, plain as i64], None)
        .unwrap();
    let reasons: Vec<&str> = res.refused.iter().map(|r| r.reason.as_str()).collect();
    assert_eq!(reasons, [text("basket_trailing")]);
    assert_eq!(res.added.len(), 1);
    std::fs::remove_dir_all(format!("{long_root}\\odd.")).unwrap();
}

// --------------------------------------------------------------------------- the basket

fn store_root() -> &'static str {
    if WIN { "C:\\r" } else { "/r" }
}

fn basket() -> Basket {
    Basket::new(Some(Box::new(|_: &str| "VOL".to_string())))
}

fn root_of(store: &Store) -> NodeId {
    store.read().roots().next().unwrap()
}

/// The children of `folder` named `name`, in listing order.
fn children(store: &Store, folder: NodeId, name: &str) -> Vec<NodeId> {
    let r = store.read();
    r.children(folder)
        .filter(|&c| r.name_bytes(c) == name.as_bytes())
        .collect()
}

fn child(store: &Store, folder: NodeId, name: &str) -> NodeId {
    children(store, folder, name)[0]
}

fn path_of(store: &Store, node: NodeId) -> String {
    store.read().path(node).to_string_lossy().into_owned()
}

/// The root holds folders `a` and `A`, `b` alone, two entries named `dup`, and `ab` beside `AB`;
/// `a` and `A` each hold a file `f` (sizes 1 and 2).
fn twin_store() -> Store {
    let spec: Spec = d([
        ("a", d([("f", f(1))])),
        ("A", d([("f", f(2))])),
        ("b", d([])),
        ("dup", d([])),
        ("dup", d([])),
        ("ab", d([])),
        ("AB", d([])),
    ]);
    build_with(&spec, store_root(), 1, &[])
}

#[test]
fn names_with_a_separator_cannot_go_into_the_basket() {
    let _g = setup();
    let store = build_with(
        &d([("ok", d([])), ("x/y", d([("f", f(1))])), ("x\\y", d([]))]),
        store_root(),
        1,
        &[],
    );
    let top = root_of(&store);
    let (ok, slash, back) = (
        child(&store, top, "ok"),
        child(&store, top, "x/y"),
        child(&store, top, "x\\y"),
    );
    let inner = child(&store, slash, "f");
    let res = basket()
        .add(
            &store.read(),
            "s1",
            &[ok as i64, slash as i64, back as i64, inner as i64],
            None,
        )
        .unwrap();
    let refused: std::collections::BTreeSet<String> =
        res.refused.iter().map(|r| r.path.clone()).collect();
    // the name, and anything below it
    let mut bad: std::collections::BTreeSet<String> =
        [path_of(&store, slash), path_of(&store, inner)].into();
    if WIN {
        bad.insert(path_of(&store, back)); // WinBtrfs shows Linux names with a backslash as they are
    }
    assert_eq!(refused, bad);
    assert!(res.refused.iter().all(|r| r.reason == text("bad_name")));
    assert_eq!(res.added.len(), 4 - bad.len());
}

/// The scan lists each of two entries of the very same name (WinBtrfs shows invalid UTF-8 as
/// U+FFFD) with its own contents; a path opens one of them, so neither is taken, nor anything
/// below them.
#[test]
fn names_held_twice_cannot_go_into_the_basket() {
    let _g = setup();
    let store = build_with(
        &d([
            ("n\u{FFFD}", d([("f", f(1))])),
            ("n\u{FFFD}", d([])),
            ("ok", d([])),
        ]),
        store_root(),
        1,
        &[],
    );
    let top = root_of(&store);
    let same = children(&store, top, "n\u{FFFD}");
    let inner = child(&store, same[0], "f");
    let ok = child(&store, top, "ok");
    let res = basket()
        .add(
            &store.read(),
            "s1",
            &[same[0] as i64, same[1] as i64, inner as i64, ok as i64],
            None,
        )
        .unwrap();
    let got: Vec<(String, &str)> = res
        .refused
        .iter()
        .map(|r| (r.path.clone(), r.reason.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            (path_of(&store, same[0]), text("dup_name")),
            (path_of(&store, same[1]), text("dup_name")),
            (path_of(&store, inner), text("dup_name")),
        ]
    );
    assert_eq!(res.added.len(), 1);
}

/// Deleting checks a path's names below its scan root (`reaches_another`): the basket keeps the
/// root with each item, so the check holds once the scan is closed too.
#[test]
fn items_keep_the_scan_root_they_were_found_under() {
    let _g = setup();
    let store = twin_store();
    let top = root_of(&store);
    let a = child(&store, top, "a");
    let af = child(&store, a, "f");
    let b = basket();
    b.add(&store.read(), "s1", &[af as i64], None).unwrap();
    // a group of small items
    b.add(
        &store.read(),
        "s1",
        &[top as i64],
        Some(&[child(&store, top, "b") as i64]),
    )
    .unwrap();
    let got: std::collections::BTreeSet<(String, String)> = b
        .targets(None)
        .into_iter()
        .map(|t| (t.path, t.root))
        .collect();
    assert_eq!(
        got,
        [
            (path_of(&store, af), store_root().to_string()),
            (
                path_of(&store, child(&store, top, "b")),
                store_root().to_string()
            ),
        ]
        .into()
    );
}

// --------------------------------------------------------------------------- twins in the basket

/// Two entries that differ only in case are two items: the second is not taken for the first
/// (which left it on disk, reported as already in the basket), and what lies below each is
/// covered by that one only.
#[test]
fn case_twins_in_the_basket_stay_two_items() {
    let _g = setup();
    let store = twin_store();
    let top = root_of(&store);
    let (a, big_a) = (child(&store, top, "a"), child(&store, top, "A"));
    let b = basket();
    let first = b.add(&store.read(), "s1", &[a as i64], None).unwrap();
    let second = b.add(&store.read(), "s1", &[big_a as i64], None).unwrap();
    assert_eq!(first.added.len(), 1, "{first:?}");
    assert_eq!(second.added.len(), 1, "{second:?}");
    assert!(second.merged.is_empty(), "{second:?}");
    assert_ne!(first.added, second.added);
    assert_eq!(b.len(), 2);
    // a file in `A` is covered by `A`, not by `a`
    let inner = child(&store, big_a, "f");
    let res = b.add(&store.read(), "s1", &[inner as i64], None).unwrap();
    assert_eq!(res.merged.len(), 1, "{res:?}");
    assert_eq!(res.merged[0].into, second.added[0]);
    let got: std::collections::BTreeSet<String> =
        b.targets(None).into_iter().map(|t| t.path).collect();
    assert_eq!(got, [path_of(&store, a), path_of(&store, big_a)].into());
    // their folder takes both in
    let res = b.add(&store.read(), "s1", &[top as i64], None).unwrap();
    assert_eq!(res.absorbed.len(), 2, "{res:?}");
}

/// A name no `String` can hold, as the file system names it: a lone UTF-16 surrogate on Windows,
/// a byte that is not UTF-8 on Linux (None on macOS: its file systems take UTF-8 only).
fn odd(stem: &str, unit: u16) -> Option<std::ffi::OsString> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        let mut w: Vec<u16> = stem.encode_utf16().collect();
        w.push(0xDC00 | unit);
        Some(std::ffi::OsString::from_wide(&w))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        use std::os::unix::ffi::OsStringExt;
        let mut b = stem.as_bytes().to_vec();
        b.push(0x80 | unit as u8);
        Some(std::ffi::OsString::from_vec(b))
    }
    #[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
    {
        let _ = (stem, unit);
        None
    }
}

/// Scan `top` (a small folder of the test's own).
fn scanned(top: &Path) -> Store {
    let (scanner, _) =
        sk_core::scan::scan_path(top, sk_core::scan::ScanOptions::default()).unwrap();
    scanner.store().clone()
}

fn node_at(store: &Store, p: &Path) -> i64 {
    store
        .read()
        .find_path(p)
        .unwrap_or_else(|| panic!("{p:?} not in the tree")) as i64
}

/// Add `paths` one by one, delete what the basket then holds, and take the results back in.
fn add_and_delete(store: &Store, top: &Path, paths: &[PathBuf]) {
    let b = basket();
    for p in paths {
        let res = b
            .add(&store.read(), "s1", &[node_at(store, p)], None)
            .unwrap();
        assert_eq!(res.added.len(), 1, "{p:?}: {res:?}");
    }
    assert_eq!(b.len(), paths.len());
    let targets = b.targets(None);
    assert_eq!(targets.len(), paths.len());
    let os: Vec<PathBuf> = targets.iter().map(|t| t.os_path.clone()).collect();
    let roots: HashMap<String, String> = targets.iter().map(|t| (t.path.clone(), s(top))).collect();
    let results = delete_permanently(&os, opts(&roots));
    assert!(results.iter().all(|r| r.ok), "{results:?}");
    let left = b.apply_results(targets.iter().zip(&results));
    assert_eq!(left.len(), paths.len());
    assert!(b.is_empty(), "{:?}", b.items());
}

/// Two names that show the same once their bytes that are not Unicode became U+FFFD: two items,
/// each deleting its own, and both leave the basket.
#[test]
fn names_that_lost_different_bytes_stay_two_items() {
    let _g = setup();
    let (Some(one), Some(two)) = (odd("x", 0), odd("x", 1)) else {
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let top = tmp.path().join("scan");
    std::fs::create_dir(&top).unwrap();
    if std::fs::write(top.join(&one), b"1").is_err() {
        return; // a file system that refuses such names
    }
    std::fs::write(top.join(&two), b"2").unwrap();
    std::fs::write(top.join("keep.txt"), b"k").unwrap();
    let store = scanned(&top);
    add_and_delete(&store, &top, &[top.join(&one), top.join(&two)]);
    assert!(!top.join(&one).exists() && !top.join(&two).exists());
    assert!(top.join("keep.txt").exists());
}

/// Case twins on disk, both put into the basket: each deletes its own, both leave the basket.
#[test]
fn case_twins_on_disk_each_leave_the_basket() {
    let _g = setup();
    let tmp = tempfile::tempdir().unwrap();
    let Some((root, a, big_a)) = twins(tmp.path()) else {
        return;
    };
    let store = scanned(&root);
    add_and_delete(&store, &root, &[a.clone(), big_a.clone()]);
    assert!(!a.exists() && !big_a.exists());
}
