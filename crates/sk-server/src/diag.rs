//! Memory watchdog (Python `server/diag.py`): when the backend holds far more than its scans
//! account for, say what holds it.
//!
//! Every [`PERIOD`] the private memory of the process is compared with what the scans need (node
//! stores in plain memory, bytes held while an MFT tree is built, name indexes). Beyond
//! `2 × accounted + 1.5 GiB` — plus the largest scan closed in the last hour, which the heap keeps
//! for the next scan to reuse (docs/memory.md §8) — the heap is first asked to give back what it
//! can; when that does not bring the process under the limit, a warning goes to the log panel and
//! a breakdown to the log file: scans, views, the compute slots, session outboxes. Reports repeat
//! only after another 25 % of growth.

use std::sync::Arc;
use std::time::Duration;

use sk_core::events::{self, EventLevel, Source};
use sk_core::memory::{process_memory, release};
use sk_core::tr;

use crate::app::Server;
use crate::scans::CLOSED_KEEP;

pub const PERIOD: Duration = Duration::from_secs(15);
/// 1.5 GiB
pub const SLACK: u64 = 3 << 29;
const GIB: f64 = (1u64 << 30) as f64;

/// Private bytes the open scans need: node stores in plain memory (mapped columns are file-backed,
/// not private), what an MFT build holds besides, and name indexes.
pub fn accounted(server: &Server) -> u64 {
    server
        .scans
        .list()
        .iter()
        .map(|h| (h.store.ram_bytes() + h.st.lock().transient + h.ctx.names.memory_bytes()) as u64)
        .sum()
}

/// What the heap may still keep from scans closed in the last hour: about the largest of them.
pub fn retained(server: &Server) -> u64 {
    server
        .scans
        .closed
        .lock()
        .iter()
        .filter(|(when, _)| when.elapsed() < CLOSED_KEEP)
        .map(|(_, size)| *size as u64)
        .max()
        .unwrap_or(0)
}

/// The breakdown written to the log file.
pub fn report(server: &Server) -> String {
    let mem = process_memory();
    let mut lines = vec![format!(
        "private {:.2} GiB, resident {:.2} GiB, accounted {:.2} GiB, retained after closed scans up to {:.2} GiB",
        mem.private.unwrap_or(0) as f64 / GIB,
        mem.resident.unwrap_or(0) as f64 / GIB,
        accounted(server) as f64 / GIB,
        retained(server) as f64 / GIB,
    )];
    for h in server.scans.list() {
        let nodes = h.store.read();
        lines.push(format!(
            "  scan {}: {} nodes, memory {:.0} MB{}, names {:.0} MB, name index {:.0} MB",
            h.id,
            sk_core::fmt::count(nodes.len() as i64),
            nodes.memory_bytes() as f64 / 1048576.0,
            if nodes.columns_mapped() { " (mapped)" } else { "" },
            nodes.names_size() as f64 / 1048576.0,
            h.ctx.names.memory_bytes() as f64 / 1048576.0,
        ));
    }
    let (free, slots) = crate::compute::stats();
    lines.push(format!("  views {}, compute {} of {} slots busy", server.views.len(), slots - free.min(slots), slots));
    for s in server.sessions() {
        let (count, size) = s.outbox_stats();
        lines.push(format!("  session {}: outbox {count} messages, {:.1} MB", s.id, size as f64 / 1048576.0));
    }
    lines.join("\n")
}

/// One look: true when it warned.
pub fn check(server: &Server, last: &mut u64) -> bool {
    let Some(private) = process_memory().private else { return false };
    let limit = 2 * accounted(server) + SLACK + retained(server);
    if private <= limit || (private as f64) <= *last as f64 * 1.25 {
        return false;
    }
    release(); // freed but kept by the heap: give it back, then judge
    let Some(private) = process_memory().private else { return false };
    if private <= limit {
        return false;
    }
    *last = private;
    tracing::warn!("memory above what the scans account for:\n{}", report(server));
    events::emit(
        Source::System,
        &tr!(
            "后端内存占用异常：{used} GB（扫描数据约 {scans} GB），诊断信息已写入日志文件",
            used = format!("{:.1}", private as f64 / GIB),
            scans = format!("{:.1}", accounted(server) as f64 / GIB)
        ),
        EventLevel::Warning,
        None,
        None,
    );
    true
}

/// Start the watchdog (a background task of the server).
pub fn start(server: &Arc<Server>) {
    let weak = Arc::downgrade(server);
    server.spawn_background(async move {
        let mut last = 0u64;
        loop {
            tokio::time::sleep(PERIOD).await;
            let Some(server) = weak.upgrade() else { return };
            // diagnostics never take the backend down
            let res = tokio::task::spawn_blocking(move || {
                let mut l = last;
                check(&server, &mut l);
                l
            })
            .await;
            match res {
                Ok(l) => last = l,
                Err(e) => tracing::error!("memory watchdog failed: {e}"),
            }
        }
    });
}
