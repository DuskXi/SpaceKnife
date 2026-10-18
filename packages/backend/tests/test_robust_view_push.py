"""Scan-driven view pushes: the end of a scan always yields a 'scan-final' frame."""

from __future__ import annotations

import asyncio
from types import SimpleNamespace
from typing import Any, cast

import pytest

from spaceknife.server.scans import ScanManager
from spaceknife.server.views import ViewHost, ViewManager
from spaceknife_protocol import view as pv


class Harness:
    """A view whose renders block on a gate, so a status can arrive mid-render."""

    def __init__(self, monkeypatch: pytest.MonkeyPatch) -> None:
        self.views = ViewManager(ScanManager())
        self.gate = asyncio.Event()
        self.rendered: list[str] = []
        self.pushed: list[str] = []
        harness = self

        async def render(self: ViewManager, host: ViewHost, reason: str) -> pv.FrameInfo:
            harness.rendered.append(reason)
            await harness.gate.wait()
            return pv.FrameInfo.model_construct(reason=reason)

        monkeypatch.setattr(ViewManager, "render", render)
        session = SimpleNamespace(emit=lambda spec, ev: self.pushed.append(ev.frame.reason))
        view = SimpleNamespace(id="view-1", filter=SimpleNamespace(active=False))
        self.host = ViewHost(view=cast(Any, view), scan_id="scan-1", session=cast(Any, session))
        self.views.views["view-1"] = self.host

    async def started(self, n: int) -> None:
        for _ in range(100):
            if len(self.rendered) >= n:
                return
            await asyncio.sleep(0)
        raise AssertionError(f"only {self.rendered} rendered")

    async def finish(self) -> None:
        self.gate.set()
        while self.host.push_task is not None and not self.host.push_task.done():
            await self.host.push_task


async def test_final_status_during_a_progress_push_still_renders_the_final_frame(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    h = Harness(monkeypatch)
    h.views._on_scan("scan-1", "scanning")
    await h.started(1)
    h.views._on_scan("scan-1", "done")  # arrives while the progress frame is rendering
    h.views._on_scan("scan-1", "done")  # the second final publish (snapshot saved) adds nothing
    await h.finish()
    assert h.rendered == ["scan", "scan-final"]
    assert h.pushed == ["scan", "scan-final"]
    assert not h.host.final_pending


async def test_final_push_is_not_repeated(monkeypatch: pytest.MonkeyPatch) -> None:
    h = Harness(monkeypatch)
    h.views._on_scan("scan-1", "done")
    await h.started(1)
    h.views._on_scan("scan-1", "done")
    await h.finish()
    assert h.rendered == ["scan-final"]


async def test_closing_the_view_drops_a_pending_final(monkeypatch: pytest.MonkeyPatch) -> None:
    h = Harness(monkeypatch)
    h.views._on_scan("scan-1", "scanning")
    await h.started(1)
    h.views._on_scan("scan-1", "cancelled")
    task = h.host.push_task
    h.views.close("view-1")
    assert task is not None
    with pytest.raises(asyncio.CancelledError):
        await task
    assert h.rendered == ["scan"]
