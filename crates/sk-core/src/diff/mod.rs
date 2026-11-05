//! Snapshot compare (docs/manage.md §3): two trees merged by path, with both ends and the change.
//!
//! * [`compare`] merges two stores into a [`DiffTree`] (the union tree, a [`sk_store::Store`],
//!   plus per-row arrays: both ends, the change, a [`Status`], growth and shrinkage below).
//! * [`DiffTree::changes`] ranks change units (a partition of the change, largest first).
//! * [`ChangeTree`] keeps only what changed (the store the compare views draw), with
//!   [`subtree_sums`], [`short_label`] and the [`sk_protocol::compare::ChangeItem`] builders
//!   the server's compare handlers use.

mod changes;
mod engine;
mod text;

pub use changes::{
    ChangeTree, change_item, change_kind, change_status, rest_item, short_label, subtree_sums,
};
pub use engine::Change;
pub use engine::{
    CompareOptions, DOMINANT, DiffError, DiffTree, Direction, MINOR, Status, compare, compare_with,
    status_label,
};
