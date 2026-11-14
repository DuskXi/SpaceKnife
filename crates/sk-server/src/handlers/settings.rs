//! settings.get / update / reset and the live effects of a change (Python
//! `server/settings_handlers.py`; settings.export / import are in [`super::maintenance`]).

use std::sync::Arc;

use parking_lot::Mutex;
use sk_core::settings::{SettingsError, SettingsStore};
use sk_protocol::ErrorCode;
use sk_protocol::methods::{SettingsGet, SettingsReset, SettingsUpdate};

use crate::app::Server;
use crate::rpc::{Router, RpcError, RpcResult};
use crate::topics::TopicRegistry;

pub(crate) fn mapped(e: SettingsError) -> RpcError {
    match e {
        SettingsError::Conflict(m) => RpcError::new(ErrorCode::SettingsConflict, m),
        SettingsError::Invalid(m) | SettingsError::Io(m) => RpcError::invalid_params(m),
    }
}

/// What was applied last (to act on changes only).
#[derive(Default)]
struct Applied {
    log_level: Option<String>,
    monitor: Option<bool>,
}

static APPLIED: Mutex<Applied> = Mutex::new(Applied {
    log_level: None,
    monitor: None,
});

/// The live effects of the settings: the log file's level, the memory budget, the views'
/// geometry, live monitoring, the filter vocabulary's language.
pub fn apply_live(server: &Arc<Server>, st: &SettingsStore) {
    let level = st.get_str("advanced.log_level");
    let monitor = st.get_bool("scan.monitor");
    let (level_changed, monitor_changed) = {
        let mut a = APPLIED.lock();
        let lc = a.log_level.as_deref() != Some(level.as_str());
        let mc = a.monitor.is_some_and(|m| m != monitor);
        a.log_level = Some(level.clone());
        a.monitor = Some(monitor);
        (lc, mc)
    };
    if level_changed && let Some(l) = sk_core::log::parse_level(&level) {
        sk_core::log::set_file_level(l);
    }
    let policy = sk_store::Policy::global();
    policy.set_fraction(st.get_int("advanced.memory_budget") as f64 / 100.0); // the next growth / load
    policy.set_large(
        st.get_bool("advanced.map_large_scans")
            .then_some(sk_store::mapped::LARGE_BYTES),
    );
    server.views.apply_settings_live(server, st);
    server.views.refresh_vocab(server);
    if monitor_changed {
        server.monitors.apply_settings(server);
    }
}

pub fn install(r: &mut Router, _t: &mut TopicRegistry) {
    r.add::<SettingsGet, _, _>(|call, _| async move { Ok(call.server.settings.state()) });

    r.add::<SettingsUpdate, _, _>(|call, p| async move {
        let st = call.server.settings.clone();
        // the listeners run here: they emit and schedule frames (non-blocking)
        let res: RpcResult<()> = crate::compute::blocking(move || {
            st.update(&p.values, p.version).map(|_| ()).map_err(mapped)
        })
        .await?;
        res?;
        Ok(call.server.settings.state())
    });

    r.add::<SettingsReset, _, _>(|call, p| async move {
        let st = call.server.settings.clone();
        let res: RpcResult<()> =
            crate::compute::blocking(move || st.reset(p.category).map(|_| ()).map_err(mapped))
                .await?;
        res?;
        Ok(call.server.settings.state())
    });
}
