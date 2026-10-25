"""Acceptance: a plugin's settings and the interface language reach its process and what the host
shows of it (round 5's probe ``growthwatch-race``, round 4's gap 9 for legends, the language
switch)."""

from __future__ import annotations

import shutil
from pathlib import Path
from typing import Any

import pytest

from .harness import FIXTURES, Backend

pytestmark = pytest.mark.timeout(240)

EARLY = "acc.early"


async def _enable(backend: Backend, tmp_path: Path, runtime: str) -> str:
    """The app's flow up to enabling; for the in-process runtime a copy that asks for it, with
    developer mode on."""
    folder = FIXTURES / EARLY
    if runtime == "inprocess":
        await backend.settings({"advanced.plugin_dev_mode": True})
        folder = tmp_path / EARLY
        shutil.copytree(FIXTURES / EARLY, folder, ignore=shutil.ignore_patterns("__pycache__"))
        toml = folder / "plugin.toml"
        toml.write_text(
            toml.read_text(encoding="utf-8").replace(
                'entry = "plugin.py"', 'entry = "plugin.py"\nruntime = "inprocess"'
            ),
            encoding="utf-8",
        )
    res = await backend.call("plugin.install", {"path": str(folder), "replace": True})
    desc = await backend.call("plugin.describe", {"id": res["id"]})
    grant = [p["id"] for p in desc["plugin"]["permissions"]]
    info = await backend.call(
        "plugin.enable", {"id": res["id"], "hash": desc["plugin"]["hash"], "grant": grant}
    )
    assert info["state"] == "enabled", info
    return str(res["id"])


@pytest.mark.parametrize("runtime", ["process", "inprocess"])
async def test_settings_changed_right_after_enabling_reach_the_plugin(
    backend: Backend, tmp_path: Path, runtime: str
) -> None:
    """The plugin has a task that starts with it: enabling starts its process, which takes half a
    second to load. Settings changed at once (the user turns it on and edits its settings beside
    the switch) were stored but never reached the process until the next change; growthwatch went
    on with its default threshold. Now the first call already sees them, and so does the
    ``settings`` hook."""
    pid = await _enable(backend, tmp_path, runtime)
    await backend.settings({f"plugin.{pid}.rules": "downloads > 3 MB", f"plugin.{pid}.size": 7})
    if runtime == "process":
        process = (await backend.plugin(pid)).get("process") or {}
        assert process.get("state") != "running", process  # changed while it starts: the case
    now = await backend.pcall(pid, "now")
    assert (now["rules"], now["size"]) == ("downloads > 3 MB", 7), now
    heard = await backend.until(
        "the settings hook to see the new values",
        lambda: backend.pcall(pid, "now"),
        lambda n: any(h["rules"] == "downloads > 3 MB" and h["size"] == 7 for h in n["heard"]),
    )
    assert all(h["rules"] == "downloads > 3 MB" for h in heard["heard"][-1:])
    # and a change once it runs, as always
    await backend.settings({f"plugin.{pid}.size": 9})
    assert (await backend.pcall(pid, "now"))["size"] == 9


async def _legend(backend: Backend, mode: str) -> list[str]:
    ext = await backend.extensions()
    found = [c for c in ext["color_modes"] if c["id"] == mode]
    return list(found[0]["legend"]) if found else []


async def test_the_legend_follows_a_settings_change(backend: Backend, probe: str) -> None:
    """A colour mode's legend worked out by the plugin from its settings: the colour menu shows the
    new one after the setting changed (round 4's gap 9: it kept the legend asked before the plugin
    had the new value)."""
    mode = f"{probe}.bands"
    await backend.until(
        "the first legend", lambda: _legend(backend, mode), lambda lg: lg == ["其他", "1d", "1w"]
    )
    await backend.settings({f"plugin.{probe}.bands": "2d, 3w, 1y"})
    await backend.until(
        "the legend after the change",
        lambda: _legend(backend, mode),
        lambda lg: lg == ["其他", "2d", "3w", "1y"],
    )
    seen = await backend.dump(probe)
    assert seen["settings"] and seen["settings"][-1] == {"changed": ["bands"], "bands": "2d, 3w, 1y"}
    await backend.settings({f"plugin.{probe}.bands": "1d"})
    await backend.until(
        "the legend after a second change", lambda: _legend(backend, mode), lambda lg: lg == ["其他", "1d"]
    )


async def _texts(backend: Backend, pid: str) -> dict[str, Any]:
    ext = await backend.extensions()
    cmds = await backend.call("palette.commands", {})
    return {
        "commands": sorted(c["name"] for c in cmds["commands"] if c.get("plugin") == pid),
        "field": [f["title"] for f in ext["node_fields"] if f["plugin"] == pid],
        "colour": [c["title"] for c in ext["color_modes"] if c["plugin"] == pid],
        "legend": [c["legend"] for c in ext["color_modes"] if c["plugin"] == pid],
        "report": [r["title"] for r in ext["reports"] if r["plugin"] == pid],
    }


ZH = {
    "commands": sorted(["探针：动作", "探针：在哪", "探针：拒绝", "探针：出错", "探针：慢"]),
    "field": ["探针：哪棵树"],
    "colour": ["探针分档"],
    "legend": [["其他", "1d", "1w"]],
    "report": ["探针：报告"],
}
EN = {
    "commands": sorted(["Probe: act", "Probe: where", "Probe: refuses", "Probe: breaks", "Probe: dawdles"]),
    "field": ["Probe: which tree"],
    "colour": ["Probe bands"],
    "legend": [["Other", "1d", "1w"]],
    "report": ["Probe: report"],
}


async def test_a_language_switch_reaches_the_texts_and_the_plugin(
    backend: Backend, probe: str, data: Path
) -> None:
    """general.language from Chinese to English: the plugin's texts in the menus, the details
    panel and the colour menu follow (the legend too, worked out again), the plugin's ``lang`` hook
    runs, and its calls get the new language; and back."""
    (data / "a.bin").write_bytes(b"a" * 1000)
    sid = await backend.scan(data)
    node = await backend.find(sid, data / "a.bin")
    assert node is not None
    await backend.until("the Chinese texts", lambda: _texts(backend, probe), lambda t: t == ZH)

    await backend.settings({"general.language": "en"})
    await backend.until("the English texts", lambda: _texts(backend, probe), lambda t: t == EN)
    await backend.until("the lang hook", lambda: backend.dump(probe), lambda d: d["langs"][-1:] == ["en"])
    said = (await backend.pcall(probe, "about", scan_id=sid))["said"]
    assert said["lang"] == "en"
    await backend.until(
        "a field asked in English",
        lambda: backend.fields(probe, sid, node),
        lambda f: bool(f) and f[0]["title"] == "Probe: which tree" and f[0].get("value") not in (None, ""),
    )
    assert (await backend.calls(probe, "field"))[-1]["lang"] == "en"

    await backend.settings({"general.language": "zh-CN"})
    await backend.until("the Chinese texts again", lambda: _texts(backend, probe), lambda t: t == ZH)
    await backend.until(
        "the lang hook again", lambda: backend.dump(probe), lambda d: d["langs"][-1:] == ["zh-CN"]
    )
    assert (await backend.pcall(probe, "about", scan_id=sid))["said"]["lang"] == "zh-CN"
