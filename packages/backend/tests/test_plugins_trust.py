"""Trust: the user's confirmation with the folder's hash and the granted permissions, kept in
``config_dir()/plugins.json``; anything that changes after it asks again (docs/plugins.md)."""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from spaceknife.extensions import EXTENSIONS
from spaceknife.plugins import paths as ppaths
from spaceknife.plugins.store import TrustStore
from spaceknife.server import Server, ServerConfig
from spaceknife.server.rpc import RpcError
from spaceknife_protocol.errors import ErrorCode

from .plugin_helpers import TOKEN, copy_plugin

pytestmark = pytest.mark.timeout(120)

T_FULL = ["tree.read", "files.read", "basket.propose"]


@pytest.fixture(autouse=True)
def _home(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    monkeypatch.setenv("SPACEKNIFE_HOME", str(tmp_path / "home"))


async def _server() -> Server:
    srv = Server(ServerConfig(token=TOKEN, hello_timeout=0.5, ping_interval=None))
    await srv.start()
    return srv


def test_the_trust_file(tmp_path: Path) -> None:
    file = tmp_path / "plugins.json"
    store = TrustStore(file)
    assert store.get("a.b") is None
    store.link(tmp_path)
    store.set_enabled("a.b", True)  # nothing to switch on before it was confirmed
    assert store.get("a.b") is None
    again = TrustStore(file)
    assert again.doc.dev == [str(tmp_path.resolve())]
    again.unlink(tmp_path)
    assert TrustStore(file).doc.dev == []
    # an unreadable file starts empty (and is not overwritten until something changes)
    file.write_text("{ not json", encoding="utf-8")
    broken = TrustStore(file)
    assert broken.doc.plugins == {} and file.read_text(encoding="utf-8") == "{ not json"


async def test_confirmed_once_then_kept_across_restarts() -> None:
    copy_plugin("t.full", ppaths.plugins_dir())
    srv = await _server()
    try:
        rec = srv.plugins.records["t.full"]
        assert rec.state == "needs_confirm" and rec.granted == frozenset()
        assert not EXTENSIONS.owned("t.full")  # nothing of it before the user says yes
        # every permission it asks for, or none: a partial grant is refused
        with pytest.raises(RpcError) as err:
            await srv.plugins.enable("t.full", rec.hash, ["tree.read"])
        assert err.value.code == ErrorCode.INVALID_PARAMS
        info = await srv.plugins.enable("t.full", rec.hash, T_FULL)
        assert info.state == "enabled"
        entry = json.loads(ppaths.trust_file().read_text(encoding="utf-8"))["plugins"]["t.full"]
        assert entry["enabled"] and entry["hash"] == rec.hash and sorted(entry["granted"]) == sorted(T_FULL)
        assert entry["version"] == "0.2.0" and entry["confirmed_at"]
    finally:
        await srv.close()
    srv = await _server()
    try:
        rec = srv.plugins.records["t.full"]
        assert rec.state == "enabled" and rec.granted == frozenset(T_FULL)
        await srv.plugins.disable("t.full")
        assert srv.plugins.records["t.full"].state == "disabled" and not EXTENSIONS.owned("t.full")
    finally:
        await srv.close()
    srv = await _server()
    try:
        assert srv.plugins.records["t.full"].state == "disabled"  # off stays off
        rec = srv.plugins.records["t.full"]
        info = await srv.plugins.enable("t.full", rec.hash, T_FULL)
        assert info.state == "enabled"
    finally:
        await srv.close()


async def test_asking_for_more_asks_again() -> None:
    folder = copy_plugin("t.full", ppaths.plugins_dir())
    srv = await _server()
    try:
        rec = srv.plugins.records["t.full"]
        await srv.plugins.enable("t.full", rec.hash, T_FULL)
    finally:
        await srv.close()
    manifest = folder / "plugin.toml"
    text = manifest.read_text(encoding="utf-8")
    manifest.write_text(
        text.replace(
            'permissions = ["tree.read", "files.read", "basket.propose"]', 'permissions = ["tree.read"]'
        ),
        encoding="utf-8",
    )
    srv = await _server()
    try:
        # fewer permissions, but other files: the hash pins the folder, so it asks again
        rec = srv.plugins.records["t.full"]
        assert rec.state == "changed" and rec.granted == frozenset()
        with pytest.raises(RpcError):  # the old hash does not confirm the new files
            await srv.plugins.enable("t.full", "sha256:" + "0" * 64, ["tree.read"])
        info = await srv.plugins.enable("t.full", rec.hash, ["tree.read"])
        assert info.state == "enabled" and srv.plugins.records["t.full"].granted == frozenset({"tree.read"})
    finally:
        await srv.close()


async def test_uninstall_forgets_the_confirmation(monkeypatch: pytest.MonkeyPatch) -> None:
    import shutil

    from spaceknife.plugins import install as inst

    # the folder goes to the recycle bin; not the real one in a test
    trashed: list[str] = []

    def to_bin(paths: list[str]) -> list[str]:
        trashed.extend(paths)
        for p in paths:
            shutil.rmtree(p)
        return []

    monkeypatch.setattr(inst, "TRASH", to_bin)
    monkeypatch.setattr(inst, "REMOVE_OUTRIGHT", lambda _folder: False)  # as for a real data folder
    copy_plugin("t.decl", ppaths.plugins_dir())
    srv = await _server()
    try:
        rec = srv.plugins.records["t.decl"]
        await srv.plugins.enable("t.decl", rec.hash, list(rec.requested))
        assert EXTENSIONS.owned("t.decl")
        await srv.plugins.uninstall("t.decl", remove_data=True)
        assert "t.decl" not in srv.plugins.records and not EXTENSIONS.owned("t.decl")
        assert TrustStore().get("t.decl") is None
        assert not (ppaths.plugins_dir() / "t.decl").exists()
        assert trashed == [str(ppaths.plugins_dir() / "t.decl")]
    finally:
        await srv.close()


def test_only_plugin_folders_go_to_the_bin(tmp_path: Path) -> None:
    from spaceknife.plugins import install as inst

    # anything outside the plugin folders is refused before the recycle bin is asked
    assert inst._trash([str(tmp_path)]) == [f"{tmp_path}: not a plugin folder"]
    assert inst._trash([str(ppaths.plugins_dir())]) == [f"{ppaths.plugins_dir()}: not a plugin folder"]
    assert tmp_path.exists()
