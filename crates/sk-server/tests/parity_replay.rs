//! The protocol parity gate without Python (tests/parity/README.md): every scenario of
//! `tests/parity/scenarios` runs against the real `spaceknife-cli serve` and its normalized
//! transcript must equal the 0.3.4 reference's, recorded in `tests/parity/golden/<platform>/`.
//!
//! This file mirrors `tests/parity/skparity/` (engine.py, norm.py, fixtures.py): the steps, the
//! normalization and the comparison are the same, rule for rule. Change both together.
//! A platform without goldens (macOS today) runs nothing and says so.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::sync::{oneshot, watch};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

const TOKEN: &str = "parity-token-0123456789abcdef";
const SUBPROTOCOL: &str = "spaceknife.v1";
const CAPABILITIES: [&str; 8] = [
    "binary-frames.v1",
    "subscriptions",
    "cancel",
    "log-stream",
    "visuals.v1",
    "layout.flame.v1",
    "layout.polar.v1",
    "updates.v1",
];
const KEY_NODE_MASK: u64 = (1 << 30) - 1;
const DAY: f64 = 86400.0;

fn parity_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/parity")
}

fn platform() -> &'static str {
    if cfg!(windows) {
        "win32"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    }
}

fn read_json(p: &Path) -> Value {
    let text = std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

// ============================================================================ rules

/// normalize.json with a scenario's `normalize` additions (lists extended, maps merged).
fn load_rules(extra: Option<&Value>) -> Value {
    let mut rules = read_json(&parity_dir().join("normalize.json"));
    if let Some(Value::Object(extra)) = extra {
        for (k, v) in extra {
            match (rules.get_mut(k), v) {
                (Some(Value::Array(a)), Value::Array(b)) => a.extend(b.iter().cloned()),
                (Some(Value::Object(a)), Value::Object(b)) => {
                    for (bk, bv) in b {
                        a.insert(bk.clone(), bv.clone());
                    }
                }
                _ => {
                    rules[k] = v.clone();
                }
            }
        }
    }
    rules
}

fn strs(v: &Value) -> BTreeSet<String> {
    v.as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect())
        .unwrap_or_default()
}

fn is_int(v: &Value) -> bool {
    matches!(v, Value::Number(n) if n.is_i64() || n.is_u64())
}

fn int_of(v: &Value) -> i64 {
    v.as_i64()
        .or_else(|| v.as_u64().map(|u| u as i64))
        .unwrap_or(0)
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

// ============================================================================ normalization

/// ISO 8601 date-time as both backends write it.
fn iso_time() -> &'static regex::Regex {
    static R: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    R.get_or_init(|| {
        regex::Regex::new(
            r"^\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}(:\d{2}(\.\d+)?)?(Z|[+-]\d{2}:?\d{2})?$",
        )
        .unwrap()
    })
}

/// A date-time inside a line of text (exports).
fn inline_time() -> &'static regex::Regex {
    static R: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    R.get_or_init(|| {
        regex::Regex::new(
            r"\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}(:\d{2}(\.\d+)?)?(Z|[+-]\d{2}:?\d{2})?",
        )
        .unwrap()
    })
}

/// A snapshot file's time stamp (`20261108-030158`, not part of a longer number) → `<stamp>`.
fn replace_stamps(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let digit = |j: usize| j < b.len() && b[j].is_ascii_digit();
    while i < b.len() {
        let fits = i + 15 <= b.len()
            && (0..8).all(|k| b[i + k].is_ascii_digit())
            && b[i + 8] == b'-'
            && (9..15).all(|k| b[i + k].is_ascii_digit())
            && !(i > 0 && b[i - 1].is_ascii_digit())
            && !digit(i + 15);
        if fits {
            out.push_str("<stamp>");
            i += 15;
        } else {
            let ch = s[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

/// Types only: objects keep their non-null keys, lists the merged shape of their items.
fn shape(v: &Value) -> Value {
    match v {
        Value::Null => json!("null"),
        Value::Bool(_) => json!("bool"),
        Value::Number(_) => json!("number"),
        Value::String(_) => json!("string"),
        Value::Array(a) => {
            let Some(first) = a.first() else {
                return json!([]);
            };
            let mut merged = shape(first);
            for item in &a[1..] {
                merged = merge(&merged, &shape(item));
            }
            json!([merged])
        }
        Value::Object(o) => Value::Object(
            o.iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k.clone(), shape(v)))
                .collect(),
        ),
    }
}

fn merge(a: &Value, b: &Value) -> Value {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let mut out = x.clone();
            for (k, v) in y {
                let m = match out.get(k) {
                    Some(old) => merge(old, v),
                    None => v.clone(),
                };
                out.insert(k.clone(), m);
            }
            Value::Object(out)
        }
        (Value::Array(x), Value::Array(y)) => {
            if x.is_empty() || y.is_empty() {
                if x.is_empty() { b.clone() } else { a.clone() }
            } else {
                json!([merge(&x[0], &y[0])])
            }
        }
        _ if a == b => a.clone(),
        _ => json!("mixed"),
    }
}

/// One backend's transcript: id symbols, node paths and path prefixes.
struct Normalizer {
    drop: BTreeSet<String>,
    drop_in: HashMap<String, BTreeSet<String>>,
    shape_keys: BTreeSet<String>,
    server_drop: BTreeSet<String>,
    server_marker: BTreeSet<String>,
    times: BTreeSet<String>,
    id_keys: HashMap<String, String>,
    markers: Vec<(BTreeSet<String>, String, String)>,
    node_keys: BTreeSet<String>,
    key_keys: BTreeSet<String>,
    node_info: BTreeSet<String>,
    /// (placeholder, real path), longest real path first
    places: Vec<(String, String)>,
    symbols: HashMap<String, String>,
    counts: HashMap<String, usize>,
    nodes: HashMap<(String, i64), String>,
    scan: String,
}

impl Normalizer {
    fn new(rules: &Value, places: &[(String, String)]) -> Normalizer {
        let mut places = places.to_vec();
        places.sort_by_key(|(_, real)| std::cmp::Reverse(real.len()));
        Normalizer {
            drop: strs(&rules["drop_keys"]),
            drop_in: rules["drop_in"]
                .as_object()
                .map(|o| o.iter().map(|(k, v)| (k.clone(), strs(v))).collect())
                .unwrap_or_default(),
            shape_keys: strs(&rules["shape_keys"]),
            server_drop: strs(&rules["server_info_drop"]),
            server_marker: strs(&rules["server_info_marker"]),
            times: strs(&rules["time_keys"]),
            id_keys: rules["id_keys"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_owned()))
                .collect(),
            markers: rules["symbol_markers"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|m| {
                            (
                                strs(&m["marker"]),
                                m["key"].as_str().unwrap().to_owned(),
                                m["kind"].as_str().unwrap().to_owned(),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default(),
            node_keys: strs(&rules["node_keys"]),
            key_keys: strs(&rules["key_keys"]),
            node_info: strs(&rules["node_info_marker"]),
            places,
            symbols: HashMap::new(),
            counts: HashMap::new(),
            nodes: HashMap::new(),
            scan: String::new(),
        }
    }

    fn has_all(o: &Map<String, Value>, keys: &BTreeSet<String>) -> bool {
        keys.iter().all(|k| o.contains_key(k))
    }

    fn symbol(&mut self, kind: &str, raw: &str) -> String {
        if let Some(s) = self.symbols.get(raw) {
            return s.clone();
        }
        let n = self.counts.entry(kind.to_owned()).or_insert(0);
        *n += 1;
        let s = format!("<{kind}#{n}>");
        self.symbols.insert(raw.to_owned(), s.clone());
        s
    }

    /// Node ids a value names (node keys, layout keys, NodeInfo ids) to resolve first.
    fn node_ids(&self, v: &Value, out: &mut BTreeSet<i64>) {
        match v {
            Value::Object(o) => {
                let info = Self::has_all(o, &self.node_info);
                for (k, val) in o {
                    if is_int(val) {
                        let n = int_of(val);
                        if self.node_keys.contains(k) || (info && k == "id") {
                            if n >= 0 {
                                out.insert(n);
                            }
                        } else if self.key_keys.contains(k) && n >= 0 {
                            out.insert(n & KEY_NODE_MASK as i64);
                        }
                    } else if !val.is_boolean() {
                        self.node_ids(val, out);
                    }
                }
            }
            Value::Array(a) => {
                for x in a {
                    self.node_ids(x, out);
                }
            }
            _ => {}
        }
    }

    fn node_ref(&self, nid: i64) -> String {
        if nid < 0 {
            return "@none".into();
        }
        match self.nodes.get(&(self.scan.clone(), nid)) {
            Some(p) => format!("@{p}"),
            None => format!("@?{nid}"),
        }
    }

    fn key_ref(&self, key: i64) -> String {
        let (tag, low) = (key >> 30, key & KEY_NODE_MASK as i64);
        match tag {
            0 => self.node_ref(low),
            1 => format!("@small:{}", self.node_ref(low)),
            2 => format!("@more:{}", self.node_ref(low)),
            _ => match low {
                1 => "@free".into(),
                2 => "@unknown".into(),
                _ => format!("@k3:{low}"),
            },
        }
    }

    /// How a backend may spell a folder: as is, with `/`, inside JSON text, as WSL sees it.
    fn variants(real: &str) -> Vec<String> {
        let mut out = vec![real.to_owned(), real.replace('\\', "/")];
        if real.contains('\\') {
            out.push(real.replace('\\', "\\\\"));
        }
        let b = real.as_bytes();
        if b.len() > 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
            out.push(format!(
                "/mnt/{}/{}",
                (b[0] as char).to_ascii_lowercase(),
                real[3..].replace('\\', "/")
            ));
        }
        out
    }

    fn path_text(&self, s: &str) -> String {
        let mut s = s.to_owned();
        let mut hit = false;
        for (name, real) in &self.places {
            for variant in Self::variants(real) {
                if variant.is_empty() {
                    continue;
                }
                if cfg!(windows) {
                    let vl = variant.to_ascii_lowercase();
                    let mut from = 0;
                    while let Some(i) = s.to_ascii_lowercase()[from..].find(&vl).map(|i| i + from) {
                        s = format!("{}{}{}", &s[..i], name, &s[i + variant.len()..]);
                        hit = true;
                        from = i + name.len();
                    }
                } else if s.contains(&variant) {
                    s = s.replace(&variant, name);
                    hit = true;
                }
            }
        }
        if hit {
            s = s.replace("\\\\?\\$", "$");
            if cfg!(windows) {
                s = s.replace("\\\\", "/").replace('\\', "/");
            }
            s = replace_stamps(&s);
        }
        s
    }

    fn text(&self, s: &str) -> String {
        if let Some(sym) = self.symbols.get(s) {
            return sym.clone();
        }
        if iso_time().is_match(s) {
            return "<time>".into();
        }
        self.path_text(s)
    }

    fn time(v: &Value) -> Value {
        match v {
            Value::Null => Value::Null,
            Value::Bool(_) => json!("<bad-time>"),
            Value::Number(n) => {
                if n.as_f64().unwrap_or(0.0) > 0.0 {
                    json!("<time>")
                } else {
                    v.clone()
                }
            }
            Value::String(s) if iso_time().is_match(s) => json!("<time>"),
            _ => json!("<bad-time>"),
        }
    }

    fn marked_id(&self, o: &Map<String, Value>, key: &str) -> Option<String> {
        self.markers
            .iter()
            .find(|(m, k, _)| k == key && Self::has_all(o, m))
            .map(|(_, _, kind)| kind.clone())
    }

    fn norm(&mut self, v: &Value, parent: &str) -> Value {
        match v {
            Value::Object(o) => {
                let without_version: BTreeSet<&String> =
                    o.keys().filter(|k| k.as_str() != "version").collect();
                let server = Self::has_all(o, &self.server_marker)
                    && self.server_drop.iter().any(|k| without_version.contains(k));
                let info = Self::has_all(o, &self.node_info);
                let inner = self.drop_in.get(parent).cloned().unwrap_or_default();
                let mut out = Map::new();
                for (k, val) in o {
                    if self.drop.contains(k)
                        || inner.contains(k)
                        || (server && self.server_drop.contains(k))
                    {
                        continue;
                    }
                    let n = if self.shape_keys.contains(k) && !val.is_null() {
                        shape(val)
                    } else if self.times.contains(k) {
                        Self::time(val)
                    } else if let (Some(kind), Value::String(s)) = (self.id_keys.get(k), val) {
                        let kind = kind.clone();
                        json!(self.symbol(&kind, s))
                    } else if let (Value::String(s), Some(kind)) = (val, self.marked_id(o, k)) {
                        json!(self.symbol(&kind, s))
                    } else if is_int(val) && (self.node_keys.contains(k) || (info && k == "id")) {
                        json!(self.node_ref(int_of(val)))
                    } else if is_int(val) && self.key_keys.contains(k) {
                        json!(self.key_ref(int_of(val)))
                    } else {
                        self.norm(val, k)
                    };
                    out.insert(k.clone(), n);
                }
                Value::Object(out)
            }
            Value::Array(a) => Value::Array(a.iter().map(|x| self.norm(x, parent)).collect()),
            Value::String(s) => json!(self.text(s)),
            other => other.clone(),
        }
    }

    /// Code and the first item's `loc`.
    fn error(&mut self, e: &Value) -> Value {
        let mut loc = Value::Null;
        if let Some(first) = e["data"].as_array().and_then(|a| a.first())
            && let Some(l) = first.get("loc")
            && first.is_object()
        {
            loc = Value::Array(
                l.as_array()
                    .map(|a| a.iter().map(|x| self.norm(x, "")).collect())
                    .unwrap_or_default(),
            );
        }
        json!({"code": e["code"], "loc": loc})
    }

    fn text_stream(&self, s: &str) -> String {
        if let Some((head, tail)) = s.split_once(':')
            && let Some(sym) = self.symbols.get(tail)
        {
            return format!("{head}:{sym}");
        }
        s.to_owned()
    }

    fn frame(&self, f: &Frame) -> Value {
        let mut cols = Map::new();
        for (name, (dtype, values)) in &f.columns {
            let vals: Vec<Value> = match name.as_str() {
                "key" => values
                    .iter()
                    .map(|x| json!(self.key_ref(x.as_f64().unwrap() as i64)))
                    .collect(),
                "node" => values
                    .iter()
                    .map(|x| json!(self.node_ref(x.as_f64().unwrap() as i64)))
                    .collect(),
                _ => values.clone(),
            };
            cols.insert(name.clone(), json!({"dtype": dtype, "values": vals}));
        }
        let h = &f.header;
        let mut out = json!({
            "stream": self.text_stream(h["stream"].as_str().unwrap_or("")),
            "layout": h["layout"],
            "seq": h["seq"],
            "final": h.get("final").cloned().unwrap_or(json!(true)),
            "count": h["count"],
            "columns": cols,
        });
        if let (Some(names), Some((_, lens))) = (f.blobs.get("names"), f.columns.get("name_len")) {
            let mut pos = 0usize;
            let mut list = Vec::new();
            for n in lens {
                let n = n.as_f64().unwrap() as usize;
                let end = (pos + n).min(names.len());
                list.push(json!(
                    self.path_text(&String::from_utf8_lossy(&names[pos.min(end)..end]))
                ));
                pos += n;
            }
            out["names"] = Value::Array(list);
        }
        let other: BTreeMap<&String, usize> = f
            .blobs
            .iter()
            .filter(|(k, _)| k.as_str() != "names")
            .map(|(k, v)| (k, v.len()))
            .collect();
        if !other.is_empty() {
            out["blobs"] = json!(other);
        }
        out
    }
}

// ============================================================================ binary frames

struct Frame {
    header: Value,
    columns: BTreeMap<String, (String, Vec<Value>)>,
    blobs: BTreeMap<String, Vec<u8>>,
}

fn decode(buf: &[u8]) -> Frame {
    let n = u32::from_le_bytes(buf[..4].try_into().unwrap()) as usize;
    let header: Value = serde_json::from_slice(&buf[4..4 + n]).unwrap();
    let mut start = 4 + n;
    start += (8 - start % 8) % 8;
    let count = header["count"].as_u64().unwrap() as usize;
    let mut columns = BTreeMap::new();
    for (name, spec) in header["columns"].as_object().cloned().unwrap_or_default() {
        let dtype = spec["dtype"].as_str().unwrap().to_owned();
        let off = start + spec["offset"].as_u64().unwrap() as usize;
        let size = match dtype.as_str() {
            "<u1" | "<i1" => 1,
            "<u2" | "<i2" => 2,
            "<u4" | "<i4" | "<f4" => 4,
            _ => 8,
        };
        let values = (0..count)
            .map(|i| {
                let b = &buf[off + i * size..off + (i + 1) * size];
                match dtype.as_str() {
                    "<u1" => json!(b[0]),
                    "<i1" => json!(b[0] as i8),
                    "<u2" => json!(u16::from_le_bytes(b.try_into().unwrap())),
                    "<i2" => json!(i16::from_le_bytes(b.try_into().unwrap())),
                    "<u4" => json!(u32::from_le_bytes(b.try_into().unwrap())),
                    "<i4" => json!(i32::from_le_bytes(b.try_into().unwrap())),
                    "<u8" => json!(u64::from_le_bytes(b.try_into().unwrap())),
                    "<i8" => json!(i64::from_le_bytes(b.try_into().unwrap())),
                    "<f4" => json!(f32::from_le_bytes(b.try_into().unwrap()) as f64),
                    _ => json!(f64::from_le_bytes(b.try_into().unwrap())),
                }
            })
            .collect();
        columns.insert(name, (dtype, values));
    }
    let mut blobs = BTreeMap::new();
    for (name, spec) in header["blobs"].as_object().cloned().unwrap_or_default() {
        let off = start + spec["offset"].as_u64().unwrap() as usize;
        let len = spec["length"].as_u64().unwrap() as usize;
        blobs.insert(name, buf[off..off + len].to_vec());
    }
    Frame {
        header,
        columns,
        blobs,
    }
}

// ============================================================================ comparison

fn escape(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

/// Differences as (path, expected, actual); numbers within the relative tolerance are equal.
fn compare(exp: &Value, act: &Value, rules: &Value) -> Vec<(String, Value, Value)> {
    fn num(x: &Value) -> bool {
        x.is_number()
    }
    fn walk(e: &Value, a: &Value, p: &str, t: f64, f4: f64, out: &mut Vec<(String, Value, Value)>) {
        match (e, a) {
            (Value::Object(eo), Value::Object(ao)) => {
                let t2 = if eo.get("dtype") == Some(&json!("<f4")) {
                    f4
                } else {
                    t
                };
                let keys: BTreeSet<&String> = eo.keys().chain(ao.keys()).collect();
                for k in keys {
                    let q = format!("{p}/{}", escape(k));
                    match (eo.get(k), ao.get(k)) {
                        (Some(x), None) => out.push((q, x.clone(), json!("<missing>"))),
                        (None, Some(y)) => out.push((q, json!("<missing>"), y.clone())),
                        (Some(x), Some(y)) => walk(x, y, &q, t2, f4, out),
                        (None, None) => {}
                    }
                }
            }
            (Value::Array(ea), Value::Array(aa)) => {
                for i in 0..ea.len().min(aa.len()) {
                    walk(&ea[i], &aa[i], &format!("{p}/{i}"), t, f4, out);
                }
                if ea.len() != aa.len() {
                    out.push((format!("{p}/#len"), json!(ea.len()), json!(aa.len())));
                }
            }
            _ if num(e) && num(a) => {
                let (x, y) = (e.as_f64().unwrap(), a.as_f64().unwrap());
                let same_int = match (e.as_i64(), a.as_i64()) {
                    (Some(i), Some(j)) => i == j,
                    _ => false,
                } || matches!((e.as_u64(), a.as_u64()), (Some(i), Some(j)) if i == j);
                if !same_int
                    && x != y
                    && !(x.is_finite() && y.is_finite() && (x - y).abs() <= t * x.abs().max(y.abs()))
                {
                    out.push((p.to_owned(), e.clone(), a.clone()));
                }
            }
            _ => {
                if e != a {
                    out.push((p.to_owned(), e.clone(), a.clone()));
                }
            }
        }
    }
    let mut out = Vec::new();
    let tol = rules["float_rel_tol"].as_f64().unwrap_or(1e-9);
    let f4 = rules["f4_rel_tol"].as_f64().unwrap_or(1e-6);
    walk(exp, act, "steps", tol, f4, &mut out);
    out
}

/// Records by their step index (`steps/<i>/…`).
fn keyed(transcript: &[Value]) -> Value {
    let mut m = Map::new();
    for r in transcript {
        let mut r = r.as_object().unwrap().clone();
        let i = r.remove("i").unwrap();
        m.insert(int_of(&i).to_string(), Value::Object(r));
    }
    Value::Object(m)
}

/// fnmatch within one segment: `*` any characters, `?` one.
fn seg_match(p: &[char], s: &[char]) -> bool {
    match p.first() {
        None => s.is_empty(),
        Some('*') => (0..=s.len()).any(|k| seg_match(&p[1..], &s[k..])),
        Some('?') => !s.is_empty() && seg_match(&p[1..], &s[1..]),
        Some(c) => s.first() == Some(c) && seg_match(&p[1..], &s[1..]),
    }
}

/// `*` within one path segment, `**` any number of segments.
fn glob_match(pattern: &str, path: &str) -> bool {
    let pat: Vec<Vec<char>> = pattern
        .trim_matches('/')
        .split('/')
        .map(|s| s.chars().collect())
        .collect();
    let segs: Vec<Vec<char>> = path
        .trim_matches('/')
        .split('/')
        .map(|s| s.chars().collect())
        .collect();
    fn m(pat: &[Vec<char>], segs: &[Vec<char>]) -> bool {
        match pat.first() {
            None => segs.is_empty(),
            Some(p) if p.iter().collect::<String>() == "**" => {
                (0..=segs.len()).any(|k| m(&pat[1..], &segs[k..]))
            }
            Some(p) => !segs.is_empty() && seg_match(p, &segs[0]) && m(&pat[1..], &segs[1..]),
        }
    }
    m(&pat, &segs)
}

// ============================================================================ fixtures

fn set_times(p: &Path, created: Option<f64>, accessed: f64, modified: f64) {
    let at = |t: f64| SystemTime::UNIX_EPOCH + Duration::from_secs_f64(t.max(0.0));
    let times = std::fs::FileTimes::new()
        .set_accessed(at(accessed))
        .set_modified(at(modified));
    #[cfg(windows)]
    {
        use std::os::windows::fs::{FileTimesExt, OpenOptionsExt};
        let times = match created {
            Some(c) => times.set_created(at(c)),
            None => times,
        };
        // FILE_WRITE_ATTRIBUTES, FILE_FLAG_BACKUP_SEMANTICS (folders too)
        let f = std::fs::OpenOptions::new()
            .access_mode(0x100)
            .custom_flags(0x0200_0000)
            .open(p)
            .unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        f.set_times(times).unwrap();
    }
    #[cfg(not(windows))]
    {
        let _ = created;
        let f = std::fs::File::open(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        f.set_times(times).unwrap();
    }
}

#[cfg(windows)]
fn set_attrs(p: &Path, f: &Value) {
    use std::os::windows::ffi::OsStrExt;
    let mut attrs = 0x20u32; // ARCHIVE
    if f["hidden"] == true {
        attrs |= 0x2;
    }
    if f["readonly"] == true {
        attrs |= 0x1;
    }
    if f["system"] == true {
        attrs |= 0x4;
    }
    let wide: Vec<u16> = p.as_os_str().encode_wide().chain([0]).collect();
    // SAFETY: a NUL-terminated wide path.
    unsafe { windows_sys::Win32::Storage::FileSystem::SetFileAttributesW(wide.as_ptr(), attrs) };
}

/// `range` entries expanded: `{i}` the index, `{i3}` the index padded to 3.
fn expand(spec: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    for f in spec["files"].as_array().cloned().unwrap_or_default() {
        if let Some(n) = f.get("range").and_then(Value::as_u64) {
            for i in 0..n {
                let mut g = f.clone();
                let path = f["path"]
                    .as_str()
                    .unwrap()
                    .replace("{i3}", &format!("{i:03}"))
                    .replace("{i}", &i.to_string());
                g["path"] = json!(path);
                g["size"] = json!(f["size"].as_u64().unwrap() + f["step"].as_u64().unwrap_or(0) * i);
                g.as_object_mut().unwrap().remove("range");
                g.as_object_mut().unwrap().remove("step");
                out.push(g);
            }
        } else {
            out.push(f);
        }
    }
    out
}

fn rel(root: &Path, p: &str) -> PathBuf {
    p.split('/').fold(root.to_path_buf(), |acc, part| acc.join(part))
}

fn build_fixture(root: &Path, name: &str) {
    let all = read_json(&parity_dir().join("fixtures/trees.json"));
    let spec = &all[name];
    assert!(spec.is_object(), "no fixture tree {name}");
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    let fixed = spec["mtime"].as_f64();
    std::fs::create_dir_all(root).unwrap();
    let files = expand(spec);
    for f in &files {
        let p = rel(root, f["path"].as_str().unwrap());
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let size = f["size"].as_u64().unwrap();
        if spec["fill"] == "zero" {
            std::fs::File::create(&p).unwrap().set_len(size).unwrap();
        } else {
            std::fs::write(&p, vec![b'x'; size as usize]).unwrap();
        }
    }
    for d in spec["dirs"].as_array().cloned().unwrap_or_default() {
        std::fs::create_dir_all(rel(root, d.as_str().unwrap())).unwrap();
    }
    for f in &files {
        let p = rel(root, f["path"].as_str().unwrap());
        if let Some(md) = f["modified_days"].as_f64() {
            let m = now - md * DAY;
            let a = now - f["accessed_days"].as_f64().unwrap_or(md) * DAY;
            let c = f["created_days"].as_f64().map(|c| now - c * DAY);
            set_times(&p, c, a, m);
        } else if let Some(t) = fixed {
            set_times(&p, Some(t), t, t);
        }
        #[cfg(windows)]
        if ["hidden", "readonly", "system"].iter().any(|k| f[*k] == true) {
            set_attrs(&p, f);
        }
    }
    if let Some(t) = fixed {
        let mut dirs = vec![root.to_path_buf()];
        let mut stack = vec![root.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap() {
                let e = e.unwrap();
                if e.file_type().unwrap().is_dir() {
                    dirs.push(e.path());
                    stack.push(e.path());
                }
            }
        }
        dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
        for d in dirs {
            set_times(&d, Some(t), t, t);
        }
    }
}

/// Remove a folder tree, read-only fixture files included.
fn remove_all(p: &Path) {
    if std::fs::remove_dir_all(p).is_ok() || !p.exists() {
        return;
    }
    let mut stack = vec![p.to_path_buf()];
    while let Some(d) = stack.pop() {
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                let path = e.path();
                if let Ok(md) = std::fs::symlink_metadata(&path) {
                    let mut perm = md.permissions();
                    #[allow(clippy::permissions_set_readonly_false)]
                    perm.set_readonly(false);
                    let _ = std::fs::set_permissions(&path, perm);
                    if md.is_dir() {
                        stack.push(path);
                    }
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(p);
}

// ============================================================================ the upgrade data

/// SHA-1 (FIPS 180-4), for 0.3.4's root keys only.
fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64) * 8;
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes(chunk[i * 4..i * 4 + 4].try_into().unwrap());
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 20];
    for (i, x) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&x.to_be_bytes());
    }
    out
}

/// 0.3.4's `snapshot.root_key` (Python `os.path.normcase(os.path.normpath(r))`, sorted, SHA-1).
fn root_key_034(roots: &[String]) -> String {
    let norm = |r: &str| {
        let mut s = r.to_owned();
        if cfg!(windows) {
            s = s.replace('/', "\\").to_lowercase();
        }
        while s.len() > 1 && (s.ends_with('/') || s.ends_with('\\')) && !s.ends_with(":\\") {
            s.pop();
        }
        s
    };
    let mut normed: Vec<String> = roots.iter().map(|r| norm(r)).collect();
    normed.sort();
    let digest: String = sha1(normed.join("\n").as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()[..12]
        .to_owned();
    let first = roots[0].trim_end_matches(['\\', '/']);
    let base = first.rsplit(['\\', '/']).next().unwrap_or("");
    let base = if base.is_empty() { roots[0].as_str() } else { base };
    let label: String = base
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect();
    let label: String = label.trim_matches('_').chars().take(24).collect();
    format!(
        "{}-{digest}",
        if label.is_empty() { "root" } else { &label }
    )
}

// ============================================================================ backend and client

struct Places {
    tmp: PathBuf,
    home: PathBuf,
    root: PathBuf,
}

impl Places {
    fn make() -> Places {
        // the harness's prefix and length (Python's mkdtemp: 8 characters)
        let tmp = tempfile::Builder::new()
            .prefix("sk-parity-")
            .rand_bytes(8)
            .tempdir()
            .unwrap()
            .keep();
        // the long name, as the backend reports it
        let tmp = std::fs::canonicalize(&tmp).unwrap_or(tmp);
        let tmp = PathBuf::from(
            tmp.to_string_lossy()
                .strip_prefix(r"\\?\")
                .map(str::to_owned)
                .unwrap_or_else(|| tmp.to_string_lossy().into_owned()),
        );
        let (home, root) = (tmp.join("home"), tmp.join("root"));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        Places { tmp, home, root }
    }

    fn mapping(&self) -> Vec<(String, String)> {
        vec![
            ("$ROOT".into(), self.root.to_string_lossy().into_owned()),
            ("$HOME".into(), self.home.to_string_lossy().into_owned()),
            ("$TMP".into(), self.tmp.to_string_lossy().into_owned()),
        ]
    }

    fn path(&self, s: &str) -> String {
        for (name, real) in self.mapping() {
            if s == name || s.starts_with(&format!("{name}/")) {
                let rest = &s[name.len()..].trim_start_matches('/');
                return if rest.is_empty() {
                    real
                } else {
                    rest.split('/')
                        .filter(|p| !p.is_empty())
                        .fold(PathBuf::from(real), |acc, p| acc.join(p))
                        .to_string_lossy()
                        .into_owned()
                };
            }
        }
        s.to_owned()
    }
}

struct Backend {
    child: Option<Child>,
    ready: Value,
}

impl Backend {
    async fn start(home: &Path, cwd: &Path, env: &[(String, String)], args: &[String]) -> Backend {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_spaceknife-cli"));
        cmd.arg("serve")
            .arg("--token-stdin")
            .arg("--log-format")
            .arg("plain")
            .args(args)
            .current_dir(cwd);
        for (k, _) in std::env::vars() {
            if k.starts_with("SPACEKNIFE_") {
                cmd.env_remove(k);
            }
        }
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.env("SPACEKNIFE_HOME", home);
        let err = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(cwd.join("candidate.stderr.log"))
            .unwrap();
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(err)
            .kill_on_drop(true);
        let mut child = cmd.spawn().expect("spaceknife-cli starts");
        let mut stdin = child.stdin.take().unwrap();
        stdin
            .write_all(format!("{TOKEN}\n").as_bytes())
            .await
            .unwrap();
        stdin.flush().await.unwrap();
        // keep stdin open: closing it stops the backend
        let stdout = child.stdout.take().unwrap();
        let mut lines = tokio::io::BufReader::new(stdout).lines();
        let line = tokio::time::timeout(Duration::from_secs(120), lines.next_line())
            .await
            .expect("a ready line in time")
            .unwrap()
            .expect("a ready line");
        let ready: Value = serde_json::from_str(&line).unwrap();
        child.stdin = Some(stdin);
        Backend {
            child: Some(child),
            ready,
        }
    }

    async fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            drop(child.stdin.take());
            if tokio::time::timeout(Duration::from_secs(30), child.wait())
                .await
                .is_err()
            {
                let _ = child.kill().await;
            }
        }
    }

    async fn exited(&mut self, secs: f64) -> bool {
        let Some(child) = self.child.as_mut() else {
            return true;
        };
        let end = Instant::now() + Duration::from_secs_f64(secs);
        while Instant::now() < end {
            if let Ok(Some(_)) = child.try_wait() {
                self.child = None;
                return true;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        false
    }
}

#[derive(Default)]
struct Inbox {
    events: HashMap<String, VecDeque<Value>>,
    frames: HashMap<String, Vec<Vec<u8>>>,
    /// the last event of each name an `events` step took
    last_event: HashMap<String, Value>,
}

/// Drop queued events equal to the one taken last, and repeats of the one before them.
fn squash(q: &mut VecDeque<Value>, last: Option<&Value>) {
    while let (Some(front), Some(l)) = (q.front(), last) {
        if front != l {
            break;
        }
        q.pop_front();
    }
    let mut i = 1;
    while i < q.len() {
        if q[i] == q[i - 1] {
            q.remove(i);
        } else {
            i += 1;
        }
    }
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>;

struct Client {
    tx: tokio::sync::mpsc::UnboundedSender<Message>,
    pending: Pending,
    inbox: Arc<Mutex<Inbox>>,
    arrived: watch::Receiver<u64>,
    next: u64,
    hello: Value,
}

fn hello_frame(token: &str, protocol: &str, caps: &[String]) -> Value {
    json!({"jsonrpc": "2.0", "id": 0, "method": "session.hello",
           "params": {"token": token, "protocol": protocol,
                      "client": {"name": "parity", "version": "0", "kind": "test"},
                      "capabilities": caps}})
}

fn all_caps() -> Vec<String> {
    CAPABILITIES.iter().map(|s| (*s).to_owned()).collect()
}

impl Client {
    async fn open(url: &str, caps: Option<Vec<String>>) -> Result<Client, String> {
        let mut req = url.into_client_request().unwrap();
        req.headers_mut().insert(
            "Sec-WebSocket-Protocol",
            HeaderValue::from_static(SUBPROTOCOL),
        );
        let (ws, _) = tokio_tungstenite::connect_async(req)
            .await
            .map_err(|e| e.to_string())?;
        let (mut sink, mut stream) = ws.split();
        let caps = caps.unwrap_or_else(all_caps);
        sink.send(Message::text(hello_frame(TOKEN, "1.1", &caps).to_string()))
            .await
            .unwrap();
        let first: Value = loop {
            match tokio::time::timeout(Duration::from_secs(30), stream.next()).await {
                Ok(Some(Ok(Message::Text(t)))) => break serde_json::from_str(&t).unwrap(),
                Ok(Some(Ok(_))) => continue,
                other => return Err(format!("no hello answer: {other:?}")),
            }
        };
        if first.get("error").is_some() {
            return Err(format!("hello failed: {}", first["error"]));
        }
        let (tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<Message>();
        tokio::spawn(async move {
            while let Some(m) = out_rx.recv().await {
                if sink.send(m).await.is_err() {
                    break;
                }
            }
        });
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let inbox = Arc::new(Mutex::new(Inbox::default()));
        let (bump, arrived) = watch::channel(0u64);
        {
            let (pending, inbox) = (pending.clone(), inbox.clone());
            tokio::spawn(async move {
                while let Some(Ok(m)) = stream.next().await {
                    match m {
                        Message::Text(t) => {
                            let v: Value = serde_json::from_str(&t).unwrap();
                            if let Some(method) = v.get("method").and_then(Value::as_str) {
                                let params = v.get("params").cloned().unwrap_or(json!({}));
                                inbox
                                    .lock()
                                    .unwrap()
                                    .events
                                    .entry(method.to_owned())
                                    .or_default()
                                    .push_back(params);
                            } else if let Some(id) = v.get("id").and_then(Value::as_u64) {
                                let slot = pending.lock().unwrap().remove(&id);
                                if let Some(slot) = slot {
                                    let _ = slot.send(v);
                                }
                            }
                        }
                        Message::Binary(b) => {
                            let n = u32::from_le_bytes(b[..4].try_into().unwrap()) as usize;
                            let h: Value = serde_json::from_slice(&b[4..4 + n]).unwrap();
                            let key = h.get("request_id").map(|r| r.to_string()).unwrap_or_default();
                            inbox
                                .lock()
                                .unwrap()
                                .frames
                                .entry(key)
                                .or_default()
                                .push(b.to_vec());
                        }
                        Message::Close(_) => break,
                        _ => {}
                    }
                    bump.send_modify(|n| *n += 1);
                }
                pending.lock().unwrap().clear();
                bump.send_modify(|n| *n += 1);
            });
        }
        Ok(Client {
            tx,
            pending,
            inbox,
            arrived,
            next: 1,
            hello: first["result"].clone(),
        })
    }

    async fn call(&mut self, method: &str, params: &Value, secs: f64) -> Result<(u64, Value), String> {
        let id = self.next;
        self.next += 1;
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let frame = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        if self.tx.send(Message::text(frame.to_string())).is_err() {
            return Err("connection closed".into());
        }
        match tokio::time::timeout(Duration::from_secs_f64(secs), rx).await {
            Err(_) => Err("TimeoutError".into()),
            Ok(Err(_)) => Err("connection closed".into()),
            Ok(Ok(v)) => Ok((id, v)),
        }
    }

    async fn wait(&mut self, secs: f64, mut cond: impl FnMut(&mut Inbox) -> bool) -> bool {
        let end = tokio::time::Instant::now() + Duration::from_secs_f64(secs);
        loop {
            if cond(&mut self.inbox.lock().unwrap()) {
                return true;
            }
            match tokio::time::timeout_at(end, self.arrived.changed()).await {
                Ok(Ok(())) => {}
                // timed out, or the connection is gone: nothing more will arrive
                _ => return cond(&mut self.inbox.lock().unwrap()),
            }
        }
    }

    fn close(self) {
        let _ = self.tx.send(Message::Close(None));
    }
}

// ============================================================================ a run

struct Run<'a> {
    sc: &'a Value,
    places: &'a Places,
    feed: String,
    norm: Normalizer,
    saved: HashMap<String, Value>,
    last_scan: String,
    view_scan: HashMap<String, String>,
    backend: Option<Backend>,
    clients: HashMap<String, Client>,
}

fn pick(step: &Value, v: &Value) -> Value {
    let mut v = v.clone();
    if let (Some(keys), Value::Object(o)) = (step.get("pick").and_then(Value::as_array), &v) {
        let mut out = Map::new();
        for k in keys {
            let k = k.as_str().unwrap();
            if let Some(x) = o.get(k) {
                out.insert(k.to_owned(), x.clone());
            }
        }
        v = Value::Object(out);
    }
    if let (Some(keys), Value::Object(o)) = (step.get("omit").and_then(Value::as_array), &mut v) {
        for k in keys {
            o.remove(k.as_str().unwrap());
        }
    }
    v
}

fn secs(step: &Value, key: &str, default: f64) -> f64 {
    step.get(key).and_then(Value::as_f64).unwrap_or(default)
}

impl<'a> Run<'a> {
    fn env(&self) -> Vec<(String, String)> {
        let mut env = vec![
            ("SPACEKNIFE_LANGUAGE".to_owned(), "en".to_owned()),
            ("SPACEKNIFE_UPDATE_FEED".to_owned(), self.feed.clone()),
            ("SPACEKNIFE_APP_VERSION".to_owned(), "0.3.4".to_owned()),
            ("SPACEKNIFE_INSTALL_KIND".to_owned(), "portable".to_owned()),
        ];
        if let Some(o) = self.sc["env"].as_object() {
            for (k, v) in o {
                env.push((k.clone(), v.as_str().unwrap_or("").to_owned()));
            }
        }
        env
    }

    async fn boot(&mut self) {
        let args: Vec<String> = self.sc["args"]
            .as_array()
            .map(|a| a.iter().map(|x| x.as_str().unwrap().to_owned()).collect())
            .unwrap_or_default();
        let b = Backend::start(&self.places.home, &self.places.tmp, &self.env(), &args).await;
        let url = b.ready["url"].as_str().unwrap().to_owned();
        self.backend = Some(b);
        self.clients
            .insert("main".into(), Client::open(&url, None).await.expect("hello"));
    }

    async fn shutdown(&mut self) {
        for (_, c) in self.clients.drain() {
            c.close();
        }
        if let Some(mut b) = self.backend.take() {
            b.stop().await;
        }
    }

    /// `$name.a.0.b` → a saved raw result; `$ROOT/…`, `$HOME/…`, `$TMP/…` → real paths.
    fn resolve(&self, v: &Value) -> Result<Value, String> {
        Ok(match v {
            Value::Object(o) => {
                let mut out = Map::new();
                for (k, x) in o {
                    out.insert(k.clone(), self.resolve(x)?);
                }
                Value::Object(out)
            }
            Value::Array(a) => Value::Array(a.iter().map(|x| self.resolve(x)).collect::<Result<_, _>>()?),
            Value::String(s) if s.starts_with('$') => {
                let head = s.split('/').next().unwrap();
                if ["$ROOT", "$HOME", "$TMP"].contains(&head) {
                    return Ok(json!(self.places.path(s)));
                }
                let parts: Vec<&str> = s[1..].split('.').collect();
                if let Some(saved) = self.saved.get(parts[0]) {
                    let mut cur = saved;
                    for p in &parts[1..] {
                        let next = match cur {
                            Value::Array(a) => p.parse::<usize>().ok().and_then(|i| a.get(i)),
                            Value::Object(o) => o.get(*p),
                            _ => None,
                        };
                        cur = next.ok_or_else(|| format!("{s}: not in the saved result"))?;
                    }
                    return Ok(cur.clone());
                }
                v.clone()
            }
            other => other.clone(),
        })
    }

    async fn resolve_ids(&mut self, on: &str, ids: BTreeSet<i64>, scan: &str) {
        if scan.is_empty() {
            return;
        }
        for nid in ids {
            if self.norm.nodes.contains_key(&(scan.to_owned(), nid)) {
                continue;
            }
            let c = self.clients.get_mut(on).unwrap();
            if let Ok((_, r)) = c
                .call("tree.node", &json!({"scan_id": scan, "node": nid}), 120.0)
                .await
                && let Some(p) = r.get("result").and_then(|x| x.get("path")).and_then(Value::as_str)
            {
                let p = self.norm.path_text(p);
                self.norm.nodes.insert((scan.to_owned(), nid), p);
            }
        }
    }

    fn scan_of(&self, params: &Value, result: &Value) -> String {
        if let Some(s) = params.get("scan_id").and_then(Value::as_str) {
            return s.to_owned();
        }
        if let Some(v) = params.get("view_id").and_then(Value::as_str)
            && let Some(s) = self.view_scan.get(v)
        {
            return s.clone();
        }
        if let Some(s) = result.get("scan_id").and_then(Value::as_str) {
            return s.to_owned();
        }
        if let Some(s) = result
            .get("state")
            .and_then(|st| st.get("scan_id"))
            .and_then(Value::as_str)
            && result["state"].is_object()
        {
            return s.to_owned();
        }
        self.last_scan.clone()
    }

    fn note_ids(&mut self, params: &Value, result: &Value) {
        if let Some(s) = result.get("scan_id").and_then(Value::as_str) {
            self.last_scan = s.to_owned();
        }
        if let Some(st) = result.get("state").and_then(Value::as_object)
            && let (Some(v), Some(s)) = (
                st.get("view_id").and_then(Value::as_str),
                st.get("scan_id").and_then(Value::as_str),
            )
        {
            self.view_scan.insert(v.to_owned(), s.to_owned());
        }
        if let Some(s) = params.get("scan_id").and_then(Value::as_str) {
            self.last_scan = s.to_owned();
        }
    }

    async fn step(&mut self, step: &Value) -> Result<Option<Value>, String> {
        let on = step["on"].as_str().unwrap_or("main").to_owned();
        if let Some(method) = step["call"].as_str() {
            let params = self.resolve(step.get("params").unwrap_or(&json!({})))?;
            let c = self
                .clients
                .get_mut(&on)
                .ok_or_else(|| "no such session".to_owned())?;
            let (rid, r) = c.call(method, &params, secs(step, "timeout", 120.0)).await?;
            let mut rec = json!({"call": method});
            if let Some(result) = r.get("result") {
                if let Some(name) = step["as"].as_str() {
                    self.saved.insert(name.to_owned(), result.clone());
                }
                self.note_ids(&params, result);
                let scan = self.scan_of(&params, result);
                self.norm.scan = scan.clone();
                let mut frames = Vec::new();
                if step["binary"] == true {
                    // a frame follows when the picture changed; otherwise give a stray one a moment
                    let wait = if truthy(result.get("changed")) { 15.0 } else { 0.5 };
                    let key = rid.to_string();
                    let c = self.clients.get_mut(&on).unwrap();
                    let k2 = key.clone();
                    if c.wait(wait, |ib| ib.frames.get(&k2).is_some_and(|f| !f.is_empty())).await {
                        frames = c.inbox.lock().unwrap().frames.remove(&key).unwrap_or_default();
                    }
                }
                let decoded: Vec<Frame> = frames.iter().map(|b| decode(b)).collect();
                let mut ids = BTreeSet::new();
                self.norm.node_ids(result, &mut ids);
                self.resolve_ids(&on, ids, &scan).await;
                for f in &decoded {
                    let mut ids = BTreeSet::new();
                    if let Some((_, keys)) = f.columns.get("key") {
                        ids.extend(keys.iter().map(|k| k.as_f64().unwrap() as i64 & KEY_NODE_MASK as i64));
                    }
                    if let Some((_, nodes)) = f.columns.get("node") {
                        ids.extend(nodes.iter().map(|n| n.as_f64().unwrap() as i64).filter(|&n| n >= 0));
                    }
                    self.resolve_ids(&on, ids, &scan).await;
                }
                self.norm.scan = scan;
                let shown = pick(step, result);
                rec["result"] = if step["shape"] == true {
                    shape(&shown)
                } else {
                    self.norm.norm(&shown, "")
                };
                if step["binary"] == true {
                    rec["frames"] = Value::Array(decoded.iter().map(|f| self.norm.frame(f)).collect());
                }
            } else {
                rec["error"] = self.norm.error(&r["error"]);
            }
            return Ok((step["record"] != false).then_some(rec));
        }
        if let Some(target) = step.get("wait_scan") {
            let scan_id = self.resolve(target)?;
            let scan_id = scan_id.as_str().unwrap_or("").to_owned();
            let c = self.clients.get_mut(&on).unwrap();
            let (_, r) = c
                .call("$/subscribe", &json!({"topic": "scan", "params": {"scan_id": scan_id}}), 120.0)
                .await?;
            let sub = r["result"]["sub"].as_str().unwrap_or("").to_owned();
            let want_snapshot = step["snapshot"] == true;
            let before = match step.get("newer_than") {
                Some(v) => Some(self.resolve(v)?),
                None => None,
            };
            let c = self.clients.get_mut(&on).unwrap();
            let mut seen: Vec<Value> = Vec::new();
            let done = c
                .wait(secs(step, "timeout", 120.0), |ib| {
                    let q = ib.events.entry("scan.progress".into()).or_default();
                    while let Some(ev) = q.pop_front() {
                        if ev["sub"] == sub.as_str() {
                            seen.push(ev["status"].clone());
                        }
                    }
                    seen.last().is_some_and(|s| {
                        s["state"] != "scanning"
                            && s["state"] != "queued"
                            && (!want_snapshot || !s["snapshot"].is_null())
                            && before.as_ref().is_none_or(|b| s.get("finished_at") != Some(b))
                    })
                })
                .await;
            if !done {
                return Err("the scan did not finish".into());
            }
            c.call("$/unsubscribe", &json!({"sub": sub}), 120.0).await?;
            let status = seen.last().unwrap().clone();
            if let Some(name) = step["as"].as_str() {
                self.saved.insert(name.to_owned(), status.clone());
            }
            let items: Vec<i64> = seen.iter().map(|s| s["items"].as_i64().unwrap_or(0)).collect();
            let monotonic = items.windows(2).all(|w| w[0] <= w[1]);
            self.norm.scan = scan_id.clone();
            self.last_scan = scan_id.clone();
            let mut ids = BTreeSet::new();
            self.norm.node_ids(&status, &mut ids);
            self.resolve_ids(&on, ids, &scan_id).await;
            self.norm.scan = scan_id;
            let shown = pick(step, &status);
            return Ok(Some(json!({"wait_scan": self.norm.norm(&shown, ""), "monotonic": monotonic})));
        }
        if let Some(u) = step.get("until") {
            let end = Instant::now() + Duration::from_secs_f64(secs(step, "timeout", 30.0));
            let method = u["call"].as_str().ok_or("until: no call")?.to_owned();
            let params = self.resolve(u.get("params").unwrap_or(&json!({})))?;
            let mut met = false;
            while Instant::now() < end && !met {
                let c = self.clients.get_mut(&on).unwrap();
                let (_, r) = c.call(&method, &params, 120.0).await?;
                let mut cur = r.get("result").cloned().unwrap_or(Value::Null);
                if let Some(field) = u["field"].as_str().filter(|f| !f.is_empty()) {
                    for part in field.split('.') {
                        cur = cur.get(part).cloned().unwrap_or(Value::Null);
                    }
                }
                met = if let Some(eq) = u.get("equals") {
                    &cur == eq
                } else if u["null"] == true {
                    cur.is_null()
                } else {
                    !cur.is_null()
                };
                if !met {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
            return Ok(Some(json!({"until": method, "met": met})));
        }
        if let Some(topic) = step["subscribe"].as_str() {
            let mut params = json!({"topic": topic, "params": self.resolve(step.get("params").unwrap_or(&json!({})))?});
            if let Some(rate) = step.get("rate_hz") {
                params["rate_hz"] = rate.clone();
            }
            let c = self.clients.get_mut(&on).unwrap();
            let (_, r) = c.call("$/subscribe", &params, 120.0).await?;
            let mut rec = json!({"subscribe": topic});
            if let Some(result) = r.get("result") {
                if let Some(name) = step["as"].as_str() {
                    self.saved.insert(name.to_owned(), result.clone());
                }
                rec["result"] = self.norm.norm(result, "");
            } else {
                rec["error"] = self.norm.error(&r["error"]);
            }
            return Ok(Some(rec));
        }
        if let Some(name) = step["events"].as_str() {
            let count = step["count"].as_u64().unwrap_or(1) as usize;
            let mut mode = step["mode"].as_str().unwrap_or("all").to_owned();
            let c = self.clients.get_mut(&on).unwrap();
            let t = secs(step, "timeout", 20.0);
            let got: Vec<Value>;
            let ok;
            if let Some(stop) = step.get("stop_at") {
                let field = stop["field"].as_str().unwrap().to_owned();
                let set = stop["in"].as_array().cloned().unwrap_or_default();
                let hit = |e: &Value| set.contains(e.get(&field).unwrap_or(&Value::Null));
                ok = c
                    .wait(t, |ib| ib.events.entry(name.into()).or_default().iter().any(hit))
                    .await;
                let mut ib = c.inbox.lock().unwrap();
                let q = ib.events.entry(name.into()).or_default();
                let mut taken = Vec::new();
                while let Some(e) = q.pop_front() {
                    let h = hit(&e);
                    taken.push(e);
                    if h {
                        break;
                    }
                }
                got = taken;
            } else {
                let dedupe = step["dedupe"] == true;
                ok = c
                    .wait(t, |ib| {
                        let last = ib.last_event.get(name).cloned();
                        let q = ib.events.entry(name.into()).or_default();
                        if dedupe {
                            // a state event sent again unchanged counts once
                            squash(q, last.as_ref());
                        }
                        q.len() >= count
                    })
                    .await;
                if let Some(s) = step.get("settle").and_then(Value::as_f64) {
                    // let late copies arrive (state events re-sent unchanged)
                    tokio::time::sleep(Duration::from_secs_f64(s)).await;
                }
                let mut ib = c.inbox.lock().unwrap();
                let q = ib.events.entry(name.into()).or_default();
                if mode == "latest" {
                    got = q.back().cloned().into_iter().collect();
                    q.clear();
                    mode = "all".into();
                } else {
                    let n = count.min(q.len());
                    got = q.drain(..n).collect();
                }
            }
            if let Some(last) = got.last() {
                c.inbox
                    .lock()
                    .unwrap()
                    .last_event
                    .insert(name.to_owned(), last.clone());
            }
            let body = match mode.as_str() {
                "shape" => Value::Array(got.iter().map(shape).collect()),
                "count" => json!(got.len()),
                _ => {
                    let mut all: Vec<Value> = got.iter().map(|e| self.norm.norm(&pick(step, e), "")).collect();
                    if mode == "last" {
                        all = all.split_off(all.len().saturating_sub(1));
                    }
                    Value::Array(all)
                }
            };
            let mut rec = json!({"events": name, "got": body});
            if !ok {
                rec["timeout"] = json!(true);
            }
            return Ok(Some(rec));
        }
        if let Some(name) = step["drain"].as_str() {
            if let Some(s) = step.get("settle").and_then(Value::as_f64) {
                tokio::time::sleep(Duration::from_secs_f64(s)).await;
            }
            let c = self.clients.get_mut(&on).unwrap();
            c.inbox.lock().unwrap().events.remove(name);
            return Ok(None);
        }
        if let Some(h) = step.get("handshake") {
            return Ok(Some(json!({"handshake": self.handshake(h).await})));
        }
        if let Some(name) = step["connect"].as_str() {
            let url = self.backend.as_ref().unwrap().ready["url"].as_str().unwrap().to_owned();
            let caps = step["capabilities"]
                .as_array()
                .map(|a| a.iter().map(|x| x.as_str().unwrap().to_owned()).collect());
            let c = Client::open(&url, caps).await?;
            self.clients.insert(name.to_owned(), c);
            return Ok(None);
        }
        if let Some(op) = step.get("fs") {
            self.fs(op)?;
            return Ok(None);
        }
        if let Some(file) = step["read"].as_str() {
            let p = PathBuf::from(self.places.path(file));
            let Ok(bytes) = std::fs::read(&p) else {
                return Ok(Some(json!({"read": null})));
            };
            let text = String::from_utf8_lossy(&bytes).replace("\r\n", "\n");
            let lines: Vec<Value> = text
                .split('\n')
                .map(|l| json!(inline_time().replace_all(&self.norm.path_text(l), "<time>")))
                .collect();
            return Ok(Some(json!({"read": lines})));
        }
        if let Some(s) = step.get("sleep").and_then(Value::as_f64) {
            tokio::time::sleep(Duration::from_secs_f64(s)).await;
            return Ok(None);
        }
        if step.get("restart").is_some() {
            self.shutdown().await;
            self.boot().await;
            return Ok(None);
        }
        if step.get("exit").is_some() {
            let gone = self
                .backend
                .as_mut()
                .unwrap()
                .exited(secs(step, "timeout", 20.0))
                .await;
            for (_, c) in self.clients.drain() {
                c.close();
            }
            return Ok(Some(json!({"exit": gone})));
        }
        if step.get("ready").is_some() {
            let mut r = self.backend.as_ref().unwrap().ready.clone();
            r.as_object_mut().unwrap().remove("url");
            r.as_object_mut().unwrap().remove("port");
            return Ok(Some(json!({"ready": self.norm.norm(&r, "")})));
        }
        if step.get("hello").is_some() {
            let h = self.clients[&on].hello.clone();
            return Ok(Some(json!({"hello": self.norm.norm(&h, "")})));
        }
        if step.get("note").is_some() {
            return Ok(None);
        }
        Err(format!("unknown step {step}"))
    }

    fn fs(&self, op: &Value) -> Result<(), String> {
        let p = op["path"].as_str().map(|s| PathBuf::from(self.places.path(s)));
        let t = op["mtime"].as_f64().unwrap_or(1_600_000_000.0);
        match op["op"].as_str().unwrap_or("") {
            "write" => {
                let p = p.unwrap();
                std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
                std::fs::write(&p, vec![b'x'; op["size"].as_u64().unwrap_or(0) as usize])
                    .map_err(|e| e.to_string())?;
                set_times(&p, Some(t), t, t);
            }
            "touch" => set_times(&p.unwrap(), Some(t), t, t),
            "mkdir" => std::fs::create_dir_all(p.unwrap()).map_err(|e| e.to_string())?,
            "remove" => {
                let p = p.unwrap();
                if p.is_dir() {
                    remove_all(&p);
                } else {
                    std::fs::remove_file(&p).map_err(|e| e.to_string())?;
                }
            }
            "rename" => std::fs::rename(
                p.unwrap(),
                self.places.path(op["to"].as_str().unwrap()),
            )
            .map_err(|e| e.to_string())?,
            "fixture" => build_fixture(&p.unwrap(), op["tree"].as_str().unwrap()),
            other => return Err(format!("unknown fs op {other}")),
        }
        Ok(())
    }

    /// A raw connection: what the server answers and how it closes.
    async fn handshake(&mut self, h: &Value) -> Value {
        let mut url = self.backend.as_ref().unwrap().ready["url"].as_str().unwrap().to_owned();
        if let Some(path) = h["path"].as_str() {
            url = format!("{}{path}", &url[..url.rfind('/').unwrap()]);
        }
        let mut req = url.as_str().into_client_request().unwrap();
        if h["subprotocol"] != false {
            req.headers_mut().insert(
                "Sec-WebSocket-Protocol",
                HeaderValue::from_static(SUBPROTOCOL),
            );
        }
        if let Some(o) = h["origin"].as_str() {
            req.headers_mut().insert("Origin", HeaderValue::from_str(o).unwrap());
        }
        let ws = match tokio::time::timeout(Duration::from_secs(10), tokio_tungstenite::connect_async(req)).await {
            Ok(Ok((ws, _))) => ws,
            Ok(Err(tokio_tungstenite::tungstenite::Error::Http(r))) => {
                return json!({"http_status": r.status().as_u16()});
            }
            Ok(Err(e)) => return json!({"refused": format!("{e:?}").split('(').next().unwrap_or("").to_owned()}),
            Err(_) => return json!({"refused": "TimeoutError"}),
        };
        let (mut sink, mut stream) = ws.split();
        let mut out = Map::new();
        match h["send"].as_str().unwrap_or("hello") {
            "hello" => {
                let caps: Vec<String> = h["capabilities"]
                    .as_array()
                    .map(|a| a.iter().map(|x| x.as_str().unwrap().to_owned()).collect())
                    .unwrap_or_else(all_caps);
                let mut frame = hello_frame(
                    h["token"].as_str().unwrap_or(TOKEN),
                    h["protocol"].as_str().unwrap_or("1.1"),
                    &caps,
                );
                if let Some(p) = h.get("params") {
                    frame["params"] = p.clone();
                }
                let _ = sink.send(Message::text(frame.to_string())).await;
            }
            "binary" => {
                let _ = sink.send(Message::binary(vec![0u8, 1, 2, 3])).await;
            }
            "request" => {
                let f = json!({"jsonrpc": "2.0", "id": 1, "method": "system.ping", "params": {"payload": "x"}});
                let _ = sink.send(Message::text(f.to_string())).await;
            }
            "garbage" => {
                let _ = sink.send(Message::text("{not json")).await;
            }
            _ => {}
        }
        let end = tokio::time::Instant::now() + Duration::from_secs_f64(secs(h, "timeout", 15.0));
        loop {
            match tokio::time::timeout_at(end, stream.next()).await {
                Err(_) => {
                    out.insert("timeout".into(), json!(true));
                    break;
                }
                Ok(Some(Ok(Message::Text(t)))) => {
                    let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
                    if let Some(e) = v.get("error") {
                        out.insert("error".into(), self.norm.error(e));
                    } else if let Some(r) = v.get("result") {
                        out.insert(
                            "result".into(),
                            json!({"protocol": r["protocol"], "capabilities": r["capabilities"]}),
                        );
                        break;
                    }
                }
                Ok(Some(Ok(Message::Close(c)))) => {
                    out.insert("close_code".into(), c.map_or(Value::Null, |c| json!(u16::from(c.code))));
                    break;
                }
                Ok(Some(Ok(_))) => {}
                Ok(Some(Err(_)) | None) => {
                    out.insert("close_code".into(), Value::Null);
                    break;
                }
            }
        }
        // close and wait for the server's side of it (as websockets' close() does): the session
        // is gone before the next step counts sessions
        let _ = sink.send(Message::Close(None)).await;
        let _ = tokio::time::timeout(Duration::from_secs(3), async {
            while let Some(Ok(_)) = stream.next().await {}
        })
        .await;
        Value::Object(out)
    }
}

/// The release list on 127.0.0.1 (the backend checks for updates on its own).
async fn start_feed() -> String {
    let feed = read_json(&parity_dir().join("fixtures/feed.json"));
    let body = serde_json::to_vec(&feed["releases"]).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = listener.accept().await {
            let body = body.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 1024];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    match s.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                }
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nETag: \"parity-1\"\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = s.write_all(head.as_bytes()).await;
                let _ = s.write_all(&body).await;
                let _ = s.shutdown().await;
            });
        }
    });
    format!("http://127.0.0.1:{port}/repos/DuskXi/SpaceKnife/releases")
}

/// The data folder 0.3.4 wrote (golden/<platform>/<name>.home), placeholders filled in.
fn apply_home_template(src: &Path, places: &Places, feed: &str) {
    let manifest = read_json(&src.join("manifest.json"));
    let keys: Vec<String> = manifest["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|roots| {
            let real: Vec<String> = roots
                .as_array()
                .unwrap()
                .iter()
                .map(|r| places.path(r.as_str().unwrap()))
                .collect();
            root_key_034(&real)
        })
        .collect();
    let mut values = places.mapping();
    values.push(("$FEED".into(), feed.to_owned()));
    for f in manifest["files"].as_array().unwrap() {
        let mut text = std::fs::read_to_string(src.join(f["data"].as_str().unwrap())).unwrap();
        let mut rel_path = f["path"].as_str().unwrap().to_owned();
        for (i, k) in keys.iter().enumerate() {
            text = text.replace(&format!("{{key:{i}}}"), k);
            rel_path = rel_path.replace(&format!("{{key:{i}}}"), k);
        }
        for (name, real) in &values {
            let escaped = serde_json::to_string(real).unwrap();
            text = text
                .replace(&format!("{{json:{name}}}"), &escaped[1..escaped.len() - 1])
                .replace(&format!("{{raw:{name}}}"), real);
        }
        let target = rel(&places.home, &rel_path);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(target, text).unwrap();
    }
}

/// Run one scenario against the golden of this platform; the unexpected differences.
async fn replay(name: &str) -> Option<Vec<(String, Value, Value)>> {
    let dir = parity_dir();
    let sc = read_json(&dir.join("scenarios").join(format!("{name}.json")));
    let golden_path = dir.join("golden").join(platform()).join(format!("{name}.json"));
    let runs_here = sc["platforms"]
        .as_array()
        .is_none_or(|a| a.iter().any(|p| p == platform()));
    if !runs_here || !golden_path.exists() {
        eprintln!("{name}: no golden for {} — nothing to compare", platform());
        return None;
    }
    let golden = read_json(&golden_path);
    let rules = load_rules(sc.get("normalize"));
    let feed = start_feed().await;
    let places = Places::make();
    for (fname, tree) in sc["fixtures"].as_object().cloned().unwrap_or_default() {
        build_fixture(&places.root.join(&fname), tree.as_str().unwrap());
    }
    let upgrade = sc.get("setup").is_some();
    if upgrade {
        apply_home_template(&dir.join("golden").join(platform()).join(format!("{name}.home")), &places, &feed);
    }
    let mapping = places.mapping();
    let mut run = Run {
        sc: &sc,
        places: &places,
        feed,
        norm: Normalizer::new(&rules, &mapping),
        saved: HashMap::new(),
        last_scan: String::new(),
        view_scan: HashMap::new(),
        backend: None,
        clients: HashMap::new(),
    };
    run.boot().await;
    let mut transcript = Vec::new();
    let mut live_only = BTreeSet::new();
    for (i, step) in sc["steps"].as_array().unwrap().iter().enumerate() {
        if step["live_only"] == true {
            live_only.insert(i as i64);
            continue; // needs what only a live 0.3.4 writes (its format-2 snapshots)
        }
        let rec = match run.step(step).await {
            Ok(r) => r,
            Err(e) => Some(json!({"step_error": e})),
        };
        if let Some(mut rec) = rec {
            let mut m = Map::new();
            m.insert("i".into(), json!(i));
            m.append(rec.as_object_mut().unwrap());
            transcript.push(Value::Object(m));
        }
    }
    run.shutdown().await;
    let expected: Vec<Value> = golden["steps"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| !live_only.contains(&int_of(&r["i"])))
        .cloned()
        .collect();
    let diffs = compare(&keyed(&expected), &keyed(&transcript), &rules);
    let mut known: Vec<Value> = read_json(&dir.join("known_diffs.json"))["known"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    known.extend(sc["known_diffs"].as_array().cloned().unwrap_or_default());
    let bad: Vec<_> = diffs
        .into_iter()
        .filter(|(p, _, _)| !known.iter().any(|k| glob_match(k["path"].as_str().unwrap(), p)))
        .collect();
    if bad.is_empty() {
        remove_all(&places.tmp);
    } else {
        // kept for a look: the backend's log is there
        eprintln!("{name}: kept {}", places.tmp.display());
    }
    Some(bad)
}

fn report(name: &str, bad: &[(String, Value, Value)]) -> String {
    let mut s = format!("{name}: {} differences from the 0.3.4 golden\n", bad.len());
    for (p, e, a) in bad.iter().take(40) {
        let cut = |v: &Value| {
            let t = v.to_string();
            t.chars().take(200).collect::<String>()
        };
        s.push_str(&format!("  {p}: expected {}  actual {}\n", cut(e), cut(a)));
    }
    s
}

fn check(name: &str) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    if let Some(bad) = rt.block_on(replay(name)) {
        assert!(bad.is_empty(), "{}", report(name, &bad));
    }
}

macro_rules! scenarios {
    ($($name:ident),* $(,)?) => {
        /// One test per scenario file (`cargo test -p sk-server --test parity_replay scenario::views`).
        mod scenario {
            $(
                #[test]
                fn $name() {
                    super::check(stringify!($name));
                }
            )*
        }
        const LISTED: &[&str] = &[$(stringify!($name)),*];
    };
}

scenarios!(
    basket_ops,
    compare,
    filter_oracle,
    handshake,
    invalid_params,
    monitor,
    palette,
    scan_tree,
    settings_classes,
    system,
    views,
);

/// 0.3.4 writes, 0.3.5 reads back: tags, palette jumps and snapshots are lost until the data
/// migration (plan 5.5) restores 0.3.4's SHA-1 root keys and reads format-2 snapshots.
#[test]
#[ignore = "bug: 0.3.4 data is not migrated yet (plan 5.5; review #4 root key; format-2 snapshots)"]
fn upgrade() {
    check("upgrade");
}

#[test]
fn every_scenario_has_a_test() {
    let mut on_disk: Vec<String> = std::fs::read_dir(parity_dir().join("scenarios"))
        .unwrap()
        .filter_map(|e| {
            let p = e.ok()?.path();
            (p.extension()? == "json").then(|| p.file_stem().unwrap().to_string_lossy().into_owned())
        })
        .collect();
    on_disk.sort();
    let mut listed: Vec<String> = LISTED.iter().map(|s| (*s).to_owned()).collect();
    listed.push("upgrade".into());
    listed.sort();
    assert_eq!(on_disk, listed, "list each scenario in parity_replay.rs");
}

#[test]
fn normalizer_helpers_follow_the_python_rules() {
    // norm.py's glob_match, STAMP, shape; hashlib.sha1
    assert!(glob_match("**/error/loc/**", "steps/3/error/loc/#len"));
    assert!(glob_match("**/error/loc/**", "steps/3/handshake/error/loc"));
    assert!(glob_match("steps/*/wait_scan/monitor", "steps/12/wait_scan/monitor"));
    assert!(!glob_match("steps/*/wait_scan/monitor", "steps/12/wait_scan/io"));
    assert_eq!(replace_stamps("a/20261108-030158.sks"), "a/<stamp>.sks");
    assert_eq!(replace_stamps("120261108-030158"), "120261108-030158");
    assert_eq!(
        sha1(b"abc").iter().map(|b| format!("{b:02x}")).collect::<String>(),
        "a9993e364706816aba3e25717850c26c9cd0d89d"
    );
    // what 0.3.4's snapshot.root_key gives (Python, Windows)
    #[cfg(windows)]
    assert_eq!(
        root_key_034(&[r"C:\Temp\Sk-Parity\root\tree".to_owned()]),
        "tree-533b17933f71"
    );
    assert_eq!(shape(&json!({"a": [1, {"b": null, "c": "x"}], "d": null})), json!({"a": ["mixed"]}));
    assert_eq!(
        shape(&json!([{"a": 1}, {"b": true}])),
        json!([{"a": "number", "b": "bool"}])
    );
}
