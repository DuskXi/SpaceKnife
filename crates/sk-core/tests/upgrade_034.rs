//! What SpaceKnife 0.3.4 (the Python backend) left in its folders reads back, and what this
//! build cannot read of it is never thrown away.
//!
//! The data files are real 0.3.4 output (`tests/data/v034/<platform>/home`, a SPACEKNIFE_HOME
//! made by `tools/fixtures/v034_fixtures.py` with the v0.3.4 backend; `classes_v1.json` by
//! 0.3.3's own `classes.save`). `facts.json` says what was put in.

use std::path::{Path, PathBuf};

use serde_json::Value;
use sk_core::{classes, pathclass};
use sk_protocol::classes::ClassEdit;

fn data() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/v034")
}

/// The fixtures made on this kind of system (keys and paths follow its rules).
fn native() -> PathBuf {
    data().join(if cfg!(windows) { "windows" } else { "posix" })
}

fn facts() -> Value {
    serde_json::from_slice(&std::fs::read(native().join("facts.json")).unwrap()).unwrap()
}

/// A fixture file in a temporary folder of its own (the readers may write next to it).
fn copied(rel: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let to = dir.path().join(Path::new(rel).file_name().unwrap());
    std::fs::copy(native().join(rel), &to).unwrap();
    (dir, to)
}

#[test]
fn settings_of_0_3_4_read_back() {
    let (_dir, file) = copied("home/config/settings.json");
    let store = sk_core::settings::SettingsStore::new(Some(file));
    let facts = facts();
    for (key, want) in facts["settings"].as_object().unwrap() {
        let got = serde_json::to_value(store.get(key).unwrap()).unwrap();
        assert_eq!(&got, want, "{key}");
    }
}

/// The classes 0.3.4 saved (version 2): every class as the file has it, its own (untranslated)
/// names included.
fn saved_classes() -> Value {
    let text = std::fs::read(native().join("home/config/classes.json")).unwrap();
    let doc: Value = serde_json::from_slice(&text).unwrap();
    assert_eq!(doc["version"], 2);
    doc["classes"].clone()
}

#[test]
fn classes_json_of_0_3_4_reads_back() {
    let (_dir, file) = copied("home/config/classes.json");
    let table = classes::load(&file);
    assert!(table.custom());
    assert_eq!(
        serde_json::to_value(table.classes()).unwrap(),
        saved_classes()
    );
    // what the editor sent then (shown in English) is what the table shows now
    let ids: Vec<String> = table.classes().iter().map(|c| c.id.clone()).collect();
    let sent: Vec<String> = facts()["classes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(ids, sent);
}

/// Version 1 (0.3.3 and older, one colour for both themes): the colour is the light one too.
#[test]
fn classes_json_version_1_of_0_3_3_reads_back() {
    let (_dir, file) = copied("classes_v1.json");
    let table = classes::load(&file);
    let mut want = facts()["classes_v1"]["classes"].clone();
    for c in want.as_array_mut().unwrap() {
        c["color_light"] = c["color"].clone();
    }
    assert_eq!(serde_json::to_value(table.classes()).unwrap(), want);
}

/// Python read a `version` it could make an integer of (`int("2")`); so does this.
#[test]
fn a_classes_version_python_read_is_read() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("classes.json");
    let text = std::fs::read_to_string(native().join("home/config/classes.json")).unwrap();
    let mut doc: Value = serde_json::from_str(&text).unwrap();
    doc["version"] = "2".into();
    std::fs::write(&file, serde_json::to_vec(&doc).unwrap()).unwrap();
    let table = classes::load(&file);
    assert_eq!(
        serde_json::to_value(table.classes()).unwrap(),
        saved_classes()
    );
}

/// A classes.json this build cannot read (damaged, or valid to 0.3.4 but not here) falls back to
/// the built-in table, and is kept: the next save (an edit, a reset) moves it aside, never over
/// it.
#[test]
fn a_classes_json_that_cannot_be_read_is_never_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("classes.json");
    for (content, then) in [("{ broken", "edit"), ("{\"classes\": 5}", "reset")] {
        std::fs::write(&file, content).unwrap();
        let table = classes::load(&file);
        assert!(!table.custom(), "the built-in table stands in");
        if then == "edit" {
            let mut edits: Vec<ClassEdit> = classes::ClassTable::builtin()
                .classes()
                .iter()
                .map(|c| ClassEdit {
                    id: Some(c.id.clone()),
                    name: c.name.clone(),
                    extensions: c.extensions.clone(),
                    color: c.color.clone(),
                    color_light: c.color_light.clone(),
                })
                .collect();
            edits[0].name = "Renamed".into();
            classes::save(&classes::build(&edits).unwrap(), &file).unwrap();
            assert!(classes::load(&file).custom(), "the edit is saved");
        } else {
            classes::save(&classes::ClassTable::builtin(), &file).unwrap();
            assert!(!file.exists());
        }
        let kept: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p != &file)
            .map(|p| std::fs::read_to_string(p).unwrap())
            .collect();
        assert!(kept.contains(&content.to_owned()), "{then}: {kept:?}");
        for e in std::fs::read_dir(dir.path()).unwrap() {
            std::fs::remove_file(e.unwrap().path()).unwrap();
        }
    }
}

#[test]
fn class_folders_of_0_3_4_read_back() {
    let (_dir, file) = copied("home/config/class_folders.json");
    let mut got = pathclass::load(&file);
    got.sort();
    let mut want: Vec<(String, String)> =
        serde_json::from_value(facts()["folders"].clone()).unwrap();
    want.sort();
    assert_eq!(got, want);
}

/// One bad entry costs that entry, not the file (Python read a number as its text; an entry
/// without a path or class it dropped the file for).
#[test]
fn class_folders_skip_bad_entries_one_by_one() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("class_folders.json");
    let text = r#"{"version": 1, "folders": [
        {"path": "/home/lenny/games", "class": "game"},
        {"path": 5, "class": "video"},
        {"path": "/home/lenny/x"},
        {"path": "/home/lenny/y", "class": null},
        "not an entry",
        {"path": "/home/lenny/films", "class": "video", "extra": true}
    ]}"#;
    std::fs::write(&file, text).unwrap();
    assert_eq!(
        pathclass::load(&file),
        [
            ("/home/lenny/games".to_owned(), "game".to_owned()),
            ("/home/lenny/films".to_owned(), "video".to_owned())
        ]
    );
}

/// A class_folders.json that cannot be read gives no choices, and the next save keeps it aside.
#[test]
fn a_class_folders_json_that_cannot_be_read_is_never_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("class_folders.json");
    std::fs::write(&file, "{\"folders\": [ broken").unwrap();
    assert!(pathclass::load(&file).is_empty());
    pathclass::replace_folders([("/home/lenny/games".to_owned(), "game".to_owned())]);
    pathclass::save(&file).unwrap();
    assert_eq!(pathclass::load(&file).len(), 1);
    let kept: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p != &file)
        .map(|p| std::fs::read_to_string(p).unwrap())
        .collect();
    assert_eq!(kept, ["{\"folders\": [ broken"]);
    pathclass::replace_folders(Vec::new());
}

#[test]
fn update_json_of_0_3_4_reads_back() {
    let (_dir, file) = copied("home/data/update.json");
    let state = sk_core::update::load_state(&file, sk_core::update::RELEASES_URL);
    let want = &facts()["update"];
    let at = chrono::DateTime::parse_from_rfc3339(want["checked_at"].as_str().unwrap()).unwrap();
    assert_eq!(state.checked_at.unwrap(), at);
    assert_eq!(state.etag.as_deref(), want["etag"].as_str());
    assert_eq!(state.kept as u64, want["kept"].as_u64().unwrap());
    let versions: Vec<&str> = state.releases.iter().map(|r| r.version.as_str()).collect();
    let wanted: Vec<&str> = want["releases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["version"].as_str().unwrap())
        .collect();
    assert_eq!(versions, wanted);
    assert_eq!(
        serde_json::to_value(&state.dismissed).unwrap(),
        want["dismissed"]
    );
    assert_eq!(serde_json::to_value(&state.muted).unwrap(), want["muted"]);
    assert_eq!(state.releases[0].assets.len(), 1);
}

/// The tag file is found by the key 0.3.4 named it with, and holds what was tagged.
#[test]
fn tags_of_0_3_4_are_found_and_read() {
    let facts = facts();
    let roots: Vec<String> = serde_json::from_value(facts["roots"]["plain"].clone()).unwrap();
    let key = sk_store::snapshot::root_key(&roots);
    let file = native().join("home/data/tags").join(format!("{key}.json"));
    assert!(file.is_file(), "{key}.json");
    let (_dir, file) = copied(&format!("home/data/tags/{key}.json"));
    let tags = sk_core::view::tags::TagStore::new(Some(file));
    let got: Vec<(String, String)> = tags
        .tags
        .iter()
        .map(|((root, path), color)| {
            assert_eq!(*root, 0);
            (path.clone(), color.as_str().to_owned())
        })
        .collect();
    let mut want: Vec<(String, String)> = facts["tags"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(p, c)| (p.clone(), c.as_str().unwrap().to_owned()))
        .collect();
    want.sort();
    assert_eq!(got, want);
}

/// The palette's jumps are kept by the key of their scan target: 0.3.4's are found.
#[test]
fn palette_recents_of_0_3_4_are_found() {
    let facts = facts();
    let roots: Vec<String> = serde_json::from_value(facts["roots"]["plain"].clone()).unwrap();
    let key = sk_store::snapshot::root_key(&roots);
    let (_dir, file) = copied("home/data/palette_recent.json");
    let recents = sk_core::palette::recents::Recents::open(file);
    let entries = recents.entries(Some(&key));
    let texts: Vec<&str> = entries.iter().map(|e| e.text.as_str()).collect();
    let mut want: Vec<String> = serde_json::from_value(facts["jumps"].clone()).unwrap();
    want.extend(serde_json::from_value::<Vec<String>>(facts["filters"].clone()).unwrap());
    // newest first: the filters were added last
    let mut sorted = want.clone();
    sorted.rotate_left(2);
    assert_eq!(texts, sorted, "jumps {:?}", facts["jumps"]);
}

/// 0.3.4's loguru rotated its log to `spaceknife.<time>.log` (10 MB, 5 kept); this build rotates
/// to `spaceknife.log.<n>`. The old files go once, the current file and anything else stay.
#[test]
fn the_rotated_logs_of_0_3_4_go() {
    let dir = tempfile::tempdir().unwrap();
    let logs = dir.path();
    let old = [
        "spaceknife.2026-10-26_09-00-00_123456.log",
        "spaceknife.2026-10-27_10-11-12_000001.log",
        "spaceknife.2026-10-27_10-11-12_000001.2.log",
    ];
    let keep = [
        "spaceknife.log",
        "spaceknife.log.1",
        "spaceknife.log.5",
        "spaceknife.notes.log",
        "other.2026-10-26_09-00-00_123456.log",
        "spaceknife.2026-10-26.log",
        "update-swap.log",
    ];
    for name in old.iter().chain(&keep) {
        std::fs::write(logs.join(name), b"x").unwrap();
    }
    let mut removed: Vec<String> = sk_core::log::remove_old_rotations(logs)
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    removed.sort();
    let mut want = old.map(str::to_owned).to_vec();
    want.sort();
    assert_eq!(removed, want);
    for name in keep {
        assert!(logs.join(name).exists(), "{name}");
    }
    assert!(sk_core::log::remove_old_rotations(logs).is_empty(), "once");
    assert!(sk_core::log::remove_old_rotations(&logs.join("missing")).is_empty());
}
