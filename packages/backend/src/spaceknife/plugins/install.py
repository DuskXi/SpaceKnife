"""Installing plugins from a folder or a zip, and uninstalling them (to the recycle bin).

Unpacking is careful: no absolute paths, ``..``, links or devices; at most 2000 files and 64 MiB,
each member at most 100 times its compressed size (zip bombs). Everything lands in
``plugins/.incoming/<random>/`` first and is checked there; only then does it become
``plugins/<id>/``. Installing never enables anything: the user confirms the permissions next.
"""

from __future__ import annotations

import contextlib
import os
import secrets
import shutil
import stat
import zipfile
from collections.abc import Callable, Sequence
from dataclasses import dataclass
from pathlib import Path, PurePosixPath

from spaceknife_sdk.errors import ManifestError
from spaceknife_sdk.manifest import MANIFEST, load_manifest

from ..i18n import _, tr
from . import paths as ppaths
from .store import MAX_BYTES, MAX_FILES, plugin_files

MAX_RATIO = 100


class InstallError(ValueError):
    """Installing failed; the message is for the user."""


@dataclass(frozen=True, slots=True)
class Installed:
    id: str
    version: str
    installed: bool
    conflict: bool = False
    existing_version: str = ""
    folder: Path | None = None


def _incoming() -> Path:
    base = ppaths.plugins_dir() / ".incoming" / secrets.token_hex(6)
    base.mkdir(parents=True, exist_ok=False)
    return base


def _safe_member(name: str) -> PurePosixPath:
    p = PurePosixPath(name.replace("\\", "/"))
    if p.is_absolute() or name.startswith(("/", "\\")) or (len(name) > 1 and name[1] == ":"):
        raise InstallError(tr("压缩包里有绝对路径：{name}", name=name))
    if any(part == ".." for part in p.parts):
        raise InstallError(tr("压缩包里有指向外面的路径：{name}", name=name))
    return p


def unpack_zip(source: Path, target: Path) -> None:
    """Unpack ``source`` into ``target`` with the rules above (a single top folder is dropped)."""
    try:
        zf = zipfile.ZipFile(source)
    except (zipfile.BadZipFile, OSError) as exc:
        raise InstallError(tr("打不开压缩包：{reason}", reason=exc)) from exc
    with zf:
        infos = [i for i in zf.infolist() if not i.is_dir()]
        if len(infos) > MAX_FILES:
            raise InstallError(tr("压缩包里的文件超过 {n} 个", n=MAX_FILES))
        total = 0
        names = [_safe_member(i.filename) for i in zf.infolist()]
        tops = {n.parts[0] for n in names if n.parts}
        strip = len(tops) == 1 and not any(
            len(n.parts) == 1 and not i.is_dir() for n, i in zip(names, zf.infolist(), strict=True)
        )
        for info in infos:
            kind = stat.S_IFMT((info.external_attr >> 16) & 0xFFFF)  # 0: no Unix mode recorded
            if kind and kind not in (stat.S_IFREG, stat.S_IFDIR):
                raise InstallError(tr("压缩包里有链接或特殊文件：{name}", name=info.filename))
            total += info.file_size
            if total > MAX_BYTES:
                raise InstallError(tr("解压后超过 {mb} MiB", mb=MAX_BYTES >> 20))
            if (
                info.compress_size
                and info.file_size / info.compress_size > MAX_RATIO
                and info.file_size > 1 << 20
            ):
                raise InstallError(tr("压缩包里的文件压缩比异常：{name}", name=info.filename))
            rel = _safe_member(info.filename)
            parts = rel.parts[1:] if strip else rel.parts
            if not parts:
                continue
            dest = target.joinpath(*parts)
            if not dest.resolve().is_relative_to(target.resolve()):
                raise InstallError(tr("压缩包里有指向外面的路径：{name}", name=info.filename))
            dest.parent.mkdir(parents=True, exist_ok=True)
            written = 0
            with zf.open(info) as src, open(dest, "wb") as out:
                while chunk := src.read(1 << 20):
                    written += len(chunk)
                    if written > info.file_size or written > MAX_BYTES:
                        raise InstallError(tr("压缩包里的文件大小与记录不符：{name}", name=info.filename))
                    out.write(chunk)


def copy_folder(source: Path, target: Path) -> None:
    try:
        files = plugin_files(source)
    except ValueError as exc:
        raise InstallError(str(exc)) from exc
    total = 0
    for f in files:
        total += f.stat().st_size
        if total > MAX_BYTES:
            raise InstallError(tr("插件文件夹超过 {mb} MiB", mb=MAX_BYTES >> 20))
        dest = target / f.relative_to(source)
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(f, dest)


def install(
    path: str | Path, *, replace: bool = False, existing: Callable[[str], str | None] | None = None
) -> Installed:
    """Install from a folder or a ``.zip``. With a plugin of the same id installed, nothing happens
    unless ``replace``: the result says ``conflict`` and both versions."""
    source = Path(path).expanduser()
    if not source.exists():
        raise InstallError(tr("找不到 {path}", path=source))
    staging = _incoming()
    try:
        if source.is_dir():
            copy_folder(source, staging)
        elif source.suffix.lower() == ".zip":
            unpack_zip(source, staging)
        else:
            raise InstallError(_("只能从插件文件夹或 .zip 安装"))
        if not (staging / MANIFEST).is_file():
            raise InstallError(tr("这不是插件：缺少 {file}", file=MANIFEST))
        try:
            manifest = load_manifest(staging)
        except ManifestError as exc:
            raise InstallError(tr("插件的清单不合法：{reason}", reason=exc)) from exc
        if manifest.builtin:
            raise InstallError(_("builtin.* 是随程序发布的插件专用的 id"))
        target = ppaths.plugins_dir() / manifest.id
        old_version = existing(manifest.id) if existing is not None else None
        if target.exists() and old_version is None:
            with contextlib.suppress(ManifestError):
                old_version = load_manifest(target).version
            old_version = old_version or "?"
        if target.exists() or old_version is not None:
            if not replace:
                return Installed(manifest.id, manifest.version, False, True, old_version or "")
            if target.exists():
                old = ppaths.plugins_dir() / ".old" / f"{manifest.id}-{old_version}-{secrets.token_hex(3)}"
                old.parent.mkdir(parents=True, exist_ok=True)
                os.replace(target, old)
        os.replace(staging, target)
        return Installed(manifest.id, manifest.version, True, False, old_version or "", target)
    finally:
        if staging.exists():
            shutil.rmtree(staging, ignore_errors=True)


def cleanup_old() -> None:
    """Remove what earlier installs and replacements left (next start)."""
    for name in (".old", ".incoming"):
        folder = ppaths.plugins_dir() / name
        if folder.is_dir():
            shutil.rmtree(folder, ignore_errors=True)


def _trash(paths: Sequence[str]) -> list[str]:
    """Move paths to the recycle bin; returns the error messages (tests replace this)."""
    from ..ops.trash import move_to_trash

    return [r.message or r.code for r in move_to_trash(list(paths), check_locks=False) if not r.ok]


TRASH: Callable[[Sequence[str]], list[str]] = _trash


def uninstall(folder: Path, plugin: str, *, remove_data: bool) -> None:
    """Move the plugin's folder (and its data) to the recycle bin."""
    targets = [str(folder)]
    data = ppaths.data_dir(plugin)
    if remove_data and data.exists():
        targets.append(str(data))
    errors = TRASH(targets)
    if errors:
        raise InstallError(tr("无法移到回收站：{reason}", reason="; ".join(errors)))
    shutil.rmtree(ppaths.cache_dir(plugin), ignore_errors=True)


__all__ = [
    "TRASH",
    "InstallError",
    "Installed",
    "cleanup_old",
    "copy_folder",
    "install",
    "uninstall",
    "unpack_zip",
]
