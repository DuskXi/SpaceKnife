"""update.* over the real WebSocket: checks with a fake release list, update.changed only to sessions
with updates.v1, closing a card, the verified download (local HTTP server), and the schedule."""

from __future__ import annotations

import asyncio
import hashlib
import json
import threading
from collections.abc import Callable
from datetime import UTC, datetime, timedelta
from pathlib import Path

import pytest

from spaceknife import update
from spaceknife.logbus import LogHub
from spaceknife.server import Server, ServerConfig
from spaceknife.server.update_handlers import UpdateService
from spaceknife.update import Asset, Fetched, Release, UpdateError
from spaceknife_client.connection import Connection, RemoteError
from spaceknife_protocol import ALL_CAPABILITIES, CAP_UPDATES
from spaceknife_protocol import palette as pp
from spaceknife_protocol.base import Empty
from spaceknife_protocol.errors import ErrorCode
from spaceknife_protocol.methods import (
    EVENT_UPDATE,
    PALETTE_QUERY,
    UPDATE_CANCEL,
    UPDATE_CHECK,
    UPDATE_DISMISS,
    UPDATE_DOWNLOAD,
    UPDATE_STATUS,
)
from spaceknife_protocol.session import ClientInfo
from spaceknife_protocol.update import UpdateChangedEvent, UpdateStatus, UpdateVersionParams

from .conftest import CLIENT, TOKEN
from .test_update import LocalFeed

UI = ClientInfo(name="pytest-ui", version="0.3.0", kind="test")
OLD_CAPS = [c for c in ALL_CAPABILITIES if c != CAP_UPDATES]
PAGE = "https://github.com/DuskXi/SpaceKnife/releases"


@pytest.fixture(autouse=True)
def isolated(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("SPACEKNIFE_HOME", str(tmp_path / "home"))
    monkeypatch.setenv("SPACEKNIFE_DOWNLOAD_DIR", str(tmp_path / "downloads"))
    monkeypatch.setenv("no_proxy", "*")
    monkeypatch.setenv("NO_PROXY", "*")
    for var in ("SPACEKNIFE_APP_VERSION", "SPACEKNIFE_INSTALL_KIND", "SPACEKNIFE_UPDATE_FEED"):
        monkeypatch.delenv(var, raising=False)


class FakeGitHub:
    """The release list the service sees (called in a daemon thread)."""

    def __init__(self) -> None:
        self.releases: list[Release] = []
        self.error: UpdateError | None = None
        self.calls = 0
        self.agents: list[str] = []
        self.etags: list[str | None] = []

    def __call__(self, feed: str, etag: str | None, user_agent: str) -> Fetched:
        self.calls += 1
        self.agents.append(user_agent)
        self.etags.append(etag)
        if self.error is not None:
            raise self.error
        return Fetched(list(self.releases), f'"{self.calls}"')


def rel(version: str, *assets: Asset, prerelease: bool = False, notes: str = "") -> Release:
    return Release(
        version=version,
        tag=f"v{version}",
        name=f"SpaceKnife {version}",
        url=f"{PAGE}/tag/v{version}",
        prerelease=prerelease,
        notes=notes,
        assets=list(assets),
    )


def installer(version: str) -> Asset:
    name = f"SpaceKnife-{version}-Setup-x64.exe"
    return Asset(name=name, size=10, url=f"{PAGE}/download/v{version}/{name}")


async def until(cond: Callable[[], bool], within: float = 10.0) -> None:
    loop = asyncio.get_running_loop()
    end = loop.time() + within
    while not cond():
        if loop.time() > end:
            raise AssertionError("condition not reached in time")
        await asyncio.sleep(0.01)


def fake(server: Server) -> FakeGitHub:
    gh = FakeGitHub()
    server.updates.fetch = gh
    server.updates.kind = "nsis"
    return gh


async def test_check_levels_events_and_dismiss(server: Server) -> None:
    gh = fake(server)
    gh.releases = [rel("0.3.1", installer("0.3.1"), notes="- 修复")]
    events: list[UpdateChangedEvent] = []
    old_events: list[UpdateChangedEvent] = []
    async with (
        await Connection.open(server.url, TOKEN, UI) as ui,
        await Connection.open(server.url, TOKEN, CLIENT, capabilities=OLD_CAPS) as old,
    ):
        ui.on_event(EVENT_UPDATE, events.append)
        old.on_event(EVENT_UPDATE, old_events.append)

        st = await ui.call(UPDATE_STATUS, Empty())
        assert (st.current, st.install_kind, st.enabled, st.channel) == ("0.3.0", "nsis", True, "preview")
        assert st.level == "none" and st.latest is None and st.checked_at is None and not st.checking

        # patch: the badge only (the front end decides; the backend says how far behind)
        st = await ui.call(UPDATE_CHECK, Empty())
        assert st.level == "patch" and st.latest is not None and st.latest.version == "0.3.1"
        assert st.latest.notes == "- 修复" and st.latest.url == f"{PAGE}/tag/v0.3.1"
        assert st.latest.asset is not None and st.latest.asset.name == "SpaceKnife-0.3.1-Setup-x64.exe"
        assert st.checked_at is not None and st.error is None and not st.dismissed
        assert gh.agents == ["SpaceKnife/0.3.0"] and gh.etags == [None]
        await until(lambda: len(events) >= 2 and events[-1].status.level == "patch")
        assert events[0].status.checking and not events[-1].status.checking

        # minor, then closing its card; the next check sends the cached list's ETag
        gh.releases = [rel("0.4.0", installer("0.4.0"))]
        st = await ui.call(UPDATE_CHECK, Empty())
        assert st.level == "minor" and gh.etags[-1] == '"1"'
        st = await ui.call(UPDATE_DISMISS, UpdateVersionParams(version="0.4.0"))
        assert st.dismissed and st.level == "minor"
        saved = json.loads(server.updates.file.read_text(encoding="utf-8"))
        assert saved["dismissed"] == ["0.4.0"] and saved["etag"] == '"2"'
        await until(lambda: events[-1].status.dismissed)

        # major: a new version, so a new card
        gh.releases = [rel("1.0.0", installer("1.0.0")), rel("0.4.0")]
        st = await ui.call(UPDATE_CHECK, Empty())
        assert st.level == "major" and st.latest is not None and st.latest.version == "1.0.0"
        assert not st.dismissed

        # the stable channel skips a newer pre-release (from the cache, no new request)
        gh.releases = [rel("1.1.0-beta.1", prerelease=True), rel("1.0.0")]
        st = await ui.call(UPDATE_CHECK, Empty())
        assert st.latest is not None and st.latest.version == "1.1.0-beta.1"
        calls = gh.calls
        server.settings.update({"general.update_channel": "stable"})  # no scheduler in tests
        st = await ui.call(UPDATE_STATUS, Empty())
        assert st.channel == "stable" and st.latest is not None and st.latest.version == "1.0.0"
        assert gh.calls == calls

        # a failed check keeps what the last one found and says why
        gh.error = UpdateError("rate")
        st = await ui.call(UPDATE_CHECK, Empty())
        assert st.error == "GitHub 暂时限制了查询次数，请过一会儿再试" and st.latest is not None

        # the palette knows
        res = await ui.call(
            PALETTE_QUERY, pp.PaletteQueryParams(text="jcgx", context=pp.PaletteContext(tab="start"))
        )
        cmd = next(c for c in res.commands if c.id == "app.check_updates")
        assert cmd.enabled and cmd.status == "有新版本 1.0.0"

        await asyncio.sleep(0.1)
        assert old_events == []  # no updates.v1, no update.changed

        with pytest.raises(RemoteError) as info:
            await ui.call(UPDATE_DOWNLOAD, UpdateVersionParams(version="0.9.0"))
        assert info.value.code == ErrorCode.INVALID_PARAMS


async def test_download_over_the_server(server: Server, tmp_path: Path) -> None:
    payload = b"installer bytes " * 50_000
    digest = hashlib.sha256(payload).hexdigest()
    gh = fake(server)
    with LocalFeed() as feed:
        exe = "SpaceKnife-0.4.0-Setup-x64.exe"
        sums = feed.file("/SHA256SUMS.txt", f"{digest}  {exe}\n".encode())
        gh.releases = [
            rel(
                "0.4.0",
                Asset(name=exe, size=len(payload), url=feed.file(f"/{exe}", payload)),
                Asset(name="SHA256SUMS.txt", size=100, url=sums),
            )
        ]
        events: list[UpdateChangedEvent] = []
        async with await Connection.open(server.url, TOKEN, UI) as ui:
            ui.on_event(EVENT_UPDATE, events.append)
            await ui.call(UPDATE_CHECK, Empty())
            st = await ui.call(UPDATE_DOWNLOAD, UpdateVersionParams(version="0.4.0"))
            assert st.download.state in ("downloading", "done") and st.download.version == "0.4.0"
            await until(lambda: bool(events) and events[-1].status.download.state == "done")
            done = events[-1].status
            assert done.download.verified is True and done.download.received == len(payload)
            assert done.download.path == str(tmp_path / "downloads" / exe)
            assert (tmp_path / "downloads" / exe).read_bytes() == payload
            assert done.latest is not None and done.latest.asset is not None
            assert done.latest.asset.sha256 == digest

            # asked again: the file is there, nothing is fetched
            st = await ui.call(UPDATE_DOWNLOAD, UpdateVersionParams(version="0.4.0"))
            assert st.download.state == "done" and feed.hits(f"/{exe}") == 1
            assert (await ui.call(UPDATE_CANCEL, Empty())).download.state == "done"  # nothing running

            # no file for this kind of install: the release page only
            server.updates.kind = "dev"
            with pytest.raises(RemoteError) as info:
                await ui.call(UPDATE_DOWNLOAD, UpdateVersionParams(version="0.4.0"))
            assert info.value.code == ErrorCode.INVALID_REQUEST


def published(feed: LocalFeed, version: str, payload: bytes, sums: str) -> Release:
    exe = f"SpaceKnife-{version}-Setup-x64.exe"
    return rel(
        version,
        Asset(name=exe, size=len(payload), url=feed.file(f"/{version}/{exe}", payload)),
        Asset(name="SHA256SUMS.txt", size=100, url=feed.file(f"/{version}/SHA256SUMS.txt", sums.encode())),
    )


async def test_download_mismatch_and_cancel(server: Server, tmp_path: Path) -> None:
    payload = b"x" * (3 * update.CHUNK)
    downloads = tmp_path / "downloads"
    gh = fake(server)
    go_on = threading.Event()
    with LocalFeed() as feed:
        events: list[UpdateChangedEvent] = []
        async with await Connection.open(server.url, TOKEN, UI) as ui:
            ui.on_event(EVENT_UPDATE, events.append)
            # the published checksum does not match: nothing is kept
            gh.releases = [published(feed, "0.4.0", payload, f"{'0' * 64}  SpaceKnife-0.4.0-Setup-x64.exe")]
            await ui.call(UPDATE_CHECK, Empty())
            await ui.call(UPDATE_DOWNLOAD, UpdateVersionParams(version="0.4.0"))
            await until(lambda: bool(events) and events[-1].status.download.state == "failed")
            failed = events[-1].status.download
            assert failed.error == "下载的文件与发布的 SHA-256 校验值不符，已删除" and failed.path is None
            assert not any(downloads.iterdir())

            # a download that stalls before its first byte, cancelled meanwhile
            gh.releases = [published(feed, "0.4.1", payload, "")]  # no checksum for it
            exe = "/0.4.1/SpaceKnife-0.4.1-Setup-x64.exe"

            def stalled(_headers: dict[str, str]) -> tuple[int, dict[str, str], bytes]:
                go_on.wait(10)
                return 200, {"Content-Length": str(len(payload))}, payload

            feed.routes[exe] = stalled
            st = await ui.call(UPDATE_CHECK, Empty())
            assert st.download.state == "idle"  # another release now: the failed download is forgotten
            st = await ui.call(UPDATE_DOWNLOAD, UpdateVersionParams(version="0.4.1"))
            assert st.download.state == "downloading"
            await until(lambda: feed.hits(exe) == 1)
            st = await ui.call(UPDATE_CANCEL, Empty())
            assert st.download.state == "cancelled"
            go_on.set()
            task = server.updates._download_task
            assert task is not None
            await until(task.done)  # the thread saw the cancel and removed its .part
            assert not any(downloads.iterdir())
            assert (await ui.call(UPDATE_STATUS, Empty())).download.state == "cancelled"


async def test_schedule(server: Server) -> None:
    svc = server.updates
    gh = fake(server)
    gh.releases = [rel("0.4.0")]
    now = [datetime(2026, 10, 20, 12, tzinfo=UTC)]
    svc.clock = lambda: now[0]
    svc.first_delay = 0.05
    hour = timedelta(hours=1)

    async def settle() -> None:  # let the scheduler look at the clock
        svc.wake()
        await asyncio.sleep(0.15)

    async def checked(calls: int) -> None:  # the check is over, its time recorded
        await until(lambda: gh.calls == calls and not svc.status().checking)

    # never checked: the first round checks
    svc.start()
    await checked(1)
    assert svc.due_in() == 24 * 3600

    # within the day: nothing; a day after the last success: checks
    now[0] += 23 * hour
    await settle()
    assert gh.calls == 1
    now[0] += hour
    svc.wake()
    await checked(2)

    # a failure is retried after 6 hours, not sooner
    gh.error = UpdateError("offline", reason="test")
    now[0] += 24 * hour
    svc.wake()
    await checked(3)
    assert svc.due_in() == 6 * 3600 and svc.status().error == "连不上 GitHub：test"
    now[0] += 5 * hour
    await settle()
    assert gh.calls == 3
    now[0] += hour
    svc.wake()
    await checked(4)
    gh.error = None

    # off: never, however long ago
    server.settings.update({"general.update_check": False})
    now[0] += 72 * hour
    await settle()
    assert gh.calls == 4 and svc.due_in() is None

    # switched on: checks (it is due); switched on again or another channel: checks right away,
    # due or not
    server.settings.update({"general.update_check": True})
    await checked(5)
    await settle()
    assert gh.calls == 5 and svc.due_in() == 24 * 3600
    server.settings.update({"general.update_check": False})
    server.settings.update({"general.update_check": True})
    await checked(6)
    server.settings.update({"general.update_channel": "stable"})
    await checked(7)
    await settle()
    assert gh.calls == 7 and svc.due_in() == 24 * 3600


async def test_the_real_app_schedules_and_stops(tmp_path: Path) -> None:
    gh = FakeGitHub()
    srv = Server(
        ServerConfig(token=TOKEN, ping_interval=None, log_file=False, update_checks=True), log_hub=LogHub()
    )
    srv.updates.fetch = gh
    await srv.start()
    try:
        scheduler = srv.updates._scheduler
        assert scheduler is not None and not scheduler.done()
    finally:
        await srv.close()
    assert scheduler.cancelled() and gh.calls == 0  # the first check waits a minute
    plain = Server(ServerConfig(token=TOKEN, ping_interval=None), log_hub=LogHub())
    await plain.start()
    try:
        assert plain.updates._scheduler is None  # tests and tools: no automatic checks
    finally:
        await plain.close()


async def test_state_survives_a_restart(server: Server) -> None:
    gh = fake(server)
    gh.releases = [rel("0.4.0")]
    async with await Connection.open(server.url, TOKEN, UI) as ui:
        await ui.call(UPDATE_CHECK, Empty())
        await ui.call(UPDATE_DISMISS, UpdateVersionParams(version="0.4.0"))
    again = UpdateService(server)  # what the next start reads
    again.app_version = "0.3.0"
    st: UpdateStatus = again.status()
    assert st.level == "minor" and st.dismissed and st.checked_at == server.updates.state.checked_at
    assert again.state.etag == '"1"' and again.due_in() == pytest.approx(24 * 3600, abs=60)
