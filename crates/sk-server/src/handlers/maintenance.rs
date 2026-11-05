//! The settings page's 「高级」 (UI-W7-36): settings export / import (SpaceKnife's own file or
//! SpaceSniffer's configuration), the scan helper, the cache, remembered tags (Python
//! `server/maintenance_handlers.py`).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use sk_core::settings::io::{ImportContext, export_to, read_plan};
use sk_core::{classes, pathclass, paths, tr};
use sk_protocol::methods::{
    SettingsExport, SettingsImport, SystemCacheClear, SystemHelperStop, SystemMaintenance,
    TagsClearSaved,
};
use sk_protocol::settings::{MaintenanceInfo, SettingsImportResult};
use sk_protocol::{EmptyResult, ErrorCode};

use crate::app::Server;
use crate::rpc::{Router, RpcError, RpcResult};
use crate::topics::TopicRegistry;

/// (bytes, files) below a folder; a missing one: (0, 0). Small folders only (cache, snapshots).
fn tree_bytes(root: &Path) -> (i64, i64) {
    let (mut total, mut files) = (0i64, 0i64);
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(it) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in it.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                stack.push(e.path());
            } else if let Ok(m) = e.metadata() {
                total += m.len() as i64;
                files += 1;
            }
        }
    }
    (total, files)
}

/// Delete what can be deleted: column files of live scans are open (Windows refuses, POSIX keeps
/// the mapping), the rest goes.
fn clear_cache(root: &Path) {
    let Ok(it) = std::fs::read_dir(root) else {
        return;
    };
    for e in it.flatten() {
        let p = e.path();
        let is_dir = e.file_type().is_ok_and(|t| t.is_dir() && !t.is_symlink());
        let _ = if is_dir {
            std::fs::remove_dir_all(&p)
        } else {
            std::fs::remove_file(&p)
        };
    }
}

fn count_files(dir: &Path, depth: usize, ext: &str) -> i64 {
    let Ok(it) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut n = 0;
    for e in it.flatten() {
        let p = e.path();
        if depth > 0 {
            if p.is_dir() {
                n += count_files(&p, depth - 1, ext);
            }
        } else if p.extension().is_some_and(|x| x == ext) && p.is_file() {
            n += 1;
        }
    }
    n
}

fn info_sync(server: &Server) -> MaintenanceInfo {
    let helper = server.scans.helper.current();
    let cache = paths::cache_dir();
    let snaps = paths::snapshots_dir();
    MaintenanceInfo {
        helper_running: helper.is_some(),
        helper_elevated: helper.as_ref().is_some_and(|h| h.elevated()),
        cache_dir: cache.display().to_string(),
        cache_bytes: tree_bytes(&cache).0,
        snapshots_dir: snaps.display().to_string(),
        snapshots_bytes: tree_bytes(&snaps).0,
        snapshot_count: count_files(&snaps, 1, "sks"),
        saved_tags: count_files(&paths::data_dir().join("tags"), 0, "json"),
    }
}

async fn info(server: &Arc<Server>) -> RpcResult<MaintenanceInfo> {
    let s = server.clone();
    crate::compute::blocking(move || info_sync(&s)).await
}

pub fn install(r: &mut Router, _t: &mut TopicRegistry) {
    r.add::<SettingsExport, _, _>(|call, p| async move {
        let path = PathBuf::from(&p.path);
        let folder = path.parent().map(Path::to_path_buf).unwrap_or_default();
        if !folder.is_dir() {
            return Err(RpcError::invalid_params(tr!(
                "目标文件夹不存在：{folder}",
                folder = folder.display().to_string()
            )));
        }
        let st = call.server.settings.clone();
        let res = crate::compute::blocking(move || {
            export_to(
                &path,
                &st.values(),
                &classes::active(),
                &pathclass::folders(),
            )
            .map_err(|e| {
                tr!(
                    "写不了 {path}：{reason}",
                    path = path.display().to_string(),
                    reason = crate::util::os_message(&e)
                )
            })
        })
        .await?;
        res.map_err(|m| RpcError::new(ErrorCode::InvalidRequest, m))?;
        Ok(EmptyResult {})
    });

    r.add::<SettingsImport, _, _>(|call, p| async move {
        let server = call.server.clone();
        let store = server.settings.clone();
        let current = store.values();
        let path = PathBuf::from(&p.path);
        let cur = current.clone();
        let plan = crate::compute::blocking(move || {
            let active = classes::active();
            let folders = pathclass::folders();
            let names: Vec<&str> = sk_core::export::BUILTINS.iter().map(|b| b.name).collect();
            let ctx = ImportContext {
                active: &active,
                folders: &folders,
                builtin_templates: &names,
            };
            read_plan(&path, &cur, &ctx)
        })
        .await?
        .map_err(|e| RpcError::invalid_params(e.0))?;
        let changes = plan.changes(&current);
        if p.apply {
            if !plan.values.is_empty() {
                let (st, values) = (store.clone(), plan.values.clone());
                crate::compute::blocking(move || st.update(&values, None))
                    .await?
                    .map_err(|e| RpcError::invalid_params(e.to_string()))?;
            }
            if let Some(folders) = &plan.folders {
                pathclass::replace_folders(folders.clone());
                super::classes::apply_folders(&server).await?;
            }
            if let Some(table) = &plan.classes {
                super::classes::apply_table(&server, table.clone()).await?;
            }
            tracing::info!(
                "settings imported from {} ({}): {} changes",
                p.path,
                plan.source.as_str(),
                changes.len()
            );
        }
        Ok(SettingsImportResult {
            source: plan.source,
            changes,
            classes: plan.classes.as_ref().map(|t| classes::shown(t)),
            skipped: plan.skipped.clone(),
            applied: p.apply,
        })
    });

    r.add::<SystemMaintenance, _, _>(|call, _| async move { info(&call.server).await });

    r.add::<SystemHelperStop, _, _>(|call, _| async move {
        let helper = call.server.scans.helper.clone();
        crate::compute::blocking(move || helper.close()).await?;
        info(&call.server).await
    });

    r.add::<SystemCacheClear, _, _>(|call, _| async move {
        crate::compute::blocking(|| clear_cache(&paths::cache_dir())).await?;
        info(&call.server).await
    });

    r.add::<TagsClearSaved, _, _>(|call, _| async move {
        crate::compute::blocking(|| {
            let _ = std::fs::remove_dir_all(paths::data_dir().join("tags"));
        })
        .await?;
        call.server.views.forget_tags(&call.server).await;
        info(&call.server).await
    });
}
