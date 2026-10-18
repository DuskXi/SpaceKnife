"""Backend-wide disk-load registry: samples every local disk from start-up, one governor per disk.

Sampling runs at 1 Hz while no scan is using a disk (enough for the W2 pre-scan baseline,
UI-W2-40) and at 4 Hz while one is: the governor takes a control step every window, and its
baseline probes hold dispatch for one window, so short windows keep probes cheap. The W2 history
stays at one point per second. Status pushes go out at 2 Hz to the listeners of each governor
(:meth:`IoGovernor.subscribe`).

The sampler is not thread-safe and blocks: it lives on one dedicated worker thread.
"""

from __future__ import annotations

import asyncio
import contextlib
import time
from collections.abc import Callable
from concurrent.futures import ThreadPoolExecutor
from types import TracebackType

from loguru import logger

from .disks import DiskId, DiskRef, disk_ref_for_path
from .governor import IoGovernor, IoStatus, Level
from .sampler import DiskSample, DiskSampler, default_sampler


class IoRegistry:
    def __init__(
        self,
        *,
        sampler_factory: Callable[[], DiskSampler] = default_sampler,
        max_workers: int = 8,
        idle_interval: float = 1.0,
        active_interval: float = 0.25,
        push_interval: float = 0.5,
        clock: Callable[[], float] = time.monotonic,
    ) -> None:
        self._factory = sampler_factory
        self.max_workers = max_workers
        self.idle_interval = idle_interval
        self.active_interval = active_interval
        self.push_interval = push_interval
        self._clock = clock
        self._govs: dict[DiskId, IoGovernor] = {}
        self._sampler: DiskSampler | None = None
        self._exec: ThreadPoolExecutor | None = None
        self._tasks: list[asyncio.Task[None]] = []
        self._defaults: tuple[Level, float | None, bool] = ("balanced", None, True)

    # ------------------------------------------------------------------ lifecycle

    @property
    def running(self) -> bool:
        return bool(self._tasks)

    @property
    def sampler(self) -> DiskSampler | None:
        return self._sampler

    async def start(self) -> None:
        if self._tasks:
            return
        loop = asyncio.get_running_loop()
        self._exec = ThreadPoolExecutor(max_workers=1, thread_name_prefix="spaceknife-io")
        self._sampler = await loop.run_in_executor(self._exec, self._factory)
        for ref in self._sampler.refs():
            self._ensure(ref)
        self._tasks = [
            asyncio.create_task(self._sample_loop(), name="io-sample"),
            asyncio.create_task(self._push_loop(), name="io-push"),
        ]
        logger.debug("io registry: sampler {}, {} disks", self._sampler.name, len(self._govs))

    async def stop(self) -> None:
        tasks, self._tasks = self._tasks, []
        for task in tasks:
            task.cancel()
        for task in tasks:
            with contextlib.suppress(asyncio.CancelledError):
                await task
        if self._sampler is not None and self._exec is not None:
            loop = asyncio.get_running_loop()
            with contextlib.suppress(Exception):
                await loop.run_in_executor(self._exec, self._sampler.close)
        if self._exec is not None:
            self._exec.shutdown(wait=False, cancel_futures=True)
            self._exec = None

    async def __aenter__(self) -> IoRegistry:
        await self.start()
        return self

    async def __aexit__(
        self, exc_type: type[BaseException] | None, exc: BaseException | None, tb: TracebackType | None
    ) -> None:
        await self.stop()

    # ------------------------------------------------------------------ governors

    def set_defaults(
        self, level: Level = "balanced", custom: float | None = None, yield_when_busy: bool = True
    ) -> None:
        """Settings applied to governors created from now on (scans set their own level on start)."""
        self._defaults = (level, custom, yield_when_busy)

    def _ensure(self, ref: DiskRef) -> IoGovernor:
        gov = self._govs.get(ref.id)
        if gov is None:
            gov = IoGovernor(ref, max_workers=self.max_workers, clock=self._clock)
            level, custom, yield_busy = self._defaults
            gov.set_level(level, custom)
            gov.set_yield_when_busy(yield_busy)
            self._govs[ref.id] = gov
        elif gov.disk.media == "unknown" and ref.media != "unknown":
            gov.core.set_media(ref.media)
        return gov

    def governor(self, disk: DiskId | DiskRef) -> IoGovernor:
        """The governor of a disk (created on first use)."""
        if isinstance(disk, DiskRef):
            return self._ensure(disk)
        gov = self._govs.get(disk)
        if gov is not None:
            return gov
        ref = self._sampler.ref(disk) if self._sampler is not None else None
        return self._ensure(
            ref or DiskRef(disk, "physical" if not disk.startswith(("net:", "vol:")) else "volume")
        )

    async def governor_for_path(self, path: str) -> IoGovernor:
        """The governor of the physical disk holding ``path`` (shared by every scan on that disk)."""
        ref = await asyncio.to_thread(disk_ref_for_path, path)
        gov = self._ensure(ref)
        if ref.sampled and self._sampler is not None and self._exec is not None:
            await asyncio.get_running_loop().run_in_executor(self._exec, self._sampler.watch, ref)
        return gov

    def governors(self) -> list[IoGovernor]:
        return list(self._govs.values())

    def statuses(self, history: bool = False) -> list[IoStatus]:
        return [g.status(history) for g in self._govs.values()]

    # ------------------------------------------------------------------ loops

    def _interval(self) -> float:
        busy = any(g.active() for g in self._govs.values())
        floor = self._sampler.min_interval if self._sampler is not None else 0.0
        return max(floor, self.active_interval if busy else self.idle_interval)

    async def _sample_loop(self) -> None:
        assert self._sampler is not None
        assert self._exec is not None
        loop = asyncio.get_running_loop()
        sampler, failures = self._sampler, 0
        next_at = self._clock()
        while True:
            next_at += self._interval()
            delay = next_at - self._clock()
            if delay < 0:
                next_at, delay = self._clock(), 0.0
            await asyncio.sleep(delay)
            try:
                samples: dict[DiskId, DiskSample] = await loop.run_in_executor(self._exec, sampler.sample)
                failures = 0
            except Exception as exc:
                failures += 1
                if failures in (1, 30):
                    logger.warning("disk load sampling failed: {}", exc)
                samples = {}
            self._dispatch(samples, sampler)

    def _dispatch(self, samples: dict[DiskId, DiskSample], sampler: DiskSampler) -> None:
        wall = time.time()
        for disk, sample in samples.items():
            gov = self._govs.get(disk) or self._ensure(sampler.ref(disk) or DiskRef(disk))
            gov.observe(sample, wall)
        for disk, gov in list(self._govs.items()):
            if disk not in samples and not (sampler.supported and gov.disk.sampled):
                gov.observe(None, wall)  # open loop: keep the governor ticking

    async def _push_loop(self) -> None:
        while True:
            await asyncio.sleep(self.push_interval)
            for gov in list(self._govs.values()):
                if gov.has_listeners:
                    gov.push()


__all__ = ["IoRegistry"]
