//! Port of tests/test_compare_server.py: snapshot vs. now over the real WebSocket, the change tree
//! view, summary, hover card. (The pure change-tree and short-label tests live in sk-core's diff
//! tests.)

mod common;

use std::path::Path;
use std::time::Duration;

use common::*;
use serde_json::{Value, json};
use sk_protocol::binary::decode_frame;
use sk_protocol::compare::DSTAT_NEW;

const BIG: usize = 300_000;

/// tests/tree_fixture.py `build_tree`.
fn build_tree(root: &Path) {
    let mut files: Vec<(String, usize)> = vec![
        ("a/one.bin".into(), 1000),
        ("a/two.bin".into(), 5000),
        ("a/sub/movie.mp4".into(), 123_456),
        ("c.txt".into(), 10),
        ("中文 文件夹/照片.jpg".into(), 2048),
        ("deep/l1/l2/l3/l4/x.dat".into(), 777),
        ("empty.dat".into(), 0),
    ];
    for i in 0..300 {
        files.push((format!("many/f{i:03}.log"), 100 + i));
    }
    let refs: Vec<(&str, usize)> = files.iter().map(|(p, s)| (p.as_str(), *s)).collect();
    make_tree(root, &refs);
    std::fs::create_dir_all(root.join("b/inner")).unwrap();
}

/// +BIG new file, a/two.bin deleted, a/one.bin grown by 9000 bytes.
fn change_files(root: &Path) {
    std::fs::remove_file(root.join("a/two.bin")).unwrap();
    std::fs::write(root.join("a/one.bin"), vec![b'y'; 10_000]).unwrap();
    std::fs::create_dir_all(root.join("new")).unwrap();
    std::fs::write(root.join("new/big.iso"), vec![b'z'; BIG]).unwrap();
}

/// A scan with its auto snapshot.
async fn scan_saved(c: &Client, root: &Path) -> Value {
    let st = c
        .ok(
            "scan.start",
            json!({"roots": [root.to_string_lossy()], "workers": 0, "auto_snapshot": true}),
        )
        .await;
    let id = st["scan_id"].as_str().unwrap().to_owned();
    let mut st = wait_done(c, &id).await;
    for _ in 0..100 {
        if !st["snapshot"].is_null() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        st = c.ok("scan.status", json!({"scan_id": id})).await;
    }
    st
}

async fn compared(c: &Client, scan_id: &str, base: &str, target: &str) -> Value {
    let task = c
        .ok(
            "compare.open",
            json!({"scan_id": scan_id, "base": base, "target": target}),
        )
        .await;
    let id = task["compare_id"].as_str().unwrap().to_owned();
    let ev = c
        .event_where("compare.progress", 60.0, |e| {
            e["compare_id"] == id.as_str()
                && ["ready", "failed", "cancelled"].contains(&e["phase"].as_str().unwrap())
        })
        .await;
    assert_eq!(ev["phase"], "ready", "{}", ev["message"]);
    assert!(!ev["info"].is_null());
    ev["info"].clone()
}

struct Frame {
    names: Vec<String>,
    keys: Vec<u32>,
    delta: Option<Vec<i64>>,
    dstat: Option<Vec<u8>>,
}

fn frame_of(buf: &[u8]) -> Frame {
    let f = decode_frame(buf).unwrap();
    let lens: Vec<u32> = f.column("name_len").unwrap().to_vec().unwrap();
    let blob = f.blob("names").unwrap();
    let mut at = 0;
    let names = lens
        .iter()
        .map(|&l| {
            let s = String::from_utf8_lossy(&blob[at..at + l as usize]).into_owned();
            at += l as usize;
            s
        })
        .collect();
    Frame {
        names,
        keys: f.column("key").unwrap().to_vec().unwrap(),
        delta: f.column("delta").map(|c| c.to_vec().unwrap()),
        dstat: f.column("dstat").map(|c| c.to_vec().unwrap()),
    }
}

/// A view call: its update and the binary frame of it (if one came).
async fn view_call(c: &Client, method: &str, params: Value) -> (Value, Option<Frame>) {
    c.drain_binary().await;
    let up = c.ok(method, params).await;
    let frame = if up["frame"].is_null() {
        None
    } else {
        Some(frame_of(&c.binary(10.0).await))
    };
    (up, frame)
}

#[tokio::test]
async fn snapshot_against_now() {
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    build_tree(&root);
    let first = scan_saved(&c, &root).await;
    let snap = first["snapshot"]
        .as_str()
        .expect("auto snapshot")
        .to_owned();
    change_files(&root);
    tokio::time::sleep(Duration::from_millis(1100)).await; // snapshot names have a 1 s resolution
    let now = scan_saved(&c, &root).await;
    let scan_id = now["scan_id"].as_str().unwrap();

    let sources = c.ok("compare.sources", json!({"scan_id": scan_id})).await;
    let paths: Vec<&str> = sources["snapshots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["path"].as_str().unwrap())
        .collect();
    assert!(paths.contains(&snap.as_str()));
    assert_eq!(sources["now"]["kind"], "now");

    let info = compared(&c, scan_id, &snap, "now").await;
    let grow = info["grow"].as_i64().unwrap();
    assert!(grow > 0 && info["shrink"].as_i64().unwrap() > 0);
    assert_eq!(info["base"]["kind"], "snapshot");
    assert_eq!(info["target"]["kind"], "now");
    let cmp = info["compare_id"].as_str().unwrap().to_owned();

    // the change tree as a view: areas are |change|, frames carry delta and dstat
    let (up, frame) = view_call(
        &c,
        "view.open",
        json!({"scan_id": cmp, "viewport": {"width": 900, "height": 600}}),
    )
    .await;
    let frame = frame.expect("a frame");
    assert_eq!(up["state"]["compare"], true);
    assert!(!frame.names.iter().any(|n| n == "many")); // unchanged folders are not in the change tree
    assert!(frame.names.iter().any(|n| n == "new") && frame.names.iter().any(|n| n == "a"));
    let new_row = frame.names.iter().position(|n| n == "new").unwrap();
    assert!(frame.delta.as_ref().unwrap()[new_row] > 0);
    assert_eq!(frame.dstat.as_ref().unwrap()[new_row], DSTAT_NEW);
    let view_id = up["state"]["view_id"].as_str().unwrap().to_owned();

    let summary = c.ok("compare.summary", json!({"view_id": view_id})).await;
    assert_eq!(summary["whole"], true);
    assert_eq!(
        summary["net"].as_i64().unwrap(),
        summary["grow"].as_i64().unwrap() - summary["shrink"].as_i64().unwrap()
    );
    assert_eq!(summary["grow"].as_i64().unwrap(), grow);
    let top = &summary["grow_items"][0];
    assert_eq!(top["label"], "big.iso");
    assert_eq!(top["status"], "new");
    assert_eq!(top["on_disk"], true);
    let shrink: Vec<&Value> = summary["shrink_items"].as_array().unwrap().iter().collect();
    assert_eq!(
        shrink
            .iter()
            .map(|i| i["status"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["deleted"]
    );
    assert!(shrink[0]["path"].as_str().unwrap().ends_with("two.bin"));
    assert_eq!(shrink[0]["on_disk"], false);

    // hover card of folder a: one grown and one deleted child
    let a_key = frame.keys[frame.names.iter().position(|n| n == "a").unwrap()];
    let card = c
        .ok("compare.node", json!({"view_id": view_id, "key": a_key}))
        .await;
    assert_eq!(card["item"]["kind"], "dir");
    let mut st: Vec<&str> = card["children"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k["status"].as_str().unwrap())
        .collect();
    st.sort_unstable();
    st.dedup();
    assert_eq!(st, ["changed", "deleted"]);

    // 「增长」 only: the deleted file leaves the layout
    let (up, frame) = view_call(
        &c,
        "view.set",
        json!({"view_id": view_id, "settings": {"direction": "grow"}}),
    )
    .await;
    assert_eq!(up["state"]["direction"], "grow");
    assert!(!frame.expect("a frame").names.iter().any(|n| n == "two.bin"));
    // a view opened like it keeps showing growth only
    let (up, frame) = view_call(
        &c,
        "view.open",
        json!({"scan_id": cmp, "viewport": {"width": 900, "height": 600}, "settings": {"direction": "grow"}}),
    )
    .await;
    assert_eq!(up["state"]["direction"], "grow");
    assert!(!frame.expect("a frame").names.iter().any(|n| n == "two.bin"));

    // tree.node on a change tree row
    let node = c
        .ok("tree.node", json!({"scan_id": cmp, "node": a_key}))
        .await;
    assert_eq!(node["node"]["name"], "a");
    let info2 = c.ok("compare.info", json!({"compare_id": cmp})).await;
    assert_eq!(info2["grow"].as_i64().unwrap(), grow);

    c.ok("compare.close", json!({"compare_id": cmp})).await;
    c.err("compare.summary", json!({"view_id": view_id})).await;
}

#[tokio::test]
async fn errors() {
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    build_tree(&root);
    let st = scan_saved(&c, &root).await;
    let other = tmp.path().join("other");
    build_tree(&other);
    let elsewhere = scan_saved(&c, &other).await;
    let scan_id = st["scan_id"].as_str().unwrap();
    // both ends the same
    c.err(
        "compare.open",
        json!({"scan_id": scan_id, "base": "now", "target": "now"}),
    )
    .await;
    // a snapshot of another root fails with a clear message
    let task = c
        .ok(
            "compare.open",
            json!({"scan_id": scan_id, "base": elsewhere["snapshot"], "target": "now"}),
        )
        .await;
    let id = task["compare_id"].as_str().unwrap().to_owned();
    let ev = c
        .event_where("compare.progress", 30.0, |e| {
            e["compare_id"] == id.as_str() && e["phase"] == "failed"
        })
        .await;
    assert!(
        ev["message"].as_str().unwrap().contains("扫描位置"),
        "{}",
        ev["message"]
    );
    // not ready / unknown
    let e = c.err("compare.info", json!({"compare_id": id})).await;
    assert_eq!(
        e.code,
        i64::from(sk_protocol::ErrorCode::InvalidRequest.code())
    );
    let e = c
        .err("compare.info", json!({"compare_id": "cmp-999"}))
        .await;
    assert_eq!(
        e.code,
        i64::from(sk_protocol::ErrorCode::UnknownScan.code())
    );
}

// ---------------------------------------------------------------------------------- the delete basket

fn key_of(frame: &Frame, name: &str) -> u32 {
    frame.keys[frame
        .names
        .iter()
        .position(|n| n == name)
        .unwrap_or_else(|| panic!("{name} not in the frame: {:?}", frame.names))]
}

fn basket_paths(state: &Value) -> Vec<String> {
    state["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["path"].as_str().unwrap().to_owned())
        .collect()
}

/// Blocks of a compare view go into the delete basket as the same items of the tab's live tree:
/// one still on disk is added (to the live scan), one the comparison shows as deleted is refused.
/// (They were all refused.)
#[tokio::test]
async fn compare_view_blocks_go_into_the_basket() {
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    build_tree(&root);
    let first = scan_saved(&c, &root).await;
    let snap = first["snapshot"]
        .as_str()
        .expect("auto snapshot")
        .to_owned();
    change_files(&root);
    tokio::time::sleep(Duration::from_millis(1100)).await; // snapshot names have a 1 s resolution
    let now = scan_saved(&c, &root).await;
    let scan_id = now["scan_id"].as_str().unwrap().to_owned();
    let info = compared(&c, &scan_id, &snap, "now").await;
    let cmp = info["compare_id"].as_str().unwrap().to_owned();
    let (up, frame) = view_call(
        &c,
        "view.open",
        json!({"scan_id": cmp, "viewport": {"width": 900, "height": 600}}),
    )
    .await;
    let frame = frame.expect("a frame");
    let view_id = up["state"]["view_id"].as_str().unwrap().to_owned();
    c.ok("basket.clear", json!({})).await;

    // by node: the new folder is on disk, the deleted file is not
    let summary = c.ok("compare.summary", json!({"view_id": view_id})).await;
    let gone = &summary["shrink_items"][0];
    assert!(
        gone["path"].as_str().unwrap().ends_with("two.bin"),
        "{summary}"
    );
    let (new_key, gone_key) = (key_of(&frame, "new"), gone["node"].as_u64().unwrap());
    let added = c
        .ok(
            "basket.add",
            json!({"scan_id": cmp, "nodes": [new_key, gone_key]}),
        )
        .await;
    assert_eq!(added["added"], 1, "{added}");
    let refused = added["refused"].as_array().unwrap();
    assert_eq!(refused.len(), 1, "{added}");
    assert!(refused[0]["path"].as_str().unwrap().ends_with("two.bin"));
    assert_eq!(refused[0]["reason"], "只能操作「现在」还在磁盘上的项");
    let state = c.ok("basket.list", json!({})).await;
    assert_eq!(
        basket_paths(&state),
        [root.join("new").to_string_lossy().into_owned()]
    );
    assert_eq!(state["items"][0]["scan_id"], scan_id.as_str()); // the live scan's item
    assert!(state["items"][0]["alloc"].as_i64().unwrap() > 0);

    // by block of the view
    c.ok("basket.clear", json!({})).await;
    let a_key = key_of(&frame, "a");
    let added = c
        .ok(
            "basket.add",
            json!({"scan_id": cmp, "view_id": view_id, "key": a_key}),
        )
        .await;
    assert_eq!(added["added"], 1, "{added}");
    let state = c.ok("basket.list", json!({})).await;
    assert_eq!(
        basket_paths(&state),
        [root.join("a").to_string_lossy().into_owned()]
    );
    c.ok("basket.clear", json!({})).await;
}

/// Makes `folder` hold names that differ only in case, or says it cannot (Windows: a
/// case-sensitive NTFS folder; elsewhere the file system decides).
fn case_sensitive(folder: &Path) -> bool {
    std::fs::create_dir_all(folder).unwrap();
    if cfg!(windows) {
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
    }
    twin
}

/// A compare view shows `A/f` beside `a/f` (names that differ only in case): put into the
/// basket, it is the `A/f` of the live tree, not its twin.
#[tokio::test]
async fn a_compare_view_finds_the_twin_it_shows() {
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    if !case_sensitive(&root) {
        eprintln!(
            "skipped: {} cannot hold names that differ only in case",
            root.display()
        );
        return;
    }
    make_tree(&root, &[("a/f", 10), ("A/f", 20), ("keep.txt", 1)]);
    let first = scan_saved(&c, &root).await;
    let snap = first["snapshot"]
        .as_str()
        .expect("auto snapshot")
        .to_owned();
    std::fs::write(root.join("A").join("f"), vec![b'x'; 50_000]).unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let now = scan_saved(&c, &root).await;
    let scan_id = now["scan_id"].as_str().unwrap().to_owned();
    let info = compared(&c, &scan_id, &snap, "now").await;
    let cmp = info["compare_id"].as_str().unwrap().to_owned();
    let (_, frame) = view_call(
        &c,
        "view.open",
        json!({"scan_id": cmp, "viewport": {"width": 900, "height": 600}}),
    )
    .await;
    let frame = frame.expect("a frame");
    // the changed folder is `A` (only it grew); its file is `f`
    let a_key = key_of(&frame, "A");
    let f_key = key_of(&frame, "f");
    c.ok("basket.clear", json!({})).await;
    let added = c
        .ok("basket.add", json!({"scan_id": cmp, "nodes": [f_key]}))
        .await;
    assert_eq!(added["added"], 1, "{added}");
    let state = c.ok("basket.list", json!({})).await;
    assert_eq!(
        basket_paths(&state),
        [root.join("A").join("f").to_string_lossy().into_owned()]
    );
    c.ok("basket.clear", json!({})).await;
    let added = c
        .ok("basket.add", json!({"scan_id": cmp, "nodes": [a_key]}))
        .await;
    assert_eq!(added["added"], 1, "{added}");
    let state = c.ok("basket.list", json!({})).await;
    assert_eq!(
        basket_paths(&state),
        [root.join("A").to_string_lossy().into_owned()]
    );
    c.ok("basket.clear", json!({})).await;
}
