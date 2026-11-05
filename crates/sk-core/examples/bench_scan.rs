//! Scans a path and prints items, time and items/s.
//!
//! ```text
//! cargo run --release -p sk-core --example bench_scan -- C:\Windows [--threads N] [--repeat N] [--profile]
//! ```
use std::time::{Duration, Instant};

use sk_core::scan::{ScanOptions, Scanner};

fn main() {
    let mut args = std::env::args().skip(1);
    let mut path = None;
    let mut threads = 0usize;
    let mut repeat = 1usize;
    let mut profile = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--threads" => threads = args.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            "--repeat" => repeat = args.next().and_then(|v| v.parse().ok()).unwrap_or(1),
            "--profile" => profile = true,
            _ => path = Some(a),
        }
    }
    let Some(path) = path else {
        eprintln!("usage: bench_scan <path> [--threads N] [--repeat N] [--profile]");
        std::process::exit(2);
    };
    for run in 1..=repeat {
        let t0 = Instant::now();
        let scanner = match Scanner::new(
            &[path.as_str()],
            ScanOptions {
                concurrency: threads,
                ..Default::default()
            },
        ) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        };
        let p = scanner.run(None, Duration::from_millis(250));
        let secs = t0.elapsed().as_secs_f64();
        println!(
            "run {run}: {} items ({} files, {} dirs, {:.2} GiB, {} errors) in {:.3}s = {:.0} items/s [{} threads]",
            p.items,
            p.files,
            p.dirs,
            p.bytes_alloc as f64 / (1u64 << 30) as f64,
            p.errors,
            secs,
            p.items as f64 / secs.max(1e-9),
            scanner.concurrency(),
        );
        if profile {
            println!("  {}", scanner.profile().text());
        }
    }
}
