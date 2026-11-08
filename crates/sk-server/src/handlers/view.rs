//! view.*: open / close, navigation, selection, settings, filter, tags, results; visual.list
//! (Python `server/view_handlers.py`).

use std::sync::Arc;
use std::sync::atomic::Ordering;

use sk_core::layout::registry;
use sk_core::tr;
use sk_core::view::{results_page, showable};
use sk_protocol::EmptyResult;
use sk_protocol::methods::*;
use sk_protocol::view::{ViewListResult, VisualInfo, VisualListResult};

use crate::compute::Latest;
use crate::rpc::Router;
use crate::topics::TopicRegistry;
use crate::views::{apply_settings, settings_key};

pub fn install(r: &mut Router, _t: &mut TopicRegistry) {
    r.add::<ViewOpen, _, _>(|call, p| async move {
        let server = &call.server;
        let host = server.views.open(server, &call.session, &p)?;
        server
            .views
            .act(&host, Some(call.id.clone()), |_| Ok((true, "open")))
            .await
    });

    r.add::<ViewClose, _, _>(|call, p| async move {
        call.server.views.get(&call.session, &p.view_id)?;
        call.server.views.close(&p.view_id);
        Ok(EmptyResult {})
    });

    r.add::<ViewList, _, _>(|call, p| async move {
        let session_id = call.session.id.clone();
        let hosts: Vec<_> = call
            .server
            .views
            .all()
            .into_iter()
            .filter(|h| {
                h.session_id == session_id && p.scan_id.as_deref().is_none_or(|s| s == h.scan_id)
            })
            .collect();
        crate::compute::blocking(move || {
            let mut views: Vec<_> = hosts
                .iter()
                .map(|h| {
                    (
                        h.id.clone(),
                        sk_core::view::view_state(&h.view.lock(), &h.scan_id),
                    )
                })
                .collect();
            views.sort_by_key(|(id, _)| id.trim_start_matches("view-").parse::<u64>().unwrap_or(0));
            ViewListResult {
                views: views.into_iter().map(|(_, s)| s).collect(),
            }
        })
        .await
    });

    macro_rules! nav {
        ($marker:ty, $reason:literal, $f:expr) => {
            r.add::<$marker, _, _>(|call, p| async move {
                let host = call.server.views.get(&call.session, &p.view_id)?;
                call.server
                    .views
                    .act(&host, Some(call.id.clone()), move |v| {
                        Ok(($f(v, &p), $reason))
                    })
                    .await
            });
        };
    }
    nav!(
        ViewRender,
        "render",
        |_: &mut sk_core::view::View, _: &sk_protocol::view::ViewIdParams| true
    );
    nav!(
        ViewZoom,
        "zoom",
        |v: &mut sk_core::view::View, p: &sk_protocol::view::ViewZoomParams| v.zoom_into(p.key)
    );
    nav!(
        ViewBack,
        "back",
        |v: &mut sk_core::view::View, _: &sk_protocol::view::ViewIdParams| v.go_back()
    );
    nav!(
        ViewForward,
        "forward",
        |v: &mut sk_core::view::View, _: &sk_protocol::view::ViewIdParams| v.go_forward()
    );
    nav!(
        ViewUp,
        "up",
        |v: &mut sk_core::view::View, _: &sk_protocol::view::ViewIdParams| v.go_up()
    );
    nav!(
        ViewHome,
        "home",
        |v: &mut sk_core::view::View, _: &sk_protocol::view::ViewIdParams| v.go_home()
    );
    nav!(
        ViewSelect,
        "select",
        |v: &mut sk_core::view::View, p: &sk_protocol::view::ViewSelectParams| {
            if v.selected == p.key {
                false
            } else {
                v.select(p.key);
                true
            }
        }
    );
    nav!(
        ViewTag,
        "tag",
        |v: &mut sk_core::view::View, p: &sk_protocol::view::ViewTagParams| v.tag_selected(p.color)
    );
    nav!(
        ViewReveal,
        "reveal",
        |v: &mut sk_core::view::View, p: &sk_protocol::view::ViewRevealParams| v.reveal(p.key)
    );

    r.add::<ViewSet, _, _>(|call, p| async move {
        let host = call.server.views.get(&call.session, &p.view_id)?;
        let caps = call.session.capabilities().to_vec();
        call.server
            .views
            .act(&host, Some(call.id.clone()), move |v| {
                let (before, visual) = (settings_key(v), v.visual.clone());
                apply_settings(v, &p.settings, &caps)?;
                let changed = settings_key(v) != before;
                // another visualization: the frontend cross-fades instead of re-laying out in place
                Ok((
                    changed,
                    if v.visual != visual {
                        "visual"
                    } else {
                        "settings"
                    },
                ))
            })
            .await
    });

    r.add::<ViewFilter, _, _>(|call, p| async move {
        let host = call.server.views.get(&call.session, &p.view_id)?;
        let generation = host.filter_gen.fetch_add(1, Ordering::SeqCst) + 1;
        let h = host.clone();
        call.server
            .views
            .act(&host, Some(call.id.clone()), move |v| {
                if h.filter_gen.load(Ordering::SeqCst) != generation {
                    return Ok((false, "filter")); // a newer expression is already waiting: skip this one
                }
                let canon =
                    |v: &sk_core::view::View| v.filter.normal.as_ref().map(|n| n.canonical.clone());
                let before = (
                    canon(v),
                    v.filter.result.as_ref().map(|r| Arc::as_ptr(r) as usize),
                );
                v.set_filter(&p.text, p.cursor);
                let after = (
                    canon(v),
                    v.filter.result.as_ref().map(|r| Arc::as_ptr(r) as usize),
                );
                Ok((after != before, "filter"))
            })
            .await
    });

    r.add::<VisualList, _, _>(|call, _| async move {
        let caps = call.session.capabilities();
        let mut visuals: Vec<VisualInfo> = registry::all_visuals()
            .into_iter()
            .map(|v| {
                let available = showable(&v.id, caps).is_some();
                VisualInfo {
                    name: sk_core::i18n::t(&v.title_id).to_owned(),
                    id: v.id,
                    layout: v.layout.to_owned(),
                    icon: v.icon,
                    order: i64::from(v.order),
                    compare: v.compare,
                    free_space: v.free_space,
                    default_color: v.default_color,
                    available,
                    reason: (!available).then(|| tr!("这种可视化在当前界面不可用")),
                }
            })
            .collect();
        visuals.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.id.cmp(&b.id)));
        Ok(VisualListResult { visuals })
    });

    let latest: Arc<Latest<(String, u32, u32)>> = Arc::new(Latest::new());
    r.add::<ViewResultsMethod, _, _>(move |call, p| {
        let latest = latest.clone();
        async move {
            let host = call.server.views.get(&call.session, &p.view_id)?;
            let _guard = host.lock.lock().await;
            let h = host.clone();
            latest
                .run((p.view_id.clone(), p.offset, p.limit), move || {
                    let mut v = h.view.lock();
                    if let Some(s) = p.sort {
                        v.results_sort = s;
                    }
                    let mut cache = h.results.lock();
                    results_page(
                        &v,
                        &mut cache,
                        h.seq.load(Ordering::SeqCst),
                        p.offset as usize,
                        p.limit as usize,
                    )
                })
                .await
        }
    });
}
