//! The partition layouts' geometry constants are written twice: in the design tokens (frontends)
//! and in `layout::flame`, `layout::flame_polar` (backend). They must agree (Python
//! `test_visual_tokens.py`).

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde_json::Value;
use sk_core::layout::{flame, flame_polar};

fn tokens() -> Value {
    let rel = "design/tokens/visual.tokens.json";
    let candidates = [
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(rel),
        PathBuf::from(file!()).join("../../../..").join(rel),
    ];
    let path = candidates
        .iter()
        .find(|p| p.exists())
        .expect("design/tokens/visual.tokens.json");
    let all: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    all["visual"].clone()
}

/// A token's number: a dimension's value (CSS px) or a plain number.
fn value(group: &Value, name: &str) -> f64 {
    let v = &group[name]["$value"];
    match v {
        Value::Object(o) => {
            assert_eq!(o["unit"], "px");
            o["value"].as_f64().unwrap()
        }
        _ => v.as_f64().unwrap(),
    }
}

#[test]
fn flame_graph_constants_match_the_tokens() {
    let t = tokens();
    let g = &t["flame"];
    assert_eq!(value(g, "rowMin"), flame::ROW_MIN as f64);
    assert_eq!(value(g, "rowMax"), flame::ROW_MAX as f64);
    assert_eq!(value(g, "gap"), flame::GAP as f64);
}

#[test]
fn polar_flame_graph_constants_match_the_tokens() {
    let t = tokens();
    let g = &t["flamePolar"];
    assert_eq!(value(g, "margin"), flame_polar::MARGIN as f64);
    assert_eq!(value(g, "holeRatio"), flame_polar::HOLE_RATIO);
    assert_eq!(value(g, "holeMin"), flame_polar::HOLE_MIN as f64);
    assert_eq!(value(g, "holeMaxRatio"), flame_polar::HOLE_MAX_RATIO);
    assert_eq!(value(g, "ringMin"), flame_polar::RING_MIN as f64);
    assert_eq!(value(g, "gap"), flame_polar::GAP as f64);
}

#[test]
fn every_geometry_token_is_checked() {
    let t = tokens();
    let names = |g: &Value| -> BTreeSet<String> {
        g.as_object()
            .unwrap()
            .keys()
            .filter(|k| !k.starts_with('$'))
            .cloned()
            .collect()
    };
    let set = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>();
    assert_eq!(names(&t["flame"]), set(&["rowMin", "rowMax", "gap"]));
    // segments: how finely frontends tessellate a sector; the backend never needs it
    assert_eq!(
        names(&t["flamePolar"]),
        set(&[
            "margin",
            "holeRatio",
            "holeMin",
            "holeMaxRatio",
            "ringMin",
            "gap",
            "segments"
        ])
    );
}
