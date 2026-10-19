from __future__ import annotations

from pathlib import Path

from spaceknife.scan.volume import (
    is_volume_root,
    list_volumes,
    parse_firmlinks,
    through_firmlinks,
    volume_for,
    volume_group_twins,
)


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


# --------------------------------------------------------------------------- macOS volume groups

FIRMLINKS_TEXT = "/AppleInternal\tAppleInternal\n/Applications\tApplications\n/Library\tLibrary\n/System/Library/Caches\tSystem/Library/Caches\n/Users\tUsers\n/private\tprivate\n/usr/local\tusr/local\n\n# not a firmlink\n"


def test_volume_group_twins_are_mounts_below_a_root_with_its_device() -> None:
    mounts = [
        "/",
        "/dev",
        "/System/Volumes/VM",
        "/System/Volumes/Preboot",
        "/System/Volumes/Data",
        "/Volumes/USB",
    ]
    devs = {
        "/": 21,
        "/System/Volumes/Data": 21,
        "/System/Volumes/VM": 25,
        "/System/Volumes/Preboot": 23,
        "/dev": 30,
        "/Volumes/USB": 40,
    }
    twins = volume_group_twins("/", mounts, devs.__getitem__)
    assert twins == ("/System/Volumes/Data",)
    # the Data volume scanned by itself, a folder that is no mount point, another volume: none
    assert volume_group_twins("/System/Volumes/Data", mounts, devs.__getitem__) == ()
    assert volume_group_twins("/System", mounts, devs.__getitem__) == ()
    assert volume_group_twins("/Volumes/USB", mounts, devs.__getitem__) == ()


def test_firmlinks_map_data_volume_paths_back_under_root() -> None:
    links = parse_firmlinks(FIRMLINKS_TEXT)
    assert ("/Users", "Users") in links and len(links) == 7
    assert links[0][1] == "System/Library/Caches"  # the deepest first
    data = "/System/Volumes/Data"
    assert (
        through_firmlinks(data + "/Users/lenny/Documents/a.txt", data, links)
        == "/Users/lenny/Documents/a.txt"
    )
    assert through_firmlinks(data + "/Users", data, links) == "/Users"
    assert through_firmlinks(data + "/usr/local/bin/x", data, links) == "/usr/local/bin/x"
    assert through_firmlinks(data + "/System/Library/Caches/c", data, links) == "/System/Library/Caches/c"
    assert through_firmlinks(data + "/.Spotlight-V100/x", data, links) is None  # no firmlink: not in the tree
    assert through_firmlinks(data + "/UsersX/y", data, links) is None
