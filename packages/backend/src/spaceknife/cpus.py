"""The CPUs this process may use: what sizes CPU work (the native walker's calling threads, the
compute pool, MFT and snapshot threads, the Python walker's processes).

A process held to a few CPUs (an affinity, a job object, a container's quota) that sized its threads
by the machine's CPUs only made them wait for each other; ``os.cpu_count()`` counts the machine's.
The native module counts them (``cpu_count``: Windows affinity across processor groups, Linux
affinity and cgroup v2 quota, docs/native.md 2.2); without it, the affinity where Python can read it
(Linux), else ``os.cpu_count()``.
"""

from __future__ import annotations

import contextlib
import functools
import os

from . import native


@functools.cache
def usable_cpus() -> int:
    """At least 1; read once per process."""
    mod = native.module()
    if mod is not None and native.has("cpu_count"):
        try:
            return max(1, int(mod.cpu_count()))
        except Exception:  # an older or broken build: the fallback below
            pass
    return _fallback()


def _fallback() -> int:
    n = os.cpu_count() or 1
    affinity = getattr(os, "sched_getaffinity", None)
    if affinity is not None:
        with contextlib.suppress(OSError):
            n = min(n, len(affinity(0))) or n
    return max(1, n)


__all__ = ["usable_cpus"]
