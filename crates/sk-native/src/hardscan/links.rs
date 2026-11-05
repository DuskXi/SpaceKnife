//! Hard links counted once while a scan runs (spaceknife.scan.scanner.Scanner._fold): the
//! `(dev, ino)` of every file with more than one link that was already counted, kept across the
//! scan's slices.

use crate::util::FxHashSet;

use super::Entries;
use super::common::{flag, kind};

/// The `(dev, ino)` of hard-linked files a scan has counted, across its slices.
#[derive(Default, Debug, Clone)]
pub struct LinkSet {
    seen: FxHashSet<(u64, u64)>,
}

impl LinkSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Goes through a slice's entries in order: a file with more than one link whose
    /// `(dev, ino)` was counted before (earlier in this slice or in an earlier one) gets
    /// [`flag::HARDLINK_DUP`] in `flags`; the first one is remembered. Returns how many were
    /// marked.
    ///
    /// # Panics
    /// When the columns differ in length.
    pub fn mark(
        &mut self,
        dev: &[u64],
        ino: &[u64],
        nlink: &[u16],
        kinds: &[u8],
        flags: &mut [u32],
    ) -> usize {
        let n = flags.len();
        assert!(
            [dev.len(), ino.len(), nlink.len(), kinds.len()]
                .iter()
                .all(|&len| len == n),
            "LinkSet::mark: columns differ in length"
        );
        let mut marked = 0;
        for i in 0..n {
            if nlink[i] > 1 && kinds[i] == kind::FILE && !self.seen.insert((dev[i], ino[i])) {
                flags[i] |= flag::HARDLINK_DUP;
                marked += 1;
            }
        }
        marked
    }

    /// [`LinkSet::mark`] over a slice's entry columns.
    pub fn mark_entries(&mut self, e: &mut Entries) -> usize {
        self.mark(&e.dev, &e.ino, &e.nlink, &e.kind, &mut e.flags)
    }

    pub fn len(&self) -> usize {
        self.seen.len()
    }

    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }

    pub fn contains(&self, dev: u64, ino: u64) -> bool {
        self.seen.contains(&(dev, ino))
    }

    pub fn clear(&mut self) {
        self.seen.clear();
    }
}
