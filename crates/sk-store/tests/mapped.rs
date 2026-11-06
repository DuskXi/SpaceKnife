//! Memory-mapped mode (test_store_layout.py): columns and names in self-deleting files.

use std::sync::Arc;

use sk_store::testing::{self, dirs_and_files};
use sk_store::*;

fn policy(dir: &std::path::Path) -> Arc<Policy> {
    let p = Arc::new(Policy::new());
    p.set_columns_dir(dir.to_path_buf());
    p.set_physical_memory(Some(Some(1 << 40))); // the budget never decides
    p
}

fn grow(store: &Store, rows: usize) {
    let mut a = store.appender();
    let root = a.add_root_dir("/r").unwrap();
    let mut pending = std::collections::VecDeque::from([root]);
    let mut k = 0;
    while a.len() < rows {
        let dd = pending.pop_front().unwrap();
        let start = a
            .ingest_listing(dd, &dirs_and_files(&format!("cache{k}_"), 3, 7))
            .unwrap();
        pending.extend(start..start + 3);
        k += 1;
    }
}

#[test]
fn mapped_mode_keeps_columns_out_of_private_memory() {
    let dir = tempfile::tempdir().unwrap();
    let p = policy(dir.path());
    p.set_force_bytes(Some(0)); // every store maps from its first growth
    let store = Store::with_options(StoreOptions {
        policy: p,
        ..StoreOptions::default()
    });
    grow(&store, 30_000);
    {
        let mut a = store.appender();
        assert!(a.is_mapped());
        assert!(a.columns_mapped());
        assert!(store.ram_bytes() < 1 << 20); // only the first, tiny name chunk stayed in plain memory
        let names = testing::all_names(&a);
        let tot = a.tot_alloc(1);
        assert_eq!(a.compact(), 0); // mapped columns keep their tail: never touched, never written
        assert!(a.columns_mapped());
        assert_eq!(testing::all_names(&a), names);
        assert_eq!(a.tot_alloc(1), tot);
        testing::check_invariants(&a).unwrap();
    }
    // Windows: delete-on-close files, listed until the store lets go; elsewhere they are unlinked
    // as soon as they are made (tempfile), and the folder stays empty throughout
    #[cfg(windows)]
    assert!(std::fs::read_dir(dir.path()).unwrap().count() > 20);
    drop(store);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0); // self-deleting
}

#[test]
fn a_scan_maps_once_it_grows_large() {
    let dir = tempfile::tempdir().unwrap();
    let p = policy(dir.path());
    p.set_large(Some(1 << 20)); // 1 MiB: about 9000 rows with their names
    let opts = StoreOptions {
        policy: p.clone(),
        ..StoreOptions::default()
    };
    let small = Store::with_options(StoreOptions {
        capacity: 16,
        ..opts.clone()
    });
    grow(&small, 2000);
    assert!(!small.appender().is_mapped()); // small scans stay in plain memory
    let store = Store::with_options(StoreOptions {
        capacity: 16,
        ..opts.clone()
    });
    grow(&store, 30_000);
    let mut a = store.appender();
    assert!(a.is_mapped() && a.columns_mapped());
    let names = testing::all_names(&a);
    a.compact(); // the end of the scan: the shrunk columns stay mapped
    assert!(a.columns_mapped());
    assert_eq!(testing::all_names(&a), names);
    drop(a);
    let (copy, _) = mutate::compact_with(&store.read(), opts.clone()).unwrap();
    assert!(copy.read().columns_mapped()); // a compacted (or MFT, or loaded) tree maps too
    p.set_large(None);
    let plain = Store::with_options(StoreOptions {
        capacity: 16,
        ..opts
    });
    grow(&plain, 30_000);
    assert!(!plain.appender().is_mapped());
}

#[test]
fn the_budget_counts_the_other_stores() {
    let dir = tempfile::tempdir().unwrap();
    let p = policy(dir.path());
    p.set_physical_memory(Some(Some(16 << 20)));
    p.set_fraction(0.5); // 8 MiB for all stores of this policy
    let opts = StoreOptions {
        policy: p.clone(),
        capacity: 16,
        ..StoreOptions::default()
    };
    let first = Store::with_options(opts.clone());
    grow(&first, 40_000); // ~6 MiB with headroom
    assert!(!first.appender().is_mapped());
    let second = Store::with_options(opts);
    grow(&second, 40_000);
    assert!(second.appender().is_mapped()); // the first one's memory counts
}
