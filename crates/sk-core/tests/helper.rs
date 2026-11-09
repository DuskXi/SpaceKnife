//! Elevated scan helper (`sk_core::helper`): framing, auth, lifecycle, bulk transfer, and the USN
//! journal through the helper (Python `test_helper.py`, `test_helper_usn.py`).
//!
//! Nothing here ever shows a UAC prompt or polkit's password dialog: `elevate = true` is only
//! exercised with the ShellExecuteEx call replaced. Helpers are this test binary itself, run with
//! the `helper_entry` test as its only test and a mode after `--sk-helper-mode` (see [`command`]):
//! the real helper ([`run_main`]), or one whose MFT reader / journal / NTFS check are replaced by
//! fakes, or misbehaving impostors. Live tests skip themselves when this process has administrator
//! rights (the helpers would be elevated too).

use std::ffi::OsString;
use std::io;
use std::ops::ControlFlow;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sk_core::helper::auth::{new_token, read_token_file, write_token_file};
use sk_core::helper::launch::start_subprocess;
use sk_core::helper::messages::{
    Call, KIND_BINARY, KIND_JSON, MAX_CONTROL_BYTES, PROTOCOL_VERSION, Reply, Request, Scalars,
    frame_header,
};
use sk_core::helper::service::{Hooks, OpError, run_main_with};
use sk_core::helper::transport::{Listener, connect};
use sk_core::helper::usn::{
    self, ERROR_ACCESS_DENIED, ERROR_INVALID_FUNCTION, ERROR_INVALID_PARAMETER,
    ERROR_JOURNAL_DELETE_IN_PROGRESS, ERROR_JOURNAL_ENTRY_DELETED, ERROR_JOURNAL_NOT_ACTIVE,
    ERROR_NOT_SUPPORTED, ERROR_PRIVILEGE_NOT_HELD, Journal, ReadOptions, UsnDevice, UsnError,
    UsnRows,
};
use sk_core::helper::{
    HelperClient, HelperError, LaunchOptions, MftOptions, Phase, SharedHelper, is_elevated,
};
use sk_native::mft::MftTable;

// --------------------------------------------------------------------------- the helper entry

const MODE_FLAG: &str = "--sk-helper-mode";

/// Runs only as a helper (spawned by the tests below with `--sk-helper-mode`); a no-op otherwise.
#[test]
fn helper_entry() {
    let args: Vec<OsString> = std::env::args_os().collect();
    let Some(i) = args.iter().position(|a| a == MODE_FLAG) else {
        return;
    };
    let mode = args[i + 1].to_string_lossy().into_owned();
    let rest = args[i + 2..].to_vec();
    std::process::exit(run_mode(&mode, &rest));
}

/// The command that starts this binary as a helper in `mode` (the helper's own arguments follow).
fn command(mode: &str) -> Vec<OsString> {
    [
        std::env::current_exe().unwrap().into_os_string(),
        "helper_entry".into(),
        "--exact".into(),
        "--nocapture".into(),
        "--test-threads=1".into(),
        "--".into(),
        MODE_FLAG.into(),
        mode.into(),
    ]
    .to_vec()
}

fn run_mode(mode: &str, args: &[OsString]) -> i32 {
    match mode {
        "real" => run_main_with(args, Hooks::default()),
        "fake_mft" => run_main_with(
            args,
            Hooks {
                scan_image: Some(Box::new(fake_scan_image)),
                ..Default::default()
            },
        ),
        "usn" => run_main_with(
            args,
            Hooks {
                elevated: Some(true), // claimed only: the process itself is unprivileged
                open_usn: Some(Box::new(|v| {
                    journal_for(v).map(|j| Box::new(j) as Box<dyn UsnDevice>)
                })),
                check_ntfs: Some(Box::new(fake_check_ntfs)),
                ..Default::default()
            },
        ),
        "exit5" => 5,
        "sleep" => {
            std::thread::sleep(Duration::from_secs(60));
            0
        }
        impostor => fake_helper(impostor, args),
    }
}

fn arg(args: &[OsString], flag: &str) -> String {
    let i = args.iter().position(|a| a == flag).expect(flag);
    args[i + 1].to_string_lossy().into_owned()
}

/// Helpers that misbehave on purpose.
fn fake_helper(mode: &str, args: &[OsString]) -> i32 {
    if mode == "child" {
        // a process the backend did not launch connects instead
        let mut cmd = command("honest");
        cmd.extend_from_slice(args);
        let status = std::process::Command::new(&cmd[0])
            .args(&cmd[1..])
            .status()
            .unwrap();
        return status.code().unwrap_or(1);
    }
    let token = read_token_file(Path::new(&arg(args, "--token-file"))).unwrap();
    let chan = connect(&arg(args, "--pipe"), Duration::from_secs(10)).unwrap();
    let t = Some(Duration::from_secs(10));
    let pid = std::process::id() + u32::from(mode == "wrong_pid");
    let hello = Reply::Hello {
        version: PROTOCOL_VERSION,
        token: if mode == "wrong_token" {
            "0".repeat(64)
        } else {
            token
        },
        pid,
        elevated: false,
    };
    if mode != "silent" {
        chan.send_message(&hello, t).unwrap();
    }
    if let Some(garbage) = mode.strip_prefix("garbage_") {
        let _ = chan.read_frame(
            Some(Duration::from_secs(30)),
            Some(Duration::from_secs(30)),
            1 << 20,
        );
        let _ = match garbage {
            "kind" => chan.send(&[&5u32.to_le_bytes(), &[0x00], b"abcd"], t),
            "json" => chan.send(&[&frame_header(KIND_JSON, 8), b"not json"], t),
            "id" => chan.send_message(&Reply::response(999, Scalars::new(), Scalars::new()), t),
            "binary" => chan.send(&[&frame_header(KIND_BINARY, 4), b"\xff\xff\xff\x7f"], t),
            "huge" => chan.send(&[&0xFFFF_FFF0u32.to_le_bytes(), &[KIND_BINARY]], t),
            _ => unreachable!(),
        };
    }
    while chan
        .read_frame(
            Some(Duration::from_secs(60)),
            Some(Duration::from_secs(60)),
            1 << 20,
        )
        .is_ok()
    {}
    0
}

// --------------------------------------------------------------------------- synthetic MFT

fn synth(rows: usize, shuffle: bool) -> MftTable {
    let mut t = MftTable::default();
    let mut off = 0u32;
    for i in 0..rows {
        let len = (3 + i % 13) as u16;
        t.record.push(i as u64 + 16);
        t.parent.push((i / 5) as u64 + 5);
        t.name_off.push(off);
        t.name_len.push(len);
        off += len as u32;
        t.kind.push((i % 4 == 0) as u8);
        t.flags.push(((i * 7) & 0xFFFF) as u32);
        t.size.push(i as i64 * 1000);
        t.alloc.push(i as i64 * 1024);
        t.ads_size.push((i % 3) as i64);
        t.ads_alloc.push((i % 3) as i64 * 8);
        t.mtime.push(1_700_000_000 + i as i64);
        t.ctime.push(1_600_000_000 + i as i64);
        t.atime.push(1_750_000_000 - i as i64);
        t.nlink.push((1 + i % 2) as u16);
    }
    t.names = (0..off as usize).map(|i| b'a' + (i % 26) as u8).collect();
    t.bad_records = 7;
    if shuffle {
        // rows out of name order: exercises the gather path of the batcher
        let mut perm: Vec<usize> = (0..rows).collect();
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        for i in (1..rows).rev() {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            perm.swap(i, (x % (i as u64 + 1)) as usize);
        }
        macro_rules! permute {
            ($($f:ident),*) => { $( t.$f = perm.iter().map(|&p| t.$f[p]).collect(); )* };
        }
        permute!(
            record, parent, name_off, name_len, kind, flags, size, alloc, ads_size, ads_alloc,
            mtime, ctime, atime, nlink
        );
    }
    t
}

fn spec(data: &str) -> std::collections::HashMap<String, String> {
    data.split_whitespace()
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| (k.into(), v.into()))
        .collect()
}

/// The image file holds a spec (`rows=N shuffle=1 sleep=1.3 record=4096 cluster=8192 fail=io`).
fn fake_scan_image(
    path: &str,
    duty: f64,
    progress: &mut dyn FnMut(u64, u64),
) -> io::Result<(MftTable, Scalars)> {
    let data = std::fs::read_to_string(path)?;
    let s = spec(&data);
    match s.get("fail").map(String::as_str) {
        Some("io") => return Err(io::Error::other("simulated read error")),
        Some("bad") => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "not an NTFS volume image",
            ));
        }
        _ => {}
    }
    let get = |k: &str, d: u64| s.get(k).map_or(d, |v| v.parse().unwrap());
    let mut geo = Scalars::new();
    geo.insert("bytes_per_sector".into(), 512.into());
    geo.insert("mft_lcn".into(), 4.into());
    geo.insert("mft_size".into(), data.len().into());
    geo.insert("record_size".into(), get("record", 1024).into());
    geo.insert("cluster_size".into(), get("cluster", 4096).into());
    geo.insert("duty_seen".into(), ((duty * 1000.0) as u64).into());
    progress(data.len() as u64 / 2, data.len() as u64); // half read while the (slow) parse runs
    std::thread::sleep(Duration::from_secs_f64(
        s.get("sleep").map_or(0.0, |v| v.parse().unwrap()),
    ));
    Ok((
        synth(
            get("rows", 0) as usize,
            s.get("shuffle").map(String::as_str) == Some("1"),
        ),
        geo,
    ))
}

fn gather(names: &[u8], off: &[u32], len: &[u16]) -> Vec<u8> {
    let mut out = Vec::new();
    for (&o, &l) in off.iter().zip(len) {
        out.extend_from_slice(&names[o as usize..o as usize + l as usize]);
    }
    out
}

fn assert_tables_equal(got: &MftTable, want: &MftTable) {
    assert_eq!(got.rows(), want.rows());
    macro_rules! same {
        ($($f:ident),*) => { $( assert_eq!(got.$f, want.$f, stringify!($f)); )* };
    }
    same!(
        record, parent, name_len, kind, flags, size, alloc, ads_size, ads_alloc, mtime, ctime,
        atime, nlink
    );
    assert_eq!(
        gather(&got.names, &got.name_off, &got.name_len),
        gather(&want.names, &want.name_off, &want.name_len)
    );
}

fn image(dir: &Path, spec: &str) -> String {
    let path = dir.join("mft.img");
    std::fs::write(&path, spec).unwrap();
    path.to_string_lossy().into_owned()
}

// --------------------------------------------------------------------------- live helpers

/// False (the test is skipped) when this process is elevated: its helpers would be too.
fn unprivileged() -> bool {
    if is_elevated() {
        println!("skipped: running with administrator rights");
        return false;
    }
    true
}

fn opts(mode: &str) -> LaunchOptions {
    LaunchOptions {
        command: Some(command(mode)),
        ..LaunchOptions::unelevated()
    }
}

fn launch(mode: &str) -> HelperClient {
    HelperClient::launch(opts(mode)).unwrap()
}

fn launch_err(o: LaunchOptions) -> HelperError {
    match HelperClient::launch(o) {
        Ok(_) => panic!("launch should fail"),
        Err(e) => e,
    }
}

fn kill(pid: u32) {
    #[cfg(windows)]
    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/PID", &pid.to_string()])
        .output();
    #[cfg(unix)]
    // SAFETY: plain syscall.
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }
}

fn platform() -> &'static str {
    if cfg!(windows) {
        "win32"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    }
}

#[test]
fn hello_ping_and_close() {
    if unprivileged() {
        let client = launch("real");
        assert!(client.alive());
        assert!(!client.elevated());
        let info = client.ping().unwrap();
        assert_eq!(info["pid"], client.pid());
        assert_eq!(info["elevated"], false);
        assert_eq!(info["version"], 1);
        assert_eq!(info["platform"], platform());
        client.close();
        assert!(!client.alive());
        assert_eq!(client.wait_exit(Duration::from_secs(10)), Some(0)); // shut down cleanly, not killed
        let err = client.ping().unwrap_err();
        assert!(err.is_unavailable() && err.code == "closed");
        client.close(); // idempotent
    }
}

#[test]
fn scan_mft_needs_admin_and_real_reader_errors() {
    if unprivileged() {
        let client = launch("real");
        let err = client
            .scan_mft("C:", MftOptions::default(), None)
            .unwrap_err();
        assert_eq!(err.code, "needs_admin");
        assert!(
            err.message.contains("需要管理员权限") || err.message.to_lowercase().contains("admin"),
            "{}",
            err.message
        );
        assert!(!err.is_unavailable());
        // the real image reader on something that is not an NTFS image
        let dir = tempfile::tempdir().unwrap();
        let path = image(dir.path(), "not a volume");
        let err = client
            .scan_mft_image(&path, MftOptions::default(), None)
            .unwrap_err();
        assert_eq!(err.code, "io_error", "{}", err.message);
        assert_eq!(client.ping().unwrap()["pid"], client.pid()); // still usable
    }
}

#[test]
fn volume_info() {
    if unprivileged() {
        let helper = launch("real");
        if cfg!(windows) {
            let system = std::env::var("SYSTEMDRIVE").unwrap_or_else(|_| "C:".into());
            let info = helper.volume_info(&system.to_lowercase()).unwrap();
            assert_eq!(info["volume"], system.to_uppercase());
            assert_eq!(info["fs_type"], "NTFS");
            assert!(
                info["cluster_size"].as_u64().unwrap() > 0
                    && info["bytes_per_sector"].as_u64().unwrap() > 0
            );
            assert!(!info.contains_key("record_size")); // FSCTL data needs administrator rights
        } else {
            assert_eq!(helper.volume_info("C:").unwrap_err().code, "not_supported");
        }
        let bad = helper.volume_info("C:\\Windows").unwrap_err();
        assert_eq!(bad.code, "invalid_request");
    }
}

#[test]
fn scan_mft_image_roundtrip() {
    if unprivileged() {
        let helper = launch("fake_mft");
        let dir = tempfile::tempdir().unwrap();
        let rows = 25_000;
        let mut phases = Vec::new();
        let mut read = Vec::new();
        let path = image(
            dir.path(),
            &format!("rows={rows} shuffle=1 sleep=1.3 record=4096 cluster=8192"),
        );
        let mut on_progress = |phase: Phase, _elapsed: f64, fraction: Option<f64>| {
            phases.push(phase);
            read.push(fraction);
        };
        let opts = MftOptions {
            batch_rows: 6000,
            duty: 0.5,
        };
        let table = helper
            .scan_mft_image(&path, opts, Some(&mut on_progress))
            .unwrap();
        assert_tables_equal(&table, &synth(rows, true));
        assert_eq!(table.bad_records, 7); // the table's scalars come back with it
        let stats = helper.last_scan();
        assert_eq!(
            (stats["frames"].as_u64(), stats["rows"].as_u64()),
            (Some(5), Some(rows as u64))
        );
        assert_eq!(
            (
                stats["record_size"].as_u64(),
                stats["cluster_size"].as_u64()
            ),
            (Some(4096), Some(8192))
        );
        assert_eq!(
            stats["mft_size"].as_u64(),
            Some(std::fs::metadata(&path).unwrap().len())
        );
        assert_eq!(stats["duty_seen"].as_u64(), Some(500));
        assert!(phases.contains(&Phase::Scan)); // heartbeats while the (slow) scan ran
        assert!(read.iter().any(|f| f.is_some_and(|f| 0.45 < f && f <= 0.5))); // carrying how much was read
    }
}

#[test]
fn stream_batches_and_abandon() {
    if unprivileged() {
        let helper = launch("fake_mft");
        let dir = tempfile::tempdir().unwrap();
        let path = image(dir.path(), "rows=10000");
        let mut sizes = Vec::new();
        let opts = |n| MftOptions {
            batch_rows: n,
            duty: 1.0,
        };
        helper
            .stream_mft_image(&path, opts(3000), None, &mut |batch| {
                sizes.push(batch.rows());
                assert_eq!(batch.name_off[0], 0); // offsets are relative to the batch's own blob
                ControlFlow::Continue(())
            })
            .unwrap();
        assert_eq!(sizes, [3000, 3000, 3000, 1000]);
        // abandon: the rest of the stream is skipped, the helper stays usable
        helper
            .stream_mft_image(&path, opts(1000), None, &mut |_| ControlFlow::Break(()))
            .unwrap();
        assert!(helper.alive());
        assert_eq!(helper.ping().unwrap()["pid"], helper.pid());
        let empty = helper
            .scan_mft_image(&image(dir.path(), "rows=0"), MftOptions::default(), None)
            .unwrap();
        assert_eq!((empty.rows(), empty.names.len()), (0, 0));
    }
}

#[test]
fn scan_errors_keep_the_helper() {
    if unprivileged() {
        let helper = launch("fake_mft");
        let dir = tempfile::tempdir().unwrap();
        for spec in ["rows=1 fail=io", "rows=1 fail=bad"] {
            let err = helper
                .scan_mft_image(&image(dir.path(), spec), MftOptions::default(), None)
                .unwrap_err();
            assert!(err.code == "io_error" && !err.is_unavailable(), "{err:?}");
        }
        let missing = dir.path().join("missing.img");
        let err = helper
            .scan_mft_image(&missing.to_string_lossy(), MftOptions::default(), None)
            .unwrap_err();
        assert_eq!(err.code, "io_error");
        assert_eq!(helper.ping().unwrap()["pid"], helper.pid());
    }
}

#[test]
fn scan_mft_image_throughput() {
    if unprivileged() {
        let helper = launch("fake_mft");
        let dir = tempfile::tempdir().unwrap();
        let rows = 3_000_000;
        let path = image(dir.path(), &format!("rows={rows}"));
        let t0 = Instant::now();
        let table = helper
            .scan_mft_image(&path, MftOptions::default(), None)
            .unwrap();
        let elapsed = t0.elapsed().as_secs_f64();
        let stats = helper.last_scan();
        assert_eq!(
            (stats["frames"].as_u64(), stats["rows"].as_u64()),
            (Some(3), Some(rows as u64))
        );
        assert_tables_equal(&table, &synth(rows, false));
        let wire = stats["wire_bytes"].as_u64().unwrap();
        let transfer = elapsed - stats["scan_seconds"].as_f64().unwrap();
        println!(
            "helper bulk transfer: {rows} rows, {:.0} MB in {} frames; end-to-end {elapsed:.2} s; helper send {:.2} s; \
             transfer+assembly {transfer:.2} s = {:.0} MB/s",
            wire as f64 / 1e6,
            stats["frames"],
            stats["send_seconds"].as_f64().unwrap(),
            wire as f64 / 1e6 / transfer
        );
        assert!(wire > rows as u64 * 85);
    }
}

#[test]
fn idle_timeout() {
    if unprivileged() {
        let client = HelperClient::launch(LaunchOptions {
            idle_timeout: Duration::from_millis(500),
            ..opts("real")
        })
        .unwrap();
        assert_eq!(client.wait_exit(Duration::from_secs(10)), Some(0));
        assert!(!client.alive());
        let err = client.ping().unwrap_err();
        assert!(err.is_unavailable() && err.code == "exited", "{err:?}");
        client.close();
    }
}

#[test]
fn helper_exits_when_backend_disconnects() {
    if unprivileged() {
        let helper = launch("fake_mft");
        helper.abort(); // what the OS does when the backend process dies: the channel closes
        assert_eq!(helper.wait_exit(Duration::from_secs(10)), Some(0));
        assert!(!helper.alive());
    }
}

#[test]
fn aborted_scan_tears_down_the_connection() {
    if unprivileged() {
        let helper = Arc::new(launch("fake_mft"));
        let dir = tempfile::tempdir().unwrap();
        let path = image(dir.path(), "rows=200000");
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let h2 = helper.clone();
        let consumer = std::thread::spawn(move || {
            let opts = MftOptions {
                batch_rows: 1000,
                duty: 1.0,
            };
            h2.stream_mft_image(&path, opts, None, &mut |_| {
                let _ = started_tx.send(());
                std::thread::sleep(Duration::from_millis(50));
                ControlFlow::Continue(())
            })
        });
        started_rx.recv().unwrap();
        helper.abort();
        let err = consumer.join().unwrap().unwrap_err();
        assert!(err.is_unavailable(), "{err:?}");
        assert!(!helper.alive()); // no draining of 199 frames after an abort
        assert!(helper.wait_exit(Duration::from_secs(10)).is_some());
    }
}

#[test]
fn helper_death_is_reported() {
    if unprivileged() {
        let helper = launch("fake_mft");
        kill(helper.pid());
        helper.wait_exit(Duration::from_secs(10));
        let err = helper.ping().unwrap_err();
        assert!(err.is_unavailable() && err.code == "exited", "{err:?}");
        assert!(!helper.alive());
    }
}

#[test]
fn shared_helper_is_reused() {
    if unprivileged() {
        let shared = SharedHelper::new(opts("fake_mft"));
        let first = shared.get(false).unwrap();
        assert!(Arc::ptr_eq(&shared.get(false).unwrap(), &first));
        kill(first.pid());
        first.wait_exit(Duration::from_secs(10));
        let second = shared.get(false).unwrap();
        assert!(!Arc::ptr_eq(&second, &first) && second.pid() != first.pid());
        assert!(Arc::ptr_eq(&shared.current().unwrap(), &second));
        shared.close();
        assert!(shared.current().is_none());
    }
}

// --------------------------------------------------------------------------- authentication

#[test]
fn handshake_rejects_impostors() {
    for (mode, why) in [
        ("wrong_token", "令牌不符"),
        ("wrong_pid", "不符"),
        ("child", "不是启动的进程"),
    ] {
        let err = launch_err(opts(mode));
        assert_eq!(err.code, "auth", "{mode}: {err:?}");
        assert!(err.is_unavailable());
        assert!(
            err.message.contains(why) || sk_core::i18n::language() != sk_core::i18n::SOURCE,
            "{mode}: {}",
            err.message
        );
    }
}

#[test]
fn silent_helper_times_out() {
    let t0 = Instant::now();
    let err = launch_err(LaunchOptions {
        connect_timeout: Some(Duration::from_secs(2)),
        ..opts("silent")
    });
    assert_eq!(err.code, "timeout", "{err:?}");
    assert!(t0.elapsed() < Duration::from_secs(15));
}

#[test]
fn helper_that_exits_or_never_connects() {
    let t0 = Instant::now();
    let err = launch_err(opts("exit5"));
    assert!(err.code == "exited" && err.message.contains('5'), "{err:?}");
    assert!(t0.elapsed() < Duration::from_secs(15)); // noticed the exit, did not wait for the timeout
    let late = launch_err(LaunchOptions {
        connect_timeout: Some(Duration::from_secs(1)),
        ..opts("sleep")
    });
    assert_eq!(late.code, "timeout");
}

#[test]
fn helper_refuses_bad_arguments() {
    let dir = tempfile::tempdir().unwrap();
    let token_path = write_token_file(&new_token(), Some(dir.path())).unwrap();
    let run = |extra: &[&str]| {
        let cmd = command("real");
        let mut c = std::process::Command::new(&cmd[0]);
        c.args(&cmd[1..])
            .args(["--server-pid", &std::process::id().to_string()])
            .args(extra);
        c.stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        c.status().unwrap().code()
    };
    let bad_pipe = run(&[
        "--pipe",
        r"C:\Windows\win.ini",
        "--token-file",
        &token_path.to_string_lossy(),
    ]);
    assert_eq!(bad_pipe, Some(3));
    assert!(!token_path.exists()); // read and deleted before anything else
    let missing = dir.path().join("nope.token");
    assert_eq!(
        run(&["--pipe", "x", "--token-file", &missing.to_string_lossy()]),
        Some(2)
    );
    assert_eq!(run(&["--pipe", "x"]), Some(2)); // a required argument is missing
}

// --------------------------------------------------------------------------- malformed traffic

#[test]
fn client_survives_malformed_helper() {
    for mode in [
        "garbage_kind",
        "garbage_json",
        "garbage_id",
        "garbage_binary",
        "garbage_huge",
    ] {
        let client = launch(mode);
        let err = client.ping().unwrap_err();
        assert!(
            err.is_unavailable() && err.code == "protocol",
            "{mode}: {err:?}"
        );
        assert!(!client.alive());
        client.close();
        assert!(
            client.wait_exit(Duration::from_secs(10)).is_some(),
            "{mode}"
        );
    }
}

#[test]
fn helper_survives_malformed_requests() {
    if unprivileged() {
        let mut listener = Listener::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let token = new_token();
        let token_path = write_token_file(&token, Some(dir.path())).unwrap();
        let mut argv = command("real");
        argv.extend(
            [
                "--pipe",
                listener.address(),
                "--token-file",
                &token_path.to_string_lossy(),
                "--server-pid",
                &std::process::id().to_string(),
            ]
            .map(OsString::from),
        );
        let mut process = start_subprocess(&argv).unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        let chan = loop {
            match listener.accept(Duration::from_millis(250)) {
                Ok(c) => break c,
                Err(e) if e.kind() == io::ErrorKind::TimedOut && Instant::now() < deadline => {
                    continue;
                }
                Err(e) => panic!("{e}"),
            }
        };
        listener.close();
        let t = Some(Duration::from_secs(10));
        let (_, hello) = chan.read_frame(t, t, 0).unwrap();
        assert!(
            matches!(Reply::parse(&hello).unwrap(), Reply::Hello { token: theirs, .. } if theirs == token)
        );
        let send_raw = |kind: u8, body: &[u8]| {
            chan.send(&[&frame_header(kind, body.len()), body], t)
                .unwrap();
            let (_, reply) = chan.read_frame(t, t, 1 << 20).unwrap();
            Reply::parse(&reply).unwrap()
        };
        let is_error = |r: &Reply, code: &str, id: Option<u64>| matches!(r, Reply::Error { code: c, id: i, .. } if c == code && *i == id);
        assert!(is_error(
            &send_raw(KIND_JSON, b"not json at all"),
            "invalid_request",
            None
        ));
        let r = send_raw(
            KIND_JSON,
            br#"{"type":"request","id":7,"call":{"op":"format_disk"}}"#,
        );
        assert!(is_error(&r, "invalid_request", Some(7)), "{r:?}");
        let r = send_raw(
            KIND_JSON,
            br#"{"type":"request","id":8,"call":{"op":"scan_mft","volume":"C:\\Windows"}}"#,
        );
        assert!(
            matches!(&r, Reply::Error { id: Some(8), message, .. } if message.contains("volume")),
            "{r:?}"
        );
        let r = send_raw(
            KIND_JSON,
            br#"{"type":"request","id":9,"call":{"op":"scan_mft_image","path":""}}"#,
        );
        assert!(matches!(r, Reply::Error { id: Some(9), .. }));
        assert!(is_error(
            &send_raw(KIND_BINARY, &[0; 16]),
            "invalid_request",
            None
        ));
        chan.send_message(&Request::new(10, Call::Ping {}), t)
            .unwrap();
        let (_, body) = chan.read_frame(t, t, 1 << 20).unwrap();
        assert!(matches!(
            Reply::parse(&body).unwrap(),
            Reply::Response { id: 10, .. }
        ));
        // oversized: the stream is lost
        chan.send(
            &[
                &((MAX_CONTROL_BYTES + 100) as u32).to_le_bytes(),
                &[KIND_JSON],
            ],
            t,
        )
        .unwrap();
        let (_, body) = chan.read_frame(t, t, 1 << 20).unwrap();
        assert!(is_error(&Reply::parse(&body).unwrap(), "bad_frame", None));
        assert!(chan.read_frame(t, t, 1 << 20).is_err());
        assert_eq!(process.wait(Duration::from_secs(10)), Some(0));
        chan.close();
        process.terminate();
        process.close();
    }
}

// --------------------------------------------------------------------------- elevation path (mocked)

#[cfg(windows)]
mod uac {
    use super::*;
    use sk_core::helper::launch::ShellExecuteParams;
    use sk_core::helper::launch::{Elevator, HelperProcess};
    use std::sync::Mutex;

    fn split_cmdline(cmdline: &str) -> Vec<OsString> {
        use std::os::windows::ffi::OsStringExt;
        use windows_sys::Win32::UI::Shell::CommandLineToArgvW;
        let w: Vec<u16> = format!("x {cmdline}").encode_utf16().chain([0]).collect();
        let mut argc = 0;
        // SAFETY: a valid NUL-terminated string; the result is freed below.
        unsafe {
            let argv = CommandLineToArgvW(w.as_ptr(), &mut argc);
            let out = (1..argc as usize)
                .map(|i| {
                    let p = *argv.add(i);
                    let n = (0..).take_while(|&k| *p.add(k) != 0).count();
                    OsString::from_wide(std::slice::from_raw_parts(p, n))
                })
                .collect();
            windows_sys::Win32::Foundation::LocalFree(argv.cast());
            out
        }
    }

    fn elevator(
        f: impl Fn(&ShellExecuteParams) -> io::Result<Box<dyn HelperProcess>> + Send + Sync + 'static,
    ) -> Elevator {
        Elevator {
            shell_execute: Arc::new(f),
            ..Elevator::default()
        }
    }

    #[test]
    fn launch_elevated_denied() {
        // an installed app's folder: the window program and spaceknife-cli next to it
        let app = tempfile::tempdir().unwrap();
        let gui = app.path().join("SpaceKnife.exe");
        let cli = app.path().join("spaceknife-cli.exe");
        std::fs::write(&gui, b"").unwrap();
        std::fs::write(&cli, b"").unwrap();
        let calls = Arc::new(Mutex::new(Vec::<ShellExecuteParams>::new()));
        let seen = calls.clone();
        let err = launch_err(LaunchOptions {
            elevate: true,
            assume_elevated: Some(false),
            program: Some(gui.clone()),
            elevator: elevator(move |p| {
                seen.lock().unwrap().push(p.clone());
                Err(io::Error::from_raw_os_error(1223)) // ERROR_CANCELLED
            }),
            ..LaunchOptions::default()
        });
        assert!(err.is_denied() && err.code == "denied", "{err:?}");
        let calls = calls.lock().unwrap();
        let params = &calls[0];
        assert_eq!(params.verb, "runas");
        // the console program is elevated, never the window program
        assert_eq!(params.file, cli.to_string_lossy());
        assert!(
            params
                .parameters
                .starts_with(r"helper --pipe \\.\pipe\spaceknife-helper-"),
            "{}",
            params.parameters
        );
        let argv = split_cmdline(&params.parameters);
        let i = argv.iter().position(|a| a == "--token-file").unwrap();
        assert!(!Path::new(&argv[i + 1]).exists()); // cleaned up after the refusal
    }

    #[test]
    fn no_console_program_no_prompt() {
        // this test binary has no spaceknife-cli next to it: the helper is unavailable, and no UAC
        // prompt ever shows for the running program itself
        let calls = Arc::new(Mutex::new(Vec::<ShellExecuteParams>::new()));
        let seen = calls.clone();
        let err = launch_err(LaunchOptions {
            elevate: true,
            assume_elevated: Some(false),
            elevator: elevator(move |p| {
                seen.lock().unwrap().push(p.clone());
                Err(io::Error::from_raw_os_error(1223))
            }),
            ..LaunchOptions::default()
        });
        let exe = std::env::current_exe().unwrap();
        let calls = calls.lock().unwrap();
        assert!(
            calls.iter().all(|p| p.file != exe.to_string_lossy()),
            "{:?}",
            calls.iter().map(|p| &p.file).collect::<Vec<_>>()
        );
        if !exe.with_file_name("spaceknife-cli.exe").is_file() {
            assert!(calls.is_empty());
            assert!(err.is_unavailable() && err.code == "unavailable", "{err:?}");
            assert!(err.message.contains("spaceknife-cli"), "{}", err.message);
        }
    }

    #[test]
    fn launch_elevated_path_with_mocked_uac() {
        if unprivileged() {
            // everything after the UAC prompt is real: command line quoting, connect, hello, requests
            let mode_cmd = command("real");
            let client = HelperClient::launch(LaunchOptions {
                elevate: true,
                assume_elevated: Some(false),
                command: Some(mode_cmd),
                elevator: elevator(|p| {
                    assert_eq!(p.verb, "runas");
                    let mut argv = vec![OsString::from(&p.file)];
                    argv.extend(split_cmdline(&p.parameters));
                    assert_eq!(argv[1], "helper_entry");
                    start_subprocess(&argv).map_err(|e| io::Error::other(e.message))
                }),
                ..LaunchOptions::default()
            })
            .unwrap();
            assert_eq!(client.ping().unwrap()["pid"], client.pid());
            client.close();

            let err = launch_err(LaunchOptions {
                elevate: true,
                assume_elevated: Some(false),
                connect_timeout: Some(Duration::from_millis(500)),
                elevator: elevator(|_| {
                    std::thread::sleep(Duration::from_secs(2)); // the user leaves the prompt open
                    Err(io::Error::from_raw_os_error(1223))
                }),
                ..LaunchOptions::default()
            });
            assert!(err.code == "timeout" && !err.is_denied(), "{err:?}");
        }
    }

    #[test]
    fn real_shell_execute_runs_a_process() {
        // the real ShellExecuteExW with the "open" verb (no prompt): a process handle comes back
        let params = ShellExecuteParams {
            verb: "open".into(),
            file: std::env::current_exe()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            parameters: "helper_entry --exact -- --sk-helper-mode exit5".into(),
            directory: None,
            show: 0,
            mask: sk_core::helper::launch::SEE_MASK_NOCLOSEPROCESS
                | sk_core::helper::launch::SEE_MASK_FLAG_NO_UI
                | sk_core::helper::launch::SEE_MASK_NOASYNC,
        };
        let mut proc = sk_core::helper::win32::shell_execute(&params).unwrap();
        assert!(proc.pid() > 0);
        assert_eq!(proc.wait(Duration::from_secs(20)), Some(5));
        proc.close();
    }
}

#[cfg(target_os = "linux")]
#[test]
fn launch_through_a_stand_in_pkexec() {
    // everything but polkit is real: the helper starts through "pkexec" (a script that runs it as
    // the same user), connects and authenticates; a dismissed or refused prompt says why
    if unprivileged() {
        use sk_core::helper::launch::Elevator;
        use std::os::unix::fs::PermissionsExt;
        if std::env::var_os("APPIMAGE").is_some() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("pkexec");
        let write = |script: &str| {
            std::fs::write(&fake, format!("#!/bin/sh\n{script}\n")).unwrap();
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        let elevated_opts = || LaunchOptions {
            elevate: true,
            assume_elevated: Some(false),
            command: Some(command("real")),
            elevator: Elevator {
                pkexec_paths: vec![fake.to_string_lossy().into_owned()],
            },
            ..LaunchOptions::default()
        };
        write("exec \"$@\"");
        let client = HelperClient::launch(elevated_opts()).unwrap();
        assert_eq!(client.ping().unwrap()["pid"], client.pid());
        assert_eq!(client.disk_health("sda").unwrap_err().code, "needs_admin"); // the stand-in ran it as this user
        client.close();
        write("exit 126");
        assert!(launch_err(elevated_opts()).is_denied());
        write(
            "echo 'Error executing command as another user: No authentication agent found.' >&2; exit 127",
        );
        assert_eq!(launch_err(elevated_opts()).code, "no_agent");

        // without a command given: pkexec runs the console program next to the app, never the
        // window program
        let app = tempfile::tempdir().unwrap();
        let gui = app.path().join("spaceknife");
        let cli = app.path().join("spaceknife-cli");
        std::fs::write(&gui, "").unwrap();
        std::fs::write(&cli, "").unwrap();
        let argv_file = dir.path().join("argv");
        write(&format!(
            "printf '%s\\n' \"$@\" > '{}'; exit 126",
            argv_file.display()
        ));
        let err = launch_err(LaunchOptions {
            command: None,
            program: Some(gui),
            ..elevated_opts()
        });
        assert!(err.is_denied(), "{err:?}");
        let argv = std::fs::read_to_string(&argv_file).unwrap();
        let argv: Vec<&str> = argv.lines().collect();
        assert_eq!(argv[0], cli.to_str().unwrap());
        assert_eq!(argv[1], "helper");
        assert!(argv.contains(&"--token-file"), "{argv:?}");
    }
}

#[test]
fn disk_health_needs_linux_and_admin() {
    if unprivileged() {
        let helper = launch("fake_mft");
        let err = helper.disk_health("nvme0n1").unwrap_err();
        assert_eq!(
            err.code,
            if cfg!(target_os = "linux") {
                "needs_admin"
            } else {
                "not_supported"
            }
        );
        assert!(!err.is_unavailable());
        assert_eq!(helper.ping().unwrap()["pid"], helper.pid()); // still usable
        for bad in ["../etc/shadow", "sda1", "loop0"] {
            assert_eq!(helper.disk_health(bad).unwrap_err().code, "invalid_request"); // never sent
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
fn mac_elevation_not_supported() {
    if unprivileged() {
        let err = launch_err(LaunchOptions {
            command: Some(command("real")),
            ..LaunchOptions::default()
        });
        assert_eq!(err.code, "not_supported");
    }
}

// ========================================================================== USN journal

const TS: i64 = 133_700_000_000_000_000; // a FILETIME; the parser ignores it
const FRN_HI: u64 = 0x5EED; // V3 ids: high half (ignored), must not leak into frn

fn utf16(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// One USN_RECORD_V2 / V3 (or a V4 range record, which has no name).
#[allow(clippy::too_many_arguments)]
fn record(
    frn: u64,
    parent: u64,
    usn: i64,
    name: &[u16],
    version: u16,
    reason: u32,
    attrs: u32,
    pad: usize,
) -> Vec<u8> {
    if version == 4 {
        let mut r = Vec::new();
        r.extend_from_slice(&64u32.to_le_bytes());
        r.extend_from_slice(&4u16.to_le_bytes());
        r.extend_from_slice(&0u16.to_le_bytes());
        r.extend_from_slice(&frn.to_le_bytes());
        r.extend_from_slice(&0u64.to_le_bytes());
        r.extend_from_slice(&parent.to_le_bytes());
        r.extend_from_slice(&0u64.to_le_bytes());
        r.extend_from_slice(&usn.to_le_bytes());
        r.resize(64, 0);
        return r;
    }
    let data: Vec<u8> = name.iter().flat_map(|u| u.to_le_bytes()).collect();
    let head = if version == 2 { 60 } else { 76 };
    let name_off = head + pad;
    let length = (name_off + data.len() + 7) & !7;
    let mut r = Vec::with_capacity(length);
    r.extend_from_slice(&(length as u32).to_le_bytes());
    r.extend_from_slice(&version.to_le_bytes());
    r.extend_from_slice(&(if version == 2 { 0u16 } else { 1 }).to_le_bytes());
    r.extend_from_slice(&frn.to_le_bytes());
    if version == 3 {
        r.extend_from_slice(&FRN_HI.to_le_bytes());
    }
    r.extend_from_slice(&parent.to_le_bytes());
    if version == 3 {
        r.extend_from_slice(&FRN_HI.to_le_bytes());
    }
    r.extend_from_slice(&usn.to_le_bytes());
    r.extend_from_slice(&TS.to_le_bytes());
    r.extend_from_slice(&reason.to_le_bytes());
    r.extend_from_slice(&0u32.to_le_bytes());
    r.extend_from_slice(&0u32.to_le_bytes());
    r.extend_from_slice(&attrs.to_le_bytes());
    r.extend_from_slice(&(data.len() as u16).to_le_bytes());
    r.extend_from_slice(&(name_off as u16).to_le_bytes());
    assert_eq!(r.len(), head);
    r.resize(name_off, 0xEE);
    r.extend_from_slice(&data);
    r.resize(length, 0);
    r
}

fn rec(frn: u64, parent: u64, usn: i64, name: &str) -> Vec<u8> {
    record(frn, parent, usn, &utf16(name), 2, 0x100, 0x20, 0)
}

/// UTF-16 → WTF-8, independently of the code under test.
fn canonical(units: &[u16]) -> Vec<u8> {
    let mut out = Vec::new();
    for c in char::decode_utf16(units.iter().copied()) {
        match c {
            Ok(c) => out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes()),
            Err(e) => {
                let u = e.unpaired_surrogate() as u32;
                out.extend_from_slice(&[
                    0xE0 | (u >> 12) as u8,
                    0x80 | ((u >> 6) & 0x3F) as u8,
                    0x80 | (u & 0x3F) as u8,
                ]);
            }
        }
    }
    out
}

fn name_of(i: usize) -> Vec<u16> {
    let mut name = utf16(&format!("文件-{i}.txt"));
    if i % 7 == 3 {
        name.splice(0..0, [0xD83D, 0xDE00]); // a surrogate pair: one astral character
    }
    if i % 11 == 5 {
        name.push(0xDC80); // lone low surrogate
    }
    if i % 13 == 6 {
        name.insert(0, 0xD800); // lone high surrogate
    }
    name
}

/// (frn, parent, name, version): sequence numbers in the top 16 bits.
fn spec_of(i: usize) -> (u64, u64, Vec<u16>, u16) {
    let frn = 0x0001_0000_0000_0000 * (1 + i as u64 % 3) + 1000 + i as u64;
    let parent = 0x0005_0000_0000_0000 + 5 + i as u64 % 17;
    (frn, parent, name_of(i), if i % 4 == 1 { 3 } else { 2 })
}

fn names_of(rows: &UsnRows) -> Vec<Vec<u8>> {
    (0..rows.len()).map(|i| rows.name(i).to_vec()).collect()
}

fn buffer(next_usn: i64, records: &[Vec<u8>]) -> Vec<u8> {
    let mut b = next_usn.to_le_bytes().to_vec();
    for r in records {
        b.extend_from_slice(r);
    }
    b
}

/// An in-memory journal: USNs are byte offsets, as on NTFS. `read` mimics FSCTL_READ_USN_JOURNAL.
#[derive(Clone)]
struct FakeJournal {
    journal_id: u64,
    first_usn: i64,
    next_usn: i64,
    records: Vec<(i64, Vec<u8>)>,
    per_read: Option<usize>,
    delay: Duration,
    reads: Arc<std::sync::Mutex<Vec<(i64, usize)>>>,
    grow: usize,
    fail_read: Option<UsnError>,
    recreate_on_read: bool,
    garbage: bool,
    busy: Option<Arc<std::sync::Mutex<Duration>>>, // a simulated clock each read advances by 10 ms
}

impl FakeJournal {
    fn new(count: usize, v4: bool) -> Self {
        let mut j = FakeJournal {
            journal_id: 0xD1CE_1234_5678,
            first_usn: 1 << 20,
            next_usn: 1 << 20,
            records: Vec::new(),
            per_read: None,
            delay: Duration::ZERO,
            reads: Default::default(),
            grow: 0,
            fail_read: None,
            recreate_on_read: false,
            garbage: false,
            busy: None,
        };
        for i in 0..count {
            let (frn, parent, name, version) = spec_of(i);
            j.append(frn, parent, &name, version);
            if v4 && i % 50 == 25 {
                j.append(9, 9, &[], 4);
            }
        }
        j
    }

    fn append(&mut self, frn: u64, parent: u64, name: &[u16], version: u16) {
        let r = record(frn, parent, self.next_usn, name, version, 0x100, 0x20, 0);
        self.records.push((self.next_usn, r.clone()));
        self.next_usn += r.len() as i64;
    }

    fn reads(&self) -> Vec<(i64, usize)> {
        self.reads.lock().unwrap().clone()
    }
}

impl UsnDevice for FakeJournal {
    fn query(&mut self) -> Result<Journal, UsnError> {
        Ok(Journal {
            journal_id: self.journal_id,
            first_usn: self.first_usn,
            next_usn: self.next_usn,
            lowest_valid_usn: self.first_usn,
            max_usn: 1 << 60,
            max_size: 32 << 20,
            allocation_delta: 8 << 20,
            max_major: 3,
        })
    }

    fn read(&mut self, start_usn: i64, journal_id: u64, size: usize) -> Result<Vec<u8>, UsnError> {
        self.reads.lock().unwrap().push((start_usn, size));
        if let Some(clock) = &self.busy {
            *clock.lock().unwrap() += Duration::from_millis(10);
        }
        std::thread::sleep(self.delay);
        if let Some(e) = &self.fail_read {
            return Err(e.clone());
        }
        if self.recreate_on_read {
            self.journal_id += 1;
            return Err(UsnError::new(
                "io_error",
                "读取失败：[WinError 87] 参数错误。",
            ));
        }
        if journal_id != self.journal_id {
            return Err(UsnError::new(
                "io_error",
                "读取失败：[WinError 87] 参数错误。",
            ));
        }
        if start_usn < self.first_usn {
            return Err(UsnError::new(
                "usn_gone",
                "读取失败：[WinError 1181] 日志项已删除。",
            ));
        }
        if self.garbage {
            let mut b = self.next_usn.to_le_bytes().to_vec();
            b.extend_from_slice(&4u32.to_le_bytes());
            b.extend_from_slice(&2u16.to_le_bytes());
            b.extend_from_slice(&0u16.to_le_bytes());
            return Ok(b);
        }
        let mut out: Vec<Vec<u8>> = Vec::new();
        let mut used = 8;
        let mut nxt = self.next_usn;
        for (pos, r) in &self.records {
            if *pos < start_usn {
                continue;
            }
            if used + r.len() > size || self.per_read.is_some_and(|n| out.len() >= n) {
                nxt = *pos;
                break;
            }
            used += r.len();
            out.push(r.clone());
        }
        if out.is_empty() && nxt != self.next_usn {
            return Err(UsnError::new(
                "io_error",
                "读取失败：[WinError 122] 缓冲区太小。",
            ));
        }
        for i in 0..self.grow {
            // the volume keeps changing while we read
            let name = utf16(&format!("new-{}-{i}", self.records.len()));
            self.append(77, 5, &name, 2);
        }
        Ok(buffer(nxt, &out))
    }
}

fn journal_for(volume: &str) -> Result<FakeJournal, UsnError> {
    match volume {
        "C:" => Ok(FakeJournal::new(3000, true)),
        "S:" => {
            // slow: several reads with heartbeats in between
            let mut j = FakeJournal::new(1000, false);
            j.per_read = Some(200);
            j.delay = Duration::from_millis(300);
            Ok(j)
        }
        "D:" => Err(UsnError::new("usn_inactive", "D: 没有启用 USN 日志")),
        "E:" => Err(UsnError::new("needs_admin", "拒绝访问。")),
        _ => {
            let mut j = FakeJournal::new(100, false);
            match volume {
                "F:" => j.fail_read = Some(UsnError::new("io_error", "数据错误（循环冗余检查）。")),
                "G:" => j.garbage = true,
                "H:" => j.recreate_on_read = true,
                _ => {}
            }
            Ok(j)
        }
    }
}

fn fake_check_ntfs(volume: &str) -> Result<Scalars, OpError> {
    if volume.eq_ignore_ascii_case("N:") {
        return Err(OpError::new("not_ntfs", "N: 不是 NTFS 卷（FAT32）"));
    }
    let mut m = Scalars::new();
    m.insert("drive_type".into(), 3.into());
    m.insert("fs_type".into(), "NTFS".into());
    Ok(m)
}

// --------------------------------------------------------------------------- parser

#[test]
fn parse_v2_records() {
    let names = ["a.txt", "目录", &"x".repeat(255), "with space .tmp"];
    let mut recs = Vec::new();
    let mut pos = 4096;
    for (i, name) in names.iter().enumerate() {
        recs.push(record(
            100 + i as u64,
            5,
            pos,
            &utf16(name),
            2,
            0x8000_0100 + i as u32,
            0x10 * i as u32,
            0,
        ));
        pos += recs.last().unwrap().len() as i64;
    }
    let chunk = usn::parse_read_buffer(&buffer(pos, &recs), None).unwrap();
    assert_eq!(
        (
            chunk.rows.len(),
            chunk.truncated,
            chunk.skipped,
            chunk.next_usn
        ),
        (4, false, 0, pos)
    );
    assert_eq!(chunk.rows.frn, [100, 101, 102, 103]);
    assert_eq!(chunk.rows.parent_frn, [5; 4]);
    let mut at = 4096;
    let want_usn: Vec<i64> = recs
        .iter()
        .map(|r| {
            let u = at;
            at += r.len() as i64;
            u
        })
        .collect();
    assert_eq!(chunk.rows.usn, want_usn);
    assert_eq!(
        chunk.rows.reason,
        (0..4).map(|i| 0x8000_0100 + i).collect::<Vec<u32>>()
    );
    assert_eq!(chunk.rows.attrs, [0, 0x10, 0x20, 0x30]);
    assert_eq!(
        names_of(&chunk.rows),
        names
            .iter()
            .map(|n| n.as_bytes().to_vec())
            .collect::<Vec<_>>()
    );
    assert_eq!(chunk.rows.name_off[0], 0);
    assert_eq!(
        chunk.rows.names.len(),
        names.iter().map(|n| n.len()).sum::<usize>()
    );
    assert!(recs.iter().all(|r| r.len() % 8 == 0)); // records are 8-byte aligned
}

#[test]
fn parse_v3_takes_the_low_64_bits() {
    let big = 0xFFFF_0000_0000_0010u64; // sequence in the top 16 bits, record number below
    let recs = [
        record(
            big,
            0x0003_0000_0000_0005,
            64,
            &utf16("v3.bin"),
            3,
            0x100,
            0x20,
            0,
        ),
        record(7, 5, 200, &utf16("v2.bin"), 2, 0x100, 0x20, 0),
        record(9, 9, 300, &[], 4, 0, 0, 0), // range record: skipped
        record(8, 5, 400, &utf16("again"), 3, 0x100, 0x20, 0),
    ];
    let chunk = usn::parse_read_buffer(&buffer(999, &recs), None).unwrap();
    assert_eq!((chunk.rows.len(), chunk.skipped), (3, 1));
    assert_eq!(chunk.rows.frn, [big, 7, 8]); // FRN_HI never shows up
    assert_eq!(chunk.rows.parent_frn, [0x0003_0000_0000_0005, 5, 5]);
    assert_eq!(chunk.rows.usn, [64, 200, 400]);
    assert_eq!(
        names_of(&chunk.rows),
        [b"v3.bin".to_vec(), b"v2.bin".to_vec(), b"again".to_vec()]
    );
}

#[test]
fn parse_names_with_surrogates() {
    let names: Vec<Vec<u16>> = vec![
        [vec![0xD83D, 0xDE00], utf16("smile")].concat(), // pair → one character, 4 UTF-8 bytes
        [utf16("lone-high"), vec![0xD800]].concat(),     // ends with a high surrogate ...
        [vec![0xDC00], utf16("lone-low")].concat(), // ... the next starts with a low one: no pairing across names
        vec![0xDFFF],
        utf16("中文名字"),
        utf16("ascii"),
        vec![],
    ];
    let recs: Vec<_> = names
        .iter()
        .enumerate()
        .map(|(i, n)| {
            record(
                i as u64,
                5,
                8 * (i as i64 + 1),
                n,
                2 + (i % 2) as u16,
                0x100,
                0x20,
                0,
            )
        })
        .collect();
    let chunk = usn::parse_read_buffer(&buffer(0, &recs), None).unwrap();
    assert_eq!(
        names_of(&chunk.rows),
        names.iter().map(|n| canonical(n)).collect::<Vec<_>>()
    );
    assert_eq!(chunk.rows.name_len[0], 4 + 5);
    assert_eq!(chunk.rows.name_len[1], 9 + 3);
    assert_eq!(names_of(&chunk.rows)[0], "😀smile".as_bytes());
    assert_eq!(names_of(&chunk.rows)[3], [0xED, 0xBF, 0xBF]);
}

#[test]
fn parse_alignment_and_name_offsets() {
    let recs: Vec<_> = (0..20)
        .map(|i| {
            record(
                i,
                5,
                100 + i as i64,
                &utf16(&format!("n{i}").repeat(i as usize + 1)),
                2 + (i % 2) as u16,
                0x100,
                0x20,
                8 * (i as usize % 3),
            )
        })
        .collect();
    let data = buffer(12345, &recs);
    let want = usn::parse_read_buffer(&data, None).unwrap();
    // FileNameOffset is honoured
    assert_eq!(
        names_of(&want.rows),
        (0..20)
            .map(|i| format!("n{i}").repeat(i + 1).into_bytes())
            .collect::<Vec<_>>()
    );
    for lead in [1, 3, 5] {
        // the buffer itself at an odd address
        let mut raw = vec![0u8; lead];
        raw.extend_from_slice(&data);
        let got = usn::parse_read_buffer(&raw[lead..], None).unwrap();
        assert_eq!(got.rows, want.rows);
        assert_eq!(got.next_usn, 12345);
        let cut = usn::parse_read_buffer(&raw[lead..], Some(5)).unwrap(); // next USN read at an odd address
        assert!(cut.truncated);
        assert_eq!(cut.next_usn, want.rows.usn[5]);
        assert_eq!(cut.next_usn, 105);
    }
}

#[test]
fn parse_limit_and_continuation() {
    let recs: Vec<_> = (0..10)
        .map(|i| rec(i, 5, 1000 + 128 * i as i64, &format!("f{i}")))
        .collect();
    let data = buffer(5000, &recs);
    let full = usn::parse_read_buffer(&data, None).unwrap();
    let first = usn::parse_read_buffer(&data, Some(4)).unwrap();
    assert!(first.rows.len() == 4 && first.truncated);
    assert_eq!(first.next_usn, 1000 + 128 * 4); // the first record not returned
    assert_eq!(names_of(&first.rows), names_of(&full.rows)[..4]);
    let nothing = usn::parse_read_buffer(&data, Some(0)).unwrap();
    assert!(nothing.rows.is_empty() && nothing.truncated && nothing.next_usn == 1000);
    let exact = usn::parse_read_buffer(&data, Some(10)).unwrap();
    assert!(exact.rows.len() == 10 && !exact.truncated && exact.next_usn == 5000);
    let empty = usn::parse_read_buffer(&buffer(777, &[]), None).unwrap();
    assert!(
        empty.rows.is_empty()
            && !empty.truncated
            && empty.next_usn == 777
            && empty.rows.names.is_empty()
    );
}

#[test]
fn parse_rejects_malformed_buffers() {
    let good = rec(1, 5, 64, "name.txt");
    let mut zero = vec![0u8; 64];
    zero[4..6].copy_from_slice(&2u16.to_le_bytes()); // RecordLength 0 (would loop forever)
    let mut bad_name = good.clone();
    bad_name[56..58].copy_from_slice(&200u16.to_le_bytes()); // FileNameLength past the record
    let mut odd_name = good.clone();
    odd_name[56..58].copy_from_slice(&3u16.to_le_bytes());
    let mut early_name = good.clone();
    early_name[58..60].copy_from_slice(&40u16.to_le_bytes()); // FileNameOffset inside the fixed part
    let mut short = vec![0u8; 40];
    short[0..4].copy_from_slice(&40u32.to_le_bytes());
    short[4..6].copy_from_slice(&2u16.to_le_bytes());
    let cases: Vec<Vec<u8>> = vec![
        vec![0; 7], // shorter than the next USN
        buffer(0, &[zero]),
        buffer(0, &[good[..good.len() - 8].to_vec()]), // runs past the end of the buffer
        buffer(0, &[good.clone(), vec![0; 4]]),        // a partial header at the end
        buffer(0, &[short]),                           // shorter than a V2 header
        buffer(0, &[bad_name]),
        buffer(0, &[odd_name]),
        buffer(0, &[early_name]),
    ];
    for data in cases {
        let err = usn::parse_read_buffer(&data, None).unwrap_err();
        assert!(err.contains("USN"), "{err}");
    }
}

#[test]
fn parse_many_records_matches_a_reference() {
    let alphabet: Vec<Vec<u16>> = "abcxyz.-_ 文件夹ü"
        .chars()
        .map(|c| utf16(&c.to_string()))
        .chain([vec![0xD83D, 0xDE00], vec![0xD800], vec![0xDC01]])
        .collect();
    let mut x = 5u64;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let names: Vec<Vec<u16>> = (0..20_000)
        .map(|_| {
            (0..1 + next() % 39)
                .flat_map(|_| alphabet[(next() % alphabet.len() as u64) as usize].clone())
                .collect()
        })
        .collect();
    let recs: Vec<_> = names
        .iter()
        .enumerate()
        .map(|(i, n)| {
            record(
                i as u64,
                i as u64 / 7,
                64 * i as i64,
                n,
                2 + (i % 2) as u16,
                0x100,
                0x20,
                0,
            )
        })
        .collect();
    let data = buffer(1 << 40, &recs);
    let t0 = Instant::now();
    let chunk = usn::parse_read_buffer(&data, None).unwrap();
    println!(
        "USN parse: {} records, {:.1} MB in {:.0} ms",
        chunk.rows.len(),
        data.len() as f64 / 1e6,
        t0.elapsed().as_secs_f64() * 1e3
    );
    assert_eq!(
        names_of(&chunk.rows),
        names.iter().map(|n| canonical(n)).collect::<Vec<_>>()
    );
    assert_eq!(
        chunk.rows.parent_frn,
        (0..20_000u64).map(|i| i / 7).collect::<Vec<_>>()
    );
}

#[test]
fn append_rebases_names() {
    let a = usn::parse_read_buffer(
        &buffer(
            0,
            &(0..3)
                .map(|i| rec(i, 5, 8 * i as i64, &format!("a{i}")))
                .collect::<Vec<_>>(),
        ),
        None,
    )
    .unwrap();
    let b = usn::parse_read_buffer(
        &buffer(
            0,
            &(0..4)
                .map(|i| rec(i, 5, 8 * i as i64, &format!("bb{i}")))
                .collect::<Vec<_>>(),
        ),
        None,
    )
    .unwrap();
    let mut joined = a.rows.clone();
    joined.append(&b.rows).unwrap();
    let names: Vec<String> = names_of(&joined)
        .into_iter()
        .map(|n| String::from_utf8(n).unwrap())
        .collect();
    assert_eq!(names, ["a0", "a1", "a2", "bb0", "bb1", "bb2", "bb3"]);
    assert_eq!(joined.frn, [0, 1, 2, 0, 1, 2, 3]);
}

// --------------------------------------------------------------------------- read loop (in-process)

#[test]
fn read_journal_whole_and_in_pieces() {
    let mut journal = FakeJournal::new(500, true);
    let (jid, first, end) = (journal.journal_id, journal.first_usn, journal.next_usn);
    let (table, stats) =
        usn::read_journal(&mut journal, first, jid, ReadOptions::rows(10_000)).unwrap();
    assert_eq!(
        (
            table.rows.len(),
            table.truncated,
            table.next_usn,
            table.journal_id
        ),
        (500, false, end, jid)
    );
    assert_eq!(
        names_of(&table.rows),
        (0..500).map(|i| canonical(&name_of(i))).collect::<Vec<_>>()
    );
    assert_eq!((stats.skipped, stats.reads), (10, 1));
    assert!(stats.bytes_read > 500 * 60);
    // in pieces of 64: every piece continues exactly where the last one stopped
    let mut seen = Vec::new();
    let mut pos = first;
    loop {
        let (part, _) = usn::read_journal(&mut journal, pos, jid, ReadOptions::rows(64)).unwrap();
        assert!(part.rows.len() <= 64);
        seen.extend(names_of(&part.rows));
        pos = part.next_usn;
        if !part.truncated {
            break;
        }
    }
    assert_eq!(seen, names_of(&table.rows));
    assert_eq!(pos, end);
    let sizes: std::collections::BTreeSet<usize> =
        journal.reads()[1..].iter().map(|r| r.1).collect();
    assert_eq!(
        sizes.into_iter().collect::<Vec<_>>(),
        [usn::buffer_size(64)]
    );
    assert!(usn::buffer_size(64) <= 8 << 10); // small reads stay small
}

#[test]
fn read_journal_checkpoint_checks() {
    let mut journal = FakeJournal::new(20, false);
    let (jid, first, end) = (journal.journal_id, journal.first_usn, journal.next_usn);
    for (start, ident) in [(first, jid + 1), (first - 8, jid), (end + 8, jid)] {
        let err = usn::read_journal(&mut journal, start, ident, ReadOptions::rows(10)).unwrap_err();
        assert_eq!(err.code, "usn_gone");
    }
    assert!(journal.reads().is_empty()); // refused before reading anything
    let (table, stats) = usn::read_journal(&mut journal, end, jid, ReadOptions::rows(10)).unwrap(); // up to date
    assert!(table.rows.is_empty() && table.next_usn == end && !table.truncated);
    assert_eq!(stats.reads, 0);
    assert!(journal.reads().is_empty());
}

#[test]
fn read_journal_stops_at_the_end_seen_at_start() {
    let mut journal = FakeJournal::new(300, false);
    journal.per_read = Some(40);
    let end = journal.next_usn;
    journal.grow = 5; // every read appends records: the loop must not chase them forever
    let (jid, first) = (journal.journal_id, journal.first_usn);
    let (table, _) =
        usn::read_journal(&mut journal, first, jid, ReadOptions::rows(100_000)).unwrap();
    assert!(table.rows.len() >= 300 && table.next_usn >= end);
    assert_eq!(journal.reads().len(), 8); // 300 / 40 → 8 reads, then the end seen at the start is reached
}

#[test]
fn read_journal_duty_and_progress() {
    let clock = Arc::new(std::sync::Mutex::new(Duration::ZERO));
    let mut journal = FakeJournal::new(100, false);
    journal.per_read = Some(25);
    journal.busy = Some(clock.clone()); // every read keeps the disk busy for 10 ms
    let (jid, first, total) = (
        journal.journal_id,
        journal.first_usn,
        (journal.next_usn - journal.first_usn) as u64,
    );
    let mut slept = Vec::new();
    let mut progress = Vec::new();
    let mut on_progress = |done, total| progress.push((done, total));
    let opts = ReadOptions {
        max_rows: 1000,
        duty: 0.25,
        progress: Some(&mut on_progress),
    };
    let c2 = clock.clone();
    usn::read_journal_paced(
        &mut journal,
        first,
        jid,
        opts,
        &mut || *c2.lock().unwrap(),
        &mut |d| slept.push(d),
    )
    .unwrap();
    // busy 10 ms at 25 % → idle 30 ms; none after the last read
    assert_eq!(slept.len(), 3);
    assert!(
        slept.iter().all(|d| (d.as_secs_f64() - 0.03).abs() < 1e-6),
        "{slept:?}"
    );
    assert_eq!(progress.iter().map(|p| p.1).collect::<Vec<_>>(), [total; 4]);
    assert!(progress.windows(2).all(|w| w[0].0 <= w[1].0) && progress.last().unwrap().0 == total);
}

#[test]
fn read_journal_errors() {
    let mut journal = FakeJournal::new(10, false);
    journal.recreate_on_read = true; // the journal was recreated between query and read
    let (jid, first) = (journal.journal_id, journal.first_usn);
    let err = usn::read_journal(&mut journal, first, jid, ReadOptions::rows(10)).unwrap_err();
    assert!(
        err.code == "usn_gone" && (err.message.contains("重建") || !err.message.is_empty()),
        "{err:?}"
    );
    let mut journal = FakeJournal::new(10, false);
    journal.fail_read = Some(UsnError::new("io_error", "设备未就绪"));
    let err = usn::read_journal(&mut journal, first, jid, ReadOptions::rows(10)).unwrap_err();
    assert_eq!(err.code, "io_error"); // same journal: the read error stays an I/O error
    let mut journal = FakeJournal::new(10, false);
    journal.garbage = true;
    let err = usn::read_journal(&mut journal, first, jid, ReadOptions::rows(10)).unwrap_err();
    assert!(
        err.code == "io_error" && err.message.contains("USN"),
        "{err:?}"
    );
}

#[test]
fn error_code_mapping() {
    assert_eq!(usn::error_code(ERROR_JOURNAL_NOT_ACTIVE), "usn_inactive");
    assert_eq!(
        usn::error_code(ERROR_JOURNAL_DELETE_IN_PROGRESS),
        "usn_inactive"
    );
    assert_eq!(usn::error_code(ERROR_JOURNAL_ENTRY_DELETED), "usn_gone");
    assert_eq!(usn::error_code(ERROR_ACCESS_DENIED), "needs_admin");
    assert_eq!(usn::error_code(ERROR_PRIVILEGE_NOT_HELD), "needs_admin");
    assert_eq!(usn::error_code(ERROR_INVALID_FUNCTION), "not_supported");
    assert_eq!(usn::error_code(ERROR_NOT_SUPPORTED), "not_supported");
    assert_eq!(usn::error_code(ERROR_INVALID_PARAMETER), "io_error");
    assert_eq!(usn::error_code(23), "io_error"); // ERROR_CRC
    if !cfg!(windows) {
        assert_eq!(usn::open_volume("C:").err().unwrap().code, "not_supported");
    }
}

// --------------------------------------------------------------------------- live helper (fake journal)

fn i64_of(m: &Scalars, k: &str) -> i64 {
    m[k].as_i64().unwrap()
}

fn u64_of(m: &Scalars, k: &str) -> u64 {
    m[k].as_u64().unwrap()
}

#[test]
fn query_and_read_through_the_helper() {
    if unprivileged() {
        let helper = launch("usn");
        assert!(helper.elevated()); // claimed by the patched helper
        let mut want = FakeJournal::new(3000, true);
        let info = helper.query_usn("c:").unwrap();
        let mut expect = Scalars::new();
        expect.insert("volume".into(), "C:".into());
        expect.extend(want.query().unwrap().scalars());
        assert_eq!(info, expect);
        let got = helper
            .read_usn(
                "C:",
                i64_of(&info, "first_usn"),
                u64_of(&info, "journal_id"),
                1_000_000,
                1.0,
                None,
            )
            .unwrap();
        assert_eq!(
            (got.rows.len(), got.truncated, got.journal_id, got.next_usn),
            (3000, false, want.journal_id, want.next_usn)
        );
        let specs: Vec<_> = (0..3000).map(spec_of).collect();
        assert_eq!(got.rows.frn, specs.iter().map(|s| s.0).collect::<Vec<_>>());
        assert_eq!(
            got.rows.parent_frn,
            specs.iter().map(|s| s.1).collect::<Vec<_>>()
        );
        assert_eq!(
            names_of(&got.rows),
            specs.iter().map(|s| canonical(&s.2)).collect::<Vec<_>>()
        );
        assert!(got.rows.usn.windows(2).all(|w| w[1] > w[0]) && got.rows.usn[0] == want.first_usn);
        let stats = helper.last_usn();
        assert_eq!(
            (
                u64_of(&stats, "rows"),
                u64_of(&stats, "frames"),
                u64_of(&stats, "skipped")
            ),
            (3000, 1, 60)
        );
        assert_eq!(
            (u64_of(&stats, "reads"), stats["volume"].as_str()),
            (1, Some("C:"))
        );
        assert!(helper.last_scan().is_empty()); // journal reads leave the scan statistics alone
    }
}

#[test]
fn read_in_pieces_through_the_helper() {
    if unprivileged() {
        let helper = launch("usn");
        let info = helper.query_usn("C:").unwrap();
        let jid = u64_of(&info, "journal_id");
        let mut pos = i64_of(&info, "first_usn");
        let mut names = Vec::new();
        let mut pieces = 0;
        loop {
            let part = helper.read_usn("C:", pos, jid, 700, 0.9, None).unwrap();
            pieces += 1;
            assert!(part.rows.len() <= 700);
            names.extend(names_of(&part.rows));
            pos = part.next_usn;
            if !part.truncated {
                break;
            }
        }
        assert_eq!(
            (pieces, names.len(), pos),
            (5, 3000, i64_of(&info, "next_usn"))
        );
        let up_to_date = helper
            .read_usn("C:", pos, jid, 1_000_000, 1.0, None)
            .unwrap();
        assert!(
            up_to_date.rows.is_empty()
                && up_to_date.next_usn == pos
                && up_to_date.rows.names.is_empty()
        );
    }
}

#[test]
fn heartbeats_while_reading() {
    if unprivileged() {
        let helper = launch("usn");
        let mut seen = Vec::new();
        let info = helper.query_usn("S:").unwrap();
        let mut on_progress =
            |phase: Phase, _: f64, fraction: Option<f64>| seen.push((phase, fraction));
        let got = helper
            .read_usn(
                "S:",
                i64_of(&info, "first_usn"),
                u64_of(&info, "journal_id"),
                1_000_000,
                1.0,
                Some(&mut on_progress),
            )
            .unwrap();
        assert_eq!(
            (got.rows.len(), u64_of(&helper.last_usn(), "reads")),
            (1000, 5)
        );
        let fractions: Vec<f64> = seen
            .iter()
            .filter(|(p, _)| *p == Phase::Scan)
            .map(|(_, f)| f.unwrap_or(-1.0))
            .collect();
        assert!(
            !fractions.is_empty() && fractions.iter().all(|&f| 0.0 < f && f <= 1.0),
            "{fractions:?}"
        );
        assert!(fractions.iter().any(|&f| f < 1.0)); // a heartbeat in the middle of the read
    }
}

#[test]
fn errors_through_the_helper() {
    if unprivileged() {
        let helper = launch("usn");
        let info = helper.query_usn("C:").unwrap();
        let (jid, first) = (u64_of(&info, "journal_id"), i64_of(&info, "first_usn"));
        for (volume, start, ident, code) in [
            ("C:", first, jid ^ 0xFF, "usn_gone"), // journal recreated since the checkpoint
            ("C:", first - 64, jid, "usn_gone"),   // checkpoint's records overwritten
            ("D:", 0, 0, "usn_inactive"),
            ("E:", 0, 0, "needs_admin"),
            ("N:", 0, 0, "not_ntfs"),
        ] {
            let err = helper
                .read_usn(volume, start, ident, 1_000_000, 1.0, None)
                .unwrap_err();
            assert_eq!(err.code, code, "{volume}: {}", err.message);
            assert!(!err.is_unavailable());
        }
        let j = FakeJournal::new(100, false);
        for (volume, code) in [("F:", "io_error"), ("G:", "io_error"), ("H:", "usn_gone")] {
            let err = helper
                .read_usn(volume, j.first_usn, j.journal_id, 1_000_000, 1.0, None)
                .unwrap_err();
            assert_eq!(err.code, code, "{volume}: {}", err.message);
        }
        for (volume, code) in [
            ("D:", "usn_inactive"),
            ("E:", "needs_admin"),
            ("N:", "not_ntfs"),
        ] {
            assert_eq!(helper.query_usn(volume).unwrap_err().code, code);
        }
        assert_eq!(
            helper
                .read_usn("C:\\Windows", 0, 0, 1_000_000, 1.0, None)
                .unwrap_err()
                .code,
            "invalid_request"
        );
        assert_eq!(
            helper
                .read_usn("C:", -1, 0, 1_000_000, 1.0, None)
                .unwrap_err()
                .code,
            "invalid_request"
        );
        assert_eq!(
            helper.read_usn("C:", 0, 0, 0, 1.0, None).unwrap_err().code,
            "invalid_request"
        );
        assert!(helper.alive());
        assert_eq!(helper.ping().unwrap()["pid"], helper.pid()); // still usable
    }
}

#[test]
fn unelevated_helper_needs_admin() {
    if unprivileged() {
        let client = launch("real"); // the real helper, no seams
        assert!(!client.elevated());
        assert_eq!(client.query_usn("C:").unwrap_err().code, "needs_admin");
        assert_eq!(
            client
                .read_usn("C:", 0, 1, 1_000_000, 1.0, None)
                .unwrap_err()
                .code,
            "needs_admin"
        );
        assert_eq!(client.ping().unwrap()["pid"], client.pid());
    }
}
