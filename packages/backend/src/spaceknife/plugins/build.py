"""``spaceknife plugin build``: the files a plugin's folder carries but nobody writes by hand.

* ``generated/schema.json``: the plugin's own protocol schema (its methods, events, states and
  streams; :mod:`spaceknife_sdk.schema`);
* ``web/sk-plugin.d.ts``: TypeScript types of the same, for its web panels
  (:func:`spaceknife_sdk.tsgen.generate_plugin`);
* ``generated/pinyin.json``: the pinyin of its command titles (the palette finds commands by it);
* ``web/_sk/``: a copy of the app's panel SDK (``bridge.js`` …), so a panel opens on its own in a
  browser in mock mode (``web/index.html?mock=fixtures.json``); inside the app ``_sk/`` is always
  the app's own copy.

``--zip`` packs the folder as ``dist/<id>-<version>.zip`` (what ``plugin install`` and the plugins
page take). ``plugin check`` reports generated files that are missing or out of date.
"""

from __future__ import annotations

import zipfile
from dataclasses import dataclass, field
from pathlib import Path

from spaceknife_protocol.text import Text
from spaceknife_sdk.errors import Issue, ManifestError
from spaceknife_sdk.manifest import load_manifest
from spaceknife_sdk.plugin import Description, describe_declarative

from .check import PANEL_SDK, _texts, describe_in_process, describe_in_subprocess, generated_files, panel_sdk
from .store import plugin_files


@dataclass
class BuildResult:
    plugin: str
    written: list[str] = field(default_factory=list)  # relative paths that changed
    unchanged: list[str] = field(default_factory=list)
    missing_en: list[str] = field(default_factory=list)  # Chinese texts without English
    zip: Path | None = None
    issues: list[Issue] = field(default_factory=list)

    @property
    def ok(self) -> bool:
        return not any(i.severity == "error" for i in self.issues)


def _write(folder: Path, rel: str, data: bytes, result: BuildResult) -> None:
    path = folder / rel
    try:
        if path.read_bytes() == data:
            result.unchanged.append(rel)
            return
    except OSError:
        pass
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)
    result.written.append(rel)


def describe(folder: Path, *, isolated: bool = True) -> tuple[Description | None, list[Issue]]:
    manifest = load_manifest(folder)
    if manifest.entry is None:
        return describe_declarative(folder, manifest), []
    return (describe_in_subprocess if isolated else describe_in_process)(folder)


def build(folder: Path | str, *, zip: bool = False, isolated: bool = True) -> BuildResult:
    """Write the generated files of the plugin in ``folder`` (and with ``zip`` the package)."""
    folder = Path(folder).resolve()
    try:
        manifest = load_manifest(folder)
    except ManifestError as exc:
        return BuildResult(plugin=folder.name, issues=list(exc.issues))
    result = BuildResult(plugin=manifest.id)
    try:
        desc, issues = describe(folder, isolated=isolated)
    except ManifestError as exc:
        desc, issues = None, list(exc.issues)
    result.issues += issues
    if desc is None:
        return result
    for rel, text in generated_files(folder, desc, manifest).items():
        _write(folder, rel, text.encode("utf-8"), result)
    if (folder / "web").is_dir():
        for name, data in panel_sdk().items():
            _write(folder, f"web/_sk/{name}", data, result)
    seen: set[str] = set()
    items: list[tuple[str, Text]] = list(_texts(manifest, ""))
    for c in desc.contributions:
        for _where, t in _texts_of_dict(c.get("spec") or {}):
            items.append(("", t))
    for _where, t in items:
        if t.zh and not (t.en or desc.catalog.get(t.zh)) and t.zh not in seen and _has_cjk(t.zh):
            seen.add(t.zh)
            result.missing_en.append(t.zh)
    if zip:
        result.zip = pack(folder, manifest.id, manifest.version)
    return result


def _has_cjk(text: str) -> bool:
    return any("\u3400" <= ch <= "\u9fff" for ch in text)  # i18n: ignore


def _texts_of_dict(value: object) -> list[tuple[str, Text]]:
    """Texts in a dumped spec (``{"zh": …, "en": …}`` objects)."""
    out: list[tuple[str, Text]] = []
    if isinstance(value, dict):
        if set(value) <= {"zh", "en"} and isinstance(value.get("zh"), str):
            out.append(("", Text(zh=str(value["zh"]), en=str(value.get("en") or ""))))
        else:
            for v in value.values():
                out += _texts_of_dict(v)
    elif isinstance(value, list):
        for v in value:
            out += _texts_of_dict(v)
    return out


def pack(folder: Path, plugin: str, version: str) -> Path:
    """``dist/<id>-<version>.zip`` with the plugin's files under ``<id>/`` (dist/ itself left out)."""
    out = folder / "dist" / f"{plugin}-{version}.zip"
    out.parent.mkdir(parents=True, exist_ok=True)
    tmp = out.with_suffix(".tmp")
    with zipfile.ZipFile(tmp, "w", zipfile.ZIP_DEFLATED) as z:
        for p in plugin_files(folder):
            rel = p.relative_to(folder).as_posix()
            if rel.startswith(("dist/", "tests/__pycache__/")):
                continue
            z.write(p, f"{plugin}/{rel}")
    tmp.replace(out)
    return out


__all__ = ["PANEL_SDK", "BuildResult", "build", "describe", "pack", "panel_sdk"]
