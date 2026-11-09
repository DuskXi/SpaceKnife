//! The `$MFT` parser and reader on synthetic images (the port of
//! packages/native/tests/test_mft_native.py and the parser part of
//! packages/backend/tests/test_mft.py; the images come from mft_image, a port of
//! backend/tests/mft_image.py).

mod mft_image;

use std::io::ErrorKind;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use mft_image as im;
use sk_native::mft::{
    self, MftTable, ScanOptions, ScanStats, image_info, parse_mft, parse_mft_stream,
    parse_mft_stream_concurrent, read_mft_image, scan_mft_image,
};

const RS: usize = 1024;
const CS: usize = 4096;

// flags (the store's Flag bits)
const HIDDEN: u32 = 1 << 5;
const SYSTEM: u32 = 1 << 6;
const READONLY: u32 = 1 << 7;
const REPARSE: u32 = 1 << 8;
const CLOUD: u32 = 1 << 9;
const ARCHIVE: u32 = 1 << 12;
const COMPRESSED: u32 = 1 << 3;
const SPARSE: u32 = 1 << 4;
const ENCRYPTED: u32 = 1 << 13;
const ADS: u32 = 1 << 18;
const FILE: u8 = 0;
const DIR: u8 = 1;
const LINK: u8 = 2;

fn sample() -> MftTable {
    parse_mft(&im::build_mft(&im::sample_records(), RS, CS, 0), RS, CS).unwrap()
}

fn names_of(t: &MftTable) -> Vec<Vec<u8>> {
    (0..t.rows()).map(|i| t.name(i).to_vec()).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Row {
    record: u64,
    parent: u64,
    kind: u8,
    flags: u32,
    size: i64,
    alloc: i64,
    ads_size: i64,
    ads_alloc: i64,
    mtime: i64,
    ctime: i64,
    atime: i64,
    nlink: u16,
}

fn row_at(t: &MftTable, i: usize) -> Row {
    Row {
        record: t.record[i],
        parent: t.parent[i],
        kind: t.kind[i],
        flags: t.flags[i],
        size: t.size[i],
        alloc: t.alloc[i],
        ads_size: t.ads_size[i],
        ads_alloc: t.ads_alloc[i],
        mtime: t.mtime[i],
        ctime: t.ctime[i],
        atime: t.atime[i],
        nlink: t.nlink[i],
    }
}

fn row(t: &MftTable, name: &str) -> Row {
    let i = (0..t.rows())
        .position(|i| t.name(i) == name.as_bytes())
        .unwrap_or_else(|| panic!("no row {name}"));
    row_at(t, i)
}

fn count(t: &MftTable, name: &[u8]) -> usize {
    (0..t.rows()).filter(|&i| t.name(i) == name).count()
}

fn check_columns(t: &MftTable) {
    let n = t.rows();
    for len in [
        t.parent.len(),
        t.name_off.len(),
        t.name_len.len(),
        t.kind.len(),
        t.flags.len(),
        t.size.len(),
        t.alloc.len(),
        t.ads_size.len(),
        t.ads_alloc.len(),
        t.mtime.len(),
        t.ctime.len(),
        t.atime.len(),
        t.nlink.len(),
    ] {
        assert_eq!(len, n);
    }
}

/// Rows in record order (stable: a record's names keep their order) and their names.
fn by_record(t: &MftTable) -> Vec<(Row, Vec<u8>)> {
    let mut order: Vec<usize> = (0..t.rows()).collect();
    order.sort_by_key(|&i| t.record[i]);
    order
        .into_iter()
        .map(|i| (row_at(t, i), t.name(i).to_vec()))
        .collect()
}

fn assert_same_rows(streamed: &MftTable, whole: &MftTable) {
    check_columns(streamed);
    assert_eq!(by_record(streamed), by_record(whole));
    assert_eq!(streamed.bad_records, whole.bad_records);
}

struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n
    }
}

// --------------------------------------------------------------------------- parser

#[test]
fn sample_image() {
    for rs in [1024, 4096] {
        let t = parse_mft(&im::build_mft(&im::sample_records(), rs, CS, 0), rs, CS).unwrap();
        check_columns(&t);
        // torn, bad usa, bad length, BAAD, bad first attribute
        assert_eq!((t.bad_records, t.rows()), (5, 60), "rs {rs}");
        assert!(t.record.windows(2).all(|w| w[0] <= w[1])); // rows in record order
    }
}

#[test]
fn names_and_namespaces() {
    let t = sample();
    let names = names_of(&t);
    let has = |n: &str| names.iter().any(|x| x == n.as_bytes());
    assert!(!has("PROGRA~1.TXT")); // DOS alias of a long name in the same directory
    assert!(has("Y~1")); // a DOS name alone in its directory stays
    assert_eq!(count(&t, b"hl-a.bin"), 1);
    assert_eq!(count(&t, b"hl-b.bin"), 1);
    assert!(has("CaseSensitive") && has("casesensitive"));
    for gone in [
        "deleted.txt",
        "torn",
        "bad-usa",
        "bad-length",
        "baad",
        "bad-first",
        "ghost",
        "ghost2",
    ] {
        assert!(!has(gone), "{gone}");
    }
    // lone surrogate as WTF-8 (surrogatepass)
    assert_eq!(count(&t, b"lone\xed\xa0\x80surrogate.txt"), 1);
    assert!(has("\u{1F600}.PNG") && has("文档.docx"));
    assert_eq!(row(&t, "hl-b.bin").nlink, 2);
    assert_eq!(row(&t, "x").nlink, 2);
    assert_eq!(row(&t, "Program Files Long.txt").nlink, 1);
    assert_eq!(row(&t, "hl-b.bin").parent, im::DOCS);
    assert_eq!(row(&t, ".").record, im::ROOT);
}

#[test]
fn sizes() {
    let t = sample();
    let r = row(&t, "readme.txt");
    assert_eq!((r.size, r.alloc), (100, 0));
    let r = row(&t, "big.iso");
    assert_eq!((r.size, r.alloc), (10_000_000, 10_002_432));
    let r = row(&t, "comp.log");
    assert_eq!((r.size, r.alloc), (1 << 20, 256 << 10));
    let r = row(&t, "sparse.vhd");
    assert_eq!((r.size, r.alloc), (1 << 30, 64 << 10));
    let r = row(&t, "with-ads.txt");
    assert_eq!(
        (r.size, r.alloc, r.ads_size, r.ads_alloc),
        (3000, 4096, 100_026, 102_400)
    );
    assert!(r.flags & ADS != 0);
    assert_eq!(row(&t, "docs").size, 0);
    assert_eq!(row(&t, "adsdir").ads_size, 9000);
    assert_eq!(row(&t, "$UsnJrnl").ads_alloc, 2 << 20);
}

#[test]
fn kinds_and_flags() {
    let t = sample();
    assert_eq!(row(&t, "docs").kind, DIR);
    assert_eq!(row(&t, "link.txt").kind, LINK);
    assert_eq!(row(&t, "junction").kind, LINK); // mount point: shown, never descended
    assert_eq!(row(&t, "long-target.lnk").kind, LINK); // tag not in the MFT
    let d = row(&t, "dedup.vhdx");
    assert!(d.kind == FILE && d.flags & REPARSE != 0); // not a name surrogate
    assert_eq!(row(&t, "hidden-sys-ro").flags, HIDDEN | SYSTEM | READONLY);
    assert_eq!(row(&t, "cloud.docx").flags, CLOUD | SPARSE);
    assert_eq!(row(&t, "comp.log").flags, ARCHIVE | COMPRESSED);
    assert_eq!(row(&t, "secret.bin").flags, ENCRYPTED);
}

#[test]
fn extension_records() {
    let t = sample();
    let (a, b) = (row(&t, "fragmented.dat"), row(&t, "frag-link.dat"));
    assert!(a.record == 78 && b.record == 78);
    assert_eq!((a.size, a.nlink, a.mtime), (7_000_000, 2, 1_700_000_000));
    assert_eq!(row(&t, "ads-in-ext").ads_size, 5000);
    assert_eq!(row(&t, "stale-ext.bin").size, 0); // its extension record has another sequence
}

#[test]
fn times() {
    let t = sample();
    let r = row(&t, "readme.txt");
    assert_eq!(
        (r.ctime, r.mtime, r.atime),
        (1_600_000_000, 1_700_000_000, 1_700_000_500)
    );
    let r = row(&t, "no-si");
    assert_eq!((r.ctime, r.mtime, r.atime, r.flags), (0, 0, 0, 0));
}

#[test]
fn record_size_4096_same_rows() {
    let other = parse_mft(&im::build_mft(&im::sample_records(), 4096, CS, 0), 4096, CS).unwrap();
    assert_eq!(other, sample());
}

#[test]
fn edge_inputs() {
    let empty = parse_mft(b"", RS, CS).unwrap();
    assert!(empty.rows() == 0 && empty.names.is_empty() && empty.bad_records == 0);
    let data = im::build_mft(&im::metafile_records(), RS, CS, 0);
    let mut tail = data.clone();
    tail.extend_from_slice(&[0xff; 100]); // a partial record is ignored
    assert_eq!(
        parse_mft(&tail, RS, CS).unwrap().record,
        parse_mft(&data, RS, CS).unwrap().record
    );
    for (rs, cs, what) in [
        (1000, CS, "record_size"),
        (RS, 0, "cluster_size"),
        (RS, 3000, "cluster_size"),
    ] {
        let e = parse_mft(&data, rs, cs).unwrap_err();
        assert_eq!(e.kind(), ErrorKind::InvalidInput);
        assert!(e.to_string().contains(what), "{e}");
        assert_eq!(
            parse_mft_stream(&data, rs, cs, 1).unwrap_err().kind(),
            ErrorKind::InvalidInput
        );
    }
}

#[test]
fn synthetic_image() {
    let data = im::synthetic_mft(60_000, 7, RS);
    let t = parse_mft(&data, RS, CS).unwrap();
    check_columns(&t);
    assert!(t.rows() > 60_000 && t.bad_records == 0);
    // the streaming parser, windows not a divisor of the parallel chunk
    assert_same_rows(&parse_mft_stream(&data, RS, CS, 4093).unwrap(), &t);
}

#[test]
fn fuzzed_images_parse_alike_whole_and_streamed() {
    // random corruption anywhere in the records: never a panic, and both parsers agree
    let mut rng = Rng(1234);
    let base = im::build_mft(&im::sample_records(), RS, CS, 0);
    let n = (base.len() / RS) as u64;
    let hot = [
        0x04, 0x06, 0x14, 0x16, 0x18, 0x20, 0x26, 0x10, 0x3A, 0x3C, 0x40, 0x50, 0x9C, 0xA0, 0xA2,
        0xB0,
    ];
    for _ in 0..300 {
        let mut img = base.clone();
        for _ in 0..1 + rng.below(11) {
            let rec = rng.below(n) as usize;
            let off = if rng.below(10) < 3 {
                hot[rng.below(hot.len() as u64) as usize]
            } else {
                rng.below(RS as u64) as usize
            };
            img[rec * RS + off] = rng.below(256) as u8;
        }
        let whole = parse_mft(&img, RS, CS).unwrap();
        check_columns(&whole);
        let window = 1 + rng.below(8) as usize;
        assert_same_rows(&parse_mft_stream(&img, RS, CS, window).unwrap(), &whole);
    }
}

#[test]
fn streaming_parse_matches() {
    // extension records before and after their base, in other windows: same rows
    for rs in [1024, 4096] {
        let data = im::build_mft(&im::sample_records(), rs, CS, 0);
        let whole = parse_mft(&data, rs, CS).unwrap();
        for window in [1, 2, 7, 64, 100_000] {
            assert_same_rows(&parse_mft_stream(&data, rs, CS, window).unwrap(), &whole);
        }
    }
}

#[test]
fn decode_runs() {
    let mp = [
        0x21, 0x10, 0x00, 0x01, 0x01, 0x05, 0x31, 0x02, 0xFF, 0xFF, 0xFF, 0,
    ];
    assert_eq!(
        mft::decode_runs(&mp).unwrap(),
        [
            (0, Some(0x100), 0x10),
            (0x10, None, 5),
            (0x15, Some(0xFF), 2)
        ]
    );
    assert!(mft::decode_runs(&[0x21, 0x10]).is_none());
    assert_eq!(
        mft::decode_runs(&im::encode_runs(&[(Some(5), 3), (None, 2), (Some(1), 7)])).unwrap(),
        [(0, Some(5), 3), (3, None, 2), (5, Some(1), 7)]
    );
}

// --------------------------------------------------------------------------- volume images

type Case = (
    &'static [(u64, u64)],
    Option<usize>,
    Option<usize>,
    usize,
    usize,
    usize,
);

const CASES: [Case; 5] = [
    (&[(10, 20)], None, None, 1024, 4096, 0),
    (
        &[(40, 4), (12, 3), (90, 11)],
        None,
        None,
        1024,
        4096,
        3 * 1024,
    ), // fragmented, backwards
    (
        &[(40, 6), (12, 3), (90, 9)],
        Some(1),
        None,
        1024,
        4096,
        5 * 1024,
    ), // in extension record 16
    (
        &[(40, 6), (12, 3), (90, 9)],
        Some(2),
        Some(50),
        1024,
        4096,
        1024,
    ), // valid length < size
    (
        &[(64, 201), (1000, 43), (600, 300)],
        Some(1),
        None,
        4096,
        512,
        3 * 4096,
    ), // records span runs
];

fn volume_records() -> Vec<im::Rec> {
    im::sample_records()
        .into_iter()
        .filter(|r| r.number <= 66)
        .collect()
}

fn build(dir: &Path, case: &Case) -> (std::path::PathBuf, Vec<u8>) {
    let (runs, split, valid, rs, cs, _) = *case;
    let img = dir.join("vol.img");
    let expected = im::build_volume(&img, &volume_records(), runs, rs, cs, split, valid);
    (img, expected)
}

#[test]
fn read_mft_images() {
    let tmp = tempfile::tempdir().unwrap();
    for case in &CASES {
        let (runs, _split, valid, rs, cs, _) = *case;
        let (img, expected) = build(tmp.path(), case);
        let data = read_mft_image(&img).unwrap();
        assert!(data == expected, "{case:?}");
        let info = image_info(&img).unwrap();
        assert_eq!((info.record_size, info.cluster_size), (rs, cs));
        assert_eq!(info.mft_lcn, runs[0].0);
        assert_eq!(info.mft_size, expected.len() as u64);
        assert_eq!(info.mft_fragments, runs.len());
        assert_eq!(info.bytes_per_sector, 512);
        assert!(info.mft_valid_size <= info.mft_size);
        let t = parse_mft(&data, rs, cs).unwrap();
        assert_eq!(count(&t, b"big.iso") == 1, valid.is_none());
        assert_eq!(t.bad_records, 0);
    }
}

#[test]
fn fuzzed_volume_images_never_panic() {
    // corrupt boot sector / record 0 / extension record 16: the bytes, or an error
    let tmp = tempfile::tempdir().unwrap();
    let mut rng = Rng(99);
    let runs = [(40u64, 6u64), (12, 3), (90, 9)];
    let base = tmp.path().join("base.img");
    let expected = im::build_volume(&base, &volume_records(), &runs, RS, CS, Some(1), None);
    let raw = std::fs::read(&base).unwrap();
    let mft_at = runs[0].0 as usize * CS;
    let targets = [(0usize, 512usize), (mft_at, RS), (mft_at + 16 * RS, RS)];
    let img = tmp.path().join("fuzzed.img");
    let mut same = 0;
    for _ in 0..150 {
        let mut data = raw.clone();
        for _ in 0..1 + rng.below(4) {
            let (start, span) = targets[rng.below(3) as usize];
            data[start + rng.below(span as u64) as usize] = rng.below(256) as u8;
        }
        std::fs::write(&img, &data).unwrap();
        if let Ok(got) = read_mft_image(&img) {
            same += usize::from(got == expected);
            let info = image_info(&img).unwrap();
            assert_eq!(got.len() as u64, info.mft_size);
        }
    }
    assert!(same > 0);
}

#[test]
fn read_mft_image_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let img = tmp.path().join("zero.img");
    std::fs::write(&img, vec![0u8; 1 << 16]).unwrap();
    let e = read_mft_image(&img).unwrap_err();
    assert_eq!(e.kind(), ErrorKind::InvalidData);
    assert!(e.to_string().contains("not an NTFS volume"));
    let e = image_info(tmp.path().join("missing.img")).unwrap_err();
    assert_eq!(e.kind(), ErrorKind::NotFound);
}

#[cfg(windows)]
fn elevated() -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::{
        GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    // SAFETY: plain Win32 calls with valid out-pointers; the token is closed.
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return true; // unknown: treat as elevated (never touch the volume)
        }
        let mut e = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            (&raw mut e).cast(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        );
        CloseHandle(token);
        ok == 0 || e.TokenIsElevated != 0
    }
}

#[test]
fn volume_functions() {
    if !cfg!(windows) {
        for e in [
            mft::volume_info("C:").unwrap_err(),
            mft::read_mft("C:").unwrap_err(),
        ] {
            assert_eq!(e.kind(), ErrorKind::Unsupported);
        }
        return;
    }
    for e in [
        mft::volume_info("C:\\Windows").unwrap_err(),
        mft::read_mft("C:\\Windows").unwrap_err(),
        mft::scan_mft("C:\\Windows", ScanOptions::default()).unwrap_err(),
    ] {
        assert_eq!(e.kind(), ErrorKind::InvalidInput);
        assert!(e.to_string().contains("local volume"));
    }
    #[cfg(windows)]
    {
        if elevated() {
            eprintln!("elevated: opening the volume would succeed (never reads the $MFT in tests)");
            return;
        }
        let drive = std::env::var("SYSTEMDRIVE").unwrap_or_else(|_| "C:".into());
        for e in [
            mft::volume_info(&drive).unwrap_err(),
            mft::read_mft(&drive).unwrap_err(),
        ] {
            assert_eq!(e.kind(), ErrorKind::PermissionDenied, "{e}");
        }
    }
}

#[test]
#[ignore = "reads the system volume's boot sector and $MFT record 0 (elevated)"]
fn volume_info_of_system_volume() {
    let drive = std::env::var("SYSTEMDRIVE").unwrap_or_else(|_| "C:".into());
    let info = mft::volume_info(&drive).unwrap();
    assert_eq!(info.mft_size % info.record_size as u64, 0);
    assert!(info.mft_valid_size <= info.mft_size);
}

// --------------------------------------------------------------------------- streaming (scan_mft)

#[test]
fn scan_mft_image_streams_the_volume() {
    let tmp = tempfile::tempdir().unwrap();
    for case in &CASES {
        let (_runs, _split, _valid, rs, cs, window) = *case;
        let (img, _) = build(tmp.path(), case);
        let opts = |duty: f64| ScanOptions {
            duty,
            window,
            ..ScanOptions::default()
        };
        let (table, info) = scan_mft_image(&img, opts(1.0)).unwrap();
        assert_eq!(info, image_info(&img).unwrap());
        let whole = parse_mft(&read_mft_image(&img).unwrap(), rs, cs).unwrap();
        assert_same_rows(&table, &whole);
        // an I/O limit changes timing only
        let (paced, _) = scan_mft_image(&img, opts(0.5)).unwrap();
        assert_same_rows(&paced, &table);
        for duty in [0.0, -1.0, 1.5, f64::NAN] {
            let e = scan_mft_image(&img, opts(duty)).unwrap_err();
            assert_eq!(e.kind(), ErrorKind::InvalidInput);
            assert!(e.to_string().contains("duty"));
        }

        // progress: (done, total) from 0 to all of it, once per window
        let mut seen: Vec<(u64, u64)> = Vec::new();
        let mut record = |done, total| seen.push((done, total));
        scan_mft_image(
            &img,
            ScanOptions {
                progress: Some(&mut record),
                ..opts(1.0)
            },
        )
        .unwrap();
        let total = seen[0].1;
        assert!(total > 0);
        assert_eq!(seen[0], (0, total));
        assert_eq!(*seen.last().unwrap(), (total, total));
        assert!(seen.windows(2).all(|w| w[1].0 > w[0].0));
        assert!(seen.iter().all(|s| s.1 == total));
        if window != 0 {
            // windows are whole records
            let per = (window / rs).max(1) * rs;
            assert_eq!(seen.len() - 1, (total as usize).div_ceil(per));
        }
    }
}

#[test]
fn scan_mft_image_can_be_cancelled() {
    let tmp = tempfile::tempdir().unwrap();
    let (img, _) = build(tmp.path(), &CASES[1]);
    let cancel = Arc::new(AtomicBool::new(true));
    let e = scan_mft_image(
        &img,
        ScanOptions {
            cancel: Some(cancel.clone()),
            ..ScanOptions::default()
        },
    )
    .unwrap_err();
    assert_eq!(e.kind(), ErrorKind::Interrupted);
    // cancelled from the progress callback after the first window
    cancel.store(false, Ordering::Relaxed);
    let flag = cancel.clone();
    let mut calls = 0;
    let mut stop = |done: u64, _total: u64| {
        calls += 1;
        if done > 0 {
            flag.store(true, Ordering::Relaxed);
        }
    };
    let e = scan_mft_image(
        &img,
        ScanOptions {
            window: RS,
            progress: Some(&mut stop),
            cancel: Some(cancel.clone()),
            ..ScanOptions::default()
        },
    )
    .unwrap_err();
    assert_eq!(e.kind(), ErrorKind::Interrupted);
    assert_eq!(calls, 2);
}

#[test]
fn a_panic_in_the_parse_is_the_scans_error() {
    // rayon aborts the whole process for a panic in a spawned job: a parser bug would take the
    // window (and the backend in it) down. It must come back as the scan's error, and the next
    // scan must work
    let tmp = tempfile::tempdir().unwrap();
    let (img, _) = build(tmp.path(), &CASES[1]);
    let good = scan_mft_image(&img, ScanOptions::default()).unwrap().0;
    for at in [0u64, 3, 6] {
        let hook: mft::ParseHook = Arc::new(move |first| {
            if first == at {
                panic!("injected parser bug at record {first}");
            }
        });
        let e = scan_mft_image(
            &img,
            ScanOptions {
                window: 3 * RS,
                before_parse: Some(hook),
                ..ScanOptions::default()
            },
        )
        .unwrap_err();
        assert!(e.to_string().contains("injected parser bug"), "{at}: {e}");
    }
    // a panic in the scan's own thread (here: the progress callback) too
    let mut boom = |done: u64, _total: u64| {
        if done > 0 {
            panic!("injected progress bug");
        }
    };
    let e = scan_mft_image(
        &img,
        ScanOptions {
            window: 3 * RS,
            progress: Some(&mut boom),
            ..ScanOptions::default()
        },
    )
    .unwrap_err();
    assert!(e.to_string().contains("injected progress bug"), "{e}");
    let again = scan_mft_image(&img, ScanOptions::default()).unwrap().0;
    assert_same_rows(&again, &good);
}

// --------------------------------------------------------------------------- concurrent streaming

/// A sample image, a synthetic one and six fuzzed copies of the sample.
fn stream_images() -> Vec<(String, Vec<u8>)> {
    let mut rng = Rng(5);
    let base = im::build_mft(&im::sample_records(), RS, CS, 0);
    let mut out = vec![
        ("sample".to_string(), base.clone()),
        ("synthetic".to_string(), im::synthetic_mft(20_000, 13, RS)),
    ];
    let n = (base.len() / RS) as u64;
    for k in 0..6 {
        let mut img = base.clone();
        for _ in 0..1 + rng.below(9) {
            img[rng.below(n) as usize * RS + rng.below(RS as u64) as usize] = rng.below(256) as u8;
        }
        out.push((format!("fuzzed{k}"), img));
    }
    out
}

#[test]
fn concurrent_streaming_is_the_sequential_stream() {
    // windows parsed in parallel, in shuffled order, then put back in record order: the same
    // table as feeding them one by one, row order and name blob included, whatever the window
    for (name, data) in stream_images() {
        let reference = parse_mft_stream(&data, RS, CS, 64).unwrap();
        for window in [1, 3, 64, 1000, 100_000] {
            assert!(
                parse_mft_stream(&data, RS, CS, window).unwrap() == reference,
                "{name} window {window}"
            );
            for seed in 0..3 {
                let got = parse_mft_stream_concurrent(&data, RS, CS, window, seed).unwrap();
                assert!(got == reference, "{name} window {window} seed {seed}");
            }
        }
    }
}

#[test]
fn scan_mft_image_rows_are_the_streams() {
    // reads in flight, windows parsed out of order: the rows of the in-memory stream, in order
    let tmp = tempfile::tempdir().unwrap();
    let mut cases: Vec<Case> = CASES.to_vec();
    cases.push((
        &[(64, 201), (1000, 43), (600, 300)],
        Some(1),
        None,
        4096,
        512,
        0,
    ));
    for case in &cases {
        let (_runs, _split, valid, rs, cs, window) = *case;
        let (img, _) = build(tmp.path(), case);
        let raw = read_mft_image(&img).unwrap();
        let total = valid.map_or(raw.len(), |v| v * rs);
        let mut stats = ScanStats::default();
        let (table, _) = scan_mft_image(
            &img,
            ScanOptions {
                window,
                stats: Some(&mut stats),
                ..ScanOptions::default()
            },
        )
        .unwrap();
        let per = if window == 0 {
            mft::DEFAULT_WINDOW
        } else {
            window
        };
        let stream = parse_mft_stream(&raw[..total], rs, cs, (per / rs).max(1)).unwrap();
        assert!(table == stream, "{case:?}");
        assert!(
            stats.windows >= 1 && stats.reads + 1 >= stats.windows,
            "{stats:?}"
        );
        assert!(1 <= stats.depth && stats.depth <= stats.tuning.depth_max);
        assert!(stats.max_in_flight >= 1);
        for v in [
            stats.read_s,
            stats.busy_s,
            stats.paused_s,
            stats.parse_cpu_s,
            stats.finish_s,
            stats.total_s,
        ] {
            assert!(v >= 0.0, "{stats:?}");
        }
        let mut paced = ScanStats::default();
        let (slow, _) = scan_mft_image(
            &img,
            ScanOptions {
                duty: 0.3,
                window,
                stats: Some(&mut paced),
                ..ScanOptions::default()
            },
        )
        .unwrap();
        assert!(slow == table, "{case:?}");
        assert!(paced.paused_s >= 0.0);
    }
}

#[test]
fn scan_mft_image_fixed_depth() {
    let tmp = tempfile::tempdir().unwrap();
    let img = tmp.path().join("vol.img");
    im::build_volume(
        &img,
        &volume_records(),
        &[(40, 6), (12, 3), (90, 9)],
        RS,
        CS,
        Some(1),
        None,
    );
    let (reference, _) = scan_mft_image(
        &img,
        ScanOptions {
            window: 1024,
            ..ScanOptions::default()
        },
    )
    .unwrap();
    for depth in [1, 3] {
        let mut stats = ScanStats::default();
        let (table, _) = scan_mft_image(
            &img,
            ScanOptions {
                window: 1024,
                depth,
                stats: Some(&mut stats),
                ..ScanOptions::default()
            },
        )
        .unwrap();
        assert_eq!((stats.depth, stats.tuning.depth_max), (depth, depth));
        assert!(stats.max_in_flight <= depth + 1); // a window across two runs has two reads
        assert!(table == reference);
    }
}

#[test]
fn scan_mft_image_read_errors() {
    // a read that fails mid-scan (the image ends inside the $MFT) is an error, and the scan
    // leaves nothing behind (every read in flight is waited for)
    let tmp = tempfile::tempdir().unwrap();
    let img = tmp.path().join("vol.img");
    im::build_volume(
        &img,
        &volume_records(),
        &[(10, 4), (200, 30)],
        RS,
        CS,
        None,
        None,
    );
    let data = std::fs::read(&img).unwrap();
    std::fs::write(&img, &data[..210 * CS]).unwrap(); // the second run's clusters stop short
    for (duty, window) in [(1.0, 1024), (0.5, 0)] {
        let e = scan_mft_image(
            &img,
            ScanOptions {
                duty,
                window,
                ..ScanOptions::default()
            },
        )
        .unwrap_err();
        assert!(e.to_string().contains("read"), "{e}");
    }
}

#[test]
fn paced_scans_rest_between_bursts() {
    // under an I/O limit the reads pause in proportion to their busy time (7835ece: each burst
    // keeps the disk busy at least 50 ms, so short images are one burst and may never pause)
    let tmp = tempfile::tempdir().unwrap();
    let (img, _) = build(tmp.path(), &CASES[4]);
    let mut stats = ScanStats::default();
    scan_mft_image(
        &img,
        ScanOptions {
            duty: 0.25,
            window: 4096,
            stats: Some(&mut stats),
            ..ScanOptions::default()
        },
    )
    .unwrap();
    // pauses are busy × (1 − duty) / duty = 3 × busy, bursts included
    assert!(stats.paused_s <= 3.0 * stats.busy_s + 1e-6, "{stats:?}");
}

/// Scan speed on a large synthetic volume image (from the file cache: the pipeline and the
/// parse, not the disk). `cargo test --release -p sk-native --test mft -- --ignored --nocapture
/// scan_speed`; `SK_MFT_IMAGE` keeps the image at that path (built when missing),
/// `SK_MFT_RECORDS` sets its records (default 500,000).
#[test]
#[ignore = "benchmark"]
fn scan_speed() {
    let tmp = tempfile::tempdir().unwrap();
    let img = std::env::var_os("SK_MFT_IMAGE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| tmp.path().join("big.img"));
    if !img.exists() {
        let n: usize = std::env::var("SK_MFT_RECORDS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(500_000);
        let recs = im::synthetic_records(n, 11);
        let clusters = ((n + 64) * RS).div_ceil(CS) as u64 + 1;
        // three fragments, the middle one elsewhere on the volume
        let third = clusters / 3;
        let runs = [
            (100, third),
            (100 + 2 * clusters, third),
            (100 + third, clusters - 2 * third),
        ];
        im::build_volume(&img, &recs, &runs, RS, CS, None, None);
    }
    let raw = read_mft_image(&img).unwrap();
    for _ in 0..3 {
        let t0 = std::time::Instant::now();
        let whole = parse_mft(&raw, RS, CS).unwrap();
        let parse_s = t0.elapsed().as_secs_f64();
        let t0 = std::time::Instant::now();
        let (table, _) = scan_mft_image(&img, ScanOptions::default()).unwrap();
        let scan_s = t0.elapsed().as_secs_f64();
        let t0 = std::time::Instant::now();
        let stream = parse_mft_stream(&raw, RS, CS, (8 << 20) / RS).unwrap();
        let stream_s = t0.elapsed().as_secs_f64();
        assert_eq!(table.rows(), whole.rows());
        assert_eq!(stream.rows(), whole.rows());
        eprintln!(
            "{} MB, {} rows: parse_mft {parse_s:.3} s, parse_mft_stream {stream_s:.3} s, scan_mft_image {scan_s:.3} s",
            raw.len() >> 20,
            table.rows()
        );
    }
}
