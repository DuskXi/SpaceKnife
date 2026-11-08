//! The backend runs inside the window program (Tauri), whose toolkit threads (GTK, WebKitGTK,
//! GDBus) read the environment with `getenv` at any time: changing it meanwhile is undefined
//! behaviour on glibc. So no code that ships changes its process's environment; values go as
//! arguments instead (`ServeOptions::system_locale` → `sk_core::i18n::set_system_locale`, a
//! portable copy's data folder → `sk_core::paths::set_home`, the helper's `--system-locale`).
//! Tests are their own processes and may: every Rust file under `src/` of the workspace's crates and
//! of the desktop shell is checked, `tests/` folders, examples and build scripts are not.

use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .find(|d| {
            std::fs::read_to_string(d.join("Cargo.toml"))
                .is_ok_and(|t| t.lines().any(|l| l.trim() == "[workspace]"))
        })
        .expect("the workspace's Cargo.toml")
        .to_path_buf()
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            if e.file_name() != "tests" {
                rust_files(&p, out);
            }
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// `(line number, line)` of every call that changes the environment in `src` (comments skipped).
fn environment_changes(src: &str) -> Vec<(usize, String)> {
    src.lines()
        .enumerate()
        .filter(|(_, line)| {
            let code = line.split("//").next().unwrap_or("");
            code.contains("set_var(") || code.contains("remove_var(")
        })
        .map(|(i, line)| (i + 1, line.trim().to_owned()))
        .collect()
}

#[test]
fn shipped_code_never_changes_the_environment() {
    let root = workspace_root();
    let mut sources = vec![root.join("apps/desktop/src-tauri/src")];
    for e in std::fs::read_dir(root.join("crates")).unwrap().flatten() {
        let src = e.path().join("src");
        if src.is_dir() {
            sources.push(src);
        }
    }
    let mut files = Vec::new();
    for s in &sources {
        assert!(s.is_dir(), "{} is missing", s.display());
        rust_files(s, &mut files);
    }
    assert!(files.len() > 100, "only {} files found", files.len());
    let mut found = Vec::new();
    for f in &files {
        let text = std::fs::read_to_string(f).unwrap();
        for (n, line) in environment_changes(&text) {
            let rel = f.strip_prefix(&root).unwrap_or(f);
            found.push(format!("{}:{n}: {line}", rel.display()));
        }
    }
    assert!(
        found.is_empty(),
        "code that ships changes the process environment (pass the value as an argument):\n{}",
        found.join("\n")
    );
}

#[test]
fn the_check_finds_what_it_looks_for() {
    let src = "fn a() {\n    // std::env::set_var(\"X\", \"1\") would be wrong\n    unsafe { std::env::set_var(\"X\", \"1\") };\n    let _ = 1; // remove_var( in a comment\n    unsafe { env::remove_var(\"X\") }\n}\n";
    let found: Vec<usize> = environment_changes(src).iter().map(|(n, _)| *n).collect();
    assert_eq!(found, [3, 5]);
}
