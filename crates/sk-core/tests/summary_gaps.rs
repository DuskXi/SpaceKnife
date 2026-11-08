//! tree.summary when a part of it cannot be worked out (port of backend/tests/test_summary_gaps.py,
//! the parts that live in sk-core: the walk and its fallbacks; the handler glue is sk-server's).

use std::sync::Arc;

use sk_core::classes::ClassTable;
use sk_core::filter::classes::{ClassMap, FolderClasses};
use sk_core::layout::basis::Basis;
use sk_core::summary::{Gaps, Walk, summarize_node};
use sk_protocol::scan::SummaryPart;
use sk_store::testing::{build_with, d, f, find};
use sk_store::{Col, Nodes, Store};

fn tree() -> Store {
    let spec = d([
        (
            "usr",
            d([
                (
                    "share",
                    d([(
                        "fonts",
                        d([("TTF", d([("big.ttf", f(7000)), ("small.ttf", f(70))]))]),
                    )]),
                ),
                ("lib", d([("libc.so", f(900))])),
            ]),
        ),
        (
            "var",
            d([(
                "cache",
                d([("pacman", d([("pkg", d([("linux-6.17.zst", f(5000))]))]))]),
            )]),
        ),
        ("notes.txt", f(30)),
    ]);
    build_with(&spec, "/", 1, &[])
}

/// Folder classes whose column cannot be worked out (as v0.3.3's came out too short).
struct Broken;

impl FolderClasses for Broken {
    fn column(&self, _: &Nodes, _: &ClassMap) -> Option<Arc<Vec<u8>>> {
        panic!("index 213288 is out of bounds for axis 0 with size 213161")
    }
}

fn basis(folder: Option<Arc<dyn FolderClasses>>) -> Basis {
    let b = Basis::new(Arc::new(ClassMap::from_table(ClassTable::builtin())));
    b.set_folder_classes(folder);
    b
}

#[test]
fn a_complete_walk_has_no_gaps() {
    let store = tree();
    let r = store.read();
    let walk = summarize_node(&basis(None), &r, 0, 5);
    let Walk::Full(s) = &walk else {
        panic!("{walk:?}")
    };
    assert_eq!(s.class_files.iter().sum::<i64>(), 5);
    let mut gaps = Gaps::new("scan-full", "/");
    walk.note(&mut gaps);
    assert!(gaps.is_empty());
}

#[test]
fn without_the_folder_classes_classes_are_counted_by_extension() {
    let store = tree();
    let r = store.read();
    let walk = summarize_node(&basis(Some(Arc::new(Broken))), &r, 0, 5);
    let Walk::ByExtension(s, failure) = &walk else {
        panic!("{walk:?}")
    };
    assert_eq!(s.class_files.iter().sum::<i64>(), 5);
    assert_eq!(r.name(s.top_files[0]), "big.ttf");
    assert!(s.class_parts.is_none());
    assert!(failure.error.contains("out of bounds"));
    let mut gaps = Gaps::new("scan-ext", "/");
    walk.note(&mut gaps);
    assert_eq!(gaps.items().len(), 1);
    assert_eq!(gaps.items()[0].part, SummaryPart::Classes);
    assert!(gaps.items()[0].reason.contains("out of bounds"));
}

#[test]
fn without_the_walk_the_summary_says_why() {
    let store = tree();
    {
        // a folder whose range points back at the top: the walk revisits nodes and gives up
        let r = store.read();
        let ttf = find(&r, "usr/share/fonts/TTF");
        let (start, count) = (r.child_range(0).0, r.child_range(0).1 - r.child_range(0).0);
        drop(r);
        let mut w = store.write();
        w.set_raw::<i32>(Col::ChildCount, ttf, count as i32);
        w.set_raw::<u32>(Col::ChildStart, ttf, start);
        w.touch();
    }
    let r = store.read();
    let walk = summarize_node(&basis(None), &r, 0, 5);
    let Walk::Failed(failure) = &walk else {
        panic!("{walk:?}")
    };
    let mut gaps = Gaps::new("scan-walk", "/");
    walk.note(&mut gaps);
    assert_eq!(gaps.items()[0].part, SummaryPart::Contents);
    assert!(gaps.items()[0].reason.contains(&failure.error));
    assert!(walk.summary().is_none());
}
