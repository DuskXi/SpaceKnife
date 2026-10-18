"""Start page (W1) backend: path checks, volume media and flags."""

from __future__ import annotations

import os
from pathlib import Path

from spaceknife.scan.media import classify, system_mount
from spaceknife.server.scan_handlers import check_paths
from spaceknife_client.connection import Connection
from spaceknife_protocol import scan as p
from spaceknife_protocol.base import Empty
from spaceknife_protocol.methods import SCAN_CHECK_PATHS, VOLUME_LIST


def test_check_paths_states_and_merging(tmp_path: Path) -> None:
    (tmp_path / "a" / "b").mkdir(parents=True)
    (tmp_path / "c").mkdir()
    (tmp_path / "f.txt").write_text("x", encoding="utf-8")
    a, b, c = str(tmp_path / "a"), str(tmp_path / "a" / "b"), str(tmp_path / "c")
    res = check_paths([f" {b} ", a, "", str(tmp_path / "f.txt"), str(tmp_path / "nope"), c, a])
    states = [(i.state, i.merged_into) for i in res.items]
    assert states == [
        ("ok", a),  # inside a: merged into it
        ("ok", None),
        ("not_dir", None),
        ("missing", None),
        ("ok", None),
        ("ok", a),  # duplicate
    ]
    assert res.roots == [a, c] or res.roots == [c, a]
    assert res.items[0].path == b and res.items[0].normalized == b  # trimmed
    assert res.items[1].volume  # the volume holding the folder
    assert res.items[3].message == "路径不存在" and res.items[2].message == "不是文件夹"


def test_media_classification() -> None:
    assert classify("nvme", False) == "nvme_ssd"
    assert classify("sata", False) == "sata_ssd"
    assert classify("other", True) == "hdd"
    assert classify("usb", True) == "usb_hdd"
    assert classify("usb", False, removable=True) == "usb_flash"
    assert classify("usb", False) == "usb_ssd"
    assert classify("sd", None) == "sd_card"
    assert classify("other", None) == "unknown"
    assert system_mount().endswith(os.sep) or system_mount() == "/"


async def test_volumes_and_checks_over_the_protocol(tmp_path: Path, client: Connection) -> None:
    vols = (await client.call(VOLUME_LIST, Empty())).volumes
    assert vols and sum(v.is_system for v in vols) == 1
    assert all(v.media in p.MediaName.__args__ for v in vols)  # type: ignore[attr-defined]
    res = await client.call(SCAN_CHECK_PATHS, p.PathCheckParams(paths=[str(tmp_path), str(tmp_path / "x")]))
    assert [i.state for i in res.items] == ["ok", "missing"] and res.roots == [str(tmp_path)]
