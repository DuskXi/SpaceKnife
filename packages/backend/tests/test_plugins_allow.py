"""What plugins may not do: the host's allow-list and permission checks, and in a plugin process
the audit hook (network, writes, programs, native code, reads outside the scans)."""

from __future__ import annotations

from pathlib import Path

import pytest

from spaceknife.plugins.allow import ALLOWED, REFUSED, permission_for
from spaceknife.server.rpc import RpcError
from spaceknife_protocol.errors import ErrorCode

from .plugin_helpers import mock

pytestmark = pytest.mark.timeout(120)


@pytest.fixture(autouse=True)
def _home(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    monkeypatch.setenv("SPACEKNIFE_HOME", str(tmp_path / "home"))


def test_the_allow_list() -> None:
    for method in REFUSED:
        assert method not in ALLOWED
        assert permission_for(method) == (False, None)
    assert permission_for("basket.add") == (True, "basket.propose")
    assert permission_for("tree.summary") == (True, "tree.read")
    assert permission_for("basket.execute")[0] is False
    assert permission_for("plugin.other.thing")[0] is False


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
@pytest.mark.parametrize(
    ("what", "needle"),
    [
        ("tree", "tree.read"),
        ("execute", "may not call basket.execute"),
        ("settings", "may not call settings.update"),
        ("basket", "basket.propose"),
        ("files", "files.read"),
    ],
)
async def test_the_host_refuses(process: bool, what: str, needle: str) -> None:
    async with mock("t.evil", process=process) as host:
        res = await host.call("attempt", what=what, target="/etc/hostname")
        assert res["blocked"], res
        assert needle in res["error"], res


@pytest.mark.parametrize(
    "what", ["net", "resolve", "write", "delete", "spawn", "ctypes", "read"], ids=lambda w: str(w)
)
async def test_the_plugin_process_stops_it(
    what: str, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # the plugin may write to its temporary folder: give it one that does not hold the victim
    own_tmp = tmp_path / "plugin-tmp"
    own_tmp.mkdir()
    for var in ("TMPDIR", "TEMP", "TMP"):
        monkeypatch.setenv(var, str(own_tmp))
    victim = tmp_path / "victim.txt"
    victim.write_text("keep me", encoding="utf-8")
    outside = victim if what in ("write", "delete", "read") else Path("")
    async with mock("t.evil", process=True) as host:
        res = await host.call("attempt", what=what, target=str(outside))
        assert res["blocked"], res
        assert "PermissionError" in res["error"] or "blocked" in res["error"], res
    assert victim.read_text(encoding="utf-8") == "keep me"


async def test_temporary_files_are_allowed() -> None:
    async with mock("t.evil", process=True) as host:
        assert await host.call("attempt", what="temp") == {"blocked": False, "error": ""}


async def test_permission_denied_is_its_own_code() -> None:
    async with mock("t.full", process=False, grant=["tree.read"]) as host:
        with pytest.raises(RpcError) as err:
            await host.call("propose")
        assert err.value.code in (ErrorCode.PERMISSION_DENIED, ErrorCode.PLUGIN_ERROR)
        assert "basket.propose" in err.value.message
        assert host.basket == []
