"""Update checks against the project's GitHub releases (``update.*``, :mod:`spaceknife_protocol.update`).

The pure parts and the blocking I/O they need; the server side (schedule, sessions, the running
download) is :mod:`spaceknife.server.update_handlers`.

- versions: semantic versions with an optional pre-release part and leading ``v``; a tag that is
  not one is ignored. :func:`level` says how far behind the app is (the first part that differs);
- the release to offer (:func:`choose`): never a draft; pre-releases (GitHub's flag, or a version
  like ``0.4.0-beta.1``) only on the ``preview`` channel; the newest by version;
- the file for this kind of install (:func:`pick_asset`), by the names tools/build_package.py gives
  them, and its SHA-256 from the release's ``SHA256SUMS*.txt`` (:func:`parse_checksums`);
- the state kept between runs (``update.json`` in the data directory): the last check, its ETag, the
  trimmed release list, the versions whose card was closed;
- the two network calls: the release list (:func:`fetch_releases`, ``If-None-Match``: a 304 reuses
  the list) and, on the user's click, the download (:func:`download`), hashed while it streams.

Nothing here tells anyone anything about the user: one GET of the public release list with a
``SpaceKnife/<version>`` User-Agent and no token, and the file the user asked for. Proxies are
urllib's defaults (the environment, the system settings on Windows and macOS).
"""

from __future__ import annotations

import contextlib
import functools
import hashlib
import http.client
import json
import math
import os
import platform
import re
import shutil
import ssl
import sys
import threading
import time
import urllib.error
import urllib.request
from collections.abc import Callable, Iterable
from dataclasses import dataclass
from datetime import timedelta
from pathlib import Path
from typing import Any, get_args
from urllib.parse import quote, urlsplit

from loguru import logger
from pydantic import AwareDatetime, BaseModel, Field, ValidationError

from spaceknife_protocol.update import InstallKind, UpdateAsset, UpdateChannel, UpdateInfo, UpdateLevel

from . import paths
from .i18n import Texts

RELEASES_URL = "https://api.github.com/repos/DuskXi/SpaceKnife/releases"
RELEASES_PAGE = "https://github.com/DuskXi/SpaceKnife/releases"
TIMEOUT_S = 15.0  # every network call: connecting, and each read
CHECK_INTERVAL = timedelta(hours=24)
RETRY_INTERVAL = timedelta(hours=6)  # after a failed check
NOTES_MAX = 16 * 1024  # bytes of release notes kept (UTF-8)
LIST_MAX = 8 << 20  # the release list is a few hundred KB; anything far bigger is not one
SUMS_MAX = 1 << 20
CHUNK = 256 * 1024
PROGRESS_GAP_S = 0.25  # download progress at most 4 times a second
KEEP_DISMISSED = 20
KEEP_MUTED = 20
HISTORY_KEEP = 30  # releases kept for the timeline of the 「关于与更新」 page
STATE_VERSION = 1
LOOPBACK = frozenset({"127.0.0.1", "localhost", "::1"})
# trusted CA bundles of the distributions, for a Python whose OpenSSL looks elsewhere
CA_FILES = (
    "/etc/ssl/certs/ca-certificates.crt",  # Debian, Ubuntu
    "/etc/pki/tls/certs/ca-bundle.crt",  # RHEL, Fedora
    "/etc/ssl/cert.pem",  # macOS, Alpine
)

# User-facing text, translated when shown (M7.5).
TEXT = Texts(
    {
        "offline": "连不上 GitHub：{reason}",
        "timeout": "连接 GitHub 超时",
        "certificate": "无法验证 GitHub 的证书：{reason}",
        "rate": "GitHub 暂时限制了查询次数，请过一会儿再试",
        "rate_until": "GitHub 暂时限制了查询次数，约 {minutes} 分钟后恢复",
        "http": "GitHub 返回了错误：HTTP {code}",
        "bad_list": "GitHub 返回的发布列表无法识别",
        "failed": "检查更新失败：{reason}",
        "sums": "取不到这个版本发布的校验值（{reason}），没有下载",
        "download_net": "下载中断：{reason}",
        "download_http": "下载失败：HTTP {code}",
        "incomplete": "下载不完整：收到 {received} 字节，应为 {expected} 字节",
        "mismatch": "下载的文件与发布的 SHA-256 校验值不符，已删除",
        "write": "写不了 {path}：{reason}",
        "download_failed": "下载失败：{reason}",
    }
)


class UpdateError(Exception):
    """A check or download failed; the message is in the current language each time it is read."""

    def __init__(self, key: str, **fields: object) -> None:
        super().__init__(key)
        self.key = key
        self.fields = fields

    def __str__(self) -> str:
        return TEXT[self.key].format(**self.fields)


class Cancelled(Exception):
    """The download was cancelled; its partial file is gone."""


# ---------------------------------------------------------------- versions

_VERSION = re.compile(
    r"[vV]?(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)"
    r"(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?(?:\+[0-9A-Za-z.-]+)?"
)


@functools.total_ordering
@dataclass(frozen=True, slots=True)
class Version:
    """A semantic version; build metadata (``+…``) is dropped, it does not order."""

    major: int
    minor: int
    patch: int
    pre: tuple[str, ...] = ()

    @classmethod
    def parse(cls, text: str) -> Version | None:
        m = _VERSION.fullmatch(text.strip())
        if m is None:
            return None
        pre = tuple(m.group(4).split(".")) if m.group(4) else ()
        return cls(int(m.group(1)), int(m.group(2)), int(m.group(3)), pre)

    @property
    def key(self) -> tuple[int, int, int, tuple[Any, ...]]:
        """Semver precedence: a release after its pre-releases; numeric identifiers numerically and
        before alphanumeric ones; a longer list of equal identifiers after the shorter."""
        if not self.pre:
            return (self.major, self.minor, self.patch, (1,))
        ids = tuple((0, int(p), "") if p.isdigit() else (1, 0, p) for p in self.pre)
        return (self.major, self.minor, self.patch, (0, *ids))

    def __lt__(self, other: object) -> bool:
        if not isinstance(other, Version):
            return NotImplemented
        return self.key < other.key

    def __str__(self) -> str:
        core = f"{self.major}.{self.minor}.{self.patch}"
        return f"{core}-{'.'.join(self.pre)}" if self.pre else core


def _version(v: Version | str | None) -> Version | None:
    return Version.parse(v) if isinstance(v, str) else v


def level(current: Version | str | None, latest: Version | str | None) -> UpdateLevel:
    """How far behind ``latest`` the running ``current`` is: the first part that differs
    (a pre-release of the same version counts as a patch); ``none`` when it is not newer."""
    cur, new = _version(current), _version(latest)
    if cur is None or new is None or not cur < new:
        return "none"
    if new.major != cur.major:
        return "major"
    if new.minor != cur.minor:
        return "minor"
    return "patch"


def series(current: Version | str | None, latest: Version | str | None) -> str | None:
    """The series of ``latest`` that the user can mute (update.mute): ``"0.4"`` (every 0.4.x) for a
    minor update, ``"1"`` (every 1.x) for a major one; None for a patch release or no update."""
    new = _version(latest)
    match level(current, latest):
        case "major" if new is not None:
            return f"{new.major}"
        case "minor" if new is not None:
            return f"{new.major}.{new.minor}"
        case _:
            return None


def series_reached(name: str, current: Version | str | None) -> bool:
    """Whether ``current`` is in series ``name`` or past it: muting it says nothing any more."""
    cur = _version(current)
    try:
        parts = tuple(int(p) for p in name.split("."))
    except ValueError:
        return True  # not a series: drop it
    return cur is not None and (cur.major, cur.minor)[: len(parts)] >= parts


# ---------------------------------------------------------------- releases


class Asset(BaseModel):
    name: str
    size: int = Field(default=0, ge=0)
    url: str


class Release(BaseModel):
    """One GitHub release, trimmed to what the update card needs."""

    version: str  # without the leading v
    tag: str
    name: str = ""
    prerelease: bool = False  # GitHub's flag
    published_at: AwareDatetime | None = None
    url: str  # the release page
    notes: str = ""
    assets: list[Asset] = Field(default_factory=list)

    @property
    def parsed(self) -> Version | None:
        return Version.parse(self.version)

    @property
    def preview(self) -> bool:
        """Offered on the ``preview`` channel only: GitHub's flag or a pre-release version."""
        v = self.parsed
        return self.prerelease or v is None or bool(v.pre)


def allowed_url(url: str, feed: str = RELEASES_URL) -> bool:
    """Pages and files the app may open or fetch: on github.com — or, with a local test feed
    (:func:`feed_url`), next to it."""
    u = urlsplit(url)
    if u.scheme == "https" and u.hostname == "github.com":
        return True
    f = urlsplit(feed)
    return feed != RELEASES_URL and (u.scheme, u.netloc) == (f.scheme, f.netloc)


def cap_notes(text: str, limit: int = NOTES_MAX) -> str:
    """At most ``limit`` UTF-8 bytes, cut at a line end when there is one in the second half."""
    raw = text.encode("utf-8")
    if len(raw) <= limit:
        return text
    cut = raw[:limit].decode("utf-8", "ignore")
    end = cut.rfind("\n")
    return (cut[:end] if end > len(cut) // 2 else cut).rstrip() + "\n…"


def parse_releases(data: Any, feed: str = RELEASES_URL) -> list[Release]:
    """GitHub's release list -> the releases worth offering, newest first: no drafts, no tags
    that are not a version; files hosted anywhere else are dropped."""
    if not isinstance(data, list):
        raise ValueError("not a release list")
    out: list[tuple[Version, Release]] = []
    for item in data:
        if not isinstance(item, dict) or item.get("draft"):
            continue
        tag = item.get("tag_name")
        if not isinstance(tag, str) or (v := Version.parse(tag)) is None:
            continue
        assets = []
        for a in item.get("assets") or ():
            if not isinstance(a, dict):
                continue
            name, url, size = a.get("name"), a.get("browser_download_url"), a.get("size")
            if isinstance(name, str) and isinstance(url, str) and allowed_url(url, feed):
                assets.append(
                    Asset(name=name, url=url, size=size if isinstance(size, int) and size > 0 else 0)
                )
        page = item.get("html_url")
        if not isinstance(page, str) or not allowed_url(page, feed):
            page = f"{RELEASES_PAGE}/tag/{quote(tag)}"
        title, body = item.get("name"), item.get("body")
        fields = {
            "version": str(v),
            "tag": tag,
            "name": title if isinstance(title, str) else "",
            "prerelease": bool(item.get("prerelease")),
            "url": page,
            "notes": cap_notes(body) if isinstance(body, str) else "",
            "assets": assets,
        }
        try:
            release = Release.model_validate({**fields, "published_at": item.get("published_at")})
        except ValidationError:  # an odd date: the rest is still worth offering
            release = Release.model_validate(fields)
        out.append((v, release))
    out.sort(key=lambda t: t[0].key, reverse=True)
    return [r for _, r in out]


def choose(releases: Iterable[Release], channel: UpdateChannel) -> Release | None:
    """The newest release of a channel: ``stable`` skips pre-releases, ``preview`` includes them."""
    best: tuple[Version, Release] | None = None
    for r in releases:
        v = r.parsed
        if v is None or (channel == "stable" and r.preview):
            continue
        if best is None or best[0] < v:
            best = (v, r)
    return best[1] if best is not None else None


def trim(releases: Iterable[Release]) -> list[Release]:
    """What the cache keeps: the newest :data:`HISTORY_KEEP` releases that have a version, newest
    first. They are the timeline of the 「关于与更新」 page; the newest of each channel is among them."""
    known = [(v, r) for r in releases if (v := r.parsed) is not None]
    known.sort(key=lambda vr: vr[0].key, reverse=True)
    return [r for _, r in known[:HISTORY_KEEP]]


def to_info(release: Release, asset: Asset | None, sha256: str | None = None) -> UpdateInfo:
    return UpdateInfo(
        version=release.version,
        tag=release.tag,
        name=release.name or release.tag,
        published_at=release.published_at,
        prerelease=release.prerelease,
        url=release.url,
        notes=release.notes,
        asset=UpdateAsset(name=asset.name, size=asset.size, url=asset.url, sha256=sha256) if asset else None,
    )


# ---------------------------------------------------------------- the file for this install

_V_SLOT = r"[0-9A-Za-z.+~_-]+"  # the version as the packager wrote it (deb / rpm may change - to ~)


def arch(machine: str) -> str:
    m = machine.lower()
    if m in ("amd64", "x86_64", "x64"):
        return "x64"
    if m in ("arm64", "aarch64"):
        return "arm64"
    return m


def asset_templates(kind: InstallKind, system: str, machine: str) -> list[str]:
    """The file names for a kind of install on a platform, ``{v}`` for the version, the one to take
    first first; none: nothing is published for it (a run from source, an unknown install, an
    architecture without builds). ``machine`` is the computer's (Python reads the processor's
    architecture on Windows, also in an emulated x64 process): a portable copy moves to the build
    of the processor it runs on, so an x64 copy on Windows on ARM becomes the native one — and stays
    on x64 for a release without an ARM build (Windows runs it emulated)."""
    a = arch(machine)
    x64 = a == "x64"
    match kind:
        case "nsis":
            return ["SpaceKnife-{v}-Setup-x64.exe"]  # Windows on ARM runs it emulated
        case "portable" if system == "win32" and a == "arm64":
            return ["SpaceKnife-{v}-win-arm64-portable.zip", "SpaceKnife-{v}-win-x64-portable.zip"]
        case "portable" if system == "win32":
            return ["SpaceKnife-{v}-win-x64-portable.zip"]
        case "portable" if system.startswith("linux") and a in ("x64", "arm64"):
            return [f"SpaceKnife-{{v}}-linux-{a}-portable.tar.gz"]
        case "appimage" if x64:
            return ["SpaceKnife-{v}-x86_64.AppImage"]
        case "deb" if x64:
            return ["spaceknife_{v}_amd64.deb"]
        case "rpm" if x64:
            return ["spaceknife-{v}.x86_64.rpm"]
        case "dmg" if a in ("arm64", "x64"):
            return [f"SpaceKnife-{{v}}-mac-{a}.dmg"]
    return []


def asset_template(kind: InstallKind, system: str, machine: str) -> str | None:
    """The first of :func:`asset_templates`, or None."""
    return next(iter(asset_templates(kind, system, machine)), None)


def pick_asset(
    release: Release, kind: InstallKind, system: str | None = None, machine: str | None = None
) -> Asset | None:
    """The release's file for this kind of install: the exact name for its version, else one whose
    version part the packager spelled differently; the first name (:func:`asset_templates`) the
    release has. None when there is none."""
    for template in asset_templates(kind, system or sys.platform, machine or platform.machine()):
        exact = template.format(v=release.version)
        pattern = re.compile(re.escape(template).replace(re.escape("{v}"), _V_SLOT))
        found = [a for a in release.assets if pattern.fullmatch(a.name)]
        if found:
            return next((a for a in found if a.name == exact), found[0])
    return None


# ---------------------------------------------------------------- checksums

_SUMS = re.compile(r"SHA256SUMS[0-9A-Za-z._-]*\.txt", re.IGNORECASE)
_HEX64 = re.compile(r"[0-9a-fA-F]{64}")


def is_checksum_file(name: str) -> bool:
    return _SUMS.fullmatch(name) is not None


def parse_checksums(text: str) -> dict[str, str]:
    """``sha256sum`` / ``shasum -a 256`` output -> {file name: lowercase hex}. ``*`` (binary mode)
    and leading folders are dropped; lines that are not a checksum are skipped."""
    out: dict[str, str] = {}
    for line in text.splitlines():
        parts = line.strip().split(maxsplit=1)
        if len(parts) != 2 or not _HEX64.fullmatch(parts[0]):
            continue
        name = parts[1].strip().lstrip("*").replace("\\", "/").rsplit("/", 1)[-1]
        if name:
            out[name] = parts[0].lower()
    return out


def checksum_files(release: Release, asset: Asset) -> list[Asset]:
    """The release's checksum files, the likeliest to list ``asset`` first: SHA256SUMS-mac-arm64.txt
    for a file with "mac" and "arm64" in its name, then SHA256SUMS.txt, then the rest."""
    name = asset.name.lower()

    def rank(a: Asset) -> int:
        words = [w for w in re.split(r"[-_.]", a.name.lower()[len("sha256sums") : -len(".txt")]) if w]
        if not words:
            return 1
        return 0 if all(w in name for w in words) else 2

    return sorted((a for a in release.assets if is_checksum_file(a.name)), key=rank)


# ---------------------------------------------------------------- environment


def feed_url() -> str:
    """The release list: GitHub's — or ``SPACEKNIFE_UPDATE_FEED`` when it is on this machine
    (tests and demos serve a release list of their own; nothing else is ever contacted)."""
    value = os.environ.get("SPACEKNIFE_UPDATE_FEED", "").strip()
    if value:
        u = urlsplit(value)
        if u.scheme in ("http", "https") and u.hostname in LOOPBACK:
            return value
        logger.warning("SPACEKNIFE_UPDATE_FEED ignored: only a loopback address is allowed ({})", value)
    return RELEASES_URL


def app_version() -> str | None:
    """The running app's version as the launcher tells it (Electron main: ``app.getVersion()``)."""
    return os.environ.get("SPACEKNIFE_APP_VERSION", "").strip() or None


def install_kind() -> InstallKind:
    """How this copy was installed, as the launcher tells it: unset is a run from source."""
    value = os.environ.get("SPACEKNIFE_INSTALL_KIND", "").strip().lower()
    if not value:
        return "dev"
    return value if value in get_args(InstallKind) else "unknown"  # type: ignore[return-value]


def updates_dir() -> Path:
    """Where update files go: ``updates`` in the data directory, not the user's download folder. Each
    file is removed once the version it brings is running (:func:`clean_updates`), and the
    uninstaller removes the folder: an update leaves nothing behind."""
    return paths.data_dir() / "updates"


# the names in the updates folder: the release files (asset_template), and what Electron main writes
# while a portable copy is replaced: the new files unpacked, the old ones until the swap is done, the
# plan for the backend that swaps them (update_swap) and its log; the scripts of earlier test builds
_UPDATE_NAMES = tuple(
    re.compile(re.escape(name).replace(re.escape("{v}"), f"(?P<v>{_V_SLOT})"))
    for name in (
        "SpaceKnife-{v}-Setup-x64.exe",
        "SpaceKnife-{v}-win-x64-portable.zip",
        "SpaceKnife-{v}-win-arm64-portable.zip",
        "SpaceKnife-{v}-linux-x64-portable.tar.gz",
        "SpaceKnife-{v}-linux-arm64-portable.tar.gz",
        "SpaceKnife-{v}-x86_64.AppImage",
        "spaceknife_{v}_amd64.deb",
        "spaceknife-{v}.x86_64.rpm",
        "SpaceKnife-{v}-mac-arm64.dmg",
        "SpaceKnife-{v}-mac-x64.dmg",
        "staging-{v}",
        "previous-{v}",
        "update-{v}.json",
        "update-{v}.ps1",
        "update-{v}.sh",
        "update-{v}.log",
    )
)


def update_file_version(name: str) -> Version | None:
    """The version of a file or folder in the updates folder, from its name; None for any other name.
    A deb or rpm package may spell a pre-release's ``-`` as ``~``."""
    for pattern in _UPDATE_NAMES:
        m = pattern.fullmatch(name)
        if m is not None:
            return Version.parse(m.group("v").replace("~", "-"))
    return None


def clean_updates(folder: Path, current: str | None) -> list[str]:
    """Remove what the updates folder no longer needs (blocking): partial downloads, and everything
    of the running version or an older one (the installer that brought it, a portable copy's unpacked
    files and the old ones it replaced). The file of a newer release stays (downloaded, not installed
    yet), and so does any name this module did not write. Returns the names removed."""
    now = _version(current)
    removed: list[str] = []
    try:
        entries = sorted(folder.iterdir())
    except OSError:
        return removed
    for entry in entries:
        name = entry.name
        stale = name.endswith(".part")
        if not stale and now is not None:
            version = update_file_version(name)
            stale = version is not None and version <= now
        if not stale:
            continue
        try:
            if entry.is_dir() and not entry.is_symlink():
                shutil.rmtree(entry)
            else:
                entry.unlink()
        except OSError as exc:
            logger.warning("could not remove {}: {}", entry, exc)
            continue
        removed.append(name)
    return removed


# ---------------------------------------------------------------- persistent state


class UpdateState(BaseModel):
    """``update.json`` in the data directory."""

    version: int = STATE_VERSION
    feed: str = RELEASES_URL
    checked_at: AwareDatetime | None = None  # the last successful check
    failed_at: AwareDatetime | None = None  # the last failed one, when it came after
    etag: str | None = None  # of ``releases``
    releases: list[Release] = Field(default_factory=list)  # trimmed (:func:`trim`)
    # HISTORY_KEEP when ``releases`` was trimmed: a cache of a version that kept fewer (0.3.2 and
    # older kept one per channel) is fetched again whole, without its ETag
    kept: int = 0
    dismissed: list[str] = Field(default_factory=list)  # versions whose card / dialog was closed
    muted: list[str] = Field(default_factory=list)  # series whose prompts are off (series())


def state_file() -> Path:
    return paths.data_dir() / "update.json"


def load_state(file: Path, feed: str = RELEASES_URL) -> UpdateState:
    """The saved state; a missing or broken file is a fresh start, a cache of another feed is
    dropped (the closed cards and the muted series are kept)."""
    try:
        state = UpdateState.model_validate_json(file.read_bytes())
    except FileNotFoundError:
        return UpdateState(feed=feed)
    except (OSError, ValueError) as exc:
        logger.warning("update state {} is unreadable, starting afresh: {}", file, exc)
        return UpdateState(feed=feed)
    if state.feed != feed:
        return UpdateState(feed=feed, dismissed=state.dismissed, muted=state.muted)
    return state


def write_atomic(file: Path, text: str) -> None:
    file.parent.mkdir(parents=True, exist_ok=True)
    tmp = file.with_name(file.name + ".tmp")
    tmp.write_text(text, encoding="utf-8")
    os.replace(tmp, file)


def save_state(state: UpdateState, file: Path) -> None:
    write_atomic(file, state.model_dump_json(indent=1))


# ---------------------------------------------------------------- network


@functools.cache
def ssl_context() -> ssl.SSLContext:
    """The default context, plus the system's CA bundle where this Python's OpenSSL loaded none (a
    bundled build looking in another distribution's folder; a hashed folder it may still find is
    not counted, and the extra bundle does no harm)."""
    ctx = ssl.create_default_context()
    if sys.platform != "win32" and not ctx.cert_store_stats().get("x509_ca"):
        for ca in CA_FILES:
            if os.path.isfile(ca):
                with contextlib.suppress(ssl.SSLError, OSError):
                    ctx.load_verify_locations(cafile=ca)
                    break
    return ctx


def _reason(exc: BaseException) -> str:
    return str(getattr(exc, "strerror", None) or exc or type(exc).__name__)


def _network_error(exc: BaseException, key: str) -> UpdateError:
    reason: object = exc.reason if isinstance(exc, urllib.error.URLError) else exc
    if isinstance(reason, ssl.SSLCertVerificationError):
        return UpdateError("certificate", reason=reason.verify_message or reason)
    if isinstance(reason, TimeoutError):
        return UpdateError("timeout") if key == "offline" else UpdateError(key, reason=TEXT["timeout"])
    return UpdateError(key, reason=_reason(reason) if isinstance(reason, BaseException) else reason)


def _rate_limit(exc: urllib.error.HTTPError) -> UpdateError | None:
    headers = exc.headers
    body = b""
    with contextlib.suppress(OSError, http.client.HTTPException):
        body = exc.read(4096)
    limited = (
        exc.code == 429
        or headers.get("X-RateLimit-Remaining") == "0"
        or "Retry-After" in headers
        or b"rate limit" in body.lower()
    )
    if exc.code not in (403, 429) or not limited:
        return None
    try:
        reset = float(headers.get("X-RateLimit-Reset", ""))
    except ValueError:
        return UpdateError("rate")
    return UpdateError("rate_until", minutes=max(1, math.ceil((reset - time.time()) / 60)))


def _open(url: str, user_agent: str, timeout: float, headers: dict[str, str] | None = None) -> Any:
    req = urllib.request.Request(url, headers={"User-Agent": user_agent, **(headers or {})})
    return urllib.request.urlopen(req, timeout=timeout, context=ssl_context())


@dataclass(frozen=True, slots=True)
class Fetched:
    releases: list[Release] | None  # None: 304, the cached list still holds
    etag: str | None


def fetch_releases(feed: str, etag: str | None, user_agent: str, timeout: float = TIMEOUT_S) -> Fetched:
    """GET the release list (blocking: run it in a thread). ``etag``: the cached list's, sent as
    ``If-None-Match`` — an unchanged list comes back as 304 and does not count against the limit."""
    headers = {"Accept": "application/vnd.github+json"}
    if etag:
        headers["If-None-Match"] = etag
    try:
        with _open(feed, user_agent, timeout, headers) as resp:
            body = resp.read(LIST_MAX + 1)
            new_etag = resp.headers.get("ETag")
    except urllib.error.HTTPError as exc:
        if exc.code == 304:
            return Fetched(None, etag)
        raise _rate_limit(exc) or UpdateError("http", code=exc.code) from exc
    except (urllib.error.URLError, OSError, http.client.HTTPException) as exc:
        raise _network_error(exc, "offline") from exc
    if len(body) > LIST_MAX:
        raise UpdateError("bad_list")
    try:
        return Fetched(parse_releases(json.loads(body), feed), new_etag)
    except ValueError as exc:  # also json.JSONDecodeError, UnicodeDecodeError
        raise UpdateError("bad_list") from exc


def fetch_checksums(
    release: Release, asset: Asset, user_agent: str, timeout: float = TIMEOUT_S
) -> str | None:
    """The published SHA-256 of ``asset`` (blocking); None when the release publishes none for it.
    Raises :class:`UpdateError` (``sums``) when a checksum file exists but cannot be read: an
    unverifiable download is not started."""
    for sums in checksum_files(release, asset):
        try:
            with _open(sums.url, user_agent, timeout) as resp:
                text = resp.read(SUMS_MAX).decode("utf-8", "replace")
        except urllib.error.HTTPError as exc:
            raise UpdateError("sums", reason=f"HTTP {exc.code}") from exc
        except (urllib.error.URLError, OSError, http.client.HTTPException) as exc:
            raise _network_error(exc, "sums") from exc
        found = parse_checksums(text).get(asset.name)
        if found:
            return found
    return None


Progress = Callable[[str, int, int], None]  # (state: downloading | verifying, received, total)


def _file_sha256(path: Path, cancel: threading.Event) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as fh:
        while chunk := fh.read(1 << 20):
            if cancel.is_set():
                raise Cancelled
            digest.update(chunk)
    return digest.hexdigest()


def download(
    url: str,
    dest: Path,
    *,
    size: int,
    sha256: str | None,
    user_agent: str,
    progress: Progress,
    cancel: threading.Event,
    timeout: float = TIMEOUT_S,
) -> bool | None:
    """Download ``url`` to ``dest`` (blocking): streamed to ``<dest>.part``, hashed on the way,
    checked against ``sha256`` and only then renamed. A ``dest`` that already has the right hash is
    kept as it is. Returns whether the checksum matched (None: none was published).

    Raises :class:`Cancelled` once ``cancel`` is set, :class:`UpdateError` otherwise; either way the
    partial file is gone, and a file whose hash does not match is never kept."""
    expected = sha256.lower() if sha256 else None
    try:
        dest.parent.mkdir(parents=True, exist_ok=True)
        if expected and dest.is_file():
            progress("verifying", 0, size)
            if _file_sha256(dest, cancel) == expected:
                progress("verifying", size, size)
                return True
    except OSError as exc:
        raise UpdateError("write", path=dest, reason=_reason(exc)) from exc
    if cancel.is_set():  # while the checksums were fetched
        raise Cancelled
    part = dest.with_name(dest.name + ".part")
    try:
        try:
            resp = _open(url, user_agent, timeout)
        except urllib.error.HTTPError as exc:
            raise UpdateError("download_http", code=exc.code) from exc
        except (urllib.error.URLError, OSError, http.client.HTTPException) as exc:
            raise _network_error(exc, "download_net") from exc
        with resp:
            total = int(resp.headers.get("Content-Length") or 0) or size
            received, digest = _stream(resp, part, total, progress, cancel)
        if total and received != total:
            raise UpdateError("incomplete", received=received, expected=total)
        if expected and digest != expected:
            raise UpdateError("mismatch")
        try:
            os.replace(part, dest)
        except OSError as exc:
            raise UpdateError("write", path=dest, reason=_reason(exc)) from exc
    except BaseException:
        with contextlib.suppress(OSError):
            part.unlink(missing_ok=True)
        raise
    if os.name == "posix" and dest.suffix == ".AppImage":  # it is the program itself: runnable
        with contextlib.suppress(OSError):
            os.chmod(dest, 0o755)
    return True if expected else None


def _stream(
    resp: Any, part: Path, total: int, progress: Progress, cancel: threading.Event
) -> tuple[int, str]:
    """Copy the response into ``part``: (bytes received, their SHA-256)."""
    digest = hashlib.sha256()
    received = 0
    last = 0.0
    try:
        fh = part.open("wb")
    except OSError as exc:
        raise UpdateError("write", path=part, reason=_reason(exc)) from exc
    with fh:
        progress("downloading", 0, total)
        while True:
            if cancel.is_set():
                raise Cancelled
            try:
                chunk = resp.read(CHUNK)
            except (OSError, http.client.HTTPException) as exc:
                raise _network_error(exc, "download_net") from exc
            if not chunk:
                break
            digest.update(chunk)
            try:
                fh.write(chunk)
            except OSError as exc:
                raise UpdateError("write", path=part, reason=_reason(exc)) from exc
            received += len(chunk)
            now = time.monotonic()
            if now - last >= PROGRESS_GAP_S:
                last = now
                progress("downloading", received, total)
    progress("downloading", received, total)
    return received, digest.hexdigest()


__all__ = [
    "CHECK_INTERVAL",
    "HISTORY_KEEP",
    "KEEP_DISMISSED",
    "KEEP_MUTED",
    "RELEASES_PAGE",
    "RELEASES_URL",
    "RETRY_INTERVAL",
    "TEXT",
    "Asset",
    "Cancelled",
    "Fetched",
    "Release",
    "UpdateError",
    "UpdateState",
    "Version",
    "allowed_url",
    "app_version",
    "asset_template",
    "asset_templates",
    "checksum_files",
    "choose",
    "clean_updates",
    "download",
    "feed_url",
    "fetch_checksums",
    "fetch_releases",
    "install_kind",
    "is_checksum_file",
    "level",
    "load_state",
    "parse_checksums",
    "parse_releases",
    "pick_asset",
    "save_state",
    "state_file",
    "to_info",
    "trim",
    "update_file_version",
    "updates_dir",
    "write_atomic",
]
