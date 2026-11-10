//! ops::locks (port of tests/test_ops_locks.py): who holds a file open — a real handle held by
//! this test, plus the parsers.

use std::collections::HashMap;
#[cfg(not(target_os = "macos"))]
use std::collections::HashSet;
use std::fs::File;
use std::path::Path;

use sk_core::ops::locks::{self, find_lockers};

/// Keep `path` open the way an editor would (Windows: without FILE_SHARE_DELETE).
pub fn held_open(path: &Path) -> File {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0x1) // FILE_SHARE_READ only
            .open(path)
            .unwrap()
    }
    #[cfg(not(windows))]
    File::open(path).unwrap()
}

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

#[cfg(not(target_os = "macos"))]
#[test]
fn a_file_held_by_this_test() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("work dir");
    std::fs::create_dir(&folder).unwrap();
    let held = folder.join("报告.docx");
    std::fs::write(&held, b"x").unwrap();
    let free = dir.path().join("free.txt");
    std::fs::write(&free, b"y").unwrap();
    let handle = held_open(&held);
    let got = find_lockers(&[
        s(&held),
        s(&free),
        s(&folder),
        s(&dir.path().join("missing")),
    ]);
    let keys: HashSet<&String> = got.keys().collect();
    assert_eq!(keys, HashSet::from([&s(&held), &s(&folder)]), "{got:?}"); // the folder: anything inside
    assert!(!got[&s(&held)].is_empty());
    drop(handle);
    assert!(find_lockers(&[s(&held), s(&folder)]).is_empty());
}

#[test]
fn nothing_asked_nothing_found() {
    let dir = tempfile::tempdir().unwrap();
    assert!(find_lockers(&[]).is_empty());
    assert!(find_lockers(&[s(&dir.path().join("nope"))]).is_empty());
}

#[cfg(unix)]
#[test]
fn proc_scan_matches_files_and_folders() {
    let dir = tempfile::tempdir().unwrap();
    // resolved, as /proc reports targets (macOS: /var is a link to /private/var)
    let tmp = &dir.path().canonicalize().unwrap();
    let proc = tmp.join("proc");
    let target_dir = tmp.join("data");
    std::fs::create_dir_all(target_dir.join("sub")).unwrap();
    let file_a = target_dir.join("sub").join("a.bin");
    std::fs::write(&file_a, b"").unwrap();
    let other = format!("{} (deleted)", s(&target_dir.join("other.txt")));
    for (pid, comm, targets) in [
        (
            "101",
            "editor",
            vec![s(&file_a), "socket:[1234]".into(), "pipe:[9]".into()],
        ),
        ("202", "indexer", vec![other, "/dev/null".into()]),
        ("303", "viewer", vec![s(&file_a)]),
    ] {
        let fd = proc.join(pid).join("fd");
        std::fs::create_dir_all(&fd).unwrap();
        std::fs::write(proc.join(pid).join("comm"), format!("{comm}\n")).unwrap();
        for (i, t) in targets.iter().enumerate() {
            // the fake /proc lives on the volume under test: FAT and exFAT hold no links
            if std::os::unix::fs::symlink(t, fd.join((i + 3).to_string())).is_err() {
                eprintln!(
                    "skipped: proc_scan_matches_files_and_folders: no links ({})",
                    tmp.display()
                );
                return;
            }
        }
    }
    std::fs::create_dir(proc.join("404")).unwrap(); // fds we may not read: skipped
    std::fs::create_dir(proc.join("self")).unwrap(); // not a pid
    let got = locks::linux(
        &[s(&file_a), s(&target_dir), s(&tmp.join("unrelated"))],
        &proc,
    );
    let want: HashMap<String, Vec<String>> = HashMap::from([
        (s(&file_a), vec!["editor".to_string(), "viewer".to_string()]),
        (
            s(&target_dir),
            vec!["editor".to_string(), "viewer".to_string()],
        ),
    ]);
    assert_eq!(got, want);
}

#[test]
fn lsof_output_is_parsed() {
    let inside = "/tmp/proj/x.psd";
    let out = format!("p12\ncPhotoshop\nn{inside}\np34\ncFinder\nn/somewhere/else\n");
    let wanted = HashMap::from([("/tmp/proj".to_string(), "/tmp/proj".to_string())]);
    let got = locks::parse_lsof(&out, &wanted);
    assert_eq!(
        got,
        HashMap::from([("/tmp/proj".to_string(), vec!["Photoshop".to_string()])])
    );
}
