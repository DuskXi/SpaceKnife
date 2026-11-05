//! Port of tests/test_classes_server.py: classes.* over the WebSocket (W7 「文件类别」).

mod common;

use common::*;
use serde_json::{Value, json};
use sk_protocol::binary::decode_frame;

/// The class table and folder choices are process-wide: one test at a time.
static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Restore;
impl Drop for Restore {
    fn drop(&mut self) {
        sk_core::classes::set_active(sk_core::classes::ClassTable::builtin());
        sk_core::pathclass::replace_folders(Vec::new());
    }
}

/// (names, cls, flags) of a binary frame.
fn columns(buf: &[u8]) -> (Vec<String>, Vec<u8>, Vec<u16>) {
    let f = decode_frame(buf).unwrap();
    let lens: Vec<u32> = f.column("name_len").unwrap().to_vec().unwrap();
    let blob = f.blob("names").unwrap();
    let mut names = Vec::new();
    let mut at = 0usize;
    for l in lens {
        names.push(String::from_utf8_lossy(&blob[at..at + l as usize]).into_owned());
        at += l as usize;
    }
    (
        names,
        f.column("cls").unwrap().to_vec().unwrap(),
        f.column("flags").unwrap().to_vec().unwrap(),
    )
}

/// The table's rows as edits, with `extra` before「其他」.
fn edits(state: &Value, extra: Vec<Value>) -> Vec<Value> {
    let rows: Vec<Value> = state["classes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| json!({"id": c["id"], "name": c["name"], "extensions": c["extensions"], "color": c["color"]}))
        .collect();
    let mut out = rows[..rows.len() - 1].to_vec();
    out.extend(extra);
    out.push(rows[rows.len() - 1].clone());
    out
}

#[tokio::test]
async fn new_class_colours_and_filters() {
    let _g = LOCK.lock().await;
    let _r = Restore;
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let dir = tempfile::tempdir().unwrap();
    make_tree(
        dir.path(),
        &[("pakchunk0-Windows.pak", 5000), ("movie.mp4", 3000)],
    );
    let st = scan(&c, dir.path()).await;
    let scan_id = st["scan_id"].as_str().unwrap();
    let update = c
        .ok(
            "view.open",
            json!({"scan_id": scan_id, "viewport": {"width": 800, "height": 600}}),
        )
        .await;
    let view_id = update["state"]["view_id"].as_str().unwrap().to_owned();
    let (names, cls, _) = columns(&c.binary(5.0).await);
    let row = names
        .iter()
        .position(|n| n == "pakchunk0-Windows.pak")
        .unwrap();
    assert_eq!(
        cls[row] as usize,
        sk_core::classes::ClassTable::builtin().other_index()
    );

    let state = c.ok("classes.get", json!({})).await;
    assert_eq!(state["custom"], false);
    let new = json!({"name": "游戏资源", "extensions": ["PAK; .ucas"], "color": "#c77dff"});
    let state = c
        .ok(
            "classes.update",
            json!({"classes": edits(&state, vec![new])}),
        )
        .await;
    assert_eq!(state["custom"], true);
    let game = state["classes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|k| k["name"] == "游戏资源")
        .unwrap()
        .clone();
    assert_eq!(game["extensions"], json!(["pak", "ucas"]));
    assert_eq!(game["color"], "#C77DFF");
    assert_eq!(
        state["classes"].as_array().unwrap().last().unwrap()["id"],
        "other"
    );
    let file = sk_core::paths::config_dir().join("classes.json");
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert!(
        saved["classes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|k| k["name"] == "游戏资源")
    );

    let vocab = c.ok("filter.vocab", json!({})).await;
    assert!(
        vocab["classes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|k| k["name"] == "游戏资源")
    );
    c.drain_binary().await;
    c.ok(
        "view.filter",
        json!({"view_id": view_id, "text": ":class:游戏资源"}),
    )
    .await;
    let (names, cls, flags) = columns(&c.binary(5.0).await);
    let row = names
        .iter()
        .position(|n| n == "pakchunk0-Windows.pak")
        .unwrap();
    let ids: Vec<&Value> = state["classes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| &k["id"])
        .collect();
    assert_eq!(
        cls[row] as usize,
        ids.iter().position(|i| **i == game["id"]).unwrap()
    );
    assert!(flags[row] & 1 != 0);
    let movie = names.iter().position(|n| n == "movie.mp4").unwrap();
    assert!(flags[movie] & 1 == 0);

    let state = c.ok("classes.reset", json!({})).await;
    assert_eq!(state["custom"], false);
    assert!(!file.exists());
}

#[tokio::test]
async fn summary_gives_each_top_file_its_class() {
    let _g = LOCK.lock().await;
    let _r = Restore;
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let dir = tempfile::tempdir().unwrap();
    // ts is video or code by size (classes::STREAM_SPLITS)
    make_tree(
        dir.path(),
        &[
            ("show.ts", 188 * 5600),
            ("app.ts", 3000),
            ("movie.mp4", 3000),
        ],
    );
    let st = scan(&c, dir.path()).await;
    let node = st["roots"][0]["node"].clone();
    let res = c
        .ok(
            "tree.summary",
            json!({"scan_id": st["scan_id"], "node": node, "top": 10}),
        )
        .await;
    let got: std::collections::BTreeMap<String, String> = res["top_files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["node"]["name"].as_str().unwrap().to_owned(),
                f["class_id"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    let want: std::collections::BTreeMap<String, String> = [
        ("show.ts", "video"),
        ("app.ts", "code"),
        ("movie.mp4", "video"),
    ]
    .into_iter()
    .map(|(a, b)| (a.to_owned(), b.to_owned()))
    .collect();
    assert_eq!(got, want);
}

#[tokio::test]
async fn invalid_tables_are_refused() {
    let _g = LOCK.lock().await;
    let _r = Restore;
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let state = c.ok("classes.get", json!({})).await;
    let video = &state["classes"][0];
    let dup = json!({"name": video["name"], "extensions": ["xyz"]});
    let e = c
        .err(
            "classes.update",
            json!({"classes": edits(&state, vec![dup])}),
        )
        .await;
    assert!(e.message.contains("重复"), "{}", e.message);
    let rows = state["classes"].as_array().unwrap();
    let without_other: Vec<Value> = rows[..rows.len() - 1]
        .iter()
        .map(|k| json!({"id": k["id"], "name": k["name"], "extensions": k["extensions"]}))
        .collect();
    let e = c
        .err("classes.update", json!({"classes": without_other}))
        .await;
    assert!(e.message.contains("其他"), "{}", e.message);
    assert_eq!(c.ok("classes.get", json!({})).await["custom"], false);
}

#[tokio::test]
async fn folder_choices_are_announced_and_saved() {
    let _g = LOCK.lock().await;
    let _r = Restore;
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().display().to_string();
    let state = c
        .ok("classes.folder", json!({"path": path, "class_id": "video"}))
        .await;
    assert_eq!(
        state["folders"],
        json!([{"path": path, "class_id": "video"}])
    );
    let ev = c.event("classes.changed", 5.0).await;
    assert_eq!(ev["state"]["folders"][0]["class_id"], "video");
    assert!(sk_core::pathclass::user_file().exists());
    let e = c
        .err(
            "classes.folder",
            json!({"path": path, "class_id": "no-such"}),
        )
        .await;
    assert_eq!(e.code, -32602);
    let state = c.ok("classes.reset", json!({})).await;
    assert_eq!(state["folders"], json!([]));
    assert!(!sk_core::pathclass::user_file().exists());
}
