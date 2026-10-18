"""Per-disk I/O governor: closed-loop disk-load limiting (docs/native.md §4, UI-W2-29…45, G3).

Measured load ``rho = min(busy, queue / Q_eff)`` (``Q_eff``: the disk's effective parallelism, a
prior by media refined online). SpaceKnife's own share ``self = rho - others`` where *others* is an
EWMA baseline measured in windows during which SpaceKnife had nothing in flight: before a scan,
while paused, and in short probes (dispatch held for one sample window) that run periodically and
whenever the load moves in a way our own activity does not explain.

Two actuators, both behind :meth:`IoGovernor.permit` (awaited before every dispatched batch):

* the allowed parallelism ``concurrency`` (integer, 1..max workers), and
* a duty-cycle gate, a token bucket in worker-seconds: on average at most ``u`` batches are in
  flight (``u`` = concurrency × duty), so fractional intensities (an HDD at 35 %) are exact.

The controller adjusts the intensity ``u`` every sample window, AIMD-shaped: additive increase of at
most one worker per period while under the setpoint (proportional when the gain is known),
multiplicative decrease (at most halving) when over the limit. Yielding (G3): when the others alone
exceed the limit, the scan drops to 5 % duty until they fall back.

:class:`GovernorCore` is the synchronous, clock-free logic (tests drive it with a simulated disk);
:class:`IoGovernor` wraps it for asyncio. One governor per physical disk, shared by every scan on it.
"""

from __future__ import annotations

import asyncio
import contextlib
import math
import time
from collections import deque
from collections.abc import Callable, Hashable
from dataclasses import dataclass, replace
from typing import Any, Literal

from loguru import logger

from spaceknife_protocol.io import IoStatus, LoadLevel, LoadPoint

from ..scan.media import Media
from .disks import DiskRef
from .sampler import DiskSample

Level = LoadLevel
LEVEL_LIMITS: dict[str, float] = {"max": 1.0, "balanced": 0.70, "quiet": 0.35, "idle": 1.0}
DEFAULT_CUSTOM = 70.0

U_MIN = 0.05  # lowest intensity: 5 % of one worker ("最低进度")
U_YIELD = 0.05
CLEAN_N = 0.05  # mean in-flight below this: the window shows the others alone
IDLE_OTHERS = 0.10  # idle level: others above this make the scan yield
SATURATED = 0.95  # whole-disk load from which "total minus baseline" no longer tells our share
PROBE_EVERY = 8.0  # doubles up to PROBE_EVERY_MAX while the baseline holds still
PROBE_EVERY_MAX = 32.0
PROBE_EVERY_IDLE = 3.0
PROBE_EVERY_YIELDING = 2.0
PROBE_MIN_GAP = 1.0
STEP_PERIOD = 0.5  # additive increase: at most one worker per this much time
IDLE_RESET_S = 30.0  # unused this long: start the next scan from the media prior again
HISTORY = 60


@dataclass(frozen=True, slots=True)
class MediaProfile:
    q_eff: float | None  # prior effective parallelism; None = not measurable (open loop)
    initial: float  # initial intensity, in-flight batches
    cap: int | None  # concurrency ceiling for this media (None: max workers)


PROFILES: dict[str, MediaProfile] = {
    "nvme_ssd": MediaProfile(32, 4, None),
    "sata_ssd": MediaProfile(8, 3, None),
    "ssd": MediaProfile(8, 3, None),
    "virtual": MediaProfile(8, 2, None),
    "hdd": MediaProfile(1, 1, 2),  # low concurrency: seeks thrash (UI-W2-37)
    "usb_hdd": MediaProfile(1, 1, 2),
    "usb_ssd": MediaProfile(2, 2, 4),
    "usb_flash": MediaProfile(2, 1, 2),
    "sd_card": MediaProfile(1, 1, 2),
    "network": MediaProfile(None, 2, 8),
    "unknown": MediaProfile(4, 2, None),
}


def level_limit(level: Level, custom: float | None = None) -> float:
    """Limit of a level as a fraction (max and idle do not cap SpaceKnife's own load)."""
    if level == "custom":
        return min(1.0, max(0.01, (DEFAULT_CUSTOM if custom is None else custom) / 100.0))
    return LEVEL_LIMITS[level]


def setpoint(limit: float) -> float:
    """Where the controller aims: a little under the limit so noise rarely crosses it."""
    return limit - max(0.01, 0.03 * limit)


# --------------------------------------------------------------------------- core


class GovernorCore:
    """Synchronous governor logic; every method takes the current ``time.monotonic``-style time."""

    GAIN_ALPHA = 0.3  # EWMA weight of a new gain measurement (per sample window)
    INCREASE_GAIN = 0.3  # share of the gap to the setpoint closed per window when under it

    def __init__(self, disk: DiskRef, *, max_workers: int = 8, now: float = 0.0) -> None:
        self.disk = disk
        self.max_workers = max(1, max_workers)
        self.profile = PROFILES.get(disk.media, PROFILES["unknown"])
        self.q_eff = self.profile.q_eff
        self.measured = False
        self._open_loop = False  # the registry has no samples for this disk
        # settings
        self.level: Level = "balanced"
        self.custom = DEFAULT_CUSTOM
        self.manual: int | None = None
        self.yield_when_busy = True
        self._pauses: set[object] = set()
        # actuators
        self.u = min(float(self._cap()), self._initial(level_limit(self.level)))
        self.tokens = 0.0
        self.inflight = 0
        self._waiters: deque[Hashable] = deque()
        self._t = now  # last integration time (tokens, in-flight area)
        self.batch_s = 0.03  # EWMA of batch duration
        self._last_dispatch = -math.inf
        # per-window accounting
        self._area = 0.0
        self._win_t0 = now
        self._win_area0 = 0.0
        self._win_peak = 0
        self._win_blocked = False
        self._last_n = 0.0
        # estimator
        self._lat_ref: dict[int, float] = {}  # lowest read latency seen per transfer-size class
        self._saturated = 0  # consecutive windows that look saturated
        self.others: float | None = None
        self.g_ref: float | None = None  # own load per in-flight batch, learned when others are known
        self._last_clean = -math.inf
        self._anomalies = 0
        self.total = 0.0
        self.self_load = 0.0
        self.active = 0.0
        self._shown = [0.0, 0.0, 0.0]  # ~1 s averages of self, total, active for status pushes
        self.yielding = False
        self._u_before_yield = self.u
        self._gain: float | None = None  # filtered own load per batch in flight (control only)
        self._gains: deque[float] = deque(maxlen=3)
        # probes
        self._probe: Literal["armed", "holding"] | None = None
        self._hold_from = 0.0
        self._last_probe = -math.inf
        self._probe_every = PROBE_EVERY
        self._confirm = False  # the last probe moved the baseline a lot: probe again soon
        self._dt = 1.0  # last sample window length
        # history (one point per second)
        self.history: deque[LoadPoint] = deque(maxlen=HISTORY)
        self._bucket = [
            0.0,
            0.0,
            0.0,
            False,
            False,
            0.0,
        ]  # self·dt, total·dt, dt, yielding, paused, active·dt

    # ------------------------------------------------------------------ derived

    @property
    def limit(self) -> float:
        return level_limit(self.level, self.custom)

    @property
    def paused(self) -> bool:
        return bool(self._pauses)

    @property
    def auto(self) -> bool:
        return self.manual is None

    @property
    def concurrency(self) -> int:
        if self.manual is not None:
            return self.manual
        return max(1, min(self._cap(), math.ceil(self.u - 1e-9)))

    @property
    def duty(self) -> float:
        return min(1.0, self.u / self.concurrency)

    @property
    def low_io_priority(self) -> bool:
        return self.level == "idle"

    @property
    def waiting(self) -> int:
        return len(self._waiters)

    def active_recently(self, now: float, within: float = 5.0) -> bool:
        return self.inflight > 0 or bool(self._waiters) or now - self._last_dispatch < within

    def _cap(self) -> int:
        if self.manual is not None:
            return self.manual
        return max(1, min(self.max_workers, self.profile.cap or self.max_workers))

    def _initial(self, limit: float) -> float:
        """Start intensity: the media prior, or what one I/O-bound worker per ``Q_eff`` predicts."""
        if limit >= 1.0 or self.q_eff is None:
            return self.profile.initial
        return max(U_MIN, min(self.profile.initial, setpoint(limit) * self.q_eff))

    def _limited(self) -> bool:
        return self.level not in ("max", "idle") and self.limit < 1.0

    # ------------------------------------------------------------------ settings

    def set_media(self, media: Media) -> None:
        if media == self.disk.media or media not in PROFILES:
            return
        self.disk = replace(self.disk, media=media)
        self.profile = PROFILES[media]
        self.q_eff = self.profile.q_eff
        self._lat_ref.clear()
        self.u = min(self.u, float(self._cap()))

    def set_level(self, level: Level, custom: float | None, now: float) -> None:
        if level not in ("max", "balanced", "quiet", "idle", "custom"):
            raise ValueError(f"unknown level: {level}")
        self._integrate(now)
        old = self.limit
        self.level = level
        if custom is not None:
            self.custom = min(100.0, max(1.0, float(custom)))
        new = self.limit
        if not self._limited():
            self.yielding = self.yielding and level == "idle"
            self.u = float(self._cap())
        elif self._open_loop:
            self.u = self._open_loop_u()
        elif new < old or old >= 1.0:
            # apply the new limit at once (UI-W2-33), scaled by the last measurement
            base = min(self.u, max(self._last_n, U_MIN))
            if self.self_load > 0.0:
                self.u = base * min(1.0, max(0.1, setpoint(new) / self.self_load))
            else:
                self.u = min(self.u, self._initial(new))
        self.u = min(max(self.u, U_MIN), float(self._cap()))
        self.tokens = min(self.tokens, self._burst())
        if old != new:
            logger.debug("io {}: level {} ({:.0%}) u={:.2f}", self.disk.id, level, new, self.u)

    def set_manual_concurrency(self, n: int | None, now: float) -> None:
        self._integrate(now)
        self.manual = None if n is None else max(1, int(n))
        self.u = min(max(self.u, U_MIN), float(self._cap()))
        if not self._limited():
            self.u = float(self._cap())

    def set_yield_when_busy(self, on: bool) -> None:
        self.yield_when_busy = on
        if not on and self.level != "idle" and self.yielding:
            self._leave_yield()

    def set_max_workers(self, n: int) -> None:
        self.max_workers = max(1, n)
        self.u = min(self.u, float(self._cap()))

    def pause(self, owner: object = None) -> None:
        self._pauses.add(owner)

    def resume(self, owner: object = None) -> None:
        self._pauses.discard(owner)

    # ------------------------------------------------------------------ dispatch gate

    def _burst(self) -> float:
        return 0.05 + 0.05 * self.u

    def _integrate(self, now: float) -> None:
        dt = now - self._t
        if dt <= 0:
            return
        self._area += self.inflight * dt
        self.tokens = max(-1.0, min(self._burst(), self.tokens + (self.u - self.inflight) * dt))
        self._t = now

    def _duty_limited(self) -> bool:
        return self.u < self.concurrency - 1e-9

    def _price(self) -> float:
        """Credit one more batch needs: its expected share above the intensity ``u`` (pre-paid, so
        several free slots do not all start at once and leave a long debt behind)."""
        return max(0.0, self.inflight + 1 - self.u) * self.batch_s

    def _probe_blocks(self, now: float) -> bool:
        if self._probe is None:
            return False
        if now > self._hold_from + 2.5 * max(self._dt, 0.25):  # sampling stalled: never hang a scan
            self._probe = None
            return False
        return self._probe == "holding" or now >= self._hold_from

    def can_dispatch(self, now: float) -> bool:
        self._integrate(now)
        if self._pauses or self._probe_blocks(now) or self.inflight >= self.concurrency:
            return False
        return not self._duty_limited() or self.tokens >= self._price()

    def try_acquire(self, now: float) -> bool:
        """Dispatch at once if nobody is queued and the gate is open."""
        if self._waiters or not self.can_dispatch(now):
            return False
        self._dispatch(now)
        return True

    def request(self, key: Hashable, now: float) -> None:
        self._waiters.append(key)
        self._win_blocked = True

    def cancel(self, key: Hashable) -> None:
        with contextlib.suppress(ValueError):
            self._waiters.remove(key)

    def grant(self, now: float) -> list[Hashable]:
        """Pop (FIFO) and dispatch every waiter the gate lets through now."""
        out: list[Hashable] = []
        while self._waiters and self.can_dispatch(now):
            out.append(self._waiters.popleft())
            self._dispatch(now)
        if self._waiters:
            self._win_blocked = True
        return out

    def wake_time(self, now: float) -> float | None:
        """When :meth:`grant` may succeed next; None: only after an event (release, resume, sample)."""
        if not self._waiters or self._pauses:
            return None
        self._integrate(now)
        if self._probe is not None:
            stall = self._hold_from + 2.5 * max(self._dt, 0.25) + 1e-3
            if self._probe == "holding" or now >= self._hold_from:
                return stall
        if self.inflight >= self.concurrency:
            return None
        price = self._price()
        if self._duty_limited() and self.tokens < price:
            rate = self.u - self.inflight
            return None if rate <= 0 else now + (price - self.tokens) / rate + 1e-4
        return now

    def _dispatch(self, now: float) -> None:
        self._integrate(now)
        self.inflight += 1
        self._win_peak = max(self._win_peak, self.inflight)
        self._last_dispatch = now

    def release(self, now: float, started: float | None = None) -> None:
        self._integrate(now)
        self.inflight = max(0, self.inflight - 1)
        if started is not None and now > started:
            self.batch_s += 0.2 * (min(now - started, 2.0) - self.batch_s)

    # ------------------------------------------------------------------ measurement

    def _learn_q_eff(self, s: DiskSample, own_share: float) -> None:
        """Refine Q_eff from read statistics by Little's law: of ``queue`` outstanding I/Os,
        ``queue · L_ref / L`` are in service and the rest wait. ``L_ref`` is the lowest read latency
        seen for transfers of about this size (a 64 KiB read is slower than a 4 KiB one without the
        disk being any busier), learned only while our own reads dominate the disk.

        More I/Os in service than Q_eff at unchanged latency: the disk is wider than assumed, raise
        it. Read latency stuck far above the reference at a deep queue: the disk is saturated at
        what is in service, lower it (slowly, never below a quarter of the prior). Otherwise drift
        back to the prior, which forgets passing interference.
        """
        prior = self.profile.q_eff
        if prior is None or self.q_eff is None:
            return
        lat, iops = s.read_latency, s.read_iops
        if not lat or not iops or iops * s.dt < 20 or own_share < 0.7:
            self.q_eff += 0.002 * (prior - self.q_eff)
            self._saturated = 0
            return
        size = (s.read_bps or 0.0) / iops
        size_class = 0 if size <= 4096 else min(8, round(math.log2(size / 4096)))  # 4 KiB … 1 MiB+
        drift = 1.0 + 0.004 * s.dt  # old minima are forgotten over minutes
        ref = self._lat_ref.get(size_class)
        ref = lat if ref is None else min(ref * drift, lat)
        self._lat_ref[size_class] = ref
        in_service = s.queue * ref / lat
        saturated = s.queue >= 4 and s.busy >= 0.95 and lat >= 4 * ref
        self._saturated = self._saturated + 1 if saturated else 0
        if lat <= 1.5 * ref and in_service > self.q_eff:
            self.q_eff += 0.2 * (in_service - self.q_eff)
        elif self._saturated >= 4:
            self.q_eff += 0.05 * (max(1.0, in_service) - self.q_eff)
        else:
            self.q_eff += 0.002 * (prior - self.q_eff)
        self.q_eff = min(prior * 4, max(max(1.0, prior / 4), self.q_eff))

    def _rho(self, s: DiskSample) -> float:
        if self.q_eff is None:
            return s.busy
        return max(0.0, min(1.0, s.busy, s.queue / self.q_eff))

    def observe(self, sample: DiskSample | None, now: float, wall: float | None = None) -> None:
        """Feed the load of the window that just ended (None: no data) and run one control step."""
        self._integrate(now)
        span = now - self._win_t0
        n_bar = (self._area - self._win_area0) / span if span > 0 else 0.0
        peak, blocked = self._win_peak, self._win_blocked
        self._win_t0, self._win_area0 = now, self._area
        self._win_peak, self._win_blocked = self.inflight, bool(self._waiters)
        self._last_n = n_bar
        demand = peak > 0 or blocked
        if not demand and self._limited() and now - self._last_dispatch > IDLE_RESET_S:
            self.u = self._initial(self.limit)
        if sample is None:
            self.measured, self._open_loop = False, True
            self.total = self.self_load = self.active = 0.0
            if self._limited() and demand and not self._pauses:
                self.u = self._open_loop_u()
            self._record(span, wall)
            return
        self.measured, self._open_loop = True, False
        self._dt = sample.dt if sample.dt > 0 else self._dt
        rho = self._rho(sample)
        self.active, self.total = sample.busy, rho
        clean = n_bar <= CLEAN_N
        held = False  # this window was a probe: dispatch was held on purpose
        if self._probe == "holding":
            self._probe, self._last_probe, held = None, now, True
            self.tokens = min(self.tokens, 0.0)  # no burst when dispatch resumes
        elif self._probe == "armed" and now >= self._hold_from - 0.1 * self._dt:
            self._probe = "holding"
        previous = self.others
        self._estimate(rho, n_bar, clean, now, held)
        self._learn_q_eff(sample, self.self_load / rho if rho > 0 else 0.0)
        if held and clean:
            # a quiet baseline needs fewer probes; a moving one, more
            moved = previous is None or abs((self.others or 0.0) - previous) >= 0.03
            self._probe_every = PROBE_EVERY if moved else min(PROBE_EVERY_MAX, 2 * self._probe_every)
        if clean:
            self._update_yield()
        if not self._pauses and demand and not held:
            self._control(n_bar, peak, blocked, sample.dt)
        self._plan_probe(now, demand)
        self._record(span, wall)

    def _estimate(self, rho: float, n_bar: float, clean: bool, now: float, probe: bool) -> None:
        g = self.g_ref or 0.0
        if clean:
            measured = max(0.0, rho - g * n_bar)
            # A probe is rare and deliberate: trust it, unless it moves the baseline a lot (a device
            # stall can spill into the probe window): then half-trust it and confirm with another.
            if probe and self.others is not None and abs(measured - self.others) > 0.1 and not self._confirm:
                alpha, self._confirm = 0.5, True
            elif probe:
                alpha, self._confirm = 1.0, False
            else:
                alpha = 0.6 if self.active_recently(now, 10.0) else 0.3
            self.others = measured if self.others is None else self.others + alpha * (measured - self.others)
            self._last_clean = now
            self.self_load = min(rho, g * n_bar)
            self._anomalies = 0
            return
        base = self.others or 0.0
        self.self_load = max(0.0, min(rho, rho - base))
        saturated = rho >= SATURATED
        if saturated and self.g_ref is not None and n_bar >= 0.2:
            # a full disk hides who is using it: total minus baseline says nothing any more, so
            # fall back to the learned load per batch in flight (whichever is larger)
            self.self_load = min(rho, max(self.self_load, self.g_ref * n_bar))
        fresh = now - self._last_clean <= 2.5 * self._dt + 1e-6
        if fresh and not saturated and n_bar >= 0.2 and self.self_load > 0.005:
            ratio = self.self_load / n_bar
            self.g_ref = ratio if self.g_ref is None else self.g_ref + 0.3 * (ratio - self.g_ref)
        if self.g_ref is not None and self.others is not None and n_bar >= 0.2:
            predicted = min(1.0, self.others + self.g_ref * n_bar)
            miss = abs(rho - predicted)
            if miss > max(0.15, 0.4 * max(predicted, rho)):
                self._anomalies += 2
            elif miss > max(0.08, 0.25 * max(predicted, rho)):
                self._anomalies += 1
            else:
                self._anomalies = 0

    def _update_yield(self) -> None:
        if self.others is None:
            return
        if self.level == "idle":
            threshold = IDLE_OTHERS
        elif self.yield_when_busy and self._limited():
            threshold = self.limit
        else:
            if self.yielding:
                self._leave_yield()
            return
        if not self.yielding and self.others > threshold:
            self.yielding = True
            self._u_before_yield = self.u
            self.u = U_YIELD
            self.tokens = min(self.tokens, 0.0)
            logger.info("io {}: yielding (others {:.0%} > {:.0%})", self.disk.id, self.others, threshold)
        elif self.yielding and self.others < 0.85 * threshold:
            self._leave_yield()

    def _leave_yield(self) -> None:
        self.yielding = False
        self.u = float(self._cap()) if not self._limited() else max(U_MIN, 0.5 * self._u_before_yield)
        logger.info("io {}: yield over, u={:.2f}", self.disk.id, self.u)

    def _open_loop_u(self) -> float:
        if self.level == "idle":
            return 1.0
        return max(U_MIN, min(float(self._cap()), self._cap() * self.limit))

    def _control(self, n_bar: float, peak: int, blocked: bool, dt: float) -> None:
        cap = float(self._cap())
        if self.yielding:
            self.u = U_YIELD
            return
        if not self._limited():
            self.u = cap
            return
        limit, target_load, load = self.limit, setpoint(self.limit), self.self_load
        # additive increase: +1 worker per STEP_PERIOD; far below the setpoint (scan start, after a
        # yield) grow by up to half the intensity per period instead, like TCP slow start
        far = load < 0.5 * target_load
        per_period = min(max(1.0, 0.5 * self.u), max(0.1, self.u)) if far else 1.0
        step = per_period * dt / STEP_PERIOD
        # The gain (own load per batch in flight) hardly depends on the intensity, so filtering it
        # damps measurement noise without delaying the response to our own changes (n̄ is exact).
        gain = None
        if n_bar >= 0.05 and load > 0.005:
            # median of the last three windows first: a single-window device stall is not a trend
            self._gains.append(load / n_bar)
            g_now = sorted(self._gains)[len(self._gains) // 2]
            a = self.GAIN_ALPHA
            self._gain = g_now if self._gain is None else self._gain + a * (g_now - self._gain)
            gain = self._gain
        estimate = load if gain is None else gain * n_bar
        if estimate > limit:
            # multiplicative decrease, proportional to the excess, at most halving
            base = n_bar if n_bar >= U_MIN else self.u  # what actually produced this load
            self.u = base * max(0.5, target_load / estimate)
        elif estimate < target_load:
            if gain is not None:
                wanted = target_load / gain  # linear plant: load ∝ batches in flight
                self.u += min(step, max(0.0, self.INCREASE_GAIN * (wanted - self.u)))
            else:
                self.u += step if self.u >= 1.0 else max(0.1, self.u) * step
        if not blocked:
            self.u = min(self.u, peak + 1.0)  # anti-windup: demand, not the gate, is the bottleneck
        self.u = min(cap, max(U_MIN, self.u))
        self.tokens = min(self.tokens, self._burst())

    def _plan_probe(self, now: float, demand: bool) -> None:
        if self._probe is not None or (not self._limited() and self.level != "idle"):
            return
        if not demand or self._pauses or now - self._last_probe < PROBE_MIN_GAP:
            return
        every = (
            PROBE_EVERY_YIELDING
            if self.yielding
            else PROBE_EVERY_IDLE
            if self.level == "idle"
            else self._probe_every
        )
        if self._confirm or self._anomalies >= 2 or now - self._last_clean >= every:
            self._anomalies = 0
            self._probe = "armed"
            guard = min(0.4 * self._dt, max(0.05, 1.5 * self.batch_s))
            self._hold_from = now + self._dt - guard

    def _record(self, span: float, wall: float | None) -> None:
        b = self._bucket
        span = max(0.0, span)
        a = min(1.0, span)  # EWMA over about one second
        for i, value in enumerate((self.self_load, self.total, self.active)):
            self._shown[i] += a * (value - self._shown[i])
        b[0] += self.self_load * span
        b[1] += self.total * span
        b[2] += span
        b[3] = bool(b[3]) or self.yielding
        b[4] = bool(b[4]) or self.paused
        b[5] += self.active * span
        if b[2] >= 0.95:
            self.history.append(
                LoadPoint(
                    t=time.time() if wall is None else wall,
                    self_pct=round(100.0 * b[0] / b[2], 2),
                    total_pct=round(100.0 * b[1] / b[2], 2),
                    active_pct=round(100.0 * b[5] / b[2], 2),
                    yielding=bool(b[3]),
                    paused=bool(b[4]),
                )
            )
            self._bucket = [0.0, 0.0, 0.0, False, False, 0.0]

    def status(self, history: bool = True) -> IoStatus:
        return IoStatus(
            disk=self.disk.id,
            media=self.disk.media,
            level=self.level,
            limit=round(100.0 * self.limit, 2),
            self_pct=round(100.0 * self._shown[0], 2),
            total_pct=round(100.0 * self._shown[1], 2),
            others_pct=round(100.0 * max(0.0, self._shown[1] - self._shown[0]), 2),
            active_pct=round(100.0 * self._shown[2], 2),
            concurrency=self.concurrency,
            inflight=self.inflight,
            duty=round(self.duty, 3),
            auto=self.auto,
            yielding=self.yielding,
            paused=self.paused,
            measured=self.measured,
            low_io_priority=self.low_io_priority,
            q_eff=None if self.q_eff is None else round(self.q_eff, 2),
            history=list(self.history) if history else [],
        )


# --------------------------------------------------------------------------- asyncio wrapper


class Permit:
    """One dispatched batch. Release it when the batch is done (idempotent, thread-safe)."""

    __slots__ = ("_done", "_gov", "started")

    def __init__(self, gov: IoGovernor, started: float) -> None:
        self._gov = gov
        self.started = started
        self._done = False

    def release(self) -> None:
        if self._done:
            return
        self._done = True
        self._gov._release_threadsafe(self)

    def bind(self, fut: asyncio.Future[Any]) -> None:
        """Release when ``fut`` (e.g. ``loop.run_in_executor(...)``) completes."""
        fut.add_done_callback(lambda _f: self.release())

    def __enter__(self) -> Permit:
        return self

    def __exit__(self, *_exc: object) -> None:
        self.release()


class IoGovernor:
    """asyncio front end of :class:`GovernorCore` for one physical disk.

    Usage in a dispatch loop::

        permit = await governor.permit()          # paces and bounds the parallelism
        fut = loop.run_in_executor(pool, work, batch)
        permit.bind(fut)                           # frees the slot when the batch is done
    """

    def __init__(
        self,
        disk: DiskRef,
        *,
        max_workers: int = 8,
        clock: Callable[[], float] = time.monotonic,
    ) -> None:
        self._clock = clock
        self.core = GovernorCore(disk, max_workers=max_workers, now=clock())
        self._futures: dict[Hashable, asyncio.Future[Permit]] = {}
        self._seq = 0
        self._timer: asyncio.TimerHandle | None = None
        self._loop: asyncio.AbstractEventLoop | None = None
        self._listeners: list[Callable[[IoStatus], None]] = []

    # ------------------------------------------------------------------ properties

    @property
    def disk(self) -> DiskRef:
        return self.core.disk

    @property
    def level(self) -> Level:
        return self.core.level

    @property
    def limit(self) -> float:
        """Current limit, percent."""
        return 100.0 * self.core.limit

    @property
    def concurrency(self) -> int:
        return self.core.concurrency

    @property
    def paused(self) -> bool:
        return self.core.paused

    @property
    def yielding(self) -> bool:
        return self.core.yielding

    @property
    def low_io_priority(self) -> bool:
        return self.core.low_io_priority

    def active(self) -> bool:
        """Scans are using this disk (the registry samples faster then)."""
        return self.core.active_recently(self._clock())

    # ------------------------------------------------------------------ dispatch

    async def permit(self) -> Permit:
        """Wait until one more batch may be dispatched on this disk (FIFO across scans)."""
        loop = asyncio.get_running_loop()
        self._loop = loop
        now = self._clock()
        if self.core.try_acquire(now):
            return Permit(self, now)
        self._seq += 1
        key = self._seq
        fut: asyncio.Future[Permit] = loop.create_future()
        self._futures[key] = fut
        self.core.request(key, now)
        self._pump()
        try:
            return await fut
        except asyncio.CancelledError:
            self.core.cancel(key)
            self._futures.pop(key, None)
            if fut.done() and not fut.cancelled():
                fut.result().release()  # granted in the same tick it was cancelled
            raise

    def _pump(self) -> None:
        now = self._clock()
        for key in self.core.grant(now):
            fut = self._futures.pop(key, None)
            if fut is None or fut.done():
                self.core.release(now)  # waiter vanished: give the slot back
                continue
            fut.set_result(Permit(self, now))
        if self._timer is not None:
            self._timer.cancel()
            self._timer = None
        wake = self.core.wake_time(now)
        if wake is not None and self._loop is not None and not self._loop.is_closed():
            self._timer = self._loop.call_later(max(0.0, wake - now), self._pump)

    def _release_threadsafe(self, permit: Permit) -> None:
        loop = self._loop
        try:
            running = asyncio.get_running_loop()
        except RuntimeError:
            running = None
        if loop is not None and running is not loop:
            if not loop.is_closed():
                loop.call_soon_threadsafe(self._release, permit)
            return
        self._release(permit)

    def _release(self, permit: Permit) -> None:
        self.core.release(self._clock(), permit.started)
        self._pump()

    # ------------------------------------------------------------------ control

    def set_level(self, level: Level, custom: float | None = None) -> None:
        self.core.set_level(level, custom, self._clock())
        self._pump()

    def set_manual_concurrency(self, n: int | None) -> None:
        """Fix the parallelism (UI-W2-45); the limit still holds through the duty gate. None: auto."""
        self.core.set_manual_concurrency(n, self._clock())
        self._pump()

    def set_yield_when_busy(self, on: bool) -> None:
        self.core.set_yield_when_busy(on)
        self._pump()

    def set_max_workers(self, n: int) -> None:
        self.core.set_max_workers(n)
        self._pump()

    def pause(self, owner: object = None) -> None:
        """Stop granting permits (batches in flight finish); several owners may pause at once."""
        self.core.pause(owner)
        self._pump()

    def resume(self, owner: object = None) -> None:
        self.core.resume(owner)
        self._pump()

    def observe(self, sample: DiskSample | None, wall: float | None = None) -> None:
        """Called by the registry with every new sample of this disk."""
        self.core.observe(sample, self._clock(), wall)
        self._pump()

    # ------------------------------------------------------------------ status

    def status(self, history: bool = True) -> IoStatus:
        return self.core.status(history)

    def subscribe(self, callback: Callable[[IoStatus], None]) -> Callable[[], None]:
        """Status pushes (2 Hz, from the registry); returns the unsubscribe function."""
        self._listeners.append(callback)

        def unsubscribe() -> None:
            if callback in self._listeners:
                self._listeners.remove(callback)

        return unsubscribe

    @property
    def has_listeners(self) -> bool:
        return bool(self._listeners)

    def push(self) -> None:
        if not self._listeners:
            return
        status = self.status()
        for cb in list(self._listeners):
            try:
                cb(status)
            except Exception:
                logger.exception("io status listener failed")


__all__ = [
    "LEVEL_LIMITS",
    "PROFILES",
    "GovernorCore",
    "IoGovernor",
    "IoStatus",
    "Level",
    "LoadPoint",
    "MediaProfile",
    "Permit",
    "level_limit",
    "setpoint",
]
