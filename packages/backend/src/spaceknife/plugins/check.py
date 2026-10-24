"""``spaceknife plugin check``: everything that can be wrong with a plugin, before anyone enables it.

In order (docs/plugin-dev.md 「检查」): the manifest; the description (the code imported in a
separate process under the audit hook, with no permissions); texts in both languages with the same
placeholders; icons; colours; imports (the frozen app's list); the permissions the code uses against
those it asks for; rule files, templates, filter presets; keys; then the contracts — the plugin
enabled in a :class:`~spaceknife_sdk.testing.MockHost` (in a plugin process by default) and every
code contribution run once on synthetic trees: visualizations under the row contract, colour modes,
export formats, reports within their limits, fields, cleanups, node sets, sources with a sample in
``fixtures/``; and last whether the generated files are current.

Every issue has a file and line where there is one, a code, a message, a fix and the anchor of the
section of docs/plugin-dev.md that explains it (``spaceknife plugin docs --section <anchor>``).
"""

from __future__ import annotations

import ast
import asyncio
import json
import re
import subprocess
import sys
from collections.abc import Iterable, Mapping, Sequence
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from spaceknife_protocol.text import Text
from spaceknife_sdk.errors import Issue, ManifestError
from spaceknife_sdk.manifest import (
    CODE_KINDS,
    MANIFEST,
    ColorModeSpec,
    CommandSpec,
    ExportTemplateSpec,
    FileClassSpec,
    FilterPresetSpec,
    IconSpec,
    KeySpec,
    Manifest,
    PanelSpec,
    ReportSpec,
    Spec,
    VisualizationSpec,
    load_manifest,
    spec_key,
)
from spaceknife_sdk.plugin import Description, describe_declarative

DOC = "docs/plugin-dev.md"
DESCRIBE_TIMEOUT = 60.0

# issue code -> the section of the author docs that explains it
ANCHORS: dict[str, str] = {
    "no_manifest": "manifest",
    "toml": "manifest",
    "unknown_field": "manifest",
    "invalid": "manifest",
    "reserved_id": "manifest",
    "folder_name": "layout",
    "api": "manifest",
    "describe": "code",
    "import": "code",
    "no_entry": "code",
    "no_plugin": "code",
    "untyped": "kind-method",
    "duplicate": "contributions",
    "declared_twice": "contributions",
    "no_code": "contributions",
    "unknown_kind": "contributions",
    "missing_en": "texts",
    "placeholders": "texts",
    "icon": "icons",
    "icon_name": "icons",
    "bad_reference": "kind-command",
    "private_import": "imports",
    "bad_color": "colors",
    "low_contrast": "colors",
    "same_color": "colors",
    "forbidden_import": "imports",
    "unknown_import": "imports",
    "syntax": "code",
    "missing_permission": "permissions",
    "unused_permission": "permissions",
    "rules": "kind-insight_rules",
    "template": "kind-export_template",
    "filter": "kind-filter_preset",
    "key_conflict": "kind-keybinding",
    "panel": "kind-panel",
    "webrtc": "panels",
    "register": "contributions",
    "contract": "testing",
    "visual": "kind-visualization",
    "color": "kind-color_mode",
    "export": "kind-export_format",
    "report": "kind-report",
    "field": "kind-node_field",
    "cleanup": "kind-cleanup",
    "node_set": "kind-node_set",
    "source": "kind-scan_source",
    "hook": "kind-hook",
    "stale_generated": "build",
    "not_built": "build",
    "tests": "testing",
}

# what the code may touch -> the permission it needs (ctx.<name>, the AST check)
API_PERMISSIONS = {
    "tree": "tree.read",
    "scan": "tree.read",
    "scans": "tree.read",
    "node": "tree.read",
    "files": "files.read",
    "net": "net",
    "exec": "exec",
    "basket": "basket.propose",
    "snapshots": "snapshots.read",
    "clipboard": "clipboard",
}
# kinds whose handlers get a tree (so their code needs tree.read without naming ctx.tree)
TREE_KINDS = frozenset({"node_field", "insight", "node_set", "export_format", "cleanup"})
SURFACES = {"light": (0xFF, 0xFF, 0xFF), "dark": (0x08, 0x09, 0x0B)}  # color.bg.window of the themes
SEMANTIC_COLORS = frozenset({"accent", "ok", "warn", "danger", "muted"})
HEX = re.compile(r"^#[0-9a-fA-F]{6}(?:[0-9a-fA-F]{2})?$")
TOKEN = re.compile(r"^[a-z][a-z0-9_]*(\.[a-z0-9_]+)+$")
PLACEHOLDER = re.compile(r"\{([A-Za-z_][A-Za-z0-9_]*)[^}]*\}")
MISSING_EN_FIX = (
    'write T("中文", "English") or { zh = "…", en = "…" }, or add it to locales/en.json'  # i18n: ignore
)
CJK = re.compile("[\u3400-\u9fff]")  # i18n: ignore
WEBRTC = re.compile(r"\b(RTCPeerConnection|webkitRTCPeerConnection|RTCDataChannel|RTCIceCandidate)\b")


@dataclass
class CheckResult:
    folder: Path
    plugin: str
    issues: list[Issue] = field(default_factory=list)
    description: Description | None = None
    manifest: Manifest | None = None

    def errors(self, strict: bool = False) -> list[Issue]:
        bad = ("error", "warning") if strict else ("error",)
        return [i for i in self.issues if i.severity in bad]

    def ok(self, strict: bool = False) -> bool:
        return not self.errors(strict)

    def as_dict(self, strict: bool = False) -> dict[str, Any]:
        return {
            "plugin": self.plugin,
            "folder": str(self.folder),
            "ok": self.ok(strict),
            "strict": strict,
            "issues": [{**i.as_dict(), "doc": doc_of(i)} for i in self.issues],
        }


def doc_of(issue: Issue) -> str:
    anchor = ANCHORS.get(issue.code, "errors")
    return f"{DOC}#{anchor}"


def _issue(code: str, message: str, *, severity: str = "error", **kw: Any) -> Issue:
    return Issue(code, message, severity=severity, **kw)


# --------------------------------------------------------------------------- the description


def describe_in_process(folder: Path) -> tuple[Description | None, list[Issue]]:
    """Import the plugin here (tests, ``MockHost.check``; the plugin's code runs in this process)."""
    from spaceknife_sdk._runtime import describe_safely, load_plugin, unload_plugin

    try:
        manifest = load_manifest(folder)
        plugin = load_plugin(folder, manifest)
    except ManifestError as exc:
        return None, list(exc.issues)
    except Exception as exc:
        return None, [_issue("import", f"{type(exc).__name__}: {exc}", file=_entry(folder))]
    try:
        desc, err = describe_safely(plugin)
    finally:
        unload_plugin(manifest)
    if desc is None:
        return None, _error_issues(err)
    return Description.model_validate(desc), []


def describe_in_subprocess(
    folder: Path, timeout: float = DESCRIBE_TIMEOUT
) -> tuple[Description | None, list[Issue]]:
    """Import the plugin in a plugin process (audit hook, no permissions) and take its description."""
    from .runtime.process import child_command, child_env

    try:
        proc = subprocess.run(
            [*child_command(), "--describe", str(folder)],
            capture_output=True,
            timeout=timeout,
            env=child_env(),
            check=False,
        )
    except subprocess.TimeoutExpired:
        return None, [_issue("describe", f"importing the plugin took more than {timeout:.0f} s")]
    except OSError as exc:
        return None, [_issue("describe", f"cannot start a plugin process: {exc}")]
    line = next(
        (ln for ln in reversed(proc.stdout.decode("utf-8", "replace").splitlines()) if ln.strip()), ""
    )
    try:
        out = json.loads(line)
    except ValueError:
        tail = proc.stderr.decode("utf-8", "replace").strip().splitlines()[-5:]
        return None, [
            _issue("describe", f"the plugin process exited ({proc.returncode}): {' | '.join(tail)}")
        ]
    if out.get("description") is not None:
        return Description.model_validate(out["description"]), []
    return None, _error_issues(out.get("error"))


def _entry(folder: Path) -> str:
    try:
        return load_manifest(folder).entry or MANIFEST
    except ManifestError:
        return MANIFEST


def _error_issues(err: Any) -> list[Issue]:
    if not isinstance(err, Mapping):
        return [_issue("describe", str(err))]
    issues = [
        Issue(
            str(i.get("code", "invalid")),
            str(i.get("message", "")),
            file=str(i.get("file", "")),
            line=int(i.get("line", 0) or 0),
            fix=str(i.get("fix", "")),
            severity=str(i.get("severity", "error")),
        )
        for i in err.get("issues") or ()
    ]
    if issues:
        return issues
    message = str(err.get("message", "describe failed"))
    trace = str(err.get("trace", ""))
    file, line = _trace_place(trace)
    return [_issue(str(err.get("code", "describe")), message, file=file, line=line, fix=_last_lines(trace))]


def _trace_place(trace: str) -> tuple[str, int]:
    hits = re.findall(r'File "([^"]+)", line (\d+)', trace)
    for path, line in reversed(hits):
        if "spaceknife_sdk" not in path and "importlib" not in path and "<frozen" not in path:
            return Path(path).name, int(line)
    return "", 0


def _last_lines(trace: str) -> str:
    lines = [ln for ln in trace.strip().splitlines() if ln.strip()]
    return lines[-1].strip() if lines else ""


# --------------------------------------------------------------------------- static checks


def _texts(value: Any, where: str) -> Iterable[tuple[str, Text]]:
    if isinstance(value, Text):
        yield where, value
    elif isinstance(value, Spec | Manifest):
        for name in type(value).model_fields:
            yield from _texts(getattr(value, name), f"{where}.{name}" if where else name)
    elif isinstance(value, list | tuple):
        for i, v in enumerate(value):
            yield from _texts(v, f"{where}[{i}]")
    elif isinstance(value, dict):
        for k, v in value.items():
            yield from _texts(v, f"{where}.{k}")


def check_texts(
    specs: Sequence[tuple[str, Spec]], manifest: Manifest, catalog: Mapping[str, str]
) -> list[Issue]:
    out: list[Issue] = []
    seen: set[str] = set()
    items = list(_texts(manifest.name, "name"))
    if manifest.description is not None:
        items += list(_texts(manifest.description, "description"))
    for kind, spec in specs:
        items += list(_texts(spec, f"{kind} {getattr(spec, 'name', '') or ''}".strip()))
    for where, text in items:
        en = text.en or catalog.get(text.zh, "")
        if not en and text.zh and text.zh not in seen and CJK.search(text.zh):
            seen.add(text.zh)
            out.append(
                _issue(
                    "missing_en",
                    f"{where}: {text.zh!r} has no English",
                    severity="warning",
                    fix=MISSING_EN_FIX,
                )
            )
        if en:
            a, b = set(PLACEHOLDER.findall(text.zh)), set(PLACEHOLDER.findall(en))
            if a != b:
                out.append(
                    _issue(
                        "placeholders",
                        f"{where}: the placeholders differ: {sorted(a)} in Chinese, {sorted(b)} in English",
                        fix="use the same {names} in both languages",
                    )
                )
    return out


def check_icons(folder: Path, specs: Sequence[tuple[str, Spec]]) -> list[Issue]:
    from .icons import sanitize_svg

    files = (
        {f"icons/{p.name}" for p in (folder / "icons").glob("*.svg")}
        if (folder / "icons").is_dir()
        else set()
    )
    files |= {s.file for k, s in specs if isinstance(s, IconSpec)}
    out: list[Issue] = []
    for rel in sorted(files):
        try:
            sanitize_svg((folder / rel).read_text(encoding="utf-8"))
        except (OSError, UnicodeDecodeError, ValueError) as exc:
            out.append(
                _issue("icon", str(exc), file=rel, fix='24×24 viewBox, paths only, stroke="currentColor"')
            )
    return out


def app_icons() -> list[str]:
    """The names of the app's own icons (design/icons/icons.json, as the frontends have them)."""
    from .icons import APP_ICONS

    return sorted(APP_ICONS)


def _icon_ok(name: str, own: set[str]) -> bool:
    if not name:
        return True
    camel = re.sub(r"-([a-z0-9])", lambda m: m.group(1).upper(), name)
    stem = name.rsplit("/", 1)[-1].removesuffix(".svg")
    slug = re.sub(r"[^a-z0-9]+", "_", stem.lower()).strip("_")[:24].rstrip("_")
    return camel in set(app_icons()) or stem in own or slug in own


def check_references(
    folder: Path, manifest: Manifest, desc: Description, specs: Sequence[tuple[str, Spec]]
) -> list[Issue]:
    """Names that point at something: icons (the app's or the plugin's own), and what commands do
    (methods, reports, panels, commands, cleanups, settings, visualizations, colour modes)."""
    import difflib

    from spaceknife_sdk.actions import BUILTIN_COLORS, BUILTIN_VISUALS

    own_icons = {
        re.sub(r"[^a-z0-9]+", "_", p.stem.lower()).strip("_")
        for p in ((folder / "icons").glob("*.svg") if (folder / "icons").is_dir() else [])
    } | {s.name for _k, s in specs if isinstance(s, IconSpec)}
    out: list[Issue] = []
    places: list[tuple[str, str]] = [("plugin.toml icon", manifest.icon)]
    for kind, spec in specs:
        icon = getattr(spec, "icon", None)
        if isinstance(icon, str) and icon:
            places.append((f"{kind} {spec_key(kind, spec)!r}", icon))
    for where, icon in places:
        if not _icon_ok(icon, own_icons):
            near = difflib.get_close_matches(icon, app_icons(), n=3, cutoff=0.4)
            out.append(
                _issue(
                    "icon_name",
                    f"{where}: {icon!r} is not an icon of the app or of the plugin",
                    severity="warning",
                    fix=("closest: " + ", ".join(near) + "; " if near else "")
                    + "the list: spaceknife plugin docs --section api-icons; own ones: icons/<name>.svg",
                )
            )
    names: dict[str, set[str]] = {}
    for kind, spec in specs:
        names.setdefault(kind, set()).add(spec_key(kind, spec))
    methods = {m.name for m in desc.methods}
    pid = manifest.id

    def own(kind: str, ref: str, what: str, where: str) -> None:
        if "." in ref:
            return  # another plugin's, by full id: it may not be installed here
        if ref not in names.get(kind, set()):
            near = difflib.get_close_matches(ref, sorted(names.get(kind, set())), n=1)
            out.append(
                _issue(
                    "bad_reference",
                    f"{where}: there is no {what} {ref!r} in this plugin",
                    fix=f"did you mean {near[0]!r}?" if near else f"declare the {what}, or use its full id",
                )
            )

    for kind, spec in specs:
        if (
            isinstance(spec, VisualizationSpec)
            and spec.default_color
            and spec.default_color not in BUILTIN_COLORS
        ):
            own(
                "color_mode",
                spec.default_color.removeprefix(pid + "."),
                "colour mode",
                f"visualization {spec.name!r} default_color",
            )
        if kind == "export_format":
            for o in getattr(spec, "options", []):
                if o.default_from:
                    own(
                        "setting",
                        o.default_from,
                        "setting",
                        f"export format {spec_key(kind, spec)!r} option {o.name!r} default_from",
                    )
    for _kind, spec in specs:
        if not isinstance(spec, CommandSpec) or spec.action is None:
            continue
        a, where = spec.action, f"command {spec.name!r}"
        k = a.get("kind")
        if k == "call" and "." not in str(a.get("method", "")) and a.get("method") not in methods:
            out.append(_issue("bad_reference", f"{where}: there is no method {a.get('method')!r}"))
        elif k == "open":
            what, _, ref = str(a.get("target", "")).partition(":")
            if what in ("report", "panel"):
                own(what, ref, what, where)
        elif k == "command":
            own("command", str(a.get("command", "")), "command", where)
        elif k == "cleanup":
            own("cleanup", str(a.get("cleanup", "")), "cleanup", where)
        elif k == "setting" and "." not in str(a.get("key", "")):
            own("setting", str(a.get("key", "")), "setting", where)
        elif k == "view":
            st = a.get("settings") or {}
            vis = st.get("visual")
            if isinstance(vis, str) and vis not in BUILTIN_VISUALS:
                own("visualization", vis.removeprefix(pid + "."), "visualization", where)
            mode = st.get("color_mode")
            if isinstance(mode, str) and mode not in BUILTIN_COLORS:
                own("color_mode", mode.removeprefix(pid + "."), "colour mode", where)
    return out


def _rgb(hex_color: str) -> tuple[int, int, int]:
    h = hex_color.lstrip("#")
    return int(h[0:2], 16), int(h[2:4], 16), int(h[4:6], 16)


def _luminance(rgb: tuple[int, int, int]) -> float:
    def ch(c: int) -> float:
        v = c / 255
        return v / 12.92 if v <= 0.03928 else ((v + 0.055) / 1.055) ** 2.4

    r, g, b = rgb
    return 0.2126 * ch(r) + 0.7152 * ch(g) + 0.0722 * ch(b)


def contrast(a: str, b: tuple[int, int, int]) -> float:
    la, lb = _luminance(_rgb(a)), _luminance(b)
    hi, lo = max(la, lb), min(la, lb)
    return (hi + 0.05) / (lo + 0.05)


def _color_issues(entry: Any, where: str) -> list[Issue]:
    from spaceknife_protocol.plugins import ThemedColor

    out: list[Issue] = []
    pairs: list[tuple[str, str]] = []
    if isinstance(entry, ThemedColor):
        pairs = [("light", entry.light), ("dark", entry.dark)]
        for theme, value in pairs:
            if not HEX.match(value):
                out.append(_issue("bad_color", f"{where}: {theme} = {value!r} is not #RRGGBB"))
    elif isinstance(entry, str):
        if HEX.match(entry):
            pairs = [("light", entry), ("dark", entry)]
        elif entry in SEMANTIC_COLORS or entry.startswith("class:") or TOKEN.match(entry):
            return []
        else:
            return [
                _issue(
                    "bad_color",
                    f"{where}: {entry!r} is not a colour (the app would paint it grey)",
                    fix="#RRGGBB, { light = …, dark = … }, accent / ok / warn / danger / muted, "
                    "class:<id>, or a token",
                )
            ]
    for theme, value in pairs:
        if HEX.match(value) and contrast(value[:7], SURFACES[theme]) < 1.3:
            out.append(
                _issue(
                    "low_contrast",
                    f"{where}: {value} barely shows on the {theme} background",
                    severity="warning",
                    fix="pick a lighter / darker colour, or one per theme: { light = …, dark = … }",
                )
            )
    return out


def check_colors(specs: Sequence[tuple[str, Spec]]) -> list[Issue]:
    out: list[Issue] = []
    for _kind, spec in specs:
        if isinstance(spec, ColorModeSpec):
            seen: dict[str, int] = {}
            for i, entry in enumerate(spec.palette):
                where = f"color_mode {spec.name!r} palette[{i}]"
                out += _color_issues(entry, where)
                key = entry.lower() if isinstance(entry, str) else f"{entry.light}/{entry.dark}".lower()
                if key in seen and i > 0:
                    out.append(
                        _issue(
                            "same_color",
                            f"{where} is the same as palette[{seen[key]}]",
                            severity="warning",
                            fix="two indices with one colour cannot be told apart",
                        )
                    )
                seen.setdefault(key, i)
        elif isinstance(spec, FileClassSpec) and spec.color is not None:
            out += _color_issues(spec.color, f"class {spec.name!r} color")
    return out


def _code_files(folder: Path, manifest: Manifest) -> list[Path]:
    if manifest.entry is None:
        return []
    return [
        p
        for p in sorted(folder.rglob("*.py"))
        if not {"lib", "tests", "__pycache__"} & set(p.relative_to(folder).parts[:-1])
        and "tests" not in p.relative_to(folder).parts[:1]
    ]


def _ctx_uses(tree: ast.AST) -> dict[str, int]:
    """``ctx.<name>`` uses (the first line of each), for any parameter or variable named ctx."""
    out: dict[str, int] = {}
    for node in ast.walk(tree):
        if (
            isinstance(node, ast.Attribute)
            and isinstance(node.value, ast.Name)
            and node.value.id in ("ctx", "context")
            and node.attr in API_PERMISSIONS
        ):
            out.setdefault(node.attr, node.lineno)
        if (
            isinstance(node, ast.Attribute)
            and node.attr == "propose"
            and isinstance(node.value, ast.Attribute)
            and node.value.attr == "basket"
        ):
            out.setdefault("basket", node.lineno)
    return out


def check_permissions(folder: Path, manifest: Manifest, specs: Sequence[tuple[str, Spec]]) -> list[Issue]:
    asked = set(manifest.permissions)
    used: dict[str, tuple[str, int]] = {}
    for path in _code_files(folder, manifest):
        rel = path.relative_to(folder).as_posix()
        try:
            tree = ast.parse(path.read_text(encoding="utf-8"), filename=rel)
        except (SyntaxError, UnicodeDecodeError):
            continue  # check_imports reports it
        for attr, line in _ctx_uses(tree).items():
            perm = API_PERMISSIONS[attr]
            used.setdefault(perm, (rel, line))
    kinds = {k for k, _s in specs}
    if kinds & TREE_KINDS:
        used.setdefault("tree.read", (manifest.entry or MANIFEST, 0))
    if any(
        (isinstance(s, VisualizationSpec) and s.size == "code")
        or (isinstance(s, ColorModeSpec) and s.by is None)
        for _k, s in specs
    ):
        used.setdefault("tree.read", (manifest.entry or MANIFEST, 0))
    out: list[Issue] = []
    for perm, (rel, line) in sorted(used.items()):
        if perm not in asked:
            out.append(
                _issue(
                    "missing_permission",
                    f"the code uses what needs {perm!r}, which {MANIFEST} does not ask for",
                    file=rel,
                    line=line,
                    fix=f'permissions = [..., "{perm}"]',
                )
            )
    for perm in sorted(asked - set(used)):
        if perm in ("tree.read", "clipboard", "host.open") and manifest.entry is None:
            continue  # declarative contributions and actions use them without code
        if perm in ("clipboard", "host.open", "tree.read"):
            continue  # actions (copy, open, reveal) and panels use these too
        out.append(
            _issue(
                "unused_permission",
                f"{perm!r} is asked for but the code does not seem to use it",
                severity="warning",
                file=MANIFEST,
                fix="ask only for what it needs: the user sees the list when enabling it",
            )
        )
    return out


def check_rules(folder: Path) -> list[Issue]:
    from ..insights.rules import _parse_file

    out: list[Issue] = []
    rules = folder / "insights"
    for f in sorted(rules.glob("*.toml")) if rules.is_dir() else []:
        rel = f"insights/{f.name}"
        try:
            text = f.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError) as exc:
            out.append(_issue("rules", str(exc), file=rel))
            continue
        _parsed, problems = _parse_file(text, rel)
        for p in problems:
            out.append(_issue("rules", p.message, file=rel, fix="docs/insights.md"))
    return out


def check_templates(folder: Path, specs: Sequence[tuple[str, Spec]]) -> list[Issue]:
    from ..export import parse_template

    out: list[Issue] = []
    for _kind, spec in specs:
        if not isinstance(spec, ExportTemplateSpec):
            continue
        try:
            source = (folder / spec.file).read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError) as exc:
            out.append(_issue("template", str(exc), file=spec.file))
            continue
        for i in parse_template(source, spec.name).issues:
            out.append(
                _issue(
                    "template",
                    i.message,
                    file=spec.file,
                    line=i.line,
                    severity="error" if i.severity == "error" else "warning",
                    fix="docs/export-templates.md",
                )
            )
    return out


def check_filters(specs: Sequence[tuple[str, Spec]], plugin: str = "") -> list[Issue]:
    from spaceknife_protocol.filter_syntax import parse

    from .adapters import _class, preset_vocab
    from .store import PluginRecord

    out: list[Issue] = []
    rec = PluginRecord(id=plugin, folder=Path("."), manifest=None)
    vocab = preset_vocab([_class(rec, s) for _k, s in specs if isinstance(s, FileClassSpec)])
    for _kind, spec in specs:
        if not isinstance(spec, FilterPresetSpec):
            continue
        parsed = parse(spec.expr, None, vocab)
        bad = [d for d in parsed.diagnostics if d.severity == "error" and d.code != "unknown_set"]
        if parsed.normal is None and bad:
            out.append(
                _issue(
                    "filter",
                    f"filter {spec.name!r}: {spec.expr!r} does not parse ({', '.join(d.code for d in bad)})",
                    file=MANIFEST,
                    fix="docs/filter-syntax.md",
                )
            )
    return out


def check_keys(manifest: Manifest, specs: Sequence[tuple[str, Spec]]) -> list[Issue]:
    from ..palette import commands as pal
    from ..palette.keymap import normalize

    builtin = {normalize(k): c.id for c in pal.registry() if c.owner == "builtin" for k in c.keys}
    mine: dict[str, str] = {}
    out: list[Issue] = []
    for kind, spec in specs:
        pairs: list[tuple[str, str, bool]] = []
        if kind == "command":
            pairs = [(k, str(getattr(spec, "name", "")), False) for k in getattr(spec, "keys", ())]
        elif isinstance(spec, KeySpec):
            pairs = [(spec.key, spec.command, spec.override)]
        for key, command, override in pairs:
            norm = normalize(key)
            if norm in builtin and not override:
                out.append(
                    _issue(
                        "key_conflict",
                        f"{key} is the app's key for {builtin[norm]}: the app's command keeps it",
                        severity="warning",
                        fix="pick another key, or keys = [{ key = …, command = …, override = true }]",
                    )
                )
            if norm in mine and mine[norm] != command:
                out.append(_issue("key_conflict", f"{key} is bound to {mine[norm]!r} and {command!r}"))
            mine.setdefault(norm, command)
    return out


def check_panels(
    folder: Path, specs: Sequence[tuple[str, Spec]], methods: Sequence[str] | None = None
) -> list[Issue]:
    out: list[Issue] = []
    fixtures = folder / "web" / "fixtures.json"
    if fixtures.is_file():
        try:
            mock = json.loads(fixtures.read_text(encoding="utf-8"))
        except (OSError, UnicodeDecodeError, ValueError) as exc:
            out.append(_issue("panel", f"web/fixtures.json is not JSON: {exc}", file="web/fixtures.json"))
        else:
            calls = mock.get("calls") if isinstance(mock, dict) else None
            known = set(methods or ())
            for name in sorted(calls or {}) if isinstance(calls, dict) and methods is not None else []:
                if name not in known:
                    out.append(
                        _issue(
                            "panel",
                            f"web/fixtures.json answers {name!r}, which is not one of the plugin's methods",
                            file="web/fixtures.json",
                            severity="warning",
                            fix="mock mode answers sk.call(name) from calls; rename it or drop it",
                        )
                    )
    for _kind, spec in specs:
        if isinstance(spec, PanelSpec) and not (folder / spec.entry).is_file():
            out.append(_issue("panel", f"panel {spec.name!r}: {spec.entry} does not exist", file=MANIFEST))
    web = folder / "web"
    for f in sorted(web.rglob("*")) if web.is_dir() else []:
        if f.suffix.lower() not in (".js", ".mjs", ".html", ".htm") or "_sk" in f.relative_to(web).parts:
            continue
        try:
            text = f.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        for n, line in enumerate(text.splitlines(), start=1):
            if WEBRTC.search(line):
                out.append(
                    _issue(
                        "webrtc",
                        "web panels may not use WebRTC (it would reach the network around the sandbox)",
                        file=f.relative_to(folder).as_posix(),
                        line=n,
                        fix="talk to the plugin's own methods through sk.call",
                    )
                )
                break
    return out


# --------------------------------------------------------------------------- contracts


def _trees() -> list[Any]:
    from spaceknife_sdk.testing import make_tree

    tiny = make_tree({"only.txt": 10})
    mixed = make_tree(
        {
            "a.log": 500,
            "b.txt": (10, "2020-01-01"),
            "movie.mp4": (5000, "2019-06-01"),
            "photo.jpg": 3000,
            "sub": {"c.log": 200, "d.bin": 1000, "e.dat": b"hello world", "deep": {"f.txt": 7}},
            "project": {"node_modules": {"x.js": 4000}, "build": {"out.o": 900}},
            "empty": {},
        }
    )
    wide = make_tree(
        {
            f"d{i}": {
                f"f{j}.{('txt', 'jpg', 'bin', 'mp4')[j % 4]}": (j + 1) * (i + 1) * 97 for j in range(12)
            }
            for i in range(16)
        }
    )
    return [mixed, tiny, wide]


async def run_contracts(host: Any, description: Description) -> list[Issue]:
    """Run every code contribution of the enabled plugin in ``host`` once (a started MockHost whose
    first tree is the mixed synthetic one)."""
    from spaceknife_protocol.errors import ErrorCode

    from .. import i18n
    from ..server.rpc import RpcError
    from .adapters import specs_of

    specs, _issues = specs_of(description)
    handlers = set(description.handlers)
    out: list[Issue] = []
    trees = _trees()

    def failed(code: str, what: str, exc: BaseException) -> None:
        detail = exc.data.get("detail") if isinstance(exc, RpcError) and isinstance(exc.data, dict) else None
        crashed = (isinstance(detail, dict) and "type" in detail) if isinstance(exc, RpcError) else True
        if isinstance(exc, RpcError) and exc.code == ErrorCode.PLUGIN_ERROR and not crashed:
            return  # a PluginError, a message for the user ("select a folder first"): fine
        text = getattr(exc, "message", None) or f"{type(exc).__name__}: {exc}"
        out.append(_issue(code, f"{what}: {text}", fix="spaceknife plugin render / call shows the traceback"))

    host.scan(trees[0])
    try:
        node: int | None = host.find("sub")
        file_node: int | None = host.find("a.log")
    except Exception:
        node = file_node = None
    granted = set(getattr(host.server.plugins.records.get(host.plugin_id), "granted", ()) or ())
    for kind, spec in specs:
        name = str(getattr(spec, "name", ""))
        try:
            if isinstance(spec, VisualizationSpec):
                for i, tree in enumerate(trees):
                    host.scan(tree)
                    await host.visual(name)
                    if i == 0:
                        await host.visual(name, width=400, height=300)
                host.scan(trees[0])
            elif isinstance(spec, ColorModeSpec):
                frame = await host.color(name)
                pal = getattr(frame, "pal", None)
                if pal is not None and len(pal) and int(max(pal)) >= len(spec.palette):
                    out.append(
                        _issue(
                            "color", f"colour mode {name!r} paints index {int(max(pal))}, beyond its palette"
                        )
                    )
            elif kind == "export_format":
                data = await host.export(name)
                if not data:
                    out.append(
                        _issue(
                            "export", f"export format {name!r} wrote nothing for a tree", severity="warning"
                        )
                    )
            elif isinstance(spec, ReportSpec) and ("report", name) in handlers:
                rep = await host.report(name, node=node if spec.slot == "inspector" else None)
                last = rep.blocks[-1] if rep.blocks else None
                if (
                    last is not None
                    and last.type == "callout"
                    and last.text == i18n._("报告太大，只显示了一部分")
                ):
                    out.append(
                        _issue(
                            "report",
                            f"report {name!r} was cut: at most 200 blocks, 2000 rows a table, 4 levels",
                            severity="warning",
                            fix="r.table(..., more=…) pages the rest; sum up instead of listing everything",
                        )
                    )
            elif kind == "node_field" and node is not None:
                await host.fields(node=node)
            elif kind == "cleanup":
                await host.cleanup(name)
            elif kind == "node_set":
                await host.node_set(name)
            elif kind == "hook":
                event = str(getattr(spec, "event", ""))
                args: dict[str, Any] = {"event": event}
                if event in ("scan_done", "scan_closed"):
                    args["scan"] = {"scan_id": host.scan_id, "roots": ["/"], "items": 1}
                elif event == "settings":
                    args["changed"] = []
                await host.server.plugins.call(host.plugin_id, "hook", name, args)
            elif (
                isinstance(spec, CommandSpec)
                and (spec.action or {}).get("kind") == "call"
                and "." not in str((spec.action or {}).get("method", ""))
                and not granted & {"net", "exec"}  # nothing leaves the machine during a check
            ):
                # as the menus would: on a folder, on a file and with nothing selected, wherever its
                # `when` and `check` let it run (in the plugin's process, as in the app)
                for at in (node, file_node, None):
                    if await host.command_available(name, node=at) is None:
                        await host.command(name, node=at)
            elif kind == "scan_source":
                samples = (
                    sorted((host.folder / "fixtures").glob(f"{name}.*"))
                    if (host.folder / "fixtures").is_dir()
                    else []
                )
                if samples:
                    await host.source(name, str(samples[0]))
                    host.scan(trees[0])
        except AssertionError as exc:
            out.append(
                _issue(
                    "visual" if isinstance(spec, VisualizationSpec) else "contract", f"{kind} {name!r}: {exc}"
                )
            )
        except (RpcError, Exception) as exc:
            code = {
                "visualization": "visual",
                "color_mode": "color",
                "export_format": "export",
                "report": "report",
                "node_field": "field",
                "cleanup": "cleanup",
                "node_set": "node_set",
                "scan_source": "source",
                "hook": "hook",
                "command": "command",
            }.get(kind, "contract")
            failed(code, f"{kind} {name!r}", exc)
    return out


async def _contracts(folder: Path, description: Description, process: bool) -> list[Issue]:
    from spaceknife_sdk.testing import MockHost

    host = MockHost.load(folder, process=process)
    try:
        try:
            await host.start()
        except RuntimeError as exc:
            return [_issue("register", str(exc), fix="the host refused a contribution: fix it first")]
        return await run_contracts(host, description)
    finally:
        await host.close()


# --------------------------------------------------------------------------- generated files


def generated_files(folder: Path, description: Description, manifest: Manifest) -> dict[str, str]:
    """What ``plugin build`` writes (relative path -> text), without the panel SDK copy."""
    from spaceknife_sdk.schema import dumps, export_plugin_schema
    from spaceknife_sdk.tsgen import generate_plugin

    out: dict[str, str] = {}
    schema = export_plugin_schema(description)
    if manifest.entry is not None:
        out["generated/schema.json"] = dumps(schema)
    if (folder / "web").is_dir():
        out["web/sk-plugin.d.ts"] = generate_plugin(schema)
    pinyin = _pinyin_table(description)
    if pinyin is not None:
        out["generated/pinyin.json"] = pinyin
    return out


def _pinyin_table(description: Description) -> str | None:
    """Pinyin of its command titles and filter presets' commands (the palette finds them by pinyin;
    computed here once)."""
    from ..palette import commands as pal

    def title(c: Mapping[str, Any]) -> str:
        return str((c.get("spec") or {}).get("title", {}).get("zh", ""))

    names = sorted(
        {title(c) for c in description.contributions if c.get("kind") == "command"}
        | {
            pal.NAMED.raw("filter").format(name=title(c))
            for c in description.contributions
            if c.get("kind") == "filter_preset" and title(c)
        }
        - {""}
    )
    if not names:
        return None
    try:
        from pypinyin import lazy_pinyin
    except ImportError:
        return None

    rows = [
        f" {json.dumps(n, ensure_ascii=False)}: "
        f"{json.dumps(lazy_pinyin(n, errors=pal.each_char), ensure_ascii=False)}"
        for n in names
    ]
    return "{\n" + ",\n".join(rows) + "\n}\n"


PANEL_SDK = ("bridge.js", "base.css", "sk.d.ts", "guard.js")


def panel_sdk() -> dict[str, bytes]:
    """The app's panel SDK files (the copy in the Python SDK; a test keeps it the same as the
    Electron app's ``resources/plugin-sdk``)."""
    from importlib import resources

    base = resources.files("spaceknife_sdk").joinpath("web")
    return {name: base.joinpath(name).read_bytes() for name in PANEL_SDK}


def check_generated(folder: Path, description: Description, manifest: Manifest) -> list[Issue]:
    out: list[Issue] = []
    if (folder / "web").is_dir():
        for name, data in panel_sdk().items():
            rel = f"web/_sk/{name}"
            try:
                same = (folder / rel).read_bytes() == data
            except OSError:
                out.append(
                    _issue(
                        "not_built",
                        f"{rel} is missing (a panel opened on its own needs it)",
                        severity="warning",
                        fix="spaceknife plugin build",
                    )
                )
                continue
            if not same:
                out.append(
                    _issue(
                        "stale_generated",
                        f"{rel} is not this app's panel SDK",
                        file=rel,
                        severity="warning",
                        fix="spaceknife plugin build",
                    )
                )
    for rel, text in generated_files(folder, description, manifest).items():
        path = folder / rel
        if not path.is_file():
            if rel == "generated/pinyin.json" and not (folder / "generated").is_dir():
                severity = "info"
            else:
                severity = "warning"
            out.append(
                _issue("not_built", f"{rel} is missing", severity=severity, fix="spaceknife plugin build")
            )
            continue
        try:
            current = path.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            current = ""
        if current != text:
            out.append(
                _issue(
                    "stale_generated",
                    f"{rel} is out of date",
                    file=rel,
                    severity="warning",
                    fix="spaceknife plugin build",
                )
            )
    return out


# --------------------------------------------------------------------------- everything


def check_plugin(
    folder: Path | str,
    *,
    contracts: bool = True,
    process: bool = True,
    subprocess_describe: bool = True,
) -> CheckResult:
    """Every check on the plugin in ``folder`` (see the module docstring)."""
    from spaceknife_sdk.imports import check_imports

    folder = Path(folder).resolve()
    result = CheckResult(folder=folder, plugin=folder.name)
    try:
        manifest = load_manifest(folder)
    except ManifestError as exc:
        result.issues += exc.issues
        return result
    result.plugin = manifest.id
    result.manifest = manifest
    if folder.name != manifest.id:
        result.issues.append(
            _issue(
                "folder_name",
                f"the folder is named {folder.name!r}: installed, it must be named {manifest.id!r}",
                severity="info",
                fix="plugin install and plugin build --zip name it for you",
            )
        )
    if manifest.entry is not None:
        desc, issues = (describe_in_subprocess if subprocess_describe else describe_in_process)(folder)
    else:
        try:
            desc, issues = describe_declarative(folder, manifest), []
        except ManifestError as exc:
            desc, issues = None, list(exc.issues)
    result.issues += issues
    imports = check_imports(folder)
    if manifest.builtin:  # a shipped plugin may use the app's own modules
        imports = [i for i in imports if not i.message.startswith("spaceknife.")]
    result.issues += imports
    if desc is None:
        return result
    result.description = desc
    result.issues += static_issues(folder, manifest, desc)
    if contracts and not result.errors() and not manifest.builtin and needs_contracts(desc):
        result.issues += asyncio.run(_contracts(folder, desc, process))
    if not manifest.builtin:
        result.issues += check_generated(folder, desc, manifest)
    if (folder / "tests").is_dir():
        result.issues.append(
            _issue(
                "tests",
                "it has tests: run them with pytest",
                severity="info",
                fix=f"uv run pytest {folder / 'tests'}",
            )
        )
    return result


def static_issues(folder: Path, manifest: Manifest, desc: Description) -> list[Issue]:
    """The checks that read the files and the description (no plugin code runs)."""
    from .adapters import specs_of

    specs, out = specs_of(desc)
    out += check_texts(specs, manifest, desc.catalog)
    out += check_icons(folder, specs)
    out += check_references(folder, manifest, desc, specs)
    out += check_colors(specs)
    out += check_permissions(folder, manifest, specs)
    out += check_rules(folder)
    out += check_templates(folder, specs)
    out += check_filters(specs, manifest.id)
    out += check_keys(manifest, specs)
    out += check_panels(folder, specs, [m.name for m in desc.methods])
    return out


def needs_contracts(desc: Description) -> bool:
    kinds = {str(c.get("kind")) for c in desc.contributions}
    commands = "command" in kinds and bool(desc.methods)  # a command may call the plugin's code
    return bool(kinds & (CODE_KINDS | {"visualization", "color_mode", "report"})) or commands


def format_text(result: CheckResult, strict: bool = False) -> str:
    """The issues as ``plugin check`` prints them (one a line, then a verdict)."""
    lines: list[str] = []
    for i in result.issues:
        mark = {"error": "error", "warning": "warning", "info": "note"}.get(i.severity, i.severity)
        where = f"{i.file}:{i.line}: " if i.file and i.line else f"{i.file}: " if i.file else ""
        lines.append(f"{where}{mark} [{i.code}] {i.message}")
        if i.fix:
            lines.append(f"    fix: {i.fix}")
        lines.append(f"    see: {doc_of(i)}")
    errors = len(result.errors(strict))
    if result.ok(strict):
        lines.append(f"{result.plugin}: ok" + (" (strict)" if strict else ""))
    else:
        lines.append(
            f"{result.plugin}: {errors} problem{'s' if errors != 1 else ''}"
            + (" (strict: warnings count)" if strict else "")
        )
    return "\n".join(lines)


def main_check(
    folder: Path, *, strict: bool, as_json: bool, contracts: bool = True, process: bool = True
) -> int:
    result = check_plugin(folder, contracts=contracts, process=process)
    if as_json:
        sys.stdout.write(json.dumps(result.as_dict(strict), ensure_ascii=False, indent=1) + "\n")
    else:
        sys.stdout.write(format_text(result, strict) + "\n")
    return 0 if result.ok(strict) else 1


__all__ = [
    "ANCHORS",
    "CheckResult",
    "check_plugin",
    "describe_in_process",
    "describe_in_subprocess",
    "doc_of",
    "format_text",
    "generated_files",
    "needs_contracts",
    "run_contracts",
    "static_issues",
]
