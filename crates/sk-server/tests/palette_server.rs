//! Port of tests/test_palette_server.py: palette.* over the WebSocket.

mod common;

use std::path::Path;

use common::*;
use serde_json::{Value, json};

/// Recents live in one file of the shared test home: one test at a time.
static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// tests/tree_fixture.py's tree.
fn build_tree(root: &Path) {
    let mut files: Vec<(String, usize)> = [
        ("a/one.bin", 1000),
        ("a/two.bin", 5000),
        ("a/sub/movie.mp4", 123456),
        ("c.txt", 10),
        ("中文 文件夹/照片.jpg", 2048),
        ("deep/l1/l2/l3/l4/x.dat", 777),
        ("empty.dat", 0),
    ]
    .into_iter()
    .map(|(a, b)| (a.to_owned(), b))
    .collect();
    files.extend((0..300).map(|i| (format!("many/f{i:03}.log"), 100 + i)));
    let refs: Vec<(&str, usize)> = files.iter().map(|(a, b)| (a.as_str(), *b)).collect();
    make_tree(root, &refs);
    for d in ["b", "b/inner"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
}

async fn scanned(tmp: &Path, c: &Client) -> String {
    let root = tmp.join("root");
    build_tree(&root);
    let st = scan(c, &root).await;
    st["scan_id"].as_str().unwrap().to_owned()
}

async fn open_view(c: &Client, scan_id: &str) -> String {
    let u = c
        .ok(
            "view.open",
            json!({"scan_id": scan_id, "viewport": {"width": 1000, "height": 700}}),
        )
        .await;
    u["state"]["view_id"].as_str().unwrap().to_owned()
}

async fn find(c: &Client, scan_id: &str, path: &Path) -> u64 {
    let r = c
        .ok(
            "tree.find",
            json!({"scan_id": scan_id, "path": path.display().to_string()}),
        )
        .await;
    r["node"]["id"].as_u64().unwrap()
}

async fn query(c: &Client, text: &str, ctx: &Value) -> Value {
    c.ok("palette.query", json!({"text": text, "context": ctx}))
        .await
}

fn ids(res: &Value) -> Vec<String> {
    res["commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn palette_round_trip() {
    let _g = LOCK.lock().await;
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let scan_id = scanned(tmp.path(), &c).await;
    c.ok("palette.clear", json!({"scan_id": scan_id})).await;
    let view_id = open_view(&c, &scan_id).await;
    let ctx = json!({"tab": "scan", "scan_id": scan_id, "view_id": view_id, "basket": 3});

    // empty input: the nine commands, no recents, no path group without a selection
    let res = query(&c, "", &ctx).await;
    assert_eq!(
        ids(&res)[..3],
        ["view.rescan", "scan.rescan_all", "view.cycle_color"]
    );
    assert_eq!(ids(&res).len(), 9);
    assert_eq!(res["recent"], json!([]));
    assert_eq!(res["direct"], json!([]));
    assert_eq!(res["paths"], false);
    let basket = res["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "basket.open")
        .unwrap();
    assert!(
        basket["status"].as_str().unwrap().starts_with("3 项"),
        "{basket}"
    );

    // a selection seeds the path group
    let movie = find(
        &c,
        &scan_id,
        &tmp.path()
            .join("root")
            .join("a")
            .join("sub")
            .join("movie.mp4"),
    )
    .await;
    c.ok("view.select", json!({"view_id": view_id, "key": movie}))
        .await;
    let res = query(&c, "", &ctx).await;
    assert_eq!(res["paths"], true);
    assert_eq!(res["seed"], "movie.mp4");
    let found = c
        .ok(
            "palette.paths",
            json!({"scan_id": scan_id, "view_id": view_id, "limit": 5}),
        )
        .await;
    assert_eq!(found["seed"], "movie.mp4");
    assert!(
        !found["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["node"] == movie)
    );

    // typed: commands by pinyin, paths by name
    let res = query(&c, "qhps", &ctx).await;
    assert_eq!(res["commands"][0]["id"], "view.cycle_color");
    assert_eq!(res["commands"][0]["hits"], json!([{"start": 0, "end": 4}]));
    let found = c
        .ok(
            "palette.paths",
            json!({"scan_id": scan_id, "text": "movie"}),
        )
        .await;
    assert_eq!(found["items"][0]["name"], "movie.mp4");
    assert_eq!(found["items"][0]["tier"], 1);
    assert_eq!(found["partial"], false);

    // filter direct item + preview
    let res = query(&c, "*.mp4;>100kb", &ctx).await;
    assert_eq!(res["direct"][0]["kind"], "filter");
    assert_eq!(res["direct"][0]["valid"], true);
    let bad = query(&c, ">500xb", &ctx).await;
    assert_eq!(bad["direct"][0]["valid"], false);
    assert_eq!(bad["direct"][0]["diagnostics"][0]["code"], "unknown_unit");
    let preview = c
        .ok(
            "palette.preview",
            json!({"scan_id": scan_id, "text": "*.mp4"}),
        )
        .await;
    let update = c
        .ok("view.filter", json!({"view_id": view_id, "text": "*.mp4"}))
        .await;
    assert_eq!(preview["files"], 1);
    assert_eq!(
        (&preview["files"], &preview["alloc"]),
        (&update["filter"]["files"], &update["filter"]["alloc"])
    );

    // path direct item and folder listing
    let root = tmp.path().join("root").display().to_string();
    let res = query(&c, &root, &ctx).await;
    assert_eq!(res["direct"][0]["kind"], "path");
    assert_eq!(res["direct"][0]["state"], "in_scan");
    assert_eq!(res["direct"][0]["is_dir"], true);
    let folder = format!(
        "{}{}",
        tmp.path().join("root").join("a").display(),
        std::path::MAIN_SEPARATOR
    );
    let listing = c
        .ok("palette.paths", json!({"scan_id": scan_id, "text": folder}))
        .await;
    assert!(!listing["folder"].is_null());
    let mut names: Vec<&str> = listing["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["name"].as_str().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["one.bin", "sub", "two.bin"]);

    // recents: remembered, matched, sized; then cleared
    c.ok(
        "palette.remember",
        json!({"kind": "filter", "text": "*.mp4"}),
    )
    .await;
    let a = tmp.path().join("root").join("a").display().to_string();
    c.ok(
        "palette.remember",
        json!({"kind": "jump", "text": a, "scan_id": scan_id}),
    )
    .await;
    let res = query(&c, "", &ctx).await;
    let kinds: Vec<&str> = res["recent"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["jump", "filter"]);
    assert_eq!(res["recent"][0]["exists"], true);
    assert!(res["recent"][0]["alloc"].as_i64().unwrap() > 0);
    assert!(!res["recent"][1]["tokens"].as_array().unwrap().is_empty());
    c.ok("palette.clear", json!({"scan_id": scan_id})).await;
    let res = query(&c, "", &ctx).await;
    assert_eq!(res["recent"], json!([]));

    // outside a scan tab: no paths, scan commands disabled
    let res = query(&c, "", &json!({"tab": "settings"})).await;
    let save = res["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "snapshot.save")
        .unwrap();
    assert_eq!(save["enabled"], false);
    assert_eq!(res["paths"], false);
}

#[tokio::test]
async fn check_one_command_where_the_user_is() {
    let _g = LOCK.lock().await;
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let scan_id = scanned(tmp.path(), &c).await;
    let view_id = open_view(&c, &scan_id).await;
    let on_scan = json!({"tab": "scan", "scan_id": scan_id, "view_id": view_id});
    let q = query(&c, "", &on_scan).await;
    for cmd in q["commands"].as_array().unwrap() {
        let checked = c
            .ok(
                "palette.check",
                json!({"id": cmd["id"], "context": on_scan}),
            )
            .await;
        let mut want = cmd.clone();
        want["hits"] = json!([]);
        assert_eq!(checked, want);
    }
    let save = c
        .ok(
            "palette.check",
            json!({"id": "snapshot.save", "context": on_scan}),
        )
        .await;
    assert_eq!(save["enabled"], true);
    assert_eq!(save["keys"], json!(["Ctrl+S"]));
    for cid in ["view.rescan", "scan.rescan_all", "snapshot.save"] {
        let idle = c
            .ok(
                "palette.check",
                json!({"id": cid, "context": {"tab": "start"}}),
            )
            .await;
        assert_eq!(idle["enabled"], false);
        assert_eq!(idle["status"], "需要先完成扫描");
    }
    let e = c
        .err(
            "palette.check",
            json!({"id": "no.such.command", "context": on_scan}),
        )
        .await;
    assert!(e.message.contains("unknown command"));
}

#[tokio::test]
async fn selected_scan_root_seeds_with_its_last_segment() {
    let _g = LOCK.lock().await;
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let scan_id = scanned(tmp.path(), &c).await;
    let view_id = open_view(&c, &scan_id).await;
    let root = find(&c, &scan_id, &tmp.path().join("root")).await; // stored as a full path
    c.ok("view.select", json!({"view_id": view_id, "key": root}))
        .await;
    let ctx = json!({"tab": "scan", "scan_id": scan_id, "view_id": view_id});
    let res = query(&c, "", &ctx).await;
    assert_eq!(res["paths"], true);
    assert_eq!(res["seed"], "root");
    let found = c
        .ok(
            "palette.paths",
            json!({"scan_id": scan_id, "view_id": view_id}),
        )
        .await;
    assert_eq!(found["seed"], "root");
    assert!(
        !found["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["node"] == root)
    );
}

#[cfg(not(windows))] // '?' cannot be part of a Windows file name
#[tokio::test]
async fn selected_name_is_searched_literally() {
    let _g = LOCK.lock().await;
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    make_tree(
        &root,
        &[("what?.txt", 1), ("whatX.txt", 1), ("what?.txt.bak", 1)],
    );
    let st = scan(&c, &root).await;
    let scan_id = st["scan_id"].as_str().unwrap().to_owned();
    let view_id = open_view(&c, &scan_id).await;
    let node = find(&c, &scan_id, &root.join("what?.txt")).await;
    c.ok("view.select", json!({"view_id": view_id, "key": node}))
        .await;
    let found = c
        .ok(
            "palette.paths",
            json!({"scan_id": scan_id, "view_id": view_id}),
        )
        .await;
    assert_eq!(found["seed"], "what?.txt");
    let names: Vec<&str> = found["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["what?.txt.bak"]);
}

#[tokio::test]
async fn palette_commands_list_every_command() {
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let list = c.ok("palette.commands", json!({})).await;
    let cmds = list["commands"].as_array().unwrap();
    assert!(
        cmds.iter()
            .any(|c| c["id"] == "snapshot.save" && c["keys"] == json!(["Ctrl+S"]))
    );
    assert!(cmds.iter().all(|c| !c["name"].as_str().unwrap().is_empty()));
}
