"""ops.basket: path containment, small-item groups, freeable space (cloud, hard links), refresh.

Everything here works on synthetic in-memory stores: no file is created or touched.
"""

from __future__ import annotations

import os
import time
from pathlib import Path
from typing import Any

import numpy as np
import pytest

from spaceknife.ops import basket as B
from spaceknife.ops.basket import TEXT, Basket, Merged
from spaceknife.ops.paths import TEXT as PATH_TEXT
from spaceknife.ops.trash import OpResult
from spaceknife.store.nodes import Flag, NodeStore

from .store_builder import build, find

KB4 = 4096

SPEC: dict[str, Any] = {
    "docs": {"a.txt": 1000, "b.txt": 2000, "sub": {"c.bin": 5000}},
    "small": {**{f"s{i}.log": 10 + i for i in range(6)}, "big.iso": 100_000},
    "links": {"x.dat": 8192},
    "other": {"y.dat": 8192},
    "cloud": {"online.docx": 50_000, "local.docx": 3000},
}


@pytest.fixture
def root(tmp_path: Path) -> str:
    return str(tmp_path / "scan")


@pytest.fixture
def store(root: str) -> NodeStore:
    return build(SPEC, root=root, cluster=KB4)


def basket() -> Basket:
    return Basket(volume_of=lambda root: "VOL")


def path_of(root: str, rel: str) -> str:
    return os.path.join(root, *rel.split("/"))


def link(store: NodeStore, rel: str, ino: int, nlink: int, *, dup: bool = False) -> int:
    node = find(store, rel)
    store.cols["ino"][node] = ino
    store.cols["nlink"][node] = nlink
    if dup:
        store.cols["flags"][node] |= np.uint32(Flag.HARDLINK_DUP)
    return node


# --------------------------------------------------------------------------- containment


def test_add_a_file(store: NodeStore, root: str) -> None:
    b = basket()
    res = b.add(store, "scan1", [find(store, "docs/a.txt")])
    assert len(res.added) == 1 and not res.merged and not res.refused
    (item,) = b.items()
    assert item.id == res.added[0]
    assert (item.path, item.name, item.kind, item.scan_id) == (
        path_of(root, "docs/a.txt"),
        "a.txt",
        "file",
        "scan1",
    )
    assert (item.size, item.alloc, item.files, item.dirs) == (1000, KB4, 1, 0)
    assert (item.volume, item.status, item.note, item.members) == ("VOL", "ready", "", [])
    assert b.contains(item.path) == item.id
    assert b.contains(path_of(root, "docs")) is None
    again = basket().add(store, "scan1", [find(store, "docs/a.txt")])
    assert again.added == [item.id]  # the id is a hash of the path: stable across baskets


def test_same_path_twice_is_merged(store: NodeStore, root: str) -> None:
    b = basket()
    first = b.add(store, "s", [find(store, "docs/a.txt")]).added[0]
    res = b.add(store, "s", [find(store, "docs/a.txt")])
    assert res.added == [] and res.merged == [Merged(path_of(root, "docs/a.txt"), first)]
    assert len(b) == 1


def test_child_of_an_item_is_merged(store: NodeStore, root: str) -> None:
    b = basket()
    docs = b.add(store, "s", [find(store, "docs")]).added[0]
    res = b.add(store, "s", [find(store, "docs/sub/c.bin"), find(store, "docs/b.txt")])
    assert res.added == []
    assert res.merged == [
        Merged(path_of(root, "docs/sub/c.bin"), docs),
        Merged(path_of(root, "docs/b.txt"), docs),
    ]
    assert b.contains(path_of(root, "docs/sub/c.bin")) == docs


def test_ancestor_absorbs_items(store: NodeStore, root: str) -> None:
    b = basket()
    first = b.add(store, "s", [find(store, "docs/a.txt"), find(store, "docs/sub/c.bin")]).added
    assert len(first) == 2
    res = b.add(store, "s", [find(store, "docs")])
    assert sorted(res.absorbed) == sorted(first)
    (item,) = b.items()
    assert item.id == res.added[0] and item.kind == "dir" and item.name == "docs"
    assert (item.size, item.alloc, item.files, item.dirs) == (8000, 4 * KB4, 3, 1)
    assert b.freeable() == {"VOL": 4 * KB4}


def test_child_and_parent_in_one_call(store: NodeStore, root: str) -> None:
    b = basket()
    res = b.add(store, "s", [find(store, "docs/sub/c.bin"), find(store, "docs/sub"), find(store, "docs")])
    (docs,) = res.added
    assert res.absorbed == []  # added and absorbed in the same call: reported as merged
    assert {m.path for m in res.merged} == {path_of(root, "docs/sub/c.bin"), path_of(root, "docs/sub")}
    assert {m.into for m in res.merged} == {docs}
    assert [i.id for i in b.items()] == [docs]


def test_refusals(store: NodeStore, root: str, tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    b = basket()
    res = b.add(store, "s", [0, len(store) + 5, -1])
    assert [r.reason for r in res.refused] == [TEXT["bad_node"]] * 3
    store.cols["flags"][find(store, "cloud")] |= np.uint32(Flag.VIRTUAL)  # an MFT pseudo folder
    res = b.add(store, "s", [find(store, "cloud/local.docx")])
    assert [r.reason for r in res.refused] == [TEXT["virtual"]]
    # protected: everything under SpaceKnife's own data folder
    monkeypatch.setenv("SPACEKNIFE_HOME", str(tmp_path / "skhome"))
    own = build({"snapshots": {"c.sks": 10}}, root=str(tmp_path / "skhome" / "data"))
    res = b.add(own, "own", [find(own, "snapshots/c.sks"), next(iter(own.roots()))])
    assert [r.reason for r in res.refused] == [PATH_TEXT["app"]] * 2
    assert len(b) == 0


def test_gone_rows_are_refused(store: NodeStore) -> None:
    node = find(store, "docs/a.txt")
    store.cols["flags"][node] |= np.uint32(Flag.GONE)
    res = basket().add(store, "s", [node])
    assert [r.reason for r in res.refused] == [TEXT["bad_node"]]


# --------------------------------------------------------------------------- small-item groups


def small_members(store: NodeStore, *names: int) -> list[int]:
    return [find(store, f"small/s{i}.log") for i in names]


def test_group_is_one_item(store: NodeStore, root: str) -> None:
    b = basket()
    folder = find(store, "small")
    res = b.add(store, "s", [folder], members=small_members(store, *range(6)))
    (gid,) = res.added
    (item,) = b.items()
    assert item.id == gid and item.kind == "group" and item.path == path_of(root, "small")
    assert item.name == TEXT["group_name"].format(folder="small", count=6)
    assert item.members == [path_of(root, f"small/s{i}.log") for i in range(6)]
    assert (item.size, item.alloc, item.files, item.dirs) == (sum(range(10, 16)), 6 * KB4, 6, 0)
    # a member (or the same member again) is covered; a sibling outside the group is not
    res = b.add(store, "s", [find(store, "small/s3.log")])
    assert res.merged == [Merged(path_of(root, "small/s3.log"), gid)]
    res = b.add(store, "s", [find(store, "small/big.iso")])
    assert len(res.added) == 1 and len(b) == 2
    assert b.contains(path_of(root, "small")) is None  # the group covers its members only


def test_group_grows_and_folder_absorbs_it(store: NodeStore, root: str) -> None:
    b = basket()
    folder = find(store, "small")
    gid = b.add(store, "s", [folder], members=small_members(store, 0, 1, 2)).added[0]
    res = b.add(store, "s", [folder], members=small_members(store, 2, 3))
    assert res.added == [gid]  # the same group, grown
    assert res.merged == [Merged(path_of(root, "small/s2.log"), gid)]
    (item,) = b.items()
    assert len(item.members) == 4 and item.alloc == 4 * KB4
    assert item.name == TEXT["group_name"].format(folder="small", count=4)
    big = b.add(store, "s", [find(store, "small/big.iso")]).added[0]
    res = b.add(store, "s", [folder])
    assert sorted(res.absorbed) == sorted([gid, big])
    (item,) = b.items()
    assert item.kind == "dir" and item.alloc == 6 * KB4 + 102400


def test_group_member_rules(store: NodeStore, root: str) -> None:
    b = basket()
    folder = find(store, "small")
    res = b.add(store, "s", [folder], members=[find(store, "docs/a.txt"), *small_members(store, 0)])
    assert [r.reason for r in res.refused] == [TEXT["not_member"]]
    assert len(b.items()[0].members) == 1
    with pytest.raises(ValueError, match="exactly one folder"):
        b.add(store, "s", [folder, folder], members=small_members(store, 1))
    res = b.add(store, "s", [find(store, "small/s5.log")], members=[])
    assert [r.reason for r in res.refused] == [TEXT["not_folder"]]
    # a group inside a folder that is already in the basket is merged as a whole
    docs = b.add(store, "s", [find(store, "docs")]).added[0]
    res = b.add(store, "s", [find(store, "docs/sub")], members=[find(store, "docs/sub/c.bin")])
    assert res.merged == [Merged(path_of(root, "docs/sub"), docs)]


def test_targets_expand_groups(store: NodeStore, root: str) -> None:
    b = basket()
    gid = b.add(store, "s", [find(store, "small")], members=small_members(store, 0, 1)).added[0]
    fid = b.add(store, "s", [find(store, "docs/b.txt")]).added[0]
    targets = b.targets()
    assert [(t.item, t.path, t.size) for t in targets] == [
        (gid, path_of(root, "small/s0.log"), KB4),
        (gid, path_of(root, "small/s1.log"), KB4),
        (fid, path_of(root, "docs/b.txt"), KB4),
    ]
    assert [t.path for t in b.targets([fid])] == [path_of(root, "docs/b.txt")]
    assert b.lock_targets() == [t.path for t in targets]


# --------------------------------------------------------------------------- freeable (§2.3)


def test_hard_links_count_only_when_all_names_are_in(store: NodeStore) -> None:
    x = link(store, "links/x.dat", ino=77, nlink=2)
    y = link(store, "other/y.dat", ino=77, nlink=2, dup=True)
    store.reaggregate()
    b = basket()
    xid = b.add(store, "s", [x]).added[0]
    item = b.get(xid)
    assert item is not None and item.alloc == 0
    assert item.note == TEXT["links_file"].format(count=1)
    assert b.freeable() == {"VOL": 0}
    yid = b.add(store, "s", [y]).added[0]
    got = {i.id: i for i in b.items()}
    assert got[xid].alloc == 8192 and got[yid].alloc == 0  # counted once, for the first item
    assert got[xid].note == got[yid].note == ""
    assert b.freeable() == {"VOL": 8192}
    b.remove([xid])
    item = b.get(yid)
    assert item is not None and item.alloc == 0 and item.note == TEXT["links_file"].format(count=1)


def test_hard_links_inside_folders(store: NodeStore) -> None:
    link(store, "links/x.dat", ino=5, nlink=2)
    link(store, "other/y.dat", ino=5, nlink=2, dup=True)
    store.reaggregate()
    b = basket()
    lid = b.add(store, "s", [find(store, "links")]).added[0]
    item = b.get(lid)
    assert item is not None and item.alloc == 0 and item.note == TEXT["links_dir"].format(count=1)
    b.add(store, "s", [find(store, "other")])
    assert b.freeable() == {"VOL": 8192}
    assert all(i.note == "" for i in b.items())


def test_hard_link_with_a_name_outside_the_scan(store: NodeStore) -> None:
    x = link(store, "links/x.dat", ino=9, nlink=3)
    y = link(store, "other/y.dat", ino=9, nlink=3, dup=True)
    store.reaggregate()
    b = basket()
    b.add(store, "s", [x, y])
    assert b.freeable() == {"VOL": 0}  # the third name keeps the data alive
    assert {i.note for i in b.items()} == {TEXT["links_file"].format(count=1)}


def test_cloud_placeholders_free_nothing(store: NodeStore) -> None:
    online = find(store, "cloud/online.docx")
    store.cols["flags"][online] |= np.uint32(Flag.CLOUD)  # its alloc column is still non-zero here
    b = basket()
    oid = b.add(store, "s", [online]).added[0]
    item = b.get(oid)
    assert item is not None and item.alloc == 0 and item.size == 50_000
    b.add(store, "s", [find(store, "cloud")])
    (folder,) = b.items()
    assert folder.alloc == KB4  # local.docx only


def test_freeable_per_volume(tmp_path: Path) -> None:
    s1 = build({"f.bin": 10_000}, root=str(tmp_path / "one"), cluster=KB4)
    s2 = build({"g.bin": 5000}, root=str(tmp_path / "two"), cluster=KB4)
    volumes = {str(tmp_path / "one"): "C:\\", str(tmp_path / "two"): "D:\\"}
    b = Basket(volume_of=volumes.__getitem__)
    b.add(s1, "a", [find(s1, "f.bin")])
    b.add(s2, "b", [find(s2, "g.bin")])
    assert b.freeable() == {"C:\\": 12288, "D:\\": 8192}
    assert [i.volume for i in b.items()] == ["C:\\", "D:\\"]


def test_default_volume_of_a_real_folder(tmp_path: Path) -> None:
    from spaceknife.scan.volume import volume_for

    assert B.default_volume_of(str(tmp_path)) == volume_for(str(tmp_path)).mount_point
    assert B.default_volume_of(str(tmp_path / "not" / "there")) != ""


# --------------------------------------------------------------------------- refresh


def test_refresh_after_tree_changes(store: NodeStore, root: str) -> None:
    b = basket()
    aid = b.add(store, "s", [find(store, "docs/a.txt")]).added[0]
    sid = b.add(store, "s", [find(store, "docs/sub")]).added[0]
    gid = b.add(store, "s", [find(store, "small")], members=small_members(store, 0, 1)).added[0]
    changed = {
        **SPEC,
        "docs": {"a.txt": 10_000, "b.txt": 2000},  # a grew, sub is gone
        "small": {"s0.log": 10, "big.iso": 100_000},  # s1 is gone
    }
    newer = build(changed, root=root, cluster=KB4)
    current: dict[str, NodeStore | None] = {"s": newer}
    assert b.refresh(current.get) == [sid]
    got = {i.id: i for i in b.items()}
    assert (got[aid].size, got[aid].alloc, got[aid].status) == (10_000, 12288, "ready")
    assert (got[sid].status, got[sid].alloc, got[sid].note) == ("missing", 0, TEXT["missing"])
    assert got[gid].status == "ready" and got[gid].alloc == KB4
    assert got[gid].note == TEXT["members_missing"].format(count=1)
    assert got[gid].name == TEXT["group_name"].format(folder="small", count=1)
    assert b.freeable() == {"VOL": 12288 + KB4}
    assert b.targets([sid]) == []  # missing items are not executed
    assert b.refresh(current.get) == []  # reported once
    # the scan was closed: what is known stays
    assert b.refresh(lambda scan_id: None) == []
    assert b.get(aid) == got[aid]
    # it came back
    current["s"] = store
    assert b.refresh(current.get) == []
    item = b.get(sid)
    assert item is not None and item.status == "ready" and item.alloc == 2 * KB4


def test_refresh_skips_unchanged_items(store: NodeStore, monkeypatch: pytest.MonkeyPatch) -> None:
    b = basket()
    b.add(store, "s", [find(store, "docs"), find(store, "small/big.iso")])
    calls = []
    real = B._measure

    def counting(*args: Any, **kwargs: Any) -> Any:
        calls.append(args[1])
        return real(*args, **kwargs)

    monkeypatch.setattr(B, "_measure", counting)
    b.refresh(lambda scan_id: store)
    assert calls == []
    store.cols["size"][find(store, "small/big.iso")] = 1  # in place, as store.mutate.update_stats does
    b.refresh(lambda scan_id: store)
    assert len(calls) == 1


# --------------------------------------------------------------------------- locks, execution, undo


def test_lockers_mark_items(store: NodeStore, root: str) -> None:
    b = basket()
    aid = b.add(store, "s", [find(store, "docs/a.txt")]).added[0]
    gid = b.add(store, "s", [find(store, "small")], members=small_members(store, 0, 1)).added[0]
    before = b.freeable()
    b.set_lockers({path_of(root, "docs/a.txt"): ["Word"], path_of(root, "small/s1.log"): ["Excel", "Word"]})
    got = {i.id: i for i in b.items()}
    assert got[aid].status == "locked" and got[aid].note == TEXT["locked"].format(names="Word")
    assert got[gid].status == "locked" and got[gid].note == TEXT["locked"].format(names="Excel、Word")
    assert b.freeable() == before  # held items still count (UI-W5-31)
    b.set_lockers({})
    assert {(i.status, i.note) for i in b.items()} == {("ready", "")}


def test_apply_results_and_reinstate(store: NodeStore, root: str) -> None:
    b = basket()
    aid = b.add(store, "s", [find(store, "docs/a.txt")]).added[0]
    bid = b.add(store, "s", [find(store, "docs/b.txt")]).added[0]
    gid = b.add(store, "s", [find(store, "small")], members=small_members(store, 0, 1, 2)).added[0]
    results = [
        OpResult(path_of(root, "docs/a.txt"), True, "ok", "已移到回收站", "ref-a"),
        OpResult(path_of(root, "docs/b.txt"), False, "denied", "没有权限"),
        OpResult(path_of(root, "small/s0.log"), True, "ok", "已移到回收站", "ref-0"),
        OpResult(path_of(root, "small/s1.log"), True, "ok", "已移到回收站", "ref-1"),
        OpResult(path_of(root, "small/s2.log"), False, "cancelled", "已取消"),
    ]
    left = b.apply_results(results)
    assert [(i.id, i.members) for i in left] == [
        (aid, []),
        (gid, [path_of(root, "small/s0.log"), path_of(root, "small/s1.log")]),
    ]
    got = {i.id: i for i in b.items()}
    assert set(got) == {bid, gid}
    assert got[bid].note == "没有权限" and got[bid].status == "ready"
    assert got[gid].members == [path_of(root, "small/s2.log")] and got[gid].alloc == KB4
    assert got[gid].note == TEXT["members_failed"].format(count=1, message="已取消")
    # undo: the items come back (numbers exact again after a refresh)
    res = b.reinstate(left)
    assert sorted(res.added) == sorted([aid, gid])
    b.refresh(lambda scan_id: store)
    got = {i.id: i for i in b.items()}
    assert len(got[gid].members) == 3 and got[gid].alloc == 3 * KB4
    assert got[aid].alloc == KB4 and got[aid].status == "ready"


def test_clear_and_remove(store: NodeStore) -> None:
    b = basket()
    ids = b.add(store, "s", [find(store, "docs/a.txt"), find(store, "links")]).added
    v = b.version
    b.remove([ids[0], "no-such-id"])
    assert [i.id for i in b.items()] == [ids[1]] and b.version > v
    b.clear()
    assert len(b) == 0 and b.freeable() == {} and b.contains(find_path(store, "links")) is None


def find_path(store: NodeStore, rel: str) -> str:
    return store.path(find(store, rel))


# --------------------------------------------------------------------------- scale (synthetic)


def test_many_items_stay_fast(tmp_path: Path) -> None:
    n = 5000
    big = build({"bulk": {f"f{i:05d}.dat": 100 for i in range(n)}}, root=str(tmp_path / "big"))
    files = list(big.children(find(big, "bulk")))
    b = basket()
    started = time.perf_counter()
    assert len(b.add(big, "s", files).added) == n
    res = b.add(big, "s", [find(big, "bulk")])
    assert len(res.absorbed) == n and len(b) == 1
    b.clear()
    gid = b.add(big, "s", [find(big, "bulk")], members=files).added[0]
    assert len(b.targets()) == n
    left = b.apply_results(OpResult(p, True, "ok", "") for p in b.lock_targets()[: n // 2])
    assert len(left[0].members) == n // 2
    assert len(b.get(gid).members) == n - n // 2  # type: ignore[union-attr]
    assert time.perf_counter() - started < 20
