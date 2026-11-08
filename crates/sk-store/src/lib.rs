//! # sk-store — the scanned tree as columns
//!
//! The tree is a struct-of-arrays table (20 columns, 89 bytes a node) plus a heap of names; no
//! object per node. Per-node cost, scale and the two storage modes: `docs/memory.md`.
//!
//! ## Layout invariants (relied upon everywhere)
//!
//! * Node 0 is a virtual top node (`Dir`, `VIRTUAL`, parent -1); scan roots are its children
//!   (depth 1), added before any listing.
//! * A directory's children are appended together, so they occupy the contiguous id range
//!   `child_start .. child_start + child_count` ([`Nodes::child_range`]).
//! * While a scan builds the tree, every node's id is greater than its parent's. A relist
//!   ([`mutate::relist`]) moves a folder to new ids but keeps its children where they are, so
//!   afterwards ranges may lie before their folder; [`mutate::compact`] restores the order.
//! * Names are appended in id order, each as `\0name`, into chunks that never move
//!   ([`Nodes::names_run`]): consecutive nodes' names are adjacent, so a name index searches
//!   them in place, zero-copy.
//! * `tot_*` of a folder cover its descendants (not itself); a node's own `size`/`alloc` count
//!   toward its parent unless it is a `HARDLINK_DUP`. [`Nodes::effective_size`] is what to show.
//! * `GONE` rows (deleted, or the old copy of a moved row) are in no child range, kind `Other`,
//!   parent -1, zero sizes.
//! * Kinds and flag bits are those of the Python backend ([`Kind`], [`Flags`]).
//!
//! ## Threading contract
//!
//! A [`Store`] is a cheap `Clone` handle (`Arc`) shared by every thread. Access goes through
//! three guards; all three deref to [`Nodes`], the read API.
//!
//! | guard | how many | excludes | may change |
//! |---|---|---|---|
//! | [`Reader`] ([`Store::read`]) | any number | writers | nothing |
//! | [`Appender`] ([`Store::appender`]) | one | other appenders, writers | new rows; the atomic columns |
//! | [`Writer`] ([`Store::write`]) | one | everything | anything |
//!
//! * **Readers run while the scanner appends.** A reader sees the rows published when its guard
//!   was taken ([`Nodes::len`] stays fixed for the guard's life) and never rows written later.
//!   Rows below that count never change in the non-atomic columns while any reader exists. The
//!   columns a scan updates after publishing a row — `flags`, `child_start`, `child_count`,
//!   `tot_*` — are atomics; readers may see them move (totals growing, a folder becoming
//!   listed) but never a torn value.
//! * **Publishing order.** An ingest writes the new rows, publishes the store's count, then
//!   stores the listed folder's `child_start`, `child_count` last (release). [`Nodes::child_range`]
//!   loads the count first (acquire), then the start, and treats a range that starts at 0 or
//!   runs past the view's count as empty: a folder being listed right now is simply not listed
//!   yet. Every walk in this crate also stops after `len` nodes ([`StoreError::RunawayWalk`]).
//! * **Growth never invalidates readers.** Columns and name chunks never move: a grown column is
//!   a new buffer (or, mapped, the same file mapped longer); a reader keeps the generation it
//!   started with (`Arc`), so its slices stay valid. The appender swaps columns one at a time,
//!   so the extra memory of a growth is one column (when no reader holds the old ones).
//! * **Changes are exclusive and short.** [`Writer`] waits for the readers running when it asked
//!   and the appender. While those older readers run, new readers still start (the writer could
//!   not start before them anyway), so one long reader holds up the change, not every other
//!   reader; once they are done, new readers wait for the writer (neither side starves). A writer
//!   that gives up ([`Store::write_until`] / [`Store::try_write_for`]) lets them go on. Relists
//!   take milliseconds; take the writer on a blocking thread (`spawn_blocking`), not in async
//!   code that other tasks share a thread with.
//! * **Long reads.** A reader guard blocks writers for as long as it lives. Computations over a
//!   whole volume (summaries, renders, snapshot saves) should take one guard per request, not
//!   hold one for minutes; caches must key on [`Store::version`] and the view's `len`.
//! * **No nesting / no lock inversion.** Never take [`Store::read`] or [`Store::write`] on a
//!   thread that already holds a guard of the same store (a waiting writer would deadlock it):
//!   an appender reads through its own deref to [`Nodes`]. Guards are not `Send`.
//! * **Renumbering.** [`mutate::compact`] is a pure read (run it under a [`Reader`]) that builds a
//!   new store; adopt it with [`Writer::adopt`] when [`Store::version`] did not move meanwhile.
//!   After an adoption every id changes: drop id-keyed caches ([`lookup::PathIndex::forget`]).
//!
//! ## Memory
//!
//! Columns live in plain memory or in self-deleting mapped files ([`mapped::Policy`]): a store
//! maps when its columns and names would push the stores of its policy past the budget (25% of
//! RAM by default), or past `large` bytes when that is switched on. While a scan grows a store
//! the columns grow by half ([`store::GROWTH`]); once complete (compacted, loaded, built whole)
//! by 1/16 or 64 Ki rows ([`store::SETTLED_SHARE`]). [`Appender::reserve`] sizes a scan up front.
//! `name_off` is u32 and widens to u64 once the names pass 4 GiB.
//!
//! ## Modules
//!
//! * [`store`]: [`Store`], the guards, ingest ([`Appender::ingest_listing`],
//!   [`Appender::ingest_slice`], [`Appender::add_root`]), [`StoreBuilder`] for whole columns.
//!   The checks and the per-entry writing of new rows are the performance layer's
//!   (`sk_native::fold`): the store lends it rows nobody reads yet ([`ColumnSlices`], the heap's
//!   tail) and publishes them in the order above.
//! * [`mutate`]: relist, stat updates, compaction (file-change monitor).
//! * [`subtree`]: subtree walks (whole or chunked) and summaries.
//! * [`lookup`]: path → node with per-folder name maps.
//! * [`snapshot`]: `.sks` files (format 3) and their retention.
//! * [`names`]: name bytes, `normcase`/`normpath`, paths.
//! * `testing` (feature `test-support`): fixture trees for the tests of this and other crates.

pub mod heap;
mod lock;
pub mod lookup;
pub mod mapped;
pub mod mutate;
pub mod names;
pub mod snapshot;
pub mod store;
pub mod subtree;
mod types;

#[cfg(any(test, feature = "test-support"))]
pub mod testing;

pub use mapped::Policy;
pub use store::{
    Appender, ColumnSlices, NamesRun, Nodes, Reader, SortBy, Store, StoreBuilder, StoreOptions,
    Writer, separate_names,
};
pub use types::*;
