"""Release helpers for the Release workflow (.github/workflows/release.yml); they run here too.

    uv run python tools/release.py assemble 0.3.4 ARTIFACTS OUT --notes release-notes/0.3.4.md \
        --notes-out notes.md [--build-run URL …]
    uv run python tools/release.py check 0.3.4 [--sums OUT] [--download]

``assemble`` takes the build runs' artifacts (any folder layout: one folder per artifact, as
``actions/download-artifact`` and ``gh run download`` write them), checks every checksum file
against the files next to it, checks that exactly the release's files are there (the names the
update check looks for, docs/packaging.md「更新检查」), copies them into OUT with the checksum files
the release publishes (the two installer lists merged into ``SHA256SUMS.txt``), and fills the
notes' placeholders: ``{{version}}``, ``{{size:FILE}}`` (MiB, rounded, written "N MB" as in every
release so far) and ``{{build_runs}}`` (links to the runs that built the files).

``check`` reads the published release the way an installed SpaceKnife does (spaceknife.update,
no token): it must be the newest of its channel, every kind of install must find its file under
the exact name, and the checksum the app would read for it must match ``--sums`` (the assembled
folder) or, with ``--download``, the file itself.
"""

from __future__ import annotations

import argparse
import hashlib
import re
import shutil
import sys
from dataclasses import dataclass
from pathlib import Path

from rich.console import Console
from rich.table import Table

console = Console(highlight=False)

SUMS = "SHA256SUMS.txt"
INSTALLER_SUMS = ("SHA256SUMS-installers-win-x64.txt", "SHA256SUMS-installers-linux-x64.txt")
_HEX64 = re.compile(r"[0-9a-f]{64}")
_PLACEHOLDER = re.compile(r"\{\{([^{}]*)\}\}")


class ReleaseError(Exception):
    """What is wrong with the artifacts or the notes, said in one line each."""


@dataclass(frozen=True, slots=True)
class Expected:
    """One file of the release and the checksum file that lists it (None: listed in none)."""

    name: str
    sums: str | None


def deb_version(version: str) -> str:
    """deb and rpm write a pre-release part with ~ (it sorts before the release)."""
    return version.replace("-", "~")


def expected(version: str) -> list[Expected]:
    """Every file of a release, with the checksum file it is published in."""
    v, pv = version, deb_version(version)
    out = [
        Expected(f"SpaceKnife-{v}-Setup-x64.exe", SUMS),
        Expected(f"SpaceKnife-{v}-x86_64.AppImage", SUMS),
        Expected(f"spaceknife_{pv}_amd64.deb", SUMS),
        Expected(f"spaceknife-{pv}.x86_64.rpm", SUMS),
        Expected(f"SpaceKnife-{v}-x86_64.AppImage.zsync", None),
    ]
    for target, ext in (
        ("win-x64", "zip"),
        ("win-arm64", "zip"),
        ("linux-x64", "tar.gz"),
        ("linux-arm64", "tar.gz"),
    ):
        out.append(Expected(f"SpaceKnife-{v}-{target}-portable.{ext}", f"SHA256SUMS-portable-{target}.txt"))
    for a in ("arm64", "x64"):
        out.append(Expected(f"SpaceKnife-{v}-mac-{a}.dmg", f"SHA256SUMS-mac-{a}.txt"))
        out.append(Expected(f"SpaceKnife-{v}-mac-{a}-portable.zip", f"SHA256SUMS-mac-{a}.txt"))
    return out


def published_sums(version: str) -> list[str]:
    """The checksum files the release publishes."""
    return sorted({e.sums for e in expected(version) if e.sums})


def parse_sums(text: str) -> dict[str, str]:
    """``sha256sum`` / ``shasum -a 256`` lines -> {file name: hex}; anything else is an error."""
    out: dict[str, str] = {}
    for n, line in enumerate(text.splitlines(), 1):
        if not line.strip():
            continue
        parts = line.strip().split(maxsplit=1)
        if len(parts) != 2 or not _HEX64.fullmatch(parts[0].lower()):
            raise ReleaseError(f"line {n} is not a checksum: {line!r}")
        name = parts[1].strip().lstrip("*").replace("\\", "/").rsplit("/", 1)[-1]
        if name in out:
            raise ReleaseError(f"{name} is listed twice")
        out[name] = parts[0].lower()
    return out


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as fh:
        while chunk := fh.read(1 << 20):
            digest.update(chunk)
    return digest.hexdigest()


def mib(size: int) -> str:
    """A file size as every release so far wrote it: MiB, rounded, with "MB"."""
    return f"{max(1, round(size / (1 << 20)))} MB"


@dataclass(slots=True)
class Assembled:
    files: dict[str, Path]  # published name -> path in OUT
    hashes: dict[str, str]  # file name -> sha256 (every listed file)


def assemble(version: str, artifacts: Path, out: Path) -> Assembled:
    """Check the artifacts and copy the release's files into ``out`` (which must be empty or new)."""
    if not artifacts.is_dir():
        raise ReleaseError(f"no artifacts folder: {artifacts}")
    found: dict[str, list[Path]] = {}
    for p in sorted(artifacts.rglob("*")):
        if p.is_file():
            found.setdefault(p.name, []).append(p)
    problems: list[str] = []
    for name, paths in found.items():
        if len(paths) > 1:
            problems.append(
                f"{name} is in more than one artifact: {', '.join(str(p.parent.name) for p in paths)}"
            )

    # every checksum file, against the files next to it
    hashes: dict[str, str] = {}
    sums_files = {n: ps[0] for n, ps in found.items() if n.upper().startswith("SHA256SUMS")}
    for name, path in sums_files.items():
        try:
            listed = parse_sums(path.read_text(encoding="utf-8"))
        except ReleaseError as exc:
            problems.append(f"{name}: {exc}")
            continue
        if not listed:
            problems.append(f"{name} lists no file")
        for fname, digest in listed.items():
            target = path.parent / fname
            if not target.is_file():
                problems.append(f"{name} lists {fname}, which is not next to it")
                continue
            actual = sha256(target)
            if actual != digest:
                problems.append(f"{fname}: SHA-256 {actual} but {name} says {digest}")
            if fname in hashes and hashes[fname] != digest:
                problems.append(f"{fname} has two different checksums")
            hashes[fname] = digest

    want = expected(version)
    wanted = {e.name for e in want}
    allowed = wanted | set(INSTALLER_SUMS) | set(published_sums(version))
    for e in want:
        if e.name not in found:
            problems.append(f"missing: {e.name}")
        elif e.sums is not None and e.name not in hashes:
            problems.append(f"{e.name} is in no checksum file")
    for name in sorted(found):
        if name not in allowed:
            problems.append(f"not part of a release: {name}")
    for name in (*INSTALLER_SUMS, *(s for s in published_sums(version) if s != SUMS)):
        if name not in found:
            problems.append(f"missing checksum file: {name}")
    if problems:
        raise ReleaseError("\n".join(problems))

    out.mkdir(parents=True, exist_ok=True)
    if any(out.iterdir()):
        raise ReleaseError(f"{out} is not empty")
    files: dict[str, Path] = {}
    for e in want:
        dest = out / e.name
        shutil.copy2(found[e.name][0], dest)
        files[e.name] = dest
    for name in published_sums(version):
        dest = out / name
        if name == SUMS:  # the two installer lists, one file as in every release
            merged = {
                **parse_sums(found[INSTALLER_SUMS[0]][0].read_text(encoding="utf-8")),
                **parse_sums(found[INSTALLER_SUMS[1]][0].read_text(encoding="utf-8")),
            }
            dest.write_text(
                "".join(f"{merged[n]}  {n}\n" for n in sorted(merged)), encoding="utf-8", newline="\n"
            )
        else:
            shutil.copy2(found[name][0], dest)
        files[name] = dest
    return Assembled(files, hashes)


def fill_notes(text: str, version: str, files: dict[str, Path], runs: list[str]) -> str:
    """Replace the placeholders; an unknown one or a size of a file not in the release is an error."""
    problems: list[str] = []

    def one(m: re.Match[str]) -> str:
        key = m.group(1).strip()
        if key == "version":
            return version
        if key == "build_runs":
            if not runs:
                problems.append("{{build_runs}} without --build-run")
                return ""
            return ", ".join(f"[{_run_label(u)}]({u})" for u in runs)
        if key.startswith("size:"):
            name = key[5:].strip()
            if name not in files:
                problems.append(f"{{{{size:{name}}}}}: not a file of this release")
                return m.group(0)
            return mib(files[name].stat().st_size)
        problems.append(f"unknown placeholder {m.group(0)}")
        return m.group(0)

    out = _PLACEHOLDER.sub(one, text)
    if version not in out:
        problems.append(f"the notes never mention {version}")
    if problems:
        raise ReleaseError("\n".join(problems))
    return out


def _run_label(url: str) -> str:
    m = re.search(r"/actions/runs/(\d+)", url)
    return f"run {m.group(1)}" if m else url


# ---------------------------------------------------------------- after publishing

# (install kind, sys.platform, machine) for every kind of install a release serves
INSTALLS = (
    ("nsis", "win32", "AMD64"),
    ("portable", "win32", "AMD64"),
    ("portable", "win32", "ARM64"),
    ("portable", "linux", "x86_64"),
    ("portable", "linux", "aarch64"),
    ("portable", "darwin", "arm64"),
    ("portable", "darwin", "x86_64"),
    ("appimage", "linux", "x86_64"),
    ("deb", "linux", "x86_64"),
    ("rpm", "linux", "x86_64"),
    ("dmg", "darwin", "arm64"),
    ("dmg", "darwin", "x86_64"),
)


def check(version: str, sums_dir: Path | None, download: bool) -> list[str]:
    """What an installed SpaceKnife would find for ``version``; returns the problems."""
    from spaceknife import update  # the backend: present in the workflow's and the repo's env

    agent = "SpaceKnife-release-check"
    releases = update.fetch_releases(update.RELEASES_URL, None, agent).releases or []
    release = next((r for r in releases if r.version == version), None)
    if release is None:
        return [f"{version} is not in the published release list (a draft is not)"]
    problems: list[str] = []
    newest = update.choose(releases, "preview")
    if newest is None or newest.version != version:
        problems.append(f"the newest release (preview channel) is {newest.version if newest else None}")
    if not release.prerelease:
        stable = update.choose(releases, "stable")
        if stable is None or stable.version != version:
            problems.append(f"the newest stable release is {stable.version if stable else None}")
    local: dict[str, str] = {}
    if sums_dir is not None:
        for name in published_sums(version):
            local.update(parse_sums((sums_dir / name).read_text(encoding="utf-8")))
    table = Table("install", "system", "file", "checksum")
    for kind, system, machine in INSTALLS:
        asset = update.pick_asset(release, kind, system, machine)  # type: ignore[arg-type]
        want = update.asset_template(kind, system, machine)  # type: ignore[arg-type]
        want_name = want.format(v=version) if want else None
        if kind in ("deb", "rpm") and want_name:
            want_name = want.format(v=deb_version(version)) if want else None
        if asset is None or asset.name != want_name:
            problems.append(
                f"{kind} on {system}/{machine}: found {asset.name if asset else None}, want {want_name}"
            )
            table.add_row(kind, f"{system}/{machine}", str(asset.name if asset else None), "-")
            continue
        published = update.fetch_checksums(release, asset, agent)
        state = "ok"
        if published is None:
            problems.append(f"{asset.name}: no published checksum")
            state = "none"
        elif local and local.get(asset.name) != published:
            problems.append(f"{asset.name}: published {published}, assembled {local.get(asset.name)}")
            state = "differs"
        elif download:
            actual = _download_sha256(asset.url, agent)
            if actual != published:
                problems.append(f"{asset.name}: downloaded {actual}, published {published}")
                state = "differs"
        table.add_row(kind, f"{system}/{machine}", asset.name, state)
    console.print(table)
    return problems


def _download_sha256(url: str, agent: str) -> str:
    import urllib.request

    digest = hashlib.sha256()
    req = urllib.request.Request(url, headers={"User-Agent": agent})
    with urllib.request.urlopen(req, timeout=120) as resp:
        while chunk := resp.read(1 << 20):
            digest.update(chunk)
    return digest.hexdigest()


# ---------------------------------------------------------------- command line


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    a = sub.add_parser("assemble", help="check the artifacts, collect the release's files, fill the notes")
    a.add_argument("version")
    a.add_argument("artifacts", type=Path)
    a.add_argument("out", type=Path)
    a.add_argument("--notes", type=Path, required=True)
    a.add_argument("--notes-out", type=Path, required=True)
    a.add_argument("--build-run", action="append", default=[], help="URL of a run that built the files")
    c = sub.add_parser("check", help="read the published release as SpaceKnife does")
    c.add_argument("version")
    c.add_argument("--sums", type=Path, help="the assembled folder: its checksums must be the published ones")
    c.add_argument("--download", action="store_true", help="also download every file and hash it")
    args = ap.parse_args(argv)

    if args.cmd == "assemble":
        try:
            done = assemble(args.version, args.artifacts, args.out)
            notes = fill_notes(
                args.notes.read_text(encoding="utf-8"), args.version, done.files, args.build_run
            )
        except (ReleaseError, OSError) as exc:
            console.print(f"[red]release {args.version}: not ready[/]\n{exc}")
            return 1
        args.notes_out.write_text(notes, encoding="utf-8", newline="\n")
        table = Table("file", "size", "sha256")
        for name, path in done.files.items():
            table.add_row(name, mib(path.stat().st_size), done.hashes.get(name, "")[:16])
        console.print(table)
        console.print(f"[green]{len(done.files)} files in {args.out}, notes in {args.notes_out}[/]")
        return 0
    problems = check(args.version, args.sums, args.download)
    for p in problems:
        console.print(f"[red]{p}[/]")
    if not problems:
        console.print(f"[green]{args.version}: every kind of install finds its file and checksum[/]")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
