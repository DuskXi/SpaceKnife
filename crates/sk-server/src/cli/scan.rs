//! `scan` and `snapshot list` / `snapshot show` (port of `cli_scan.py`): a terminal front end for
//! the scanner, with no server in between.
//!
//! The text is the Python CLI's (summary panel, "Time N ms", the `--mft` method line, the tree and
//! largest-file tables, in both languages, sizes in base 1024 or 1000 on macOS): the external
//! benchmark (Disk-Scan-Benchmark, `run_cli_scanners.py`) parses it.

use std::collections::BinaryHeap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Args;
use sk_core::fmt;
use sk_core::i18n::{self, Arg, t};
use sk_core::scan::{IoGate, RootInfo, ScanOptions, ScanProgress, ScanState, Scanner};
use sk_core::tr;
use sk_store::snapshot;
use sk_store::{Flags, Kind, NodeId, Nodes, SortBy, Store};

use super::interrupt;
use super::rich::{ACCENT, BAR_EMPTY, Column, Console, MUTED, Style, Table, Text, WARN, fill};

#[derive(Debug, Args)]
pub struct ScanArgs {
    /// Folders or drives to scan, e.g. C:\ or ~/Downloads.
    #[arg(required = true)]
    pub paths: Vec<String>,
    /// Listing calls in flight (threads). Default: as in the app (SPACEKNIFE_SCAN_THREADS sets it there too).
    #[arg(long)]
    pub workers: Option<usize>,
    /// Rows per level in the summary.
    #[arg(long, default_value_t = 15)]
    pub top: usize,
    /// Levels to expand in the summary.
    #[arg(long, default_value_t = 2)]
    pub depth: usize,
    /// Descend into filesystems mounted below the roots.
    #[arg(long, overrides_with = "no_cross_mounts")]
    pub cross_mounts: bool,
    #[arg(long, hide = true)]
    pub no_cross_mounts: bool,
    /// Also save the snapshot to this file.
    #[arg(long)]
    pub save: Option<PathBuf>,
    /// Do not auto-save a snapshot.
    #[arg(long)]
    pub no_snapshot: bool,
    /// Also show where the scan's time went.
    #[arg(long)]
    pub profile: bool,
    /// Limit the disk load as the app does: balanced (70 %), quiet (35 %), idle, max, or a percentage. Default: no limit.
    #[arg(long)]
    pub disk_limit: Option<String>,
    /// Windows: read a whole local NTFS drive's $MFT (the app's MFT fast scan; asks for administrator
    /// rights once) instead of listing every folder. A path it cannot read this way is scanned the
    /// usual way, with a note why.
    #[arg(long = "mft")]
    pub mft: bool,
}

/// How the tree was read: `--mft` asked for the MFT fast scan; `mft` it was read that way;
/// `fallback` why it was scanned the hard way instead; the MFT phases' seconds.
#[derive(Debug, Default)]
pub struct ScanMethod {
    pub asked: bool,
    pub mft: bool,
    pub fallback: String,
    pub phases: Vec<(&'static str, f64)>,
    /// the scan was paced by `--disk-limit`
    pub gated: bool,
}

/// `--disk-limit`: a level of the app's 「磁盘负载上限」 or a percentage (custom).
pub fn load_limit(text: &str) -> Result<(sk_core::io::Level, Option<f64>), String> {
    use sk_core::io::Level;
    let lower = text.trim().to_lowercase();
    let value = lower.strip_suffix('%').unwrap_or(&lower);
    match value {
        "max" => return Ok((Level::Max, None)),
        "balanced" => return Ok((Level::Balanced, None)),
        "quiet" => return Ok((Level::Quiet, None)),
        "idle" => return Ok((Level::Idle, None)),
        _ => {}
    }
    if !value.is_empty()
        && value.bytes().all(|b| b.is_ascii_digit())
        && let Ok(n) = value.parse::<u32>()
        && (1..=100).contains(&n)
    {
        return Ok((Level::Custom, Some(n as f64)));
    }
    Err(t("磁盘负载上限只能是 max、balanced、quiet、idle 或 1–100 的百分比").to_owned())
}

/// The `$MFT` reader's busy share for `--disk-limit`, as the app maps its load levels.
pub fn mft_duty(limit: Option<(sk_core::io::Level, Option<f64>)>) -> f64 {
    use sk_core::io::Level;
    match limit {
        None | Some((Level::Max, _)) => 1.0,
        Some((Level::Idle, _)) => 0.2,
        Some((level, custom)) => sk_core::io::level_limit(level, custom).clamp(0.05, 1.0),
    }
}

/// Why an MFT fast scan cannot read these roots, or None when it can: one root that is a whole
/// local NTFS volume on Windows (`scan/mft.py`'s `mft_unsupported`).
pub fn mft_unsupported(roots: &[RootInfo]) -> Option<String> {
    if !cfg!(windows) {
        return Some(t("MFT 快速扫描只在 Windows 上可用").to_owned());
    }
    if roots.len() != 1 {
        return Some(tr!("一次只能读一个卷，这次有 {count} 个扫描位置", count = roots.len()));
    }
    let root = &roots[0];
    let path = root.path.display().to_string();
    let Some(vol) = &root.volume else {
        return Some(tr!("认不出 {path} 所在的卷", path = path));
    };
    if vol.is_remote {
        return Some(tr!("{path} 在网络位置上", path = path));
    }
    if !vol.fs_type.eq_ignore_ascii_case("NTFS") {
        let fs = if vol.fs_type.is_empty() { t("未知文件系统").to_owned() } else { vol.fs_type.clone() };
        return Some(tr!("{mount} 是 {fs} 卷，只有 NTFS 卷有 $MFT", mount = &vol.mount_point, fs = fs));
    }
    if !root.whole_volume {
        return Some(tr!("只能扫整个卷（例如 {mount}），{path} 只是其中的一个文件夹", mount = &vol.mount_point, path = path));
    }
    None
}

// ------------------------------------------------------------------------------------- scan

/// `spaceknife-cli scan`: returns the exit code.
pub fn scan(args: ScanArgs) -> i32 {
    super::init_warnings();
    super::apply_language();
    let mut out = Console::stdout();
    interrupt::install(line_text(&Text::styled(&tr!("已中断"), Style::YELLOW), &out));
    let code = scan_with(&args, &mut out);
    out.flush();
    code
}

/// [`scan`] printing to `out` (the language already set).
pub fn scan_with(args: &ScanArgs, out: &mut Console) -> i32 {
    let limit = match args.disk_limit.as_deref().map(load_limit).transpose() {
        Ok(l) => l,
        Err(msg) => {
            eprintln!("Error: Invalid value for '--disk-limit': {msg}");
            return 2;
        }
    };
    let (scanner, result, method) = match run_scan(args, limit, out) {
        Ok(r) => r,
        Err(reason) => {
            out.text(&Text::styled(&tr!("无法扫描：{reason}", reason = reason), Style::RED));
            return 2;
        }
    };
    if interrupt::interrupted() {
        out.text(&Text::styled(&tr!("已中断"), Style::YELLOW));
        return 130;
    }
    let roots = scanner.roots();
    let mut snap = None;
    if result.state == ScanState::Done && !args.no_snapshot {
        let keys: Vec<String> = roots.iter().map(|r| r.path.to_string_lossy().into_owned()).collect();
        let path = snapshot::auto_path(&keys, None, &sk_core::paths::snapshots_dir());
        if save(&scanner, &path, &method) {
            if let Some(folder) = path.parent() {
                let _ = snapshot::prune(folder, snapshot::KEEP_RECENT);
            }
            snap = Some(path);
        }
    }
    if let Some(path) = &args.save {
        save(&scanner, path, &method);
    }
    out.panel(&summary(&scanner, &roots, &result, snap.as_deref(), &method), &tr!("扫描结果"));
    if args.profile {
        let (lines, title) = if method.mft { mft_profile(&method) } else { profile(&scanner, method.gated) };
        out.panel(&lines, &title);
    }
    let r = scanner.store().read();
    for root in &roots {
        let title = tr!("{path} 里最大的项目", path = root.path.display().to_string());
        out.table(&top_table(&r, root.node, args.top, args.depth, &title));
        out.blank();
    }
    if let Some(table) = largest_files(&r, args.top) {
        out.table(&table);
    }
    0
}

fn line_text(text: &Text, out: &Console) -> String {
    let mut c = Console::capture(out.width);
    c.text(text);
    let s = c.take();
    if cfg!(windows) { s.replace('\n', "\r\n") } else { s }
}

type Limit = Option<(sk_core::io::Level, Option<f64>)>;

fn run_scan(args: &ScanArgs, limit: Limit, out: &mut Console) -> Result<(Arc<Scanner>, ScanProgress, ScanMethod), String> {
    let concurrency = match args.workers {
        Some(0) => 1, // Python's in-process walker: one listing at a time
        Some(n) => n,
        None => 0,
    };
    let mut opts = ScanOptions { concurrency, cross_mounts: args.cross_mounts && !args.no_cross_mounts, ..Default::default() };
    // the disk-load limit: paced like the app's scans
    let mut registry = None;
    let mut low_io = false;
    if let Some((level, custom)) = limit {
        let most = sk_core::scan::listing_max(); // the governor counts folders listed at once
        let reg = sk_core::io::IoRegistry::new(sk_core::io::RegistryOptions { max_workers: most, ..Default::default() });
        reg.start();
        let first = sk_core::scan::normalise_root(&args.paths[0]);
        let gov = reg.governor_for_path(&first);
        gov.set_max_workers(most);
        gov.set_level(level, custom);
        low_io = gov.low_io_priority() && sk_core::io::set_low_io_priority(None, true, false);
        opts.gate = Some(Arc::new(gov) as Arc<dyn IoGate>);
        registry = Some(reg);
    }
    let gated = opts.gate.is_some();
    let result = (|| {
        let scanner = Arc::new(Scanner::new(&args.paths, opts).map_err(|e| e.to_string())?);
        let mut method = ScanMethod { asked: args.mft, gated, ..Default::default() };
        if args.mft {
            try_mft(&scanner, &mut method, limit, out);
            if method.mft {
                let p = scanner.progress();
                return Ok((scanner, p, method));
            }
        }
        let result = hard_scan(&scanner, out);
        Ok((scanner, result, method))
    })();
    if low_io {
        sk_core::io::set_low_io_priority(None, false, false);
    }
    if let Some(reg) = registry {
        reg.stop();
    }
    result
}

/// The hard scan, with a live progress line on a terminal (none when the output is redirected:
/// scripts, benchmarks).
fn hard_scan(scanner: &Arc<Scanner>, out: &mut Console) -> ScanProgress {
    interrupt::cooperative(true);
    let live = out.interactive();
    let started = Instant::now();
    let mut tick = 0usize;
    let mut on_progress = |p: &ScanProgress| {
        if interrupt::interrupted() {
            scanner.cancel();
        }
        if live {
            tick += 1;
            progress_line(out, tick, started, p);
        }
    };
    let result = scanner.run(Some(&mut on_progress), Duration::from_millis(100));
    interrupt::cooperative(false);
    if live {
        clear_progress(out);
    } else {
        out.blank(); // the empty line rich's Progress.stop() prints there
    }
    result
}

const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

fn bar_text(fraction: Option<f64>, width: usize, tick: usize) -> Text {
    let mut t = Text::new();
    match fraction {
        Some(f) => {
            let filled = ((f * width as f64).round() as usize).min(width);
            t.push(&"━".repeat(filled), ACCENT);
            t.push(&"━".repeat(width - filled), BAR_EMPTY);
        }
        None => {
            // pulsing bar: a lit stretch moving along
            for i in 0..width {
                let lit = (i + width - tick % width) % width < width / 3;
                t.push("━", if lit { ACCENT } else { BAR_EMPTY });
            }
        }
    }
    t
}

fn elapsed_clock(d: Duration) -> String {
    let s = d.as_secs();
    format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

fn progress_line(out: &mut Console, tick: usize, started: Instant, p: &ScanProgress) {
    use std::io::Write as _;
    let path = if p.current_path.chars().count() < 60 {
        p.current_path.clone()
    } else {
        let tail: String = p.current_path.chars().rev().take(58).collect::<Vec<_>>().into_iter().rev().collect();
        format!("…{tail}")
    };
    let mut t = Text::styled(&SPINNER[tick % SPINNER.len()].to_string(), ACCENT);
    t.push(" ", Style::NONE).push(&tr!("扫描中"), Style::BOLD).push(" ", Style::NONE);
    t.append(bar_text(p.progress, 28, tick));
    let pct = p.progress.map(|f| format!("{:4.1}%", f * 100.0)).unwrap_or_default();
    t.push(&format!(" {pct} "), Style::NONE);
    let rate = i18n::format("{r:,.0f}", &[("r", Arg::Float(p.rate))]);
    t.push(
        &tr!(
            "{task.fields[items]} 项 · {task.fields[size]} · 每秒 {task.fields[rate]} 项",
        )
        .replace("{task.fields[items]}", &fmt::count(p.items))
        .replace("{task.fields[size]}", &fmt::size(p.bytes_alloc as f64))
        .replace("{task.fields[rate]}", &rate),
        Style::NONE,
    );
    t.push(&format!(" {} ", elapsed_clock(started.elapsed())), Style::rgb(0xF5C542));
    t.push(&path, MUTED);
    let mut c = Console::capture(out.width.saturating_sub(1));
    c.color = out.color;
    c.lines(vec![t.0.clone()]);
    let line = c.take();
    let mut o = std::io::stdout().lock();
    let _ = write!(o, "\r{}\x1b[K", line.trim_end_matches('\n'));
    let _ = o.flush();
}

fn clear_progress(out: &mut Console) {
    use std::io::Write as _;
    out.flush();
    let mut o = std::io::stdout().lock();
    let _ = write!(o, "\r\x1b[K");
    let _ = o.flush();
}

/// `--mft`: the MFT fast scan where the roots allow it, with its own progress line; a note why
/// not (method.fallback), and then the caller scans the hard way.
fn try_mft(scanner: &Arc<Scanner>, method: &mut ScanMethod, limit: Limit, out: &mut Console) {
    let roots = scanner.roots();
    if let Some(reason) = mft_unsupported(&roots) {
        out.text(&Text::styled(&tr!("不支持 MFT 快速扫描：{reason}。改用硬扫描。", reason = &reason), WARN));
        method.fallback = reason;
        return;
    }
    let elevated = sk_core::helper::is_elevated();
    if !elevated {
        out.text(&Text::styled(t("读取 $MFT 需要管理员权限：Windows 会请求一次授权。"), MUTED));
    }
    out.flush();
    let live = out.interactive();
    let started = Instant::now();
    let width = out.width;
    let color = out.color;
    let mut show = move |step: &str, fraction: Option<f64>| {
        if !live {
            return;
        }
        use std::io::Write as _;
        let mut line = Text::styled("⠿ ", ACCENT);
        line.push(t("MFT 快速扫描"), Style::BOLD).push(" ", Style::NONE);
        line.append(bar_text(fraction, 28, started.elapsed().as_millis() as usize / 100));
        let pct = fraction.map(|f| format!("{:4.1}%", f * 100.0)).unwrap_or_default();
        line.push(&format!(" {pct} {} ", elapsed_clock(started.elapsed())), Style::NONE);
        line.push(step, MUTED);
        let mut c = Console::capture(width.saturating_sub(1));
        c.color = color;
        c.lines(vec![line.0]);
        let line = c.take();
        let mut o = std::io::stdout().lock();
        let _ = write!(o, "\r{}\x1b[K", line.trim_end_matches('\n'));
        let _ = o.flush();
    };
    let outcome = read_mft(scanner, &roots[0], method, mft_duty(limit), elevated, &mut show);
    if live {
        clear_progress(out);
    } else {
        out.blank(); // rich's Progress.stop() on a redirected console
    }
    match outcome {
        Ok(()) => method.mft = true,
        Err(reason) => {
            out.text(&Text::styled(&tr!("MFT 快速扫描不可用：{reason}；改用硬扫描", reason = &reason), WARN));
            method.fallback = reason;
        }
    }
}

/// Reads the root's volume and adopts the tree built from its `$MFT` (what the app's MFT scan
/// does). Already elevated: read here, no helper process; else through the read-only helper
/// (one UAC prompt). Err: why it failed, and the scanner is ready for a hard scan.
fn read_mft(
    scanner: &Arc<Scanner>,
    root: &RootInfo,
    method: &mut ScanMethod,
    duty: f64,
    elevated: bool,
    show: &mut dyn FnMut(&str, Option<f64>),
) -> Result<(), String> {
    let root_path = root.path.to_string_lossy().into_owned();
    let volume: String = root_path.chars().take(2).collect();
    let started = Instant::now();
    scanner.begin_external();
    let mut helper = None;
    let outcome = (|| -> Result<(), String> {
        let reading;
        let table;
        if elevated {
            reading = Instant::now();
            show(t("读取 $MFT"), None);
            let mut progress = |done: u64, total: u64| {
                show(t("读取 $MFT"), (total > 0).then(|| done as f64 / total as f64));
            };
            let (tbl, _info) = sk_core::scan::mft::scan_volume(&volume, duty, Some(&mut progress), None)
                .map_err(|e| e.to_string())?;
            table = tbl;
            method.phases.push(("read", reading.elapsed().as_secs_f64()));
        } else {
            show(t("启动读取 $MFT 的只读助手"), None);
            let shared = sk_core::helper::SharedHelper::new(sk_core::helper::LaunchOptions::default());
            let client = shared.get(true).map_err(|e| e.message.clone())?;
            helper = Some(shared);
            reading = Instant::now();
            method.phases.push(("helper", (reading - started).as_secs_f64()));
            let mut sending: Option<Instant> = None;
            let mut progress = |phase: sk_core::helper::Phase, _elapsed: f64, read: Option<f64>| match phase {
                sk_core::helper::Phase::Send => {
                    sending.get_or_insert_with(Instant::now);
                    show(t("接收读到的表"), None);
                }
                sk_core::helper::Phase::Scan => show(t("读取 $MFT"), read),
            };
            let opts = sk_core::helper::MftOptions { duty, ..Default::default() };
            table = client.scan_mft(&volume, opts, Some(&mut progress)).map_err(|e| e.message.clone())?;
            let received = Instant::now();
            method.phases.push(("read", (sending.unwrap_or(received) - reading).as_secs_f64()));
            if let Some(s) = sending {
                method.phases.push(("receive", (received - s).as_secs_f64()));
            }
        }
        let building = Instant::now();
        show(t("建树"), None);
        let store = sk_core::scan::mft::table_to_store(table, &root_path, &Default::default()).map_err(|e| e.to_string())?;
        scanner.store().write().adopt(&store);
        drop(store);
        method.phases.push(("build", building.elapsed().as_secs_f64()));
        scanner.finish_external(ScanState::Done);
        Ok(())
    })();
    if let Some(h) = helper {
        h.close();
    }
    if outcome.is_err() {
        scanner.reset_external();
    }
    outcome
}

// ------------------------------------------------------------------------------------- output

fn pct(share: f64) -> String {
    format!("{:.1}%", share * 100.0)
}

fn percent0(v: f64) -> String {
    format!("{:.0}%", v * 100.0)
}

/// `_bar`: a share as 12 cells of █ and ░.
fn bar(fraction: f64) -> Text {
    let width = 12usize;
    let filled = ((fraction * width as f64).round_ties_even().max(0.0) as usize).min(width);
    Text::styled(&"█".repeat(filled), ACCENT).with(&"░".repeat(width - filled), BAR_EMPTY)
}

fn tx(s: String) -> Text {
    Text::plain(&s)
}

/// The summary panel's lines (`summary_panel`).
pub fn summary(scanner: &Scanner, roots: &[RootInfo], result: &ScanProgress, snap: Option<&Path>, method: &ScanMethod) -> Text {
    let state = match result.state {
        ScanState::Done => Text::styled(t("完成"), Style::GREEN),
        ScanState::Cancelled => Text::styled(t("已停止（结果不完整）"), Style::YELLOW),
        ScanState::Failed => Text::styled(t("失败"), Style::RED),
        other => Text::plain(other.as_str()),
    };
    let rate = i18n::format("{r:,.0f}", &[("r", Arg::Float(result.rate))]);
    let mut lines: Vec<Text> = vec![
        fill(
            t("状态 {state}    用时 {time}    速度 每秒 {rate} 项"),
            Style::NONE,
            &[("state", state), ("time", tx(fmt::duration(result.elapsed_s))), ("rate", tx(rate))],
        ),
        fill(
            t("项目 {items}（文件 {files} · 文件夹 {folders}）"),
            Style::NONE,
            &[
                ("items", tx(fmt::count(result.items))),
                ("files", tx(fmt::count(result.files))),
                ("folders", tx(fmt::count(result.dirs))),
            ],
        ),
        fill(
            t("占用 {size}    逻辑大小 {logical}"),
            Style::NONE,
            &[
                ("size", Text::styled(&fmt::size(result.bytes_alloc as f64), Style::BOLD)),
                ("logical", tx(fmt::size(result.bytes_size as f64))),
            ],
        ),
    ];
    if method.asked {
        if method.mft {
            lines.push(Text::plain(t("方式 MFT 快速扫描（读取卷的 $MFT）")));
        } else {
            lines.push(fill(t("方式 硬扫描（{reason}）"), Style::NONE, &[("reason", Text::styled(&method.fallback, WARN))]));
        }
    }
    for root in roots {
        let Some(vol) = &root.volume else { continue };
        let fs = if vol.fs_type.is_empty() { t("未知").to_owned() } else { vol.fs_type.clone() };
        let mut line = fill(
            t("卷 {mount} · {fs} · 共 {total}"),
            Style::NONE,
            &[("mount", tx(vol.mount_point.clone())), ("fs", tx(fs)), ("total", tx(fmt::size(vol.total as f64)))],
        );
        if let Some((free, unknown)) = scanner.space_summary(root) {
            line.push(" · ", Style::NONE);
            line.append(fill(
                t("可用 {free} · 未知空间 {unknown}"),
                Style::NONE,
                &[("free", Text::styled(&fmt::size(free as f64), Style::GREEN)), ("unknown", tx(fmt::size(unknown as f64)))],
            ));
        }
        lines.push(line);
    }
    if result.errors > 0 {
        let denied = i18n::plural(
            result.errors,
            "{count} 个文件夹无法访问",
            "1 folder couldn't be accessed",
            &[("count", Arg::from(result.errors))],
        );
        let example = result.denied.first().cloned().unwrap_or_default();
        lines.push(fill(
            t("{denied}（已计入未知空间），例如 {example}"),
            Style::NONE,
            &[("denied", Text::styled(&denied, WARN)), ("example", tx(example))],
        ));
    }
    if let Some(p) = snap {
        lines.push(Text::styled(&tr!("快照已保存：{path}", path = p), MUTED));
    }
    let ntfs = roots.iter().any(|r| r.volume.as_ref().is_some_and(|v| v.fs_type.to_uppercase() == "NTFS"));
    if !method.mft && ntfs {
        // hard scan on Windows: explain the accounting limits honestly
        lines.push(Text::styled(
            t("说明：这是硬扫描。NTFS 的硬链接按每个链接计入，占用按簇推算；扫整个本地 NTFS 卷时加 --mft 改用 MFT 快速扫描，会给出精确值（需要管理员权限）。"),
            MUTED,
        ));
    }
    join_lines(lines)
}

fn join_lines(lines: Vec<Text>) -> Text {
    let mut out = Text::new();
    for (i, l) in lines.into_iter().enumerate() {
        if i > 0 {
            out.push("\n", Style::NONE);
        }
        out.append(l);
    }
    out
}

/// Where the scan's time went (`profile_panel`).
fn profile(scanner: &Scanner, gated: bool) -> (Text, String) {
    let p = scanner.profile();
    let walker = t("原生遍历器");
    let bold = |s: String| Text::styled(&s, Style::BOLD);
    let mut lines = vec![
        Text::plain(&tr!("{walker} · 列目录并发 {threads}", walker = walker, threads = p.threads)),
        fill(
            t("列目录线程平均忙碌 {busy}（系统调用；接近 100% 说明瓶颈在磁盘或文件系统）"),
            Style::NONE,
            &[("busy", bold(percent0(p.busy())))],
        ),
        fill(t("主线程合并结果 {ingest}（接近 100% 说明瓶颈在合并，加线程无用）"), Style::NONE, &[("ingest", bold(percent0(p.ingest())))]),
        Text::plain(&tr!("共 {slices} 批", slices = fmt::count(p.slices as i64))),
    ];
    if gated {
        lines.push(fill(t("等磁盘负载上限放行 {limited}（没有任何目录在列）"), Style::NONE, &[("limited", bold(percent0(p.limited())))]));
    }
    if cfg!(target_os = "macos") {
        lines.push(Text::plain(&tr!("批量列目录（getattrlistbulk）：{state}", state = sk_native::hardscan::bulk_listing())));
    }
    (join_lines(lines), tr!("耗时分解"))
}

/// Where an MFT fast scan's time went (`--mft --profile`).
fn mft_profile(method: &ScanMethod) -> (Text, String) {
    let mut lines = vec![Text::plain(t("MFT 快速扫描"))];
    for &(key, seconds) in &method.phases {
        let name = match key {
            "helper" => t("启动只读助手（含管理员授权）"),
            "read" => t("读取并解析 $MFT"),
            "receive" => t("接收读到的表"),
            "build" => t("建树"),
            _ => continue,
        };
        lines.push(Text::plain(&format!("{name}  ")).with(&fmt::duration(seconds), Style::BOLD));
    }
    (join_lines(lines), tr!("耗时分解"))
}

fn kind_icon(kind: Kind) -> &'static str {
    match kind {
        Kind::Dir => "▸",
        Kind::File => " ",
        Kind::Link => "↪",
        _ => "·",
    }
}

/// The largest items below a node, `depth` levels, `limit` rows each (`top_table`).
pub fn top_table(r: &Nodes, node: NodeId, limit: usize, depth: usize, title: &str) -> Table {
    let (_, total) = r.effective_size(node);
    let mut table = Table::new(Some(title.to_owned()), false);
    table
        .column(Column::new(t("名称")).no_wrap().max(44))
        .column(Column::new(t("占用")).right().style(Style::BOLD).min(8).no_wrap())
        .column(Column::new(t("占比")).right().min(6).no_wrap())
        .column(Column::new("").no_wrap().min(12).max(18))
        .column(Column::new(t("文件数")).right().style(MUTED).no_wrap());
    let share_of = |alloc: i64| if total != 0 { alloc as f64 / total as f64 } else { 0.0 };
    fn add(r: &Nodes, table: &mut Table, parent: NodeId, level: usize, limit: usize, depth: usize, share_of: &dyn Fn(i64) -> f64) {
        let children = r.sorted_children(parent, SortBy::Size, true);
        let shown = &children[..limit.min(children.len())];
        let indent = "  ".repeat(level);
        for &child in shown {
            let kind = r.kind(child);
            let flags = r.flags(child);
            let (_, alloc) = r.effective_size(child);
            let share = share_of(alloc);
            let denied = flags.intersects(Flags::ACCESS_DENIED);
            let mut label = Text::plain(&format!("{indent}{} ", kind_icon(kind)));
            let name_style = if denied {
                WARN
            } else if kind == Kind::Dir {
                Style::BOLD
            } else {
                Style::NONE
            };
            label.push(&r.name(child), name_style);
            if denied {
                label.push(&format!("  {}", t("无法访问")), WARN);
            }
            let files = if kind == Kind::Dir { fmt::count(r.tot_files(child) as i64) } else { String::new() };
            table.row(vec![label, tx(fmt::size(alloc as f64)), tx(pct(share)), bar(share), tx(files)]);
            if level + 1 < depth && kind == Kind::Dir && r.child_count(child) > 0 {
                add(r, table, child, level + 1, limit, depth, share_of);
            }
        }
        let rest = &children[shown.len()..];
        if !rest.is_empty() {
            let rest_alloc: i64 = rest.iter().map(|&i| r.effective_size(i).1).sum();
            let share = share_of(rest_alloc);
            let label = format!("{indent}  {}", tr!("…其余 {count} 项", count = fmt::count(rest.len() as i64)));
            table.row(vec![Text::styled(&label, MUTED), tx(fmt::size(rest_alloc as f64)), tx(pct(share)), bar(share), Text::new()]);
        }
    }
    add(r, &mut table, node, 0, limit, depth, &share_of);
    table
}

/// The `limit` largest files of the whole store (by allocation; ties: the earlier node first).
pub fn largest(r: &Nodes, limit: usize) -> Vec<NodeId> {
    if limit == 0 {
        return Vec::new();
    }
    let kinds = r.kinds_raw();
    let allocs = r.allocs();
    let file = Kind::File as u8;
    // a min-heap of the best `limit` (key: alloc, then the lower id)
    let mut heap: BinaryHeap<std::cmp::Reverse<(i64, std::cmp::Reverse<NodeId>)>> = BinaryHeap::with_capacity(limit + 1);
    for (i, (&k, &a)) in kinds.iter().zip(allocs).enumerate() {
        if k != file {
            continue;
        }
        let key = (a, std::cmp::Reverse(i as NodeId));
        if heap.len() < limit {
            heap.push(std::cmp::Reverse(key));
        } else if let Some(min) = heap.peek()
            && key > min.0
        {
            heap.pop();
            heap.push(std::cmp::Reverse(key));
        }
    }
    let mut keys: Vec<(i64, std::cmp::Reverse<NodeId>)> = heap.into_iter().map(|k| k.0).collect();
    keys.sort_by(|a, b| b.cmp(a));
    keys.into_iter().map(|(_, std::cmp::Reverse(id))| id).collect()
}

/// The largest files table (`_largest_files`); None when there are no files.
pub fn largest_files(r: &Nodes, limit: usize) -> Option<Table> {
    let ids = largest(r, limit);
    if ids.is_empty() {
        // Python: no table without files; with files and --top 0 an empty table (header only)
        if limit > 0 || !r.kinds_raw().contains(&(Kind::File as u8)) {
            return None;
        }
    }
    let mut table = Table::new(Some(tr!("最大的文件")), true);
    table
        .column(Column::new(t("占用")).right().style(Style::BOLD))
        .column(Column::new(t("修改于")).right().style(MUTED))
        .column(Column::new(t("路径")).fold());
    for id in ids {
        table.row(vec![tx(fmt::size(r.alloc(id) as f64)), tx(fmt::age(r.mtime(id) as i64, None)), tx(r.path_lossy(id))]);
    }
    Some(table)
}

fn iso(dt: Option<sk_protocol::DateTime>) -> serde_json::Value {
    dt.map_or(serde_json::Value::Null, |d| d.to_rfc3339_opts(chrono::SecondsFormat::Micros, false).into())
}

/// Saves the scan's tree as a snapshot; false (and a note on stderr) when it cannot.
fn save(scanner: &Scanner, path: &Path, method: &ScanMethod) -> bool {
    let pr = scanner.progress();
    let roots = scanner.roots();
    let mut stats = serde_json::Map::new();
    let mut put = |k: &str, v: serde_json::Value| {
        stats.insert(k.to_owned(), v);
    };
    put("items", pr.items.into());
    put("files", pr.files.into());
    put("dirs", pr.dirs.into());
    put("bytes_alloc", pr.bytes_alloc.into());
    put("bytes_size", pr.bytes_size.into());
    put("errors", pr.errors.into());
    put("elapsed_s", pr.elapsed_s.into());
    put("started_at", iso(pr.started_at));
    put("finished_at", iso(pr.finished_at));
    put("denied", pr.denied.clone().into());
    put("state", pr.state.as_str().into());
    put(
        "roots",
        roots
            .iter()
            .map(|r| serde_json::json!({"path": r.path.to_string_lossy(), "whole_volume": r.whole_volume}))
            .collect::<Vec<_>>()
            .into(),
    );
    put("method", if method.mft { "mft" } else { "hard" }.into());
    let opts = snapshot::SaveOptions {
        roots: roots.iter().map(|r| r.path.to_string_lossy().into_owned()).collect(),
        volumes: roots
            .iter()
            .map(|r| r.volume.as_ref().and_then(|v| serde_json::to_value(v.to_protocol()).ok()).unwrap_or_else(|| serde_json::json!({})))
            .collect(),
        stats,
        ..Default::default()
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let r = scanner.store().read();
    match snapshot::save(&r, path, &opts) {
        Ok(_) => true,
        Err(e) => {
            eprintln!("{}", tr!("无法保存快照 {path}：{reason}", path = path, reason = e.to_string()));
            false
        }
    }
}

// ------------------------------------------------------------------------------------- snapshots

/// `snapshot list`: auto-saved snapshots, newest first.
pub fn snapshot_list() -> i32 {
    super::apply_language();
    let mut out = Console::stdout();
    let folder = sk_core::paths::snapshots_dir();
    let found = snapshot::list_snapshots(&folder);
    if found.is_empty() {
        out.text(&Text::plain(t("还没有快照。先运行 spaceknife scan <路径>。")));
        out.flush();
        return 0;
    }
    let mut table = Table::new(None, true);
    table
        .column(Column::new(t("时间")).no_wrap())
        .column(Column::new(t("扫描对象")).fold())
        .column(Column::new(t("占用")).right().no_wrap())
        .column(Column::new(t("项目")).right().no_wrap())
        .column(Column::new(t("快照文件")).style(MUTED).no_wrap());
    for (path, header) in &found {
        let alloc = header.stats.get("bytes_alloc").and_then(|v| v.as_i64()).unwrap_or(0);
        table.row(vec![
            tx(local_minutes(header.created_at)),
            tx(header.roots.join("; ")),
            tx(fmt::size(alloc as f64)),
            tx(fmt::count(header.node_count as i64 - 1)),
            tx(path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()),
        ]);
    }
    out.table(&table);
    let total = i18n::plural(
        found.len() as i64,
        "共 {count} 个快照，位于 {folder}",
        "1 snapshot in {folder}",
        &[("count", Arg::from(found.len())), ("folder", Arg::from(&folder))],
    );
    out.text(&Text::styled(&total, MUTED));
    out.flush();
    0
}

fn local_minutes(secs: u64) -> String {
    chrono::DateTime::from_timestamp(secs as i64, 0)
        .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

/// `snapshot show`: where the space went in a saved snapshot (no rescan).
pub fn snapshot_show(file: &Path, top: usize, depth: usize) -> i32 {
    super::apply_language();
    let mut out = Console::stdout();
    let (store, header): (Store, _) = match snapshot::load(file) {
        Ok(s) => s,
        Err(e) => {
            out.text(&Text::styled(&tr!("无法打开快照：{reason}", reason = e.to_string()), Style::RED));
            out.flush();
            return 2;
        }
    };
    let r = store.read();
    let items = header.node_count as i64 - 1;
    let line = i18n::plural(
        items,
        "保存于 {time} · {items} 项 · {size}",
        "Saved {time} · 1 item · {size}",
        &[
            ("time", Arg::from(local_minutes(header.created_at))),
            ("items", Arg::from(fmt::count(items))),
            ("size", Arg::from(fmt::size(r.tot_alloc(0) as f64))),
        ],
    );
    let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    out.panel(&Text::plain(&line), &tr!("快照 {name}", name = name));
    for root in r.roots() {
        let title = tr!("{path} 里最大的项目", path = r.name(root).into_owned());
        out.table(&top_table(&r, root, top, depth, &title));
    }
    drop(r);
    out.flush();
    0
}
