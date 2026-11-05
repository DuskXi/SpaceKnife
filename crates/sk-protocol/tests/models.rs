//! View models, visual ids, params validation and the JSON shapes of the messages.

use serde_json::{Value, json};
use sk_protocol::methods::{self, METHODS, Method};
use sk_protocol::view::*;
use sk_protocol::*;

fn info(extra: Value) -> Result<FrameInfo, serde_json::Error> {
    let mut v = json!({
        "view_id": "v1", "seq": 1, "width": 800, "height": 600, "dpr": 1.0, "rows": 0,
        "breadcrumb": [], "scanned": 0, "layout_ms": 0.1,
    });
    for (k, x) in extra.as_object().unwrap() {
        v[k] = x.clone();
    }
    serde_json::from_value(v)
}

#[test]
fn visual_capabilities_and_minor_version() {
    for cap in [CAP_VISUALS, CAP_LAYOUT_FLAME, CAP_LAYOUT_POLAR] {
        assert!(ALL_CAPABILITIES.contains(&cap));
    }
    assert_eq!(
        (CAP_VISUALS, CAP_LAYOUT_FLAME, CAP_LAYOUT_POLAR),
        ("visuals.v1", "layout.flame.v1", "layout.polar.v1")
    );
    assert_eq!(protocol_major(PROTOCOL_VERSION), Some(1));
    assert_eq!(
        methods::method("visual.list").unwrap().name,
        methods::VisualList::NAME
    );
}

#[test]
fn frame_info_defaults_describe_a_treemap_frame() {
    let i = info(json!({})).unwrap();
    assert_eq!(
        (i.visual.as_str(), i.layout.as_str(), &i.geometry, &i.root),
        ("treemap", DRAWLIST_LAYOUT, &None, &None)
    );
}

#[test]
fn geometry_is_a_tagged_union() {
    let rect = json!({"x": 0, "y": 560, "w": 800, "h": 38});
    let flame = info(json!({
        "visual": "flame",
        "layout": FLAME_LAYOUT,
        "geometry": {"kind": "flame", "origin": "bottom", "row_h": 40, "rows": 5, "gap": 2, "root": rect},
        "root": {"key": 7, "node": 7, "kind": 1, "size": 123},
    }))
    .unwrap();
    let Some(FrameGeometry::Flame(g)) = &flame.geometry else {
        panic!("{:?}", flame.geometry)
    };
    assert_eq!(
        g.root,
        PixelRect {
            x: 0,
            y: 560,
            w: 800,
            h: 38
        }
    );
    let root = flame.root.as_ref().unwrap();
    assert_eq!((root.tone, root.branch, root.delta), (4, 0, None));

    let polar = info(json!({
        "visual": "flame_polar",
        "layout": POLAR_LAYOUT,
        "geometry": {"kind": "polar", "cx": 400, "cy": 300, "radius": 292, "hole": 70, "ring_w": 44, "rings": 5, "gap": 2},
    }))
    .unwrap();
    let Some(FrameGeometry::Polar(g)) = &polar.geometry else {
        panic!()
    };
    assert_eq!(g.unit, ANGLE_UNIT);
    assert_eq!(ANGLE_UNIT, 1 << 20);
    // Every angle of a turn is exact in an f32 (24-bit mantissa).
    const { assert!(ANGLE_UNIT < 1 << 24) };

    let again: FrameInfo = serde_json::from_str(&serde_json::to_string(&polar).unwrap()).unwrap();
    assert_eq!(again, polar);
    assert!(info(json!({"geometry": {"kind": "spiral"}})).is_err());
    // the discriminator decides, even when the other fields would fit
    assert!(info(json!({"geometry": {"kind": "polar", "origin": "top", "row_h": 1, "rows": 1, "gap": 1, "root": rect}})).is_err());
}

fn settings(v: Value) -> Result<ViewSettingsParams, ParamsError> {
    parse_params_str(&v.to_string())
}

#[test]
fn visual_ids_accepted() {
    for visual in BUILTIN_VISUALS
        .iter()
        .copied()
        .chain(["plugin.heat-map", "a1"])
    {
        assert_eq!(
            settings(json!({"visual": visual}))
                .unwrap()
                .visual
                .as_deref(),
            Some(visual)
        );
    }
}

#[test]
fn visual_ids_rejected() {
    let long = "a".repeat(49);
    for visual in [
        "",
        "Flame",
        "1flame",
        "flame_",
        "flame__polar",
        long.as_str(),
        "x/y",
    ] {
        let err = settings(json!({"visual": visual})).unwrap_err();
        assert_eq!(err.loc, ["visual"], "{visual:?}");
        assert_eq!(err.code(), ErrorCode::InvalidParams);
    }
}

#[test]
fn branch_colour_mode_and_visual_list() {
    assert_eq!(
        settings(json!({"color_mode": "branch"}))
            .unwrap()
            .color_mode,
        Some(ColorModeName::Branch)
    );
    let result: VisualListResult = serde_json::from_value(json!({
        "visuals": [{
            "id": "flame_polar", "layout": POLAR_LAYOUT, "name": "Polar flame graph", "icon": "viz-flame-polar",
            "order": 2, "compare": true, "free_space": true, "default_color": "branch", "available": false,
            "reason": "This window can't show this visualization",
        }]
    }))
    .unwrap();
    assert_eq!(result.visuals[0].default_color, ColorModeName::Branch);
    assert!(!result.visuals[0].available);
}

#[test]
fn params_reject_unknown_fields_and_check_constraints() {
    use sk_protocol::scan::*;
    let ok: TreeChildrenParams = parse_params_str(r#"{"scan_id": "s1", "node": 3}"#).unwrap();
    assert_eq!((ok.offset, ok.limit, ok.sort), (0, 200, ChildSort::Size));
    assert!(
        parse_params_str::<TreeChildrenParams>(r#"{"scan_id": "s1", "node": 3, "nope": 1}"#)
            .is_err()
    );
    let e = parse_params_str::<TreeChildrenParams>(r#"{"scan_id": "s1", "node": 3, "limit": 0}"#)
        .unwrap_err();
    assert_eq!(
        (e.loc.clone(), e.kind),
        (vec!["limit".to_owned()], "greater_than_equal")
    );
    let e = parse_params_str::<TreeChildrenParams>(r#"{"scan_id": "", "node": 3}"#).unwrap_err();
    assert_eq!(e.kind, "string_too_short");
    assert_eq!(
        e.data(),
        json!([{"loc": ["scan_id"], "msg": "String should have at least 1 character", "type": "string_too_short"}])
    );
    assert!(parse_params_str::<TreeChildrenParams>(r#"{"scan_id": "s", "node": -1}"#).is_err());
    assert!(parse_params_str::<ScanStartParams>(r#"{"roots": []}"#).is_err());
    assert!(parse_params_str::<ScanStartParams>(r#"{"roots": ["/"], "workers": 65}"#).is_err());
    assert!(
        parse_params_str::<ScanStartParams>(
            r#"{"roots": ["/"], "load_level": "custom", "load_custom": 50}"#
        )
        .is_ok()
    );
    // nested models carry their path
    let e = parse_params_str::<ViewOpenParams>(
        r#"{"scan_id": "s", "viewport": {"width": 0, "height": 10}}"#,
    )
    .unwrap_err();
    assert_eq!(e.loc, ["viewport", "width"]);
    let v: ViewOpenParams =
        parse_params_str(r#"{"scan_id": "s", "viewport": {"width": 10, "height": 10}}"#).unwrap();
    assert_eq!(v.viewport.dpr, 1.0);
    assert!(parse_params_str::<ViewZoomParams>(r#"{"view_id": "v", "key": 4294967296}"#).is_err());
    // empty params for methods without arguments
    let _: Empty = parse_params(None).unwrap();
    assert!(parse_params_str::<Empty>(r#"{"x": 1}"#).is_err());
    // the handshake
    let hello = r#"{"token": "0123456789abcdef", "protocol": "1.1",
        "client": {"name": "t", "version": "0.1", "kind": "test"}, "capabilities": ["cancel"]}"#;
    let h: session::HelloParams = parse_params_str(hello).unwrap();
    assert_eq!(h.client.kind, session::ClientKind::Test);
    assert!(parse_params_str::<session::HelloParams>(&hello.replace("1.1", "1")).is_err());
    assert!(
        parse_params_str::<session::HelloParams>(&hello.replace("0123456789abcdef", "short"))
            .is_err()
    );
    let e = parse_params_str::<session::HelloParams>(&hello.replace("\"t\"", "\"\"")).unwrap_err();
    assert_eq!(e.loc, ["client", "name"]);
    // update.mute
    assert!(
        parse_params_str::<update::UpdateMuteParams>(r#"{"series": "0.4"}"#)
            .unwrap()
            .muted
    );
    assert!(parse_params_str::<update::UpdateMuteParams>(r#"{"series": "0.4.1"}"#).is_err());
    // class editor colours
    let ok = r##"{"classes": [{"name": "x", "color": "#A0B0C0"}]}"##;
    assert!(parse_params_str::<classes::ClassesUpdateParams>(ok).is_ok());
    let e = parse_params_str::<classes::ClassesUpdateParams>(&ok.replace("#A0B0C0", "red"))
        .unwrap_err();
    assert_eq!(e.loc, ["classes", "0", "color"]);
    assert!(parse_params_str::<classes::ClassesUpdateParams>(r#"{"classes": []}"#).is_err());
    // machine.read_health params are lenient (a Payload in Python)
    assert!(parse_params_str::<machine::ReadHealthParams>(r#"{"disks": [], "extra": 1}"#).is_ok());
}

#[test]
fn outputs_serialise_every_field() {
    let status = update::UpdateStatus {
        current: "0.4.0".into(),
        install_kind: update::InstallKind::Dev,
        enabled: false,
        channel: update::UpdateChannel::Preview,
        checking: false,
        checked_at: None,
        error: None,
        level: update::UpdateLevel::None,
        latest: None,
        dismissed: false,
        series: None,
        muted: false,
        download: update::UpdateDownload::default(),
        next_check_at: None,
    };
    let v = serde_json::to_value(&status).unwrap();
    assert_eq!(v["checked_at"], Value::Null);
    assert_eq!(
        v["download"],
        json!({"state": "idle", "version": null, "received": 0, "total": 0, "path": null, "verified": null, "error": null})
    );
    assert_eq!(v.as_object().unwrap().len(), 14);
    // timestamps are RFC 3339 in UTC
    let ping = system::PingResult {
        payload: None,
        server_time: "2025-01-02T03:04:05.5Z".parse().unwrap(),
    };
    assert_eq!(
        serde_json::to_value(&ping).unwrap(),
        json!({"payload": null, "server_time": "2025-01-02T03:04:05.500Z"})
    );
    // literal enums keep their wire spelling
    assert_eq!(
        serde_json::to_value(machine::HealthNote::None).unwrap(),
        json!("")
    );
    assert_eq!(
        serde_json::to_value(settings::Language::ZhCn).unwrap(),
        json!("zh-CN")
    );
    assert_eq!(
        serde_json::to_value(streams::LogLevel::Warning).unwrap(),
        json!("WARNING")
    );
}

#[test]
fn payloads_ignore_unknown_fields() {
    let s: system::ShutdownResult =
        serde_json::from_str(r#"{"accepted": true, "added_later": 1}"#).unwrap();
    assert!(s.accepted);
}

#[test]
fn every_method_params_type_parses_its_own_default_shape() {
    // Each method's params type exists in the schema registry with a resolvable name.
    let doc = schema::export_schema();
    for m in METHODS {
        let p = doc["methods"][m.name]["params"].as_str().unwrap();
        assert!(p.starts_with("#/$defs/"), "{}: {p}", m.name);
    }
    // marker types tie names to types
    assert_eq!(<methods::ScanStart as Method>::NAME, methods::SCAN_START);
    let _: <methods::SystemPing as Method>::Params = parse_params(None).unwrap();
}

#[test]
fn classes_localize() {
    let video = &classes::builtin_classes()[0];
    assert_eq!(video.id, "video");
    let en = classes::localized(video, "en");
    assert_eq!(en.name, "Video");
    assert!(en.aliases.contains(&"视频".to_owned()));
    assert!(!en.aliases.iter().any(|a| a == "Video"));
    assert_eq!(classes::localized(video, "zh-CN"), *video);
    assert!(
        classes::builtin_classes()[0]
            .extensions
            .contains(&"mp4".to_owned())
    );
    assert!(
        classes::builtin_classes()
            .iter()
            .find(|c| c.id == "game")
            .unwrap()
            .extensions
            .is_empty()
    );
}

#[test]
fn generated_typescript_is_up_to_date() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("generated")
        .join("protocol.ts");
    let current = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .replace("\r\n", "\n");
    assert!(
        current == ts::generate(),
        "{} is stale: run `cargo run -p sk-protocol --bin gen-ts`",
        path.display()
    );
}
