"""IoGovernor.permit() on a real event loop, and the IoRegistry sampling / push loops (short, real time)."""

from __future__ import annotations

import asyncio
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import pytest

from spaceknife.io import IoGovernor, IoRegistry, IoStatus
from spaceknife.io.disks import DiskId, DiskRef
from spaceknife.io.sampler import DiskSample, DiskSampler

DISK = DiskRef(DiskId("fake0"), media="sata_ssd")


def governor(**kw: int) -> IoGovernor:
    gov = IoGovernor(DISK, max_workers=kw.get("max_workers", 8))
    gov.set_level("max")
    return gov


async def test_permit_bounds_the_parallelism() -> None:
    gov = governor()
    gov.set_manual_concurrency(2)
    running, peak, done = 0, 0, 0

    async def batch() -> None:
        nonlocal running, peak, done
        permit = await gov.permit()
        running += 1
        peak = max(peak, running)
        await asyncio.sleep(0.02)
        running -= 1
        done += 1
        permit.release()

    await asyncio.gather(*(batch() for _ in range(6)))
    assert peak == 2 and done == 6
    assert gov.status().concurrency == 2 and not gov.status().auto
    assert gov.core.inflight == 0


async def test_pause_holds_new_permits_until_resume() -> None:
    gov = governor()
    gov.pause("scan-a")
    waiter = asyncio.ensure_future(gov.permit())
    await asyncio.sleep(0.05)
    assert not waiter.done() and gov.status().paused
    gov.pause("scan-b")
    gov.resume("scan-a")  # still paused by the other owner
    await asyncio.sleep(0.02)
    assert not waiter.done()
    gov.resume("scan-b")
    permit = await asyncio.wait_for(waiter, 1.0)
    permit.release()
    assert not gov.paused


async def test_cancelled_waiter_does_not_leak_a_slot() -> None:
    gov = governor()
    gov.set_manual_concurrency(1)
    first = await gov.permit()
    waiter = asyncio.ensure_future(gov.permit())
    await asyncio.sleep(0.01)
    waiter.cancel()
    with pytest.raises(asyncio.CancelledError):
        await waiter
    first.release()
    second = await asyncio.wait_for(gov.permit(), 1.0)
    assert gov.core.inflight == 1
    second.release()
    assert gov.core.inflight == 0 and gov.core.waiting == 0


async def test_bind_and_release_from_another_thread() -> None:
    gov = governor()
    loop = asyncio.get_running_loop()
    with ThreadPoolExecutor(2) as pool:
        permit = await gov.permit()
        fut = loop.run_in_executor(pool, time.sleep, 0.02)
        permit.bind(fut)
        await fut
        await asyncio.sleep(0)
        assert gov.core.inflight == 0
        other = await gov.permit()
        t = threading.Thread(target=other.release)  # e.g. a concurrent.futures callback thread
        t.start()
        t.join()
        await asyncio.sleep(0.01)
        assert gov.core.inflight == 0
        other.release()  # idempotent
        assert gov.core.inflight == 0


async def test_duty_gate_paces_fractional_intensity() -> None:
    """Open loop (no samples): quiet on one worker means about 35 % of the time in flight."""
    gov = IoGovernor(DiskRef(DiskId("net:x"), "network", media="network"), max_workers=1)
    gov.observe(None)  # the registry ticks unsampled disks with None
    gov.set_level("quiet")
    assert gov.core.u == pytest.approx(0.35)
    busy = 0.0
    start = time.perf_counter()
    while time.perf_counter() - start < 1.5:
        permit = await gov.permit()
        t0 = time.perf_counter()
        await asyncio.sleep(0.02)
        busy += time.perf_counter() - t0
        permit.release()
    share = busy / (time.perf_counter() - start)
    assert 0.25 < share < 0.45, share


# --------------------------------------------------------------------------- registry


class FakeSampler(DiskSampler):
    """A disk whose load follows the governor: 10 % per batch in flight, others 5 %."""

    name = "fake"
    supported = True

    def __init__(self) -> None:
        super().__init__()
        self.watch(DISK)
        self.gov: IoGovernor | None = None
        self.last = time.monotonic()
        self.watched: list[DiskRef] = []

    def watch(self, ref: DiskRef) -> None:
        super().watch(ref)
        if hasattr(self, "watched"):
            self.watched.append(ref)

    def sample(self) -> dict[DiskId, DiskSample]:
        now = time.monotonic()
        dt, self.last = now - self.last, now
        n = self.gov.core.inflight if self.gov else 0
        q = 8 * (0.05 + 0.1 * n)
        return {DISK.id: DiskSample(t=now, dt=dt, busy=min(1.0, q), queue=q)}


async def test_registry_samples_controls_and_pushes(tmp_path: Path) -> None:
    fake = FakeSampler()
    registry = IoRegistry(
        sampler_factory=lambda: fake,
        max_workers=8,
        idle_interval=0.05,
        active_interval=0.05,
        push_interval=0.05,
    )
    async with registry:
        gov = registry.governor(DISK.id)
        fake.gov = gov
        assert gov is registry.governor(DISK)  # one governor per disk
        pushes: list[IoStatus] = []
        unsubscribe = gov.subscribe(pushes.append)
        gov.set_level("quiet")
        await asyncio.sleep(0.3)  # pre-scan baseline
        assert gov.core.others == pytest.approx(0.05, abs=0.01)

        async def scan() -> None:
            loop = asyncio.get_running_loop()
            until = time.monotonic() + 1.5
            while time.monotonic() < until:
                permit = await gov.permit()
                permit.bind(asyncio.ensure_future(asyncio.sleep(0.01)))
                await asyncio.sleep(0)
            await asyncio.sleep(0.05)
            assert loop.is_running()

        await scan()
        unsubscribe()
        assert len(pushes) >= 10  # ~20 Hz here (push_interval), 2 Hz in production
        st = gov.status()
        assert st.measured and st.limit == 35.0 and len(st.history) >= 1
        assert 0 < st.concurrency <= 8
        other = await registry.governor_for_path(str(tmp_path))  # real mapping of this machine's disk
        assert other.disk.id != DISK.id and other in registry.governors()
        assert any(r.id == other.disk.id for r in fake.watched) or not other.disk.sampled
        net = registry.governor(DiskId("net:\\\\nas\\share"))
        await asyncio.sleep(0.15)
        assert not net.status().measured  # ticked open loop
    assert not registry.running
