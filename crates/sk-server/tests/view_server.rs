//! Port of tests/test_view_server.py and tests/test_robust_view_push.py: view.* over the real
//! WebSocket (frames, navigation, filter, tags, pushes, ownership), plus live changes (M5).

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::*;
use serde_json::{Value, json};
use sk_core::view::flag;
use sk_protocol::ErrorCode;
use sk_protocol::scan::ScanStateName;

fn code(e: ErrorCode) -> i64 {
    i64::from(e.code())
}

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A decoded binary view frame.
struct Frame {
    header: Value,
    names: Vec<String>,
    cols: std::collections::HashMap<String, Vec<i64>>,
}

impl Frame {
    fn decode(buf: &[u8]) -> Frame {
        let d = sk_protocol::binary::decode_frame(buf).unwrap();
        let header = frame_header(buf);
        let mut cols = std::collections::HashMap::new();
        for name in header["columns"].as_object().unwrap().keys() {
            let c = d.column(name).unwrap();
            let v: Vec<i64> = (0..d.header.count as usize)
                .map(|i| match c.dtype {
                    sk_protocol::binary::Dtype::U1 => i64::from(c.get::<u8>(i)),
                    sk_protocol::binary::Dtype::U2 => i64::from(c.get::<u16>(i)),
                    sk_protocol::binary::Dtype::I4 => i64::from(c.get::<i32>(i)),
                    sk_protocol::binary::Dtype::U4 => i64::from(c.get::<u32>(i)),
                    sk_protocol::binary::Dtype::I8 => c.get::<i64>(i),
                    other => panic!("unexpected dtype {other:?}"),
                })
                .collect();
            cols.insert(name.clone(), v);
        }
        let blob = d.blob("names").unwrap_or(&[]);
        let mut names = Vec::new();
        let mut at = 0usize;
        for &n in &cols["name_len"] {
            names.push(String::from_utf8_lossy(&blob[at..at + n as usize]).into_owned());
            at += n as usize;
        }
        Frame {
            header,
            names,
            cols,
        }
    }

    fn len(&self) -> usize {
        self.header["count"].as_u64().unwrap() as usize
    }

    fn row(&self, name: &str) -> usize {
        self.names
            .iter()
            .position(|n| n == name)
            .unwrap_or_else(|| panic!("{name} not in {:?}", self.names))
    }

    fn col(&self, name: &str, row: usize) -> i64 {
        self.cols[name][row]
    }
}

/// A view call: the update and, when it changed something, its binary frame (paired by seq).
async fn vcall(c: &Client, method: &str, params: Value) -> Result<(Value, Option<Frame>), Remote> {
    let update = c.call(method, params).await?;
    let frame = match update.get("frame").filter(|f| !f.is_null()) {
        Some(info) => {
            let seq = info["seq"].as_u64().unwrap();
            let stream = format!("view:{}", info["view_id"].as_str().unwrap());
            loop {
                let buf = c.binary(10.0).await;
                let h = frame_header(&buf);
                if h["stream"] == stream.as_str() && h["seq"] == seq {
                    break Some(Frame::decode(&buf));
                }
            }
        }
        None => None,
    };
    Ok((update, frame))
}

async fn vok(c: &Client, method: &str, params: Value) -> (Value, Option<Frame>) {
    match vcall(c, method, params).await {
        Ok(x) => x,
        Err(e) => panic!("{method}: {e:?}"),
    }
}

/// The next pushed frame (view.frame event) and its binary frame.
async fn pushed(c: &Client) -> (Value, Frame) {
    let ev = c.event("view.frame", 10.0).await;
    let info = ev["frame"].clone();
    let seq = info["seq"].as_u64().unwrap();
    loop {
        let buf = c.binary(10.0).await;
        if frame_header(&buf)["seq"] == seq {
            return (info, Frame::decode(&buf));
        }
    }
}

async fn scanned(c: &Client, tmp: &Path) -> (String, TreeFacts) {
    let facts = build_tree(&tmp.join("root"));
    let st = c
        .ok(
            "scan.start",
            json!({"roots": [facts.root], "workers": 0, "auto_snapshot": false}),
        )
        .await;
    let id = st["scan_id"].as_str().unwrap().to_owned();
    wait_final(c, &id, false).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    c.drain_events().await;
    (id, facts)
}

fn viewport(w: f64, h: f64) -> Value {
    json!({"width": w, "height": h, "dpr": 1.0})
}

#[tokio::test]
async fn open_navigate_and_back() {
    let _g = SERIAL.lock().await;
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let (scan_id, _) = scanned(&c, tmp.path()).await;
    let (update, frame) = vok(
        &c,
        "view.open",
        json!({"scan_id": scan_id, "viewport": {"width": 1000, "height": 700, "dpr": 1.25}}),
    )
    .await;
    let frame = frame.expect("a frame");
    let info = &update["frame"];
    assert_eq!(frame.len() as i64, info["rows"].as_i64().unwrap());
    assert!(frame.len() > 0);
    assert_eq!(
        (info["width"].as_i64(), info["height"].as_i64()),
        (Some(1250), Some(875))
    );
    assert_eq!(frame.header["columns"]["x"]["dtype"], "<i4");
    assert_eq!(frame.header["columns"]["flags"]["dtype"], "<u2");
    assert!(
        frame.header["request_id"].is_u64(),
        "the binary frame names the request"
    );
    let view_id = update["state"]["view_id"].as_str().unwrap().to_owned();
    assert_eq!(info["breadcrumb"].as_array().unwrap().len(), 1);
    assert_eq!(info["breadcrumb"][0]["key"], update["state"]["zoom_node"]);
    // zoom into "a" by its key
    let key = frame.col("key", frame.row("a"));
    let (update, frame) = vok(&c, "view.zoom", json!({"view_id": view_id, "key": key})).await;
    let frame = frame.unwrap();
    assert_eq!(update["state"]["zoom_node"], key);
    assert_eq!(update["state"]["can_back"], true);
    assert_eq!(
        update["frame"]["breadcrumb"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()["label"],
        "a"
    );
    assert!(
        frame.names.contains(&"two.bin".to_owned()) && frame.names.contains(&"one.bin".to_owned())
    );
    let (update, frame) = vok(&c, "view.up", json!({"view_id": view_id})).await;
    let frame = frame.unwrap();
    assert_eq!(update["state"]["selected"], key);
    assert!(frame.col("flags", frame.row("a")) & i64::from(flag::SELECTED) != 0);
    let (update, _) = vok(&c, "view.back", json!({"view_id": view_id})).await;
    assert_eq!(update["state"]["zoom_node"], key);
    assert_eq!(update["state"]["can_forward"], true);
    let (update, _) = vok(&c, "view.forward", json!({"view_id": view_id})).await;
    assert_eq!(update["state"]["can_forward"], false);
    let (update, _) = vok(&c, "view.home", json!({"view_id": view_id})).await;
    assert_ne!(update["state"]["zoom_node"], key);
    // nothing to do: no frame
    let (update, frame) = vok(&c, "view.select", json!({"view_id": view_id, "key": null})).await;
    let (update, frame) = if update["changed"] == true {
        vok(&c, "view.select", json!({"view_id": view_id, "key": null})).await
    } else {
        (update, frame)
    };
    assert_eq!(update["changed"], false);
    assert!(frame.is_none());
    srv.shutdown();
}

#[tokio::test]
async fn filter_dim_errors_and_hide() {
    let _g = SERIAL.lock().await;
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let (scan_id, _) = scanned(&c, tmp.path()).await;
    let (update, _) = vok(
        &c,
        "view.open",
        json!({"scan_id": scan_id, "viewport": viewport(1200.0, 900.0), "settings": {"detail": 8, "min_element": 3}}),
    )
    .await;
    let view_id = update["state"]["view_id"].as_str().unwrap().to_owned();
    let (update, frame) = vok(
        &c,
        "view.filter",
        json!({"view_id": view_id, "text": "*.mp4"}),
    )
    .await;
    let frame = frame.unwrap();
    let f = &update["filter"];
    assert_eq!(
        (
            f["active"].as_bool(),
            f["files"].as_i64(),
            f["size"].as_i64()
        ),
        (Some(true), Some(1), Some(123456))
    );
    assert!(frame.col("flags", frame.row("movie.mp4")) & i64::from(flag::MATCH) != 0);
    if frame.names.iter().any(|n| n == "c.txt") {
        assert!(frame.col("flags", frame.row("c.txt")) & i64::from(flag::DIM) != 0);
    }
    // an error keeps the last valid filter and sends no frame
    let (update, frame) = vok(
        &c,
        "view.filter",
        json!({"view_id": view_id, "text": "*.mp4;>500xb"}),
    )
    .await;
    assert!(frame.is_none());
    let f = &update["filter"];
    assert_eq!(f["stale"], true);
    assert_eq!(f["diagnostics"][0]["code"], "unknown_unit");
    assert_eq!(f["files"], 1);
    // hide mode: only the match is laid out
    let (_, frame) = vok(
        &c,
        "view.set",
        json!({"view_id": view_id, "settings": {"filter_mode": "hide"}}),
    )
    .await;
    assert!(frame.is_some());
    vok(
        &c,
        "view.filter",
        json!({"view_id": view_id, "text": "*.mp4"}),
    )
    .await;
    let (_, frame) = vok(&c, "view.render", json!({"view_id": view_id})).await;
    let frame = frame.unwrap();
    let leaves: Vec<&String> = frame
        .names
        .iter()
        .enumerate()
        .filter(|(i, n)| frame.col("kind", *i) == 0 && !n.is_empty())
        .map(|x| x.1)
        .collect();
    assert_eq!(leaves, vec!["movie.mp4"]);
    srv.shutdown();
}

#[tokio::test]
async fn results_and_reveal() {
    let _g = SERIAL.lock().await;
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let (scan_id, facts) = scanned(&c, tmp.path()).await;
    let (update, _) = vok(
        &c,
        "view.open",
        json!({"scan_id": scan_id, "viewport": viewport(1200.0, 900.0), "settings": {"detail": 1}}),
    )
    .await;
    let view_id = update["state"]["view_id"].as_str().unwrap().to_owned();
    let empty = c.ok("view.results", json!({"view_id": view_id})).await;
    assert_eq!(
        (empty["active"].as_bool(), empty["total"].as_i64()),
        (Some(false), Some(0))
    );
    assert_eq!(empty["rows"], json!([]));
    let (update, _) = vok(
        &c,
        "view.filter",
        json!({"view_id": view_id, "text": "*.mp4;*.txt"}),
    )
    .await;
    let res = c
        .ok(
            "view.results",
            json!({"view_id": view_id, "sort": "name", "limit": 500}),
        )
        .await;
    assert_eq!(res["active"], true);
    assert_eq!(res["sort"], "name");
    assert_eq!(res["seq"], update["frame"]["seq"]);
    let rows = res["rows"].as_array().unwrap();
    let count: i64 = rows.iter().map(|r| r["count"].as_i64().unwrap()).sum();
    assert_eq!(count, res["files"].as_i64().unwrap());
    assert_eq!(count, update["filter"]["files"].as_i64().unwrap());
    let alloc: i64 = rows.iter().map(|r| r["alloc"].as_i64().unwrap()).sum();
    assert_eq!(alloc, res["alloc"].as_i64().unwrap());
    assert_eq!(
        res["max_alloc"].as_i64(),
        rows.iter().map(|r| r["alloc"].as_i64().unwrap()).max()
    );
    let page = c
        .ok(
            "view.results",
            json!({"view_id": view_id, "offset": 1, "limit": 1}),
        )
        .await;
    assert_eq!(page["sort"], "name");
    assert_eq!(page["offset"], 1);
    assert_eq!(page["rows"], json!(rows[1..2]));
    let list = c.ok("view.list", json!({})).await;
    assert_eq!(list["views"][0]["results_sort"], "name");
    // locate the movie: at detail 1 it is folded away, so the view navigates to show it
    let movie = c
        .ok(
            "tree.find",
            json!({"scan_id": scan_id, "path": facts.root.join("a").join("sub").join("movie.mp4")}),
        )
        .await;
    let id = movie["node"]["id"].clone();
    let (update, frame) = vok(&c, "view.reveal", json!({"view_id": view_id, "key": id})).await;
    let frame = frame.unwrap();
    assert_eq!(update["state"]["selected"], id);
    assert_eq!(update["state"]["can_back"], true);
    assert!(frame.col("flags", frame.row("movie.mp4")) & i64::from(flag::SELECTED) != 0);
    srv.shutdown();
}

#[tokio::test]
async fn tag_the_selection() {
    let _g = SERIAL.lock().await;
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let (scan_id, _) = scanned(&c, tmp.path()).await;
    let (update, frame) = vok(
        &c,
        "view.open",
        json!({"scan_id": scan_id, "viewport": viewport(1000.0, 700.0)}),
    )
    .await;
    let frame = frame.unwrap();
    let view_id = update["state"]["view_id"].as_str().unwrap().to_owned();
    let key = frame.col("key", frame.row("a"));
    vok(&c, "view.select", json!({"view_id": view_id, "key": key})).await;
    let (_, frame) = vok(&c, "view.tag", json!({"view_id": view_id, "color": "red"})).await;
    let frame = frame.unwrap();
    assert_eq!(frame.col("tag", frame.row("a")), 1);
    let (update, _) = vok(
        &c,
        "view.filter",
        json!({"view_id": view_id, "text": ":red"}),
    )
    .await;
    assert_eq!(
        update["filter"]["files"], 3,
        "everything in a inherits the tag"
    );
    let (_, frame) = vok(&c, "view.tag", json!({"view_id": view_id, "color": null})).await;
    let frame = frame.unwrap();
    assert_eq!(frame.col("tag", frame.row("a")), 0);
    srv.shutdown();
}

#[tokio::test]
async fn scan_status_pushes_frames_and_marks_scanning() {
    let _g = SERIAL.lock().await;
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let (scan_id, _) = scanned(&c, tmp.path()).await;
    let (update, _) = vok(
        &c,
        "view.open",
        json!({"scan_id": scan_id, "viewport": viewport(800.0, 600.0)}),
    )
    .await;
    let view_id = update["state"]["view_id"].as_str().unwrap().to_owned();
    let host = srv.server.views.lookup(&view_id).unwrap();
    assert!(!host.view.lock().scanning, "opened after the scan ended");
    let server = srv.server.clone();
    server
        .views
        .on_scan(&server, &scan_id, ScanStateName::Scanning);
    assert!(host.view.lock().scanning);
    let (info, frame) = pushed(&c).await;
    assert_eq!(info["reason"], "scan");
    assert_eq!(frame.len() as i64, info["rows"].as_i64().unwrap());
    server.views.on_scan(&server, &scan_id, ScanStateName::Done);
    let (info, frame) = pushed(&c).await;
    assert_eq!(info["reason"], "scan-final");
    assert_eq!(info["view_id"], view_id.as_str());
    assert_eq!(frame.len() as i64, info["rows"].as_i64().unwrap());
    assert!(!host.view.lock().scanning);
    srv.shutdown();
}

/// test_robust_view_push: a final status that arrives while a progress push renders still gets
/// its final frame, exactly once; a closed view gets nothing.
#[tokio::test]
async fn final_status_during_a_push_renders_the_final_frame_once() {
    let _g = SERIAL.lock().await;
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let (scan_id, _) = scanned(&c, tmp.path()).await;
    let (update, _) = vok(
        &c,
        "view.open",
        json!({"scan_id": scan_id, "viewport": viewport(800.0, 600.0)}),
    )
    .await;
    let view_id = update["state"]["view_id"].as_str().unwrap().to_owned();
    let host = srv.server.views.lookup(&view_id).unwrap();
    let server = srv.server.clone();
    // hold the view: the progress push waits, the final status arrives meanwhile
    let guard = host.lock.lock().await;
    server
        .views
        .on_scan(&server, &scan_id, ScanStateName::Scanning);
    server.views.on_scan(&server, &scan_id, ScanStateName::Done);
    server.views.on_scan(&server, &scan_id, ScanStateName::Done);
    drop(guard);
    let (a, _) = pushed(&c).await;
    let (b, _) = pushed(&c).await;
    assert_eq!(
        (a["reason"].as_str(), b["reason"].as_str()),
        (Some("scan"), Some("scan-final"))
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    let more: Vec<_> = c
        .drain_events()
        .await
        .into_iter()
        .filter(|(m, _)| m == "view.frame")
        .collect();
    assert!(more.is_empty(), "the final frame is not repeated: {more:?}");
    // a closed view gets no pushes
    c.ok("view.close", json!({"view_id": view_id})).await;
    server.views.on_scan(&server, &scan_id, ScanStateName::Done);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        c.drain_events()
            .await
            .iter()
            .all(|(m, _)| m != "view.frame")
    );
    srv.shutdown();
}

#[tokio::test]
async fn views_belong_to_their_session_and_scan() {
    let _g = SERIAL.lock().await;
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let (scan_id, _) = scanned(&c, tmp.path()).await;
    let (update, _) = vok(
        &c,
        "view.open",
        json!({"scan_id": scan_id, "viewport": viewport(800.0, 600.0)}),
    )
    .await;
    let view_id = update["state"]["view_id"].clone();
    let list = c.ok("view.list", json!({})).await;
    assert_eq!(list["views"].as_array().unwrap().len(), 1);
    assert_eq!(list["views"][0]["view_id"], view_id);
    let other = Client::open(&srv.url).await;
    assert_eq!(
        other
            .err("view.render", json!({"view_id": view_id}))
            .await
            .code,
        code(ErrorCode::UnknownView)
    );
    other.close().await;
    c.ok("view.close", json!({"view_id": view_id})).await;
    assert_eq!(
        c.err("view.render", json!({"view_id": view_id})).await.code,
        code(ErrorCode::UnknownView)
    );
    // closing the scan closes its views
    vok(
        &c,
        "view.open",
        json!({"scan_id": scan_id, "viewport": viewport(800.0, 600.0)}),
    )
    .await;
    c.ok("scan.close", json!({"scan_id": scan_id})).await;
    assert!(srv.server.views.is_empty());
    let e = c
        .err(
            "view.open",
            json!({"scan_id": scan_id, "viewport": viewport(8.0, 8.0)}),
        )
        .await;
    assert_eq!(e.code, code(ErrorCode::UnknownScan));
    // a session's views go with it
    let (scan_id, _) = scanned(&c, &tmp.path().join("again")).await;
    let third = Client::open(&srv.url).await;
    vok(
        &third,
        "view.open",
        json!({"scan_id": scan_id, "viewport": viewport(80.0, 60.0)}),
    )
    .await;
    assert_eq!(srv.server.views.len(), 1);
    third.close().await;
    for _ in 0..100 {
        if srv.server.views.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(srv.server.views.is_empty());
    srv.shutdown();
}

#[tokio::test]
async fn switching_the_visualization_keeps_each_colour_mode() {
    let _g = SERIAL.lock().await;
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let (scan_id, _) = scanned(&c, tmp.path()).await;
    let vp = viewport(800.0, 600.0);
    let (update, _) = vok(&c, "view.open", json!({"scan_id": scan_id, "viewport": vp})).await;
    let view_id = update["state"]["view_id"].as_str().unwrap().to_owned();
    vok(
        &c,
        "view.set",
        json!({"view_id": view_id, "settings": {"color_mode": "time"}}),
    )
    .await;
    let (update, frame) = vok(
        &c,
        "view.set",
        json!({"view_id": view_id, "settings": {"visual": "flame", "color_mode": "flat"}}),
    )
    .await;
    let info = &update["frame"];
    assert_eq!(info["reason"], "visual");
    assert_eq!(info["layout"], "flame.v1");
    assert_eq!(info["geometry"]["kind"], "flame");
    assert!(info["root"].is_object());
    assert_eq!(frame.unwrap().header["layout"], "flame.v1");
    let st = update["state"].clone();
    assert_eq!(st["colors"]["treemap"], "time");
    assert_eq!(st["colors"]["flame"], "flat");
    // a split pane / a reconnect opens with what the old view carried
    let like =
        json!({"visual": st["visual"], "colors": st["colors"], "color_mode": st["color_mode"]});
    let (update, _) = vok(
        &c,
        "view.open",
        json!({"scan_id": scan_id, "viewport": vp, "settings": like}),
    )
    .await;
    assert_eq!(
        (
            update["state"]["visual"].as_str(),
            update["state"]["color_mode"].as_str()
        ),
        (Some("flame"), Some("flat"))
    );
    let vid = update["state"]["view_id"].clone();
    let (update, _) = vok(
        &c,
        "view.set",
        json!({"view_id": vid, "settings": {"visual": "treemap"}}),
    )
    .await;
    assert_eq!(update["state"]["color_mode"], "time");
    // unknown visualizations in the map are left out; unknown visuals are INVALID_PARAMS
    let (update, _) = vok(
        &c,
        "view.open",
        json!({"scan_id": scan_id, "viewport": vp, "settings": {"colors": {"pie": "flat"}}}),
    )
    .await;
    assert!(update["state"]["colors"].get("pie").is_none());
    let e = c
        .err(
            "view.set",
            json!({"view_id": vid, "settings": {"visual": "pie"}}),
        )
        .await;
    assert_eq!(e.code, code(ErrorCode::InvalidParams));
    let vis = c.ok("visual.list", json!({})).await;
    let ids: Vec<&str> = vis["visuals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["treemap", "flame", "flame_polar"]);
    assert!(
        vis["visuals"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["available"] == true)
    );
    srv.shutdown();
}

#[tokio::test]
async fn older_frontends_stay_on_the_treemap() {
    let _g = SERIAL.lock().await;
    let srv = server().await;
    let old = Client::open_caps(&srv.url, &["binary-frames.v1", "subscriptions"]).await;
    let tmp = tempfile::tempdir().unwrap();
    let (scan_id, _) = scanned(&old, tmp.path()).await;
    let (update, frame) = vok(
        &old,
        "view.open",
        json!({"scan_id": scan_id, "viewport": viewport(800.0, 600.0), "settings": {"visual": "flame", "color_mode": "branch"}}),
    )
    .await;
    assert!(frame.is_some());
    let st = &update["state"];
    assert_eq!(
        (st["visual"].as_str(), st["color_mode"].as_str()),
        (Some("treemap"), Some("class"))
    );
    assert_eq!(update["frame"]["layout"], "drawlist.v1");
    let view_id = st["view_id"].clone();
    for settings in [json!({"visual": "flame"}), json!({"color_mode": "branch"})] {
        let e = old
            .err(
                "view.set",
                json!({"view_id": view_id, "settings": settings}),
            )
            .await;
        assert_eq!(e.code, code(ErrorCode::InvalidParams));
    }
    let host = srv.server.views.lookup(view_id.as_str().unwrap()).unwrap();
    {
        let v = host.view.lock();
        assert_eq!(
            (v.visual.as_str(), v.color_mode().as_str()),
            ("treemap", "class")
        );
    }
    let vis = old.ok("visual.list", json!({})).await;
    let avail: Vec<bool> = vis["visuals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["available"].as_bool().unwrap())
        .collect();
    assert_eq!(avail, [true, false, false]);
    srv.shutdown();
}

/// Live changes (M5): a file deleted while the scan is watched leaves the tree, the open view gets
/// a pushed frame without it, and node ids that moved are followed.
#[tokio::test]
async fn monitored_changes_reach_open_views() {
    let _g = SERIAL.lock().await;
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let (scan_id, facts) = scanned(&c, tmp.path()).await;
    let handle = srv.server.scans.get(&scan_id).unwrap();
    for _ in 0..250 {
        if srv
            .server
            .monitors
            .status(&scan_id)
            .is_some_and(|s| matches!(s.state, sk_protocol::scan::MonitorState::Running))
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let (update, _) = vok(
        &c,
        "view.open",
        json!({"scan_id": scan_id, "viewport": viewport(800.0, 600.0)}),
    )
    .await;
    let view_id = update["state"]["view_id"].as_str().unwrap().to_owned();
    // zoom into a/sub: its id must follow when "a" is listed again
    let sub = c
        .ok(
            "tree.find",
            json!({"scan_id": scan_id, "path": facts.root.join("a").join("sub")}),
        )
        .await;
    let sub_id = sub["node"]["id"].as_u64().unwrap();
    vok(&c, "view.zoom", json!({"view_id": view_id, "key": sub_id})).await;
    c.drain_events().await;
    c.drain_binary().await;
    std::fs::remove_file(facts.root.join("a").join("one.bin")).unwrap();
    std::fs::write(facts.root.join("a").join("added.bin"), vec![1u8; 70_000]).unwrap();
    let one = facts.root.join("a").join("one.bin");
    let mut ok = false;
    for _ in 0..100 {
        let gone = c
            .ok("tree.find", json!({"scan_id": scan_id, "path": one}))
            .await["node"]
            .is_null();
        let added = !c
            .ok(
                "tree.find",
                json!({"scan_id": scan_id, "path": facts.root.join("a").join("added.bin")}),
            )
            .await["node"]
            .is_null();
        if gone && added {
            ok = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(ok, "the monitor applied the changes");
    let (info, _) = pushed(&c).await;
    assert_eq!(info["reason"], "change");
    // the view still shows a/sub (by path), whatever its id is now
    let (update, _) = vok(&c, "view.render", json!({"view_id": view_id})).await;
    let zoom = update["state"]["zoom_node"].as_u64().unwrap() as u32;
    let path = handle.store.read().path(zoom);
    assert_eq!(path, facts.root.join("a").join("sub"));
    let st = c.ok("scan.status", json!({"scan_id": scan_id})).await;
    assert!(st["monitor"]["changes"].as_i64().unwrap() > 0);
    srv.shutdown();
}

#[tokio::test]
async fn new_views_take_the_settings_and_follow_live_geometry() {
    let _g = SERIAL.lock().await;
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let (scan_id, _) = scanned(&c, tmp.path()).await;
    let st = c.ok("settings.get", json!({})).await;
    let version = st["version"].clone();
    c.ok("settings.update", json!({"values": {"treemap.initial_detail": 3, "appearance.default_visual": "flame"}, "version": version}))
        .await;
    let (update, _) = vok(
        &c,
        "view.open",
        json!({"scan_id": scan_id, "viewport": viewport(800.0, 600.0)}),
    )
    .await;
    assert_eq!(update["state"]["detail"], 3);
    assert_eq!(update["state"]["visual"], "flame");
    c.drain_events().await;
    c.ok(
        "settings.update",
        json!({"values": {"treemap.sort_by_size": false}}),
    )
    .await;
    let ev = c.event("settings.changed", 5.0).await;
    assert_eq!(ev["state"]["values"]["treemap.sort_by_size"], false);
    let (info, _) = pushed(&c).await;
    assert_eq!(info["reason"], "settings");
    let e = c
        .err(
            "settings.update",
            json!({"values": {"treemap.sort_by_size": true}, "version": 0}),
        )
        .await;
    assert_eq!(e.code, code(ErrorCode::SettingsConflict));
    c.ok("settings.reset", json!({})).await;
    let _ = Arc::strong_count(&srv.server);
    srv.shutdown();
}
