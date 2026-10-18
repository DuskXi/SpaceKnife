from __future__ import annotations

from pathlib import Path

from spaceknife.scan.volume import is_volume_root, list_volumes, volume_for


def test_volume_for_temp_dir(tmp_path: Path) -> None:
    vol = volume_for(str(tmp_path))
    assert vol.total > 0
    assert 0 <= vol.free <= vol.total
    assert vol.used == vol.total - vol.free - vol.reserved  # root-only blocks are neither
    assert vol.cluster_size > 0
    assert str(tmp_path).lower().startswith(vol.mount_point.lower().rstrip("\\/")) or vol.mount_point == "/"
    assert is_volume_root(vol.mount_point, vol)
    assert not is_volume_root(str(tmp_path), vol)


def test_list_volumes_includes_the_temp_volume(tmp_path: Path) -> None:
    vols = list_volumes()
    assert vols
    assert volume_for(str(tmp_path)).mount_point in {v.mount_point for v in vols}
