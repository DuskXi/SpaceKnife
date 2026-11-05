//! Snapshot files (test_snapshot.py, test_memory_lean.py, test_store_layout.py).

use std::sync::Arc;

use sk_store::snapshot::{self, Codec, SaveOptions, SnapshotError};
use sk_store::testing::{self, d, f};
use sk_store::*;

fn scanned() -> Store {
    let spec = d([
        (
            "a",
            d([
                ("one.bin", f(1000)),
                ("two.bin", f(5000)),
                ("sub", d([("movie.mp4", f(123456))])),
            ]),
        ),
        ("中文 文件夹", d([("照片.jpg", f(2048))])),
        ("empty.dat", f(0)),
        ("b", d([("inner", d([]))])),
    ]);
    testing::build_with(&spec, "/root", 4096, &[])
}

#[test]
fn roundtrip_is_lossless_with_both_codecs() {
    let dir = tempfile::tempdir().unwrap();
    let store = scanned();
    for codec in [Codec::Zstd, Codec::Raw] {
        let path = dir.path().join(format!("snap-{codec:?}.sks"));
        let mut opts = SaveOptions::roots(["/root"]);
        opts.codec = codec;
        opts.stats.insert("files".into(), 1.into());
        let header = snapshot::save(&store.read(), &path, &opts).unwrap();
        assert!(path.exists());
        assert!(!dir.path().join(format!("snap-{codec:?}.sks.tmp")).exists());
        let (loaded, h) = snapshot::load(&path).unwrap();
        assert_eq!(h.node_count, header.node_count);
        assert_eq!(h.node_count as usize, store.len());
        assert_eq!(h.stats["files"], 1);
        let (a, b) = (store.read(), loaded.read());
        testing::assert_same_store(&a, &b);
        assert_eq!(loaded.ext_table(), store.ext_table());
        let last = a.len() as NodeId - 1;
        assert_eq!(b.path_bytes(last), a.path_bytes(last));
        assert_eq!(snapshot::read_header(&path).unwrap().roots, h.roots);
        assert!(loaded.appender().is_settled());
    }
}

#[test]
fn a_store_changed_after_loading_saves_again() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.sks");
    snapshot::save(&scanned().read(), &path, &SaveOptions::default()).unwrap();
    let (loaded, _) = snapshot::load(&path).unwrap();
    {
        let mut w = loaded.write();
        let a = testing::find(&w, "a");
        let mut b = EntryBatch::new();
        b.push(Entry::file(b"one.bin", 7, 4096));
        b.push(Entry::file(b"three.bin", 9, 4096));
        mutate::relist(&mut w, a, &b).unwrap();
    }
    let again = dir.path().join("t.sks");
    snapshot::save(&loaded.read(), &again, &SaveOptions::default()).unwrap();
    let (back, _) = snapshot::load(&again).unwrap();
    testing::assert_same_store(&loaded.read(), &back.read());
    testing::check_invariants(&back.read()).unwrap();
}

#[test]
fn corrupt_files_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let store = testing::random_store(3, 2000);
    for codec in [Codec::Zstd, Codec::Raw] {
        let good = dir.path().join("good.sks");
        let opts = SaveOptions {
            codec,
            ..SaveOptions::default()
        };
        snapshot::save(&store.read(), &good, &opts).unwrap();
        let bytes = std::fs::read(&good).unwrap();

        let bad = dir.path().join("bad.sks");
        let mut b = bytes.clone();
        b[..8].copy_from_slice(b"NOTASNAP");
        std::fs::write(&bad, &b).unwrap();
        assert!(matches!(snapshot::load(&bad), Err(SnapshotError::BadMagic)));

        std::fs::write(&bad, &bytes[..bytes.len() - 50]).unwrap();
        assert!(snapshot::load(&bad).is_err());

        if codec == Codec::Zstd {
            let mut b = bytes.clone();
            let n = b.len();
            b[n - 40..n - 20].fill(0xff); // inside the names block
            std::fs::write(&bad, &b).unwrap();
            assert!(matches!(
                snapshot::load(&bad),
                Err(SnapshotError::Corrupt(_))
            ));
        } else {
            // raw: a damaged name separator or parent is caught by the structure check
            let mut b = bytes.clone();
            let n = b.len();
            b[n - 40..n - 20].fill(0xff);
            std::fs::write(&bad, &b).unwrap();
            match snapshot::load(&bad) {
                Err(SnapshotError::Corrupt(_)) => {}
                Ok((s, _)) => assert_eq!(s.len(), store.len()), // names only changed: still a tree
                Err(e) => panic!("{e}"),
            }
        }
    }
}

#[test]
fn retention_keeps_recent_and_one_per_week() {
    let dir = tempfile::tempdir().unwrap();
    let start = snapshot::parse_stamp("20260101-000000").unwrap();
    for day in 0..60 {
        let name = format!("{}.sks", snapshot::stamp(start + day * 86400));
        std::fs::write(dir.path().join(name), b"").unwrap();
    }
    std::fs::write(dir.path().join("00-not-a-stamp.sks"), b"").unwrap();
    let deleted = snapshot::prune(dir.path(), 10).unwrap();
    let mut kept: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    kept.sort();
    assert!(kept.contains(&"00-not-a-stamp.sks".to_string()));
    kept.retain(|k| k != "00-not-a-stamp.sks");
    assert_eq!(kept.len() + deleted.len(), 60);
    let newest: Vec<String> = (50..60)
        .map(|d| format!("{}.sks", snapshot::stamp(start + d * 86400)))
        .collect();
    assert!(newest.iter().all(|n| kept.contains(n)));
    let older = kept.len() - 10;
    assert!((8..=9).contains(&older), "{older} weeks kept"); // 50 days: one per ISO week
}

#[test]
fn auto_path_and_listing() {
    let dir = tempfile::tempdir().unwrap();
    let store = scanned();
    let roots = vec!["/root".to_string()];
    let path = snapshot::auto_path(&roots, Some(1_800_000_000), dir.path());
    assert!(path.starts_with(dir.path()));
    let mut opts = SaveOptions::roots(roots.clone());
    opts.created_at = Some(1_800_000_000);
    snapshot::save(&store.read(), &path, &opts).unwrap();
    let older = snapshot::auto_path(&roots, Some(1_700_000_000), dir.path());
    opts.created_at = Some(1_700_000_000);
    snapshot::save(&store.read(), &older, &opts).unwrap();
    let listed: Vec<_> = snapshot::list_snapshots(dir.path())
        .into_iter()
        .map(|(p, _)| p)
        .collect();
    assert_eq!(listed, vec![path, older]);
}

#[test]
fn snapshots_load_into_mapped_files_beyond_the_budget() {
    let dir = tempfile::tempdir().unwrap();
    let cols = tempfile::tempdir().unwrap();
    let store = testing::random_store(5, 3000);
    let path = dir.path().join("m.sks");
    snapshot::save(&store.read(), &path, &SaveOptions::default()).unwrap();
    let policy = Arc::new(Policy::new());
    policy.set_force_bytes(Some(0));
    policy.set_columns_dir(cols.path().to_path_buf());
    let (loaded, _) = snapshot::load_with(
        &path,
        StoreOptions {
            policy,
            ..StoreOptions::default()
        },
    )
    .unwrap();
    let r = loaded.read();
    assert!(r.columns_mapped() && r.names_mapped());
    assert!(loaded.ram_bytes() < 4096);
    testing::assert_same_store(&store.read(), &r);
    drop(r);
    drop(loaded);
    assert_eq!(std::fs::read_dir(cols.path()).unwrap().count(), 0); // the files went with it
}

/// A store whose names span many heap chunks (test_snapshot.py `_many_chunks`).
fn many_chunks() -> Store {
    let store = Store::with_options(StoreOptions {
        first_chunk: 4096,
        max_chunk: 4096,
        ..StoreOptions::default()
    });
    let mut a = store.appender();
    let mut folder = a.add_root_dir("/r").unwrap();
    for k in 0..6 {
        let names: Vec<String> = (0..500)
            .map(|i| format!("name-{k}-{i:04}-{}", "é".repeat(i % 7)))
            .collect();
        let mut b = EntryBatch::new();
        for (i, name) in names.iter().enumerate() {
            if i == 0 {
                b.push(Entry::dir(name.as_bytes()));
            } else {
                b.push(Entry::file(name.as_bytes(), (k * 1000 + i) as i64, 4096));
            }
        }
        folder = a.ingest_listing(folder, &b).unwrap();
    }
    drop(a);
    assert!(store.read().name_chunks() > 1);
    store
}

#[test]
fn streamed_frames_load_back_with_any_thread_count() {
    let dir = tempfile::tempdir().unwrap();
    let store = many_chunks();
    let mut headers = Vec::new();
    for threads in [1, 2, 4, 0] {
        let path = dir.path().join(format!("t{threads}.sks"));
        let opts = SaveOptions {
            threads,
            ..SaveOptions::roots(["/r"])
        };
        let header = snapshot::save(&store.read(), &path, &opts).unwrap();
        assert!(header.names.frames.len() > 1);
        assert_eq!(header, snapshot::read_header(&path).unwrap());
        let (loaded, _) = snapshot::load(&path).unwrap();
        testing::assert_same_store(&store.read(), &loaded.read());
        // the header sits in room reserved for the largest numbers, padded with spaces
        let raw = std::fs::read(&path).unwrap();
        let hlen = u32::from_le_bytes(raw[8..12].try_into().unwrap()) as usize;
        let head = &raw[12..12 + hlen];
        assert!(head.ends_with(b" "));
        assert_eq!(head.trim_ascii_end(), serde_json::to_vec(&header).unwrap());
        headers.push(header);
    }
    // the frames are the same whatever the thread count
    for h in &headers[1..] {
        assert_eq!(h.columns, headers[0].columns);
        assert_eq!(h.names, headers[0].names);
    }
}

#[test]
fn a_failed_save_leaves_nothing_behind() {
    let dir = tempfile::tempdir().unwrap();
    let store = scanned();
    let path = dir.path().join("x.sks");
    std::fs::create_dir(&path).unwrap(); // the rename onto it fails
    std::fs::write(path.join("keep"), b"the previous content").unwrap();
    assert!(snapshot::save(&store.read(), &path, &SaveOptions::default()).is_err());
    assert!(!dir.path().join("x.sks.tmp").exists());
    assert_eq!(
        std::fs::read(path.join("keep")).unwrap(),
        b"the previous content"
    );
}
