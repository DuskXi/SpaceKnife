//! A SpaceKnife 0.3.4 home, as the Python backend left it (settings, classes, folder classes, tags,
//! palette recents, update state, volume summaries and auto-saved snapshots), opened by this
//! server: everything reads back over the protocol.
//!
//! The home is real 0.3.4 output (`crates/sk-core/tests/data/v034/<platform>/home`, see
//! `tools/fixtures/v034_fixtures.py`); `facts.json` beside it says what was put in.

mod common;

use std::path::{Path, PathBuf};

use common::*;
use serde_json::{Value, json};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../sk-core/tests/data/v034")
        .join(if cfg!(windows) { "windows" } else { "posix" })
}

fn facts() -> Value {
    serde_json::from_slice(&std::fs::read(fixtures().join("facts.json")).unwrap()).unwrap()
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dst = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_tree(&e.path(), &dst);
        } else {
            std::fs::copy(e.path(), &dst).unwrap();
        }
    }
}

fn roots_of(facts: &Value, name: &str) -> Vec<String> {
    serde_json::from_value(facts["roots"][name].clone()).unwrap()
}

#[tokio::test]
async fn a_0_3_4_home_reads_back_over_the_protocol() {
    let home = init();
    copy_tree(&fixtures().join("home"), &home);
    let facts = facts();
    let srv = server().await;
    let c = Client::open(&srv.url).await;

    // settings: every value 0.3.4 changed
    let st = c.ok("settings.get", json!({})).await;
    assert!(
        st["error"].as_str().unwrap_or("").is_empty(),
        "{}",
        st["error"]
    );
    for (key, want) in facts["settings"].as_object().unwrap() {
        assert_eq!(&st["values"][key], want, "{key}");
    }

    // the file classes and the folders' classes
    let cl = c.ok("classes.get", json!({})).await;
    assert_eq!(cl["custom"], true);
    let ids: Vec<&str> = cl["classes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    let want: Vec<&str> = facts["classes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, want);
    let mut folders: Vec<(String, String)> = cl["folders"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["path"].as_str().unwrap().to_owned(),
                f["class_id"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    folders.sort();
    let mut want: Vec<(String, String)> = serde_json::from_value(facts["folders"].clone()).unwrap();
    want.sort();
    assert_eq!(folders, want);

    // every auto-saved snapshot is listed (start page, compare, history)
    let base = home.join("data").join("snapshots");
    let files = std::fs::read_dir(&base)
        .unwrap()
        .flat_map(|d| std::fs::read_dir(d.unwrap().path()).unwrap())
        .count();
    let listed = c.ok("snapshot.list", json!({})).await;
    assert_eq!(listed["snapshots"].as_array().unwrap().len(), files);

    // the plain tree's snapshot opens, with its tags and its palette jumps
    let auto = &facts["auto"]["plain"];
    let path = base
        .join(auto["key"].as_str().unwrap())
        .join(auto["file"].as_str().unwrap());
    let loaded = c
        .ok("snapshot.load", json!({"path": path.display().to_string()}))
        .await;
    assert_eq!(loaded["source"], "snapshot");
    assert_eq!(loaded["state"], "done");
    let scan_id = loaded["scan_id"].as_str().unwrap().to_owned();
    let root = roots_of(&facts, "plain").remove(0);
    let sep = if cfg!(windows) { "\\" } else { "/" };
    for (rel, color) in facts["tags"].as_object().unwrap() {
        let target = format!("{root}{sep}{}", rel.replace('/', sep));
        let found = c
            .ok("tree.find", json!({"scan_id": scan_id, "path": target}))
            .await;
        let node = found["node"]["id"].clone();
        assert!(node.is_u64(), "{target}");
        let s = c
            .ok(
                "tree.summary",
                json!({"scan_id": scan_id, "node": node, "top": 1}),
            )
            .await;
        assert_eq!(&s["tag"], color, "{rel}");
    }
    let u = c
        .ok(
            "view.open",
            json!({"scan_id": scan_id, "viewport": {"width": 800, "height": 600}}),
        )
        .await;
    let view_id = u["state"]["view_id"].as_str().unwrap().to_owned();
    let res = c
        .ok(
            "palette.query",
            json!({"text": "", "context": {"tab": "scan", "scan_id": scan_id, "view_id": view_id}}),
        )
        .await;
    let recent: Vec<(String, String)> = res["recent"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["kind"].as_str().unwrap().to_owned(),
                r["text"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    let jumps: Vec<String> = serde_json::from_value(facts["jumps"].clone()).unwrap();
    let filters: Vec<String> = serde_json::from_value(facts["filters"].clone()).unwrap();
    for j in &jumps {
        assert!(
            recent.contains(&("jump".into(), j.clone())),
            "{j}: {recent:?}"
        );
    }
    for f in &filters {
        assert!(
            recent.contains(&("filter".into(), f.clone())),
            "{f}: {recent:?}"
        );
    }

    // the stopped scan opens as stopped, its folders still to list; the monitored one without
    // its deleted rows
    for (name, state) in [
        ("stopped", "cancelled"),
        ("monitored", "done"),
        ("odd", "done"),
    ] {
        let auto = &facts["auto"][name];
        let path = base
            .join(auto["key"].as_str().unwrap())
            .join(auto["file"].as_str().unwrap());
        let st = c
            .ok("snapshot.load", json!({"path": path.display().to_string()}))
            .await;
        assert_eq!(st["state"], state, "{name}");
    }

    // the update state: the release cache and what was dismissed and muted
    let up = c.ok("update.status", json!({})).await;
    let at = chrono::DateTime::parse_from_rfc3339(up["checked_at"].as_str().unwrap()).unwrap();
    let want =
        chrono::DateTime::parse_from_rfc3339(facts["update"]["checked_at"].as_str().unwrap())
            .unwrap();
    assert_eq!(at, want);
    srv.shutdown();
}

/// The 「本机磁盘」 page's summaries 0.3.4 kept, found for their volumes.
#[test]
fn volume_summaries_of_0_3_4_read_back() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("volume_summaries.json");
    std::fs::copy(fixtures().join("home/data/volume_summaries.json"), &file).unwrap();
    let store = sk_server::volume_summaries::SummaryStore::open(Some(file));
    let facts = facts();
    let entries = facts["volume_summaries"]["volumes"].as_array().unwrap();
    assert_eq!(store.len(), entries.len());
    for e in entries {
        let identity = e["identity"].as_str().unwrap();
        let mut v = sk_core::volume::VolumeInfo {
            mount_point: e["mount"].as_str().unwrap().into(),
            ..Default::default()
        };
        if let Some(serial) = identity.strip_prefix("serial:") {
            v.serial = serial.into();
        } else {
            let (fs, total) = identity.split_once(':').unwrap();
            v.fs_type = fs.into();
            v.total = total.parse().unwrap();
        }
        let kept = store.get(&v).unwrap_or_else(|| panic!("{identity}"));
        assert_eq!(kept.files, e["files"].as_i64().unwrap());
        assert_eq!(kept.bytes_alloc, e["bytes_alloc"].as_i64().unwrap());
        assert_eq!(kept.snapshot.as_deref(), e["snapshot"].as_str());
        assert_eq!(serde_json::to_value(&kept.classes).unwrap(), e["classes"]);
        assert_eq!(serde_json::to_value(&kept.folders).unwrap(), e["folders"]);
    }
}
