//! What SpaceKnife 0.3.4 (the Python backend) left on disk reads back: its snapshots (format 2,
//! `SKSNAP\0\x01`) and the keys of its snapshot folders and tag files (`root_key`).
//!
//! The fixtures are real 0.3.4 output (`crates/sk-core/tests/data/v034`, made by
//! `tools/fixtures/v034_fixtures.py` with the v0.3.4 backend on Windows and on Linux); each
//! `expected/<name>.json` is what 0.3.4 itself reads back from `snapshots/<name>.sks`.

use std::path::{Path, PathBuf};

use serde_json::Value;
use sk_store::snapshot::{self, SnapshotError};
use sk_store::{Col, Flags};

fn data() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../sk-core/tests/data/v034")
}

/// The fixtures made on this kind of system (keys depend on Windows' or POSIX's path rules).
fn native() -> PathBuf {
    data().join(if cfg!(windows) { "windows" } else { "posix" })
}

fn json(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

/// Python's `surrogatepass` UTF-8 as text: each lone surrogate (3 bytes `ED A0..BF xx`) one U+FFFD,
/// as a path with one shows.
fn surrogates_lossy(raw: &[u8]) -> String {
    let mut fixed = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == 0xED && raw.get(i + 1).is_some_and(|b| (0xA0..=0xBF).contains(b)) {
            fixed.extend_from_slice("\u{fffd}".as_bytes());
            i += 3;
        } else {
            fixed.push(raw[i]);
            i += 1;
        }
    }
    String::from_utf8(fixed).unwrap()
}

/// Unix seconds of an ISO 8601 time with an offset (`2026-11-08T02:46:06.123456+00:00`).
fn unix(iso: &str) -> u64 {
    let (date, time) = iso.split_once('T').unwrap();
    let d: Vec<i64> = date.split('-').map(|x| x.parse().unwrap()).collect();
    let (clock, offset) = time.split_at(time.find(['+', '-', 'Z']).unwrap());
    let hms: Vec<f64> = clock.split(':').map(|x| x.parse().unwrap()).collect();
    assert!(matches!(offset, "Z" | "+00:00"), "{iso}");
    let (y, m) = if d[1] <= 2 {
        (d[0] - 1, d[1] + 9)
    } else {
        (d[0], d[1] - 3)
    };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + d[2] - 1;
    let days = era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468;
    (days * 86400) as u64 + (hms[0] * 3600.0 + hms[1] * 60.0 + hms[2]).floor() as u64
}

fn column_values(nodes: &sk_store::Nodes, c: Col) -> Vec<i128> {
    let raw = nodes.column_bytes(c, 0..nodes.len());
    let item = c.item();
    raw.chunks(item)
        .map(|b| match (c.dtype(), item) {
            ("u1", 1) => b[0] as i128,
            ("u2", 2) => u16::from_le_bytes(b.try_into().unwrap()) as i128,
            ("u4", 4) => u32::from_le_bytes(b.try_into().unwrap()) as i128,
            ("i4", 4) => i32::from_le_bytes(b.try_into().unwrap()) as i128,
            ("i8", 8) => i64::from_le_bytes(b.try_into().unwrap()) as i128,
            ("u8", 8) => u64::from_le_bytes(b.try_into().unwrap()) as i128,
            other => panic!("{other:?}"),
        })
        .collect()
}

/// Every snapshot of both fixture sets (Windows names are WTF-8, POSIX ones raw bytes: both
/// load anywhere) reads back exactly what 0.3.4 reads back.
#[test]
fn snapshots_of_0_3_4_load_with_every_column_and_name() {
    let mut seen = 0;
    for platform in ["windows", "posix"] {
        let dir = data().join(platform);
        for entry in std::fs::read_dir(dir.join("expected")).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_stem().unwrap().to_str().unwrap().to_owned();
            let want = json(&path);
            let file = dir.join("snapshots").join(format!("{name}.sks"));
            let header = snapshot::read_header(&file)
                .unwrap_or_else(|e| panic!("{platform}/{name}: read_header: {e}"));
            let (store, loaded) =
                snapshot::load(&file).unwrap_or_else(|e| panic!("{platform}/{name}: load: {e}"));
            assert_eq!(header, loaded, "{platform}/{name}");
            let what = format!("{platform}/{name}");
            assert_eq!(loaded.format, 2, "{what}");
            assert_eq!(
                loaded.node_count,
                want["node_count"].as_u64().unwrap(),
                "{what}"
            );
            let roots: Vec<String> = serde_json::from_value(want["roots"].clone()).unwrap();
            assert_eq!(loaded.roots, roots, "{what}");
            assert_eq!(
                loaded.created_at,
                unix(want["created_at"].as_str().unwrap()),
                "{what}"
            );
            assert_eq!(
                loaded.app_version,
                want["app_version"].as_str().unwrap(),
                "{what}"
            );
            assert_eq!(Value::Object(loaded.stats.clone()), want["stats"], "{what}");
            assert_eq!(
                Value::Array(loaded.volumes.clone()),
                want["volumes"],
                "{what}"
            );
            let nodes = store.read();
            assert_eq!(nodes.len() as u64, loaded.node_count, "{what}");
            let ext: Vec<String> = want["ext_table_hex"]
                .as_array()
                .unwrap()
                .iter()
                .map(|h| surrogates_lossy(&unhex(h.as_str().unwrap())))
                .collect();
            let got: Vec<String> = nodes.ext_table().iter().map(|e| e.to_string()).collect();
            assert_eq!(
                got, ext,
                "{what}: the extension table (lone surrogates as U+FFFD)"
            );
            for (col, values) in want["columns"].as_object().unwrap() {
                let c = Col::from_name(col).unwrap();
                let want: Vec<i128> = values
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| {
                        v.as_i64()
                            .map(i128::from)
                            .unwrap_or_else(|| v.as_u64().unwrap() as i128)
                    })
                    .collect();
                assert_eq!(column_values(&nodes, c), want, "{what}: column {col}");
            }
            for (i, h) in want["names_hex"].as_array().unwrap().iter().enumerate() {
                assert_eq!(
                    nodes.name_bytes(i as u32),
                    unhex(h.as_str().unwrap()).as_slice(),
                    "{what}: name of node {i}"
                );
            }
            // paths work (the parents form a tree): every node's path starts with its root's
            for i in 1..nodes.len() as u32 {
                if !nodes.flags(i).intersects(Flags::GONE) {
                    assert!(
                        nodes.path_bytes(i).starts_with(b"C:\\")
                            || nodes.path_bytes(i).starts_with(b"/")
                    );
                }
            }
            seen += 1;
        }
    }
    assert_eq!(seen, 15, "7 Windows and 8 POSIX snapshots");
}

/// The keys 0.3.4 gave to a list of tricky roots (drives, UNC, `\\?\`, `..`, case, letters that
/// lower-case differently, marks and symbols in the label).
#[test]
fn root_keys_are_those_of_0_3_4() {
    let cases = json(&native().join("root_keys.json"));
    let mut wrong = Vec::new();
    for case in cases.as_array().unwrap() {
        let roots: Vec<String> = serde_json::from_value(case["roots"].clone()).unwrap();
        let want = case["key"].as_str().unwrap();
        let got = snapshot::root_key(&roots);
        if got != want {
            wrong.push(format!("{roots:?}: {got} (0.3.4: {want})"));
        }
    }
    assert!(
        wrong.is_empty(),
        "{} keys differ:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}

/// 0.3.4's auto-save folders are found under the same names (and every file in them listed).
#[test]
fn the_auto_save_folders_of_0_3_4_are_found() {
    let facts = json(&native().join("facts.json"));
    let base = native().join("home/data/snapshots");
    for (name, auto) in facts["auto"].as_object().unwrap() {
        let roots: Vec<String> = serde_json::from_value(auto["roots"].clone()).unwrap();
        let key = snapshot::root_key(&roots);
        assert_eq!(key, auto["key"].as_str().unwrap(), "{name}");
        assert!(
            base.join(&key)
                .join(auto["file"].as_str().unwrap())
                .is_file(),
            "{name}"
        );
    }
    let listed = snapshot::list_snapshots(&base);
    let files = std::fs::read_dir(&base)
        .unwrap()
        .flat_map(|d| std::fs::read_dir(d.unwrap().path()).unwrap())
        .count();
    assert_eq!(listed.len(), files, "every 0.3.4 snapshot is listed");
    assert!(
        listed
            .windows(2)
            .all(|w| w[0].1.created_at >= w[1].1.created_at)
    );
}

/// Retention never deletes a file it cannot read (a damaged one, a newer format): only the
/// snapshots it can read count and go.
#[test]
fn prune_never_deletes_what_it_cannot_read() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("plain-key");
    std::fs::create_dir_all(&folder).unwrap();
    let plain = data().join("posix/snapshots/plain.sks");
    // twelve readable snapshots in one week, the oldest two of them to go with keep_recent 10
    let start = snapshot::parse_stamp("20261019-090000").unwrap();
    for k in 0..12u64 {
        std::fs::copy(
            &plain,
            folder.join(format!("{}.sks", snapshot::stamp(start + 3600 * k))),
        )
        .unwrap();
    }
    // older still, same week: not snapshots this build can read
    let damaged = folder.join(format!("{}.sks", snapshot::stamp(start - 7200)));
    std::fs::write(&damaged, b"SKSNAP\0\x01 not really").unwrap();
    let future = folder.join(format!("{}.sks", snapshot::stamp(start - 3600)));
    std::fs::write(&future, b"SKSNAP\0\x09\0\0\0\0").unwrap();
    // and the newest file of all is damaged too: it takes none of the ten places
    let newest = folder.join(format!("{}.sks", snapshot::stamp(start + 3600 * 20)));
    std::fs::write(&newest, b"").unwrap();
    let deleted = snapshot::prune(&folder, 10).unwrap();
    assert!(
        damaged.exists() && future.exists() && newest.exists(),
        "unreadable files are kept"
    );
    let gone: Vec<String> = deleted
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    // the ten newest readable ones stay, and the newest of the older ones (its week's)
    assert_eq!(gone, [format!("{}.sks", snapshot::stamp(start))]);
}

/// A format-2 header that names a format this build does not know is refused by its number.
#[test]
fn unknown_formats_are_named() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("x.sks");
    std::fs::write(&f, b"SKSNAP\0\x09\0\0\0\0").unwrap();
    assert!(matches!(
        snapshot::read_header(&f),
        Err(SnapshotError::Format(9))
    ));
    let head = br#"{"format":7,"created_at":"2026-01-01T00:00:00Z","roots":[],"node_count":1,"ext_table":[],"columns":{},"names":{"offset":0,"length":0,"raw_length":0}}"#;
    let mut file = b"SKSNAP\0\x01".to_vec();
    file.extend_from_slice(&(head.len() as u32).to_le_bytes());
    file.extend_from_slice(head);
    std::fs::write(&f, file).unwrap();
    assert!(matches!(
        snapshot::read_header(&f),
        Err(SnapshotError::Format(7))
    ));
}
