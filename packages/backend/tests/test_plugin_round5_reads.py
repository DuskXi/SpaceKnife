"""Round 5 (fixes only): ``ctx.files`` gives a disk-read permit back as soon as it has read (a
burst used up, a file read to its end, a ``hash_many`` reader with no more files), not when the host
takes it back by itself 2 s later (``PERMIT_MAX_S``): each read test of example.dupes was exactly 2 s
slower on a disk with an I/O governor."""

from __future__ import annotations

import os
import time

import pytest

from spaceknife.plugins import hostapi as hostapi_mod
from spaceknife_sdk.testing import MockHost, make_tree

from .plugin_helpers import fixture

pytestmark = pytest.mark.timeout(120)


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
async def test_readers_give_their_permits_back_when_they_are_done(process: bool) -> None:
    tree = make_tree({f"f{i}.bin": os.urandom(4096 + i) for i in range(6)})
    async with MockHost.load(fixture("t.tasks"), process=process) as host:
        host.scan(tree)
        await host.call("quick")
        api = host.server.plugins.api
        base = os.path.normcase(os.path.realpath(str(host.files_root))).rstrip("\\/")
        gov = await api._governor_of(base)
        assert gov is not None
        gov.set_level("max")
        gov.set_manual_concurrency(1)  # one read at a time on this disk
        t0 = time.monotonic()
        out = await host.call("hashing")  # one file, then hash_many of six with four readers
        took = time.monotonic() - t0
        assert all(d is not None for d in out["many"]) and api.io_permits() == 0
        # was: a reader that had read its last file kept its permit; the others waited for the
        # host to take it back (PERMIT_MAX_S, 2 s), one after the other
        assert took < hostapi_mod.PERMIT_MAX_S
