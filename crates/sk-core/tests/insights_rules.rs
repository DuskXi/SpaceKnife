//! Insights rules library (port of tests/test_insights.py; docs/insights.md, contract
//! docs/manage.md §5). Path strings and in-memory TOML, except the user-directory tests.

use std::collections::HashSet;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use parking_lot::Mutex;
use sk_core::insights::{
    self, Cleanup, InsightKind, Kinds, RuleSet, builtin_text, compile_rules, load_rules, node_kind,
    parse_size,
};
use sk_store::Kind;
use sk_store::testing::{build_with, d, f, find};

const MB: i64 = 1 << 20;

fn ruleset(host: &str, extra: &[&str]) -> RuleSet {
    let mut sources = vec![("builtin.toml".to_string(), builtin_text().to_string())];
    for (i, text) in extra.iter().enumerate() {
        sources.push((format!("user{i}.toml"), text.to_string()));
    }
    compile_rules(&sources, Some(host), Vec::new()).unwrap()
}

fn host_set(host: &str) -> &'static RuleSet {
    static WIN: OnceLock<RuleSet> = OnceLock::new();
    static LIN: OnceLock<RuleSet> = OnceLock::new();
    static MAC: OnceLock<RuleSet> = OnceLock::new();
    let cell = match host {
        "windows" => &WIN,
        "linux" => &LIN,
        _ => &MAC,
    };
    cell.get_or_init(|| ruleset(host, &[]))
}

fn win() -> &'static RuleSet {
    host_set("windows")
}
fn lin() -> &'static RuleSet {
    host_set("linux")
}
fn mac() -> &'static RuleSet {
    host_set("macos")
}

/// Tests that change the active rule set run one at a time.
static ACTIVE: Mutex<()> = Mutex::new(());

fn rule(id: &str, body: &str) -> String {
    format!("[[rule]]\nid = \"{id}\"\nkind = \"cache\"\ntitle = \"T {id}\"\nreason = \"R {id}\"\n{body}\n")
}

fn one(host: &str, text: &str) -> RuleSet {
    compile_rules(&[("t".to_string(), text.to_string())], Some(host), Vec::new()).unwrap()
}

fn hit(set: &RuleSet, path: &str, kind: &str) -> Option<String> {
    set.match_path(path, node_kind(kind).unwrap(), None, None).map(|i| i.rule)
}

// --- the built-in library -----------------------------------------------------------------------

#[test]
fn builtin_rules_load_cleanly() {
    let w = win();
    assert!(w.problems.is_empty(), "{:?}", w.problems);
    assert!(w.len() >= 40);
    assert_eq!(w.unanchored, 0); // every built-in pattern is indexed
    let kinds: HashSet<InsightKind> = w.rules().map(|r| r.kind).collect();
    assert_eq!(kinds, InsightKind::ALL.into_iter().collect());
    let raw: toml::Table = builtin_text().parse().unwrap();
    let ids: Vec<String> = raw["rule"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect();
    let unique: HashSet<&String> = ids.iter().collect();
    assert_eq!(ids.len(), unique.len());
    assert_eq!(ids.len(), w.len());
}

#[test]
fn builtin_rules_are_translated_and_explained() {
    for r in win().rules() {
        let en = r.i18n.get("en").cloned().unwrap_or_default();
        assert!(en.get("title").is_some_and(|s| !s.is_empty()), "{}: no English title", r.id);
        assert!(en.get("reason").is_some_and(|s| !s.is_empty()), "{}: no English reason", r.id);
        assert!(r.text.reason.chars().count() >= 20, "{}", r.id);
        assert!(!r.text.note.is_empty(), "{}", r.id);
        if r.text.app.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)) {
            assert!(en.contains_key("app"), "{}: Chinese app name without an English one", r.id);
        }
    }
}

#[test]
fn builtin_rules_are_documented() {
    let docs = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/insights.md");
    let Ok(text) = std::fs::read_to_string(docs) else {
        return; // the docs folder is not part of every checkout
    };
    let missing: Vec<String> = win()
        .rules()
        .filter(|r| !text.contains(&format!("`{}`", r.id)))
        .map(|r| r.id.clone())
        .collect();
    assert!(missing.is_empty(), "{missing:?}");
}

/// (host, path, node kind, expected rule id or None)
const CASES: &[(&str, &str, &str, Option<&str>)] = &[
    // vm
    ("windows", r"C:\Users\lenny\AppData\Local\Docker", "dir", Some("docker-desktop")),
    ("windows", r"C:\Users\lenny\AppData\Local\Docker\wsl\disk\docker_data.vhdx", "file", Some("docker-desktop")),
    ("windows", r"C:\Users\lenny\AppData\Local\Docker\wsl\main\ext4.vhdx", "file", Some("docker-desktop")),
    ("windows", r"D:\WSL\Ubuntu\ext4.vhdx", "file", Some("wsl-distro-disk")),
    ("windows", r"E:\VMs\win11.vhdx", "file", Some("virtual-disk-file")),
    ("windows", r"C:\Users\Public\Documents\Hyper-V\Virtual Hard Disks\a.vhdx", "file", Some("hyperv-vms")),
    ("linux", "/home/a/VirtualBox VMs/ubuntu/ubuntu.vdi", "file", Some("virtualbox-vms")),
    ("linux", "/var/lib/docker/overlay2/x/diff/usr/lib/node_modules", "dir", Some("container-engine")),
    ("macos", "/Users/a/Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw", "file", Some("docker-desktop-mac")),
    ("macos", "/Users/a/Parallels/Windows 11.pvm", "dir", Some("parallels-vms")),
    ("linux", "/home/a/.android/avd/Pixel_8.avd", "dir", Some("android-avd")),
    // build
    ("windows", r"C:\src\app\node_modules", "dir", Some("node-modules")),
    ("linux", "/home/a/src/app/.venv", "dir", Some("python-venv")),
    ("linux", "/home/a/miniconda3/envs/torch", "dir", Some("conda-env")),
    ("macos", "/Users/a/Library/Developer/Xcode/DerivedData", "dir", Some("xcode-derived-data")),
    ("linux", "/home/a/src/crate/target", "dir", Some("cargo-target")),
    ("windows", r"C:\src\cpp\build", "dir", Some("build-output")),
    ("windows", r"C:\src\cpp\cmake-build-debug", "dir", Some("build-output")),
    ("windows", r"C:\src\repo\.git\objects\pack", "dir", Some("git-objects")),
    ("windows", r"C:\src\web\.next", "dir", Some("js-framework-build")),
    ("windows", r"C:\src\sln\.vs", "dir", Some("visual-studio-cache")),
    ("linux", "/home/a/game/Library/PackageCache", "dir", Some("unity-project-cache")),
    // cache
    ("windows", r"C:\Users\a\AppData\Local\pip\cache", "dir", Some("pip-cache")),
    ("linux", "/home/a/.cache/pip", "dir", Some("pip-cache")),
    ("macos", "/Users/a/Library/Caches/pip", "dir", Some("pip-cache")),
    ("windows", r"C:\Users\a\AppData\Local\uv\cache", "dir", Some("uv-cache")),
    ("windows", r"C:\Users\a\AppData\Local\npm-cache", "dir", Some("npm-cache")),
    ("linux", "/home/a/.local/share/pnpm/store/v3", "dir", Some("pnpm-store")),
    ("linux", "/home/a/.cargo/registry", "dir", Some("cargo-registry")),
    ("linux", "/home/a/go/pkg/mod", "dir", Some("go-cache")),
    ("windows", r"C:\Users\a\.m2\repository", "dir", Some("maven-repo")),
    ("linux", "/home/a/.gradle/caches", "dir", Some("gradle-cache")),
    ("windows", r"C:\Users\a\.nuget\packages", "dir", Some("nuget-cache")),
    ("windows", r"C:\Users\a\AppData\Local\JetBrains\PyCharm2024.3", "dir", Some("jetbrains-cache")),
    ("windows", r"C:\Users\a\AppData\Roaming\Code\CachedExtensionVSIXs", "dir", Some("vscode-cache")),
    ("windows", r"C:\Users\a\AppData\Local\Google\Chrome\User Data\Default\Cache", "dir", Some("chrome-cache")),
    ("windows", r"C:\Users\a\AppData\Local\Microsoft\Edge\User Data\Profile 1\Code Cache", "dir", Some("edge-cache")),
    ("windows", r"C:\Users\a\AppData\Local\Mozilla\Firefox\Profiles\x.default\cache2", "dir", Some("firefox-cache")),
    ("linux", "/home/a/.cache/google-chrome", "dir", Some("chrome-cache")),
    ("linux", "/home/a/.cache/huggingface/hub", "dir", Some("huggingface-cache")),
    ("linux", "/var/cache/apt/archives", "dir", Some("apt-cache")),
    ("linux", "/var/cache/dnf", "dir", Some("dnf-cache")),
    ("windows", r"C:\Users\a\AppData\Local\Temp", "dir", Some("windows-temp")),
    ("windows", r"D:\SteamLibrary\steamapps\shadercache", "dir", Some("steam-shadercache")),
    // chat
    ("windows", r"C:\Users\a\Documents\WeChat Files", "dir", Some("wechat-files")),
    ("windows", r"D:\Data\WeChat Files\wxid_7k2m9\FileStorage\Video", "dir", Some("wechat-files")),
    ("windows", r"C:\Users\a\Documents\WeChat Files\wxid_7k2m9\FileStorage\Cache", "dir", Some("wechat-cache")),
    ("windows", r"C:\Users\a\Documents\Tencent Files", "dir", Some("qq-files")),
    ("linux", "/home/a/.local/share/TelegramDesktop/tdata", "dir", Some("telegram-data")),
    ("macos", "/Users/a/Library/Containers/com.tencent.xinWeChat", "dir", Some("wechat-files")),
    // game
    ("windows", r"D:\SteamLibrary\steamapps\common\Cyberpunk 2077", "dir", Some("steam-game")),
    ("linux", "/home/a/.local/share/Steam/steamapps/common/Hades", "dir", Some("steam-game")),
    ("windows", r"C:\Program Files\Epic Games\Fortnite", "dir", Some("epic-game")),
    ("windows", r"C:\Program Files\Epic Games\UE_5.4", "dir", Some("unreal-engine")),
    ("windows", r"C:\XboxGames\Halo Infinite", "dir", Some("xbox-game")),
    ("windows", r"C:\Program Files (x86)\GOG Galaxy\Games\Witcher 3", "dir", Some("gog-game")),
    // system
    ("windows", r"C:\hiberfil.sys", "file", Some("hiberfil")),
    ("windows", r"D:\pagefile.sys", "file", Some("pagefile")),
    ("windows", r"C:\swapfile.sys", "file", Some("pagefile")),
    ("windows", r"C:\Windows.old", "dir", Some("windows-old")),
    ("windows", r"C:\Windows\WinSxS", "dir", Some("winsxs")),
    ("windows", r"C:\Windows\SoftwareDistribution\Download", "dir", Some("windows-update-download")),
    ("windows", r"C:\Windows\SoftwareDistribution\DataStore", "dir", Some("software-distribution")),
    ("windows", r"C:\$Recycle.Bin", "dir", Some("recycle-bin")),
    ("linux", "/swapfile", "file", Some("linux-swapfile")),
    ("linux", "/var/log/journal", "dir", Some("journal-logs")),
    ("macos", "/private/var/vm/sleepimage", "file", Some("macos-swap")),
    // app
    ("windows", r"C:\Users\a\.ollama\models", "dir", Some("ollama-models")),
    ("linux", "/usr/share/ollama/.ollama/models/blobs", "dir", Some("ollama-models")),
    ("macos", "/Users/a/Library/Application Support/MobileSync/Backup", "dir", Some("ios-backups")),
    ("windows", r"C:\Users\a\AppData\Local\Android\Sdk", "dir", Some("android-sdk")),
    ("linux", "/home/a/.vscode-server/extensions/ms-python.python/node_modules", "dir", Some("vscode-extensions")),
    ("windows", r"C:\Users\a\AppData\Local\Google\Chrome\User Data\Default\Bookmarks", "file", Some("browser-profile")),
    ("macos", "/Users/a/Pictures/Photos Library.photoslibrary", "dir", Some("photos-library")),
    // nothing known
    ("windows", r"C:\Users\lenny\Music", "dir", None),
    ("windows", r"C:\Users\lenny\Code\Cache", "dir", None), // a projects folder named Code is not VS Code
    ("linux", "/home/a/Documents/report.pdf", "file", None),
    ("windows", r"C:\Program Files\nodejs\node_modules\npm", "dir", None), // bundled with an app
    ("windows", r"C:\Program Files\Epic Games\Launcher", "dir", None),
    ("linux", "/usr/lib/modules/6.8.0/build", "dir", None),
    ("windows", r"C:\Python312\Lib\venv", "dir", None), // the standard library's venv package
    ("windows", r"C:\Users\a\AppData\Roaming\uv\python\cpython-3.12\Lib\venv", "dir", Some("uv-python")),
    ("linux", "/home/a/.npm/_npx/1f2e/node_modules", "dir", Some("npm-cache")),
    ("windows", r"C:\src\app\node_modules", "file", None), // node kind filter
];

#[test]
fn known_locations() {
    for &(host, path, kind, expected) in CASES {
        let set = host_set(host);
        let found = set.match_path(path, node_kind(kind).unwrap(), None, None);
        assert_eq!(found.as_ref().map(|f| f.rule.as_str()), expected, "{host} {path}");
        if let Some(f) = found {
            assert_eq!(f.kind, set.get(&f.rule).unwrap().kind);
            assert!(!f.title.is_empty() && !f.reason.is_empty() && !f.note.is_empty());
        }
    }
}

#[test]
fn cases_cover_every_kind() {
    let covered: HashSet<InsightKind> = CASES
        .iter()
        .filter_map(|c| c.3)
        .map(|id| win().get(id).unwrap().kind)
        .collect();
    assert_eq!(covered, InsightKind::ALL.into_iter().collect());
}

// --- path forms and platforms -------------------------------------------------------------------

#[test]
fn windows_path_forms() {
    for path in [
        r"C:\Users\a\AppData\Local\Docker",
        "C:/Users/a/AppData/Local/Docker",
        r"c:\users\A\appdata\LOCAL\docker",
        "C:\\Users\\a\\AppData\\Local\\Docker\\",
        r"\\?\C:\Users\a\AppData\Local\Docker",
    ] {
        for set in [win(), lin()] {
            // a Windows path is a Windows path on any host
            let found = set.match_path(path, Kind::Dir, None, None).unwrap();
            assert_eq!(found.rule, "docker-desktop");
            assert_eq!(found.path.to_lowercase(), r"c:\users\a\appdata\local\docker");
            assert!(!found.inside);
        }
    }
}

#[test]
fn windows_unc_and_roots() {
    let found = win().match_path(r"\\nas\share\proj\node_modules\left-pad\index.js", Kind::File, None, None).unwrap();
    assert_eq!(found.rule, "node-modules");
    assert_eq!(found.path, r"\\nas\share\proj\node_modules");
    assert!(found.inside);
    let found = win().match_path(r"\\?\UNC\nas\share\proj\node_modules", Kind::Dir, None, None).unwrap();
    assert_eq!(found.path, r"\\nas\share\proj\node_modules");
    let found = win().match_path(r"\\?\C:\hiberfil.sys", Kind::File, None, None).unwrap();
    assert_eq!((found.rule.as_str(), found.path.as_str()), ("hiberfil", r"C:\hiberfil.sys"));
    assert!(win().match_path("C:\\", Kind::Dir, None, None).is_none());
    assert!(win().match_path("", Kind::Dir, None, None).is_none());
}

#[test]
fn posix_case_rules() {
    // Linux is case-sensitive: .cache/PIP is just something in the cache folder
    let found = lin().match_path("/home/a/.cache/PIP", Kind::Dir, None, None).unwrap();
    assert_eq!((found.rule.as_str(), found.path.as_str()), ("xdg-cache", "/home/a/.cache"));
    // macOS (APFS default) is not
    let found = mac().match_path("/users/a/library/caches/PIP", Kind::Dir, None, None).unwrap();
    assert_eq!(found.rule, "pip-cache");
}

#[test]
fn platform_filters() {
    assert_eq!(hit(lin(), "/home/a/AppData/Local/Docker", "dir"), None);
    assert_eq!(hit(mac(), "/var/cache/apt/archives", "dir"), None);
    assert_eq!(hit(lin(), "/Users/a/Library/Developer/Xcode/DerivedData", "dir"), None);
    // a POSIX path seen on Windows gets the POSIX rules
    assert_eq!(hit(win(), "/var/cache/apt/archives", "dir").as_deref(), Some("apt-cache"));
    assert_eq!(
        hit(win(), "/Users/a/Library/Developer/Xcode/DerivedData", "dir").as_deref(),
        Some("xcode-derived-data")
    );
}

#[test]
fn node_kind_forms() {
    for kind in ["dir", "DIR"] {
        assert!(win().match_path(r"C:\src\node_modules", node_kind(kind).unwrap(), None, None).is_some());
    }
    assert!(win().match_path(r"C:\src\node_modules", Kind::Dir, None, None).is_some());
    assert!(win().match_path(r"C:\src\node_modules", Kind::File, None, None).is_none());
    assert!(node_kind("folder").unwrap_err().to_string().contains("unknown node kind"));
}

// --- ranking: subtree, priority, depth, outermost ------------------------------------------------

#[test]
fn inside_a_matched_folder() {
    let found = win().match_path(r"C:\src\app\node_modules\react\cjs\react.js", Kind::File, None, None).unwrap();
    assert_eq!(found.rule, "node-modules");
    assert!(found.inside);
    assert_eq!(found.path, r"C:\src\app\node_modules");
}

#[test]
fn outermost_match_of_the_same_rule() {
    let found = win()
        .match_path(r"C:\src\app\node_modules\a\node_modules\b\node_modules", Kind::Dir, None, None)
        .unwrap();
    assert_eq!(found.rule, "node-modules");
    assert_eq!(found.path, r"C:\src\app\node_modules");
    assert!(found.inside);
}

#[test]
fn owner_folders_win_over_what_is_inside() {
    let p = r"D:\SteamLibrary\steamapps\common\Game\resources\app\node_modules";
    assert_eq!(hit(win(), p, "dir").as_deref(), Some("steam-game"));
    assert_eq!(hit(win(), r"C:\src\x\.venv\Lib\site-packages\pkg\build", "dir").as_deref(), Some("python-venv"));
    assert_eq!(hit(lin(), "/home/a/.cache/pip/http/a/b", "dir").as_deref(), Some("pip-cache"));
}

#[test]
fn deeper_match_wins_at_equal_priority() {
    let found = win().match_path(r"C:\src\repo\.git\objects", Kind::Dir, None, None).unwrap();
    assert_eq!((found.rule.as_str(), found.inside), ("git-objects", false));
    let found = win().match_path(r"C:\src\repo\.git\config", Kind::File, None, None).unwrap();
    assert_eq!((found.rule.as_str(), found.inside), ("git-repo", true));
    let p = r"C:\Windows\SoftwareDistribution\Download\x\y.cab";
    assert_eq!(hit(win(), p, "file").as_deref(), Some("windows-update-download"));
}

#[test]
fn specificity_breaks_ties() {
    let set = one("linux", &(rule("broad", "paths = [\"*/b\"]") + &rule("narrow", "paths = [\"a/b\"]")));
    assert_eq!(hit(&set, "/x/a/b", "dir").as_deref(), Some("narrow"));
    assert_eq!(hit(&set, "/x/z/b", "dir").as_deref(), Some("broad"));
}

#[test]
fn subtree_false_and_min_size() {
    let text = rule("only-self", "names = [\"exact\"]\nsubtree = false")
        + &rule("big", "names = [\"huge\"]\nmin_size = \"1 MB\"");
    let set = one("linux", &text);
    assert!(set.match_path("/a/exact", Kind::Dir, None, None).is_some());
    assert!(set.match_path("/a/exact/child", Kind::Dir, None, None).is_none());
    assert!(set.match_path("/a/huge", Kind::Dir, Some(10), None).is_none());
    assert!(set.match_path("/a/huge", Kind::Dir, Some(2 * MB), None).is_some());
    assert!(set.match_path("/a/huge", Kind::Dir, None, None).is_some()); // size unknown
    assert!(set.match_path("/a/huge/x", Kind::File, Some(1), None).is_some()); // for the item itself
}

#[test]
fn parents_and_exclude() {
    let text = rule("objects", "names = [\"objects\"]\nparents = [\".git\", \".hg\"]")
        + &rule("logs", "names = [\"logs\"]\nexclude = [\"/var/**\"]");
    let set = one("linux", &text);
    assert!(hit(&set, "/r/.git/objects", "dir").is_some());
    assert!(hit(&set, "/r/.hg/objects", "dir").is_some());
    assert!(hit(&set, "/r/src/objects", "dir").is_none());
    assert!(hit(&set, "/objects", "dir").is_none());
    assert!(hit(&set, "/home/a/logs", "dir").is_some());
    assert!(hit(&set, "/var/log/app/logs", "dir").is_none());
}

// --- glob syntax ----------------------------------------------------------------------------------

#[test]
fn glob_syntax() {
    for (glob, yes, no) in [
        ("a/{b,c/d}", "/x/a/c/d", "/x/a/c"),
        ("a/{b,{c,e}}x", "/a/ex", "/a/dx"),
        ("data[0-9]", "/data7", "/datax"),
        ("v[!0-9]", "/vx", "/v1"),
        ("[{]a}", "/{a}", "/a"),
        ("?:/Windows.old", "C:\\Windows.old", "C:\\x\\Windows.old"),
        ("/opt/*/cache", "/opt/app/cache", "/opt/a/b/cache"),
        ("/opt/**/cache", "/opt/a/b/cache", "/srv/a/cache"),
        ("file?.bin", "/q/file1.bin", "/q/file12.bin"),
        ("*.tar.gz", "/q/a.tar.gz", "/q/a.gz"),
        ("pre*", "/q/prefix", "/q/pr"),
        (r"win\style\path", "/q/win/style/path", "/q/win/style"),
    ] {
        let quoted = format!("'{glob}'"); // a TOML literal string
        let set = one("linux", &rule("g", &format!("paths = [{quoted}]")));
        assert!(set.problems.is_empty(), "{glob}: {:?}", set.problems);
        assert_eq!(hit(&set, yes, "dir").as_deref(), Some("g"), "{glob} {yes}");
        assert_eq!(hit(&set, no, "dir"), None, "{glob} {no}");
    }
}

#[test]
fn sizes_parse() {
    use toml::Value;
    assert_eq!(parse_size(&Value::Integer(123)).unwrap(), 123);
    assert_eq!(parse_size(&Value::String("500 MB".into())).unwrap(), 500 * MB);
    assert_eq!(parse_size(&Value::String("2GiB".into())).unwrap(), 2 << 30);
    assert_eq!(parse_size(&Value::String("1.5k".into())).unwrap(), 1536);
    assert_eq!(parse_size(&Value::String(" 10 b ".into())).unwrap(), 10);
    assert!(parse_size(&Value::String("lots".into())).is_err());
    assert!(parse_size(&Value::Boolean(true)).is_err());
}

// --- user rules, overrides and malformed rules ---------------------------------------------------

#[test]
fn user_rules_override_and_disable() {
    let _g = ACTIVE.lock();
    let dir = tempfile::tempdir().unwrap();
    let mine = String::from(
        "[[rule]]\nid = \"node-modules\"\nkind = \"build\"\ntitle = \"我的依赖\"\nreason = \"自定义说明\"\nnames = [\"node_modules\", \"bower_components\"]\n",
    ) + &rule("my-renders", "kind = \"app\"\npaths = [\"Videos/Renders\"]").replace("kind = \"cache\"\n", "");
    std::fs::write(dir.path().join("10-mine.toml"), mine).unwrap();
    std::fs::write(dir.path().join("20-off.toml"), "[[rule]]\nid = \"pycache\"\nenabled = false\n").unwrap();
    std::fs::write(dir.path().join("notes.txt"), "not a rule file").unwrap();
    let set = load_rules(Some(dir.path()), Some("linux")).unwrap();
    assert!(set.problems.is_empty(), "{:?}", set.problems);
    assert!(Arc::ptr_eq(&insights::active_rules(), &set));
    let found = insights::match_path("/home/a/web/bower_components", Kind::Dir, None, None).unwrap();
    assert_eq!((found.rule.as_str(), found.title.as_str()), ("node-modules", "我的依赖"));
    assert!(set.get("node-modules").unwrap().source.ends_with("10-mine.toml"));
    assert!(set.get("pycache").is_none());
    assert!(insights::match_path("/home/a/proj/__pycache__", Kind::Dir, None, None).is_none());
    let found = insights::match_path("/home/a/Videos/Renders/final.mov", Kind::File, None, None).unwrap();
    assert_eq!((found.rule.as_str(), found.kind), ("my-renders", InsightKind::App));
    assert_eq!(set.len(), ruleset("linux", &[]).len()); // one rule added, one disabled
    insights::reset_active();
}

#[test]
fn user_rule_wins_ties_with_builtin() {
    let clone = rule("my-node-modules", "kind = \"build\"\nnames = [\"node_modules\"]\npriority = 1")
        .replace("kind = \"cache\"\n", "");
    let set = ruleset("linux", &[&clone]);
    assert_eq!(hit(&set, "/p/node_modules", "dir").as_deref(), Some("my-node-modules"));
}

#[test]
fn malformed_rules_are_reported_and_skipped() {
    let bad = [
        rule("good", "names = [\"good\"]"),
        "[[rule]]\nid = \"no-reason\"\nkind = \"cache\"\ntitle = \"x\"\nnames = [\"a\"]\n".to_string(),
        rule("bad-kind", "names = [\"a\"]").replace("kind = \"cache\"", "kind = \"music\""),
        rule("typo", "names = [\"a\"]\nplatform = \"linux\""),
        rule("bad-glob", "paths = [\"foo[ab\"]"),
        rule("bad-brace", "paths = [\"foo{a,b\"]"),
        rule("tilde", "paths = [\"~/.cache/x\"]"),
        rule("sep-in-name", "names = [\"a/b\"]"),
        rule("bad-platform", "names = [\"a\"]\nplatforms = [\"beos\"]"),
        rule("bad-size", "names = [\"a\"]\nmin_size = \"lots\""),
        rule("bad-priority", "names = [\"a\"]\npriority = true"),
        rule("no-matcher", ""),
        rule("bad-lang", "names = [\"a\"]\ni18n.english.title = \"x\""),
        rule("Bad Id!", "names = [\"a\"]"),
        rule("good", "names = [\"good2\"]"), // duplicate id: reported, the later one wins
        "[[rule]]\nkind = 1\n".to_string(),
    ]
    .join("\n");
    let set = compile_rules(
        &[
            ("bad.toml".into(), bad),
            ("broken.toml".into(), "[[rule]\nid = 1".into()),
            ("future.toml".into(), "version = 2\n".into()),
        ],
        Some("linux"),
        Vec::new(),
    )
    .unwrap();
    assert_eq!(set.rules().map(|r| r.id.clone()).collect::<Vec<_>>(), ["good"]);
    assert!(hit(&set, "/x/good2", "dir").is_some() && hit(&set, "/x/good", "dir").is_none());
    let labels: Vec<&str> = set.problems.iter().map(|p| p.rule.as_str()).collect();
    for id in [
        "no-reason", "bad-kind", "typo", "bad-glob", "bad-brace", "tilde", "sep-in-name", "bad-platform",
        "bad-size", "bad-priority", "no-matcher", "bad-lang", "Bad Id!", "good", "#16",
    ] {
        assert!(labels.contains(&id), "{id}: {labels:?}");
    }
    let sources: HashSet<&str> = set.problems.iter().map(|p| p.source.as_str()).collect();
    assert_eq!(sources, HashSet::from(["bad.toml", "broken.toml", "future.toml"]));
    assert_eq!(set.problems.len(), 17);
    assert!(set.problems.iter().any(|p| p.message.contains("未知字段 platform"))); // the field is named
}

#[test]
fn unreadable_user_files_are_reported() {
    let _g = ACTIVE.lock();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("latin1.toml"), b"[[rule]]\nid = \"x\"\ntitle = \"\xe9\"\n").unwrap();
    let set = load_rules(Some(dir.path()), Some("linux")).unwrap();
    assert_eq!(set.problems.len(), 1);
    assert!(set.problems[0].source.ends_with("latin1.toml"));
    assert_eq!(set.len(), ruleset("linux", &[]).len());
    assert!(load_rules(Some(&dir.path().join("missing")), Some("linux")).unwrap().problems.is_empty());
    insights::reset_active();
}

// --- languages ----------------------------------------------------------------------------------

#[test]
fn languages() {
    let path = r"D:\SteamLibrary\steamapps\common\Cyberpunk 2077";
    let zh = win().match_path(path, Kind::Dir, None, Some("zh-CN")).unwrap();
    assert_eq!((zh.title.as_str(), zh.note.as_str(), zh.app.as_str()), ("Steam 游戏", "游戏更新", "Steam 游戏"));
    for lang in ["en", "en-US", "EN_gb"] {
        let en = win().match_path(path, Kind::Dir, None, Some(lang)).unwrap();
        assert_eq!((en.title.as_str(), en.note.as_str(), en.app.as_str()), ("Steam game", "Game update", "Steam game"));
    }
    for lang in ["zh-CN", "fr", ""] {
        assert_eq!(win().match_path(path, Kind::Dir, None, Some(lang)).unwrap(), zh);
    }
}

#[test]
fn partial_translation_falls_back_per_field() {
    let text = rule("t", "names = [\"t\"]\nnote = \"短注\"\ni18n.en.title = \"Title\"\ni18n.zh-tw.reason = \"說明\"");
    let set = one("linux", &text);
    let en = set.match_path("/t", Kind::Dir, None, Some("en")).unwrap();
    assert_eq!((en.title.as_str(), en.reason.as_str(), en.note.as_str()), ("Title", "R t", "Title"));
    let tw = set.match_path("/t", Kind::Dir, None, Some("zh-TW")).unwrap();
    assert_eq!((tw.title.as_str(), tw.reason.as_str(), tw.note.as_str()), ("T t", "說明", "短注"));
}

// --- lists, store nodes and the module-level functions --------------------------------------------

#[test]
fn match_many_equals_match() {
    let cases: Vec<&(&str, &str, &str, Option<&str>)> = CASES.iter().filter(|c| c.0 == "windows").collect();
    let paths: Vec<&str> = cases.iter().map(|c| c.1).collect();
    let kinds: Vec<Kind> = cases.iter().map(|c| node_kind(c.2).unwrap()).collect();
    let many = win().match_many(&paths, Kinds::Each(&kinds), None, None).unwrap();
    let single: Vec<_> = paths.iter().zip(&kinds).map(|(p, k)| win().match_path(p, *k, None, None)).collect();
    assert_eq!(many, single);
    let mut siblings: Vec<String> = (0..50).map(|i| format!(r"C:\src\app\node_modules\pkg{i}")).collect();
    siblings.push(r"C:\src\app\src".into());
    let found = win().match_many(&siblings, Kinds::One(Kind::Dir), None, None).unwrap();
    assert!(found[..50].iter().all(|f| f.as_ref().is_some_and(|f| f.path == r"C:\src\app\node_modules")));
    assert!(found[50].is_none());
    let sized = win()
        .match_many(&[r"C:\a\node_modules"; 2], Kinds::One(Kind::Dir), Some(&[None, Some(5)]), None)
        .unwrap();
    assert!(sized[0].is_some() && sized[0] == sized[1]);
    assert!(win().match_many(&["a", "b"], Kinds::Each(&[Kind::Dir]), None, None).is_err());
    assert!(win().match_many(&["a"], Kinds::One(Kind::Dir), Some(&[Some(1), Some(2)]), None).is_err());
}

#[test]
fn match_node() {
    let root = if cfg!(windows) { "C:\\work" } else { "/work" };
    let spec = d([(
        "proj",
        d([
            ("node_modules", d([("react", d([("index.js", f(100))]))])),
            ("src", d([("main.py", f(10))])),
        ]),
    )]);
    let store = build_with(&spec, root, 1, &[]);
    let nodes = store.read();
    let set = ruleset(insights::host_platform(), &[]);
    let found = set.match_node(&nodes, find(&nodes, "proj/node_modules"), None).unwrap();
    assert_eq!((found.rule.as_str(), found.inside), ("node-modules", false));
    let found = set.match_node(&nodes, find(&nodes, "proj/node_modules/react/index.js"), None).unwrap();
    assert!(found.inside && found.path.ends_with("node_modules"));
    assert!(set.match_node(&nodes, find(&nodes, "proj/src/main.py"), None).is_none());
}

#[test]
fn module_functions_load_builtins_lazily() {
    let _g = ACTIVE.lock();
    insights::reset_active();
    let path = if cfg!(windows) { r"C:\home\a\proj\node_modules" } else { "/home/a/proj/node_modules" };
    let found = insights::match_path(path, Kind::Dir, None, None).unwrap();
    assert_eq!(found.rule, "node-modules");
    assert_eq!(found.cleanup, insights::active_rules().get("node-modules").unwrap().cleanup);
    let many = insights::match_many(&["/x/node_modules", "/x/src"], Kinds::One(Kind::Dir), None, None).unwrap();
    assert!(many[0].as_ref().is_some_and(|m| m.rule == "node-modules") && many[1].is_none());
    assert!(insights::active_rules().problems.is_empty());
    assert_eq!(insights::user_rules_dir().file_name().unwrap(), "insights");
    let _ = Cleanup::Safe;
}

// --- performance -----------------------------------------------------------------------------------

fn synthetic_paths(n: usize) -> Vec<String> {
    let shapes = [
        r"C:\Users\lenny\Code{i}\src\module{m}\file{i}.py",
        r"C:\Users\lenny\Code{i}\node_modules\pkg{i}\dist\index.js",
        r"D:\SteamLibrary\steamapps\common\Game{m}\data\pak{i}.pak",
        r"C:\Users\lenny\AppData\Local\Google\Chrome\User Data\Default\Cache\Cache_Data\f_{i}",
        r"C:\Windows\System32\DriverStore\FileRepository\drv{i}.inf_amd64\file{i}.sys",
        "/home/lenny/src/proj{i}/.venv/lib/python3.12/site-packages/pkg{m}/mod{i}.py",
        "/home/lenny/.cache/pip/http/{m}/{i}",
        "/var/lib/docker/overlay2/{i}/diff/usr/share/doc/pkg{m}/README",
    ];
    (0..n)
        .map(|i| shapes[i % shapes.len()].replace("{i}", &i.to_string()).replace("{m}", &(i % 13).to_string()))
        .collect()
}

#[test]
fn lookup_speed() {
    let paths = synthetic_paths(10_000);
    let w = win();
    let found = paths.iter().filter(|p| w.match_path(p, Kind::File, None, None).is_some()).count();
    assert!(found > 5000, "{found}");
    let (mut best, mut best_many) = (f64::INFINITY, f64::INFINITY);
    for _ in 0..3 {
        let t0 = Instant::now();
        for p in &paths {
            w.match_path(p, Kind::File, None, None);
        }
        best = best.min(t0.elapsed().as_secs_f64());
        let t0 = Instant::now();
        w.match_many(&paths, Kinds::One(Kind::File), None, None).unwrap();
        best_many = best_many.min(t0.elapsed().as_secs_f64());
    }
    // debug builds are slower than release ones
    let budget = if cfg!(debug_assertions) { 2.0 } else { 0.2 };
    assert!(best < budget, "10k lookups took {best:.3} s");
    assert!(best_many < budget, "match_many over 10k paths took {best_many:.3} s");
}
