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
    // real snapshots: retention only counts (and deletes) files it can read
    let one = dir.path().join("one.snapshot");
    snapshot::save(&scanned().read(), &one, &SaveOptions::default()).unwrap();
    for day in 0..60 {
        let name = format!("{}.sks", snapshot::stamp(start + day * 86400));
        std::fs::copy(&one, dir.path().join(name)).unwrap();
    }
    std::fs::remove_file(&one).unwrap();
    std::fs::copy(
        dir.path().join(format!("{}.sks", snapshot::stamp(start))),
        dir.path().join("00-not-a-stamp.sks"),
    )
    .unwrap();
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

/// A manual save in the same second as the auto-save gets a name of its own (it overwrote the
/// auto-save before); the names sort in the order they were made and prune reads them.
#[test]
fn saves_in_one_second_do_not_overwrite_each_other() {
    let dir = tempfile::tempdir().unwrap();
    let store = scanned();
    let roots = vec!["/root".to_string()];
    let when = 1_800_000_000;
    let mut made = Vec::new();
    for _ in 0..3 {
        let path = snapshot::auto_path(&roots, Some(when), dir.path());
        assert!(!made.contains(&path), "{path:?} again");
        let mut opts = SaveOptions::roots(roots.clone());
        opts.created_at = Some(when);
        snapshot::save(&store.read(), &path, &opts).unwrap();
        made.push(path);
    }
    let names: Vec<String> = made
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    let stamp = snapshot::stamp(when);
    assert_eq!(
        names,
        [
            format!("{stamp}.sks"),
            format!("{stamp}_001.sks"),
            format!("{stamp}_002.sks")
        ]
    );
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(sorted, names, "later saves sort after earlier ones");
    for p in &made {
        let stem = p.file_stem().unwrap().to_str().unwrap();
        assert_eq!(snapshot::parse_stamp(stem), Some(when));
    }
    // all three are the newest: kept; with one kept, the others of that week go
    let folder = made[0].parent().unwrap();
    assert!(snapshot::prune(folder, 3).unwrap().is_empty());
    let deleted = snapshot::prune(folder, 1).unwrap();
    assert_eq!(deleted.len(), 1, "{deleted:?}"); // the newest is kept, plus one per ISO week
    assert!(made[2].exists());
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

/// With `SK_TEST_SMALL_FS` naming a folder on a small file system (a tmpfs of 4 MiB), a snapshot
/// larger than that, opened with every column to be mapped there, fills what room there is and
/// keeps the rest in memory: it opens whole, and nothing is left behind (review issue 21).
#[test]
fn snapshots_open_whole_when_the_mapped_files_run_out_of_room() {
    let Some(small) = std::env::var_os("SK_TEST_SMALL_FS") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new();
    {
        // 100 000 files: about 10 MB of columns and names
        let mut a = store.appender();
        let root = a.add_root_dir("/big").unwrap();
        let names: Vec<String> = (0..100_000).map(|i| format!("file-{i:06}.bin")).collect();
        let mut b = EntryBatch::new();
        for (i, name) in names.iter().enumerate() {
            b.push(Entry::file(name.as_bytes(), i as i64, 4096));
        }
        a.ingest_listing(root, &b).unwrap();
    }
    let path = dir.path().join("big.sks");
    snapshot::save(&store.read(), &path, &SaveOptions::default()).unwrap();
    let policy = Arc::new(Policy::new());
    policy.set_force_bytes(Some(0));
    policy.set_columns_dir(std::path::PathBuf::from(&small));
    let (loaded, _) = snapshot::load_with(
        &path,
        StoreOptions {
            policy,
            ..StoreOptions::default()
        },
    )
    .unwrap();
    testing::assert_same_store(&store.read(), &loaded.read());
    assert!(loaded.ram_bytes() > 0, "what did not fit is in memory");
    drop(loaded);
    assert_eq!(std::fs::read_dir(&small).unwrap().count(), 0);
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

/// The bytes of a file split into magic, header length, header (JSON), and the data after it.
fn split_file(raw: &[u8]) -> (serde_json::Value, Vec<u8>) {
    let hlen = u32::from_le_bytes(raw[8..12].try_into().unwrap()) as usize;
    let head: serde_json::Value =
        serde_json::from_slice(raw[12..12 + hlen].trim_ascii_end()).unwrap();
    (head, raw[12 + hlen..].to_vec())
}

fn join_file(magic: &[u8; 8], head: &serde_json::Value, data: &[u8]) -> Vec<u8> {
    let head = serde_json::to_vec(head).unwrap();
    let mut out = magic.to_vec();
    out.extend_from_slice(&(head.len() as u32).to_le_bytes());
    out.extend_from_slice(&head);
    out.extend_from_slice(data);
    out
}

/// A snapshot whose parents form a cycle (5 → 6 → 5, or a node its own parent) is refused when
/// it is opened: walking up from such a node never ends (review issue 19).
#[test]
fn parents_that_form_a_cycle_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let store = scanned();
    let good = dir.path().join("good.sks");
    let opts = SaveOptions {
        codec: Codec::Raw, // the parent column as it is in the file
        ..SaveOptions::default()
    };
    snapshot::save(&store.read(), &good, &opts).unwrap();
    let raw = std::fs::read(&good).unwrap();
    let (head, data) = split_file(&raw);
    let parent_at = head["columns"]["parent"]["frames"][0][0].as_u64().unwrap() as usize;
    assert!(store.len() > 7);
    for (node, to) in [(5usize, 6i32), (3, 3)] {
        let mut d = data.clone();
        d[parent_at + node * 4..parent_at + node * 4 + 4].copy_from_slice(&to.to_le_bytes());
        if node == 5 {
            // and 6 → 5: a cycle of two
            d[parent_at + 24..parent_at + 28].copy_from_slice(&5i32.to_le_bytes());
        }
        let bad = dir.path().join(format!("cycle-{node}.sks"));
        std::fs::write(&bad, join_file(snapshot::MAGIC, &head, &d)).unwrap();
        match snapshot::load(&bad) {
            Err(SnapshotError::Corrupt(why)) => assert!(why.contains("cycle"), "{why}"),
            Err(e) => panic!("{node}: {e}"),
            Ok(_) => panic!("{node}: a cycle loaded"),
        }
    }
}

/// A folder a relist moved past its children (their parent now comes after them) still saves
/// and loads: only cycles are refused, not that order.
#[test]
fn a_folder_moved_past_its_children_loads() {
    let dir = tempfile::tempdir().unwrap();
    let store = scanned();
    {
        let mut w = store.write();
        let a = testing::find(&w, "a");
        let mut b = EntryBatch::new();
        b.push(Entry::dir(b"sub")); // kept, with its movie: the folder gets a new, later row
        b.push(Entry::file(b"one.bin", 1000, 4096));
        mutate::relist(&mut w, a, &b).unwrap();
    }
    let r = store.read();
    let sub = testing::find(&r, "a/sub");
    let movie = testing::find(&r, "a/sub/movie.mp4");
    assert!(movie < sub, "the child comes before its folder now");
    drop(r);
    let path = dir.path().join("moved.sks");
    snapshot::save(&store.read(), &path, &SaveOptions::default()).unwrap();
    let (back, _) = snapshot::load(&path).unwrap();
    testing::assert_same_store(&store.read(), &back.read());
}

/// A header that declares far more than its file can hold is refused before anything is
/// allocated for it: a few hundred bytes saying 2^46 bytes of names and i32::MAX nodes used to
/// make the process abort (review issue 20). Both formats.
#[test]
fn a_header_that_promises_too_much_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let good = dir.path().join("good.sks");
    snapshot::save(&scanned().read(), &good, &SaveOptions::default()).unwrap();
    let (mut head, data) = split_file(&std::fs::read(&good).unwrap());
    let n = i32::MAX as u64;
    head["node_count"] = n.into();
    for (name, info) in head["columns"].as_object_mut().unwrap() {
        let c = Col::from_name(name).unwrap();
        let raw = n * c.item() as u64;
        info["raw_length"] = raw.into();
        info["frames"] = serde_json::json!([[0, 40, raw]]);
    }
    head["names"]["raw_length"] = (1u64 << 46).into();
    head["names"]["frames"] = serde_json::json!([[0, 40, 1u64 << 46]]);
    let bad = dir.path().join("huge.sks");
    std::fs::write(&bad, join_file(snapshot::MAGIC, &head, &data)).unwrap();
    assert!(
        snapshot::read_header(&bad).is_ok(),
        "the header itself is fine"
    );
    assert!(matches!(
        snapshot::load(&bad),
        Err(SnapshotError::Corrupt(_))
    ));

    // format 2: the same promise in 0.3.x's layout
    let mut columns = serde_json::Map::new();
    for c in Col::ALL {
        columns.insert(
            c.name().into(),
            serde_json::json!({"offset": 0, "length": 40, "raw_length": n * c.item() as u64,
                               "dtype": format!("<{}", c.dtype())}),
        );
    }
    let v2 = serde_json::json!({
        "format": 2, "app_version": "0.3.4", "created_at": "2026-10-26T09:00:00Z",
        "roots": ["/x"], "node_count": n, "ext_table": [""], "columns": columns,
        "names": {"offset": 0, "length": 40, "raw_length": 1u64 << 46},
    });
    std::fs::write(&bad, join_file(snapshot::v2::MAGIC, &v2, &data)).unwrap();
    assert_eq!(snapshot::read_header(&bad).unwrap().format, 2);
    assert!(matches!(
        snapshot::load(&bad),
        Err(SnapshotError::Corrupt(_))
    ));
}

/// Format 1 (0.3's names back to back, in any order `name_off` says) loads into the same store as
/// the format 2 file it was made from.
#[test]
fn format_1_names_are_laid_out_anew() {
    use std::io::Read;
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../sk-core/tests/data/v034/posix/snapshots/plain.sks");
    let (want, _) = snapshot::load(&src).unwrap();
    let (mut head, mut data) = split_file(&std::fs::read(&src).unwrap());
    let block = |b: &serde_json::Value, data: &[u8]| -> Vec<u8> {
        let (o, l) = (
            b["offset"].as_u64().unwrap() as usize,
            b["length"].as_u64().unwrap() as usize,
        );
        let mut out = Vec::new();
        zstd::stream::read::Decoder::new(&data[o..o + l])
            .unwrap()
            .read_to_end(&mut out)
            .unwrap();
        out
    };
    let image = block(&head["names"], &data);
    let lens: Vec<usize> = block(&head["columns"]["name_len"], &data)
        .chunks(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]) as usize)
        .collect();
    // the names back to back, last node first
    let mut names = Vec::new();
    let mut offs = vec![0u64; lens.len()];
    let mut at = 1;
    let starts: Vec<usize> = lens
        .iter()
        .map(|l| {
            let s = at;
            at += l + 1;
            s
        })
        .collect();
    for i in (0..lens.len()).rev() {
        offs[i] = names.len() as u64;
        names.extend_from_slice(&image[starts[i]..starts[i] + lens[i]]);
    }
    let mut add = |raw: &[u8]| -> serde_json::Value {
        let z = zstd::stream::encode_all(raw, 3).unwrap();
        let v =
            serde_json::json!({"offset": data.len(), "length": z.len(), "raw_length": raw.len()});
        data.extend_from_slice(&z);
        v
    };
    let off_raw: Vec<u8> = offs
        .iter()
        .flat_map(|o| (*o as u32).to_le_bytes())
        .collect();
    let mut off_block = add(&off_raw);
    off_block["dtype"] = "<u4".into();
    head["columns"]["name_off"] = off_block;
    head["names"] = add(&names);
    head["format"] = 1.into();
    let dir = tempfile::tempdir().unwrap();
    let f1 = dir.path().join("format1.sks");
    std::fs::write(&f1, join_file(snapshot::v2::MAGIC, &head, &data)).unwrap();
    let (got, h) = snapshot::load(&f1).unwrap();
    assert_eq!(h.format, 1);
    testing::assert_same_store(&want.read(), &got.read());
}
