"""Handlers for update.*: the update checks, their schedule and the one-click download (``updates.v1``).

:class:`UpdateService` belongs to the server, one per backend and shared by every session. It keeps
the state between runs (:mod:`spaceknife.update`), runs the checks and the download, and sends
``update.changed`` to every session that announced ``updates.v1`` whenever any of it changes
(a check starts or ends, the download moves on, a card is closed, a setting changes).

Schedule (「自动检查更新」 on): the first automatic check comes :data:`FIRST_DELAY_S` after start,
and only when the last successful one is older than :data:`update.CHECK_INTERVAL`; then once per
interval; a failed check is tried again after :data:`update.RETRY_INTERVAL`. Switching the setting
on or changing the channel checks right away (an unchanged list costs nothing: ETag). ``update.check``
always checks, also with the setting off.

The network calls run in daemon threads (:func:`in_thread`): one stuck on a slow connection never
holds up the backend's exit, and its partial download is deleted once it notices the cancel.
"""

from __future__ import annotations

import asyncio
import contextlib
import functools
import os
import threading
from collections.abc import Callable
from datetime import UTC, datetime
from pathlib import Path
from typing import TYPE_CHECKING, Any

from loguru import logger

from spaceknife_protocol import CAP_UPDATES
from spaceknife_protocol.base import Empty
from spaceknife_protocol.errors import ErrorCode
from spaceknife_protocol.methods import (
    EVENT_UPDATE,
    UPDATE_CANCEL,
    UPDATE_CHECK,
    UPDATE_DISMISS,
    UPDATE_DOWNLOAD,
    UPDATE_STATUS,
)
from spaceknife_protocol.update import (
    UpdateChangedEvent,
    UpdateChannel,
    UpdateDownload,
    UpdateStatus,
    UpdateVersionParams,
)

from .. import __version__, i18n, update
from ..i18n import Texts
from .rpc import RpcError

if TYPE_CHECKING:
    from ..settings import SettingsStore
    from .app import Server
    from .session import Session

FIRST_DELAY_S = 60.0  # after start, before the first automatic check
ERROR_PAUSE_S = 6 * 3600.0  # the scheduler itself failed (a bug): wait this long, then go on

TEXT = Texts(
    {
        "not_latest": "{version} 不是现在可以更新到的版本",
        "no_asset": "这个版本没有适合这种安装方式的文件，请到发布页下载",
    }
)

Fetch = Callable[[str, str | None, str], update.Fetched]  # (feed, etag, user agent)
Checksum = Callable[[update.Release, update.Asset, str], str | None]  # (release, asset, user agent)


async def in_thread[T](fn: Callable[[], T], name: str) -> T:
    """``fn()`` in a daemon thread (blocking network I/O). Unlike :func:`asyncio.to_thread` the
    backend never waits for it at exit; cancelling the caller abandons the call."""
    loop = asyncio.get_running_loop()
    fut: asyncio.Future[T] = loop.create_future()

    def settle(ok: bool, value: Any) -> None:
        if fut.done():
            return
        if ok:
            fut.set_result(value)
        else:
            fut.set_exception(value)

    def target() -> None:
        value: Any
        try:
            value, ok = fn(), True
        except BaseException as exc:
            value, ok = exc, False
        with contextlib.suppress(RuntimeError):  # the loop is gone: nobody waits any more
            loop.call_soon_threadsafe(settle, ok, value)

    threading.Thread(target=target, name=name, daemon=True).start()
    return await fut


class UpdateService:
    def __init__(self, server: Server, file: Path | None = None) -> None:
        self.server = server
        self.file = file or update.state_file()
        self.feed = update.feed_url()
        self.kind = update.install_kind()
        self.app_version = update.app_version()  # None: the first updates.v1 session's version
        self.state = update.load_state(self.file, self.feed)
        # seams for tests: the blocking calls (run in a daemon thread) and the clock
        self.fetch: Fetch = update.fetch_releases
        self.fetch_checksum: Checksum = update.fetch_checksums
        self.clock: Callable[[], datetime] = lambda: datetime.now(UTC)
        self.first_delay = FIRST_DELAY_S
        self._error: update.UpdateError | None = None  # of the last check, None after a success
        self._check: asyncio.Task[None] | None = None
        self._scheduler: asyncio.Task[None] | None = None
        self._wake = asyncio.Event()
        self._force = False  # the next scheduler round checks even if not due
        self._applied = self._settings_key()
        self._save_lock = asyncio.Lock()
        self._download = UpdateDownload()
        self._download_error: update.UpdateError | None = None
        self._download_task: asyncio.Task[None] | None = None
        self._cancel = threading.Event()
        self._run = 0  # the download whose progress counts (a cancelled one stops reporting)
        self._sums: dict[tuple[str, str], str | None] = {}  # (version, file) -> published SHA-256

    # ------------------------------------------------------------- state

    @property
    def enabled(self) -> bool:
        return bool(self.server.settings.get("general.update_check"))

    @property
    def channel(self) -> UpdateChannel:
        return "stable" if self.server.settings.get("general.update_channel") == "stable" else "preview"

    def _settings_key(self) -> tuple[bool, str, str]:
        return self.enabled, self.channel, i18n.language()

    def current(self) -> str | None:
        """The running app's version: the launcher's, else what the first session that announced
        ``updates.v1`` said in its hello."""
        if self.app_version is None:
            for session in list(self.server.sessions):
                if CAP_UPDATES in session.capabilities and session.client is not None:
                    self.app_version = session.client.version
                    break
        return self.app_version

    def user_agent(self) -> str:
        return f"SpaceKnife/{self.current() or __version__}"

    def newer(self) -> tuple[update.Release, update.Asset | None] | None:
        """The newest release of the channel when it is newer than this app, and its file for
        this kind of install."""
        release = update.choose(self.state.releases, self.channel)
        if release is None or update.level(self.current(), release.version) == "none":
            return None
        return release, update.pick_asset(release, self.kind)

    def status(self) -> UpdateStatus:
        found = self.newer()
        latest = None
        if found is not None:
            release, asset = found
            sha = self._sums.get((release.version, asset.name)) if asset is not None else None
            latest = update.to_info(release, asset, sha)
        download = self._download
        if self._download_error is not None:
            download = download.model_copy(update={"error": str(self._download_error)})
        current = self.current()
        return UpdateStatus(
            current=current or "",
            install_kind=self.kind,
            enabled=self.enabled,
            channel=self.channel,
            checking=self._check is not None,
            checked_at=self.state.checked_at,
            error=str(self._error) if self._error is not None else None,
            level=update.level(current, latest.version) if latest is not None else "none",
            latest=latest,
            dismissed=latest is not None and latest.version in self.state.dismissed,
            download=download,
        )

    def broadcast(self) -> None:
        """``update.changed`` to every session that announced ``updates.v1``."""
        sessions = [s for s in list(self.server.sessions) if CAP_UPDATES in s.capabilities]
        if not sessions:
            return
        event = UpdateChangedEvent(status=self.status())
        for session in sessions:
            session.emit(EVENT_UPDATE, event)

    async def _save(self) -> None:
        text = self.state.model_dump_json(indent=1)  # on the loop: the state is not shared with threads
        async with self._save_lock:
            try:
                await asyncio.to_thread(update.write_atomic, self.file, text)
            except OSError as exc:
                logger.warning("could not save the update state to {}: {}", self.file, exc)

    # ------------------------------------------------------------- check

    async def check(self) -> UpdateStatus:
        """Check the release list now (joins a check that is already running)."""
        if self._check is None:
            self._check = asyncio.create_task(self._run_check(), name="update-check")
            self.broadcast()  # checking
        await asyncio.shield(self._check)
        return self.status()

    async def _run_check(self) -> None:
        st = self.state
        try:
            fetch = functools.partial(
                self.fetch, self.feed, st.etag if st.releases else None, self.user_agent()
            )
            got = await in_thread(fetch, "update-check")
        except update.UpdateError as exc:
            self._error, st.failed_at = exc, self.clock()
            logger.warning("update check failed: {}", exc)
        except Exception as exc:
            self._error, st.failed_at = update.UpdateError("failed", reason=exc), self.clock()
            logger.opt(exception=exc).warning("update check failed")
        else:
            if got.releases is not None:
                st.releases, st.etag = update.trim(got.releases), got.etag
            st.checked_at, st.failed_at, self._error = self.clock(), None, None
        finally:
            self._check = None
        found = self.newer()
        if found is not None:
            logger.info("update available: {} -> {}", self.current(), found[0].version)
        d = self._download
        idle = d.state not in ("downloading", "verifying")
        if idle and d.version is not None and (found is None or found[0].version != d.version):
            self._download, self._download_error = UpdateDownload(), None  # another release now
        await self._save()
        self.broadcast()

    async def dismiss(self, version: str) -> UpdateStatus:
        """The card / dialog of ``version`` was closed: not shown again for it (the badge stays)."""
        closed = self.state.dismissed
        if version not in closed:
            closed.append(version)
            del closed[: -update.KEEP_DISMISSED]
            await self._save()
            self.broadcast()
        return self.status()

    # ------------------------------------------------------------- download

    async def download(self, version: str) -> UpdateStatus:
        """Start downloading ``version``'s file for this install (progress: ``update.changed``)."""
        found = self.newer()
        if found is None or found[0].version != version:
            raise RpcError(ErrorCode.INVALID_PARAMS, TEXT["not_latest"].format(version=version))
        release, asset = found
        if asset is None:
            raise RpcError(ErrorCode.INVALID_REQUEST, TEXT["no_asset"])
        d = self._download
        if d.version == version and d.state in ("downloading", "verifying"):
            return self.status()
        if (
            d.version == version
            and d.state == "done"
            and d.path
            and await asyncio.to_thread(os.path.isfile, d.path)
        ):
            return self.status()
        self._cancel.set()  # a download of another version (only if the list changed meanwhile)
        previous = self._download_task
        self._run += 1
        self._cancel = threading.Event()
        self._download = UpdateDownload(state="downloading", version=version, total=asset.size)
        self._download_error = None
        self._download_task = asyncio.create_task(
            self._run_download(self._run, release, asset, self._cancel, previous), name="update-download"
        )
        self.broadcast()
        return self.status()

    def _progress(self, run: int, state: str, received: int, total: int) -> None:
        if run != self._run:
            return
        self._download = self._download.model_copy(
            update={"state": state, "received": received, "total": total}
        )
        self.broadcast()

    async def _run_download(
        self,
        run: int,
        release: update.Release,
        asset: update.Asset,
        cancel: threading.Event,
        previous: asyncio.Task[None] | None,
    ) -> None:
        loop = asyncio.get_running_loop()
        if previous is not None:
            await asyncio.wait({previous})  # its partial file is gone before this one starts

        def progress(state: str, received: int, total: int) -> None:
            with contextlib.suppress(RuntimeError):  # the loop is closed (shutdown)
                loop.call_soon_threadsafe(self._progress, run, state, received, total)

        version = release.version
        dest = update.download_dir() / asset.name
        error: update.UpdateError | None = None
        try:
            key = (version, asset.name)
            if key not in self._sums:
                fetch = functools.partial(self.fetch_checksum, release, asset, self.user_agent())
                self._sums[key] = await in_thread(fetch, "update-checksums")
            get = functools.partial(
                update.download,
                asset.url,
                dest,
                size=asset.size,
                sha256=self._sums[key],
                user_agent=self.user_agent(),
                progress=progress,
                cancel=cancel,
            )
            verified = await in_thread(get, "update-download")
        except update.Cancelled:
            outcome = UpdateDownload(state="cancelled", version=version)
        except update.UpdateError as exc:
            outcome, error = UpdateDownload(state="failed", version=version), exc
            logger.warning("update download failed: {}", exc)
        except Exception as exc:
            outcome = UpdateDownload(state="failed", version=version)
            error = update.UpdateError("download_failed", reason=exc)
            logger.opt(exception=exc).warning("update download failed")
        else:
            size = self._download.total or asset.size
            outcome = UpdateDownload(
                state="done", version=version, received=size, total=size, path=str(dest), verified=verified
            )
            logger.info("update {} downloaded to {} (verified: {})", version, dest, verified)
        if run == self._run:
            self._download, self._download_error = outcome, error
            self.broadcast()

    async def cancel(self) -> UpdateStatus:
        """Stop the running download (its partial file is deleted)."""
        d = self._download
        if d.state in ("downloading", "verifying"):
            self._cancel.set()
            self._run += 1  # the stopped one no longer reports
            self._download = d.model_copy(update={"state": "cancelled"})
            self.broadcast()
        return self.status()

    # ------------------------------------------------------------- schedule

    def due_in(self) -> float | None:
        """Seconds until the next automatic check (0: now); None while automatic checks are off."""
        if not self.enabled:
            return None
        now = self.clock()
        st = self.state
        last = st.checked_at
        due = now if last is None or last > now else last + update.CHECK_INTERVAL  # clock moved back
        failed = st.failed_at
        if failed is not None and failed <= now and (last is None or failed > last):
            due = max(due, failed + update.RETRY_INTERVAL)
        return max(0.0, (due - now).total_seconds())

    def wake(self, force: bool = False) -> None:
        """Look at the schedule again now (settings changed; tests after moving the clock)."""
        self._force = self._force or force
        self._wake.set()

    def start(self) -> None:
        """Start the automatic checks (:class:`Server` once listening, the real app only)."""
        if self._scheduler is None:
            self._scheduler = asyncio.get_running_loop().create_task(self._schedule(), name="update-schedule")

    async def _schedule(self) -> None:
        await asyncio.sleep(self.first_delay)
        while True:
            force, self._force = self._force, False
            try:
                delay = self.due_in()
                if delay is not None and (force or delay <= 0):
                    await self.check()
                    continue
            except Exception:
                logger.exception("update schedule failed")
                delay = ERROR_PAUSE_S
            with contextlib.suppress(TimeoutError):  # a wake during a check ends this wait at once
                await asyncio.wait_for(self._wake.wait(), delay)
            self._wake.clear()  # what woke it is in effect: the next round sees it

    def on_settings(self, st: SettingsStore) -> None:
        """Settings listener: switched on or another channel -> check now; any of them or the
        language changed -> tell the sessions (the error text follows the language)."""
        key = self._settings_key()
        before, self._applied = self._applied, key
        if key == before:
            return
        enabled, channel = key[0], key[1]
        self.wake(force=enabled and (not before[0] or channel != before[1]))
        self.broadcast()

    async def close(self) -> None:
        self._cancel.set()
        tasks = [t for t in (self._scheduler, self._check, self._download_task) if t is not None]
        for task in tasks:
            task.cancel()
        await asyncio.gather(*tasks, return_exceptions=True)
        self._scheduler = None


def install(server: Server) -> None:
    router = server.router
    service = server.updates
    server.settings.listeners["updates"] = service.on_settings
    auto = server.config.update_checks
    if server.config.log_file if auto is None else auto:
        server.on_start.append(service.start)

    @router.method(UPDATE_STATUS)
    async def update_status(session: Session, _: Empty) -> UpdateStatus:
        return service.status()

    @router.method(UPDATE_CHECK)
    async def update_check(session: Session, _: Empty) -> UpdateStatus:
        return await service.check()

    @router.method(UPDATE_DISMISS)
    async def update_dismiss(session: Session, params: UpdateVersionParams) -> UpdateStatus:
        return await service.dismiss(params.version)

    @router.method(UPDATE_DOWNLOAD)
    async def update_download(session: Session, params: UpdateVersionParams) -> UpdateStatus:
        return await service.download(params.version)

    @router.method(UPDATE_CANCEL)
    async def update_cancel(session: Session, _: Empty) -> UpdateStatus:
        return await service.cancel()
