//! What a compare view (W6) needs of its change tree: the change and status per row, and the area
//! basis of one direction. The analysis task's `crate::diff::ChangeTree` provides it through
//! [`ArrayChanges`] (the server builds one per change tree).

use std::sync::Arc;

use sk_protocol::compare::CompareDirection;
use sk_store::NodeId;

/// The change tree behind a compare view; one entry per row of the view's (change) store.
pub trait CompareSource: Send + Sync {
    /// Rows covered.
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Signed net change of allocated bytes of a row.
    fn delta(&self, id: NodeId) -> i64;
    /// 0 unchanged, 1 new, 2 deleted, 3 changed.
    fn status(&self, id: NodeId) -> u8;
    /// Area basis per row of `direction` (grow / shrink); with `matched` (a filter's matches per
    /// row), only the matching files, summed up.
    fn amounts(&self, direction: CompareDirection, matched: Option<&[bool]>) -> Vec<i64>;
}

type Amounts = dyn Fn(CompareDirection, Option<&[bool]>) -> Vec<i64> + Send + Sync;

/// A [`CompareSource`] from plain arrays and an amounts function, e.g. for a `ChangeTree` `t`:
/// `ArrayChanges::new(t.diff.delta.clone(), t.diff.status.iter().map(|s| *s as u8).collect(),
/// move |d, m| t.amounts(d == Grow, d == Shrink, m))`.
pub struct ArrayChanges {
    pub delta: Arc<Vec<i64>>,
    pub status: Arc<Vec<u8>>,
    amounts: Box<Amounts>,
}

impl ArrayChanges {
    pub fn new(
        delta: Vec<i64>,
        status: Vec<u8>,
        amounts: impl Fn(CompareDirection, Option<&[bool]>) -> Vec<i64> + Send + Sync + 'static,
    ) -> ArrayChanges {
        ArrayChanges {
            delta: Arc::new(delta),
            status: Arc::new(status),
            amounts: Box::new(amounts),
        }
    }
}

impl CompareSource for ArrayChanges {
    fn len(&self) -> usize {
        self.delta.len()
    }
    fn delta(&self, id: NodeId) -> i64 {
        self.delta.get(id as usize).copied().unwrap_or(0)
    }
    fn status(&self, id: NodeId) -> u8 {
        self.status.get(id as usize).copied().unwrap_or(0)
    }
    fn amounts(&self, direction: CompareDirection, matched: Option<&[bool]>) -> Vec<i64> {
        (self.amounts)(direction, matched)
    }
}
