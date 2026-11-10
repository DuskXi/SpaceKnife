//! The real `spaceknife-cli` as a subprocess (`CARGO_BIN_EXE_spaceknife-cli`): port of 0.3.4's
//! `tests/test_serve_process.py` (token on stdin, exit on stdin EOF, the ready file, a pooled
//! scan while stdin is watched), `tests/test_cli_startup.py` (the scan's output and exit status,
//! the language read without the settings store, the largest files) and the NUL-console fix
//! (fix/cli-nul-console `test_cli_output.py`: output to the null device or a closed pipe never
//! costs the scan or its snapshot), and of `tests/test_cli_scan_mft.py` (`--mft` falls back to a
//! hard scan with the reason; `--disk-limit` paces the $MFT read).

mod common;

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use common::{Client, all_caps, build_tree};
use serde_json::{Value, json};

const TOKEN: &str = "subprocess-token-0123456789abcdef";

fn cli() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_spaceknife-cli"));
    for (k, _) in std::env::vars() {
        if k.starts_with("SPACEKNIFE_") {
            c.env_remove(k);
        }
    }
    c
}

/// `serve` with a private home; stdin/stdout piped.
fn spawn_serve(home: &Path, args: &[&str]) -> Child {
    cli()
        .arg("serve")
        .args(args)
        .env("SPACEKNIFE_HOME", home)
        .env("SPACEKNIFE_LANGUAGE", "en")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spaceknife-cli starts")
}

fn ready_line(child: &mut Child) -> Value {
    let out = child.stdout.take().unwrap();
    let mut line = String::new();
    BufReader::new(out).read_line(&mut line).unwrap();
    assert!(!line.is_empty(), "the backend exited before its ready line");
    let ready: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(ready["event"], "ready");
    assert!(
        ready["url"]
            .as_str()
            .unwrap()
            .starts_with("ws://127.0.0.1:")
    );
    ready
}

fn wait_exit(child: &mut Child, secs: u64) -> Option<i32> {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if let Some(st) = child.try_wait().unwrap() {
            return st.code();
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    None
}

#[tokio::test]
async fn token_on_stdin_and_exit_on_stdin_eof() {
    let home = tempfile::tempdir().unwrap();
    let mut child = spawn_serve(home.path(), &["--token-stdin"]);
    {
        let stdin = child.stdin.as_mut().unwrap();
        writeln!(stdin, "{TOKEN}").unwrap();
        stdin.flush().unwrap();
    }
    let ready = ready_line(&mut child);
    assert!(ready.get("token").is_none(), "{ready}");
    let c = Client::open_with(ready["url"].as_str().unwrap(), TOKEN, all_caps(), None)
        .await
        .unwrap();
    assert_eq!(
        c.ok("system.ping", json!({"payload": "e2e"})).await["payload"],
        "e2e"
    );
    c.close().await;
    drop(child.stdin.take()); // EOF
    assert_eq!(wait_exit(&mut child, 15), Some(0));
}

#[tokio::test]
async fn generated_token_and_app_shutdown() {
    let home = tempfile::tempdir().unwrap();
    let mut child = spawn_serve(home.path(), &[]);
    let ready = ready_line(&mut child);
    let token = ready["token"].as_str().unwrap().to_owned();
    assert!(token.len() >= 32);
    let c = Client::open_with(ready["url"].as_str().unwrap(), &token, all_caps(), None)
        .await
        .unwrap();
    assert_eq!(c.ok("app.shutdown", json!({})).await["accepted"], true);
    assert_eq!(wait_exit(&mut child, 15), Some(0));
}

#[tokio::test]
async fn ready_file_lets_another_process_attach() {
    let home = tempfile::tempdir().unwrap();
    let file = home.path().join("conn").join("ready.json");
    let mut child = spawn_serve(home.path(), &["--ready-file", file.to_str().unwrap()]);
    let ready = ready_line(&mut child);
    let info: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(info["url"], ready["url"]);
    assert_eq!(info["token"], ready["token"]);
    let c = Client::open_with(
        info["url"].as_str().unwrap(),
        info["token"].as_str().unwrap(),
        all_caps(),
        None,
    )
    .await
    .unwrap();
    assert_eq!(c.ok("app.shutdown", json!({})).await["accepted"], true);
    assert_eq!(wait_exit(&mut child, 15), Some(0));
    assert!(!file.exists(), "the ready file goes with the process");
}

/// Regression (Windows): a blocking read of stdin stalled starting the scan's workers.
#[tokio::test]
async fn pooled_scan_with_token_on_stdin() {
    let home = tempfile::tempdir().unwrap();
    let facts = build_tree(&home.path().join("root"));
    let mut child = spawn_serve(home.path(), &["--token-stdin"]);
    {
        let stdin = child.stdin.as_mut().unwrap();
        writeln!(stdin, "{TOKEN}").unwrap();
    }
    let ready = ready_line(&mut child);
    let c = Client::open_with(ready["url"].as_str().unwrap(), TOKEN, all_caps(), None)
        .await
        .unwrap();
    let st = c
        .ok(
            "scan.start",
            json!({"roots": [facts.root], "workers": 2, "auto_snapshot": false}),
        )
        .await;
    let id = st["scan_id"].as_str().unwrap().to_owned();
    let mut status = st;
    for _ in 0..240 {
        status = c.ok("scan.status", json!({"scan_id": id})).await;
        if status["state"] != "queued" && status["state"] != "scanning" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert_eq!(status["state"], "done");
    assert_eq!(status["files"], facts.files);
    c.close().await;
    drop(child.stdin.take());
    assert_eq!(wait_exit(&mut child, 15), Some(0));
}

fn snapshots_in(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "sks") {
                out.push(p);
            }
        }
    }
    out
}

/// The snapshot is complete on disk: its header's node count is the tree's.
fn assert_complete(snap: &Path) {
    let (store, header) = sk_store::snapshot::load(snap).unwrap();
    assert_eq!(header.node_count as usize, store.read().len());
    assert!(store.read().len() > 1);
}

#[test]
fn scan_exits_with_its_output_complete() {
    let tmp = tempfile::tempdir().unwrap();
    let facts = build_tree(&tmp.path().join("root"));
    let home = tmp.path().join("home");
    let run = |args: &[&str]| {
        cli()
            .args(args)
            .env("SPACEKNIFE_HOME", &home)
            .env("SPACEKNIFE_LANGUAGE", "zh-CN")
            .env("COLUMNS", "120")
            .output()
            .unwrap()
    };
    let root = facts.root.to_str().unwrap();
    let out = run(&["scan", root, "--top", "3"]);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(out.status.code(), Some(0), "{text}");
    let tail = &text[text.find("最大的文件").expect("the largest files table")..];
    assert!(tail.contains("movie.mp4"), "{text}");
    let snaps = snapshots_in(&home);
    assert_eq!(snaps.len(), 1, "{snaps:?}");
    assert_complete(&snaps[0]);
    let missing = tmp.path().join("missing");
    let bad = run(&["scan", missing.to_str().unwrap()]);
    assert_eq!(bad.status.code(), Some(2));
    let pooled = run(&[
        "scan",
        root,
        "--workers",
        "2",
        "--no-snapshot",
        "--top",
        "3",
    ]);
    let text = String::from_utf8_lossy(&pooled.stdout).into_owned();
    assert_eq!(pooled.status.code(), Some(0), "{text}");
    assert!(text[text.find("最大的文件").unwrap()..].contains("movie.mp4"));
}

/// `spaceknife scan <folder> > NUL` (`>/dev/null` in Git Bash): the null device is not a console
/// and the scan ends normally with its snapshot.
#[test]
fn scan_with_stdout_to_the_null_device() {
    let tmp = tempfile::tempdir().unwrap();
    let facts = build_tree(&tmp.path().join("root"));
    let home = tmp.path().join("home");
    let null = std::fs::OpenOptions::new()
        .write(true)
        .open(if cfg!(windows) { "NUL" } else { "/dev/null" })
        .unwrap();
    let out = cli()
        .args(["scan", facts.root.to_str().unwrap(), "--top", "3"])
        .env("SPACEKNIFE_HOME", &home)
        .stdout(Stdio::from(null))
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{err}");
    assert!(!err.contains("panicked"), "{err}");
    let snaps = snapshots_in(&home);
    assert_eq!(snaps.len(), 1);
    assert_complete(&snaps[0]);
}

/// The output fails (the pipe is closed before the scan writes): the auto snapshot and the
/// `--save` file are saved all the same.
#[test]
fn a_failed_output_keeps_the_snapshots() {
    let tmp = tempfile::tempdir().unwrap();
    let facts = build_tree(&tmp.path().join("root"));
    let home = tmp.path().join("home");
    let saved = tmp.path().join("saved.sks");
    let mut child = cli()
        .args([
            "scan",
            facts.root.to_str().unwrap(),
            "--top",
            "3",
            "--save",
            saved.to_str().unwrap(),
        ])
        .env("SPACEKNIFE_HOME", &home)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    drop(child.stdout.take()); // nobody reads: every write fails
    let code = wait_exit(&mut child, 60);
    assert!(code.is_some(), "the scan ended");
    assert!(saved.exists(), "--save was written (exit {code:?})");
    assert_complete(&saved);
    let snaps = snapshots_in(&home);
    assert_eq!(
        snaps.len(),
        1,
        "the auto snapshot was written (exit {code:?})"
    );
    assert_complete(&snaps[0]);
}

/// `stored_language` (the scan's fast path) gives what the settings store would: a file the store
/// rejects as invalid (another value out of range, a version that is not a number) leaves the
/// defaults there, so the scan speaks the system's language too, as in 0.3.4.
#[test]
fn stored_language_reads_what_the_settings_store_reads() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("settings.json");
    let cases: [Option<&str>; 16] = [
        None,
        Some(""),
        Some("{not json"),
        Some("[]"),
        Some(r#"{"values": []}"#),
        Some(r#"{"values": null}"#),
        Some("{}"),
        Some(r#"{"version": 1, "values": {}}"#),
        Some(r#"{"values": {"general.language": "system"}}"#),
        Some(r#"{"values": {"general.language": "en"}}"#),
        Some(r#"{"values": {"general.language": "zh-CN", "general.hover_dates": false}}"#),
        // an invalid file: the store falls back to the defaults
        Some(r#"{"values": {"general.language": "en", "general.hover_dates": "no"}}"#),
        Some(r#"{"values": {"general.language": "fr"}}"#),
        Some(r#"{"values": {"general.language": 5}}"#),
        Some(r#"{"version": "x", "values": {"general.language": "en"}}"#),
        Some(r#"{"values": {"general.language": "system", "general.hover_dates": "no"}}"#),
    ];
    let mut wrong = Vec::new();
    for content in cases {
        let _ = std::fs::remove_file(&file);
        if let Some(c) = content {
            std::fs::write(&file, c).unwrap();
        }
        let fast = sk_server::cli::stored_language(&file).unwrap_or_else(|| "system".into());
        let store =
            sk_core::settings::SettingsStore::new(Some(file.clone())).get_str("general.language");
        if fast != store {
            wrong.push(format!("{content:?}: fast {fast}, store {store}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn the_fast_language_path_assumes_the_default() {
    let tmp = tempfile::tempdir().unwrap();
    let store = sk_core::settings::SettingsStore::new(Some(tmp.path().join("none.json")));
    assert_eq!(store.get_str("general.language"), "system");
}

/// The largest files without a full sort equal the files sorted by allocation, ties by id.
#[test]
fn largest_matches_a_stable_sort() {
    for seed in [7u64, 8] {
        let store = sk_store::testing::random_store(seed, 300);
        let nodes = store.read();
        let mut files: Vec<(i64, u32)> = (0..nodes.len() as u32)
            .filter(|&i| nodes.kind(i) == sk_store::Kind::File)
            .map(|i| (nodes.allocs()[i as usize], i))
            .collect();
        // a stable sort by allocation, largest first: equal ones keep their id order
        files.sort_by_key(|f| std::cmp::Reverse(f.0));
        for k in [
            0usize,
            1,
            5,
            17,
            files.len().saturating_sub(1),
            files.len(),
            files.len() + 50,
        ] {
            let want: Vec<u32> = files.iter().take(k).map(|f| f.1).collect();
            let got = sk_server::cli::scan::largest(&nodes, k);
            assert_eq!(got, want, "seed {seed}, k {k}");
        }
    }
}

/// `--mft` on a folder (or off Windows): the scan says why it cannot read it that way and scans it
/// the hard way; the saved snapshot says hard.
#[test]
fn a_path_it_cannot_read_is_scanned_the_hard_way() {
    let tmp = tempfile::tempdir().unwrap();
    let facts = build_tree(&tmp.path().join("root"));
    let saved = tmp.path().join("root.sks");
    let out = cli()
        .args([
            "scan",
            facts.root.to_str().unwrap(),
            "--mft",
            "--no-snapshot",
            "--top",
            "3",
            "--depth",
            "1",
            "--save",
            saved.to_str().unwrap(),
        ])
        .env("SPACEKNIFE_HOME", tmp.path().join("home"))
        .env("SPACEKNIFE_LANGUAGE", "zh-CN")
        .env("COLUMNS", "400")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(out.status.code(), Some(0), "{text}");
    let roots = sk_core::scan::Scanner::new(&[facts.root.to_str().unwrap()], Default::default())
        .unwrap()
        .roots();
    let reason = sk_server::cli::scan::mft_unsupported(&roots).expect("a folder cannot");
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let reason_flat = reason.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        flat.contains(&format!("不支持 MFT 快速扫描：{reason_flat}。改用硬扫描。")),
        "{text}"
    );
    assert!(
        flat.contains(&format!("方式 硬扫描（{reason_flat}）")),
        "{text}"
    );
    let header = sk_store::snapshot::read_header(&saved).unwrap();
    assert_eq!(header.stats["files"], facts.files);
    assert_eq!(header.stats["method"], "hard");
}

/// `--disk-limit` paces the $MFT read as the app maps its load levels.
#[test]
fn disk_limit_paces_the_mft_read() {
    use sk_server::cli::scan::{load_limit, mft_duty};
    let duty = |s: Option<&str>| mft_duty(s.map(|s| load_limit(s).unwrap()));
    assert_eq!(duty(None), 1.0);
    assert_eq!(duty(Some("max")), 1.0);
    assert_eq!(duty(Some("idle")), 0.2);
    assert!((duty(Some("quiet")) - 0.35).abs() < 1e-9);
    assert_eq!(duty(Some("2%")), 0.05);
    assert!(load_limit("101").is_err() && load_limit("").is_err());
}
