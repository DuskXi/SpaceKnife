//! What `tree.summary` could not work out (834ab21, server/scans.py `summary_of`).
//!
//! A summary only needs the node itself (its row, path and whether it is on disk). Every other
//! part — the walk below it (largest files, class totals, newest change), the folder classes, the
//! largest files' classes, why the node takes a folder class, the tags, the volume, the parent —
//! may fail on its own: it is left empty and named in [`Gaps`] (`TreeSummaryResult.gaps`), with
//! the reason in the interface language. When the folder classes fail, the class totals are
//! counted by extension alone ([`summarize_node`]). A kind of failure is logged as a warning once
//! per scan, then at debug level (the details panel asks again every second or so). When even the
//! node itself cannot be described, [`summary_failed`] logs it and gives the error message the
//! handler returns instead of "internal error".
//!
//! Rust has no exceptions: a part "fails" when it returns an error (a walk over an inconsistent
//! tree) or panics (an index out of range); [`Gaps::guard`] catches both.

use std::any::Any;
use std::collections::HashSet;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use parking_lot::Mutex;
use sk_protocol::scan::{SummaryGap, SummaryPart};
use sk_store::subtree::{Summary, summarize};
use sk_store::{NodeId, Nodes};

use crate::layout::basis::{Basis, Classes};
use crate::tr;

/// (scan, part, kind of failure) pairs logged as a warning are kept up to this many.
pub const GAP_LOG_KEEP: usize = 256;
/// Ids per piece of the walk.
pub const WALK_CHUNK: usize = 1 << 16;

static LOGGED: Mutex<Option<HashSet<(String, String, String)>>> = Mutex::new(None);

/// The reason shown for a failed part, in the interface language.
pub fn gap_text(part: SummaryPart, error: &str) -> String {
    match part {
        SummaryPart::Contents => tr!(
            "子项的统计没能算出（最大的文件、类别统计、最近修改）：{error}",
            error = error
        ),
        SummaryPart::Classes => tr!(
            "文件夹类别没能算出，类别只按扩展名统计：{error}",
            error = error
        ),
        SummaryPart::TopFiles => tr!("最大的文件没能列出：{error}", error = error),
        SummaryPart::FolderClass => tr!("文件夹归类的来由没能读出：{error}", error = error),
        SummaryPart::Tags => tr!("标签没能读出：{error}", error = error),
        SummaryPart::Volume => tr!("所在卷的信息没能读出：{error}", error = error),
        SummaryPart::Parent => tr!("上级文件夹的信息没能读出：{error}", error = error),
    }
}

/// An error as one printable line of at most 300 characters.
pub fn error_text(error: &str) -> String {
    let clean: String = error
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let clean = clean.trim();
    let clean = if clean.is_empty() { "error" } else { clean };
    if clean.chars().count() <= 300 {
        clean.to_owned()
    } else {
        let mut out: String = clean.chars().take(299).collect();
        out.push('…');
        out
    }
}

/// The text of a caught panic.
pub fn panic_text(payload: &(dyn Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        format!("panic: {s}")
    } else if let Some(s) = payload.downcast_ref::<String>() {
        format!("panic: {s}")
    } else {
        "panic".to_owned()
    }
}

/// Whether this kind of failure is new for this scan (then it is logged as a warning, else at
/// debug level). `kind` names the failure (Python: the exception type).
pub fn first_time(scan_id: &str, part: &str, kind: &str) -> bool {
    let mut g = LOGGED.lock();
    let set = g.get_or_insert_with(HashSet::new);
    let key = (scan_id.to_owned(), part.to_owned(), kind.to_owned());
    if set.contains(&key) {
        return false;
    }
    if set.len() >= GAP_LOG_KEEP {
        set.clear();
    }
    set.insert(key);
    true
}

/// A failure of one part: its text and its kind (for the log-once key).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub kind: String,
    pub error: String,
}

impl Failure {
    pub fn error(e: impl std::fmt::Display) -> Failure {
        Failure {
            kind: "error".into(),
            error: e.to_string(),
        }
    }

    pub fn panic(payload: &(dyn Any + Send)) -> Failure {
        Failure {
            kind: "panic".into(),
            error: panic_text(payload),
        }
    }
}

/// The parts of one summary that failed: each part once, each kind of failure logged once per scan.
#[derive(Debug)]
pub struct Gaps {
    scan_id: String,
    path: String,
    items: Vec<SummaryGap>,
}

impl Gaps {
    pub fn new(scan_id: &str, path: &str) -> Gaps {
        Gaps {
            scan_id: scan_id.to_owned(),
            path: path.to_owned(),
            items: Vec::new(),
        }
    }

    /// Records that `part` failed (the first failure of a part counts).
    pub fn add(&mut self, part: SummaryPart, failure: &Failure) {
        if self.items.iter().any(|g| g.part == part) {
            return;
        }
        let reason = gap_text(part, &error_text(&failure.error));
        let line = tr!(
            "{path} 的详情不完整：{reason}",
            path = self.path.as_str(),
            reason = reason.as_str()
        );
        if first_time(&self.scan_id, part.as_str(), &failure.kind) {
            tracing::warn!(scan_id = %self.scan_id, "{line}");
        } else {
            tracing::debug!(scan_id = %self.scan_id, "{line}");
        }
        self.items.push(SummaryGap { part, reason });
    }

    /// `f()`, or `fallback` (and a gap) when it panics.
    pub fn guard<T>(&mut self, part: SummaryPart, f: impl FnOnce() -> T, fallback: T) -> T {
        match catch_unwind(AssertUnwindSafe(f)) {
            Ok(v) => v,
            Err(p) => {
                self.add(part, &Failure::panic(&*p));
                fallback
            }
        }
    }

    /// `f()`'s value, or `fallback` (and a gap) when it errs or panics.
    pub fn guard_result<T, E: std::fmt::Display>(
        &mut self,
        part: SummaryPart,
        f: impl FnOnce() -> Result<T, E>,
        fallback: T,
    ) -> T {
        match catch_unwind(AssertUnwindSafe(f)) {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                self.add(part, &Failure::error(e));
                fallback
            }
            Err(p) => {
                self.add(part, &Failure::panic(&*p));
                fallback
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn items(&self) -> &[SummaryGap] {
        &self.items
    }

    pub fn into_items(self) -> Vec<SummaryGap> {
        self.items
    }
}

/// The walk below a node ([`summarize_node`]).
#[derive(Clone, Debug)]
pub enum Walk {
    /// With the folder classes.
    Full(Arc<Summary>),
    /// The folder classes failed: classes by extension alone (a `classes` gap).
    ByExtension(Arc<Summary>, Failure),
    /// The walk itself failed (a `contents` gap).
    Failed(Failure),
}

impl Walk {
    pub fn summary(&self) -> Option<&Arc<Summary>> {
        match self {
            Walk::Full(s) | Walk::ByExtension(s, _) => Some(s),
            Walk::Failed(_) => None,
        }
    }

    /// Records this walk's gap, if any.
    pub fn note(&self, gaps: &mut Gaps) {
        match self {
            Walk::Full(_) => {}
            Walk::ByExtension(_, f) => gaps.add(SummaryPart::Classes, f),
            Walk::Failed(f) => gaps.add(SummaryPart::Contents, f),
        }
    }
}

/// The class lookups by extension alone (no folder classes).
pub fn ext_classes(basis: &Basis, nodes: &Nodes) -> Classes {
    Basis::new(basis.class_map()).classes(nodes)
}

/// Class index of `id` under `classes`, folded into `0..len` (a folder or an unknown index
/// counts as "other").
pub fn class_index(classes: &Classes, nodes: &Nodes, id: NodeId, by_ext: bool) -> usize {
    let n = classes.len().max(1);
    let other = (classes.other() as usize).min(n - 1);
    let c = if by_ext {
        classes.ext_cls_of(nodes, id)
    } else {
        classes.cls_of(nodes, id)
    } as usize;
    if c < n { c } else { other }
}

fn walk_with(
    classes: &Classes,
    nodes: &Nodes,
    node: NodeId,
    top: usize,
    by_ext: bool,
) -> Result<Summary, sk_store::StoreError> {
    let n = classes.len().max(1);
    let mut base = |id: NodeId| class_index(classes, nodes, id, true);
    summarize(
        nodes,
        node,
        |id| class_index(classes, nodes, id, by_ext),
        n,
        top,
        if by_ext { None } else { Some(&mut base) },
        WALK_CHUNK,
    )
}

fn attempt<T>(f: impl FnOnce() -> Result<T, sk_store::StoreError>) -> Result<T, Failure> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(Failure::error(e)),
        Err(p) => Err(Failure::panic(&*p)),
    }
}

/// Largest files (`top`), class totals and newest change below `node`, with the folder classes;
/// when they fail, by extension alone; [`Walk::Failed`] when the walk itself fails.
pub fn summarize_node(basis: &Basis, nodes: &Nodes, node: NodeId, top: usize) -> Walk {
    let full = attempt(|| {
        let classes = basis.classes(nodes);
        walk_with(&classes, nodes, node, top, false)
    });
    match full {
        Ok(s) => Walk::Full(Arc::new(s)),
        Err(f) => match attempt(|| walk_with(&ext_classes(basis, nodes), nodes, node, top, true)) {
            Ok(s) => Walk::ByExtension(Arc::new(s), f),
            Err(_) => Walk::Failed(f),
        },
    }
}

/// `tree.summary` could not even describe the node itself: logs it (once per scan and kind of
/// failure) and returns the message of the error the handler answers with.
pub fn summary_failed(scan_id: &str, path: Option<&str>, failure: &Failure) -> String {
    let error = error_text(&failure.error);
    let path = path.map_or_else(|| "?".to_owned(), str::to_owned);
    let line = tr!(
        "读不到 {path} 的详情：{error}",
        path = path.as_str(),
        error = error.as_str()
    );
    if first_time(scan_id, "failed", &failure.kind) {
        tracing::error!(scan_id, "{line}");
    } else {
        tracing::debug!(scan_id, "{line}");
    }
    tr!("读不到这一项的详情：{error}", error = error.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_part_fails_once_and_is_logged_once_per_scan() {
        let mut g = Gaps::new("scan-gaps-test", "/r");
        let v = g.guard(SummaryPart::Tags, || -> Option<u8> { panic!("boom") }, None);
        assert_eq!(v, None);
        g.guard(SummaryPart::Tags, || -> u8 { panic!("again") }, 0);
        let r: u8 = g.guard_result(SummaryPart::Volume, || Err::<u8, _>("no volume"), 7);
        assert_eq!(r, 7);
        assert_eq!(g.items().len(), 2);
        assert_eq!(g.items()[0].part, SummaryPart::Tags);
        assert!(g.items()[0].reason.contains("boom"));
        assert!(!first_time("scan-gaps-test", "tags", "panic"));
        assert!(first_time("scan-gaps-test-2", "tags", "panic"));
    }

    #[test]
    fn errors_are_one_short_line() {
        assert_eq!(error_text("a\nb"), "a b");
        let long = "x".repeat(400);
        let t = error_text(&long);
        assert_eq!(t.chars().count(), 300);
        assert!(t.ends_with('…'));
        assert_eq!(error_text(""), "error");
    }
}
