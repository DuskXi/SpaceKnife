//! `scan`'s output read the way the external benchmark reads it (DuskXi/Disk-Scan-Benchmark,
//! `scripts/wrappers/run_cli_scanners.py`: `parse_spaceknife_scan_ms`, `spaceknife_mft_fallback`,
//! `parse_spaceknife_subtree`, `parse_spaceknife_output`, `parse_cli_structure_samples`), its
//! regular expressions ported as they are, over the command lines `scanners_registry.json` runs.

use std::path::Path;

use regex::{Regex, RegexBuilder};

use super::super::rich::Console;
use super::super::{Cli, Command};
use super::{lock_language, tree};

fn clean(text: &str) -> String {
    Regex::new(r"\x1b\[[0-9;?]*[a-zA-Z]")
        .unwrap()
        .replace_all(text, "")
        .replace('\r', "\n")
}

fn ci(pattern: &str) -> Regex {
    RegexBuilder::new(pattern)
        .case_insensitive(true)
        .build()
        .unwrap()
}

/// `parse_spaceknife_scan_ms`.
fn scan_ms(text: &str) -> Option<f64> {
    let re = ci(
        r"(?:Time|Duration|Elapsed|用时)\s+([0-9][0-9,]*(?:\.[0-9]+)?)\s*(ms|milliseconds?|毫秒|secs?|seconds?|秒|s)\b",
    );
    let c = clean(text);
    let m = re.captures(&c)?;
    let value: f64 = m[1].replace(',', "").parse().ok()?;
    let unit = m[2].to_lowercase();
    Some(
        if unit == "ms" || unit == "毫秒" || unit.starts_with("milli") {
            value
        } else {
            value * 1000.0
        },
    )
}

/// `spaceknife_mft_fallback`.
fn mft_fallback(text: &str) -> Option<String> {
    let c = clean(text);
    if Regex::new(r"(?:Method|方式)\s+MFT\s*(?:quick scan|快速扫描)")
        .unwrap()
        .is_match(&c)
    {
        return None;
    }
    let re = Regex::new(r"(?s)Method full scan \((.*?)\)|方式\s*硬扫描（(.*?)）").unwrap();
    if let Some(m) = re.captures(&c) {
        let reason = m.get(1).or(m.get(2)).map_or("", |g| g.as_str());
        let joined = reason.split_whitespace().collect::<Vec<_>>().join(" ");
        return Some(if joined.is_empty() {
            "no reason given".into()
        } else {
            joined
        });
    }
    Some("no scan method line in the output".into())
}

fn units(os_key: &str) -> impl Fn(&str) -> f64 {
    let base: f64 = if os_key == "macos" { 1000.0 } else { 1024.0 };
    move |u: &str| match u.to_lowercase().as_str() {
        "b" => 1.0,
        "kb" => base,
        "mb" => base.powi(2),
        "gb" => base.powi(3),
        "tb" => base.powi(4),
        "pb" => base.powi(5),
        "kib" => 1024f64,
        "mib" => 1024f64.powi(2),
        "gib" => 1024f64.powi(3),
        "tib" => 1024f64.powi(4),
        "pib" => 1024f64.powi(5),
        _ => 1.0,
    }
}

/// `parse_spaceknife_subtree`: (bytes, files) of one row of the summary table.
fn subtree(text: &str, name: &str, os_key: &str) -> (i64, i64) {
    let c = clean(text);
    let unit = units(os_key);
    let row = RegexBuilder::new(&format!(
        r"^[\s│▸▾]*{}\s+([0-9]+(?:\.[0-9]+)?)\s*(PiB|TiB|GiB|MiB|KiB|PB|TB|GB|MB|KB|B)\s+[0-9.]+%(.*)$",
        regex::escape(name)
    ))
    .case_insensitive(true)
    .multi_line(true)
    .build()
    .unwrap();
    let Some(m) = row.captures(&c) else {
        return (0, 0);
    };
    let bytes = (m[1].parse::<f64>().unwrap() * unit(&m[2])) as i64;
    let files = Regex::new(r"([0-9][0-9,]*)$")
        .unwrap()
        .captures(m[3].trim())
        .map_or(0, |f| f[1].replace(',', "").parse().unwrap());
    (bytes, files)
}

/// `parse_spaceknife_output`: (bytes, files) from the summary panel.
fn output(text: &str, metric: &str, os_key: &str) -> (i64, i64) {
    let c = clean(text);
    let unit = units(os_key);
    let size = |label: &str| {
        ci(&format!(
            r"(?:{label})\s+([0-9]+(?:\.[0-9]+)?)\s*(PiB|TiB|GiB|MiB|KiB|PB|TB|GB|MB|KB|B)\b"
        ))
        .captures(&c)
        .map_or(0, |m| (m[1].parse::<f64>().unwrap() * unit(&m[2])) as i64)
    };
    let alloc = size("Size on disk|占用");
    let logical = size("Logical size|逻辑大小");
    let files = ci(r"(?:files|文件)\s+([0-9,]+)")
        .captures(&c)
        .map_or(0, |m| m[1].replace(',', "").parse().unwrap_or(0));
    let mut chosen = if metric == "apparent" { logical } else { alloc };
    if chosen == 0 {
        chosen = if alloc != 0 { alloc } else { logical };
    }
    (chosen, files)
}

/// `parse_cli_structure_samples` (the spaceknife branch).
fn zones(text: &str) -> Vec<&'static str> {
    let c = clean(text);
    [
        "zone_sparse_media",
        "zone_deep",
        "zone_flat_cache",
        "zone_edge",
    ]
    .into_iter()
    .filter(|z| c.contains(z))
    .collect()
}

/// The command line of the registry, run through the real argument parser and the scan.
fn run_scan(args: &[&str], columns: usize) -> (i32, String) {
    let mut argv = vec!["spaceknife-cli"];
    argv.extend_from_slice(args);
    let cli =
        <Cli as clap::Parser>::try_parse_from(argv).expect("the benchmark's command line parses");
    let Command::Scan(scan) = cli.command else {
        panic!("not a scan")
    };
    // rich: COLUMNS=200 on a pipe is 199 columns on Windows, 200 elsewhere
    let mut out = Console::capture(columns - usize::from(cfg!(windows)));
    out.legacy_windows = cfg!(windows);
    let code = super::super::scan::scan_with(&scan, &mut out);
    (code, out.take())
}

fn bench_tree(root: &Path) {
    tree(
        root,
        &[
            ("zone_sparse_media/a.bin", 300_000),
            ("zone_sparse_media/b.bin", 120_000),
            ("zone_deep/l0/l1/l2/l3/deep.txt", 5000),
            ("zone_flat_cache/c00.tmp", 4000),
            ("zone_flat_cache/c01.tmp", 9000),
            ("zone_flat_cache/c02.tmp", 1),
            ("zone_edge/中文/图片 一.jpg", 64_000),
            ("zone_edge/space name/x y.txt", 10),
            ("readme.md", 42),
        ],
    );
}

const OS_KEY: &str = if cfg!(target_os = "macos") {
    "macos"
} else if cfg!(windows) {
    "windows"
} else {
    "linux"
};

#[test]
fn cli_scan_output_parses_like_the_benchmark() {
    let dir = tempfile::tempdir().unwrap();
    let bench = dir.path().join("bench");
    bench_tree(&bench);
    let path = bench.to_string_lossy().into_owned();
    for lang in [sk_core::i18n::Language::En, sk_core::i18n::Language::ZhCn] {
        let _g = lock_language(lang);
        let (code, text) = run_scan(
            &["scan", &path, "--no-snapshot", "--top", "5", "--depth", "1"],
            200,
        );
        assert_eq!(code, 0, "{text}");
        // the scan's own numbers, to compare the parsed ones with
        let (scanner, truth) = sk_core::scan::scan_path(&bench, Default::default()).unwrap();
        let (bytes, files) = output(&text, "allocated", OS_KEY);
        assert_eq!(files, 9, "{text}");
        let err = (bytes - truth.bytes_alloc).abs() as f64 / truth.bytes_alloc as f64;
        assert!(err < 0.05, "{bytes} vs {}: {text}", truth.bytes_alloc);
        let (logical, _) = output(&text, "apparent", OS_KEY);
        assert!(
            ((logical - truth.bytes_size).abs() as f64) < 0.05 * truth.bytes_size as f64,
            "{text}"
        );
        assert!(scan_ms(&text).is_some_and(|ms| ms >= 0.0), "{text}");
        let found = zones(&text);
        assert_eq!(found.len(), 4, "{text}");
        let r = scanner.store().read();
        let root = r.roots().next().unwrap();
        for zone in found {
            let (zb, zf) = subtree(&text, zone, OS_KEY);
            let node = r.children(root).find(|&c| r.name(c) == zone).unwrap();
            assert_eq!(zf, r.tot_files(node) as i64, "{zone}: {text}");
            let want = r.tot_alloc(node);
            assert!(
                ((zb - want).abs() as f64) <= 0.05 * want.max(1) as f64 + 1.0,
                "{zone}: {zb} vs {want}"
            );
        }
        // a plain scan says nothing about the method
        assert_eq!(
            mft_fallback(&text).as_deref(),
            Some("no scan method line in the output")
        );
    }
}

#[test]
fn cli_scan_mft_fallback_is_reported_with_its_reason() {
    let dir = tempfile::tempdir().unwrap();
    let bench = dir.path().join("bench");
    bench_tree(&bench);
    let path = bench.to_string_lossy().into_owned();
    for lang in [sk_core::i18n::Language::En, sk_core::i18n::Language::ZhCn] {
        let _g = lock_language(lang);
        // the registry's MFT command line, on a folder: no MFT scan, the full scan with the reason
        let (code, text) = run_scan(
            &[
                "scan",
                &path,
                "--mft",
                "--no-snapshot",
                "--top",
                "5",
                "--depth",
                "2",
            ],
            200,
        );
        assert_eq!(code, 0, "{text}");
        let reason = mft_fallback(&text).expect("a fallback");
        let expected = super::super::scan::mft_unsupported(
            &sk_core::scan::Scanner::new(&[&path], Default::default())
                .unwrap()
                .roots(),
        )
        .unwrap();
        // the benchmark's lazy `\((.*?)\)` stops at the reason's own first ")" (as with Python's text)
        let joined = expected.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            !reason.is_empty() && joined.starts_with(&reason),
            "{reason:?} / {joined:?}: {text}"
        );
        // the benchmark subtree's row is there at depth 2 too
        assert!(subtree(&text, "zone_edge", OS_KEY).1 == 2, "{text}");
        assert_eq!(output(&text, "allocated", OS_KEY).1, 9);
    }
}

#[test]
fn cli_scan_method_line_of_an_mft_scan() {
    // the line an MFT scan prints, as the benchmark recognises it
    let _g = lock_language(sk_core::i18n::Language::En);
    let method = super::super::scan::ScanMethod {
        asked: true,
        mft: true,
        ..Default::default()
    };
    let dir = tempfile::tempdir().unwrap();
    tree(dir.path(), &[("a.txt", 10)]);
    let scanner =
        sk_core::scan::Scanner::new(&[dir.path().to_string_lossy()], Default::default()).unwrap();
    let progress = scanner.run(None, std::time::Duration::from_secs(1));
    let roots = scanner.roots();
    for lang in [sk_core::i18n::Language::En, sk_core::i18n::Language::ZhCn] {
        sk_core::i18n::set_language(lang);
        let mut out = Console::capture(120);
        out.panel(
            &super::super::scan::summary(&scanner, &roots, &progress, None, &method),
            "x",
        );
        assert_eq!(mft_fallback(&out.take()), None);
    }
}
