//! Port of tests/test_machine_server.py: `machine.disks` (the 「本机磁盘」 page), the live answer, and
//! what scans and snapshots tell; `machine.read_health`.

mod common;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use chrono::Utc;
use common::*;
use parking_lot::Mutex;
use serde_json::{Value, json};
use sk_core::disks::{Probe, new_disk};
use sk_core::helper::{HelperError, Scalars};
use sk_core::volume::VolumeInfo;
use sk_protocol::machine::{
    DiskHealth, DiskInterface, DiskPart, HealthNote, HealthSource, MachineDisks, ReadHealthResult,
};
use sk_protocol::scan::{ClassStat, MediaName};
use sk_server::handlers::machine::{
    self as mh, HealthClient, HealthHelper, TOP_CLASSES, TOP_FOLDERS,
};
use sk_server::volume_summaries::{Kept, identity};

fn norm(p: &str) -> String {
    let n = sk_store::names::normpath(p.as_bytes());
    String::from_utf8_lossy(&sk_store::names::normcase(&n)).into_owned()
}

fn vol(mount: &str, serial: &str, total: i64) -> VolumeInfo {
    VolumeInfo {
        mount_point: mount.to_owned(),
        device: mount.to_owned(),
        fs_type: "NTFS".to_owned(),
        total,
        free: total / 10,
        cluster_size: 4096,
        serial: serial.to_owned(),
        ..VolumeInfo::default()
    }
}

/// Scan `root` with an auto snapshot; the finished status.
async fn scan_saved(c: &Client, root: &Path) -> Value {
    let st = c
        .ok(
            "scan.start",
            json!({"roots": [root.to_string_lossy()], "workers": 0}),
        )
        .await;
    let id = st["scan_id"].as_str().unwrap().to_owned();
    let mut done = wait_done(c, &id).await;
    for _ in 0..100 {
        if done["snapshot"].is_string() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        done = c.ok("scan.status", json!({"scan_id": id})).await;
    }
    done
}

#[tokio::test]
async fn this_machine_over_the_protocol() {
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let volumes = c.ok("volume.list", json!({})).await;
    let res: MachineDisks = serde_json::from_value(c.ok("machine.disks", json!({})).await).unwrap();
    let mounts: Vec<String> = volumes["volumes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["mount_point"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        res.volumes
            .iter()
            .map(|v| v.volume.mount_point.clone())
            .collect::<Vec<_>>(),
        mounts
    );
    let ids: Vec<&str> = res.disks.iter().map(|d| d.id.as_str()).collect();
    assert!(
        res.volumes
            .iter()
            .all(|v| v.disks.iter().all(|d| ids.contains(&d.as_str())))
    );
    assert!(
        res.disks
            .iter()
            .flat_map(|d| &d.parts)
            .all(|p| mounts.contains(&p.mount_point))
    );
}

#[tokio::test]
async fn what_a_scan_and_a_snapshot_tell() {
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tree = sample_tree();
    let root = tree.path().to_string_lossy().into_owned();
    let status = scan_saved(&c, tree.path()).await;
    let scan_id = status["scan_id"].as_str().unwrap().to_owned();
    // let the scanned folder stand in for a whole volume
    let (key, id) = (norm(&root), scan_id.clone());
    srv.server
        .summaries
        .set_whole_root(Some(Arc::new(move |h| (h.id == id).then(|| key.clone()))));
    let volume = vol(&root, "", 1_000_000_000);
    let mut disk = new_disk("d0");
    disk.model = "Disk".into();
    disk.size = Some(1_000_000_000);
    let mut found = Probe {
        disks: vec![disk],
        ..Probe::default()
    };
    found
        .parts
        .insert(root.clone(), vec![("d0".into(), Some(1_000_000_000))]);

    let s = srv.server.clone();
    let (v, f) = (volume.clone(), found.clone());
    let out = tokio::task::spawn_blocking(move || {
        mh::overview(&s, &[v], &f, &HashMap::new(), None, None)
    })
    .await
    .unwrap();
    assert_eq!(
        out.disks[0].parts,
        vec![DiskPart {
            mount_point: root.clone(),
            bytes: Some(1_000_000_000)
        }]
    );
    let ov = &out.volumes[0];
    assert_eq!(ov.disks, vec!["d0".to_owned()]);
    let scan = ov.scan.as_ref().unwrap();
    assert_eq!(scan.source.as_str(), "scan");
    assert_eq!(scan.scan_id.as_deref(), Some(scan_id.as_str()));
    assert!(!scan.running);
    assert_eq!(
        (scan.files, scan.bytes_alloc),
        (
            status["files"].as_i64().unwrap(),
            status["bytes_alloc"].as_i64().unwrap()
        )
    );
    assert_eq!(
        serde_json::to_value(scan.at).unwrap(),
        status["finished_at"]
    );
    assert_eq!(scan.method.map(|m| m.as_str()), Some("hard"));
    assert!(!scan.classes.is_empty() && scan.classes.len() <= TOP_CLASSES);
    assert!(scan.classes.windows(2).all(|w| w[0].bytes >= w[1].bytes));
    assert!(!scan.folders.is_empty() && scan.folders.len() <= TOP_FOLDERS);
    assert!(
        scan.folders
            .iter()
            .all(|f| tree.path().join(&f.name).is_dir())
    );
    assert!(scan.folders.windows(2).all(|w| w[0].bytes >= w[1].bytes));

    // without the open scan: the newest snapshot (its header only), and the history of the volume
    let (saved, history) = mh::saved_scans(&root, None);
    let saved = saved.unwrap();
    assert_eq!(saved.source.as_str(), "snapshot");
    assert_eq!(saved.snapshot.as_deref(), status["snapshot"].as_str());
    assert_eq!(
        (saved.files, saved.bytes_alloc),
        (
            status["files"].as_i64().unwrap(),
            status["bytes_alloc"].as_i64().unwrap()
        )
    );
    assert_eq!(saved.method.map(|m| m.as_str()), Some("hard"));
    assert!(saved.classes.is_empty() && saved.folders.is_empty());
    assert_eq!(history.len(), 1);
    assert_eq!(
        (history[0].files, history[0].bytes_alloc),
        (saved.files, saved.bytes_alloc)
    );
    srv.server
        .summaries
        .set_whole_root(Some(Arc::new(|_| None)));
    let mut map = HashMap::new();
    map.insert(root.clone(), (Some(saved.clone()), history.clone()));
    let s = srv.server.clone();
    let out =
        tokio::task::spawn_blocking(move || mh::overview(&s, &[volume], &found, &map, None, None))
            .await
            .unwrap();
    assert_eq!(out.volumes[0].scan.as_ref(), Some(&saved));
    assert_eq!(out.volumes[0].history, history);
    let never = tree
        .path()
        .join("never scanned")
        .to_string_lossy()
        .into_owned();
    assert_eq!(mh::saved_scans(&never, None), (None, Vec::new()));

    // a second complete scan: the history grows, newest first (the first file renamed to an older
    // name: snapshot files are named by the second)
    let first = Path::new(status["snapshot"].as_str().unwrap()).to_path_buf();
    std::fs::rename(&first, first.with_file_name("20000101-000000.sks")).unwrap();
    std::fs::write(tree.path().join("grown.bin"), vec![b'x'; 300_000]).unwrap();
    let second = scan_saved(&c, tree.path()).await;
    let (_, history) = mh::saved_scans(&root, None);
    assert_eq!(
        history.iter().map(|h| h.bytes_alloc).collect::<Vec<_>>(),
        vec![
            second["bytes_alloc"].as_i64().unwrap(),
            status["bytes_alloc"].as_i64().unwrap()
        ]
    );
    assert!(history[0].at >= history[1].at);
}

// ---------------------------------------------------------------- records that need administrator rights

/// A SMART / Health log page with the given fields at their offsets.
fn nvme_log(units_written: u128, hours: u128, kelvin: u16) -> Vec<u8> {
    let mut b = vec![0u8; 512];
    b[1..3].copy_from_slice(&kelvin.to_le_bytes());
    b[3] = 100;
    b[4] = 10;
    b[5] = 9;
    b[48..64].copy_from_slice(&units_written.to_le_bytes());
    b[128..144].copy_from_slice(&hours.to_le_bytes());
    b
}

/// An elevated helper: the NVMe drive answers, the SATA disk refuses.
#[derive(Default)]
struct FakeClient {
    asked: Mutex<Vec<String>>,
}

impl HealthClient for FakeClient {
    fn elevated(&self) -> bool {
        true
    }

    fn disk_health(&self, disk: &str) -> Result<Scalars, HelperError> {
        self.asked.lock().push(disk.to_owned());
        if disk == "sda" {
            return Err(HelperError::error("磁盘没有设备统计日志", "not_supported"));
        }
        let log: String = nvme_log(1_000_000, 8070, 330)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let mut m = Scalars::new();
        m.insert("kind".into(), json!("nvme"));
        m.insert("log".into(), json!(log));
        Ok(m)
    }
}

struct FakeShared {
    fail: Option<HelperError>,
    client: Arc<FakeClient>,
    prompts: Mutex<usize>,
    running: Mutex<bool>,
}

impl FakeShared {
    fn new(fail: Option<HelperError>) -> Arc<FakeShared> {
        Arc::new(FakeShared {
            fail,
            client: Arc::default(),
            prompts: Mutex::new(0),
            running: Mutex::new(false),
        })
    }
}

impl HealthHelper for FakeShared {
    fn current(&self) -> Option<Arc<dyn HealthClient>> {
        (*self.running.lock()).then(|| self.client.clone() as Arc<dyn HealthClient>)
    }

    fn get(&self, elevate: bool) -> Result<Arc<dyn HealthClient>, HelperError> {
        assert!(elevate);
        *self.prompts.lock() += 1;
        if let Some(e) = &self.fail {
            return Err(e.clone());
        }
        *self.running.lock() = true;
        Ok(self.client.clone())
    }
}

fn linux_disks() -> Vec<sk_protocol::machine::PhysicalDisk> {
    let mut nvme = new_disk("nvme0n1");
    nvme.model = "ZHITAI TiPro9000".into();
    nvme.serial = "S1".into();
    nvme.size = Some(4_000_000_000_000);
    nvme.media = MediaName::NvmeSsd;
    nvme.interface = DiskInterface::Nvme;
    nvme.health = Some(DiskHealth {
        temperature_c: Some(47),
        ..sk_core::disks::health::empty_health(HealthSource::Nvme)
    });
    nvme.health_note = HealthNote::NeedsAdmin;
    let mut sata = new_disk("sda");
    sata.model = "CT2000MX500SSD1".into();
    sata.serial = "S2".into();
    sata.interface = DiskInterface::Sata;
    sata.health_note = HealthNote::NeedsAdmin;
    let mut reader = new_disk("sdc");
    reader.model = "STORAGE DEVICE".into();
    reader.interface = DiskInterface::Usb;
    reader.health_note = HealthNote::Usb;
    vec![nvme, sata, reader]
}

fn notes(m: &MachineDisks) -> Vec<&'static str> {
    m.disks.iter().map(|d| d.health_note.as_str()).collect()
}

#[tokio::test]
async fn records_are_read_only_on_request() {
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    srv.server.summaries.set_probe(Some(Arc::new(|_| Probe {
        disks: linux_disks(),
        ..Probe::default()
    })));
    let shared = FakeShared::new(None);
    srv.server.summaries.set_helper(Some(shared.clone()));

    let page: MachineDisks =
        serde_json::from_value(c.ok("machine.disks", json!({})).await).unwrap();
    assert_eq!(*shared.prompts.lock(), 0); // opening the page never asks
    assert_eq!(notes(&page), ["needs_admin", "needs_admin", "usb"]);

    let read: ReadHealthResult =
        serde_json::from_value(c.ok("machine.read_health", json!({})).await).unwrap();
    assert_eq!(
        (
            read.outcome.as_str(),
            read.message.as_str(),
            *shared.prompts.lock()
        ),
        ("read", "", 1)
    );
    assert_eq!(*shared.client.asked.lock(), ["nvme0n1", "sda"]); // not the card reader
    let (nvme, sata, reader) = (
        &read.machine.disks[0],
        &read.machine.disks[1],
        &read.machine.disks[2],
    );
    let h = nvme.health.as_ref().unwrap();
    assert_eq!(nvme.health_note, HealthNote::None);
    assert_eq!(h.written_bytes, Some(1_000_000 * 512_000));
    assert_eq!(h.power_on_hours, Some(8070));
    assert_eq!(h.temperature_c, Some(47)); // the live temperature
    assert!(h.read_at.is_some());
    assert_eq!(
        (sata.health.is_none(), sata.health_note),
        (true, HealthNote::Unsupported)
    );
    assert_eq!(reader.health_note, HealthNote::Usb);

    // later reads keep the record; while the helper runs they ask it again, without a prompt
    let again: MachineDisks =
        serde_json::from_value(c.ok("machine.disks", json!({})).await).unwrap();
    let ah = again.disks[0].health.clone().unwrap();
    assert_eq!(
        DiskHealth {
            read_at: h.read_at,
            ..ah
        },
        *h
    );
    assert_eq!(*shared.client.asked.lock(), ["nvme0n1", "sda", "nvme0n1"]);
    assert_eq!(*shared.prompts.lock(), 1);
    *shared.running.lock() = false;
    let kept: MachineDisks =
        serde_json::from_value(c.ok("machine.disks", json!({})).await).unwrap();
    assert_eq!(
        kept.disks[0].health.as_ref().unwrap().written_bytes,
        h.written_bytes
    );
    assert_eq!(kept.disks[1].health_note, HealthNote::Unsupported);
    assert_eq!(shared.client.asked.lock().len(), 3);

    // nothing left to read
    let nothing: ReadHealthResult =
        serde_json::from_value(c.ok("machine.read_health", json!({})).await).unwrap();
    assert_eq!(nothing.outcome.as_str(), "nothing");
    assert_eq!(*shared.prompts.lock(), 1);
}

#[tokio::test]
async fn records_when_rights_are_not_given() {
    for (fail, outcome) in [
        (HelperError::denied(), "denied"),
        (
            HelperError::unavailable("没有可以询问管理员密码的身份验证代理", "no_agent"),
            "unavailable",
        ),
    ] {
        let srv = server().await;
        let c = Client::open(&srv.url).await;
        srv.server.summaries.set_probe(Some(Arc::new(|_| Probe {
            disks: linux_disks(),
            ..Probe::default()
        })));
        srv.server
            .summaries
            .set_helper(Some(FakeShared::new(Some(fail))));
        let read: ReadHealthResult = serde_json::from_value(
            c.ok("machine.read_health", json!({"disks": ["nvme0n1"]}))
                .await,
        )
        .unwrap();
        assert_eq!(read.outcome.as_str(), outcome);
        assert!(!read.message.is_empty());
        assert_eq!(notes(&read.machine), ["needs_admin", "needs_admin", "usb"]); // still offered
    }
}

// ---------------------------------------------------------------- what a scan showed, kept

/// Closing the scan's tab keeps its classes and folders; a new backend still shows them, and they
/// add up to the page's class total.
#[tokio::test]
async fn what_a_scan_showed_is_kept() {
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tree = sample_tree();
    let status = scan_saved(&c, tree.path()).await;
    let id = status["scan_id"].as_str().unwrap().to_owned();
    let idc = id.clone();
    // let the scanned folder stand in for its whole volume
    srv.server.summaries.set_whole_root(Some(Arc::new(move |h| {
        (h.id == idc).then(|| "x".to_owned())
    })));
    let volume = srv.server.scans.get(&id).unwrap().roots.read()[0]
        .volume
        .clone()
        .unwrap();
    c.ok("scan.close", json!({"scan_id": id})).await;

    srv.server.summaries.reload(); // read back from the file, as after a restart
    let store = srv.server.summaries.store();
    let kept = store.get(&volume).unwrap();
    assert_eq!(
        (kept.files, kept.bytes_alloc),
        (
            status["files"].as_i64().unwrap(),
            status["bytes_alloc"].as_i64().unwrap()
        )
    );
    assert!(!kept.classes.is_empty() && !kept.folders.is_empty());
    assert_eq!(
        serde_json::to_value(kept.at).unwrap(),
        status["finished_at"]
    );
    assert!(kept.classes.windows(2).all(|w| w[0].bytes >= w[1].bytes));

    let s = srv.server.clone();
    let v = volume.clone();
    let out = tokio::task::spawn_blocking(move || {
        let get = |x: &VolumeInfo| store.get(x);
        mh::overview(
            &s,
            &[v],
            &Probe::default(),
            &HashMap::new(),
            Some(&get),
            None,
        )
    })
    .await
    .unwrap();
    let scan = out.volumes[0].scan.as_ref().unwrap();
    assert_eq!(scan.source.as_str(), "kept");
    assert!(scan.scan_id.is_none());
    assert_eq!(
        scan.classes,
        kept.classes
            .iter()
            .take(TOP_CLASSES)
            .cloned()
            .collect::<Vec<_>>()
    );
    assert_eq!(scan.folders, kept.folders);
    let total = out.classes.unwrap();
    assert_eq!(
        (total.volumes, total.bytes),
        (
            vec![volume.mount_point.clone()],
            status["bytes_alloc"].as_i64().unwrap()
        )
    );
    assert_eq!(
        total.classes.iter().map(|c| c.bytes).sum::<i64>(),
        kept.classes.iter().map(|c| c.bytes).sum::<i64>()
    );

    // without a known mix there is no total
    let s = srv.server.clone();
    let out = tokio::task::spawn_blocking(move || {
        mh::overview(
            &s,
            &[volume],
            &Probe::default(),
            &HashMap::new(),
            None,
            None,
        )
    })
    .await
    .unwrap();
    assert!(out.classes.is_none());
}

#[tokio::test]
async fn class_total_adds_up_volumes() {
    let srv = server().await;
    let (cv, dv, ev) = (
        vol("C:\\", "1", 1 << 40),
        vol("D:\\", "2", 1 << 40),
        vol("E:\\", "3", 1 << 40),
    );
    let now = Utc::now();
    let k = |v: &VolumeInfo, bytes: i64, classes: Vec<ClassStat>| Kept {
        mount: v.mount_point.clone(),
        identity: identity(v),
        at: now,
        seen: now,
        files: 0,
        dirs: None,
        bytes_alloc: bytes,
        method: None,
        snapshot: None,
        classes,
        folders: Vec::new(),
    };
    let cs = |id: &str, bytes: i64, files: i64| ClassStat {
        class_id: id.into(),
        bytes,
        files,
        parts: Vec::new(),
    };
    let mut store = HashMap::new();
    store.insert(
        identity(&cv),
        k(&cv, 700, vec![cs("game", 500, 5), cs("code", 200, 50)]),
    );
    store.insert(identity(&dv), k(&dv, 300, vec![cs("code", 350, 30)]));
    let s = srv.server.clone();
    let out = tokio::task::spawn_blocking(move || {
        let get = |v: &VolumeInfo| store.get(&identity(v)).cloned();
        mh::overview(
            &s,
            &[cv, dv, ev],
            &Probe::default(),
            &HashMap::new(),
            Some(&get),
            None,
        )
    })
    .await
    .unwrap();
    let total = out.classes.unwrap();
    assert_eq!(total.volumes, ["C:\\", "D:\\"]); // E: never scanned
    assert_eq!(total.bytes, 1000);
    let got: Vec<(String, i64, i64)> = total
        .classes
        .iter()
        .map(|c| (c.class_id.clone(), c.bytes, c.files))
        .collect();
    assert_eq!(
        got,
        [("code".to_owned(), 550, 80), ("game".to_owned(), 500, 5)]
    );
}
