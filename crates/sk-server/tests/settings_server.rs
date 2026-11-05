//! The server parts of tests/test_settings.py and tests/test_settings_io.py: settings.* over the
//! WebSocket, export / import, maintenance.

mod common;

use std::path::PathBuf;

use common::*;
use serde_json::{Value, json};

/// Every server of this binary shares the settings file and the class table: one test at a time,
/// each leaving the defaults behind.
static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn reset(c: &Client) {
    c.ok("settings.reset", json!({})).await;
    c.ok("classes.reset", json!({})).await;
}

fn ss_default() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../sk-core/tests/data/SpaceSnifferConfig.default.xml")
}

#[tokio::test]
async fn settings_over_the_protocol() {
    let _g = LOCK.lock().await;
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let state = c.ok("settings.get", json!({})).await;
    let defaults = serde_json::to_value(sk_core::settings::defaults()).unwrap();
    assert_eq!(state["values"], defaults);
    let keys: std::collections::BTreeSet<&str> = state["meta"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["key"].as_str().unwrap())
        .collect();
    assert_eq!(
        keys,
        defaults
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect()
    );
    assert!(state["path"].as_str().unwrap().ends_with("settings.json"));
    let state = c
        .ok(
            "settings.update",
            json!({"values": {"scan.unit_base": "1000"}, "version": state["version"]}),
        )
        .await;
    assert_eq!(state["unit_base"], 1000);
    let e = c
        .err(
            "settings.update",
            json!({"values": {"scan.unit_base": "1000"}, "version": 0}),
        )
        .await;
    assert_eq!(e.code, -32013);
    let e = c
        .err(
            "settings.update",
            json!({"values": {"treemap.min_element": 0}}),
        )
        .await;
    assert_eq!(e.code, -32602);
    let ev = c.event("settings.changed", 5.0).await;
    assert_eq!(ev["state"]["values"]["scan.unit_base"], "1000");
    let state = c.ok("settings.reset", json!({"category": "scan"})).await;
    assert_eq!(state["values"]["scan.unit_base"], "system");
    reset(&c).await;
}

#[tokio::test]
async fn the_memory_settings_reach_the_mapping_policy() {
    let _g = LOCK.lock().await;
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let policy = sk_store::Policy::global();
    assert_eq!((policy.large(), policy.fraction()), (None, 0.25));
    c.ok(
        "settings.update",
        json!({"values": {"advanced.map_large_scans": true, "advanced.memory_budget": 40}}),
    )
    .await;
    assert_eq!(
        (policy.large(), policy.fraction()),
        (Some(sk_store::mapped::LARGE_BYTES), 0.4)
    );
    c.ok("settings.reset", json!({"category": "advanced"}))
        .await;
    assert_eq!((policy.large(), policy.fraction()), (None, 0.25));
    reset(&c).await;
}

#[tokio::test]
async fn new_views_use_the_settings_and_geometry_is_live() {
    let _g = LOCK.lock().await;
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    c.ok(
        "settings.update",
        json!({"values": {"treemap.initial_detail": 7, "appearance.default_color_mode": "time", "treemap.show_free": false}}),
    )
    .await;
    let tree = sample_tree();
    let st = scan(&c, tree.path()).await;
    let u = c
        .ok(
            "view.open",
            json!({"scan_id": st["scan_id"], "viewport": {"width": 800, "height": 600}}),
        )
        .await;
    let s = &u["state"];
    assert_eq!(
        (
            &s["detail"],
            &s["color_mode"],
            &s["show_free"],
            &s["min_element"]
        ),
        (&json!(7), &json!("time"), &json!(false), &json!(10.0))
    );
    c.ok(
        "settings.update",
        json!({"values": {"treemap.min_element": 18}}),
    )
    .await;
    c.event_where("view.frame", 5.0, |p| p["frame"]["reason"] == "settings")
        .await;
    let host = srv
        .server
        .views
        .lookup(s["view_id"].as_str().unwrap())
        .unwrap();
    assert_eq!(host.view.lock().min_element, 18.0);
    reset(&c).await;
}

#[tokio::test]
async fn round_trip_and_spacesniffer_import() {
    let _g = LOCK.lock().await;
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    c.ok(
        "settings.update",
        json!({"values": {"treemap.min_element": 14}}),
    )
    .await;
    let out = tmp.path().join("mine.json");
    c.ok(
        "settings.export",
        json!({"path": out.display().to_string()}),
    )
    .await;
    let doc: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(doc["format"], "spaceknife-settings");
    assert_eq!(doc["values"], json!({"treemap.min_element": 14}));

    let ss = ss_default().display().to_string();
    let preview = c.ok("settings.import", json!({"path": ss})).await;
    assert_eq!(preview["applied"], false);
    assert!(!preview["classes"].is_null() && !preview["changes"].as_array().unwrap().is_empty());
    assert_eq!(
        c.ok("settings.get", json!({})).await["values"]["treemap.min_element"],
        14
    ); // preview only
    let applied = c
        .ok("settings.import", json!({"path": ss, "apply": true}))
        .await;
    assert_eq!(applied["applied"], true);
    let state = c.ok("settings.get", json!({})).await;
    assert_eq!(state["values"]["appearance.default_color_mode"], "flat");
    assert_eq!(state["values"]["treemap.min_element"], 10);
    assert_eq!(c.ok("classes.get", json!({})).await["custom"], true);

    // our own export puts everything back, the class table included
    let back = c
        .ok(
            "settings.import",
            json!({"path": out.display().to_string(), "apply": true}),
        )
        .await;
    assert_eq!(back["source"], "spaceknife");
    assert!(!back["classes"].is_null());
    let state = c.ok("settings.get", json!({})).await;
    assert_eq!(state["values"]["treemap.min_element"], 14);
    assert_eq!(state["values"]["appearance.default_color_mode"], "class");
    assert_eq!(c.ok("classes.get", json!({})).await["custom"], false);
    c.err(
        "settings.import",
        json!({"path": tmp.path().join("missing.json").display().to_string()}),
    )
    .await;
    let e = c
        .err(
            "settings.export",
            json!({"path": tmp.path().join("no/such/x.json").display().to_string()}),
        )
        .await;
    assert_eq!(e.code, -32602);
    reset(&c).await;
}

#[tokio::test]
async fn maintenance() {
    let _g = LOCK.lock().await;
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tags = sk_core::paths::data_dir().join("tags");
    std::fs::create_dir_all(&tags).unwrap();
    std::fs::write(tags.join("root-1.json"), r#"{"version": 1, "tags": []}"#).unwrap();
    let cache = sk_core::paths::cache_dir().join("junk");
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(cache.join("x.bin"), vec![b'x'; 1000]).unwrap();
    let info = c.ok("system.maintenance", json!({})).await;
    assert_eq!(info["saved_tags"], 1);
    assert!(info["cache_bytes"].as_i64().unwrap() >= 1000);
    assert_eq!(info["helper_running"], false);
    let info = c.ok("tags.clear_saved", json!({})).await;
    assert_eq!(info["saved_tags"], 0);
    assert!(!tags.exists());
    let info = c.ok("system.cache_clear", json!({})).await;
    assert_eq!(info["cache_bytes"], 0);
    let info = c.ok("system.helper_stop", json!({})).await;
    assert_eq!(info["helper_running"], false);
}
