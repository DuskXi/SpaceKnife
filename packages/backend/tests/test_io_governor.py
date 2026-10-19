"""IoGovernor control logic against a simulated disk (deterministic, simulated time)."""

from __future__ import annotations

import random
from collections.abc import Callable
from dataclasses import dataclass, field

import pytest

from spaceknife.io.disks import DiskId, DiskRef
from spaceknife.io.governor import GovernorCore, Level
from spaceknife.io.sampler import DiskSample

TICK = 0.002


@dataclass
class Batch:
    scan: int
    started: float
    work: float  # remaining nominal seconds


@dataclass
class Scan:
    workers: int
    waiting: int = 0
    running: int = 0
    done: int = 0
    active: bool = True


@dataclass
class Point:
    t: float
    self_true: float
    total: float
    self_est: float
    yielding: bool
    concurrency: int
    u: float


@dataclass
class Sim:
    """A disk with ``q_eff`` parallel units; each batch in flight adds ``q_worker`` to the queue.

    Measured like the real samplers: busy = share of time with I/O outstanding, queue = mean depth;
    the governor sees ``rho = min(busy, queue / q_eff)``. Batches slow down when the queue exceeds
    the disk's parallelism. ``others(t)`` is the other programs' load in rho units.
    """

    media: str = "sata_ssd"
    q_eff: float = 8.0
    q_worker: float = 0.8
    batch_s: float = 0.03
    max_workers: int = 16
    others: Callable[[float], float] = lambda t: 0.05
    noise: float = 0.02
    spikes: float = 0.0  # chance per window that the device stalls (queue × 1.5–2, like a GC pause)
    sample_s: float = 0.25  # the registry samples at 4 Hz while a scan runs
    seed: int = 1
    t: float = 0.0
    scans: list[Scan] = field(default_factory=list)
    points: list[Point] = field(default_factory=list)

    def __post_init__(self) -> None:
        self.core = GovernorCore(
            DiskRef(DiskId("sim0"), media=self.media),  # type: ignore[arg-type]
            max_workers=self.max_workers,
            now=0.0,
        )
        self.rng = random.Random(self.seed)
        self.batches: list[Batch] = []
        self._keys: dict[int, int] = {}  # waiter key -> scan index
        self._seq = 0
        self._acc = [0.0, 0.0, 0.0, 0.0]  # busy, queue, others-alone rho, n

    def add_scan(self, workers: int = 16) -> int:
        self.scans.append(Scan(workers))
        idx = len(self.scans) - 1
        for _ in range(workers):
            self._want(idx)
        return idx

    def stop_scan(self, idx: int) -> None:
        self.scans[idx].active = False

    def _want(self, idx: int) -> None:
        scan = self.scans[idx]
        if not scan.active:
            return
        if self.core.try_acquire(self.t):
            self._start(idx)
            return
        self._seq += 1
        self._keys[self._seq] = idx
        scan.waiting += 1
        self.core.request(self._seq, self.t)

    def _start(self, idx: int) -> None:
        self.scans[idx].running += 1
        self.batches.append(Batch(idx, self.t, self.batch_s))

    def _rho(self, busy: float, queue: float) -> float:
        return max(0.0, min(1.0, busy, queue / self.q_eff))

    def run(self, until: float) -> None:
        while self.t < until - 1e-9:
            self.t = round(self.t + TICK, 9)
            others_q = self.others(self.t) * self.q_eff
            q = others_q + self.q_worker * len(self.batches)
            slow = max(1.0, q / self.q_eff)
            finished = []
            for b in self.batches:
                b.work -= TICK / slow
                if b.work <= 0:
                    finished.append(b)
            for b in finished:
                self.batches.remove(b)
                self.core.release(self.t, b.started)
                scan = self.scans[b.scan]
                scan.running -= 1
                scan.done += 1
                self._want(b.scan)
            for key in self.core.grant(self.t):
                idx = self._keys.pop(int(key))  # type: ignore[call-overload]
                self.scans[idx].waiting -= 1
                self._start(idx)
            acc = self._acc
            acc[0] += min(1.0, q) * TICK
            acc[1] += q * TICK
            acc[2] += self._rho(min(1.0, others_q), others_q) * TICK
            acc[3] += TICK
            if acc[3] >= self.sample_s - 1e-9:
                self._sample()

    def _sample(self) -> None:
        busy, queue, others_alone, span = self._acc
        busy, queue, others_alone = busy / span, queue / span, others_alone / span
        noise = self.rng.uniform(-self.noise, self.noise)
        stall = self.rng.uniform(1.5, 2.0) if self.rng.random() < self.spikes else 1.0
        sample = DiskSample(
            t=self.t,
            dt=span,
            busy=min(1.0, max(0.0, busy + noise * 0.3)),
            queue=max(0.0, queue * (1 + noise) * stall),
        )
        total_true = self._rho(busy, queue)
        self.core.observe(sample, self.t, wall=self.t)
        for key in self.core.grant(self.t):
            idx = self._keys.pop(int(key))  # type: ignore[call-overload]
            self.scans[idx].waiting -= 1
            self._start(idx)
        self.points.append(
            Point(
                self.t,
                max(0.0, total_true - others_alone),
                self.core.total,
                self.core.self_load,
                self.core.yielding,
                self.core.concurrency,
                self.core.u,
            )
        )
        self._acc = [0.0, 0.0, 0.0, 0.0]

    # ------------------------------------------------------------------ metrics

    def window(self, t0: float, t1: float) -> list[Point]:
        return [p for p in self.points if t0 < p.t <= t1 + 1e-9]

    def mean_self(self, t0: float, t1: float) -> float:
        pts = self.window(t0, t1)
        return sum(p.self_true for p in pts) / len(pts)

    def moving_averages(self, t0: float, t1: float, span: float = 5.0) -> list[float]:
        out = []
        t = t0 + span
        while t <= t1 + 1e-9:
            out.append(self.mean_self(t - span, t))
            t += 1.0
        return out

    def seconds(self, t0: float, t1: float) -> list[float]:
        """Per-second means (what the W2 curve shows)."""
        out, t = [], t0
        while t + 1.0 <= t1 + 1e-9:
            out.append(self.mean_self(t, t + 1.0))
            t += 1.0
        return out


def start(sim: Sim, level: Level, *, at: float = 3.0, custom: float | None = None, workers: int = 16) -> int:
    sim.core.set_level(level, custom, sim.t)
    sim.run(at)  # pre-scan baseline (UI-W2-40)
    return sim.add_scan(workers)


@pytest.mark.parametrize(
    ("media", "q_eff", "q_worker", "level", "limit"),
    [
        ("sata_ssd", 8.0, 0.8, "balanced", 0.70),
        ("sata_ssd", 8.0, 0.8, "quiet", 0.35),
        ("hdd", 1.0, 0.9, "quiet", 0.35),
        ("hdd", 1.0, 0.9, "balanced", 0.70),
        ("nvme_ssd", 32.0, 0.9, "quiet", 0.35),
    ],
)
def test_converges_within_3s(media: str, q_eff: float, q_worker: float, level: Level, limit: float) -> None:
    sim = Sim(media=media, q_eff=q_eff, q_worker=q_worker, max_workers=32 if media == "nvme_ssd" else 16)
    start(sim, level)
    sim.run(25.0)
    per_second = sim.seconds(6.0, 25.0)  # 3 s after the start
    assert max(per_second) <= limit + 0.05, per_second  # single points: at most 5 points over
    mas = sim.moving_averages(6.0, 25.0)
    assert max(mas) <= limit + 0.005, mas  # steady state: 5 s average under the limit
    assert min(mas) >= 0.9 * limit, mas  # … and within 10 % of it
    est = [p.self_est for p in sim.window(8.0, 25.0)]
    assert abs(sum(est) / len(est) - sim.mean_self(8.0, 25.0)) < 0.03  # the estimate tracks the truth


def test_level_change_applies_within_3s() -> None:
    sim = Sim()
    start(sim, "balanced")
    sim.run(12.0)
    assert sim.mean_self(7.0, 12.0) > 0.6
    sim.core.set_level("quiet", None, sim.t)
    sim.run(25.0)
    assert max(sim.seconds(15.0, 25.0)) <= 0.40
    assert 0.9 * 0.35 <= sim.mean_self(15.0, 25.0) <= 0.35
    sim.core.set_level("custom", 50, sim.t)
    sim.run(35.0)
    assert 0.45 <= sim.mean_self(30.0, 35.0) <= 0.50


def test_max_level_does_not_throttle() -> None:
    sim = Sim(max_workers=12)
    start(sim, "max")
    sim.run(10.0)
    assert sim.core.concurrency == 12
    assert sim.core.duty == 1.0
    assert sim.mean_self(5.0, 10.0) > 0.9


def test_hdd_concurrency_stays_low() -> None:
    ssd, hdd = Sim(media="nvme_ssd", q_eff=32), Sim(media="hdd", q_eff=1, q_worker=0.9)
    assert hdd.core.concurrency < ssd.core.concurrency  # UI-W2-37: initial concurrency by media
    start(hdd, "max")
    hdd.run(8.0)
    assert hdd.core.concurrency <= 2


def test_yields_when_others_exceed_the_limit() -> None:
    sim = Sim(others=lambda t: 0.85 if 12.0 <= t < 24.0 else 0.05)
    start(sim, "balanced")
    sim.run(12.0)
    assert not sim.core.yielding
    sim.run(16.0)
    assert sim.core.yielding  # detected within 4 s
    sim.run(24.0)
    assert sim.core.duty <= 0.06 and sim.core.concurrency == 1  # lowest progress: 5 % duty
    assert all(p.yielding for p in sim.window(16.0, 24.0))
    progress = sim.scans[0].done
    sim.run(29.0)
    assert not sim.core.yielding  # recovers within 5 s of the others falling back
    assert sim.scans[0].done > progress
    sim.run(40.0)
    assert sim.mean_self(35.0, 40.0) > 0.6


def test_no_yield_when_disabled() -> None:
    sim = Sim(others=lambda t: 0.85 if t >= 12.0 else 0.05)
    sim.core.set_yield_when_busy(False)
    start(sim, "balanced")
    sim.run(30.0)
    assert not any(p.yielding for p in sim.points)
    assert min(p.u for p in sim.window(20.0, 30.0)) >= 1.0  # external load does not slow the scan


def test_others_rise_is_not_billed_to_us() -> None:
    """A moderate external load (below the limit) is re-baselined by a probe, not throttled away."""
    sim = Sim(others=lambda t: 0.25 if t >= 12.0 else 0.05)
    start(sim, "balanced")
    sim.run(30.0)
    assert not any(p.yielding for p in sim.points)
    assert abs((sim.core.others or 0.0) - 0.25) < 0.05
    assert sim.mean_self(22.0, 30.0) > 0.5  # still near the limit: capped by 100 % - others


def test_others_drop_does_not_overshoot() -> None:
    """When the others stop, a stale baseline would let our real load exceed the limit."""
    sim = Sim(others=lambda t: 0.20 if t < 12.0 else 0.0)
    sim.core.set_yield_when_busy(False)
    start(sim, "quiet")
    sim.run(35.0)
    assert max(sim.moving_averages(20.0, 35.0)) <= 0.36
    assert max(sim.seconds(20.0, 35.0)) <= 0.40


def test_pause_drops_self_to_zero_and_resume_recovers() -> None:
    sim = Sim()
    start(sim, "balanced")
    sim.run(10.0)
    done = sim.scans[0].done
    sim.core.pause("scan-1")
    sim.run(11.0)  # within 1 s
    assert sim.core.inflight == 0
    assert sim.points[-1].self_est == 0.0 and sim.points[-1].self_true < 0.01
    sim.run(14.0)
    assert sim.scans[0].done - done < 16  # only batches already in flight finished
    assert sim.core.status().paused
    assert abs((sim.core.others or 0.0) - 0.05) < 0.03  # baseline re-measured while paused
    sim.core.resume("scan-1")
    sim.run(20.0)
    assert sim.mean_self(16.0, 20.0) > 0.6


def test_two_scans_share_one_budget() -> None:
    sim = Sim()
    start(sim, "balanced", workers=8)
    sim.add_scan(8)  # a second scan on another partition of the same disk
    sim.run(25.0)
    assert max(sim.moving_averages(6.0, 25.0)) <= 0.705
    assert max(sim.seconds(6.0, 25.0)) <= 0.75
    a, b = sim.scans[0].done, sim.scans[1].done
    assert min(a, b) / (a + b) > 0.4  # FIFO permits: both progress at about the same pace


def test_manual_concurrency_keeps_the_limit() -> None:
    sim = Sim(media="hdd", q_eff=1, q_worker=0.9)
    sim.core.set_manual_concurrency(4, 0.0)
    start(sim, "quiet")
    sim.run(20.0)
    assert {p.concurrency for p in sim.points} == {4}
    assert not sim.core.auto
    assert max(sim.moving_averages(6.0, 20.0)) <= 0.355
    sim.core.set_manual_concurrency(None, sim.t)
    assert sim.core.auto


def test_idle_level_runs_only_when_the_disk_is_quiet() -> None:
    sim = Sim(others=lambda t: 0.30 if 10.0 <= t < 20.0 else 0.0)
    start(sim, "idle")
    sim.run(10.0)
    assert sim.mean_self(5.0, 10.0) > 0.85 and not sim.core.yielding  # minus frequent probes
    assert sim.core.low_io_priority
    sim.run(20.0)
    assert sim.core.yielding
    assert sim.mean_self(15.0, 20.0) < 0.08
    sim.run(30.0)
    assert not sim.core.yielding
    assert sim.mean_self(26.0, 30.0) > 0.9


def test_status_model_and_history() -> None:
    sim = Sim()
    start(sim, "quiet")
    sim.run(70.0)
    st = sim.core.status()
    assert st.disk == "sim0" and st.level == "quiet" and st.limit == 35.0
    assert len(st.history) == 60  # one point per second, last 60 s
    assert all(p.self_pct <= p.total_pct + 1e-6 for p in st.history)  # the line never tops the area
    assert st.measured and st.auto and not st.paused
    assert st.model_dump(mode="json")["history"][0].keys() >= {"t", "self_pct", "total_pct", "yielding"}


def test_open_loop_without_samples() -> None:
    core = GovernorCore(DiskRef(DiskId("net:\\\\nas\\share"), "network", media="network"), max_workers=8)
    core.set_level("quiet", None, 0.0)
    assert core.try_acquire(0.0)
    core.observe(None, 1.0)
    st = core.status()
    assert not st.measured and st.self_pct == 0.0
    assert core.u == pytest.approx(8 * 0.35)


def test_status_reports_the_disks_read_rate_and_latency() -> None:
    core = GovernorCore(DiskRef(DiskId("disk0"), media="nvme"))
    sample = DiskSample(
        t=1.0, dt=0.5, busy=0.2, queue=0.4, read_iops=2000, read_latency=0.00042, read_bps=1.5e8
    )
    core.observe(sample, 1.0)
    st = core.status()
    assert st.measured and st.read_bps == 150_000_000 and st.read_latency_ms == pytest.approx(0.42)
    core.observe(None, 1.5)  # no data: nothing to show
    st = core.status()
    assert not st.measured and st.read_bps is None and st.read_latency_ms is None
