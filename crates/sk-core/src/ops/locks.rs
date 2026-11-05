//! Which processes hold files open (docs/manage.md §2.4, UI-W5-37).
//!
//! * Windows: Restart Manager; the friendly name is `strAppName` ("Microsoft Word"). A folder
//!   registers its files (at most [`MAX_FILES`] per call). One session answers "is anything held
//!   at all"; only when something is, sessions on halves of the set find which paths.
//! * Linux: the links in `/proc/<pid>/fd` (processes we may not inspect are skipped). An open
//!   file does not block deleting it there, but the space only comes back once it is closed.
//! * macOS: `lsof -Fpcn` when it is installed (best effort, not tested on a Mac), else nothing.
//!
//! Synchronous and possibly slow (Restart Manager takes about 0.1–0.2 s per session): call it
//! from a worker thread.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Names registered per call at most (a folder's walk stops there) ...
pub const MAX_FILES: usize = 20_000;
/// ... and per folder.
pub const PER_FOLDER: usize = 4_000;
/// Seconds `lsof` may take.
pub const LSOF_TIMEOUT_S: u64 = 20;

/// Path → friendly names of the processes using it (a folder: anything inside it).
///
/// Only paths that are in use appear in the result. Never fails: when detection itself fails
/// the answer is "nothing known to be in use" (and a debug log line).
pub fn find_lockers(paths: &[String]) -> HashMap<String, Vec<String>> {
    if paths.is_empty() {
        return HashMap::new();
    }
    let run = std::panic::catch_unwind(|| {
        #[cfg(windows)]
        {
            windows(paths)
        }
        #[cfg(target_os = "macos")]
        {
            darwin(paths)
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            linux(paths, Path::new("/proc"))
        }
        #[cfg(not(any(windows, unix)))]
        {
            HashMap::new()
        }
    });
    run.unwrap_or_else(|_| {
        tracing::debug!("lock detection failed");
        HashMap::new()
    })
}

fn unique(names: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for n in names {
        if !out.contains(&n) {
            out.push(n);
        }
    }
    out
}

// --------------------------------------------------------------------------- Windows

/// `path` itself, or for a folder the files inside it (not following links), at most `budget`.
/// Restart Manager takes files only: a folder among them fails the whole query.
#[cfg_attr(not(windows), allow(dead_code))]
fn walk_files(path: &Path, budget: usize) -> Vec<String> {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return vec![path.to_string_lossy().into_owned()];
    };
    if meta.file_type().is_symlink() {
        // a link (or junction) to a folder: nothing of its own to hold
        return if std::fs::metadata(path).is_ok_and(|m| m.is_dir()) {
            Vec::new()
        } else {
            vec![path.to_string_lossy().into_owned()]
        };
    }
    if !meta.is_dir() {
        return vec![path.to_string_lossy().into_owned()];
    }
    let mut names = Vec::new();
    let mut stack: Vec<PathBuf> = vec![path.to_path_buf()];
    while let Some(folder) = stack.pop() {
        if names.len() >= budget {
            break;
        }
        let Ok(it) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in it.flatten() {
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_dir() {
                stack.push(entry.path());
                continue;
            }
            if ft.is_symlink() && std::fs::metadata(entry.path()).is_ok_and(|m| m.is_dir()) {
                continue; // a junction / directory link: not followed
            }
            names.push(entry.path().to_string_lossy().into_owned());
            if names.len() >= budget {
                break;
            }
        }
    }
    names
}

#[cfg(windows)]
fn windows(paths: &[String]) -> HashMap<String, Vec<String>> {
    let mut budget = MAX_FILES;
    let mut names: HashMap<&str, Vec<String>> = HashMap::new();
    let mut order: Vec<&str> = Vec::new();
    for p in paths {
        if budget == 0 {
            break;
        }
        let exists = std::fs::symlink_metadata(p).is_ok();
        let got = if exists {
            walk_files(Path::new(p), PER_FOLDER.min(budget))
        } else {
            Vec::new()
        };
        if !got.is_empty() && !names.contains_key(p.as_str()) {
            budget = budget.saturating_sub(got.len());
            names.insert(p, got);
            order.push(p);
        }
    }
    if names.is_empty() {
        return HashMap::new();
    }
    // who holds any file of `group` open; None when Restart Manager refuses the query
    let held = |group: &[&str]| -> Option<Vec<String>> {
        let files: Vec<String> = group.iter().flat_map(|p| names[p].iter().cloned()).collect();
        match super::win::rm_lockers(&files) {
            Ok(list) => Some(unique(list.into_iter().map(|(_, n)| n))),
            Err(e) => {
                if group.len() == 1 {
                    tracing::debug!("lock detection skipped {}: {e}", group[0]);
                }
                None
            }
        }
    };
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    let mut pending: Vec<Vec<&str>> = vec![order];
    while let Some(group) = pending.pop() {
        let who = held(&group);
        if who.as_ref().is_some_and(Vec::is_empty) {
            continue;
        }
        if group.len() == 1 {
            if let Some(w) = who {
                out.insert(group[0].to_string(), w);
            }
            continue;
        }
        // someone holds something here (or one name spoils the query)
        let half = group.len() / 2;
        pending.push(group[..half].to_vec());
        pending.push(group[half..].to_vec());
    }
    out
}

// --------------------------------------------------------------------------- Linux

/// Parent folders resolved, the last component kept (a link is itself, not its target).
#[cfg_attr(windows, allow(dead_code))]
fn resolved(path: &str) -> String {
    super::paths::resolved(Path::new(path))
        .to_string_lossy()
        .into_owned()
}

#[cfg_attr(windows, allow(dead_code))]
fn dirname(p: &str) -> &str {
    match p.rfind('/') {
        Some(0) => "/",
        Some(i) => &p[..i],
        None => p,
    }
}

#[cfg_attr(windows, allow(dead_code))]
fn process_name(pid: &str, proc: &Path) -> String {
    if let Ok(exe) = std::fs::read_link(proc.join(pid).join("exe")) {
        let exe = exe.to_string_lossy();
        let exe = exe.strip_suffix(" (deleted)").unwrap_or(&exe);
        if let Some(base) = exe.rsplit('/').next().filter(|b| !b.is_empty()) {
            return base.to_string();
        }
    }
    match std::fs::read(proc.join(pid).join("comm")) {
        Ok(raw) => {
            let s = String::from_utf8_lossy(&raw).trim().to_string();
            if s.is_empty() { format!("PID {pid}") } else { s }
        }
        Err(_) => format!("PID {pid}"),
    }
}

/// The `/proc` scan (`proc`: the proc file system's root; tests point it at a fake one).
#[cfg_attr(windows, allow(dead_code))]
pub fn linux(paths: &[String], proc: &Path) -> HashMap<String, Vec<String>> {
    let mut wanted: HashMap<String, Vec<&String>> = HashMap::new();
    for p in paths {
        wanted.entry(resolved(p)).or_default().push(p);
    }
    let mut found: HashMap<String, Vec<String>> = HashMap::new();
    let Ok(dir) = std::fs::read_dir(proc) else {
        return HashMap::new();
    };
    let mut pids: Vec<(u64, String)> = dir
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.parse::<u64>().ok().map(|n| (n, name))
        })
        .collect();
    pids.sort();
    for (_, pid) in pids {
        let fd_dir = proc.join(&pid).join("fd");
        let Ok(fds) = std::fs::read_dir(&fd_dir) else {
            continue; // gone, or not ours to inspect
        };
        let mut name: Option<String> = None;
        let mut fds: Vec<PathBuf> = fds.flatten().map(|e| e.path()).collect();
        fds.sort();
        for fd in fds {
            let Ok(target) = std::fs::read_link(&fd) else {
                continue;
            };
            let target = target.to_string_lossy().into_owned();
            if !target.starts_with('/') || target.ends_with(" (deleted)") {
                continue; // socket:[…], pipe:[…], anon_inode:…
            }
            let mut hit = target.as_str();
            loop {
                if let Some(asked) = wanted.get(hit) {
                    let n = name.get_or_insert_with(|| process_name(&pid, proc)).clone();
                    for a in asked {
                        let names = found.entry((*a).clone()).or_default();
                        if !names.contains(&n) {
                            names.push(n.clone());
                        }
                    }
                }
                let up = dirname(hit);
                if up == hit {
                    break;
                }
                hit = up;
            }
        }
    }
    found
}

// --------------------------------------------------------------------------- macOS (untested)

/// Lines of `lsof -Fpcn` → asked path → commands (`wanted`: resolved path → the path asked for).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn parse_lsof(stdout: &str, wanted: &HashMap<String, String>) -> HashMap<String, Vec<String>> {
    let mut found: HashMap<String, Vec<String>> = HashMap::new();
    let mut command = String::new();
    for line in stdout.lines() {
        let (tag, value) = line.split_at(line.len().min(1));
        match tag {
            "p" => command.clear(),
            "c" => command = value.to_string(),
            "n" if value.starts_with('/') => {
                let mut hit = value;
                loop {
                    if let Some(asked) = wanted.get(hit)
                        && !command.is_empty()
                    {
                        let names = found.entry(asked.clone()).or_default();
                        if !names.contains(&command) {
                            names.push(command.clone());
                        }
                    }
                    let up = dirname(hit);
                    if up == hit {
                        break;
                    }
                    hit = up;
                }
            }
            _ => {}
        }
    }
    found.retain(|_, v| !v.is_empty());
    found
}

#[cfg(target_os = "macos")]
fn darwin(paths: &[String]) -> HashMap<String, Vec<String>> {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let lsof = ["/usr/sbin/lsof", "/usr/bin/lsof"]
        .into_iter()
        .find(|p| Path::new(p).exists());
    let Some(lsof) = lsof else {
        return HashMap::new();
    };
    let mut cmd = Command::new(lsof);
    cmd.args(["-w", "-Fpcn", "+c", "0"]);
    for p in paths {
        let dir = std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir());
        if dir {
            cmd.args(["+D", p]);
        } else {
            cmd.args(["--", p]);
        }
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::null());
    let Ok(mut child) = cmd.spawn() else {
        return HashMap::new();
    };
    let mut stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(out) = stdout.as_mut() {
            use std::io::Read;
            let _ = out.read_to_string(&mut s);
        }
        s
    });
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < Duration::from_secs(LSOF_TIMEOUT_S) => {
                std::thread::sleep(Duration::from_millis(50))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return HashMap::new();
            }
        }
    }
    let out = reader.join().unwrap_or_default();
    let wanted: HashMap<String, String> = paths.iter().map(|p| (resolved(p), p.clone())).collect();
    parse_lsof(&out, &wanted)
}
