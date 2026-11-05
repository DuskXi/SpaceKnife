//! compare.*: snapshot comparisons (W6, docs/manage.md §3; Python `server/compare_handlers.py`).
//! Progress goes out as `compare.progress` events to the session that opened the comparison.

use std::sync::Arc;

use sk_protocol::EmptyResult;
use sk_protocol::methods::{
    CompareClose, CompareInfoMethod, CompareNode, CompareOpen, CompareSources, CompareSummaryMethod,
};

use crate::compute::Latest;
use crate::rpc::Router;
use crate::topics::TopicRegistry;

pub fn install(r: &mut Router, _t: &mut TopicRegistry) {
    r.add::<CompareSources, _, _>(|call, p| async move {
        let server = call.server.clone();
        crate::compute::blocking(move || server.compares.sources(&server, &p.scan_id)).await?
    });

    r.add::<CompareOpen, _, _>(|call, p| async move {
        call.server.compares.open(&call.server, &call.session, &p)
    });

    r.add::<CompareInfoMethod, _, _>(|call, p| async move {
        call.server.compares.get(&p.compare_id)?.info()
    });

    r.add::<CompareClose, _, _>(|call, p| async move {
        call.server.compares.close(&call.server, &p.compare_id);
        Ok(EmptyResult {})
    });

    let latest: Arc<Latest<String>> = Arc::new(Latest::new());
    r.add::<CompareSummaryMethod, _, _>(move |call, p| {
        let latest = latest.clone();
        async move {
            let host = call.server.views.get(&call.session, &p.view_id)?;
            let server = call.server.clone();
            let _guard = host.lock.lock().await;
            let h = host.clone();
            latest
                .run(format!("summary:{}", p.view_id), move || {
                    server.compares.summary_of(&server, &h, p.limit as usize)
                })
                .await?
        }
    });

    r.add::<CompareNode, _, _>(|call, p| async move {
        let host = call.server.views.get(&call.session, &p.view_id)?;
        let server = call.server.clone();
        let _guard = host.lock.lock().await;
        let h = host.clone();
        crate::compute::compute(move || server.compares.node(&h, p.key, p.children as usize))
            .await?
    });
}
