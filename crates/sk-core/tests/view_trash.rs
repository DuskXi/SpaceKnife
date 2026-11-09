//! Port of the rest of 0.3.4's `tests/test_trash_view.py` (the path rules, lookalikes and the
//! shared trash of a scanned mount are in view_state.rs `trash_paths`): between full searches the
//! bins found last are looked up again by path. 0.3.4 counted the searches with a spy; here a bin
//! that appears is only found by the next full search, after [`FULL_SEARCH`].

use sk_core::view::trash::{FULL_SEARCH, TrashCache};
use sk_store::testing::{build_with, d, f, relist_from_spec};

#[test]
fn the_bins_are_looked_up_again_by_path_between_full_searches() {
    let before = d([(".Trashes", d([("x", f(1))])), ("data", d([("y", f(1))]))]);
    let store = build_with(&before, "/", 1, &[]);
    let cache = TrashCache::new();
    let first = cache.find_bins(&store.read());
    assert_eq!(first.len(), 1);
    let root = store.read().roots().next().unwrap();
    // a change (monitoring) elsewhere: the known bin is found again by its path
    let changed = d([
        (".Trashes", d([("x", f(1))])),
        ("data", d([("y", f(1))])),
        ("more", d([("z", f(1))])),
    ]);
    {
        let mut w = store.write();
        relist_from_spec(&mut w, root, &changed);
    }
    let again = cache.find_bins(&store.read());
    assert_eq!(again.len(), 1);
    assert!(store.read().path(again[0]).ends_with(".Trashes"));
    assert_eq!(store.read().name(again[0]), ".Trashes");
    // a bin that appears within FULL_SEARCH is not looked for yet ...
    let plain = d([("data", d([("y", f(1))]))]);
    let store = build_with(&plain, "/", 1, &[]);
    let cache = TrashCache::new();
    assert!(cache.find_bins(&store.read()).is_empty());
    let root = store.read().roots().next().unwrap();
    let with_bin = d([("data", d([("y", f(1))])), (".Trash", d([("z", f(1))]))]);
    {
        let mut w = store.write();
        relist_from_spec(&mut w, root, &with_bin);
    }
    assert!(cache.find_bins(&store.read()).is_empty());
    // ... the next change after FULL_SEARCH searches the whole store again and finds it
    std::thread::sleep(FULL_SEARCH + std::time::Duration::from_millis(200));
    let later = d([
        ("data", d([("y", f(1)), ("y2", f(1))])),
        (".Trash", d([("z", f(1))])),
    ]);
    let data = {
        let r = store.read();
        r.children(root).find(|&k| r.name(k) == "data").unwrap()
    };
    {
        let mut w = store.write();
        relist_from_spec(&mut w, data, later.get("data").unwrap());
    }
    let found = cache.find_bins(&store.read());
    assert_eq!(found.len(), 1);
    assert!(store.read().name(found[0]).ends_with(".Trash"));
}
