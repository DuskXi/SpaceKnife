//! Shared helpers of the view task's tests (filter, layout, view): Python's `random.Random`, small
//! stores from nested specs, fingerprints.
#![allow(dead_code)]

use sk_store::testing::ext_of;
use sk_store::{Entry, EntryBatch, Flags, Kind, NodeId, Nodes, Store, Times};

// --------------------------------------------------------------------------- Python's random

/// CPython's `random.Random` (MT19937 and the integer helpers built on `getrandbits`), so fixture
/// trees recorded by the Python tests are rebuilt node for node.
pub struct PyRandom {
    mt: [u32; 624],
    i: usize,
}

impl PyRandom {
    pub fn new(seed: u64) -> PyRandom {
        let mut key = Vec::new();
        let mut s = seed;
        while s > 0 {
            key.push(s as u32);
            s >>= 32;
        }
        if key.is_empty() {
            key.push(0);
        }
        let mut r = PyRandom {
            mt: [0; 624],
            i: 624,
        };
        r.init_genrand(19_650_218);
        let (mut i, mut j) = (1usize, 0usize);
        for _ in 0..624.max(key.len()) {
            let prev = r.mt[i - 1] ^ (r.mt[i - 1] >> 30);
            r.mt[i] = (r.mt[i] ^ prev.wrapping_mul(1_664_525))
                .wrapping_add(key[j])
                .wrapping_add(j as u32);
            i += 1;
            j += 1;
            if i >= 624 {
                r.mt[0] = r.mt[623];
                i = 1;
            }
            if j >= key.len() {
                j = 0;
            }
        }
        for _ in 0..623 {
            let prev = r.mt[i - 1] ^ (r.mt[i - 1] >> 30);
            r.mt[i] = (r.mt[i] ^ prev.wrapping_mul(1_566_083_941)).wrapping_sub(i as u32);
            i += 1;
            if i >= 624 {
                r.mt[0] = r.mt[623];
                i = 1;
            }
        }
        r.mt[0] = 0x8000_0000;
        r.i = 624;
        r
    }

    fn init_genrand(&mut self, s: u32) {
        self.mt[0] = s;
        for i in 1..624 {
            let prev = self.mt[i - 1] ^ (self.mt[i - 1] >> 30);
            self.mt[i] = 1_812_433_253u32.wrapping_mul(prev).wrapping_add(i as u32);
        }
    }

    pub fn next_u32(&mut self) -> u32 {
        if self.i >= 624 {
            for k in 0..624 {
                let y = (self.mt[k] & 0x8000_0000) | (self.mt[(k + 1) % 624] & 0x7fff_ffff);
                let mut v = self.mt[(k + 397) % 624] ^ (y >> 1);
                if y & 1 != 0 {
                    v ^= 0x9908_b0df;
                }
                self.mt[k] = v;
            }
            self.i = 0;
        }
        let mut y = self.mt[self.i];
        self.i += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9d2c_5680;
        y ^= (y << 15) & 0xefc6_0000;
        y ^ (y >> 18)
    }

    /// `random()`
    pub fn random(&mut self) -> f64 {
        let a = (self.next_u32() >> 5) as f64;
        let b = (self.next_u32() >> 6) as f64;
        (a * 67_108_864.0 + b) * (1.0 / 9_007_199_254_740_992.0)
    }

    pub fn getrandbits(&mut self, k: u32) -> u64 {
        if k == 0 {
            return 0;
        }
        if k <= 32 {
            return (self.next_u32() >> (32 - k)) as u64;
        }
        let lo = self.next_u32() as u64;
        let hi = (self.next_u32() >> (64 - k)) as u64;
        lo | (hi << 32)
    }

    /// `_randbelow(n)`
    pub fn below(&mut self, n: u64) -> u64 {
        let k = 64 - n.leading_zeros();
        let mut r = self.getrandbits(k);
        while r >= n {
            r = self.getrandbits(k);
        }
        r
    }

    /// `randrange(n)`
    pub fn randrange(&mut self, n: u64) -> u64 {
        self.below(n)
    }

    /// `randint(a, b)`
    pub fn randint(&mut self, a: i64, b: i64) -> i64 {
        a + self.below((b - a + 1) as u64) as i64
    }

    /// `choice(seq)`
    pub fn choice<'a, T>(&mut self, seq: &'a [T]) -> &'a T {
        &seq[self.below(seq.len() as u64) as usize]
    }
}

// --------------------------------------------------------------------------- stores

/// A spec node: a file of a size, or a folder (Python tests' nested dicts, in order).
#[derive(Clone, Debug)]
pub enum S {
    F(i64),
    D(Vec<(String, S)>),
}

pub fn dir<N: Into<String>>(items: impl IntoIterator<Item = (N, S)>) -> S {
    S::D(items.into_iter().map(|(n, s)| (n.into(), s)).collect())
}

/// Store with one scan root (Python `store_builder.build`); file allocation = size rounded up to
/// `cluster`; folders at `pending` paths stay unlisted.
pub fn build_spec(spec: &S, root: &str, cluster: i64, pending: &[&str]) -> Store {
    let store = Store::new();
    {
        let mut app = store.appender();
        let root_id = app.add_root_dir(root).unwrap();
        let mut queue: std::collections::VecDeque<(NodeId, &S, String)> =
            std::collections::VecDeque::from([(root_id, spec, String::new())]);
        while let Some((node, s, prefix)) = queue.pop_front() {
            let S::D(children) = s else { continue };
            let mut b = EntryBatch::new();
            for (name, c) in children {
                match c {
                    S::F(size) => {
                        let ext = app.ext_id(&ext_of(name));
                        let alloc = if cluster <= 1 {
                            *size
                        } else {
                            (size + cluster - 1) / cluster * cluster
                        };
                        b.push(Entry {
                            ext,
                            ..Entry::file(name.as_bytes(), *size, alloc)
                        });
                    }
                    S::D(_) => b.push(Entry::dir(name.as_bytes())),
                }
            }
            let first = app.ingest_listing(node, &b).unwrap();
            for (i, (name, c)) in children.iter().enumerate() {
                if let S::D(_) = c {
                    let path = format!("{prefix}/{name}");
                    if pending.contains(&path.trim_start_matches('/')) {
                        continue;
                    }
                    queue.push_back((first + i as NodeId, c, path));
                }
            }
        }
    }
    store
}

pub fn build(spec: &S) -> Store {
    build_spec(spec, "/root", 1, &[])
}

/// The node at a `/`-separated path below the first root.
pub fn find(nodes: &Nodes, path: &str) -> NodeId {
    sk_store::testing::find(nodes, path)
}

/// One flat folder of entries with explicit kinds, flags, sizes and times.
pub struct Ent<'a> {
    pub name: &'a str,
    pub kind: Kind,
    pub flags: Flags,
    pub size: i64,
    pub alloc: i64,
    pub times: Times,
}

pub fn listing(app: &mut sk_store::Appender<'_>, entries: &[Ent<'_>]) -> EntryBatch {
    let mut b = EntryBatch::new();
    for e in entries {
        let ext = if e.kind == Kind::File {
            app.ext_id(&ext_of(e.name))
        } else {
            0
        };
        b.push(Entry {
            flags: e.flags,
            size: e.size,
            alloc: e.alloc,
            mtime: e.times.mtime,
            ctime: e.times.ctime,
            atime: e.times.atime,
            ext,
            ..Entry::new(e.name.as_bytes(), e.kind)
        });
    }
    b
}

// --------------------------------------------------------------------------- fingerprints

/// `sha256` of length-prefixed parts (the Python golden test's `digest`).
pub fn digest<'a>(parts: impl IntoIterator<Item = &'a [u8]>) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for p in parts {
        h.update((p.len() as u64).to_le_bytes());
        h.update(p);
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

pub trait Le {
    const DTYPE: &'static str;
    fn le(&self, out: &mut Vec<u8>);
}
macro_rules! le {
    ($($t:ty => $d:literal),*) => {$(
        impl Le for $t {
            const DTYPE: &'static str = $d;
            fn le(&self, out: &mut Vec<u8>) { out.extend_from_slice(&self.to_le_bytes()); }
        }
    )*};
}
le!(u8 => "|u1", u16 => "<u2", u32 => "<u4", i32 => "<i4", i64 => "<i8", u64 => "<u8");

/// `"<dtype>:<digest of the bytes>"` of a column.
pub fn column<T: Le>(values: &[T]) -> String {
    let mut bytes = Vec::with_capacity(values.len() * 8);
    for v in values {
        v.le(&mut bytes);
    }
    format!("{}:{}", T::DTYPE, digest([bytes.as_slice()]))
}

// --------------------------------------------------------------------------- the golden tree

use sk_core::layout::effective_alloc;
use sk_store::Col;

const WORDS: &[&str] = &[
    "cache",
    "Temp",
    "node_modules",
    "src",
    "lib",
    "Program Files",
    "Windows",
    "Users",
    "AppData",
    "Local",
    "build",
    "dist",
    "data",
    "logs",
    "img",
    "docs",
    "pkg",
    "bin",
    "文档",
    "Ünïcode",
];
const FILE_COUNTS: &[i64] = &[0, 1, 2, 3, 4, 6, 8, 12, 20, 40, 90];

fn exts() -> Vec<&'static str> {
    let mut v = Vec::new();
    let mut add = |e: &'static str, k: usize| v.extend(std::iter::repeat_n(e, k));
    add("dll", 8);
    add("exe", 3);
    add("txt", 6);
    add("jpg", 6);
    add("png", 5);
    add("mp4", 1);
    add("zip", 1);
    add("sys", 1);
    add("pdf", 1);
    add("log", 4);
    add("json", 6);
    add("js", 9);
    add("py", 5);
    add("dat", 4);
    add("", 6);
    v
}

/// The Python test's `tree()`: drawn with integer operations only.
pub fn golden_tree(nodes: usize, seed: u64, pending_every: usize) -> Store {
    let exts = exts();
    let mut rng = PyRandom::new(seed);
    let store = Store::with_capacity(nodes + 1024);
    {
        let mut app = store.appender();
        let root = app.add_root_dir("C:\\").unwrap();
        let mut queue: Vec<(NodeId, u32)> = vec![(root, 1)];
        while !queue.is_empty() && app.len() < nodes {
            let at = if queue.len() > 64 {
                rng.randrange(queue.len() as u64) as usize
            } else {
                0
            };
            let (d, depth) = queue.remove(at);
            let n_dirs = if depth < 12 {
                *rng.choice(&[0i64, 1, 2, 3, 5, 8])
            } else {
                0
            };
            let n_files = if rng.randrange(10) == 0 {
                rng.randint(500, 5000)
            } else {
                *rng.choice(FILE_COUNTS)
            };
            let count = (n_dirs + n_files).min(nodes as i64 - app.len() as i64);
            if count <= 0 {
                continue;
            }
            let mut b = EntryBatch::new();
            for i in 0..count {
                if i < n_dirs {
                    let name = format!("{}{i}", rng.choice(WORDS));
                    b.push(Entry::dir(name.as_bytes()));
                    continue;
                }
                let ext = *rng.choice(&exts);
                let word = *rng.choice(WORDS);
                let num = rng.randrange(1_000_000);
                let name = if ext.is_empty() {
                    format!("{word}_{num}")
                } else {
                    format!("{word}_{num}.{ext}")
                };
                let mut bits = 4 + rng.randrange(12) + rng.randrange(12);
                if rng.randrange(300) == 0 {
                    bits = 24 + rng.randrange(6);
                }
                let size = if rng.randrange(50) == 0 {
                    0
                } else {
                    rng.randrange(1 << bits) as i64
                };
                let flags = if rng.randrange(40) == 0 {
                    Flags::HARDLINK_DUP
                } else {
                    Flags::NONE
                };
                b.push(Entry {
                    flags,
                    ext: app.ext_id(ext),
                    ..Entry::file(name.as_bytes(), size, (size + 4095) / 4096 * 4096)
                });
            }
            let first = app.ingest_listing(d, &b).unwrap();
            queue.extend((0..n_dirs).map(|i| (first + i as NodeId, depth + 1)));
        }
    }
    if pending_every > 0 {
        let listed: Vec<NodeId> = {
            let r = store.read();
            (0..r.len() as NodeId)
                .filter(|&i| r.is_dir(i) && r.child_count(i) > 0 && r.depth(i) > 2)
                .collect()
        };
        let mut w = store.write();
        for &i in listed.iter().step_by(pending_every) {
            let f = w.flags_raw(i) | Flags::PENDING.bits();
            w.set_raw(Col::Flags, i, f);
        }
        w.touch();
    }
    store
}

pub fn hide_sizes(nodes: &Nodes) -> Vec<i64> {
    let n = nodes.len();
    let table = nodes.ext_table();
    let wanted: Vec<u16> = table
        .iter()
        .enumerate()
        .filter(|(_, e)| &***e == "dll" || &***e == "js")
        .map(|(i, _)| i as u16)
        .collect();
    let mut sizes: Vec<i64> = (0..n as NodeId)
        .map(|i| {
            if !nodes.is_dir(i) && wanted.contains(&nodes.ext(i)) {
                effective_alloc(nodes, i)
            } else {
                0
            }
        })
        .collect();
    let max = (0..n as NodeId).map(|i| nodes.depth(i)).max().unwrap();
    for d in (2..=max).rev() {
        for i in 0..n as NodeId {
            if nodes.depth(i) == d {
                let p = nodes.parent(i) as usize;
                sizes[p] += sizes[i as usize];
            }
        }
    }
    sizes
}
