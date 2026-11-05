//! Palette recents (UI-W4-08): path jumps per scan target, filters shared by all.
//!
//! Kept newest first, without duplicates (running an entry again moves it to the front), in one
//! JSON file in the data directory. Writes go through a temporary file and an atomic rename.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sk_protocol::palette::RecentKind;

/// Entries kept per list.
pub const KEEP: usize = 50;

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub kind: RecentKind,
    pub text: String,
    /// unix seconds, strictly increasing
    pub at: f64,
}

#[derive(Default, Serialize, Deserialize)]
struct Item {
    text: String,
    at: f64,
}

#[derive(Default, Serialize, Deserialize)]
struct Payload {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    jumps: BTreeMap<String, Vec<Item>>,
    #[serde(default)]
    filters: Vec<Item>,
}

#[derive(Default)]
struct State {
    jumps: BTreeMap<String, Vec<Entry>>,
    filters: Vec<Entry>,
    last: f64,
}

/// The recents file. Thread-safe.
pub struct Recents {
    pub file: PathBuf,
    state: Mutex<State>,
}

/// The comparison key of a path (`normcase(normpath(path))`).
fn path_key(path: &str) -> Vec<u8> {
    let norm = sk_store::names::normpath(path.as_bytes());
    sk_store::names::normcase(&norm).into_owned()
}

impl Recents {
    /// The recents in `palette_recent.json` of the data directory.
    pub fn open_default() -> Recents {
        Recents::open(crate::paths::data_dir().join("palette_recent.json"))
    }

    /// The recents kept in `file` (a missing or broken file reads as empty).
    pub fn open(file: impl Into<PathBuf>) -> Recents {
        let file = file.into();
        let mut st = load(&file);
        st.last = st
            .filters
            .iter()
            .chain(st.jumps.values().flatten())
            .map(|e| e.at)
            .fold(0.0, f64::max);
        Recents {
            file,
            state: Mutex::new(st),
        }
    }

    pub fn add_jump(&self, target: &str, path: &str) -> std::io::Result<()> {
        let key = path_key(path);
        let mut st = self.state.lock();
        let at = now(&mut st);
        let items = st.jumps.remove(target).unwrap_or_default();
        let entry = Entry {
            kind: RecentKind::Jump,
            text: path.to_owned(),
            at,
        };
        st.jumps.insert(
            target.to_owned(),
            push(items, entry, |e| path_key(&e.text) != key),
        );
        save(&self.file, &st)
    }

    pub fn add_filter(&self, text: &str) -> std::io::Result<()> {
        let mut st = self.state.lock();
        let at = now(&mut st);
        let items = std::mem::take(&mut st.filters);
        st.filters = push(
            items,
            Entry {
                kind: RecentKind::Filter,
                text: text.to_owned(),
                at,
            },
            |e| e.text != text,
        );
        save(&self.file, &st)
    }

    /// Clear one scan target's jumps and the shared filters (UI-W4-11: the group disappears).
    pub fn clear(&self, target: Option<&str>) -> std::io::Result<()> {
        let mut st = self.state.lock();
        if let Some(t) = target {
            st.jumps.remove(t);
        }
        st.filters.clear();
        save(&self.file, &st)
    }

    /// Jumps of `target` and all filters, newest first.
    pub fn entries(&self, target: Option<&str>) -> Vec<Entry> {
        let st = self.state.lock();
        let mut out: Vec<Entry> = target
            .and_then(|t| st.jumps.get(t))
            .cloned()
            .unwrap_or_default();
        out.extend(st.filters.iter().cloned());
        out.sort_by(|a, b| b.at.total_cmp(&a.at));
        out
    }
}

/// Strictly increasing time stamps, so entries made in the same clock tick keep their order.
fn now(st: &mut State) -> f64 {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64());
    st.last = t.max(st.last + 1e-6);
    st.last
}

fn push(items: Vec<Entry>, entry: Entry, keep: impl Fn(&Entry) -> bool) -> Vec<Entry> {
    std::iter::once(entry)
        .chain(items.into_iter().filter(|e| keep(e)))
        .take(KEEP)
        .collect()
}

fn load(file: &Path) -> State {
    let Ok(text) = std::fs::read_to_string(file) else {
        return State::default();
    };
    let Ok(data) = serde_json::from_str::<Payload>(&text) else {
        return State::default();
    };
    let entries = |kind: RecentKind, v: Vec<Item>| -> Vec<Entry> {
        v.into_iter()
            .take(KEEP)
            .map(|i| Entry {
                kind,
                text: i.text,
                at: i.at,
            })
            .collect()
    };
    State {
        jumps: data
            .jumps
            .into_iter()
            .map(|(k, v)| (k, entries(RecentKind::Jump, v)))
            .collect(),
        filters: entries(RecentKind::Filter, data.filters),
        last: 0.0,
    }
}

fn save(file: &Path, st: &State) -> std::io::Result<()> {
    let item = |e: &Entry| Item {
        text: e.text.clone(),
        at: e.at,
    };
    let payload = Payload {
        version: 1,
        jumps: st
            .jumps
            .iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(k, v)| (k.clone(), v.iter().map(item).collect()))
            .collect(),
        filters: st.filters.iter().map(item).collect(),
    };
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = file.with_extension("tmp");
    let text = serde_json::to_string_pretty(&payload).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, file)
}
