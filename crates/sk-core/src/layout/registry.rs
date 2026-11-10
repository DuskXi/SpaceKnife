//! The visualizations this server offers (Python `layout/registry.py` and `layout/builtins.py`).
//! Built-ins register like plugins will: through [`register`].

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use parking_lot::RwLock;
use sk_protocol::view::{ColorModeName, DRAWLIST_LAYOUT, FLAME_LAYOUT, POLAR_LAYOUT};
use sk_protocol::{CAP_LAYOUT_FLAME, CAP_LAYOUT_POLAR};

use super::api::{ENGINE_API, Visualization};
use super::flame::flame_engine;
use super::flame_polar::flame_polar_engine;
use super::treemap::TreemapEngine;

const ID_MAX: usize = 48;

/// Why a visualization cannot be registered.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegistryError {
    #[error("visualization id {0:?} is not a lowercase word list of up to 48 chars")]
    BadId(String),
    #[error("visualization {0:?} needs engine API {1}, this server has {ENGINE_API}")]
    Api(String, u32),
    #[error("visualization {0:?} has a malformed layout name {1:?}")]
    BadLayout(String, String),
    #[error("visualization {0:?} is already registered")]
    Taken(String),
}

/// `[a-z][a-z0-9]*(?:[._-][a-z0-9]+)*`
fn id_ok(id: &str) -> bool {
    let b = id.as_bytes();
    if b.is_empty() || b.len() > ID_MAX || !b[0].is_ascii_lowercase() {
        return false;
    }
    let mut prev_sep = false;
    for &c in &b[1..] {
        if matches!(c, b'.' | b'_' | b'-') {
            if prev_sep {
                return false;
            }
            prev_sep = true;
        } else if c.is_ascii_lowercase() || c.is_ascii_digit() {
            prev_sep = false;
        } else {
            return false;
        }
    }
    !prev_sep
}

/// `[a-z][a-z0-9]*\.v\d+`
fn layout_ok(layout: &str) -> bool {
    let Some((name, ver)) = layout.rsplit_once(".v") else {
        return false;
    };
    let nb = name.as_bytes();
    !nb.is_empty()
        && nb[0].is_ascii_lowercase()
        && nb
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && !ver.is_empty()
        && ver.bytes().all(|c| c.is_ascii_digit())
}

static VISUALS: LazyLock<RwLock<HashMap<String, Visualization>>> = LazyLock::new(|| {
    let mut m = HashMap::new();
    for v in builtins() {
        m.insert(v.id.clone(), v);
    }
    RwLock::new(m)
});

/// The built-in visualizations: treemap, flame graph, polar flame graph.
pub fn builtins() -> Vec<Visualization> {
    vec![
        Visualization {
            id: "treemap".into(),
            api: ENGINE_API,
            layout: DRAWLIST_LAYOUT,
            capability: String::new(), // the baseline: every session can show it
            title_id: "树图".into(),
            icon: "vizTreemap".into(),
            order: 0,
            engine: Arc::new(TreemapEngine),
            default_color: ColorModeName::Class,
            compare: true,
            free_space: true,
        },
        Visualization {
            id: "flame".into(),
            api: ENGINE_API,
            layout: FLAME_LAYOUT,
            capability: CAP_LAYOUT_FLAME.into(),
            title_id: "火焰图".into(),
            icon: "vizFlame".into(),
            order: 1,
            engine: Arc::new(flame_engine()),
            default_color: ColorModeName::Branch,
            compare: true,
            free_space: true,
        },
        Visualization {
            id: "flame_polar".into(),
            api: ENGINE_API,
            layout: POLAR_LAYOUT,
            capability: CAP_LAYOUT_POLAR.into(),
            title_id: "火焰-极坐标图".into(),
            icon: "vizFlamePolar".into(),
            order: 2,
            engine: Arc::new(flame_polar_engine()),
            default_color: ColorModeName::Branch,
            compare: true,
            free_space: true,
        },
    ]
}

/// Add `v`: an error for a malformed or taken id, another engine API or a malformed layout.
pub fn register(v: Visualization) -> Result<(), RegistryError> {
    if !id_ok(&v.id) {
        return Err(RegistryError::BadId(v.id));
    }
    if v.api != ENGINE_API {
        return Err(RegistryError::Api(v.id, v.api));
    }
    if !layout_ok(v.layout) {
        return Err(RegistryError::BadLayout(v.id, v.layout.to_owned()));
    }
    let mut m = VISUALS.write();
    if m.contains_key(&v.id) {
        return Err(RegistryError::Taken(v.id));
    }
    m.insert(v.id.clone(), v);
    Ok(())
}

/// Remove a visualization (tests; plugins being turned off). Unknown ids are ignored.
pub fn unregister(id: &str) {
    VISUALS.write().remove(id);
}

/// The visualization `id`.
pub fn get(id: &str) -> Option<Visualization> {
    VISUALS.read().get(id).cloned()
}

/// Every registered visualization, by order then id.
pub fn all_visuals() -> Vec<Visualization> {
    let mut v: Vec<Visualization> = VISUALS.read().values().cloned().collect();
    v.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.id.cmp(&b.id)));
    v
}

/// Whether a session with capabilities `caps` can show `v`.
pub fn is_available<S: AsRef<str>>(v: &Visualization, caps: &[S]) -> bool {
    v.capability.is_empty() || caps.iter().any(|c| c.as_ref() == v.capability)
}

/// The visualizations a session with capabilities `caps` can show, by order then id.
pub fn available<S: AsRef<str>>(caps: &[S]) -> Vec<Visualization> {
    all_visuals()
        .into_iter()
        .filter(|v| is_available(v, caps))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_layouts() {
        assert!(id_ok("treemap") && id_ok("test.fake") && id_ok("a-b_c.d9"));
        assert!(!id_ok("Treemap") && !id_ok("a..b") && !id_ok("a.") && !id_ok("9a") && !id_ok(""));
        assert!(layout_ok("drawlist.v1") && layout_ok("flame.v12"));
        assert!(!layout_ok("drawlist") && !layout_ok("Flame.v1") && !layout_ok("flame.v"));
    }
}
