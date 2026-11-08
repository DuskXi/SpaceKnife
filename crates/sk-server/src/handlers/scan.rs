//! volume.*, scan.*, tree.*, snapshot.*, io.* and the `scan` / `io` topics (Python
//! `server/scan_handlers.py`).

use std::path::Path;
use std::sync::Arc;

use sk_core::scan::normalise_root;
use sk_core::tr;
use sk_core::volume::{list_volumes, volume_for};
use sk_protocol::EmptyResult;
use sk_protocol::io::{IoStatusEvent, IoStatusList, IoTopicParams, TOPIC_IO};
use sk_protocol::methods::*;
use sk_protocol::scan::{
    PathCheck, PathCheckResult, PathCheckState, ScanListResult, ScanProgressEvent, ScanStateName,
    ScanTopicParams, SnapshotListResult, TOPIC_SCAN, TopFile, TreeChildrenResult, TreeFindResult,
    TreeNodeResult, TreeSummaryResult, VolumeListResult,
};
use sk_store::NodeId;
use sk_store::SortBy;

use crate::app::Server;
use crate::compute::Latest;
use crate::rpc::{Router, RpcError, RpcResult};
use crate::scans::{ScanHandle, node_info, on_disk, snapshot_info};
use crate::topics::{Subscription, TopicRegistry};

/// Validate typed scan targets (UI-W1-25): existence, folder, access; merge nested ones.
pub fn check_paths(paths: &[String]) -> PathCheckResult {
    let mut items: Vec<PathCheck> = Vec::new();
    for raw in paths {
        let text = raw.trim();
        if text.is_empty() {
            continue;
        }
        let norm = normalise_root(text);
        let normalized = norm.to_string_lossy().into_owned();
        let (mut state, mut message, mut volume) =
            (PathCheckState::Ok, String::new(), String::new());
        match std::fs::metadata(&norm) {
            Ok(m) if m.is_dir() => match std::fs::read_dir(&norm) {
                Ok(mut it) => {
                    let _ = it.next();
                    volume = volume_for(&norm).map(|v| v.mount_point).unwrap_or_default();
                }
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                    state = PathCheckState::Denied;
                    message = tr!("无法访问：拒绝访问");
                }
                Err(e) => {
                    state = PathCheckState::Error;
                    message = tr!("无法访问：{reason}", reason = crate::util::os_message(&e));
                }
            },
            Ok(_) => {
                state = PathCheckState::NotDir;
                message = tr!("不是文件夹");
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                state = PathCheckState::Missing;
                message = tr!("路径不存在");
            }
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                state = PathCheckState::Denied;
                message = tr!("无法访问：拒绝访问");
            }
            Err(e) => {
                state = PathCheckState::Error;
                message = tr!("无法访问：{reason}", reason = crate::util::os_message(&e));
            }
        }
        items.push(PathCheck {
            path: text.to_owned(),
            normalized,
            state,
            message,
            volume,
            merged_into: None,
        });
    }
    let key =
        |p: &str| String::from_utf8_lossy(&sk_store::names::normcase(p.as_bytes())).into_owned();
    let mut ok: Vec<usize> = (0..items.len())
        .filter(|&i| items[i].state == PathCheckState::Ok)
        .collect();
    ok.sort_by_key(|&i| items[i].normalized.len());
    let mut kept: Vec<String> = Vec::new();
    for i in ok {
        let k = key(&items[i].normalized);
        let parent = kept.iter().find(|p| {
            let pk = key(p);
            let base = pk.trim_end_matches(['/', '\\']);
            k == pk
                || (k.starts_with(base) && k[base.len()..].starts_with(std::path::MAIN_SEPARATOR))
        });
        match parent {
            None => kept.push(items[i].normalized.clone()),
            Some(p) => items[i].merged_into = Some(p.clone()),
        }
    }
    PathCheckResult { items, roots: kept }
}

/// `tree.summary`: parent, largest files, class totals (with what folder classes took over),
/// newest change, tag, volume, and why the node takes a folder class.
///
/// Only the node itself (its row, path and whether it is on disk) must be readable; every other
/// part that fails is left empty and named in `gaps` (`sk_core::summary`). `Err` (the message of
/// the error to answer with) when the node itself cannot be described.
pub fn summary_of(h: &ScanHandle, node: NodeId, top: usize) -> Result<TreeSummaryResult, String> {
    use sk_core::summary::{Failure, Gaps, Walk, class_index, ext_classes};
    use sk_protocol::scan::SummaryPart;

    let walk = h.summary(node, top, !h.running());
    let nodes = h.store.read();
    let me = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        (
            nodes.path(node),
            node_info(&nodes, node),
            on_disk(&nodes, node),
        )
    }));
    let (base, info, here) = match me {
        Ok(v) => v,
        Err(p) => {
            let path =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| nodes.path_lossy(node)))
                    .ok();
            return Err(sk_core::summary::summary_failed(
                &h.id,
                path.as_deref(),
                &Failure::panic(&*p),
            ));
        }
    };
    let mut gaps = Gaps::new(&h.id, &base.to_string_lossy());
    walk.note(&mut gaps);
    let class_ids = h.ctx.basis.class_ids();
    let id_of = |i: usize| class_ids.get(i).cloned().unwrap_or_default();
    let mut by_ext = matches!(walk, Walk::ByExtension(..));
    let classes = if by_ext {
        ext_classes(&h.ctx.basis, &nodes)
    } else {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| h.ctx.basis.classes(&nodes)))
        {
            Ok(c) => c,
            Err(p) => {
                gaps.add(SummaryPart::Classes, &Failure::panic(&*p));
                by_ext = true;
                ext_classes(&h.ctx.basis, &nodes)
            }
        }
    };
    let mut tags = h.ctx.tags.lock();
    let mut tag_of =
        |gaps: &mut Gaps, n: NodeId| gaps.guard(SummaryPart::Tags, || tags.of(&nodes, n), None);
    let (mut tops, mut class_stats, mut latest) = (Vec::new(), Vec::new(), 0i64);
    if let Some(s) = walk.summary() {
        latest = i64::from(s.latest_mtime);
        let ids: Vec<NodeId> = s.top_files.iter().take(top).copied().collect();
        let cls: Vec<usize> = gaps.guard(
            SummaryPart::Classes,
            || {
                ids.iter()
                    .map(|&f| class_index(&classes, &nodes, f, by_ext))
                    .collect()
            },
            Vec::new(),
        );
        let cls = if cls.len() == ids.len() {
            cls
        } else {
            let ext = ext_classes(&h.ctx.basis, &nodes);
            ids.iter()
                .map(|&f| class_index(&ext, &nodes, f, true))
                .collect()
        };
        let listed = gaps.guard(
            SummaryPart::TopFiles,
            || {
                ids.iter()
                    .zip(&cls)
                    .map(|(&f, &ci)| {
                        let parent_path = nodes.path(nodes.parent(f).max(0) as NodeId);
                        let rel = match parent_path.strip_prefix(&base) {
                            Ok(r) if r.as_os_str().is_empty() => String::new(),
                            Ok(r) => format!("{}{}", r.display(), std::path::MAIN_SEPARATOR),
                            Err(_) => String::new(),
                        };
                        (f, rel, ci)
                    })
                    .collect::<Vec<_>>()
            },
            Vec::new(),
        );
        for (f, rel, ci) in listed {
            let file = gaps.guard(SummaryPart::TopFiles, || Some(node_info(&nodes, f)), None);
            let Some(file) = file else { continue };
            tops.push(TopFile {
                node: file,
                rel_parent: rel,
                tag: tag_of(&mut gaps, f),
                class_id: id_of(ci),
            });
        }
        class_stats = gaps.guard(
            SummaryPart::Contents,
            || {
                let n = s.class_bytes.len();
                let mut order: Vec<usize> = (0..n).collect();
                order.sort_by_key(|&i| std::cmp::Reverse(s.class_bytes[i]));
                let parts = |i: usize| -> Vec<sk_protocol::scan::ClassPart> {
                    let Some(p) = &s.class_parts else {
                        return Vec::new();
                    };
                    let row = &p[i * n..(i + 1) * n];
                    let mut idx: Vec<usize> = (0..n).filter(|&j| row[j] > 0).collect();
                    idx.sort_by_key(|&j| std::cmp::Reverse(row[j]));
                    idx.into_iter()
                        .map(|j| sk_protocol::scan::ClassPart {
                            class_id: id_of(j),
                            bytes: row[j],
                        })
                        .collect()
                };
                order
                    .into_iter()
                    .filter(|&i| s.class_files[i] > 0)
                    .map(|i| sk_protocol::scan::ClassStat {
                        class_id: id_of(i),
                        bytes: s.class_bytes[i],
                        files: s.class_files[i],
                        parts: parts(i),
                    })
                    .collect()
            },
            Vec::new(),
        );
    }
    let folder_class = gaps.guard(
        SummaryPart::FolderClass,
        || {
            h.ctx
                .basis
                .class_map()
                .table()
                .and_then(|t| h.folder_classes.explain(&nodes, node, t))
                .map(|why| why.to_protocol(&nodes))
        },
        None,
    );
    let parent = gaps.guard(
        SummaryPart::Parent,
        || {
            let up = nodes.parent(node);
            (up > 0).then(|| node_info(&nodes, up as NodeId))
        },
        None,
    );
    let volume = gaps.guard(
        SummaryPart::Volume,
        || {
            h.root_of(&nodes, node)
                .and_then(|r| r.volume.map(|v| v.to_protocol()))
        },
        None,
    );
    let tag = tag_of(&mut gaps, node);
    Ok(TreeSummaryResult {
        node: info,
        path: base.display().to_string(),
        parent,
        latest_mtime: latest,
        top_files: tops,
        classes: class_stats,
        tag,
        volume,
        on_disk: here,
        folder_class,
        gaps: gaps.into_items(),
        // `nodes` is still held: the numbering its ids were read in
        generation: h.store.generation(),
    })
}

fn status_of(
    server: &Arc<Server>,
    h: Arc<ScanHandle>,
) -> impl std::future::Future<Output = RpcResult<sk_protocol::scan::ScanStatus>> {
    let server = server.clone();
    async move { crate::compute::blocking(move || h.status(&server)).await }
}

fn open_scan_topic(
    server: &Arc<Server>,
    session: &Arc<crate::session::Session>,
    p: &sk_protocol::streams::SubscribeParams,
) -> RpcResult<Subscription> {
    let opts: ScanTopicParams = p
        .topic_params()
        .map_err(|e| RpcError::from_params(&e, "scan topic needs {scan_id}"))?;
    let h = server.scans.get(&opts.scan_id)?;
    let sub = Subscription::new(TOPIC_SCAN, p.rate_hz);
    let flag = sub.flag.clone();
    let weak = Arc::downgrade(session);
    let push = move |status: &sk_protocol::scan::ScanStatus| {
        let last = !matches!(
            status.state,
            ScanStateName::Queued | ScanStateName::Scanning
        );
        if !flag.admit(last) {
            return;
        }
        if let Some(s) = weak.upgrade() {
            s.emit::<ScanProgress>(&ScanProgressEvent {
                sub: flag.id.clone(),
                status: status.clone(),
            });
        }
    };
    // listen first, then send the current status: a final status published in between is not lost
    let push: crate::scans::StatusListener = Arc::new(push);
    h.add_listener(&sub.id, push.clone());
    push(&h.status(server));
    // the listener table, not the handle: a subscription outliving scan.close must not keep it
    let weak_h = Arc::downgrade(&h);
    let key = sub.id.clone();
    sub.on_close(move || {
        if let Some(h) = weak_h.upgrade() {
            h.remove_listener(&key);
        }
    });
    Ok(sub)
}

fn open_io_topic(
    server: &Arc<Server>,
    session: &Arc<crate::session::Session>,
    p: &sk_protocol::streams::SubscribeParams,
) -> RpcResult<Subscription> {
    let opts: IoTopicParams = p
        .topic_params()
        .map_err(|e| RpcError::from_params(&e, "io topic takes {disk?}"))?;
    let sub = Subscription::new(TOPIC_IO, p.rate_hz);
    let govs: Vec<_> = server
        .scans
        .io
        .governors()
        .into_iter()
        .filter(|g| opts.disk.as_deref().is_none_or(|d| g.disk().id == d))
        .collect();
    let mut tokens = Vec::new();
    for g in &govs {
        let flag = sub.flag.clone();
        let weak = Arc::downgrade(session);
        let token = g.subscribe(Arc::new(move |status: &sk_protocol::io::IoStatus| {
            if !flag.admit(false) {
                return;
            }
            if let Some(s) = weak.upgrade() {
                s.emit::<IoLoad>(&IoStatusEvent {
                    sub: flag.id.clone(),
                    status: status.clone(),
                });
            }
        }));
        tokens.push((g.clone(), token));
        session.emit::<IoLoad>(&IoStatusEvent {
            sub: sub.id.clone(),
            status: g.status(true),
        });
    }
    sub.on_close(move || {
        for (g, t) in tokens {
            g.unsubscribe(t);
        }
    });
    Ok(sub)
}

pub fn install(r: &mut Router, t: &mut TopicRegistry) {
    t.register(TOPIC_SCAN, open_scan_topic);
    t.register(TOPIC_IO, open_io_topic);

    r.add::<VolumeList, _, _>(|call, _| async move {
        let volumes = crate::compute::blocking(|| {
            // media probes may block (diskutil): not on the async threads
            list_volumes()
                .iter()
                .map(|v| v.to_protocol())
                .collect::<Vec<_>>()
        })
        .await?;
        Ok(VolumeListResult {
            volumes,
            elevated: call.server.scans.mft_ready(),
        })
    });

    r.add::<ScanCheckPaths, _, _>(|_, p| async move {
        crate::compute::blocking(move || check_paths(&p.paths)).await
    });

    r.add::<ScanStart, _, _>(|call, p| async move {
        let h = call.server.scans.start(&call.server, p).await?;
        status_of(&call.server, h).await
    });
    r.add::<ScanCancel, _, _>(|call, p| async move {
        let h = call.server.scans.cancel(&p.scan_id)?;
        status_of(&call.server, h).await
    });
    r.add::<ScanStatusMethod, _, _>(|call, p| async move {
        let h = call.server.scans.get(&p.scan_id)?;
        status_of(&call.server, h).await
    });
    r.add::<ScanPause, _, _>(|call, p| async move {
        let h = call.server.scans.pause(&call.server, &p.scan_id)?;
        status_of(&call.server, h).await
    });
    r.add::<ScanResume, _, _>(|call, p| async move {
        let h = call.server.scans.resume(&call.server, &p.scan_id)?;
        status_of(&call.server, h).await
    });
    r.add::<ScanUseMft, _, _>(|call, p| async move {
        let h = call.server.scans.use_mft(&call.server, &p.scan_id)?;
        status_of(&call.server, h).await
    });
    r.add::<ScanContinue, _, _>(|call, p| async move {
        let h = call.server.scans.continue_scan(&call.server, &p.scan_id)?;
        status_of(&call.server, h).await
    });
    r.add::<ScanRescan, _, _>(|call, p| async move {
        let h = call.server.scans.get(&p.scan_id)?;
        h.check_node(p.node)?;
        call.server.monitors.rescan(&call.server, &h, p.node)?;
        status_of(&call.server, h).await
    });
    r.add::<ScanSetLoad, _, _>(|call, p| async move {
        let h = call.server.scans.set_load(&call.server, &p)?;
        status_of(&call.server, h).await
    });
    r.add::<IoStatusMethod, _, _>(|call, _| async move {
        Ok(IoStatusList {
            disks: call.server.scans.io.statuses(true),
        })
    });
    r.add::<ScanList, _, _>(|call, _| async move {
        let server = call.server.clone();
        crate::compute::blocking(move || ScanListResult {
            scans: server
                .scans
                .list()
                .iter()
                .map(|h| h.status(&server))
                .collect(),
        })
        .await
    });
    r.add::<ScanClose, _, _>(|call, p| async move {
        let server = &call.server;
        server.scans.get(&p.scan_id)?;
        server.views.close_scan(&p.scan_id);
        server.compares.close_scan(server, &p.scan_id);
        server.scans.close_scan(server, &p.scan_id).await?;
        Ok(EmptyResult {})
    });

    r.add::<TreeNode, _, _>(|call, p| async move {
        if let Some(r) = call.server.compares.tree_node(&p.scan_id, p.node) {
            return r; // compare views ask about change tree rows (W6)
        }
        let h = call.server.scans.get(&p.scan_id)?;
        h.check_node(p.node)?;
        crate::compute::blocking(move || {
            let nodes = h.store.read();
            TreeNodeResult {
                node: node_info(&nodes, p.node),
                path: nodes.path_printable(p.node),
                on_disk: on_disk(&nodes, p.node),
            }
        })
        .await
    });

    r.add::<TreeChildren, _, _>(|call, p| async move {
        let h = call.server.scans.get(&p.scan_id)?;
        h.check_node(p.node)?;
        crate::compute::compute(move || {
            let nodes = h.store.read();
            let by = match p.sort {
                sk_protocol::scan::ChildSort::Size => SortBy::Size,
                sk_protocol::scan::ChildSort::Name => SortBy::Name,
            };
            let ordered = nodes.sorted_children(p.node, by, by == SortBy::Size);
            let page = ordered
                .iter()
                .skip(p.offset as usize)
                .take(p.limit as usize);
            TreeChildrenResult {
                parent: node_info(&nodes, p.node),
                total: ordered.len() as i64,
                items: page.map(|&i| node_info(&nodes, i)).collect(),
            }
        })
        .await
    });

    r.add::<TreeFind, _, _>(|call, p| async move {
        let h = call.server.scans.get(&p.scan_id)?;
        crate::compute::blocking(move || {
            let nodes = h.store.read();
            let node = nodes.find_path(Path::new(&p.path));
            TreeFindResult {
                node: node.map(|n| node_info(&nodes, n)),
            }
        })
        .await
    });

    let latest: Arc<Latest<String>> = Arc::new(Latest::new());
    r.add::<TreeSummary, _, _>(move |call, p| {
        let latest = latest.clone();
        async move {
            let h = call.server.scans.get(&p.scan_id)?;
            h.check_node(p.node)?;
            let key = format!("tree.summary:{}", call.session.id);
            latest
                .run(key, move || summary_of(&h, p.node, p.top as usize))
                .await?
                .map_err(|reason| RpcError::new(sk_protocol::ErrorCode::InternalError, reason))
        }
    });

    r.add::<SnapshotList, _, _>(|_, _| async move {
        crate::compute::blocking(|| {
            let found = sk_store::snapshot::list_snapshots(&sk_core::paths::snapshots_dir());
            SnapshotListResult {
                snapshots: found.iter().map(|(p, h)| snapshot_info(p, h)).collect(),
            }
        })
        .await
    });
    r.add::<SnapshotSave, _, _>(|call, p| async move {
        call.server.scans.save_snapshot(&p.scan_id, p.path).await
    });
    r.add::<SnapshotLoad, _, _>(|call, p| async move {
        let h = call
            .server
            .scans
            .load_snapshot(&call.server, &p.path)
            .await?;
        status_of(&call.server, h).await
    });
}
