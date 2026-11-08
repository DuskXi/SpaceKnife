//! Synthetic snapshots for screenshots and demos: a made-up 2 TB data drive, no real disk data.
//!
//! `cargo run -p sk-store --example mock_snapshot -- --home DIR`
//!
//! Writes two snapshots of the same fictional volume `S:\` (label "Data") into DIR's snapshot
//! folder (`DIR/data/snapshots/<root key>/<stamp>.sks`, where the app looks when
//! `SPACEKNIFE_HOME=DIR`), as if it had been scanned a month apart: `2026-08-20` (base) and
//! `2026-09-24` (now). Between them a video project and a photo month were added, a game was
//! updated and another one uninstalled, a model checkpoint folder grew, an ISO and an old phone
//! backup were deleted, so a snapshot comparison has something to show. Everything (names, sizes,
//! dates) comes from a seeded generator: the same files every run. Prints the two paths, newest
//! last.
//!
//! Port of `tools/demo/mock_snapshot.py`: the same tree, names, size ranges and dates; the
//! generator is a SplitMix64 rather than Python's Mersenne Twister, so the drawn values differ.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use serde_json::{Value, json};
use sk_store::snapshot::{self, SaveOptions};
use sk_store::{Entry, EntryBatch, Flags, Kind, NodeId, Store, Times};

const GB: f64 = (1u64 << 30) as f64;
const MB: f64 = (1u64 << 20) as f64;
const KB: f64 = 1024.0;
const GIB: i64 = 1 << 30;
/// A letter that is rarely mounted: the start page matches records to live drives.
const ROOT: &str = "S:\\";
const CLUSTER: i64 = 4096;
/// A "2 TB" drive.
const TOTAL: i64 = 2_000_396_746_752;

/// A moment in UTC: (year, month, day, hour, minute).
#[derive(Clone, Copy)]
struct At(i64, u32, u32, u32, u32);

const BASE_DAY: At = At(2026, 8, 20, 21, 14);
const NOW_DAY: At = At(2026, 9, 24, 22, 3);

impl At {
    fn secs(self, second: u32) -> i64 {
        days_from_civil(self.0, self.1, self.2) * 86400
            + (self.3 * 3600 + self.4 * 60 + second) as i64
    }
    /// ISO 8601 with offset, like Python's `datetime.isoformat()` of an aware UTC time.
    fn iso(self, second: u32) -> String {
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}+00:00",
            self.0, self.1, self.2, self.3, self.4, second
        )
    }
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Noon UTC of a day (days past the 28th are clamped to it).
fn ts(year: i64, month: u32, day: u32) -> u32 {
    (days_from_civil(year, month, day.min(28)) * 86400 + 12 * 3600) as u32
}

fn ts15(year: i64, month: u32) -> u32 {
    ts(year, month, 15)
}

// ------------------------------------------------------------------ the tree

/// A file is (size, mtime); a folder maps names to files and folders.
#[derive(Clone, Debug)]
enum Node {
    File(i64, u32),
    Dir(Tree),
}

type Tree = BTreeMap<String, Node>;

fn f(size: i64, mtime: u32) -> Node {
    Node::File(size, mtime)
}

fn d<K: Into<String>>(items: impl IntoIterator<Item = (K, Node)>) -> Node {
    Node::Dir(tree(items))
}

fn tree<K: Into<String>>(items: impl IntoIterator<Item = (K, Node)>) -> Tree {
    items.into_iter().map(|(k, v)| (k.into(), v)).collect()
}

fn dir_mut<'a>(t: &'a mut Tree, path: &[&str]) -> &'a mut Tree {
    let mut cur = t;
    for p in path {
        match cur.get_mut(*p) {
            Some(Node::Dir(sub)) => cur = sub,
            _ => panic!("no folder {p}"),
        }
    }
    cur
}

// ------------------------------------------------------------------ the generator

/// SplitMix64: small, seeded, the same sequence on every platform.
struct Gen {
    state: u64,
}

impl Gen {
    fn new(seed: u64) -> Gen {
        Gen { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in [0, 1).
    fn random(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform integer in [lo, hi] (both included, like Python's `randint`).
    fn randint(&mut self, lo: i64, hi: i64) -> i64 {
        lo + (self.next_u64() % (hi - lo + 1) as u64) as i64
    }

    fn choice<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.randint(0, items.len() as i64 - 1) as usize]
    }

    /// `k` distinct items in random order.
    fn sample<'a>(&mut self, items: &[&'a str], k: usize) -> Vec<&'a str> {
        let mut pool = items.to_vec();
        for i in 0..k.min(pool.len()) {
            let j = self.randint(i as i64, pool.len() as i64 - 1) as usize;
            pool.swap(i, j);
        }
        pool.truncate(k);
        pool
    }

    /// Log-uniform between lo and hi bytes.
    fn size(&mut self, lo: f64, hi: f64) -> i64 {
        let (a, b) = (lo.ln(), hi.ln());
        (a + (b - a) * self.random()).exp() as i64
    }

    fn when(&mut self, y0: i64, y1: i64) -> u32 {
        let y = self.randint(y0, y1);
        let m = self.randint(1, if y < 2026 { 12 } else { 8 }) as u32;
        let day = self.randint(1, 28) as u32;
        ts(y, m, day)
    }

    fn split(&mut self, share: f64, n: usize) -> Vec<f64> {
        let w: Vec<f64> = (0..n).map(|_| self.random().powi(2) + 0.05).collect();
        let s: f64 = w.iter().sum();
        w.iter().map(|x| share * x / s).collect()
    }

    // -------------------------------------------------------------- areas

    fn game(&mut self, title: &str, total: f64, year: i64) -> Node {
        let t = ts15(year, self.randint(1, 8) as u32);
        let mut paks = Tree::new();
        for (i, w) in self.split(0.78, 6).into_iter().enumerate() {
            paks.insert(
                format!("pakchunk{i}-WindowsNoEditor.pak"),
                f((total * w) as i64, t),
            );
        }
        let mut movies = Tree::new();
        for n in ["Intro", "Credits", "Act1", "Act2", "Finale"] {
            movies.insert(format!("{n}.bk2"), f(self.size(80.0 * MB, 900.0 * MB), t));
        }
        let mut binaries = Tree::new();
        binaries.insert(
            format!("{}-Win64-Shipping.exe", title.replace(' ', "")),
            f(self.size(90.0 * MB, 260.0 * MB), t),
        );
        for lib in [
            "steam_api64",
            "bink2w64",
            "dxil",
            "amd_fidelityfx_dx12",
            "nvngx_dlss",
            "sl.interposer",
            "PhysX3_x64",
            "tbb12",
        ] {
            binaries.insert(format!("{lib}.dll"), f(self.size(200.0 * KB, 60.0 * MB), t));
        }
        let shaders = tree((0..40).map(|i| {
            (
                format!("ShaderArchive-{i:03}.ushaderbytecode"),
                f(self.size(2.0 * MB, 140.0 * MB), t),
            )
        }));
        let loc = tree(
            ["en", "de", "fr", "ja", "ko", "zh-Hans", "es", "pt-BR"].map(|lang| {
                (
                    lang,
                    d([("Game.locres", f(self.size(200.0 * KB, 4.0 * MB), t))]),
                )
            }),
        );
        let config = tree(
            ["Default", "Engine", "Game", "Input", "Scalability"]
                .map(|n| (format!("{n}.ini"), f(self.size(KB, 40.0 * KB), t))),
        );
        d([
            (
                "Content",
                d([("Paks", Node::Dir(paks)), ("Movies", Node::Dir(movies))]),
            ),
            ("Binaries", d([("Win64", Node::Dir(binaries))])),
            (
                "Engine",
                d([
                    ("Shaders", Node::Dir(shaders)),
                    ("Localization", Node::Dir(loc)),
                    ("Config", Node::Dir(config)),
                ]),
            ),
            (
                "redist",
                d([
                    ("vc_redist.x64.exe", f(25 * (1 << 20), t)),
                    ("DXSETUP.exe", f(520 * 1024, t)),
                ]),
            ),
        ])
    }

    fn footage(&mut self, n: usize, year: i64, month: u32, prefix: &str) -> Tree {
        let t = ts(year, month, self.randint(1, 28) as u32);
        tree((1..=n).map(|i| {
            (
                format!("{prefix}{i:04}.MP4"),
                f(self.size(700.0 * MB, 6.0 * GB), t),
            )
        }))
    }

    fn video_project(&mut self, name: &str, year: i64, month: u32, clips: usize) -> Node {
        let t = ts(year, month, 27);
        let slug = name.to_lowercase().replace(' ', "-");
        let footage = self.footage(clips, year, month, "C");
        let proxies = tree((1..=clips).map(|i| {
            (
                format!("C{i:04}_Proxy.mov"),
                f(self.size(60.0 * MB, 400.0 * MB), t),
            )
        }));
        let audio = tree((0..12).map(|i| {
            (
                format!("ambience-{i:02}.wav"),
                f(self.size(30.0 * MB, 600.0 * MB), t),
            )
        }));
        let exports = tree([
            (
                format!("{slug}-final-4k.mp4"),
                f(self.size(6.0 * GB, 14.0 * GB), t),
            ),
            (
                format!("{slug}-teaser.mp4"),
                f(self.size(300.0 * MB, 900.0 * MB), t),
            ),
        ]);
        let project = f(self.size(20.0 * MB, 120.0 * MB), t);
        let cache = tree((0..60).map(|i| {
            (
                format!("media-cache-{i:03}.cfa"),
                f(self.size(4.0 * MB, 260.0 * MB), t),
            )
        }));
        d([
            ("Footage".to_string(), Node::Dir(footage)),
            ("Proxies".to_string(), Node::Dir(proxies)),
            ("Audio".to_string(), Node::Dir(audio)),
            ("Exports".to_string(), Node::Dir(exports)),
            (format!("{name}.prproj"), project),
            ("Cache".to_string(), Node::Dir(cache)),
        ])
    }

    fn photo_month(&mut self, year: i64, month: u32) -> Node {
        let mut out = Tree::new();
        for _ in 0..self.randint(30, 140) {
            let t = ts(year, month, self.randint(1, 28) as u32);
            let n = self.randint(1000, 9999);
            out.insert(
                format!("DSC{n:05}.ARW"),
                f(self.size(22.0 * MB, 48.0 * MB), t),
            );
            if self.random() < 0.7 {
                out.insert(
                    format!("DSC{n:05}.JPG"),
                    f(self.size(4.0 * MB, 14.0 * MB), t),
                );
            }
        }
        if self.random() < 0.35 {
            let k = self.randint(5, 40);
            let edited = tree((0..k).map(|i| {
                (
                    format!("edit-{i:03}.jpg"),
                    f(self.size(6.0 * MB, 30.0 * MB), ts(year, month, 28)),
                )
            }));
            out.insert("Edited".into(), Node::Dir(edited));
        }
        Node::Dir(out)
    }

    fn music(&mut self) -> Node {
        const FIRST: [&str; 12] = [
            "Velvet", "Northern", "Paper", "Glass", "Silent", "Electric", "Crimson", "Lunar",
            "Golden", "Hollow", "Neon", "Wild",
        ];
        const SECOND: [&str; 12] = [
            "Harbor", "Lights", "Engines", "Tides", "Orchard", "Canyon", "Atlas", "Signal",
            "Meadow", "Static", "Parade", "Avenue",
        ];
        const WORDS: [&str; 12] = [
            "Night",
            "Ocean",
            "Drive",
            "Echoes",
            "Summer",
            "Paper",
            "Gravity",
            "Fireflies",
            "Horizon",
            "Static",
            "Rain",
            "Home",
        ];
        let mut out = Tree::new();
        for _ in 0..60 {
            let artist = format!("{} {}", self.choice(&FIRST), self.choice(&SECOND));
            let mut albums = match out.remove(&artist) {
                Some(Node::Dir(t)) => t,
                _ => Tree::new(),
            };
            for _ in 0..self.randint(1, 4) {
                let album = format!(
                    "{} {} ({})",
                    self.choice(&WORDS),
                    self.choice(&WORDS),
                    self.randint(2004, 2026)
                );
                let t = self.when(2016, 2026);
                let mut tracks = Tree::new();
                for k in 1..self.randint(8, 15) {
                    let name = format!(
                        "{k:02} - {} {}.flac",
                        self.choice(&WORDS),
                        self.choice(&WORDS)
                    );
                    tracks.insert(name, f(self.size(18.0 * MB, 70.0 * MB), t));
                }
                tracks.insert("cover.jpg".into(), f(self.size(300.0 * KB, 3.0 * MB), t));
                albums.insert(album, Node::Dir(tracks));
            }
            out.insert(artist, Node::Dir(albums));
        }
        Node::Dir(out)
    }

    fn node_modules(&mut self, depth: u32) -> Node {
        const NAMES: [&str; 30] = [
            "react",
            "react-dom",
            "scheduler",
            "typescript",
            "vite",
            "esbuild",
            "rollup",
            "postcss",
            "eslint",
            "lodash",
            "zod",
            "date-fns",
            "@babel",
            "@types",
            "webpack",
            "terser",
            "sass",
            "chalk",
            "commander",
            "semver",
            "acorn",
            "debug",
            "ms",
            "tslib",
            "core-js",
            "caniuse-lite",
            "browserslist",
            "picocolors",
            "nanoid",
            "glob",
        ];
        let mut out = Tree::new();
        let k = if depth == 0 { 26 } else { 4 };
        for name in self.sample(&NAMES, k) {
            let t = self.when(2025, 2026);
            let mut pkg = Tree::new();
            pkg.insert("package.json".into(), f(self.size(600.0, 8.0 * KB), t));
            pkg.insert("README.md".into(), f(self.size(KB, 40.0 * KB), t));
            pkg.insert("LICENSE".into(), f(1100, t));
            let stem = name.trim_matches('@');
            let dist = tree(
                [".js", ".cjs", ".mjs", ".d.ts", ".min.js", ".js.map"]
                    .map(|s| (format!("{stem}{s}"), f(self.size(2.0 * KB, 3.0 * MB), t))),
            );
            pkg.insert("dist".into(), Node::Dir(dist));
            let n = self.randint(10, 80);
            let mut lib =
                tree((0..n).map(|i| (format!("module{i}.js"), f(self.size(400.0, 90.0 * KB), t))));
            if matches!(name, "typescript" | "esbuild" | "@babel" | "core-js") && depth == 0 {
                for i in 0..120 {
                    lib.insert(format!("chunk{i}.js"), f(self.size(40.0 * KB, 9.0 * MB), t));
                }
            }
            pkg.insert("lib".into(), Node::Dir(lib));
            if depth < 1 && self.random() < 0.3 {
                pkg.insert("node_modules".into(), self.node_modules(depth + 1));
            }
            out.insert(name.to_string(), Node::Dir(pkg));
        }
        Node::Dir(out)
    }

    fn rust_target(&mut self) -> Node {
        let t = self.when(2026, 2026);
        let mut deps: Vec<(String, i64)> = Vec::new();
        for c in [
            "serde", "tokio", "rayon", "regex", "syn", "hyper", "clap", "image", "wgpu", "naga",
        ] {
            for _ in 0..6 {
                let name = format!("lib{c}-{:016x}.rlib", self.next_u64());
                deps.push((name, self.size(200.0 * KB, 90.0 * MB)));
            }
        }
        let mut inc = Tree::new();
        for c in ["core", "app", "render"] {
            for i in 0..12 {
                let n = self.randint(5, 30);
                let objs = tree(
                    (0..n).map(|j| (format!("s-{j}.o"), f(self.size(20.0 * KB, 6.0 * MB), t))),
                );
                inc.insert(format!("{c}-{i:03x}"), Node::Dir(objs));
            }
        }
        let mut build = Tree::new();
        for c in ["ring", "zstd-sys", "openssl-sys"] {
            for i in 0..3 {
                let out = d([("out", d([("output.o", f(self.size(MB, 40.0 * MB), t))]))]);
                build.insert(format!("{c}-{i}"), out);
            }
        }
        let release = tree(deps.iter().take(30).map(|(k, s)| (k.clone(), f(s / 2, t))));
        let debug_deps = tree(deps.into_iter().map(|(k, s)| (k, f(s, t))));
        d([
            (
                "debug",
                d([
                    ("deps", Node::Dir(debug_deps)),
                    ("incremental", Node::Dir(inc)),
                    ("build", Node::Dir(build)),
                ]),
            ),
            ("release", d([("deps", Node::Dir(release))])),
        ])
    }

    fn repo(&mut self, kind: &str) -> Tree {
        let t = self.when(2025, 2026);
        let mut pack = Tree::new();
        for _ in 0..self.randint(1, 4) {
            let name = format!("pack-{:016x}.pack", self.next_u64());
            pack.insert(name, f(self.size(20.0 * MB, 1.6 * GB), t));
        }
        let git = d([
            ("objects", d([("pack", Node::Dir(pack))])),
            ("index", f(self.size(100.0 * KB, 6.0 * MB), t)),
        ]);
        let src = tree(
            [
                "main", "app", "config", "router", "store", "utils", "api", "models", "views",
                "tests",
            ]
            .map(|w| (format!("{w}.{kind}"), f(self.size(600.0, 60.0 * KB), t))),
        );
        let mut out = Tree::new();
        out.insert(".git".into(), git);
        out.insert("src".into(), Node::Dir(src));
        out.insert("README.md".into(), f(self.size(KB, 20.0 * KB), t));
        if kind == "ts" || kind == "js" {
            out.insert("node_modules".into(), self.node_modules(0));
            let dist = tree((0..20).map(|i| {
                (
                    format!("assets-{i}.js"),
                    f(self.size(20.0 * KB, 2.0 * MB), t),
                )
            }));
            out.insert("dist".into(), Node::Dir(dist));
        }
        if kind == "rs" {
            out.insert("target".into(), self.rust_target());
        }
        out
    }

    // -------------------------------------------------------------- the drive

    /// The drive on the base day.
    fn drive(&mut self) -> Tree {
        let games = tree([
            (
                "Starfall Odyssey",
                self.game("Starfall Odyssey", 96.0 * GB, 2025),
            ),
            (
                "Iron Frontier 2",
                self.game("Iron Frontier 2", 74.0 * GB, 2024),
            ),
            ("Neon Drift", self.game("Neon Drift", 38.0 * GB, 2025)),
            ("Hollow Keep", self.game("Hollow Keep", 12.0 * GB, 2023)),
            (
                "Skyward Tactics",
                self.game("Skyward Tactics", 51.0 * GB, 2026),
            ),
            ("Abyssal Tide", self.game("Abyssal Tide", 63.0 * GB, 2025)),
        ]);
        let mut shadercache = Tree::new();
        for _ in 0..14 {
            let id = self.randint(100000, 3000000).to_string();
            let foz = f(self.size(80.0 * MB, 1.2 * GB), self.when(2025, 2026));
            shadercache.insert(
                id,
                d([("fozpipelines", d([("steam_pipeline_cache.foz", foz)]))]),
            );
        }
        let mut content = Tree::new();
        for _ in 0..30 {
            let id = self.randint(1_000_000, 9_000_000).to_string();
            let n = self.randint(1, 6);
            let items = tree((0..n).map(|i| {
                (
                    format!("item{i}.vpk"),
                    f(self.size(5.0 * MB, 600.0 * MB), self.when(2024, 2026)),
                )
            }));
            content.insert(id, Node::Dir(items));
        }
        let steam = d([(
            "steamapps",
            d([
                ("common", Node::Dir(games)),
                ("shadercache", Node::Dir(shadercache)),
                ("workshop", d([("content", Node::Dir(content))])),
            ]),
        )]);

        let projects = tree([
            (
                "2025 Coastline Trip",
                self.video_project("Coastline Trip", 2025, 8, 38),
            ),
            (
                "2026 Spring City Walk",
                self.video_project("Spring City Walk", 2026, 4, 22),
            ),
        ]);
        let mut drone = Tree::new();
        for m in [3u32, 5, 6] {
            let n = self.randint(6, 14) as usize;
            drone.insert(
                format!("2026-0{m}"),
                Node::Dir(self.footage(n, 2026, m, "DJI_")),
            );
        }
        let mut recordings = Tree::new();
        for i in 0..36 {
            let name = format!("recording-2026-0{}-{i:02}.mp4", self.randint(1, 8));
            recordings.insert(
                name,
                f(self.size(40.0 * MB, 2.0 * GB), self.when(2026, 2026)),
            );
        }
        let movies = tree(
            [
                "The Long Harbor",
                "Glass Orchard",
                "Northern Signal",
                "Paper Atlas",
                "Electric Meadow",
                "Silent Canyon",
                "Lunar Parade",
            ]
            .map(|w| {
                (
                    format!("{w}.mkv"),
                    f(self.size(3.0 * GB, 14.0 * GB), self.when(2020, 2025)),
                )
            }),
        );
        let videos = d([
            ("Projects", Node::Dir(projects)),
            ("Drone", Node::Dir(drone)),
            ("Screen Recordings", Node::Dir(recordings)),
            ("Movies", Node::Dir(movies)),
        ]);

        let mut photos = Tree::new();
        for year in 2019..2027i64 {
            let mut months = Tree::new();
            for month in 1..=12u32 {
                if year == 2026 && month > 8 {
                    break;
                }
                months.insert(format!("{year}-{month:02}"), self.photo_month(year, month));
            }
            photos.insert(year.to_string(), Node::Dir(months));
        }
        let catalog = f(self.size(1.5 * GB, 3.0 * GB), ts(2026, 8, 20));
        let mut previews = Tree::new();
        for i in 0..16 {
            let n = self.randint(20, 80);
            let files = tree((0..n).map(|j| {
                (
                    format!("preview-{j}.lrprev"),
                    f(self.size(200.0 * KB, 4.0 * MB), self.when(2024, 2026)),
                )
            }));
            previews.insert(format!("{i:X}"), Node::Dir(files));
        }
        photos.insert(
            "Lightroom Catalog".into(),
            d([
                ("Catalog.lrcat", catalog),
                ("Catalog Previews.lrdata", Node::Dir(previews)),
            ]),
        );

        let mut ml = self.repo("py");
        let data = tree((0..48).map(|i| {
            (
                format!("train-{i:05}-of-00048.parquet"),
                f(self.size(600.0 * MB, 2.4 * GB), ts15(2026, 6)),
            )
        }));
        ml.insert("data".into(), Node::Dir(data));
        ml.insert(
            "checkpoints".into(),
            d([10, 20].map(|s| (format!("step-{s}k.safetensors"), f(14 * GIB, ts15(2026, 7))))),
        );
        let notes = tree(
            ["ideas", "todo", "reading", "setup", "commands", "meetings"].map(|w| {
                (
                    format!("{w}.md"),
                    f(self.size(KB, 60.0 * KB), self.when(2024, 2026)),
                )
            }),
        );
        let dev = tree([
            ("web-shop", Node::Dir(self.repo("ts"))),
            ("dashboard", Node::Dir(self.repo("ts"))),
            ("landing-page", Node::Dir(self.repo("js"))),
            ("rust-tools", Node::Dir(self.repo("rs"))),
            ("game-mods", Node::Dir(self.repo("rs"))),
            ("ml-lab", Node::Dir(ml)),
            ("notes", Node::Dir(notes)),
        ]);

        let vbox_logs = tree((0..4).map(|i| {
            (
                format!("VBox.log.{i}"),
                f(self.size(100.0 * KB, 2.0 * MB), ts15(2026, 7)),
            )
        }));
        let vms = d([
            (
                "Windows 11 Dev",
                d([
                    ("Windows 11 Dev.vhdx", f(96 * GIB, ts(2026, 8, 12))),
                    ("Windows 11 Dev.vmcx", f(60 * 1024, ts15(2026, 8))),
                ]),
            ),
            (
                "Ubuntu Server 24.04",
                d([
                    ("Ubuntu Server 24.04.vdi", f(42 * GIB, ts15(2026, 7))),
                    ("Logs", Node::Dir(vbox_logs)),
                ]),
            ),
            (
                "Fedora Workstation",
                d([("fedora.qcow2", f(28 * GIB, ts15(2026, 5)))]),
            ),
        ]);

        let mut images = Tree::new();
        for _ in 0..2 {
            let name = format!("{:016x}{:016x}.vhdx", self.next_u64(), self.next_u64());
            images.insert(name, f(self.size(40.0 * GB, 120.0 * GB), ts(2025, 12, 28)));
        }
        let backups = d([
            ("Phone Backup 2026-03.zip", f(64 * GIB, ts15(2026, 3))),
            (
                "System Image 2025-12",
                d([(
                    "WindowsImageBackup",
                    d([("Backup 2025-12-28", Node::Dir(images))]),
                )]),
            ),
            ("Documents Archive 2024.7z", f(12 * GIB, ts15(2025, 1))),
        ]);

        let mut downloads = tree([
            (
                "ubuntu-24.04.3-desktop-amd64.iso",
                f((6.1 * GB) as i64, ts(2026, 8, 9)),
            ),
            ("Win11_24H2_x64.iso", f((5.8 * GB) as i64, ts15(2026, 2))),
            (
                "fedora-workstation-42-x86_64.iso",
                f((2.3 * GB) as i64, ts15(2026, 5)),
            ),
        ]);
        for w in [
            "editor", "studio", "player", "drivers", "toolkit", "client", "launcher", "sdk",
        ] {
            let name = format!(
                "setup-{w}-{}.{}.exe",
                self.randint(1, 9),
                self.randint(0, 20)
            );
            downloads.insert(
                name,
                f(self.size(20.0 * MB, 900.0 * MB), self.when(2025, 2026)),
            );
        }
        for w in [
            "assets-pack",
            "fonts",
            "sample-project",
            "textures-4k",
            "soundbank",
            "dataset-mini",
        ] {
            downloads.insert(
                format!("{w}.zip"),
                f(self.size(5.0 * MB, 4.0 * GB), self.when(2024, 2026)),
            );
        }
        for i in 0..80 {
            downloads.insert(
                format!("document-{i:03}.pdf"),
                f(self.size(100.0 * KB, 40.0 * MB), self.when(2023, 2026)),
            );
        }

        let mut work = Tree::new();
        for w in ["report", "plan", "budget", "slides", "minutes"] {
            for i in 0..40 {
                let e = self.choice(&["docx", "xlsx", "pptx", "pdf"]);
                work.insert(
                    format!("{w}-{i:02}.{e}"),
                    f(self.size(20.0 * KB, 30.0 * MB), self.when(2021, 2026)),
                );
            }
        }
        let scans = tree((0..300).map(|i| {
            (
                format!("scan-{i:04}.pdf"),
                f(self.size(200.0 * KB, 12.0 * MB), self.when(2019, 2026)),
            )
        }));
        let books = tree((0..120).map(|i| {
            (
                format!("book-{i:03}.epub"),
                f(self.size(400.0 * KB, 30.0 * MB), self.when(2019, 2026)),
            )
        }));
        let documents = d([
            ("Work", Node::Dir(work)),
            ("Scans", Node::Dir(scans)),
            ("Books", Node::Dir(books)),
        ]);

        let music = self.music();
        tree([
            ("Games", d([("SteamLibrary", steam)])),
            ("Videos", videos),
            ("Photos", Node::Dir(photos)),
            ("Music", music),
            ("Dev", Node::Dir(dev)),
            ("VMs", vms),
            ("Backups", backups),
            ("Downloads", Node::Dir(downloads)),
            ("Documents", documents),
        ])
    }

    /// The same drive five weeks on (a separate generator, so the base stays identical).
    fn month_later(&mut self, mut t: Tree) -> Tree {
        let games = dir_mut(&mut t, &["Games", "SteamLibrary", "steamapps", "common"]);
        games.remove("Hollow Keep"); // uninstalled
        dir_mut(games, &["Starfall Odyssey", "Content", "Paks"]).insert(
            "pakchunk6-WindowsNoEditor.pak".into(),
            f(12 * GIB, ts(2026, 9, 18)),
        );
        let project = self.video_project("Mountain Weekend", 2026, 9, 34);
        dir_mut(&mut t, &["Videos", "Projects"]).insert("2026 Mountain Weekend".into(), project);
        let photos = self.photo_month(2026, 9);
        dir_mut(&mut t, &["Photos", "2026"]).insert("2026-09".into(), photos);
        dir_mut(&mut t, &["Photos", "Lightroom Catalog"]).insert(
            "Catalog.lrcat".into(),
            f((2.9 * GB) as i64, ts(2026, 9, 22)),
        );
        let checkpoints = dir_mut(&mut t, &["Dev", "ml-lab", "checkpoints"]);
        for s in [30u32, 40, 50] {
            checkpoints.insert(
                format!("step-{s}k.safetensors"),
                f(14 * GIB, ts(2026, 9, s / 3)),
            );
        }
        let modules = self.node_modules(0); // reinstalled
        dir_mut(&mut t, &["Dev", "web-shop"]).insert("node_modules".into(), modules);
        dir_mut(&mut t, &["VMs", "Windows 11 Dev"])
            .insert("Windows 11 Dev.vhdx".into(), f(104 * GIB, ts(2026, 9, 20)));
        let backups = dir_mut(&mut t, &["Backups"]);
        backups.remove("Phone Backup 2026-03.zip");
        backups.insert(
            "Phone Backup 2026-09.zip".into(),
            f(71 * GIB, ts(2026, 9, 21)),
        );
        let downloads = dir_mut(&mut t, &["Downloads"]);
        downloads.remove("Win11_24H2_x64.iso");
        downloads.insert(
            "driver-pack-2026.09.exe".into(),
            f((1.2 * GB) as i64, ts(2026, 9, 3)),
        );
        t
    }
}

// ------------------------------------------------------------------ store and snapshot

fn newest(v: &Node) -> u32 {
    match v {
        Node::File(_, m) => *m,
        Node::Dir(t) => t.values().map(newest).max().unwrap_or(0),
    }
}

/// Lower-case extension of a name ("" if none, or longer than 16), as the scanners compute it.
fn ext_of(name: &str) -> String {
    match name.rfind('.') {
        Some(dot) if dot > 0 && name.len() - dot <= 17 => name[dot + 1..].to_lowercase(),
        _ => String::new(),
    }
}

/// The tree as a store, one listing per folder, like a scan.
fn build_store(t: &Tree) -> Store {
    let store = Store::new();
    {
        let mut a = store.appender();
        let root = a
            .add_root(ROOT.as_bytes(), Kind::Dir, Flags::PENDING, Times::default())
            .expect("root");
        let mut queue: Vec<(NodeId, &Tree)> = vec![(root, t)];
        while let Some((node, children)) = queue.pop() {
            let mut batch = EntryBatch::with_capacity(children.len(), 0);
            for (name, value) in children {
                let e = match value {
                    Node::Dir(sub) => {
                        let m = if sub.is_empty() {
                            ts15(2024, 1)
                        } else {
                            newest(value)
                        };
                        Entry {
                            mtime: m,
                            ctime: m,
                            atime: m,
                            ..Entry::dir(name.as_bytes())
                        }
                    }
                    Node::File(size, m) => Entry {
                        mtime: *m,
                        ctime: *m,
                        atime: *m,
                        ext: a.ext_id(&ext_of(name)),
                        ..Entry::file(
                            name.as_bytes(),
                            *size,
                            (size + CLUSTER - 1) / CLUSTER * CLUSTER,
                        )
                    },
                };
                batch.push(e);
            }
            let first = a.ingest_listing(node, &batch).expect("ingest");
            for (i, value) in children.values().enumerate() {
                if let Node::Dir(sub) = value {
                    queue.push((first + i as NodeId, sub));
                }
            }
        }
        a.compact();
    }
    store
}

/// The drive's tree on the base day, or five weeks later.
fn drive(now: bool) -> Tree {
    let t = Gen::new(7).drive();
    if now { Gen::new(8).month_later(t) } else { t }
}

fn thousands(n: i64) -> String {
    let s = n.abs().to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

/// Writes one snapshot under `home`; returns its path.
fn write(home: &Path, when: At, now: bool) -> std::io::Result<PathBuf> {
    let store = build_store(&drive(now));
    let nodes = store.read();
    let alloc = nodes.tot_alloc(0);
    let unknown = 9 * GIB; // system files and metadata the scan does not see
    let volume = json!({
        "mount_point": ROOT,
        "device": "\\\\?\\Volume{00000000-0000-0000-0000-000000000000}\\",
        "fs_type": "NTFS",
        "label": "Data",
        "total": TOTAL,
        "free": TOTAL - alloc - unknown,
        "cluster_size": CLUSTER,
        "is_remote": false,
        "is_removable": false,
        "used": alloc + unknown,
    });
    let stats = json!({
        "roots": [{"path": ROOT, "whole_volume": true}],
        "method": "mft",
        "state": "done",
        "items": nodes.len() as i64 - 1,
        "files": nodes.tot_files(0),
        "dirs": nodes.tot_dirs(0),
        "bytes_alloc": alloc,
        "bytes_size": nodes.tot_size(0),
        "elapsed_s": if now { 6.8 } else { 7.1 },
        "started_at": when.iso(0),
        "finished_at": when.iso(7),
        "errors": 0,
        "denied": [],
    });
    let Value::Object(stats) = stats else {
        unreachable!()
    };
    let secs = when.secs(0) as u64;
    let roots = vec![ROOT.to_string()];
    let base = home.join("data").join("snapshots");
    let path = snapshot::auto_path(&roots, Some(secs), &base);
    std::fs::create_dir_all(path.parent().expect("parent"))?;
    let opts = SaveOptions {
        roots,
        volumes: vec![volume],
        stats,
        created_at: Some(secs),
        ..SaveOptions::default()
    };
    snapshot::save(&nodes, &path, &opts).map_err(std::io::Error::other)?;
    let file = std::fs::File::options().write(true).open(&path)?;
    file.set_modified(UNIX_EPOCH + Duration::from_secs(secs))?;
    drop(file);
    println!(
        "{}  {} items, {} GB",
        path.display(),
        thousands(nodes.len() as i64 - 1),
        thousands((alloc as f64 / GB).round() as i64)
    );
    Ok(path)
}

fn usage() -> ! {
    eprintln!(
        "usage: cargo run -p sk-store --example mock_snapshot -- --home DIR\n\n\
         Writes two synthetic snapshots of a made-up 2 TB drive S:\\ into DIR/data/snapshots\n\
         (DIR is the SPACEKNIFE_HOME of the demo / screenshot session); prints their paths, newest last."
    );
    std::process::exit(2)
}

fn main() {
    let mut home: Option<PathBuf> = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(a) = args.next() {
        match a.to_str() {
            Some("--home") => home = args.next().map(PathBuf::from),
            Some(s) if s.starts_with("--home=") => home = Some(PathBuf::from(&s[7..])),
            Some("-h" | "--help") => usage(),
            _ => usage(),
        }
    }
    let Some(home) = home else { usage() };
    for (when, now) in [(BASE_DAY, false), (NOW_DAY, true)] {
        if let Err(e) = write(&home, when, now) {
            eprintln!("mock_snapshot: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(t: &Tree, path: &[&str]) -> Option<Node> {
        let (last, dirs) = path.split_last()?;
        let mut cur = t;
        for p in dirs {
            match cur.get(*p)? {
                Node::Dir(sub) => cur = sub,
                Node::File(..) => return None,
            }
        }
        cur.get(*last).cloned()
    }

    #[test]
    fn deterministic_and_different() {
        let base = drive(false);
        let now = drive(true);
        let again = build_store(&drive(false));
        let first = build_store(&base);
        assert_eq!(first.read().tot_alloc(0), again.read().tot_alloc(0));
        assert_eq!(first.len(), again.len());
        assert!(
            find(
                &base,
                &[
                    "Games",
                    "SteamLibrary",
                    "steamapps",
                    "common",
                    "Hollow Keep"
                ]
            )
            .is_some()
        );
        assert!(
            find(
                &now,
                &[
                    "Games",
                    "SteamLibrary",
                    "steamapps",
                    "common",
                    "Hollow Keep"
                ]
            )
            .is_none()
        );
        assert!(find(&now, &["Videos", "Projects", "2026 Mountain Weekend"]).is_some());
        assert!(find(&now, &["Backups", "Phone Backup 2026-03.zip"]).is_none());
        assert!(find(&now, &["Downloads", "Win11_24H2_x64.iso"]).is_none());
        // the base drive holds a few hundred GB on a 2 TB volume
        let gb = first.read().tot_alloc(0) / GIB;
        assert!((300..1900).contains(&gb), "{gb} GB");
    }

    #[test]
    fn snapshots_load() {
        let home = tempfile::tempdir().unwrap();
        let a = write(home.path(), BASE_DAY, false).unwrap();
        let b = write(home.path(), NOW_DAY, true).unwrap();
        assert_eq!(a.file_name().unwrap(), "20260820-211400.sks");
        assert_eq!(b.file_name().unwrap(), "20260924-220300.sks");
        let list = snapshot::list_snapshots(&home.path().join("data").join("snapshots"));
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].0, b); // newest first
        let (store, header) = snapshot::load(&b).unwrap();
        assert_eq!(header.roots, vec![ROOT.to_string()]);
        assert_eq!(header.created_at, NOW_DAY.secs(0) as u64);
        let nodes = store.read();
        assert!(
            nodes
                .find(b"S:\\Videos\\Projects\\2026 Mountain Weekend")
                .is_some()
        );
    }
}
