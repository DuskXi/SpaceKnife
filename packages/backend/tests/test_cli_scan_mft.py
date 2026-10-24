"""``spaceknife scan --mft``: the MFT fast scan from the command line, off by default; a path it
cannot read that way is scanned the hard way with a note why. Tests never start a real helper, so
no administrator prompt ever appears."""

from __future__ import annotations

import io
import os
import sys
from pathlib import Path
from typing import Any

import pytest
from rich.console import Console

from spaceknife import cli_scan, helper
from spaceknife.helper import HelperDenied
from spaceknife.scan import mft
from spaceknife.scan.scanner import RootInfo
from spaceknife.scan.volume import VolumeInfo
from spaceknife.server.scans import snapshot_info
from spaceknife.store import snapshot

from .test_m4_scan import FakeClient, FakeShared, _sample_table
from .test_scan_server import isolated_home  # noqa: F401  (fixture)
from .tree_fixture import build_tree


@pytest.fixture
def out(monkeypatch: pytest.MonkeyPatch, isolated_home: None) -> io.StringIO:  # noqa: F811
    """The command's console output, wide enough that no line wraps."""
    buf = io.StringIO()
    monkeypatch.setattr(cli_scan, "console", Console(file=buf, width=400, color_system=None))
    return buf


def scan(path: Path, **options: Any) -> None:
    cli_scan.scan([str(path)], top=3, depth=1, no_snapshot=True, **options)


def _no_helper() -> Any:
    raise AssertionError("no helper may start")


def _volume(fs: str = "NTFS", remote: bool = False) -> VolumeInfo:
    return VolumeInfo(
        mount_point="C:\\",
        device="C:",
        fs_type=fs,
        label="",
        total=1,
        free=0,
        cluster_size=4096,
        is_remote=remote,
        is_removable=False,
    )


def test_which_roots_the_mft_scan_can_read(monkeypatch: pytest.MonkeyPatch) -> None:
    """The app and the command line share the rule, and the reason says what is wrong."""
    whole = RootInfo("C:\\", 1, _volume(), True)
    monkeypatch.setattr(sys, "platform", "win32")
    assert mft.mft_unsupported([whole]) is None
    assert "一个卷" in (mft.mft_unsupported([whole, whole]) or "")
    assert "网络" in (mft.mft_unsupported([RootInfo("Z:\\", 1, _volume(remote=True), True)]) or "")
    assert "exFAT" in (mft.mft_unsupported([RootInfo("E:\\", 1, _volume("exFAT"), True)]) or "")
    assert "只能扫整个卷" in (mft.mft_unsupported([RootInfo("C:\\Users", 1, _volume(), False)]) or "")
    assert "认不出" in (mft.mft_unsupported([RootInfo("Q:\\", 1, None, True)]) or "")
    monkeypatch.setattr(sys, "platform", "linux")
    assert "Windows" in (mft.mft_unsupported([whole]) or "")


def test_mft_reads_the_volume_and_the_snapshot_says_so(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, out: io.StringIO
) -> None:
    root = tmp_path / "vol"
    root.mkdir()
    client = FakeClient(_sample_table())
    monkeypatch.setattr(cli_scan, "mft_helper", lambda: FakeShared(client))
    monkeypatch.setattr(mft, "mft_unsupported", lambda roots: None)
    monkeypatch.setattr(helper, "is_elevated", lambda: True)
    saved = tmp_path / "vol.sks"
    scan(root, use_mft=True, save=saved, profile=True)
    text = out.getvalue()
    assert "方式 MFT 快速扫描" in text and "说明：这是硬扫描" not in text
    assert "读取并解析 $MFT" in text and "建树" in text  # --profile shows the MFT phases
    assert client.duty == 1.0  # no --disk-limit: flat out
    assert snapshot_info(saved, snapshot.read_header(saved)).method == "mft"
    store, _ = snapshot.load(saved)
    assert len(store) > 40 and store.find(os.path.join(str(root), mft.META_FOLDER, "$MFT")) is not None


def test_disk_limit_paces_the_mft_read(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, out: io.StringIO
) -> None:
    root = tmp_path / "vol"
    root.mkdir()
    client = FakeClient(_sample_table())
    monkeypatch.setattr(cli_scan, "mft_helper", lambda: FakeShared(client))
    monkeypatch.setattr(mft, "mft_unsupported", lambda roots: None)
    monkeypatch.setattr(helper, "is_elevated", lambda: False)
    scan(root, use_mft=True, disk_limit="quiet")
    assert client.duty == pytest.approx(0.35)
    assert "需要管理员权限" in out.getvalue()  # said before Windows asks
    assert [cli_scan.mft_duty(x) for x in (None, ("max", None), ("idle", None), ("custom", 2.0))] == [
        1.0,
        1.0,
        0.2,
        0.05,
    ]


def test_a_refused_prompt_falls_back_to_a_hard_scan(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, out: io.StringIO
) -> None:
    facts = build_tree(tmp_path / "root")
    monkeypatch.setattr(cli_scan, "mft_helper", lambda: FakeShared(error=HelperDenied()))
    monkeypatch.setattr(mft, "mft_unsupported", lambda roots: None)
    monkeypatch.setattr(helper, "is_elevated", lambda: True)
    saved = tmp_path / "root.sks"
    scan(facts.root, use_mft=True, save=saved)
    text = out.getvalue()
    assert "MFT 快速扫描不可用：用户拒绝了管理员授权；改用硬扫描" in text
    assert "方式 硬扫描（用户拒绝了管理员授权）" in text
    header = snapshot.read_header(saved)
    assert snapshot_info(saved, header).method == "hard" and header.stats["files"] == facts.files


def test_a_path_it_cannot_read_is_scanned_the_hard_way(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, out: io.StringIO
) -> None:
    facts = build_tree(tmp_path / "root")
    monkeypatch.setattr(cli_scan, "mft_helper", _no_helper)
    saved = tmp_path / "root.sks"
    scan(facts.root, use_mft=True, save=saved)
    text = out.getvalue()
    reason = mft.mft_unsupported(cli_scan.Scanner([str(facts.root)]).roots)
    assert reason and f"不支持 MFT 快速扫描：{reason}。改用硬扫描。" in text  # a folder, or not Windows
    assert f"方式 硬扫描（{reason}）" in text
    assert snapshot.read_header(saved).stats["files"] == facts.files


def test_without_mft_nothing_changes(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, out: io.StringIO
) -> None:
    facts = build_tree(tmp_path / "root")
    monkeypatch.setattr(cli_scan, "mft_helper", _no_helper)
    scan(facts.root)
    text = out.getvalue()
    assert "方式" not in text and "MFT 快速扫描" not in text.replace("加 --mft 改用 MFT 快速扫描", "")
