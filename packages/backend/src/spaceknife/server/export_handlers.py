"""Handlers for export.*: templates and formats, template diagnostics, writing an export (M6)."""

from __future__ import annotations

from pathlib import Path
from typing import TYPE_CHECKING

from spaceknife_protocol import export as pe
from spaceknife_protocol.base import Empty
from spaceknife_protocol.errors import ErrorCode
from spaceknife_protocol.methods import EXPORT_CHECK, EXPORT_RUN, EXPORT_TARGETS

from .compute import compute
from .rpc import RpcError

if TYPE_CHECKING:
    from ..export import TemplateIssue
    from .app import Server
    from .session import Session

# User-facing text, kept in one place for the i18n extraction (M7.5).
TEXT = {
    "compare": "对比视图还不能导出：先退出对比，或用「复制汇总」",
    "no_target": "没有选择导出模板或格式",
    "template_errors": "模板有错误，没有导出：{first}",
    "folder": "目标文件夹不存在：{folder}",
    "write": "写不了 {path}：{reason}",
}


def _issue(i: TemplateIssue) -> pe.ExportIssue:
    return pe.ExportIssue(
        severity=i.severity, code=i.code, message=i.message, section=i.section, line=i.line, column=i.column
    )


def install(server: Server) -> None:
    router = server.router

    @router.method(EXPORT_TARGETS)
    async def export_targets(session: Session, _: Empty) -> pe.ExportTargets:
        from ..export import BUILTIN_INFO, FORMATS, builtin_template

        out: list[pe.ExportTarget] = []
        for name, info in BUILTIN_INFO.items():
            tpl = builtin_template(name)
            out.append(
                pe.ExportTarget(
                    name=name,
                    title=info.title,
                    description=info.description,
                    kind="template",
                    extension="txt",
                    file_name=tpl.file_name if tpl is not None else "",
                    spacesniffer=info.spacesniffer,
                )
            )
        for key, f in FORMATS.items():
            out.append(pe.ExportTarget(name=key, title=f.title, kind="format", extension=f.extension))
        return pe.ExportTargets(targets=out)

    @router.method(EXPORT_CHECK)
    async def export_check(session: Session, params: pe.ExportCheckParams) -> pe.ExportCheckResult:
        from ..export import parse_template

        tpl = parse_template(params.source)
        return pe.ExportCheckResult(ok=tpl.ok, issues=[_issue(i) for i in tpl.issues])

    @router.method(EXPORT_RUN)
    async def export_run(session: Session, params: pe.ExportRunParams) -> pe.ExportRunResult:
        from ..export import (
            ExportError,
            ExportFormat,
            ExportTemplate,
            export_chunks,
            parse_template,
            resolve,
            write_export,
        )

        host = server.views.get(session, params.view_id)
        view = host.view
        if view.compare is not None:
            raise RpcError(ErrorCode.INVALID_REQUEST, TEXT["compare"])
        target: ExportTemplate | ExportFormat
        try:
            if params.source is not None:
                target = parse_template(params.source)
            elif params.target:
                target = resolve(params.target)
            else:
                raise RpcError(ErrorCode.INVALID_PARAMS, TEXT["no_target"])
        except ExportError as exc:
            raise RpcError(ErrorCode.INVALID_PARAMS, str(exc)) from exc
        issues = list(target.issues) if isinstance(target, ExportTemplate) else []
        if isinstance(target, ExportTemplate) and not target.ok:
            first = next(i for i in issues if i.severity == "error")
            raise RpcError(ErrorCode.INVALID_PARAMS, TEXT["template_errors"].format(first=first.message))
        path = Path(params.path)
        if not path.parent.is_dir():
            raise RpcError(ErrorCode.INVALID_PARAMS, TEXT["folder"].format(folder=path.parent))
        res = view.filter.result if params.use_filter and view.filter.active else None
        root = view.zoom.node if params.scope == "zoom" else 0
        bom = isinstance(target, ExportFormat) and target.bom
        text = view.filter.text if res is not None else ""

        def work() -> int:
            chunks = export_chunks(
                target, view.store, root, match=res.match if res else None, filter_text=text
            )
            return write_export(path, chunks, bom=bom)

        try:
            written = await compute(work)
        except OSError as exc:
            raise RpcError(
                ErrorCode.INVALID_REQUEST, TEXT["write"].format(path=path, reason=exc.strerror or exc)
            ) from exc
        return pe.ExportRunResult(path=str(path), bytes=written, issues=[_issue(i) for i in issues])
