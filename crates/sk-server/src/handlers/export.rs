//! export.*: templates and formats, template diagnostics, writing an export (M6; Python
//! `server/export_handlers.py`).

use std::path::PathBuf;

use sk_core::export::{self, Target, protocol_issues};
use sk_core::tr;
use sk_protocol::export::{ExportCheckResult, ExportRunResult, ExportScope, ExportTargets};
use sk_protocol::methods::{ExportCheck, ExportRun, ExportTargetsMethod};

use crate::rpc::{Router, RpcError};
use crate::topics::TopicRegistry;

mod text {
    pub const COMPARE: &str = "对比视图还不能导出：先退出对比，或用「复制汇总」";
    pub const NO_TARGET: &str = "没有选择导出模板或格式";
    pub const TEMPLATE_ERRORS: &str = "模板有错误，没有导出：{first}";
    pub const FOLDER: &str = "目标文件夹不存在：{folder}";
    pub const WRITE: &str = "写不了 {path}：{reason}";
}

pub fn install(r: &mut Router, _t: &mut TopicRegistry) {
    r.add::<ExportTargetsMethod, _, _>(|_, _| async move {
        Ok(ExportTargets {
            targets: export::protocol_targets(),
        })
    });

    r.add::<ExportCheck, _, _>(|_, p| async move {
        let tpl = export::parse_template(&p.source, "");
        Ok(ExportCheckResult {
            ok: tpl.ok(),
            issues: protocol_issues(&tpl.issues),
        })
    });

    r.add::<ExportRun, _, _>(|call, p| async move {
        let host = call.server.views.get(&call.session, &p.view_id)?;
        let target = match (&p.source, p.target.is_empty()) {
            (Some(src), _) => Target::Template(Box::new(export::parse_template(src, ""))),
            (None, false) => export::resolve(&p.target, None)
                .map_err(|e| RpcError::invalid_params(e.to_string()))?,
            (None, true) => return Err(RpcError::invalid_params(tr!(text::NO_TARGET))),
        };
        let issues = match &target {
            Target::Template(t) => t.issues.clone(),
            Target::Format(_) => Vec::new(),
        };
        if let Target::Template(t) = &target
            && !t.ok()
        {
            let first = t
                .issues
                .iter()
                .find(|i| i.severity == export::Severity::Error)
                .map_or("", |i| i.message.as_str());
            return Err(RpcError::invalid_params(tr!(
                text::TEMPLATE_ERRORS,
                first = first
            )));
        }
        let path = PathBuf::from(&p.path);
        let folder = path.parent().map(PathBuf::from).unwrap_or_default();
        if !folder.is_dir() {
            return Err(RpcError::invalid_params(tr!(
                text::FOLDER,
                folder = folder.display().to_string()
            )));
        }
        let out = path.clone();
        let written = crate::compute::compute(move || {
            let v = host.view.lock();
            if v.compare.is_some() {
                return Err(RpcError::invalid_request(tr!(text::COMPARE)));
            }
            let res = if p.use_filter && v.filter.active() {
                v.filter.result.clone()
            } else {
                None
            };
            let root = if p.scope == ExportScope::Zoom {
                v.zoom.node
            } else {
                0
            };
            let text = if res.is_some() {
                v.filter.text.clone()
            } else {
                String::new()
            };
            let store = v.store().clone();
            drop(v);
            let nodes = store.read();
            let mask = res.as_ref().map(|r| r.matched.as_slice());
            export::write_export(&out, target.bom(), |sink| {
                export::export_chunks(&target, &nodes, root, mask, &text, sink)
            })
            .map_err(|e| {
                let reason = match &e {
                    export::ExportError::Io(io) => crate::util::os_message(io),
                    other => other.to_string(),
                };
                RpcError::invalid_request(tr!(
                    text::WRITE,
                    path = out.display().to_string(),
                    reason = reason
                ))
            })
        })
        .await??;
        Ok(ExportRunResult {
            path: path.display().to_string(),
            bytes: written as i64,
            issues: protocol_issues(&issues),
        })
    });
}
