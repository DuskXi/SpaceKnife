//! ops::paths (port of tests/test_ops_paths.py): the four copy formats (UI-W5-10..13) and the
//! protected-path lists of every platform.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use sk_core::ops::PATH_KEYS;
use sk_core::ops::paths::{
    System, match_rules, path_forms, path_forms_bytes, platform_rules, protected_key, reason_text,
};

mod exit_cleanup;
mod fs_caps;

/// One SpaceKnife home for this test binary (set before anything reads it), removed when it exits.
fn sk_home() -> &'static Path {
    static HOME: OnceLock<PathBuf> = OnceLock::new();
    HOME.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap().keep();
        exit_cleanup::remove_at_exit(dir.clone());
        let home = dir.join("skhome");
        // SAFETY: set once, before any test of this binary reads the environment
        unsafe { std::env::set_var("SPACEKNIFE_HOME", &home) };
        home
    })
}

// --------------------------------------------------------------------------- path forms

#[test]
fn windows_forms_spaces_and_unicode() {
    let f = path_forms(
        r"C:\Users\lenny\Videos\2026-08 青海自驾.mp4",
        Some(System::Windows),
    );
    assert_eq!(f.raw, r"C:\Users\lenny\Videos\2026-08 青海自驾.mp4");
    assert_eq!(f.quoted, r#""C:\Users\lenny\Videos\2026-08 青海自驾.mp4""#);
    assert_eq!(f.posix, "/mnt/c/Users/lenny/Videos/2026-08 青海自驾.mp4");
    assert_eq!(
        f.uri,
        "file:///C:/Users/lenny/Videos/2026-08%20%E9%9D%92%E6%B5%B7%E8%87%AA%E9%A9%BE.mp4"
    );
}

#[test]
fn windows_forms_plain_and_reserved() {
    let f = path_forms(
        r"C:\Users\lenny\Downloads\Win11_25H2_Chinese_Simplified_x64.iso",
        Some(System::Windows),
    );
    assert_eq!(
        f.uri,
        "file:///C:/Users/lenny/Downloads/Win11_25H2_Chinese_Simplified_x64.iso"
    );
    let g = path_forms(r"D:\a#b\c%d;e&f.txt", Some(System::Windows));
    assert_eq!(g.uri, "file:///D:/a%23b/c%25d%3Be%26f.txt");
    assert_eq!(path_forms("C:\\", Some(System::Windows)).uri, "file:///C:/");
}

#[test]
fn windows_forms_unc() {
    let f = path_forms(r"\\nas\share\photo album\x.jpg", Some(System::Windows));
    assert_eq!(f.posix, "//nas/share/photo album/x.jpg");
    assert_eq!(f.uri, "file://nas/share/photo%20album/x.jpg");
    assert_eq!(f.quoted, r#""\\nas\share\photo album\x.jpg""#);
}

#[test]
fn windows_forms_verbatim_prefix() {
    let f = path_forms("\\\\?\\C:\\very\\long", Some(System::Windows));
    assert_eq!(f.raw, "\\\\?\\C:\\very\\long");
    assert_eq!(f.posix, "/mnt/c/very/long");
    assert_eq!(f.uri, "file:///C:/very/long");
    let g = path_forms("\\\\?\\UNC\\host\\share\\x", Some(System::Windows));
    assert_eq!(g.posix, "//host/share/x");
    assert_eq!(g.uri, "file://host/share/x");
}

#[test]
fn windows_forms_lone_surrogate_is_encoded() {
    // WTF-8 of U+DC80, as NTFS names may hold it
    let f = path_forms_bytes(b"C:\\bad\xed\xb2\x80name", Some(System::Windows));
    assert_eq!(f.uri, "file:///C:/bad%ED%B2%80name");
    // the text forms write it escaped, as 0.3.4 did; the escape's backslash is no separator
    assert_eq!(f.raw, "C:\\bad\\udc80name");
    assert_eq!(f.quoted, "\"C:\\bad\\udc80name\"");
    assert_eq!(f.posix, "/mnt/c/bad\\udc80name");
}

#[test]
fn posix_forms_escape_undecodable_bytes() {
    let f = path_forms_bytes(b"/tmp/a\xffb", Some(System::Linux));
    assert_eq!(f.raw, "/tmp/a\\xffb");
    assert_eq!(f.posix, "/tmp/a\\xffb");
    assert_eq!(f.quoted, "'/tmp/a\\xffb'");
    assert_eq!(f.uri, "file:///tmp/a%FFb");
}

#[test]
fn posix_forms_quote_and_unicode() {
    let path = "/home/u/it's here/文件 $HOME `x` \"q\".txt";
    let f = path_forms(path, Some(System::Linux));
    assert_eq!(f.raw, path);
    assert_eq!(f.posix, path);
    assert_eq!(f.quoted, "'/home/u/it'\\''s here/文件 $HOME `x` \"q\".txt'");
    assert_eq!(
        f.uri,
        "file:///home/u/it%27s%20here/%E6%96%87%E4%BB%B6%20%24HOME%20%60x%60%20%22q%22.txt"
    );
}

#[test]
fn posix_forms_undecodable_bytes() {
    let f = path_forms_bytes(b"/tmp/\xff\xfe", Some(System::Darwin));
    assert_eq!(f.uri, "file:///tmp/%FF%FE"); // the original bytes
}

#[test]
fn forms_default_to_this_system() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a b.txt").to_string_lossy().into_owned();
    let this = if cfg!(windows) {
        System::Windows
    } else {
        System::Linux
    };
    assert_eq!(path_forms(&path, None), path_forms(&path, Some(this)));
}

// --------------------------------------------------------------------------- Windows rules

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn win() -> sk_core::ops::Rules {
    platform_rules(
        System::Windows,
        r"C:\Users\lenny",
        &env(&[
            ("SystemDrive", "C:"),
            ("SystemRoot", r"C:\Windows"),
            ("ProgramFiles", r"C:\Program Files"),
            ("ProgramFiles(x86)", r"C:\Program Files (x86)"),
            ("ProgramData", r"C:\ProgramData"),
        ]),
        &[r"C:\Users\lenny\AppData\Local\SpaceKnife".into()],
        &[r"C:\Apps\SpaceKnife".into()],
    )
}

fn linux() -> sk_core::ops::Rules {
    platform_rules(
        System::Linux,
        "/home/u",
        &HashMap::new(),
        &["/home/u/.local/share/SpaceKnife".into()],
        &["/opt/spaceknife".into()],
    )
}

fn mac() -> sk_core::ops::Rules {
    platform_rules(
        System::Darwin,
        "/Users/u",
        &HashMap::new(),
        &["/Users/u/Library/Application Support/SpaceKnife".into()],
        &[],
    )
}

fn check(rules: &sk_core::ops::Rules, cases: &[(&str, Option<&str>)]) {
    for (path, reason) in cases {
        assert_eq!(match_rules(path, rules), *reason, "{path}");
    }
}

#[test]
fn windows_rules() {
    check(
        &win(),
        &[
            ("C:\\", Some("root")),
            ("D:\\", Some("root")),
            ("C:", Some("root")),
            (r"\\nas\share", Some("root")),
            (r"\\nas\share\\", Some("root")),
            (r"C:\Windows", Some("system")),
            (r"c:\WINDOWS\System32\drivers\etc", Some("system")),
            ("\\\\?\\C:\\Windows\\explorer.exe", Some("system")),
            (r"C:\Windows\Temp", Some("system")),
            (r"C:\Windows\Temp\setup.log", None),
            (r"C:\Windows\Temp\sub\x.tmp", None),
            (r"C:\Program Files", Some("program_root")),
            (r"C:\Program Files (x86)", Some("program_root")),
            (r"C:\Program Files\Some App", None),
            (r"C:\ProgramData", Some("data_root")),
            (r"C:\ProgramData\Package Cache", None),
            (r"C:\Users", Some("users_root")),
            (r"C:\Users\lenny", Some("home")),
            (r"C:\Users\Public", Some("home")),
            (r"C:\Users\Default", Some("home")),
            (r"C:\Users\lenny\Downloads", None),
            (r"C:\Users\lenny\AppData", Some("profile")),
            (r"C:\Users\lenny\AppData\Local", Some("profile")),
            (r"C:\Users\lenny\AppData\Local\Temp", None),
            (r"C:\Users\lenny\AppData\Local\SpaceKnife", Some("app")),
            (
                r"C:\Users\lenny\AppData\Local\SpaceKnife\snapshots\x.sks",
                Some("app"),
            ),
            (r"C:\Apps\SpaceKnife\python.exe", Some("program")),
            (r"C:\$Recycle.Bin", Some("trash")),
            (r"D:\$RECYCLE.BIN\S-1-5-21-1\$RABC.txt", Some("trash")),
            (r"E:\System Volume Information\tracking.log", Some("system")),
            (r"C:\pagefile.sys", Some("system_file")),
            (r"D:\pagefile.sys", Some("system_file")),
            (r"D:\backup\pagefile.sys", None),
            (r"C:\Recovery\WindowsRE", Some("boot")),
            (r"D:\Recovery", None),
            (r"C:\bootmgr", Some("boot")),
            (r"D:\Games\Steam", None),
            (r"D:\Users", None),
        ],
    );
}

#[test]
fn linux_rules() {
    check(
        &linux(),
        &[
            ("/", Some("root")),
            ("/usr", Some("system")),
            ("/usr/lib/x86_64-linux-gnu/libc.so.6", Some("system")),
            ("/usr/local/bin/tool", Some("system")),
            ("/etc/passwd", Some("system")),
            ("/boot/vmlinuz", Some("system")),
            ("/proc/1/fd", Some("system")),
            ("/run", Some("system")),
            ("/run/media", Some("system")),
            ("/run/media/u/USB/movie.mkv", None),
            ("/run/user/1000/doc", None),
            ("/var", Some("shared_root")),
            ("/var/log", Some("shared_root")),
            ("/var/log/journal/abc", None),
            ("/var/cache/dnf", None),
            ("/var/lib/dpkg/status", Some("package_db")),
            ("/var/lib/rpm", Some("package_db")),
            ("/var/lib/docker", None),
            ("/tmp", Some("shared_root")),
            ("/tmp/x", None),
            ("/home", Some("users_root")),
            ("/home/u", Some("home")),
            ("/home/other", Some("home")),
            ("/root", Some("home")),
            ("/home/u/Downloads/a.iso", None),
            ("/home/u/.cache", None),
            ("/home/u/.config", Some("profile")),
            ("/home/u/.config/Code/Cache", None),
            ("/home/u/.local/share", Some("profile")),
            ("/home/u/.local/share/Trash", Some("trash")),
            ("/home/u/.local/share/Trash/files/old.txt", Some("trash")),
            ("/mnt/data/.Trash-1000/files/x", Some("trash")),
            ("/mnt/data/.Trash/1000", Some("trash")),
            ("/home/u/.local/share/SpaceKnife/snapshots", Some("app")),
            ("/opt/spaceknife/bin/python", Some("program")),
            ("/opt", Some("shared_root")),
            ("/opt/other", None),
            ("/home/u/./Downloads/../.config", Some("profile")),
        ],
    );
}

#[test]
fn linux_rules_follow_xdg_data_home() {
    let rules = platform_rules(
        System::Linux,
        "/home/u",
        &env(&[("XDG_DATA_HOME", "/data/xdg")]),
        &[],
        &[],
    );
    assert_eq!(
        match_rules("/data/xdg/Trash/files/a", &rules),
        Some("trash")
    );
    assert_eq!(match_rules("/data/xdg/other", &rules), None);
    let relative = platform_rules(
        System::Linux,
        "/home/u",
        &env(&[("XDG_DATA_HOME", "rel/xdg")]),
        &[],
        &[],
    );
    assert_eq!(
        match_rules("/home/u/.local/share/Trash/x", &relative),
        Some("trash")
    );
}

#[test]
fn mac_rules() {
    check(
        &mac(),
        &[
            ("/", Some("root")),
            ("/System", Some("system")),
            ("/System/Library/CoreServices", Some("system")),
            ("/usr/bin/true", Some("system")),
            ("/private/var/vm/swapfile0", Some("system")),
            ("/private/var/db/receipts", Some("system")),
            ("/private/var/folders/xy/cache", None),
            ("/Applications", Some("program_root")),
            ("/Applications/Foo.app", None),
            ("/Library", Some("shared_root")),
            ("/Library/Caches/com.x", None),
            ("/Volumes", Some("shared_root")),
            ("/Volumes/USB/movie.mov", None),
            ("/Users", Some("users_root")),
            ("/Users/u", Some("home")),
            ("/Users/Shared", Some("home")),
            ("/Users/u/Library", Some("profile")),
            ("/Users/u/Library/Caches/com.apple.Safari", None),
            ("/Users/u/Library/Mobile Documents", Some("profile")),
            ("/Users/u/.Trash", Some("trash")),
            ("/Users/u/.Trash/old.dmg", Some("trash")),
            ("/Volumes/USB/.Trashes/501/x", Some("trash")),
            (
                "/Users/u/Library/Application Support/SpaceKnife/x",
                Some("app"),
            ),
            ("/Users/u/Movies/a.mov", None),
        ],
    );
}

#[test]
fn every_reason_has_text() {
    let known: HashSet<&str> = PATH_KEYS.iter().copied().collect();
    for rules in [win(), linux(), mac()] {
        for r in rules.reasons() {
            assert!(known.contains(r), "{r}");
            assert!(!reason_text(r).is_empty());
        }
    }
    assert!(known.contains("root") && known.contains("mount"));
}

// --------------------------------------------------------------------------- this system

#[test]
fn protected_reason_on_this_system() {
    let home = sk_home();
    let dir = tempfile::tempdir().unwrap();
    let plain = dir.path().join("plain.txt");
    std::fs::write(&plain, b"x").unwrap();
    assert_eq!(protected_key(&plain.to_string_lossy()), None);
    assert_eq!(
        protected_key(&dir.path().join("not there").to_string_lossy()),
        None
    );
    let user_home = sk_core::paths::home_dir();
    assert_eq!(protected_key(&user_home.to_string_lossy()), Some("home"));
    let anchor: PathBuf = dir.path().ancestors().last().unwrap().to_path_buf();
    assert_eq!(protected_key(&anchor.to_string_lossy()), Some("root"));
    let data = home.join("data");
    assert_eq!(
        protected_key(&data.join("snapshots").join("c.sks").to_string_lossy()),
        Some("app")
    );
    let exe_dir = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    assert_eq!(protected_key(&exe_dir.to_string_lossy()), Some("program"));
}

fn make_dir_link(target: &Path, link: &Path) {
    assert!(fs_caps::dir_link(target, link), "link {}", link.display());
}

#[test]
fn protected_reason_sees_through_links() {
    let home = sk_home();
    let data = home.join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("snap.sks"), b"x").unwrap();
    let dir = tempfile::tempdir().unwrap();
    if !fs_caps::links(dir.path()) {
        fs_caps::skip(
            "protected_reason_sees_through_links",
            dir.path(),
            "no links",
        );
        return;
    }
    let link = dir.path().join("way-in");
    make_dir_link(&data, &link);
    let inside = protected_key(&link.join("snap.sks").to_string_lossy());
    let itself = protected_key(&link.to_string_lossy());
    if cfg!(windows) {
        std::fs::remove_dir(&link).unwrap();
    } else {
        std::fs::remove_file(&link).unwrap();
    }
    assert_eq!(inside, Some("app"));
    assert_eq!(itself, None); // the link itself may go: it is only a name
    assert!(data.join("snap.sks").exists());
}

#[test]
fn protected_reason_mount_points() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("mnt")).unwrap();
    std::fs::write(dir.path().join("file"), b"").unwrap();
    assert_eq!(
        protected_key(&dir.path().join("mnt").to_string_lossy()),
        None
    );
    assert_eq!(
        protected_key(&dir.path().join("file").to_string_lossy()),
        None
    );
}

/// The 8.3 short name of an existing path (`GetShortPathNameW`), None when it has none.
#[cfg(windows)]
fn short_name(p: &Path) -> Option<String> {
    use std::os::windows::process::CommandExt;
    // raw: cmd parses its own command line
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

/// A protected folder named by its 8.3 short name (a scan root typed as `C:\PROGRA~1`) is still
/// protected: the exact rules see the long name too.
#[cfg(windows)]
#[test]
fn protected_reason_sees_through_short_names() {
    let program_files =
        std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into());
    let long = Path::new(&program_files);
    let Some(short) = short_name(long) else {
        eprintln!("skipped: no 8.3 name for {}", long.display());
        return;
    };
    assert_eq!(protected_key(&program_files), Some("program_root"));
    assert_eq!(protected_key(&short), Some("program_root"), "{short}");
    // and the app's own folder by a short name of a long one
    let home = sk_home();
    let long_dir = home.join("a folder with a long name");
    std::fs::create_dir_all(&long_dir).unwrap();
    if let Some(short) = short_name(&long_dir) {
        assert_eq!(
            protected_key(&short),
            protected_key(&long_dir.to_string_lossy())
        );
    }
}
