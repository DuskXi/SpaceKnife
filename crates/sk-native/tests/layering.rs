//! The layering of the workspace: sk-native is the performance layer (hard scan, `$MFT`) with an
//! API of its own (README.md); the backend crates (sk-core, sk-store, sk-server) use it only through
//! that API. So sk-native depends on no other crate of this workspace — not as a normal, dev or
//! build dependency, not for any target, not through `[workspace.dependencies]` or under another
//! name — and on nothing by path.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use toml::{Table, Value};

fn read(path: &Path) -> Table {
    let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    text.parse()
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .find(|d| {
            d.join("Cargo.toml").is_file() && read(&d.join("Cargo.toml")).contains_key("workspace")
        })
        .expect("the workspace's Cargo.toml")
        .to_path_buf()
}

/// The package names of the workspace's members (`crates/*`, the desktop shell).
fn workspace_packages(root: &Path) -> BTreeSet<String> {
    let mut manifests: Vec<PathBuf> = fs::read_dir(root.join("crates"))
        .expect("crates/")
        .map(|e| e.expect("an entry of crates/").path().join("Cargo.toml"))
        .filter(|p| p.is_file())
        .collect();
    manifests.push(root.join("apps/desktop/src-tauri/Cargo.toml"));
    manifests
        .iter()
        .map(|m| {
            read(m)["package"]["name"]
                .as_str()
                .expect("a package name")
                .to_owned()
        })
        .collect()
}

/// Every dependency a manifest declares: (where, key, the package it names, its path if any).
/// `workspace = true` entries are resolved through `[workspace.dependencies]`.
fn dependencies(
    manifest: &Table,
    workspace_deps: &Table,
) -> Vec<(String, String, String, Option<String>)> {
    // with the old spellings dev_dependencies and build_dependencies
    const KINDS: [&str; 5] = [
        "dependencies",
        "dev-dependencies",
        "build-dependencies",
        "dev_dependencies",
        "build_dependencies",
    ];
    let mut tables: Vec<(String, &Table)> = Vec::new();
    for kind in KINDS {
        if let Some(t) = manifest.get(kind).and_then(Value::as_table) {
            tables.push((kind.to_owned(), t));
        }
    }
    if let Some(targets) = manifest.get("target").and_then(Value::as_table) {
        for (cfg, spec) in targets {
            for kind in KINDS {
                if let Some(t) = spec.get(kind).and_then(Value::as_table) {
                    tables.push((format!("target.{cfg}.{kind}"), t));
                }
            }
        }
    }
    let mut out = Vec::new();
    for (place, table) in tables {
        for (key, spec) in table {
            let spec = match spec {
                Value::Table(t) if t.get("workspace").and_then(Value::as_bool) == Some(true) => {
                    workspace_deps.get(key).unwrap_or_else(|| {
                        panic!(
                            "{place}.{key}: workspace = true, but not in [workspace.dependencies]"
                        )
                    })
                }
                other => other,
            };
            let (package, path) = match spec {
                Value::Table(t) => (
                    t.get("package")
                        .and_then(Value::as_str)
                        .unwrap_or(key)
                        .to_owned(),
                    t.get("path").and_then(Value::as_str).map(str::to_owned),
                ),
                _ => (key.clone(), None),
            };
            out.push((place.clone(), key.clone(), package, path));
        }
    }
    out
}

/// What is wrong with a manifest's dependencies: workspace crates and path dependencies.
fn violations(manifest: &Table, workspace: &Table, packages: &BTreeSet<String>) -> Vec<String> {
    let empty = Table::new();
    let workspace_deps = workspace
        .get("workspace")
        .and_then(|w| w.get("dependencies"))
        .and_then(Value::as_table)
        .unwrap_or(&empty);
    dependencies(manifest, workspace_deps)
        .into_iter()
        .filter_map(|(place, key, package, path)| {
            if packages.contains(&package) || package.starts_with("sk-") {
                Some(format!("{place}.{key}: the workspace crate {package}"))
            } else {
                path.map(|p| format!("{place}.{key}: a path dependency ({p})"))
            }
        })
        .collect()
}

#[test]
fn sk_native_depends_on_no_other_workspace_crate() {
    let root = workspace_root();
    let workspace = read(&root.join("Cargo.toml"));
    let packages = workspace_packages(&root);
    for name in [
        "sk-core",
        "sk-store",
        "sk-server",
        "sk-protocol",
        "spaceknife-desktop",
    ] {
        assert!(
            packages.contains(name),
            "{name} not found among {packages:?}"
        );
    }
    let me = read(&Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"));
    assert_eq!(me["package"]["name"].as_str(), Some("sk-native"));
    let wrong = violations(&me, &workspace, &packages);
    assert!(
        wrong.is_empty(),
        "sk-native must stay its own layer:\n  {}",
        wrong.join("\n  ")
    );
}

/// A source file without its `//` comments (doc comments included).
fn code_of(path: &Path) -> String {
    let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    text.lines()
        .map(|l| l.find("//").map_or(l, |i| &l[..i]))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The performance-critical work of the scans is this crate's: the backend's `$MFT` glue
/// (sk-core `scan/mft.rs`) and slice glue (`scan/mod.rs`) run nothing in parallel and write
/// nothing through raw pointers (the parallel tree build is [`sk_native::mft::build_tree`]),
/// and the store folds listings with [`sk_native::fold`] (no row writer of its own).
#[test]
fn the_hot_work_of_the_scans_is_in_this_crate() {
    let root = workspace_root();
    for rel in [
        "crates/sk-core/src/scan/mft.rs",
        "crates/sk-core/src/scan/mod.rs",
    ] {
        let code = code_of(&root.join(rel));
        for word in ["unsafe", "rayon", "par_iter", "ThreadPoolBuilder"] {
            assert!(
                !code.contains(word),
                "{rel} uses {word}: performance-critical code belongs in sk-native"
            );
        }
    }
    let store = code_of(&root.join("crates/sk-store/src/store.rs"));
    assert!(
        store.contains("sk_native::fold")
            && store.contains("SliceFold::plan")
            && store.contains("fold::write_rows"),
        "sk-store folds listings with sk_native::fold"
    );
    for gone in ["fn gone_listings", "fn write_rows", "fn sums("] {
        assert!(
            !store.contains(gone),
            "sk-store/src/store.rs has `{gone}`: the fold's row writing belongs in sk-native"
        );
    }
}

#[test]
fn the_layering_check_finds_every_way_in() {
    let workspace: Table = r#"
        [workspace.dependencies]
        sk-store = { path = "crates/sk-store" }
        store = { path = "crates/sk-store", package = "sk-store" }
        rayon = "1"
    "#
    .parse()
    .unwrap();
    let packages: BTreeSet<String> = ["sk-core", "sk-store", "sk-native"]
        .map(String::from)
        .into();
    let manifest: Table = r#"
        [package]
        name = "sk-native"
        [dependencies]
        rayon = { workspace = true }
        sk-store = { workspace = true }
        [dev-dependencies]
        helper = { path = "../helper" }
        [build-dependencies]
        sk-core = "0.3"
        [target."cfg(unix)".dependencies]
        store = { workspace = true }
        [target."cfg(windows)".dev_dependencies]
        core = { path = "../sk-core", package = "sk-core" }
    "#
    .parse()
    .unwrap();
    let mut wrong = violations(&manifest, &workspace, &packages);
    wrong.sort();
    assert_eq!(
        wrong,
        [
            "build-dependencies.sk-core: the workspace crate sk-core",
            "dependencies.sk-store: the workspace crate sk-store",
            "dev-dependencies.helper: a path dependency (../helper)",
            "target.cfg(unix).dependencies.store: the workspace crate sk-store",
            "target.cfg(windows).dev_dependencies.core: the workspace crate sk-core",
        ]
    );
    let clean: Table =
        "[package]\nname = \"sk-native\"\n[dependencies]\nrayon = { workspace = true }\n"
            .parse()
            .unwrap();
    assert!(violations(&clean, &workspace, &packages).is_empty());
}
