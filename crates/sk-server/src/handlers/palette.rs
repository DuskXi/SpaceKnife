//! palette.*: the command palette's lists, path search, filter previews and recents (W4; Python
//! `server/palette_handlers.py`). The commands' availability comes from `sk_core::palette` over a
//! [`Context`] of plain facts gathered here from the session, the scan and the view.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;
use sk_core::filter::{FilterContext, evaluate, unix_now};
use sk_core::layout::effective_alloc;
use sk_core::layout::registry;
use sk_core::palette::fuzzy::protocol_spans;
use sk_core::palette::paths::{self as ppaths, SearchOptions, Sizes};
use sk_core::palette::{Context, Recents, ShownVisual, ViewFacts, label, match_command, registry as commands, score};
use sk_core::view::{View, showable};
use sk_protocol::filter::Severity;
use sk_protocol::methods::{
    PaletteCheck, PaletteClear, PaletteCommands, PalettePathsMethod, PalettePreviewMethod, PaletteQuery,
    PaletteRemember,
};
use sk_protocol::palette::{
    DirectKind, PaletteCommand, PaletteCommandList, PaletteContext, PaletteDirect, PalettePath, PalettePaths,
    PalettePreview, PaletteQueryParams, PaletteRecent, PaletteResult, PaletteTab, RecentKind,
};
use sk_protocol::scan::{ScanSource, ScanStateName};
use sk_protocol::update::UpdateStatus;
use sk_protocol::EmptyResult;
use sk_store::{NodeId, Nodes};

use crate::app::Server;
use crate::compute::Latest;
use crate::rpc::{RpcError, RpcResult, Router};
use crate::scans::ScanHandle;
use crate::session::Session;
use crate::topics::TopicRegistry;
use crate::views::current_vocab;

pub const RECENT_SHOWN: usize = 5;
pub const COMMANDS_SHOWN: usize = 20;
const PREVIEWS_KEPT: usize = 64;

/// Filter syntax in the input (UI-W4-31): wildcards, separators, negation, comparisons, keywords,
/// quotes, or a single leading backslash (folder mask; a double one is a UNC path).
pub fn looks_like_filter(text: &str) -> bool {
    text.chars().any(|c| "*?;|<>:\"".contains(c)) || (text.starts_with('\\') && !text.starts_with("\\\\"))
}

/// The area basis per node, worked out per lookup (the nodes asked for only): effective
/// allocation, as the views size their blocks. `n` is the node count when it was taken.
struct NodeSizes<'a> {
    nodes: &'a Nodes,
    n: usize,
}

impl Sizes for NodeSizes<'_> {
    fn len(&self) -> usize {
        self.n
    }
    fn get(&self, id: NodeId) -> i64 {
        effective_alloc(self.nodes, id)
    }
}

fn target_of(h: Option<&ScanHandle>) -> Option<String> {
    h.map(|h| sk_store::snapshot::root_key(&h.root_paths()))
}

/// Saved snapshots of one scan target: the files in its auto-save folder.
fn snapshot_count(target: Option<&str>) -> u32 {
    let Some(t) = target else { return 0 };
    let Ok(it) = std::fs::read_dir(sk_core::paths::snapshots_dir().join(t)) else { return 0 };
    it.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "sks")).count() as u32
}

fn recents() -> &'static Recents {
    static R: OnceLock<Recents> = OnceLock::new();
    R.get_or_init(Recents::open_default)
}

fn view_of(server: &Server, session: &Session, view_id: Option<&str>) -> Option<Arc<Mutex<View>>> {
    let host = server.views.lookup(view_id?)?;
    (host.session_id == session.id).then(|| host.view.clone())
}

fn facts(v: &View) -> ViewFacts {
    let len = v.store().len() as u32;
    ViewFacts {
        selected: v.selected.filter(|&s| s > 0 && s < len),
        has_trash: v.has_trash(),
        compare: v.compare.is_some(),
        back: !v.back.is_empty(),
        forward: !v.forward.is_empty(),
        can_go_up: v.can_go_up(),
        color_mode: v.color_mode().as_str().to_owned(),
        visual: v.visual.clone(),
        show_free: v.show_free,
        show_unknown: v.show_unknown,
        show_trash: v.show_trash,
    }
}

/// What a palette call works on.
struct Where {
    ctx: Context,
    handle: Option<Arc<ScanHandle>>,
    view: Option<Arc<Mutex<View>>>,
}

/// What the commands' availability and status texts are computed from (palette.query and
/// palette.check alike). Blocking: reads the scan's status and counts its snapshots.
fn context_of(server: &Server, session: &Session, c: &PaletteContext, update: &UpdateStatus) -> Where {
    let scan_tab = c.tab == PaletteTab::Scan;
    let handle = if scan_tab { c.scan_id.as_deref().and_then(|id| server.scans.lookup(id)) } else { None };
    let view = if scan_tab { view_of(server, session, c.view_id.as_deref()) } else { None };
    let status = handle.as_ref().map(|h| h.status(server));
    let caps = session.capabilities().to_vec();
    let mut ctx = Context::new(c.tab);
    if let Some(s) = &status {
        ctx.scanning = matches!(s.state, ScanStateName::Queued | ScanStateName::Scanning);
        ctx.paused = s.paused;
        ctx.stopped = s.state == ScanStateName::Cancelled && s.pending_dirs > 0;
        ctx.method = s.method.as_str().to_owned();
        ctx.elapsed_s = (s.state == ScanStateName::Done).then_some(s.elapsed_s);
    }
    ctx.has_scan = handle.is_some();
    ctx.source = handle.as_ref().map_or(ScanSource::Scan, |h| h.source).as_str().to_owned();
    ctx.view = view.as_ref().map(|v| facts(&v.lock()));
    ctx.snapshots = snapshot_count(target_of(handle.as_deref()).as_deref());
    ctx.basket = c.basket;
    ctx.visuals = registry::all_visuals()
        .into_iter()
        .filter(|v| showable(&v.id, &caps).is_some())
        .map(|v| ShownVisual { title: sk_core::i18n::t(&v.title_id).to_owned(), id: v.id, compare: v.compare })
        .collect();
    ctx.caps = caps;
    ctx.app_version = update.current.clone();
    ctx.update_version = update.latest.as_ref().map(|l| l.version.clone()).unwrap_or_default();
    ctx.update_checking = update.checking;
    Where { ctx, handle, view }
}

fn command_row(cmd: &sk_core::palette::Command, ctx: &Context, hits: &[(usize, usize)]) -> PaletteCommand {
    sk_core::palette::commands::protocol_command(cmd, ctx, hits)
}

/// (negated score, registry order, command, hits)
type Ranked<'a> = (i64, usize, &'a sk_core::palette::Command, Vec<(usize, usize)>);

fn build(server: &Server, session: &Session, params: &PaletteQueryParams, update: &UpdateStatus) -> PaletteResult {
    let w = context_of(server, session, &params.context, update);
    let target = target_of(w.handle.as_deref());
    let q = params.text.trim();
    let vocab = current_vocab();
    let guard = w.handle.as_ref().map(|h| h.store.read());
    let nodes: Option<&Nodes> = guard.as_deref();
    let sizes = nodes.map(|n| NodeSizes { nodes: n, n: n.len() });
    let sizes_dyn: Option<&dyn Sizes> = sizes.as_ref().map(|s| s as &dyn Sizes);

    let mut direct = Vec::new();
    if !q.is_empty() && ppaths::looks_like_path(q) {
        let r = ppaths::resolve(nodes, sizes_dyn, q);
        direct.push(PaletteDirect {
            kind: DirectKind::Path,
            text: r.path,
            tokens: Vec::new(),
            diagnostics: Vec::new(),
            valid: true,
            state: Some(r.state),
            node: r.node,
            is_dir: r.is_dir,
            alloc: r.alloc,
            complete: r.complete,
        });
    } else if !q.is_empty() && looks_like_filter(q) {
        let parsed = vocab.parse(q, None);
        let errors: Vec<_> = parsed.diagnostics.iter().filter(|d| d.severity == Severity::Error).cloned().collect();
        direct.push(PaletteDirect {
            kind: DirectKind::Filter,
            text: q.to_owned(),
            valid: parsed.normal.as_ref().is_some_and(|n| !n.is_empty()) && errors.is_empty(),
            tokens: parsed.tokens,
            diagnostics: errors.into_iter().take(4).collect(),
            state: None,
            node: None,
            is_dir: true,
            alloc: None,
            complete: None,
        });
    }

    let mut recent: Vec<(i64, PaletteRecent)> = Vec::new();
    for e in recents().entries(target.as_deref()) {
        let got = if q.is_empty() { Some((0, Vec::new())) } else { score(q, &e.text) };
        let Some((s, hits)) = got else { continue };
        let mut r = PaletteRecent {
            kind: e.kind,
            hits: protocol_spans(&e.text, &hits),
            text: e.text.clone(),
            tokens: Vec::new(),
            exists: true,
            node: None,
            is_dir: true,
            alloc: None,
        };
        if e.kind == RecentKind::Jump {
            match (nodes, &sizes) {
                (Some(n), Some(sz)) => match n.find_path(Path::new(&e.text)) {
                    Some(node) => {
                        r.node = Some(node);
                        r.is_dir = n.is_dir(node);
                        // found after the sizes were taken (scan running): no size yet
                        r.alloc = ((node as usize) < sz.len()).then(|| sz.get(node));
                    }
                    None => r.exists = false,
                },
                _ => r.exists = false,
            }
        } else {
            r.tokens = vocab.parse(&e.text, None).tokens;
        }
        recent.push((-s, r));
    }
    recent.sort_by_key(|(s, _)| *s); // stable: newest first among equals

    let chosen: Vec<(&sk_core::palette::Command, Vec<(usize, usize)>)> = if q.is_empty() {
        let mut pinned: Vec<_> = commands().iter().filter(|c| c.pinned > 0).collect();
        pinned.sort_by_key(|c| c.pinned);
        pinned.into_iter().map(|c| (c, Vec::new())).collect()
    } else {
        let mut ranked: Vec<Ranked<'_>> = commands()
            .iter()
            .enumerate()
            .filter_map(|(i, c)| match_command(c, q).map(|(s, h)| (-s, i, c, h)))
            .collect();
        ranked.sort_by_key(|t| (t.0, t.1));
        ranked.into_iter().take(COMMANDS_SHOWN).map(|(_, _, c, h)| (c, h)).collect()
    };
    let rows = chosen.iter().map(|(c, h)| command_row(c, &w.ctx, h)).collect();

    let seed = match (&w.view, q.is_empty(), nodes) {
        (Some(v), true, Some(n)) => {
            let sel = v.lock().selected;
            sel.filter(|&s| s > 0 && (s as usize) < n.len()).map(|s| ppaths::seed_text(n, s)).filter(|s| !s.is_empty())
        }
        _ => None,
    };
    PaletteResult {
        text: params.text.clone(),
        direct,
        recent: recent.into_iter().take(RECENT_SHOWN).map(|(_, r)| r).collect(),
        commands: rows,
        paths: w.handle.is_some() && (!q.is_empty() || seed.is_some()),
        seed,
    }
}

fn find_paths(h: &ScanHandle, view: Option<&Arc<Mutex<View>>>, text: &str, limit: usize) -> PalettePaths {
    let selected = view.and_then(|v| v.lock().selected);
    let nodes = h.store.read();
    let sizes = NodeSizes { nodes: &nodes, n: nodes.len() };
    let q = text.trim();
    let (mut seed, mut folder) = (None, None);
    let (hits, total) = if q.is_empty() {
        let sel = selected.filter(|&s| s > 0 && (s as usize) < nodes.len());
        seed = sel.map(|s| ppaths::seed_text(&nodes, s)).filter(|s| !s.is_empty());
        match (&seed, sel) {
            // literally: a selected POSIX file "what?.txt" is no glob
            (Some(s), Some(sel)) => ppaths::search(
                &nodes,
                &h.ctx.names,
                &sizes,
                s,
                limit,
                SearchOptions { exclude: Some(sel), contains_only: true, literal: true },
            ),
            _ => (Vec::new(), 0),
        }
    } else if ppaths::looks_like_path(q) {
        let expanded = ppaths::expand(q);
        let (base, partial) = ppaths::split_last(&expanded);
        match (!base.is_empty()).then(|| nodes.find_path(Path::new(base))).flatten() {
            Some(node) if nodes.is_dir(node) => {
                folder = Some(nodes.path_lossy(node));
                ppaths::children(&nodes, &sizes, node, partial, limit)
            }
            _ => (Vec::new(), 0),
        }
    } else {
        ppaths::search(&nodes, &h.ctx.names, &sizes, q, limit, SearchOptions::default())
    };
    PalettePaths {
        text: text.to_owned(),
        items: hits.iter().map(|h| h.to_protocol()).collect::<Vec<PalettePath>>(),
        total: total as i64,
        partial: h.running(),
        seed,
        folder,
    }
}

type PreviewKey = (String, String, u64, usize);

fn preview(h: &ScanHandle, text: &str, cache: &Mutex<VecDeque<(PreviewKey, PalettePreview)>>) -> PalettePreview {
    let invalid = PalettePreview { text: text.to_owned(), valid: false, files: 0, alloc: 0, size: 0 };
    let parsed = h.ctx.vocab.read().clone().parse(text, None);
    let Some(normal) = parsed.normal.filter(|n| !n.is_empty()) else { return invalid };
    let nodes = h.store.read();
    let key: PreviewKey = (h.id.clone(), normal.canonical.clone(), nodes.version(), nodes.len());
    // an age condition's result moves with the clock, a tag condition's with the tags
    let cacheable = normal.ages.is_empty() && normal.tags.is_none() && normal.not_tags.is_empty();
    if cacheable {
        let mut c = cache.lock();
        if let Some(i) = c.iter().position(|(k, _)| *k == key) {
            let hit = c.remove(i).expect("found");
            let out = PalettePreview { text: text.to_owned(), ..hit.1.clone() };
            c.push_back(hit);
            return out;
        }
    }
    let tags = (normal.tags.is_some() || !normal.not_tags.is_empty()).then(|| h.ctx.tags.lock().bits(&nodes));
    let ctx = FilterContext {
        now: unix_now(),
        classes: h.ctx.basis.class_map(),
        folder_classes: h.ctx.basis.folder_classes(),
        tags,
        names: Some(h.ctx.names.clone()),
    };
    let res = evaluate(&nodes, &normal, &ctx);
    let (files, alloc, size) = res.totals(0);
    let out = PalettePreview { text: text.to_owned(), valid: true, files, alloc, size };
    if cacheable {
        let mut c = cache.lock();
        c.push_back((key, out.clone()));
        while c.len() > PREVIEWS_KEPT {
            c.pop_front();
        }
    }
    out
}

fn io_err(e: std::io::Error) -> RpcError {
    RpcError::invalid_request(crate::util::os_message(&e))
}

pub fn install(r: &mut Router, _t: &mut TopicRegistry) {
    let latest: Arc<Latest<(&'static str, String)>> = Arc::new(Latest::new());
    let previews: Arc<Mutex<VecDeque<(PreviewKey, PalettePreview)>>> = Arc::new(Mutex::new(VecDeque::new()));

    r.add::<PaletteCommands, _, _>(|_, _| async move {
        Ok(PaletteCommandList {
            commands: commands()
                .iter()
                .filter(|c| !c.typed_only)
                .map(|c| PaletteCommand {
                    id: c.id.to_owned(),
                    name: label(c),
                    hits: Vec::new(),
                    status: String::new(),
                    enabled: true,
                    keys: c.keys.iter().map(|k| (*k).to_owned()).collect(),
                    icon: c.icon.to_owned(),
                    more: c.more(),
                })
                .collect(),
        })
    });

    let l = latest.clone();
    r.add::<PaletteQuery, _, _>(move |call, p| {
        let latest = l.clone();
        async move {
            let update = call.server.updates.status();
            let (server, session) = (call.server.clone(), call.session.clone());
            latest
                .run(("palette.query", call.session.id.clone()), move || build(&server, &session, &p, &update))
                .await
        }
    });

    r.add::<PaletteCheck, _, _>(|call, p| async move {
        let update = call.server.updates.status();
        let (server, session) = (call.server.clone(), call.session.clone());
        // off the async threads: counting the target's snapshots lists a folder
        crate::compute::blocking(move || -> RpcResult<PaletteCommand> {
            let cmd = commands()
                .iter()
                .find(|c| c.id == p.id)
                .ok_or_else(|| RpcError::invalid_params(format!("unknown command: {}", p.id)))?;
            let w = context_of(&server, &session, &p.context, &update);
            Ok(command_row(cmd, &w.ctx, &[]))
        })
        .await?
    });

    let l = latest.clone();
    r.add::<PalettePathsMethod, _, _>(move |call, p| {
        let latest = l.clone();
        async move {
            let h = call.server.scans.get(&p.scan_id)?;
            let view = view_of(&call.server, &call.session, p.view_id.as_deref());
            latest
                .run(("palette.paths", call.session.id.clone()), move || {
                    find_paths(&h, view.as_ref(), &p.text, p.limit as usize)
                })
                .await
        }
    });

    let l = latest.clone();
    r.add::<PalettePreviewMethod, _, _>(move |call, p| {
        let (latest, cache) = (l.clone(), previews.clone());
        async move {
            let h = call.server.scans.get(&p.scan_id)?;
            latest.run(("palette.preview", call.session.id.clone()), move || preview(&h, &p.text, &cache)).await
        }
    });

    r.add::<PaletteRemember, _, _>(|call, p| async move {
        match p.kind {
            RecentKind::Filter => {
                let text = p.text.trim().to_owned();
                crate::compute::blocking(move || recents().add_filter(&text)).await?.map_err(io_err)?;
            }
            RecentKind::Jump => {
                let h = match &p.scan_id {
                    Some(id) => Some(call.server.scans.get(id)?),
                    None => None,
                };
                if let Some(target) = target_of(h.as_deref()) {
                    let path = ppaths::trimmed(&ppaths::expand(&p.text)).to_owned();
                    crate::compute::blocking(move || recents().add_jump(&target, &path)).await?.map_err(io_err)?;
                }
            }
        }
        Ok(EmptyResult {})
    });

    r.add::<PaletteClear, _, _>(|call, p| async move {
        let h = p.scan_id.as_deref().and_then(|id| call.server.scans.lookup(id));
        let target = target_of(h.as_deref());
        crate::compute::blocking(move || recents().clear(target.as_deref())).await?.map_err(io_err)?;
        Ok(EmptyResult {})
    });
}
