"""Update checks (spaceknife.update): versions, levels, channels, asset choice, checksums, state,
the release list fetch (ETag / 304, errors) and the verified download — against a local HTTP server."""

from __future__ import annotations

import contextlib
import hashlib
import json
import os
import socket
import threading
import time
from collections.abc import Callable, Iterator
from datetime import UTC, datetime
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any

import pytest

from spaceknife import update
from spaceknife.palette.commands import Context, evaluate, registry
from spaceknife.update import Asset, Release, UpdateError, Version
from spaceknife_protocol.update import InstallKind

Route = Callable[[dict[str, str]], tuple[int, dict[str, str], bytes]]  # request headers -> response


class LocalFeed:
    """A stand-in for GitHub on 127.0.0.1: ``routes`` answer GETs, ``requests`` records them."""

    def __init__(self) -> None:
        self.routes: dict[str, Route] = {}
        self.requests: list[tuple[str, dict[str, str]]] = []
        feed = self

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self) -> None:
                headers = {k.lower(): v for k, v in self.headers.items()}
                feed.requests.append((self.path, headers))
                route = feed.routes.get(self.path)
                status, extra, body = route(headers) if route else (404, {}, b"not found")
                with contextlib.suppress(ConnectionError):  # the client stopped reading (cancel)
                    self.send_response(status)
                    for k, v in extra.items():
                        self.send_header(k, v)
                    self.end_headers()
                    self.wfile.write(body)

            def log_message(self, format: str, *args: Any) -> None:
                pass

        self.http = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.http.daemon_threads = True
        self.port = self.http.server_address[1]
        self._thread = threading.Thread(
            target=self.http.serve_forever, kwargs={"poll_interval": 0.05}, name="local-feed", daemon=True
        )

    def url(self, path: str) -> str:
        return f"http://127.0.0.1:{self.port}{path}"

    def file(self, path: str, body: bytes, headers: dict[str, str] | None = None) -> str:
        """Serve ``body`` at ``path`` (with a Content-Length unless ``headers`` say otherwise)."""
        extra = {"Content-Length": str(len(body))} if headers is None else headers
        self.routes[path] = lambda _h: (200, extra, body)
        return self.url(path)

    def hits(self, path: str) -> int:
        return sum(1 for p, _ in self.requests if p == path)

    def __enter__(self) -> LocalFeed:
        self._thread.start()
        return self

    def __exit__(self, *exc: object) -> None:
        self.http.shutdown()
        self.http.server_close()


@pytest.fixture
def no_proxy(monkeypatch: pytest.MonkeyPatch) -> None:
    """urllib goes straight to 127.0.0.1 whatever proxy this machine has set up."""
    monkeypatch.setenv("no_proxy", "*")
    monkeypatch.setenv("NO_PROXY", "*")


@pytest.fixture
def feed(no_proxy: None) -> Iterator[LocalFeed]:
    with LocalFeed() as f:
        yield f


def gh(
    tag: str,
    *,
    pre: bool = False,
    draft: bool = False,
    assets: tuple[str, ...] = (),
    body: str = "notes",
    html: str | None = None,
    published: object = "2026-10-01T12:00:00Z",
) -> dict[str, object]:
    """One release as GitHub's API lists it."""
    base = "https://github.com/DuskXi/SpaceKnife/releases"
    return {
        "tag_name": tag,
        "name": f"SpaceKnife {tag}",
        "draft": draft,
        "prerelease": pre,
        "html_url": html or f"{base}/tag/{tag}",
        "body": body,
        "published_at": published,
        "assets": [
            {"name": n, "size": 10, "browser_download_url": f"{base}/download/{tag}/{n}"} for n in assets
        ],
    }


def release(version: str, *names: str, prerelease: bool = False) -> Release:
    base = "https://github.com/DuskXi/SpaceKnife/releases"
    return Release(
        version=version,
        tag=f"v{version}",
        url=f"{base}/tag/v{version}",
        prerelease=prerelease,
        assets=[Asset(name=n, size=10, url=f"{base}/download/v{version}/{n}") for n in names],
    )


# ---------------------------------------------------------------- versions


def test_version_parsing() -> None:
    assert Version.parse("v0.4.0") == Version(0, 4, 0)
    assert Version.parse("0.4.0") == Version(0, 4, 0)
    assert Version.parse(" V1.2.3 ") == Version(1, 2, 3)
    assert Version.parse("0.4.0-beta.1") == Version(0, 4, 0, ("beta", "1"))
    assert Version.parse("1.0.0+build.7") == Version(1, 0, 0)
    assert str(Version(0, 4, 0, ("rc", "2"))) == "0.4.0-rc.2"
    for bad in ("0.4", "latest", "v1.2.3.4", "1.02.0", "nightly-2026-10-01", "1.0.0-", ""):
        assert Version.parse(bad) is None, bad


def test_version_order_is_semver() -> None:
    ordered = [
        "1.0.0-alpha",
        "1.0.0-alpha.1",
        "1.0.0-alpha.beta",
        "1.0.0-beta",
        "1.0.0-beta.2",
        "1.0.0-beta.11",
        "1.0.0-rc.1",
        "1.0.0",
        "1.0.1",
        "1.1.0",
        "2.0.0",
    ]
    parsed = [Version.parse(v) for v in ordered]
    assert all(p is not None for p in parsed)
    assert sorted(parsed, key=lambda v: v.key if v else ()) == parsed
    assert Version(0, 9, 0) < Version(0, 10, 0) and Version(1, 0, 0) > Version(1, 0, 0, ("rc", "1"))


def test_every_part_is_a_number_of_any_length() -> None:
    # each part grows by one at a time (0.3.9 -> 0.3.10 -> 0.3.100); a part to its left moves
    # only for a real accumulated update: never compared as text or as a decimal number
    ordered = [
        "0.3.2",
        "0.3.9",
        "0.3.10",
        "0.3.11",
        "0.3.99",
        "0.3.100",
        "0.9.12",
        "0.10.0",
        "0.99.99",
        "0.100.0",
        "1.0.0",
        "1.9.0",
        "1.10.0",
        "9.0.0",
        "10.0.0",
        "100.200.300",
    ]
    parsed = [Version.parse(v) for v in ordered]
    assert all(p is not None for p in parsed)
    assert [str(v) for v in sorted(reversed(parsed), key=lambda v: v.key if v else ())] == ordered
    assert Version.parse("0.3.10-beta.10") > Version.parse("0.3.10-beta.9")  # type: ignore[operator]
    newest = update.choose(update.parse_releases([gh("v0.3.9"), gh("v0.3.10"), gh("v0.3.2")]), "stable")
    assert newest is not None and newest.version == "0.3.10"


@pytest.mark.parametrize(
    ("current", "latest", "level"),
    [
        ("0.3.9", "0.3.10", "patch"),
        ("0.3.10", "0.3.100", "patch"),
        ("0.3.99", "0.3.100", "patch"),
        ("0.3.100", "0.4.0", "minor"),
        ("0.9.12", "0.10.0", "minor"),
        ("0.10.0", "0.10.15", "patch"),
        ("0.99.99", "1.0.0", "major"),
        ("9.9.9", "10.0.0", "major"),
        ("0.3.10", "0.3.9", "none"),
        ("0.10.0", "0.9.99", "none"),
        ("0.3.10-beta.9", "0.3.10-beta.10", "patch"),
        ("0.3.0", "0.3.1", "patch"),
        ("0.3.0", "0.4.0", "minor"),
        ("0.3.9", "0.4.0", "minor"),
        ("0.3.5", "1.0.0", "major"),
        ("0.3.0", "0.4.0-beta.1", "minor"),
        ("0.4.0-beta.1", "0.4.0", "patch"),  # the release of the preview being run
        ("0.4.0-beta.1", "0.4.0-beta.2", "patch"),
        ("0.4.0", "0.4.0", "none"),
        ("0.4.0", "0.3.9", "none"),
        ("v0.3.0", "v0.3.1", "patch"),
        ("dev", "0.4.0", "none"),
        ("0.3.0", None, "none"),
    ],
)
def test_level(current: str, latest: str | None, level: str) -> None:
    assert update.level(current, latest) == level


def test_series_that_can_be_muted() -> None:
    # a minor update: its major.minor; a major one: its major; a patch release: none (badge only)
    assert update.series("0.3.2", "0.4.1") == "0.4"
    assert update.series("0.3.2", "0.4.0-beta.1") == "0.4"
    assert update.series("0.3.2", "1.2.0") == "1"
    assert update.series("0.9.9", "0.10.0") == "0.10"
    assert update.series("0.3.2", "0.3.3") is None
    assert update.series("0.4.0", "0.4.0") is None
    assert update.series("dev", "0.4.0") is None
    # reached: the running version is in it or past it, muting it says nothing any more
    assert update.series_reached("0.4", "0.4.0")
    assert update.series_reached("0.4", "0.5.1") and update.series_reached("0.4", "1.0.0")
    assert not update.series_reached("0.4", "0.3.9")
    assert not update.series_reached("0.10", "0.9.0")
    assert update.series_reached("1", "1.0.0") and not update.series_reached("1", "0.9.0")
    assert update.series_reached("junk", "0.3.0")
    assert not update.series_reached("0.4", None)


# ---------------------------------------------------------------- releases and channels


def test_parse_releases_skips_drafts_and_odd_tags() -> None:
    data = [
        gh("v0.5.0", draft=True),
        gh("nightly"),
        gh("v0.4.0-beta.1", pre=True),
        gh("v0.4.0-rc.1"),  # a pre-release by its version, not flagged on GitHub
        gh("v0.3.1", published="not a date"),
        gh("v0.3.0", assets=("SpaceKnife-0.3.0-Setup-x64.exe",)),
        "junk",
    ]
    got = update.parse_releases(data)
    assert [r.version for r in got] == ["0.4.0-rc.1", "0.4.0-beta.1", "0.3.1", "0.3.0"]
    assert got[1].prerelease and not got[0].prerelease and got[0].preview
    assert got[2].published_at is None and got[2].notes == "notes"  # an odd date costs only the date
    assert got[3].published_at == datetime(2026, 10, 1, 12, tzinfo=UTC)
    assert got[3].assets[0].name == "SpaceKnife-0.3.0-Setup-x64.exe" and got[3].assets[0].size == 10
    with pytest.raises(ValueError, match="release list"):
        update.parse_releases({"message": "Not Found"})


def test_choose_by_channel() -> None:
    releases = update.parse_releases(
        [gh("v0.3.1"), gh("v0.4.0-beta.2", pre=True), gh("v0.4.0-beta.10", pre=True), gh("v0.3.0")]
    )
    preview = update.choose(releases, "preview")
    stable = update.choose(releases, "stable")
    assert preview is not None and preview.version == "0.4.0-beta.10"
    assert stable is not None and stable.version == "0.3.1"
    assert update.choose([], "preview") is None
    assert update.choose([r for r in releases if r.preview], "stable") is None
    # the cache keeps the history (the timeline), newest first; both channels' newest are in it
    assert [r.version for r in update.trim(releases)] == ["0.4.0-beta.10", "0.4.0-beta.2", "0.3.1", "0.3.0"]


def test_trim_keeps_the_newest_releases() -> None:
    many = update.parse_releases([gh(f"v0.{m}.{p}") for m in range(4) for p in range(12)] + [gh("nightly")])
    kept = update.trim(many)
    assert len(kept) == update.HISTORY_KEEP == 30
    assert kept[0].version == "0.3.11" and kept[-1].version == "0.1.6"
    assert all(r.parsed is not None for r in kept)  # a tag without a version is not a release here


def test_only_github_urls_are_kept() -> None:
    evil = gh("v0.4.0", html="https://example.com/phish", assets=("SpaceKnife-0.4.0-Setup-x64.exe",))
    evil["assets"] = [
        {"name": "SpaceKnife-0.4.0-Setup-x64.exe", "size": 5, "browser_download_url": "https://x.io/a"}
    ]
    (r,) = update.parse_releases([evil])
    assert r.url == "https://github.com/DuskXi/SpaceKnife/releases/tag/v0.4.0" and r.assets == []
    assert update.allowed_url("https://github.com/DuskXi/SpaceKnife/releases/download/v1/x.exe")
    assert not update.allowed_url("http://github.com/x")  # not https
    assert not update.allowed_url("https://github.com.evil.io/x")
    local = "http://127.0.0.1:8123/releases"
    assert update.allowed_url("http://127.0.0.1:8123/files/x.exe", local)  # next to a local test feed
    assert not update.allowed_url("http://127.0.0.1:9999/files/x.exe", local)


def test_notes_are_capped() -> None:
    long = "\n".join(f"- 修复第 {i} 个问题" for i in range(5000))
    (r,) = update.parse_releases([gh("v0.4.0", body=long)])
    assert len(r.notes.encode("utf-8")) <= update.NOTES_MAX + 4 and r.notes.endswith("\n…")
    assert update.cap_notes("short") == "short"


def test_to_info() -> None:
    r = release("0.4.0", "SpaceKnife-0.4.0-Setup-x64.exe")
    info = update.to_info(r, r.assets[0], "ab" * 32)
    assert info.version == "0.4.0" and info.tag == "v0.4.0" and info.name == "v0.4.0"
    assert info.asset is not None and info.asset.sha256 == "ab" * 32 and info.asset.size == 10
    assert update.to_info(r, None).asset is None


# ---------------------------------------------------------------- the file for this install

ALL_ASSETS = (
    "SpaceKnife-0.4.0-Setup-x64.exe",
    "SpaceKnife-0.4.0-Setup-x64.exe.blockmap",
    "SpaceKnife-0.4.0-win-x64-portable.zip",
    "SpaceKnife-0.4.0-win-arm64-portable.zip",
    "SpaceKnife-0.4.0-linux-x64-portable.tar.gz",
    "SpaceKnife-0.4.0-linux-arm64-portable.tar.gz",
    "SpaceKnife-0.4.0-x86_64.AppImage",
    "spaceknife_0.4.0_amd64.deb",
    "spaceknife-0.4.0.x86_64.rpm",
    "SpaceKnife-0.4.0-mac-arm64.dmg",
    "SpaceKnife-0.4.0-mac-x64.dmg",
    "latest.yml",
    "SHA256SUMS.txt",
    "SHA256SUMS-mac-arm64.txt",
    "SHA256SUMS-portable-win-x64.txt",
)


@pytest.mark.parametrize(
    ("kind", "system", "machine", "name"),
    [
        ("nsis", "win32", "AMD64", "SpaceKnife-0.4.0-Setup-x64.exe"),
        ("nsis", "win32", "ARM64", "SpaceKnife-0.4.0-Setup-x64.exe"),
        ("portable", "win32", "AMD64", "SpaceKnife-0.4.0-win-x64-portable.zip"),
        ("portable", "linux", "x86_64", "SpaceKnife-0.4.0-linux-x64-portable.tar.gz"),
        ("portable", "win32", "ARM64", "SpaceKnife-0.4.0-win-arm64-portable.zip"),
        ("portable", "linux", "aarch64", "SpaceKnife-0.4.0-linux-arm64-portable.tar.gz"),
        ("portable", "darwin", "arm64", None),  # a macOS app is portable already
        ("appimage", "linux", "x86_64", "SpaceKnife-0.4.0-x86_64.AppImage"),
        ("appimage", "linux", "aarch64", None),
        ("deb", "linux", "x86_64", "spaceknife_0.4.0_amd64.deb"),
        ("rpm", "linux", "x86_64", "spaceknife-0.4.0.x86_64.rpm"),
        ("dmg", "darwin", "arm64", "SpaceKnife-0.4.0-mac-arm64.dmg"),
        ("dmg", "darwin", "x86_64", "SpaceKnife-0.4.0-mac-x64.dmg"),
        ("dev", "win32", "AMD64", None),
        ("unknown", "linux", "x86_64", None),
    ],
)
def test_pick_asset(kind: InstallKind, system: str, machine: str, name: str | None) -> None:
    got = update.pick_asset(release("0.4.0", *ALL_ASSETS), kind, system, machine)
    assert (got.name if got else None) == name


def test_a_portable_copy_on_windows_on_arm_keeps_the_x64_build_without_an_arm_one() -> None:
    # a release before the ARM builds (or one whose ARM build failed): Windows runs x64 emulated
    r = release(
        "0.4.0", "SpaceKnife-0.4.0-win-x64-portable.zip", "SpaceKnife-0.4.0-linux-x64-portable.tar.gz"
    )
    got = update.pick_asset(r, "portable", "win32", "ARM64")
    assert got is not None and got.name == "SpaceKnife-0.4.0-win-x64-portable.zip"
    assert update.pick_asset(r, "portable", "linux", "aarch64") is None  # Linux runs no x64 on ARM


def test_the_arm_portable_files_are_update_files() -> None:
    for name in ("SpaceKnife-0.4.0-win-arm64-portable.zip", "SpaceKnife-0.4.0-linux-arm64-portable.tar.gz"):
        assert str(update.update_file_version(name)) == "0.4.0"


def test_pick_asset_tolerates_the_packagers_spelling() -> None:
    r = release("0.4.0-beta.1", "spaceknife-0.4.0~beta.1.x86_64.rpm", "spaceknife_0.4.0-beta.1_amd64.deb")
    rpm = update.pick_asset(r, "rpm", "linux", "x86_64")
    deb = update.pick_asset(r, "deb", "linux", "x86_64")
    assert rpm is not None and rpm.name == "spaceknife-0.4.0~beta.1.x86_64.rpm"
    assert deb is not None and deb.name == "spaceknife_0.4.0-beta.1_amd64.deb"
    assert (
        update.pick_asset(release("0.4.0", "SpaceKnife-../../x-Setup-x64.exe"), "nsis", "win32", "AMD64")
        is None
    )
    assert update.pick_asset(release("0.4.0"), "nsis", "win32", "AMD64") is None  # nothing published


# ---------------------------------------------------------------- checksums


def test_parse_checksums() -> None:
    a, b, c = "a" * 64, "B" * 64, "c" * 64
    text = "\n".join(
        [
            f"{a}  SpaceKnife-0.4.0-Setup-x64.exe",
            f"{b} *spaceknife_0.4.0_amd64.deb",  # binary mode marker
            f"{c}  dist/electron/SpaceKnife-0.4.0-mac-arm64.dmg",
            "not a checksum line",
            "1234  short.txt",
            "",
        ]
    )
    assert update.parse_checksums(text) == {
        "SpaceKnife-0.4.0-Setup-x64.exe": a,
        "spaceknife_0.4.0_amd64.deb": "b" * 64,
        "SpaceKnife-0.4.0-mac-arm64.dmg": c,
    }
    assert update.is_checksum_file("SHA256SUMS.txt") and update.is_checksum_file("SHA256SUMS-mac-x64.txt")
    assert not update.is_checksum_file("SHA256SUMS.txt.sig") and not update.is_checksum_file("notes.txt")


def test_checksum_files_likeliest_first() -> None:
    r = release("0.4.0", *ALL_ASSETS)

    def order(name: str) -> list[str]:
        return [a.name for a in update.checksum_files(r, Asset(name=name, url="u"))]

    assert order("SpaceKnife-0.4.0-mac-arm64.dmg")[0] == "SHA256SUMS-mac-arm64.txt"
    assert order("SpaceKnife-0.4.0-win-x64-portable.zip")[0] == "SHA256SUMS-portable-win-x64.txt"
    assert order("SpaceKnife-0.4.0-Setup-x64.exe")[0] == "SHA256SUMS.txt"
    assert len(order("x")) == 3


# ---------------------------------------------------------------- environment


def test_environment(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    for var in ("SPACEKNIFE_APP_VERSION", "SPACEKNIFE_INSTALL_KIND", "SPACEKNIFE_UPDATE_FEED"):
        monkeypatch.delenv(var, raising=False)
    assert update.app_version() is None and update.install_kind() == "dev"
    assert update.feed_url() == update.RELEASES_URL
    monkeypatch.setenv("SPACEKNIFE_APP_VERSION", " 0.3.0 ")
    monkeypatch.setenv("SPACEKNIFE_INSTALL_KIND", "NSIS")
    assert update.app_version() == "0.3.0" and update.install_kind() == "nsis"
    monkeypatch.setenv("SPACEKNIFE_INSTALL_KIND", "snap")
    assert update.install_kind() == "unknown"
    monkeypatch.setenv("SPACEKNIFE_UPDATE_FEED", "http://127.0.0.1:8123/releases")
    assert update.feed_url() == "http://127.0.0.1:8123/releases"
    monkeypatch.setenv("SPACEKNIFE_UPDATE_FEED", "https://example.com/releases")  # never another host
    assert update.feed_url() == update.RELEASES_URL
    monkeypatch.setenv("SPACEKNIFE_HOME", str(tmp_path / "home"))
    assert update.updates_dir() == tmp_path / "home" / "data" / "updates"  # never the download folder


def test_update_file_versions() -> None:
    v = update.update_file_version
    assert str(v("SpaceKnife-0.3.2-Setup-x64.exe")) == "0.3.2"
    assert str(v("SpaceKnife-0.4.0-beta.1-win-x64-portable.zip")) == "0.4.0-beta.1"
    assert str(v("SpaceKnife-0.3.10-linux-x64-portable.tar.gz")) == "0.3.10"
    assert str(v("SpaceKnife-0.3.2-x86_64.AppImage")) == "0.3.2"
    assert str(v("spaceknife_0.4.0~beta.1_amd64.deb")) == "0.4.0-beta.1"
    assert str(v("spaceknife-0.3.2.x86_64.rpm")) == "0.3.2"
    assert str(v("SpaceKnife-0.3.2-mac-arm64.dmg")) == "0.3.2"
    assert str(v("staging-0.3.3")) == "0.3.3" and str(v("previous-0.3.2")) == "0.3.2"
    assert str(v("update-0.3.3.ps1")) == str(v("update-0.3.3.sh")) == str(v("update-0.3.3.log")) == "0.3.3"
    for other in ("notes.txt", "SpaceKnife-Setup.exe", "staging-", "SpaceKnife-0.3.2-Setup-x64.exe.part"):
        assert v(other) is None


def test_clean_updates(tmp_path: Path) -> None:
    folder = tmp_path / "updates"
    folder.mkdir()
    names = [
        "SpaceKnife-0.3.1-Setup-x64.exe",  # older
        "SpaceKnife-0.3.2-Setup-x64.exe",  # the running version: it was just installed
        "SpaceKnife-0.3.3-Setup-x64.exe",  # newer, downloaded, not installed yet
        "SpaceKnife-0.3.3-Setup-x64.exe.part",  # an interrupted download
        "SpaceKnife-0.4.0-beta.1-x86_64.AppImage",  # a newer pre-release
        "update-0.3.2.json",  # the plan that put the running version in place, and its log
        "update-0.3.2.log",
        "readme.txt",  # not ours: stays
    ]
    for name in names:
        (folder / name).write_bytes(b"x")
    for name in ("staging-0.3.2", "previous-0.3.1", "staging-0.3.3"):
        (folder / name / "sub").mkdir(parents=True)
        (folder / name / "sub" / "f").write_bytes(b"x")
    removed = update.clean_updates(folder, "0.3.2")
    assert sorted(removed) == [
        "SpaceKnife-0.3.1-Setup-x64.exe",
        "SpaceKnife-0.3.2-Setup-x64.exe",
        "SpaceKnife-0.3.3-Setup-x64.exe.part",
        "previous-0.3.1",
        "staging-0.3.2",
        "update-0.3.2.json",
        "update-0.3.2.log",
    ]
    left = sorted(p.name for p in folder.iterdir())
    assert left == [
        "SpaceKnife-0.3.3-Setup-x64.exe",
        "SpaceKnife-0.4.0-beta.1-x86_64.AppImage",
        "readme.txt",
        "staging-0.3.3",
    ]
    # a run from source (no version) only drops partial downloads; a missing folder is no error
    (folder / "SpaceKnife-0.3.4-Setup-x64.exe.part").write_bytes(b"x")
    assert update.clean_updates(folder, None) == ["SpaceKnife-0.3.4-Setup-x64.exe.part"]
    assert update.clean_updates(tmp_path / "missing", "0.3.2") == []


# ---------------------------------------------------------------- state


def test_state_round_trip_and_damage(tmp_path: Path) -> None:
    file = tmp_path / "data" / "update.json"
    assert update.load_state(file) == update.UpdateState()
    state = update.UpdateState(
        checked_at=datetime(2026, 10, 20, 8, tzinfo=UTC),
        etag='W/"abc"',
        releases=[release("0.4.0", "SpaceKnife-0.4.0-Setup-x64.exe")],
        dismissed=["0.4.0"],
    )
    update.save_state(state, file)
    assert update.load_state(file) == state
    assert not file.with_name("update.json.tmp").exists()
    # another feed (a local test one): the cache goes, the closed cards stay
    other = update.load_state(file, "http://127.0.0.1:1/releases")
    assert other.releases == [] and other.etag is None and other.dismissed == ["0.4.0"]
    file.write_text("{ not json", encoding="utf-8")
    assert update.load_state(file) == update.UpdateState()
    file.write_text(json.dumps({"checked_at": "2026-10-20T08:00:00"}), encoding="utf-8")  # naive time
    assert update.load_state(file) == update.UpdateState()


# ---------------------------------------------------------------- the release list


def test_fetch_with_etag(feed: LocalFeed) -> None:
    body = json.dumps([gh("v0.4.0"), gh("v0.3.0")]).encode()

    def releases(headers: dict[str, str]) -> tuple[int, dict[str, str], bytes]:
        if headers.get("if-none-match") == '"v1"':
            return 304, {"ETag": '"v1"'}, b""
        return 200, {"ETag": '"v1"', "Content-Type": "application/json"}, body

    feed.routes["/releases"] = releases
    got = update.fetch_releases(feed.url("/releases"), None, "SpaceKnife/0.3.0")
    assert got.etag == '"v1"' and got.releases is not None
    assert [r.version for r in got.releases] == ["0.4.0", "0.3.0"]
    again = update.fetch_releases(feed.url("/releases"), '"v1"', "SpaceKnife/0.3.0")
    assert again.releases is None and again.etag == '"v1"'  # 304: the cache holds
    (_, first), (_, second) = feed.requests
    assert first["user-agent"] == "SpaceKnife/0.3.0" and "if-none-match" not in first
    assert second["if-none-match"] == '"v1"'
    assert not {"authorization", "cookie"} & set(first)  # nothing about the user


def test_fetch_errors(feed: LocalFeed) -> None:
    reset = str(int(time.time()) + 600)
    feed.routes["/limited"] = lambda _h: (
        403,
        {"X-RateLimit-Remaining": "0", "X-RateLimit-Reset": reset},
        b'{"message": "API rate limit exceeded"}',
    )
    feed.routes["/limited-plain"] = lambda _h: (
        403,
        {},
        b'{"message": "API rate limit exceeded for 1.2.3.4"}',
    )
    feed.routes["/forbidden"] = lambda _h: (403, {}, b"no")
    feed.routes["/broken"] = lambda _h: (500, {}, b"oops")
    feed.file("/garbage", b"<html>not json</html>")
    feed.file("/object", b'{"message": "Not Found"}')

    def error(path: str) -> UpdateError:
        with pytest.raises(UpdateError) as info:
            update.fetch_releases(feed.url(path), None, "SpaceKnife/0.3.0")
        return info.value

    limited = error("/limited")
    assert limited.key == "rate_until" and limited.fields["minutes"] in (10, 11)
    assert "10" in str(limited) or "11" in str(limited)
    assert error("/limited-plain").key == "rate"
    assert (error("/forbidden").key, error("/forbidden").fields) == ("http", {"code": 403})
    assert str(error("/broken")) == "GitHub 返回了错误：HTTP 500"
    assert error("/garbage").key == "bad_list" and error("/object").key == "bad_list"

    def slow(_headers: dict[str, str]) -> tuple[int, dict[str, str], bytes]:
        time.sleep(1.0)
        return 200, {}, b"[]"

    feed.routes["/slow"] = slow
    with pytest.raises(UpdateError) as info:
        update.fetch_releases(feed.url("/slow"), None, "SpaceKnife/0.3.0", timeout=0.2)
    assert info.value.key == "timeout" and str(info.value) == "连接 GitHub 超时"


def test_fetch_offline(no_proxy: None) -> None:
    with socket.socket() as s:  # a port nobody listens on
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
    with pytest.raises(UpdateError) as info:
        update.fetch_releases(f"http://127.0.0.1:{port}/releases", None, "SpaceKnife/0.3.0", timeout=5)
    assert info.value.key == "offline" and str(info.value).startswith("连不上 GitHub：")


def test_fetch_checksums(feed: LocalFeed) -> None:
    digest = "d" * 64
    feed.file("/SHA256SUMS.txt", f"{'e' * 64}  other.exe\n".encode())
    feed.file("/SHA256SUMS-mac-arm64.txt", f"{digest}  SpaceKnife-0.4.0-mac-arm64.dmg\n".encode())
    r = Release(
        version="0.4.0",
        tag="v0.4.0",
        url="u",
        assets=[
            Asset(name="SHA256SUMS.txt", url=feed.url("/SHA256SUMS.txt")),
            Asset(name="SHA256SUMS-mac-arm64.txt", url=feed.url("/SHA256SUMS-mac-arm64.txt")),
        ],
    )
    dmg = Asset(name="SpaceKnife-0.4.0-mac-arm64.dmg", url="u")
    assert update.fetch_checksums(r, dmg, "SpaceKnife/0.3.0") == digest
    assert feed.hits("/SHA256SUMS.txt") == 0  # the likeliest file had it
    assert update.fetch_checksums(r, Asset(name="none.exe", url="u"), "SpaceKnife/0.3.0") is None
    assert update.fetch_checksums(release("0.4.0"), dmg, "SpaceKnife/0.3.0") is None  # none published
    gone = r.model_copy(update={"assets": [Asset(name="SHA256SUMS.txt", url=feed.url("/missing"))]})
    with pytest.raises(UpdateError) as info:
        update.fetch_checksums(gone, dmg, "SpaceKnife/0.3.0")
    assert info.value.key == "sums" and "HTTP 404" in str(info.value)


# ---------------------------------------------------------------- download

PAYLOAD = os.urandom(3 * update.CHUNK + 1234)
DIGEST = hashlib.sha256(PAYLOAD).hexdigest()


def fetch_file(
    url: str, dest: Path, sha256: str | None, *, size: int = len(PAYLOAD), cancel_after: int | None = None
) -> tuple[bool | None, list[tuple[str, int, int]]]:
    seen: list[tuple[str, int, int]] = []
    cancel = threading.Event()

    def progress(state: str, received: int, total: int) -> None:
        seen.append((state, received, total))
        if cancel_after is not None and received >= cancel_after:
            cancel.set()

    verified = update.download(
        url, dest, size=size, sha256=sha256, user_agent="SpaceKnife/0.3.0", progress=progress, cancel=cancel
    )
    return verified, seen


def test_download_verified(feed: LocalFeed, tmp_path: Path) -> None:
    url = feed.file("/file.exe", PAYLOAD)
    dest = tmp_path / "dl" / "SpaceKnife-0.4.0-Setup-x64.exe"
    verified, seen = fetch_file(url, dest, DIGEST.upper())
    assert verified is True and dest.read_bytes() == PAYLOAD
    assert not dest.with_name(dest.name + ".part").exists()
    assert seen[0] == ("downloading", 0, len(PAYLOAD)) and seen[-1] == (
        "downloading",
        len(PAYLOAD),
        len(PAYLOAD),
    )
    assert len(seen) <= 4  # throttled: a quick download reports start, end and little between

    # already there with the right hash: kept, nothing fetched
    verified, seen = fetch_file(url, dest, DIGEST)
    assert verified is True and feed.hits("/file.exe") == 1 and seen[0][0] == "verifying"

    # no checksum published: downloaded again, not verified
    verified, _ = fetch_file(url, dest, None)
    assert verified is None and feed.hits("/file.exe") == 2 and dest.read_bytes() == PAYLOAD


def test_download_mismatch_is_deleted(feed: LocalFeed, tmp_path: Path) -> None:
    url = feed.file("/file.exe", PAYLOAD)
    dest = tmp_path / "SpaceKnife-0.4.0-Setup-x64.exe"
    with pytest.raises(UpdateError) as info:
        fetch_file(url, dest, "0" * 64)
    assert info.value.key == "mismatch"
    assert list(tmp_path.iterdir()) == []  # neither the file nor its .part


def test_download_cancel_and_failures(feed: LocalFeed, tmp_path: Path) -> None:
    url = feed.file("/file.exe", PAYLOAD)
    dest = tmp_path / "SpaceKnife-0.4.0-Setup-x64.exe"
    with pytest.raises(update.Cancelled):
        fetch_file(url, dest, DIGEST, cancel_after=1)
    assert list(tmp_path.iterdir()) == []

    with pytest.raises(UpdateError) as info:
        fetch_file(feed.url("/missing.exe"), dest, DIGEST)
    assert info.value.key == "download_http" and info.value.fields == {"code": 404}

    short = feed.file("/short.exe", PAYLOAD[:1000], headers={})  # no length: ends when the server closes
    with pytest.raises(UpdateError) as info:
        fetch_file(short, dest, None, size=len(PAYLOAD))
    assert info.value.key == "incomplete" and info.value.fields["received"] == 1000
    assert list(tmp_path.iterdir()) == []


# ---------------------------------------------------------------- palette


def test_palette_commands() -> None:
    by_id = {c.id: c for c in registry()}
    check, show = by_id["app.check_updates"], by_id["app.show_update"]
    assert not check.pinned and not show.pinned and not check.typed_only
    assert evaluate(check, Context(tab="start")) == (True, "")
    assert evaluate(check, Context(tab="start", app_version="0.3.0")) == (True, "当前版本 0.3.0")
    assert evaluate(check, Context(tab="start", update_checking=True)) == (True, "正在检查…")
    assert evaluate(check, Context(tab="scan", update_version="0.4.0")) == (True, "有新版本 0.4.0")
    assert evaluate(show, Context(tab="start")) == (False, "没有可用的更新")
    assert evaluate(show, Context(tab="settings", update_version="0.4.0")) == (True, "0.4.0")
    about = by_id["app.about"]
    assert evaluate(about, Context(tab="scan", app_version="0.3.3")) == (True, "0.3.3")
