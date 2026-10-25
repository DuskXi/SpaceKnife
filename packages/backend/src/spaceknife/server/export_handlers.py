"""Handlers for export.*: templates and formats, template diagnostics, writing an export (M6)."""

from __future__ import annotations

from collections.abc import Mapping
from pathlib import Path
from typing import TYPE_CHECKING, Any

from spaceknife_protocol import CAP_PLUGINS
from spaceknife_protocol import export as pe
from spaceknife_protocol.base import Empty
from spaceknife_protocol.errors import ErrorCode
from spaceknife_protocol.methods import EXPORT_CHECK, EXPORT_RUN, EXPORT_TARGETS

from ..i18n import Texts, language
from .compute import compute
from .rpc import RpcError

if TYPE_CHECKING:
    from spaceknife_sdk.manifest import SettingSpec

    from ..export import TemplateIssue
    from .app import Server
    from .session import Session

# User-facing text, kept in one place for the i18n extraction (M7.5).
TEXT = Texts(
    {
        "compare": "对比视图还不能导出：先退出对比，或用「复制汇总」",
        "no_target": "没有选择导出模板或格式",
        "template_errors": "模板有错误，没有导出：{first}",
        "folder": "目标文件夹不存在：{folder}",
        "write": "写不了 {path}：{reason}",
        "no_selection": "没有选中要导出的项",
        "option_unknown": "导出格式没有选项 {name}（有 {names}）",
        "option_choice": "导出选项 {name} 的值 {value} 不在可选的 {choices} 里",
        "option_range": "导出选项 {name} 应在 {min} 到 {max} 之间，给的是 {value}",
        "option_type": "导出选项 {name} 的值 {value} 不对：{why}",
    }
)


def check_options(specs: list[SettingSpec], values: Mapping[str, Any]) -> dict[str, Any]:
    """An export format's option values as the dialog or ``report.export`` gives them, checked
    against the format's declared options (a plugin's format reads no surprises): unknown names,
    values that are not among a select's or a multi's choices, numbers out of range are refused
    (RpcError naming the option). A multi's values come back as the declared choices in order."""
    by_name = {o.name: o for o in specs}
    out: dict[str, Any] = {}
    for name, value in values.items():
        o = by_name.get(name)
        if o is None:
            raise RpcError(
                ErrorCode.INVALID_PARAMS,
                TEXT["option_unknown"].format(name=name, names=", ".join(by_name) or "-"),
            )
        allowed = [v for v, _label in o.options]
        if allowed and o.control == "multi":
            parts = value if isinstance(value, list) else [x for x in str(value).split(",") if x]
            bad = [x for x in parts if x not in allowed]
            if bad:
                choices = ", ".join(map(str, allowed))
                raise RpcError(
                    ErrorCode.INVALID_PARAMS,
                    TEXT["option_choice"].format(name=name, value=bad[0], choices=choices),
                )
            picked = [v for v in allowed if v in parts]
            out[name] = picked if isinstance(value, list) else ",".join(map(str, picked))
            continue
        if allowed and value not in allowed:
            raise RpcError(
                ErrorCode.INVALID_PARAMS,
                TEXT["option_choice"].format(name=name, value=value, choices=", ".join(map(str, allowed))),
            )
        if isinstance(o.default, int | float) and not isinstance(o.default, bool):
            if isinstance(value, bool) or not isinstance(value, int | float):
                try:
                    value = float(str(value)) if isinstance(o.default, float) else int(str(value))
                except ValueError:
                    raise RpcError(
                        ErrorCode.INVALID_PARAMS,
                        TEXT["option_type"].format(name=name, value=value, why=type(o.default).__name__),
                    ) from None
            lo = o.min if o.min is not None else float("-inf")
            hi = o.max if o.max is not None else float("inf")
            if not lo <= float(value) <= hi:
                raise RpcError(
                    ErrorCode.INVALID_PARAMS,
                    TEXT["option_range"].format(name=name, value=value, min=o.min, max=o.max),
                )
        out[name] = value
    return out


def _issue(i: TemplateIssue) -> pe.ExportIssue:
    return pe.ExportIssue(
        severity=i.severity, code=i.code, message=i.message, section=i.section, line=i.line, column=i.column
    )


def _option(o: SettingSpec, lang: str, plugin: str, server: Server) -> pe.ExportOption:
    from spaceknife_protocol.settings import SettingOption

    default = server.plugins.option_default(plugin, o)  # default_from: a setting's current value
    return pe.ExportOption(
        name=o.name,
        title=o.title.get(lang),
        description=o.description.get(lang) if o.description else "",
        control=o.control or "text",
        default=default,
        options=[SettingOption(value=v, label=label.get(lang)) for v, label in o.options],
        min=o.min,
        max=o.max,
        step=o.step,
        unit=o.unit.get(lang) if o.unit is not None else "",
        placeholder=o.placeholder.get(lang) if o.placeholder else "",
        group=o.group.get(lang) if o.group else "",
    )


def install(server: Server) -> None:
    router = server.router

    @router.method(EXPORT_TARGETS)
    async def export_targets(session: Session, _: Empty) -> pe.ExportTargets:
        """The registry's templates and formats: the shipped ones (``builtin.export``) under their
        own names first, then — for frontends that know plugins — the plugins', by full id."""
        from ..export import builtin_targets
        from ..extensions import EXTENSIONS

        info, templates, formats = builtin_targets()
        out: list[pe.ExportTarget] = []
        for name, item in info.items():
            out.append(
                pe.ExportTarget(
                    name=name,
                    title=item.title,
                    description=item.description,
                    kind="template",
                    extension="txt",
                    file_name=templates[name].file_name,
                    spacesniffer=item.spacesniffer,
                )
            )
        lang = language()
        for key, f in formats.items():
            out.append(
                pe.ExportTarget(
                    name=key,
                    title=f.title,
                    description=f.description.get(lang) if f.description else "",
                    kind="format",
                    extension=f.extension.lstrip("."),
                )
            )
        if CAP_PLUGINS in session.capabilities:
            for c in EXTENSIONS.of("export_template"):
                if c.shipped:
                    continue
                spec = c.spec
                out.append(
                    pe.ExportTarget(
                        name=c.id,
                        title=spec.title.get(lang),
                        description=spec.description.get(lang) if spec.description else "",
                        kind="template",
                        extension="txt",
                        file_name=c.impl.file_name,
                        plugin=c.owner,
                    )
                )
            for c in EXTENSIONS.of("export_format"):
                if c.shipped:
                    continue
                spec = c.spec
                out.append(
                    pe.ExportTarget(
                        name=c.id,
                        title=spec.title.get(lang),
                        description=spec.description.get(lang) if spec.description else "",
                        kind="format",
                        extension=spec.extension.lstrip("."),
                        plugin=c.owner,
                        options=[_option(o, lang, c.owner, server) for o in spec.options],
                    )
                )
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
            common_root,
            export_chunks,
            parse_template,
            resolve,
            subtree_mask,
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
        store = view.store
        match = res.match if res else None
        selection: list[int] = []
        if params.scope == "zoom":
            root = view.zoom.node
        elif params.scope == "node":
            if params.node is None or not 0 <= params.node < len(store):
                raise RpcError(ErrorCode.UNKNOWN_NODE, f"node {params.node} does not exist")
            root = params.node
            selection = [params.node]
        elif params.scope == "selection":
            picked = params.selection or ([view.selected] if isinstance(view.selected, int) else [])
            selection = [int(x) for x in picked if 0 < int(x) < len(store)]
            if not selection:
                raise RpcError(ErrorCode.INVALID_PARAMS, TEXT["no_selection"])
            root = common_root(store, selection)
        else:
            root = 0
        bom = isinstance(target, ExportFormat) and target.bom
        text = view.filter.text if res is not None else ""
        options: dict[str, Any] = dict(params.options)
        if params.target:  # a plugin's format: its declared options
            from ..extensions import EXTENSIONS

            c = EXTENSIONS.get("export_format", params.target)
            if c is not None and getattr(c.spec, "options", None) is not None:
                options = check_options(list(c.spec.options), options)

        def work() -> int:
            keep = match
            if params.scope == "selection" and len(selection) > 1:
                inside = subtree_mask(store, selection)
                keep = inside if keep is None else (keep[: len(inside)] & inside[: len(keep)])
            chunks = export_chunks(
                target,
                store,
                root,
                match=keep,
                filter_text=text,
                options=options,
                selection=selection,
            )
            return write_export(path, chunks, bom=bom)

        try:
            written = await compute(work)
        except OSError as exc:
            raise RpcError(
                ErrorCode.INVALID_REQUEST, TEXT["write"].format(path=path, reason=exc.strerror or exc)
            ) from exc
        return pe.ExportRunResult(path=str(path), bytes=written, issues=[_issue(i) for i in issues])
