//! Ingest of a few million synthetic nodes, then snapshot save/load.
//!
//! `cargo run --release -p sk-store --example bench_ingest -- [nodes] [listings per slice]`
//! (defaults: 5,000,000 nodes, 64 listings per slice).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use sk_store::snapshot::{self, Codec, SaveOptions};
use sk_store::subtree::subtree_chunks;
use sk_store::*;

const EXTS: [&str; 8] = ["txt", "dll", "jpg", "mp4", "rs", "json", "log", "bin"];

fn main() {
    let mut args = std::env::args().skip(1);
    let target: usize = args
        .next()
        .and_then(|a| a.parse().ok())
        .unwrap_or(5_000_000);
    let per_slice: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(64);
    let store = Store::new();

    // ------------------------------------------------------------------ ingest
    let mut ingest = Duration::ZERO;
    let mut build = Duration::ZERO;
    let mut slices = 0usize;
    {
        let mut a = store.appender();
        let root = a.add_root_dir("/bench").unwrap();
        let exts: Vec<u16> = EXTS.iter().map(|e| a.ext_id(e)).collect();
        let mut queue = VecDeque::from([root]);
        let mut k = 0u64;
        while a.len() < target {
            let t = Instant::now();
            let mut refs = Vec::with_capacity(per_slice);
            let mut bounds = vec![0u32];
            let mut batch = EntryBatch::with_capacity(per_slice * 20, per_slice * 20 * 16);
            while refs.len() < per_slice {
                let Some(dir) = queue.pop_front() else { break };
                refs.push(SliceRef::Node(dir));
                // 3 folders and 17 files a folder, a bit of variety in names and sizes
                for i in 0..20u64 {
                    k += 1;
                    let name = if i < 3 {
                        format!("folder-{k:08}")
                    } else {
                        format!("file-{k:08}.{}", EXTS[(k % 8) as usize])
                    };
                    let size = (k.wrapping_mul(2654435761) % 10_000_000) as i64;
                    batch.push(if i < 3 {
                        Entry {
                            mtime: k as u32,
                            ..Entry::dir(name.as_bytes())
                        }
                    } else {
                        Entry {
                            ext: exts[(k % 8) as usize],
                            mtime: k as u32,
                            ino: k,
                            ..Entry::file(name.as_bytes(), size, (size + 4095) / 4096 * 4096)
                        }
                    });
                }
                bounds.push(batch.len() as u32);
            }
            build += t.elapsed();
            let t = Instant::now();
            let starts = a.ingest_slice(&refs, &bounds, &batch).unwrap();
            ingest += t.elapsed();
            slices += 1;
            for (j, s) in starts.iter().enumerate() {
                if refs.len() > j {
                    let s = s.unwrap();
                    queue.extend(s..s + 3);
                }
            }
        }
        let t = Instant::now();
        let freed = a.compact();
        let compact_t = t.elapsed();
        let n = a.len();
        println!("nodes                {n:>12}");
        println!(
            "ingest_slice         {:>9.0} ms  ({:.1} M nodes/s, {slices} slices of {per_slice} listings; batches built in {:.0} ms)",
            ms(ingest),
            n as f64 / ingest.as_secs_f64() / 1e6,
            ms(build)
        );
        println!(
            "compact (headroom)   {:>9.0} ms  ({} MB freed)",
            ms(compact_t),
            freed >> 20
        );
        println!(
            "memory               {:>9} MB  ({} B/node: columns {} + names {:.1})",
            a.memory_bytes() >> 20,
            a.memory_bytes() / n,
            ROW_BYTES,
            a.names_size() as f64 / n as f64
        );
        println!("ram (incl. headroom) {:>9} MB", store.ram_bytes() >> 20);
        let t = Instant::now();
        a.reaggregate();
        println!("reaggregate          {:>9.0} ms", ms(t.elapsed()));
    }

    // ------------------------------------------------------------------ reads
    {
        let r = store.read();
        let t = Instant::now();
        let mut count = 0usize;
        for piece in subtree_chunks(&r, 0) {
            count += piece.unwrap().len();
        }
        println!(
            "subtree walk         {:>9.0} ms  ({count} nodes)",
            ms(t.elapsed())
        );
        let t = Instant::now();
        let (copy, _) = mutate::compact(&r).unwrap();
        println!(
            "mutate::compact      {:>9.0} ms  ({} nodes)",
            ms(t.elapsed()),
            copy.len()
        );
    }

    // ------------------------------------------------------------------ snapshots
    let dir = std::env::temp_dir().join(format!("sk-bench-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (codec, level) in [(Codec::Zstd, 1), (Codec::Zstd, 3), (Codec::Raw, 0)] {
        let path = dir.join(format!("bench-{codec:?}-{level}.sks"));
        let opts = SaveOptions {
            codec,
            level,
            ..SaveOptions::roots(["/bench"])
        };
        let t = Instant::now();
        snapshot::save(&store.read(), &path, &opts).unwrap();
        let save_t = t.elapsed();
        let size = std::fs::metadata(&path).unwrap().len();
        let t = Instant::now();
        let (loaded, _) = snapshot::load(&path).unwrap();
        let load_t = t.elapsed();
        assert_eq!(loaded.len(), store.len());
        println!(
            "snapshot {:<11} save {:>6.0} ms, load {:>6.0} ms, file {:>5} MB",
            format!("{codec:?}-{level}"),
            ms(save_t),
            ms(load_t),
            size >> 20
        );
        drop(loaded);
        let _ = std::fs::remove_file(&path);
    }
    let _ = std::fs::remove_dir(&dir);
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}
