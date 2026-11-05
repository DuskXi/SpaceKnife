//! Frame timing on a ~1M-node disk-like store (ignored by default; run with
//! `cargo test --release -p sk-core --test view_bench -- --ignored --nocapture`).

mod view_support;

use std::time::Instant;

use sk_core::filter::{FilterContext, NameIndex, evaluate};
use sk_core::view::{RootInfo, TagStore, View, ViewContext, ViewSettings, encode_frame_into};
use sk_protocol::filter_syntax::parse;
use sk_protocol::view::FilterModeName;
use view_support::golden_tree;

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

fn time(label: &str, reps: usize, mut f: impl FnMut()) {
    f(); // warm up
    let mut t = Vec::with_capacity(reps);
    for _ in 0..reps {
        let t0 = Instant::now();
        f();
        t.push(t0.elapsed().as_secs_f64() * 1000.0);
    }
    let min = t.iter().copied().fold(f64::INFINITY, f64::min);
    println!(
        "{label:<44} median {:>8.2} ms   min {:>8.2} ms",
        median(t),
        min
    );
}

#[test]
#[ignore]
fn frame_timing_on_a_million_nodes() {
    let t0 = Instant::now();
    let store = golden_tree(1_000_000, 7, 0);
    {
        let r = store.read();
        let dirs = (0..r.len() as u32).filter(|&i| r.is_dir(i)).count();
        let pending = (0..r.len() as u32)
            .filter(|&i| r.flags(i).intersects(sk_store::Flags::PENDING))
            .count();
        println!(
            "{dirs} folders, {pending} pending, roots children {}",
            r.child_count(1)
        );
    }
    println!(
        "built {} nodes in {:.1} s",
        store.len(),
        t0.elapsed().as_secs_f64()
    );
    let used = store.read().tot_alloc(1);
    let ctx = ViewContext::new(
        store.clone(),
        vec![RootInfo {
            node: 1,
            whole_volume: true,
            free: Some(used / 3),
            used: Some(used + used / 50),
        }],
        TagStore::new(None),
    );
    let mut v = View::new("bench", ctx.clone(), ViewSettings::default()).unwrap();
    v.set_viewport(1920.0, 1080.0, 1.0);
    let mut buf = Vec::new();
    for visual in ["treemap", "flame", "flame_polar"] {
        v.set_visual(visual);
        time(
            &format!("render {visual} 1080p ({} rows)", v.render().draw.len()),
            20,
            || {
                v.render();
            },
        );
        let rows = v.last.as_ref().unwrap().draw.len();
        time(&format!("  encode {visual} ({rows} rows)"), 20, || {
            encode_frame_into(v.last.as_ref().unwrap(), "bench", 1, None, &mut buf).unwrap();
        });
    }
    v.set_visual("treemap");
    v.set_viewport(3840.0, 2160.0, 1.0);
    v.set_detail(15);
    time("render treemap 4K detail 15", 20, || {
        v.render();
    });
    v.set_viewport(1920.0, 1080.0, 1.0);
    v.set_detail(5);
    v.scanning = true;
    time("render treemap 1080p while scanning", 20, || {
        v.render();
    });
    v.scanning = false;
    // filters (the name index of the scan is shared and warm after the first run)
    let names = ctx.names.clone();
    for text in [
        "*.dll",
        "*cache*",
        "src*",
        ">1mb",
        ":class:video",
        "\\temp",
        "*.js;>100kb;<30d",
    ] {
        let nf = parse(text, None, None).normal.unwrap();
        time(&format!("filter {text:?}"), 10, || {
            let r = store.read();
            let mut c = FilterContext::current();
            c.names = Some(names.clone());
            evaluate(&r, &nf, &c);
        });
    }
    {
        let nf = parse("*cache*", None, None).normal.unwrap();
        time("filter \"*cache*\" with a cold name index", 5, || {
            let r = store.read();
            let mut c = FilterContext::current();
            c.names = Some(std::sync::Arc::new(NameIndex::new()));
            evaluate(&r, &nf, &c);
        });
    }
    v.set_filter("*.dll;*.js", None);
    time("render treemap 1080p, dim filter", 20, || {
        v.render();
    });
    v.filter_mode = FilterModeName::Hide;
    time("render treemap 1080p, hide filter", 20, || {
        v.render();
    });
    v.filter_mode = FilterModeName::Dim;
    time("refilter *.dll;*.js + render", 10, || {
        v.refilter(None);
        v.render();
    });
    v.show_trash = false;
    time("render treemap 1080p, bins hidden", 20, || {
        v.render();
    });
}

/// A disk-like tree of about `n` nodes: ~10 files and ~1.4 folders per folder, log-distributed
/// sizes, a third of the extensions shared.
/// A disk-like tree being scanned: ~10 files and ~1.4 folders per folder, log-distributed sizes.
struct Disk {
    rng: sk_store::testing::Rng,
    queue: std::collections::VecDeque<sk_store::NodeId>,
    k: u64,
}

impl Disk {
    fn new(store: &sk_store::Store, seed: u64) -> Disk {
        let root = store.appender().add_root_dir("C:\\").unwrap();
        Disk {
            rng: sk_store::testing::Rng::new(seed),
            queue: [root].into(),
            k: 0,
        }
    }

    /// List folders until the store holds `n` nodes.
    fn scan_to(&mut self, store: &sk_store::Store, n: usize) {
        use sk_store::{Entry, EntryBatch, NodeId};
        let exts = [
            "dll", "txt", "jpg", "png", "js", "json", "py", "mp4", "zip", "log", "dat", "",
        ];
        let mut app = store.appender();
        while app.len() < n {
            let Some(d) = self.queue.pop_front() else {
                break;
            };
            let rng = &mut self.rng;
            let dirs = if self.queue.len() < 8 {
                3
            } else {
                rng.below(4) as usize
            };
            let files = rng.below(20) as usize;
            let mut b = EntryBatch::new();
            for i in 0..dirs + files {
                self.k += 1;
                let k = self.k;
                if i < dirs {
                    b.push(Entry::dir(format!("dir{k}").as_bytes()));
                } else {
                    let ext = *rng.pick(&exts);
                    let bits = 4 + rng.below(12) + rng.below(12);
                    let size = rng.below(1 << bits) as i64;
                    let name = format!("file{k}.{ext}");
                    b.push(Entry {
                        ext: app.ext_id(ext),
                        ..Entry::file(name.as_bytes(), size, (size + 4095) / 4096 * 4096)
                    });
                }
            }
            let first = app.ingest_listing(d, &b).unwrap();
            self.queue.extend((0..dirs as NodeId).map(|i| first + i));
        }
    }
}

fn wide_tree(n: usize, seed: u64) -> sk_store::Store {
    let store = sk_store::Store::with_capacity(n + 64);
    Disk::new(&store, seed).scan_to(&store, n);
    store
}

#[test]
#[ignore]
fn frame_timing_during_a_scan() {
    let store = sk_store::Store::with_capacity(1 << 20);
    let mut disk = Disk::new(&store, 5);
    disk.scan_to(&store, 50_000);
    let ctx = ViewContext::new(
        store.clone(),
        vec![RootInfo::folder(1)],
        TagStore::new(None),
    );
    let mut v = View::new("scan", ctx, ViewSettings::default()).unwrap();
    v.set_viewport(1920.0, 1080.0, 1.0);
    v.scanning = true;
    v.set_filter("*.dll;*.js", None);
    let mut times = Vec::new();
    let mut n = 50_000;
    while n < 1_000_000 {
        n += 25_000; // a quarter second of a fast scan
        disk.scan_to(&store, n);
        let t0 = Instant::now();
        v.refilter(None);
        v.render();
        times.push(t0.elapsed().as_secs_f64() * 1000.0);
    }
    let last = *times.last().unwrap();
    println!(
        "scan to {} nodes, refilter + render (scanning marks, folder classes, trash) per frame: median {:.2} ms, last {:.2} ms, max {:.2} ms",
        store.len(),
        median(times.clone()),
        last,
        times.iter().copied().fold(0.0, f64::max)
    );
    v.set_filter("", None);
    let mut plain = Vec::new();
    while n < 1_400_000 {
        n += 25_000;
        disk.scan_to(&store, n);
        let t0 = Instant::now();
        v.render();
        plain.push(t0.elapsed().as_secs_f64() * 1000.0);
    }
    println!(
        "scan to {} nodes, render only per frame: median {:.2} ms, max {:.2} ms",
        store.len(),
        median(plain.clone()),
        plain.iter().copied().fold(0.0, f64::max)
    );
}

#[test]
#[ignore]
fn frame_timing_on_a_wide_million_node_tree() {
    let t0 = Instant::now();
    let store = wide_tree(1_000_000, 11);
    println!(
        "built {} nodes in {:.2} s",
        store.len(),
        t0.elapsed().as_secs_f64()
    );
    let ctx = ViewContext::new(
        store.clone(),
        vec![RootInfo::folder(1)],
        TagStore::new(None),
    );
    let mut v = View::new("bench", ctx.clone(), ViewSettings::default()).unwrap();
    let mut buf = Vec::new();
    for (w, h, detail) in [(1920.0, 1080.0, 5), (3840.0, 2160.0, 15)] {
        v.set_viewport(w, h, 1.0);
        v.set_detail(detail);
        for visual in ["treemap", "flame", "flame_polar"] {
            v.set_visual(visual);
            let rows = v.render().draw.len();
            time(
                &format!("render {visual} {w}x{h} d{detail} ({rows} rows)"),
                10,
                || {
                    v.render();
                },
            );
            time("  layout only", 10, || {
                let r = store.read();
                let vis = sk_core::layout::registry::get(visual).unwrap();
                let mut out = sk_core::layout::DrawList::new();
                vis.engine.run(
                    &r,
                    &ctx.basis,
                    &v.params(),
                    &sk_core::layout::RootSpec::node(1),
                    &sk_core::layout::StoreSizes::of(&r),
                    &mut out,
                );
            });
            time("  encode", 10, || {
                encode_frame_into(v.last.as_ref().unwrap(), "bench", 1, None, &mut buf).unwrap();
            });
        }
    }
    v.set_visual("treemap");
    v.set_viewport(1920.0, 1080.0, 1.0);
    v.set_detail(5);
    v.scanning = true;
    time("render treemap 1080p, scanning flag on", 10, || {
        v.render();
    });
    v.scanning = false;
    v.set_filter("*.dll;*.js", None);
    time("refilter *.dll;*.js", 10, || v.refilter(None));
    time("render treemap 1080p, dim filter", 10, || {
        v.render();
    });
    v.filter_mode = FilterModeName::Hide;
    time("render treemap 1080p, hide filter", 10, || {
        v.render();
    });
}
