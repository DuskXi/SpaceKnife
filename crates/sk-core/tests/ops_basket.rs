//! ops::basket (port of tests/test_ops_basket.py): path containment, small-item groups,
//! freeable space (cloud, hard links), refresh. Synthetic stores: no file is created or touched.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Instant;

use sk_core::ops::basket::{AddResult, Basket, BasketItem, Merged, default_volume_of};
use sk_core::ops::trash::{OpCode, OpResult};
use sk_store::testing::{Spec, build_with, d, f, find};
use sk_store::{Col, Flags, NodeId, Store};
use sk_protocol::ops::BasketItemStatus as St;
use sk_protocol::compare::ChangeKind;

const KB4: i64 = 4096;

fn spec() -> Spec {
    let mut small: Vec<(String, Spec)> = (0..6).map(|i| (format!("s{i}.log"), f(10 + i))).collect();
    small.push(("big.iso".into(), f(100_000)));
    let small: Vec<(&str, Spec)> = small.iter().map(|(n, s)| (n.as_str(), s.clone())).collect();
    d([
        ("docs", d([("a.txt", f(1000)), ("b.txt", f(2000)), ("sub", d([("c.bin", f(5000))]))])),
        ("small", d(small)),
        ("links", d([("x.dat", f(8192))])),
        ("other", d([("y.dat", f(8192))])),
        ("cloud", d([("online.docx", f(50_000)), ("local.docx", f(3000))])),
    ])
}

/// A scan root below a temporary folder (never created: nothing is touched).
fn root() -> String {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| std::env::temp_dir().join("sk-basket-test").join("scan"))
        .to_string_lossy()
        .into_owned()
}

fn store() -> Store {
    build_with(&spec(), &root(), KB4, &[])
}

fn basket() -> Basket {
    Basket::new(Some(Box::new(|_: &str| "VOL".to_string())))
}

fn path_of(rel: &str) -> String {
    let mut p = PathBuf::from(root());
    for part in rel.split('/') {
        p.push(part);
    }
    p.to_string_lossy().into_owned()
}

fn node(store: &Store, rel: &str) -> i64 {
    find(&store.read(), rel) as i64
}

fn add(b: &Basket, store: &Store, scan: &str, nodes: &[i64]) -> AddResult {
    b.add(&store.read(), scan, nodes, None).unwrap()
}

fn add_group(b: &Basket, store: &Store, folder: i64, members: &[i64]) -> AddResult {
    b.add(&store.read(), "s", &[folder], Some(members)).unwrap()
}

fn link(store: &Store, rel: &str, ino: u64, nlink: u16, dup: bool) -> i64 {
    let n = node(store, rel) as NodeId;
    let mut w = store.write();
    w.set_raw::<u64>(Col::Ino, n, ino);
    w.set_raw::<u16>(Col::Nlink, n, nlink);
    if dup {
        w.mark(n, Flags::HARDLINK_DUP);
    }
    n as i64
}

fn reaggregate(store: &Store) {
    store.write().reaggregate();
}

fn merged(path: &str, into: &str) -> Merged {
    Merged { path: path.to_string(), into: into.to_string() }
}

fn by_id(b: &Basket) -> HashMap<String, BasketItem> {
    b.items().into_iter().map(|i| (i.id.clone(), i)).collect()
}

fn group_name(folder: &str, count: usize) -> String {
    format!("{folder} 中 {count} 个小项")
}

fn small_members(store: &Store, idx: &[usize]) -> Vec<i64> {
    idx.iter().map(|i| node(store, &format!("small/s{i}.log"))).collect()
}

// --------------------------------------------------------------------------- containment

#[test]
fn add_a_file() {
    let store = store();
    let b = basket();
    let res = add(&b, &store, "scan1", &[node(&store, "docs/a.txt")]);
    assert_eq!(res.added.len(), 1);
    assert!(res.merged.is_empty() && res.refused.is_empty());
    let items = b.items();
    let item = &items[0];
    assert_eq!(item.id, res.added[0]);
    assert_eq!(
        (item.path.as_str(), item.name.as_str(), item.kind, item.scan_id.as_str()),
        (path_of("docs/a.txt").as_str(), "a.txt", ChangeKind::File, "scan1")
    );
    assert_eq!((item.size, item.alloc, item.files, item.dirs), (1000, KB4, 1, 0));
    assert_eq!((item.volume.as_str(), item.status, item.note.as_str()), ("VOL", St::Ready, ""));
    assert!(item.members.is_empty());
    assert_eq!(b.contains(&item.path), Some(item.id.clone()));
    assert_eq!(b.contains(&path_of("docs")), None);
    let again = add(&basket(), &store, "scan1", &[node(&store, "docs/a.txt")]);
    assert_eq!(again.added, std::slice::from_ref(&item.id)); // a hash of the path: stable across baskets
}

#[test]
fn same_path_twice_is_merged() {
    let store = store();
    let b = basket();
    let first = add(&b, &store, "s", &[node(&store, "docs/a.txt")]).added[0].clone();
    let res = add(&b, &store, "s", &[node(&store, "docs/a.txt")]);
    assert!(res.added.is_empty());
    assert_eq!(res.merged, [merged(&path_of("docs/a.txt"), &first)]);
    assert_eq!(b.len(), 1);
}

#[test]
fn child_of_an_item_is_merged() {
    let store = store();
    let b = basket();
    let docs = add(&b, &store, "s", &[node(&store, "docs")]).added[0].clone();
    let res = add(&b, &store, "s", &[node(&store, "docs/sub/c.bin"), node(&store, "docs/b.txt")]);
    assert!(res.added.is_empty());
    assert_eq!(
        res.merged,
        [merged(&path_of("docs/sub/c.bin"), &docs), merged(&path_of("docs/b.txt"), &docs)]
    );
    assert_eq!(b.contains(&path_of("docs/sub/c.bin")), Some(docs));
}

#[test]
fn ancestor_absorbs_items() {
    let store = store();
    let b = basket();
    let mut first = add(&b, &store, "s", &[node(&store, "docs/a.txt"), node(&store, "docs/sub/c.bin")]).added;
    assert_eq!(first.len(), 2);
    let res = add(&b, &store, "s", &[node(&store, "docs")]);
    let mut absorbed = res.absorbed.clone();
    absorbed.sort();
    first.sort();
    assert_eq!(absorbed, first);
    let items = b.items();
    assert_eq!(items.len(), 1);
    let item = &items[0];
    assert_eq!((item.id.as_str(), item.kind, item.name.as_str()), (res.added[0].as_str(), ChangeKind::Dir, "docs"));
    assert_eq!((item.size, item.alloc, item.files, item.dirs), (8000, 4 * KB4, 3, 1));
    assert_eq!(b.freeable(), HashMap::from([("VOL".to_string(), 4 * KB4)]));
}

#[test]
fn child_and_parent_in_one_call() {
    let store = store();
    let b = basket();
    let res = add(
        &b,
        &store,
        "s",
        &[node(&store, "docs/sub/c.bin"), node(&store, "docs/sub"), node(&store, "docs")],
    );
    assert_eq!(res.added.len(), 1);
    let docs = res.added[0].clone();
    assert!(res.absorbed.is_empty()); // added and absorbed in the same call: reported as merged
    let mut paths: Vec<String> = res.merged.iter().map(|m| m.path.clone()).collect();
    paths.sort();
    let mut want = vec![path_of("docs/sub/c.bin"), path_of("docs/sub")];
    want.sort();
    assert_eq!(paths, want);
    assert!(res.merged.iter().all(|m| m.into == docs));
    assert_eq!(b.items().iter().map(|i| i.id.clone()).collect::<Vec<_>>(), [docs]);
}

#[test]
fn refusals() {
    let store = store();
    let b = basket();
    let n = store.read().len() as i64;
    let res = add(&b, &store, "s", &[0, n + 5, -1]);
    assert!(res.refused.iter().all(|r| r.reason == "扫描结果中没有这一项") && res.refused.len() == 3);
    let cloud = node(&store, "cloud") as NodeId;
    store.write().mark(cloud, Flags::VIRTUAL); // an MFT pseudo folder
    let res = add(&b, &store, "s", &[node(&store, "cloud/local.docx")]);
    assert_eq!(res.refused.len(), 1);
    assert_eq!(res.refused[0].reason, "MFT 伪文件夹中的项不在磁盘上");
    assert_eq!(b.len(), 0);
}

#[test]
fn protected_items_are_refused() {
    // everything under SpaceKnife's own data folder
    let data = sk_core::paths::data_dir();
    let own = build_with(&d([("snapshots", d([("c.sks", f(10))]))]), &data.to_string_lossy(), 1, &[]);
    let b = basket();
    let r = own.read().roots().next().unwrap() as i64;
    let res = b.add(&own.read(), "own", &[node(&own, "snapshots/c.sks"), r], None).unwrap();
    assert_eq!(res.refused.len(), 2);
    assert!(res.refused.iter().all(|x| x.reason == "SpaceKnife 自己的数据目录"));
    assert_eq!(b.len(), 0);
}

#[test]
fn gone_rows_are_refused() {
    let store = store();
    let n = node(&store, "docs/a.txt");
    store.write().mark(n as NodeId, Flags::GONE);
    let res = add(&basket(), &store, "s", &[n]);
    assert_eq!(res.refused.len(), 1);
    assert_eq!(res.refused[0].reason, "扫描结果中没有这一项");
}

// --------------------------------------------------------------------------- small-item groups

#[test]
fn group_is_one_item() {
    let store = store();
    let b = basket();
    let folder = node(&store, "small");
    let res = add_group(&b, &store, folder, &small_members(&store, &[0, 1, 2, 3, 4, 5]));
    let gid = res.added[0].clone();
    let items = b.items();
    let item = &items[0];
    assert_eq!((item.id.as_str(), item.kind, item.path.as_str()), (gid.as_str(), ChangeKind::Group, path_of("small").as_str()));
    assert_eq!(item.name, group_name("small", 6));
    let want: Vec<String> = (0..6).map(|i| path_of(&format!("small/s{i}.log"))).collect();
    assert_eq!(item.members, want);
    assert_eq!((item.size, item.alloc, item.files, item.dirs), ((10..16).sum::<i64>(), 6 * KB4, 6, 0));
    let res = add(&b, &store, "s", &[node(&store, "small/s3.log")]);
    assert_eq!(res.merged, [merged(&path_of("small/s3.log"), &gid)]);
    let res = add(&b, &store, "s", &[node(&store, "small/big.iso")]);
    assert_eq!(res.added.len(), 1);
    assert_eq!(b.len(), 2);
    assert_eq!(b.contains(&path_of("small")), None); // the group covers its members only
}

#[test]
fn group_grows_and_folder_absorbs_it() {
    let store = store();
    let b = basket();
    let folder = node(&store, "small");
    let gid = add_group(&b, &store, folder, &small_members(&store, &[0, 1, 2])).added[0].clone();
    let res = add_group(&b, &store, folder, &small_members(&store, &[2, 3]));
    assert_eq!(res.added, std::slice::from_ref(&gid)); // the same group, grown
    assert_eq!(res.merged, [merged(&path_of("small/s2.log"), &gid)]);
    let items = b.items();
    assert_eq!((items[0].members.len(), items[0].alloc), (4, 4 * KB4));
    assert_eq!(items[0].name, group_name("small", 4));
    let big = add(&b, &store, "s", &[node(&store, "small/big.iso")]).added[0].clone();
    let res = add(&b, &store, "s", &[folder]);
    let mut absorbed = res.absorbed.clone();
    absorbed.sort();
    let mut want = vec![gid, big];
    want.sort();
    assert_eq!(absorbed, want);
    let items = b.items();
    assert_eq!((items[0].kind, items[0].alloc), (ChangeKind::Dir, 6 * KB4 + 102_400));
}

#[test]
fn group_member_rules() {
    let store = store();
    let b = basket();
    let folder = node(&store, "small");
    let mut members = vec![node(&store, "docs/a.txt")];
    members.extend(small_members(&store, &[0]));
    let res = add_group(&b, &store, folder, &members);
    assert_eq!(res.refused.len(), 1);
    assert_eq!(res.refused[0].reason, "不是所在文件夹的直接子项");
    assert_eq!(b.items()[0].members.len(), 1);
    assert!(b.add(&store.read(), "s", &[folder, folder], Some(&small_members(&store, &[1]))).is_err());
    let res = add_group(&b, &store, node(&store, "small/s5.log"), &[]);
    assert_eq!(res.refused[0].reason, "小项聚合块的所在位置不是文件夹");
    // a group inside a folder that is already in the basket is merged as a whole
    let docs = add(&b, &store, "s", &[node(&store, "docs")]).added[0].clone();
    let res = add_group(&b, &store, node(&store, "docs/sub"), &[node(&store, "docs/sub/c.bin")]);
    assert_eq!(res.merged, [merged(&path_of("docs/sub"), &docs)]);
}

#[test]
fn targets_expand_groups() {
    let store = store();
    let b = basket();
    let gid = add_group(&b, &store, node(&store, "small"), &small_members(&store, &[0, 1])).added[0].clone();
    let fid = add(&b, &store, "s", &[node(&store, "docs/b.txt")]).added[0].clone();
    let targets = b.targets(None);
    let got: Vec<(String, String, i64)> = targets.iter().map(|t| (t.item.clone(), t.path.clone(), t.size)).collect();
    assert_eq!(
        got,
        [
            (gid.clone(), path_of("small/s0.log"), KB4),
            (gid, path_of("small/s1.log"), KB4),
            (fid.clone(), path_of("docs/b.txt"), KB4),
        ]
    );
    let only: Vec<String> = b.targets(Some(&[fid])).into_iter().map(|t| t.path).collect();
    assert_eq!(only, [path_of("docs/b.txt")]);
    assert_eq!(b.lock_targets(), targets.iter().map(|t| t.path.clone()).collect::<Vec<_>>());
}

// --------------------------------------------------------------------------- freeable (§2.3)

fn vol(n: i64) -> HashMap<String, i64> {
    HashMap::from([("VOL".to_string(), n)])
}

#[test]
fn hard_links_count_only_when_all_names_are_in() {
    let store = store();
    let x = link(&store, "links/x.dat", 77, 2, false);
    let y = link(&store, "other/y.dat", 77, 2, true);
    reaggregate(&store);
    let b = basket();
    let xid = add(&b, &store, "s", &[x]).added[0].clone();
    let item = b.get(&xid).unwrap();
    assert_eq!(item.alloc, 0);
    assert_eq!(item.note, "另有 1 个硬链接不在删除篮中，删除后不释放空间");
    assert_eq!(b.freeable(), vol(0));
    let yid = add(&b, &store, "s", &[y]).added[0].clone();
    let got = by_id(&b);
    assert_eq!((got[&xid].alloc, got[&yid].alloc), (8192, 0)); // counted once, for the first item
    assert!(got[&xid].note.is_empty() && got[&yid].note.is_empty());
    assert_eq!(b.freeable(), vol(8192));
    b.remove(std::slice::from_ref(&xid));
    let item = b.get(&yid).unwrap();
    assert_eq!(item.alloc, 0);
    assert_eq!(item.note, "另有 1 个硬链接不在删除篮中，删除后不释放空间");
}

#[test]
fn hard_links_inside_folders() {
    let store = store();
    link(&store, "links/x.dat", 5, 2, false);
    link(&store, "other/y.dat", 5, 2, true);
    reaggregate(&store);
    let b = basket();
    let lid = add(&b, &store, "s", &[node(&store, "links")]).added[0].clone();
    let item = b.get(&lid).unwrap();
    assert_eq!(item.alloc, 0);
    assert_eq!(item.note, "其中 1 个文件在别处还有硬链接，删除后不释放这部分空间");
    add(&b, &store, "s", &[node(&store, "other")]);
    assert_eq!(b.freeable(), vol(8192));
    assert!(b.items().iter().all(|i| i.note.is_empty()));
}

#[test]
fn hard_link_with_a_name_outside_the_scan() {
    let store = store();
    let x = link(&store, "links/x.dat", 9, 3, false);
    let y = link(&store, "other/y.dat", 9, 3, true);
    reaggregate(&store);
    let b = basket();
    add(&b, &store, "s", &[x, y]);
    assert_eq!(b.freeable(), vol(0)); // the third name keeps the data alive
    assert!(b.items().iter().all(|i| i.note == "另有 1 个硬链接不在删除篮中，删除后不释放空间"));
}

#[test]
fn cloud_placeholders_free_nothing() {
    let store = store();
    let online = node(&store, "cloud/online.docx");
    store.write().mark(online as NodeId, Flags::CLOUD); // its alloc column is still non-zero here
    let b = basket();
    let oid = add(&b, &store, "s", &[online]).added[0].clone();
    let item = b.get(&oid).unwrap();
    assert_eq!((item.alloc, item.size), (0, 50_000));
    add(&b, &store, "s", &[node(&store, "cloud")]);
    let items = b.items();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].alloc, KB4); // local.docx only
}

#[test]
fn freeable_per_volume() {
    let base = std::env::temp_dir().join("sk-basket-volumes");
    let (one, two) = (base.join("one"), base.join("two"));
    let s1 = build_with(&d([("f.bin", f(10_000))]), &one.to_string_lossy(), KB4, &[]);
    let s2 = build_with(&d([("g.bin", f(5000))]), &two.to_string_lossy(), KB4, &[]);
    let volumes: HashMap<String, String> = HashMap::from([
        (one.to_string_lossy().into_owned(), "C:\\".to_string()),
        (two.to_string_lossy().into_owned(), "D:\\".to_string()),
    ]);
    let b = Basket::new(Some(Box::new(move |r: &str| volumes[r].clone())));
    add(&b, &s1, "a", &[node(&s1, "f.bin")]);
    add(&b, &s2, "b", &[node(&s2, "g.bin")]);
    assert_eq!(
        b.freeable(),
        HashMap::from([("C:\\".to_string(), 12_288), ("D:\\".to_string(), 8192)])
    );
    assert_eq!(b.items().iter().map(|i| i.volume.clone()).collect::<Vec<_>>(), ["C:\\", "D:\\"]);
}

#[test]
fn default_volume_of_a_real_folder() {
    let dir = tempfile::tempdir().unwrap();
    let real = sk_core::volume::volume_for(dir.path()).unwrap().mount_point;
    assert_eq!(default_volume_of(&dir.path().to_string_lossy()), real);
    assert!(!default_volume_of(&dir.path().join("not").join("there").to_string_lossy()).is_empty());
}

// --------------------------------------------------------------------------- refresh

#[test]
fn refresh_after_tree_changes() {
    let store = store();
    let b = basket();
    let aid = add(&b, &store, "s", &[node(&store, "docs/a.txt")]).added[0].clone();
    let sid = add(&b, &store, "s", &[node(&store, "docs/sub")]).added[0].clone();
    let gid = add_group(&b, &store, node(&store, "small"), &small_members(&store, &[0, 1])).added[0].clone();
    let changed = d([
        ("docs", d([("a.txt", f(10_000)), ("b.txt", f(2000))])), // a grew, sub is gone
        ("small", d([("s0.log", f(10)), ("big.iso", f(100_000))])), // s1 is gone
        ("links", d([("x.dat", f(8192))])),
        ("other", d([("y.dat", f(8192))])),
        ("cloud", d([("online.docx", f(50_000)), ("local.docx", f(3000))])),
    ]);
    let newer = build_with(&changed, &root(), KB4, &[]);
    let current: std::cell::RefCell<Option<Store>> = std::cell::RefCell::new(Some(newer));
    let mut of = |_: &str| current.borrow().clone();
    assert_eq!(b.refresh(&mut of), std::slice::from_ref(&sid));
    let got = by_id(&b);
    assert_eq!((got[&aid].size, got[&aid].alloc, got[&aid].status), (10_000, 12_288, St::Ready));
    assert_eq!((got[&sid].status, got[&sid].alloc, got[&sid].note.as_str()), (St::Missing, 0, "已不存在"));
    assert_eq!((got[&gid].status, got[&gid].alloc), (St::Ready, KB4));
    assert_eq!(got[&gid].note, "其中 1 项已不存在");
    assert_eq!(got[&gid].name, group_name("small", 1));
    assert_eq!(b.freeable(), vol(12_288 + KB4));
    assert!(b.targets(Some(std::slice::from_ref(&sid))).is_empty());
    assert!(b.refresh(&mut of).is_empty()); // reported once
    assert!(b.refresh(&mut |_: &str| None).is_empty()); // the scan was closed: what is known stays
    assert_eq!(b.get(&aid).unwrap(), got[&aid]);
    *current.borrow_mut() = Some(store.clone()); // it came back
    let mut of = |_: &str| current.borrow().clone();
    assert!(b.refresh(&mut of).is_empty());
    let item = b.get(&sid).unwrap();
    assert_eq!((item.status, item.alloc), (St::Ready, 2 * KB4));
}

#[test]
fn refresh_skips_unchanged_items() {
    let store = store();
    let b = basket();
    add(&b, &store, "s", &[node(&store, "docs"), node(&store, "small/big.iso")]);
    let v0 = b.items();
    b.refresh(&mut |_: &str| Some(store.clone()));
    assert_eq!(b.items(), v0);
    let big = node(&store, "small/big.iso") as NodeId;
    store.write().set_raw::<i64>(Col::Size, big, 1); // in place, as update_stats does
    b.refresh(&mut |_: &str| Some(store.clone()));
    let items = b.items();
    assert_eq!(items[1].size, 1); // measured again
    assert_eq!(items[0], v0[0]);
}

// --------------------------------------------------------------------------- locks, execution, undo

#[test]
fn lockers_mark_items() {
    let store = store();
    let b = basket();
    let aid = add(&b, &store, "s", &[node(&store, "docs/a.txt")]).added[0].clone();
    let gid = add_group(&b, &store, node(&store, "small"), &small_members(&store, &[0, 1])).added[0].clone();
    let before = b.freeable();
    b.set_lockers(&HashMap::from([
        (path_of("docs/a.txt"), vec!["Word".to_string()]),
        (path_of("small/s1.log"), vec!["Excel".to_string(), "Word".to_string()]),
    ]));
    let got = by_id(&b);
    assert_eq!((got[&aid].status, got[&aid].note.as_str()), (St::Locked, "正在被 Word 使用，需先退出"));
    assert_eq!((got[&gid].status, got[&gid].note.as_str()), (St::Locked, "正在被 Excel、Word 使用，需先退出"));
    assert_eq!(b.freeable(), before); // held items still count (UI-W5-31)
    b.set_lockers(&HashMap::new());
    assert!(b.items().iter().all(|i| i.status == St::Ready && i.note.is_empty()));
}

fn op(path: &str, code: OpCode, message: &str, r: &str) -> OpResult {
    OpResult::new(path, code, message, r)
}

#[test]
fn apply_results_and_reinstate() {
    let store = store();
    let b = basket();
    let aid = add(&b, &store, "s", &[node(&store, "docs/a.txt")]).added[0].clone();
    let bid = add(&b, &store, "s", &[node(&store, "docs/b.txt")]).added[0].clone();
    let gid = add_group(&b, &store, node(&store, "small"), &small_members(&store, &[0, 1, 2])).added[0].clone();
    let results = [
        op(&path_of("docs/a.txt"), OpCode::Ok, "已移到回收站", "ref-a"),
        op(&path_of("docs/b.txt"), OpCode::Denied, "没有权限", ""),
        op(&path_of("small/s0.log"), OpCode::Ok, "已移到回收站", "ref-0"),
        op(&path_of("small/s1.log"), OpCode::Ok, "已移到回收站", "ref-1"),
        op(&path_of("small/s2.log"), OpCode::Cancelled, "已取消", ""),
    ];
    let left = b.apply_results(&results);
    let got: Vec<(String, Vec<String>)> = left.iter().map(|i| (i.id.clone(), i.members.clone())).collect();
    assert_eq!(
        got,
        [(aid.clone(), vec![]), (gid.clone(), vec![path_of("small/s0.log"), path_of("small/s1.log")])]
    );
    let got = by_id(&b);
    let mut ids: Vec<&String> = got.keys().collect();
    ids.sort();
    let mut want = vec![&bid, &gid];
    want.sort();
    assert_eq!(ids, want);
    assert_eq!((got[&bid].note.as_str(), got[&bid].status), ("没有权限", St::Ready));
    assert_eq!(got[&gid].members, [path_of("small/s2.log")]);
    assert_eq!(got[&gid].alloc, KB4);
    assert_eq!(got[&gid].note, "1 项未完成：已取消");
    // undo: the items come back (numbers exact again after a refresh)
    let res = b.reinstate(&left);
    let mut added = res.added.clone();
    added.sort();
    let mut want = vec![aid.clone(), gid.clone()];
    want.sort();
    assert_eq!(added, want);
    b.refresh(&mut |_: &str| Some(store.clone()));
    let got = by_id(&b);
    assert_eq!((got[&gid].members.len(), got[&gid].alloc), (3, 3 * KB4));
    assert_eq!((got[&aid].alloc, got[&aid].status), (KB4, St::Ready));
}

#[test]
fn clear_and_remove() {
    let store = store();
    let b = basket();
    let ids = add(&b, &store, "s", &[node(&store, "docs/a.txt"), node(&store, "links")]).added;
    let v = b.version();
    b.remove(&[ids[0].clone(), "no-such-id".to_string()]);
    assert_eq!(b.items().iter().map(|i| i.id.clone()).collect::<Vec<_>>(), [ids[1].clone()]);
    assert!(b.version() > v);
    b.clear();
    assert!(b.is_empty() && b.freeable().is_empty() && b.contains(&path_of("links")).is_none());
}

// --------------------------------------------------------------------------- scale (synthetic)

#[test]
fn many_items_stay_fast() {
    let n = 5000;
    let names: Vec<String> = (0..n).map(|i| format!("f{i:05}.dat")).collect();
    let bulk: Vec<(&str, Spec)> = names.iter().map(|s| (s.as_str(), f(100))).collect();
    let big_root = std::env::temp_dir().join("sk-basket-big");
    let big = build_with(&d([("bulk", d(bulk))]), &big_root.to_string_lossy(), 1, &[]);
    let folder = node(&big, "bulk");
    let files: Vec<i64> = big.read().children(folder as NodeId).map(|c| c as i64).collect();
    let b = basket();
    let started = Instant::now();
    assert_eq!(add(&b, &big, "s", &files).added.len(), n);
    let res = add(&b, &big, "s", &[folder]);
    assert_eq!((res.absorbed.len(), b.len()), (n, 1));
    b.clear();
    let gid = b.add(&big.read(), "s", &[folder], Some(&files)).unwrap().added[0].clone();
    assert_eq!(b.targets(None).len(), n);
    let done: Vec<OpResult> = b.lock_targets()[..n / 2]
        .iter()
        .map(|p| OpResult::new(p, OpCode::Ok, "", ""))
        .collect();
    let left = b.apply_results(&done);
    assert_eq!(left[0].members.len(), n / 2);
    assert_eq!(b.get(&gid).unwrap().members.len(), n - n / 2);
    assert!(started.elapsed().as_secs() < 20);
    let _ = Path::new("");
}
