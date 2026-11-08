//! Port of 0.3.4's `tests/test_memory_lean.py` `snapshots_open_without_a_second_copy`
//! (docs/memory.md): loading a snapshot peaks at little more than the store it builds, not twice
//! (the decompressed bytes, then the columns). 0.3.4 measured with tracemalloc; here a counting
//! global allocator of this test binary does.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use sk_store::snapshot::{self, SaveOptions};
use sk_store::testing::dirs_and_files;
use sk_store::{Kind, Store};

struct Counting;

static NOW: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

// SAFETY: forwards to the system allocator and only counts.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's contract, passed on.
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            let now = NOW.fetch_add(layout.size(), Ordering::SeqCst) + layout.size();
            PEAK.fetch_max(now, Ordering::SeqCst);
        }
        p
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        NOW.fetch_sub(layout.size(), Ordering::SeqCst);
        // SAFETY: the caller's contract, passed on.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new: usize) -> *mut u8 {
        // SAFETY: the caller's contract, passed on.
        let p = unsafe { System.realloc(ptr, layout, new) };
        if !p.is_null() {
            if new >= layout.size() {
                let now =
                    NOW.fetch_add(new - layout.size(), Ordering::SeqCst) + new - layout.size();
                PEAK.fetch_max(now, Ordering::SeqCst);
            } else {
                NOW.fetch_sub(layout.size() - new, Ordering::SeqCst);
            }
        }
        p
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// 50 folders and 200 000 files below one root (0.3.4's `tree(200_000)`).
fn tree() -> Store {
    let store = Store::new();
    {
        let mut app = store.appender();
        let root = app.add_root_dir("/r").unwrap();
        let first = app
            .ingest_listing(root, &dirs_and_files("entry-", 50, 200_000))
            .unwrap();
        let empty = dirs_and_files("x", 0, 0);
        for d in first..first + 50 {
            app.ingest_listing(d, &empty).unwrap();
        }
        app.compact();
    }
    store
}

#[test]
fn snapshots_open_without_a_second_copy() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("s.sks");
    let store = tree();
    let opts = SaveOptions {
        roots: vec!["/r".into()],
        threads: 1,
        ..SaveOptions::default()
    };
    snapshot::save(&store.read(), &path, &opts).unwrap();
    let before = NOW.load(Ordering::SeqCst);
    PEAK.store(before, Ordering::SeqCst);
    let (loaded, _header) = snapshot::load(&path).unwrap();
    let peak = PEAK.load(Ordering::SeqCst) - before;
    let size = loaded.read().memory_bytes();
    eprintln!(
        "load peak {peak} bytes, store {size} bytes ({:.2}×), mapped: {}",
        peak as f64 / size as f64,
        loaded.read().columns_mapped()
    );
    assert!(
        (peak as f64) < 1.15 * size as f64 + (4 << 20) as f64,
        "peak {:.2} × the store ({peak} bytes for {size})",
        peak as f64 / size as f64
    );
    // and it is the same tree
    let (a, b) = (store.read(), loaded.read());
    assert_eq!(a.len(), b.len());
    for i in 0..a.len() as u32 {
        assert_eq!(a.name_bytes(i), b.name_bytes(i));
    }
    assert_eq!(b.kind(b.len() as u32 - 1), Kind::File);
}
