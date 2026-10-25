"""POSIX watchers (docs/monitor.md §2.2–2.3): inotify decoding and interpretation everywhere, the live
inotify watcher on Linux, and the FSEvents mapping (the stream itself only runs on macOS)."""

from __future__ import annotations

import os
import shutil
import struct
import sys
import threading
import time
from collections.abc import Callable, Iterator
from pathlib import Path

import pytest

from spaceknife.monitor.base import Change
from spaceknife.monitor.linux import (
    IN_ATTRIB,
    IN_CLOSE_WRITE,
    IN_CREATE,
    IN_DELETE,
    IN_DELETE_SELF,
    IN_IGNORED,
    IN_ISDIR,
    IN_MOVE_SELF,
    IN_MOVED_FROM,
    IN_MOVED_TO,
    IN_Q_OVERFLOW,
    IN_UNMOUNT,
    InotifyWatcher,
    RawEvent,
    WatchTree,
    decode_events,
    interpret,
)
from spaceknife.monitor.mac import (
    F_ITEM_CREATED,
    F_ITEM_IS_DIR,
    F_ITEM_IS_FILE,
    F_ITEM_MODIFIED,
    F_ITEM_REMOVED,
    F_ITEM_RENAMED,
    F_KERNEL_DROPPED,
    F_MUST_SCAN_SUBDIRS,
    F_ROOT_CHANGED,
    FsEventsWatcher,
    map_events,
)

from .test_monitor_poll import Collector, has, wait_until

LINUX = sys.platform.startswith("linux")


def record(wd: int, mask: int, cookie: int = 0, name: bytes = b"") -> bytes:
    """One struct inotify_event as the kernel writes it (the name NUL-padded to 16 bytes)."""
    padded = name + b"\0" * (16 - len(name) % 16) if name else b""
    return struct.pack("iIII", wd, mask, cookie, len(padded)) + padded


def sample_tree() -> WatchTree:
    """/r (wd 1) → a (wd 2) → b (wd 3)."""
    tree = WatchTree("/r")
    tree.set_root(1)
    tree.add(2, 1, "a")
    tree.add(3, 2, "b")
    return tree


# --------------------------------------------------------------------------- decoding (every platform)


def test_decode_events_strips_padding() -> None:
    buf = (
        record(2, IN_CREATE, 0, b"hello.txt") + record(-1, IN_Q_OVERFLOW) + record(3, IN_DELETE, 0, b"x" * 16)
    )
    assert decode_events(buf) == [
        RawEvent(2, IN_CREATE, 0, b"hello.txt"),
        RawEvent(-1, IN_Q_OVERFLOW, 0, b""),
        RawEvent(3, IN_DELETE, 0, b"x" * 16),
    ]
    assert decode_events(buf[:-5]) == decode_events(buf)[:2]  # a truncated tail is dropped


def test_queue_overflow_is_reported_for_the_root() -> None:
    tree = sample_tree()
    res = interpret(decode_events(record(-1, IN_Q_OVERFLOW) + record(2, IN_CREATE, 0, b"f")), tree)
    assert res.changes == [Change("overflow", "/r", is_dir=True), Change("created", "/r/a/f", is_dir=False)]
    assert res.overflows == 1 and not res.root_lost  # and watching goes on


def test_create_delete_modify() -> None:
    tree = sample_tree()
    events = [
        RawEvent(2, IN_CREATE, 0, b"x"),
        RawEvent(2, IN_CLOSE_WRITE, 0, b"x"),
        RawEvent(3, IN_CLOSE_WRITE, 0, b"y"),
        RawEvent(2, IN_ATTRIB, 0, b"x"),  # the same file again: one modified, in the last position
        RawEvent(1, IN_CREATE | IN_ISDIR, 0, b"d"),
        RawEvent(3, IN_DELETE, 0, b"z"),
        RawEvent(2, IN_ATTRIB | IN_ISDIR, 0, b"b"),  # folder attributes: nothing to do
        RawEvent(3, IN_ATTRIB, 0, b""),  # about the watched folder itself
        RawEvent(99, IN_CREATE, 0, b"unknown-wd"),
    ]
    res = interpret(events, tree)
    assert res.changes == [
        Change("created", "/r/a/x", is_dir=False),
        Change("created", "/r/d", is_dir=True),
        Change("deleted", "/r/a/b/z", is_dir=False),
    ]
    assert res.modified == ["/r/a/b/y", "/r/a/x"]
    assert res.new_dirs == ["/r/d"]
    assert res.detached == [] and res.overflows == 0


def test_deleted_file_is_not_stat_ed() -> None:
    res = interpret([RawEvent(2, IN_CLOSE_WRITE, 0, b"x"), RawEvent(2, IN_DELETE, 0, b"x")], sample_tree())
    assert res.modified == []
    assert res.changes == [Change("deleted", "/r/a/x", is_dir=False)]


def test_rename_pairing_by_cookie() -> None:
    tree = sample_tree()
    events = [
        RawEvent(2, IN_MOVED_FROM, 7, b"x"),
        RawEvent(1, IN_MOVED_TO, 7, b"z"),
        RawEvent(2, IN_MOVED_FROM, 9, b"gone"),  # moved out of sight
        RawEvent(3, IN_MOVED_TO, 11, b"came"),  # moved in from elsewhere
    ]
    res = interpret(events, tree)
    assert res.changes == [
        Change("renamed", "/r/z", "/r/a/x", is_dir=False),
        Change("deleted", "/r/a/gone", is_dir=False),
        Change("created", "/r/a/b/came", is_dir=False),
    ]


def test_folder_rename_rekeys_the_subtree() -> None:
    tree = sample_tree()
    tree.add(4, 1, "c")  # an empty folder the move replaces
    events = [
        RawEvent(1, IN_MOVED_FROM | IN_ISDIR, 5, b"a"),
        RawEvent(1, IN_MOVED_TO | IN_ISDIR, 5, b"c"),
        RawEvent(2, IN_MOVE_SELF),
        RawEvent(3, IN_CREATE, 0, b"f"),  # inside the moved folder: reported under its new path
    ]
    res = interpret(events, tree)
    assert res.changes == [
        Change("renamed", "/r/c", "/r/a", is_dir=True),
        Change("created", "/r/c/b/f", is_dir=False),
    ]
    assert res.moved_dirs == [("/r/a", "/r/c")]
    assert res.detached == [4]
    assert tree.find("/r/c/b") == 3 and tree.find("/r/a") is None
    assert tree.path(3) == "/r/c/b"


def test_folder_moved_out_detaches_its_watches() -> None:
    tree = sample_tree()
    events = [
        RawEvent(1, IN_MOVED_FROM | IN_ISDIR, 5, b"a"),
        RawEvent(3, IN_CREATE, 0, b"late"),  # from a folder no longer in the tree
    ]
    res = interpret(events, tree)
    assert res.changes == [Change("deleted", "/r/a", is_dir=True)]
    assert res.detached == [2]
    assert tree.orphans() == [3]


def test_folder_moved_in_is_walked() -> None:
    res = interpret([RawEvent(3, IN_MOVED_TO | IN_ISDIR, 8, b"new")], sample_tree())
    assert res.changes == [Change("created", "/r/a/b/new", is_dir=True)]
    assert res.new_dirs == ["/r/a/b/new"]


def test_rmdir_of_a_watched_folder() -> None:
    tree = sample_tree()
    events = [
        RawEvent(3, IN_DELETE_SELF),
        RawEvent(3, IN_IGNORED),
        RawEvent(2, IN_DELETE | IN_ISDIR, 0, b"b"),
    ]
    res = interpret(events, tree)
    assert res.changes == [Change("deleted", "/r/a/b", is_dir=True)]
    assert 3 not in tree and res.detached == [] and res.overflows == 0


def test_lost_watch_is_an_overflow() -> None:
    tree = sample_tree()
    res = interpret([RawEvent(2, IN_UNMOUNT), RawEvent(2, IN_IGNORED)], tree)
    assert res.changes == [Change("overflow", "/r/a", is_dir=True)]
    assert res.detached == [2] and res.overflows == 1
    assert tree.orphans() == [3]


@pytest.mark.parametrize("mask", [IN_DELETE_SELF, IN_MOVE_SELF, IN_IGNORED])
def test_root_lost(mask: int) -> None:
    res = interpret([RawEvent(1, mask), RawEvent(2, IN_CREATE, 0, b"after")], sample_tree())
    assert res.changes == [Change("overflow", "/r", is_dir=True)]
    assert res.root_lost


def test_watch_tree_at_the_file_system_root() -> None:
    tree = WatchTree("/")
    tree.set_root(1)
    tree.add(2, 1, "home")
    tree.add(3, 2, "me")
    assert tree.path(1) == "/" and tree.path(2) == "/home" and tree.path(3) == "/home/me"
    assert tree.find("/") == 1 and tree.find("/home/me") == 3 and tree.find("/nope") is None
    assert interpret([RawEvent(1, IN_CREATE, 0, b"f")], tree).changes == [
        Change("created", "/f", is_dir=False)
    ]


@pytest.mark.skipif(sys.platform == "win32", reason="POSIX file system encoding (surrogateescape)")
def test_surrogate_names() -> None:
    res = interpret([RawEvent(2, IN_CREATE, 0, b"bad\xff")], sample_tree())
    assert res.changes[0].path == "/r/a/bad\udcff"
    assert os.fsencode(res.changes[0].path) == b"/r/a/bad\xff"


# --------------------------------------------------------------------------- FSEvents mapping


def test_fsevents_mapping() -> None:
    present = {"/r/new", "/r/dir", "/r/mod"}

    def exists(path: str) -> bool:
        return path in present

    def numbers(path: str) -> tuple[int, int, int] | None:
        return (5, 4096, 1_700_000_000) if path in present else None

    events = [
        ("/r/old", F_ITEM_RENAMED | F_ITEM_IS_FILE, 10),
        ("/r/new", F_ITEM_RENAMED | F_ITEM_IS_FILE, 11),
        ("/r/dir", F_ITEM_CREATED | F_ITEM_IS_DIR, 12),
        ("/r/flash", F_ITEM_CREATED | F_ITEM_REMOVED | F_ITEM_IS_FILE, 13),
        ("/r/mod", F_ITEM_MODIFIED | F_ITEM_IS_FILE, 14),
        ("/r/dir", F_ITEM_MODIFIED | F_ITEM_IS_DIR, 15),
        ("/r/sub", F_MUST_SCAN_SUBDIRS, 16),
        ("/r/x", F_KERNEL_DROPPED, 17),
        ("/r/plain", 0, 18),
        ("/r/away", F_ITEM_RENAMED | F_ITEM_IS_FILE, 30),  # renamed, no partner: gone
    ]
    changes, root_lost = map_events(events, "/r", exists=exists, numbers=numbers)
    assert changes == [
        Change("renamed", "/r/new", "/r/old", is_dir=False),
        Change("created", "/r/dir", is_dir=True),
        Change("deleted", "/r/flash", is_dir=False),
        Change("modified", "/r/mod", is_dir=False, size=5, alloc=4096, mtime=1_700_000_000),
        Change("overflow", "/r/sub", is_dir=True),
        Change("overflow", "/r", is_dir=True),
        Change("dir", "/r/plain", is_dir=True),
        Change("deleted", "/r/away", is_dir=False),
    ]
    assert not root_lost
    changes, root_lost = map_events([("/r", F_ROOT_CHANGED, 40), ("/r/a", F_ITEM_CREATED, 41)], "/r")
    assert changes == [Change("overflow", "/r", is_dir=True)] and root_lost


@pytest.mark.skipif(sys.platform == "darwin", reason="CoreServices exists on macOS")
def test_fsevents_constructor_fails_cleanly(tmp_path: Path) -> None:
    with pytest.raises(OSError):  # noqa: PT011 - the contract is "OSError" (create_watcher catches it)
        FsEventsWatcher(str(tmp_path))


def test_inotify_unavailable_off_linux_falls_back_to_polling(tmp_path: Path) -> None:
    if LINUX:
        pytest.skip("inotify is available here")
    w = InotifyWatcher(str(tmp_path), poll_interval=0.1)
    if sys.platform == "win32":  # no poll() for the loop either: only the constructor is portable
        assert w.status().state == "starting"
        return
    got = Collector()
    w.start(got)
    try:
        wait_until(lambda: w.status().polled_dirs == 1)
        st = w.status()
        assert st.state == "degraded" and "inotify 不可用" in st.detail
    finally:
        w.stop()


# --------------------------------------------------------------------------- live inotify (Linux)

linux_only = pytest.mark.skipif(not LINUX, reason="inotify is Linux only")


@pytest.fixture
def root(tmp_path: Path) -> Path:
    r = tmp_path / "root"
    r.mkdir()
    return r


@pytest.fixture
def watching(root: Path) -> Iterator[tuple[InotifyWatcher, Collector]]:
    w = InotifyWatcher(str(root), poll_interval=0.2)
    got = Collector()
    w.start(got)
    try:
        yield w, got
    finally:
        w.stop()


def created_or_renamed(path: Path) -> Callable[[list[Change]], bool]:
    return lambda cs: any(c.path == str(path) and c.kind in ("created", "renamed") for c in cs)


@linux_only
def test_live_files(root: Path, watching: tuple[InotifyWatcher, Collector]) -> None:
    w, got = watching
    assert w.name == "inotify"
    st = w.status()
    assert st.state == "running" and st.watched_dirs == 1 and st.polled_dirs == 0
    f = root / "x.txt"
    f.write_bytes(b"hello")
    got.wait_for(has("created", f, is_dir=False))
    got.wait_for(has("modified", f, size=5))
    m = next(c for c in got.changes() if c.kind == "modified")
    assert m.alloc is not None and m.mtime == int(os.stat(f).st_mtime)
    g = root / "y.txt"
    f.rename(g)
    got.wait_for(has("renamed", g, old_path=str(f), is_dir=False))
    os.chmod(g, 0o600)
    got.wait_for(lambda cs: sum(1 for c in cs if c.kind == "modified" and c.path == str(g)) >= 1)
    g.unlink()
    got.wait_for(has("deleted", g, is_dir=False))
    assert w.status().events == len(got.changes())


@linux_only
def test_live_new_folder_is_watched(root: Path, watching: tuple[InotifyWatcher, Collector]) -> None:
    w, got = watching
    d = root / "d"
    d.mkdir()
    got.wait_for(has("created", d, is_dir=True))
    wait_until(lambda: w.status().watched_dirs == 2)
    (d / "inner.txt").write_bytes(b"1")
    got.wait_for(created_or_renamed(d / "inner.txt"))
    e = root / "e"
    d.rename(e)
    got.wait_for(has("renamed", e, old_path=str(d), is_dir=True))
    (e / "after.txt").write_bytes(b"2")
    got.wait_for(has("created", e / "after.txt"))  # the watch followed the rename
    shutil.rmtree(e)
    got.wait_for(has("deleted", e, is_dir=True))
    wait_until(lambda: w.status().watched_dirs == 1)


@linux_only
def test_live_mkdir_race_reports_existing_entries(
    root: Path, tmp_path: Path, watching: tuple[InotifyWatcher, Collector]
) -> None:
    _, got = watching
    outside = tmp_path / "outside"
    (outside / "sub").mkdir(parents=True)
    (outside / "a.txt").write_bytes(b"a")
    (outside / "sub" / "b.txt").write_bytes(b"b")
    moved = root / "moved"
    outside.rename(moved)  # moved in from outside: one IN_MOVED_TO, the contents come from the walk
    got.wait_for(has("created", moved, is_dir=True))
    got.wait_for(has("created", moved / "a.txt", is_dir=False))
    got.wait_for(has("created", moved / "sub", is_dir=True))
    got.wait_for(has("created", moved / "sub" / "b.txt", is_dir=False))
    (moved / "sub" / "c.txt").write_bytes(b"c")  # the walk also watched the subfolder
    got.wait_for(has("created", moved / "sub" / "c.txt"))


@linux_only
def test_live_add_dirs(root: Path, watching: tuple[InotifyWatcher, Collector]) -> None:
    w, got = watching
    b = root / "a" / "b"
    b.mkdir(parents=True)
    got.wait_for(has("created", root / "a", is_dir=True))
    w.add_dirs([str(root / "a"), str(b)])  # already watched through the walk: no duplicates
    wait_until(lambda: w.status().watched_dirs == 3)
    (b / "f").write_bytes(b"")
    got.wait_for(has("created", b / "f"))
    assert w.status().watched_dirs == 3


@linux_only
def test_live_status_is_starting_until_handed_folders_are_watched(
    root: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    dirs = [root / f"d{i}" for i in range(3)]
    for d in dirs:
        d.mkdir()
    w = InotifyWatcher(str(root), poll_interval=0.2)
    gate = threading.Event()
    place = w._take_incoming

    def held() -> None:  # the thread places nothing until the test lets it
        gate.wait(5)
        place()

    monkeypatch.setattr(w, "_take_incoming", held)
    w.start(Collector())
    try:
        assert w.status().state == "running"  # nothing handed in yet: the root is watched
        w.add_dirs([str(d) for d in dirs])
        st = w.status()
        assert (st.state, st.watched_dirs) == ("starting", 1)
        gate.set()
        wait_until(lambda: w.status().state == "running")
        assert w.status().watched_dirs == 4
    finally:
        gate.set()
        w.stop()


@linux_only
def test_live_folder_changed_before_its_watch_is_listed_again(root: Path) -> None:
    """A folder the scan listed before its watch was placed: what changed in between raised no
    event. One stat once the watch is on finds it, and the folder is listed again (``dir``)."""
    quiet, changed, older, deep = root / "quiet", root / "changed", root / "older", root / "changed" / "deep"
    deep.mkdir(parents=True)
    for d in (quiet, older):
        d.mkdir()
    listed = {d: int(os.stat(d).st_mtime) for d in (quiet, changed, older, deep)}  # as the scan records
    listed[older] -= 5  # the folder's time moved since: whole seconds tell
    time.sleep(0.2)  # well past the folders' times (listed_changed allows file times some slack)
    since = time.time_ns()  # the scan started after the folders were made ...
    (changed / "late.bin").write_bytes(b"l" * 512)  # ... and this came after its listing, unseen
    w = InotifyWatcher(str(root), poll_interval=0.2)
    got = Collector()
    w.start(got)
    try:
        order = [quiet, changed, older, deep]
        w.add_dirs([str(d) for d in order], [listed[d] for d in order], since_ns=since)
        got.wait_for(has("dir", changed, is_dir=True))
        got.wait_for(has("dir", older, is_dir=True))
        wait_until(lambda: w.status().state == "running")
        assert w.status().watched_dirs == 5
        flagged = {c.path for c in got.changes() if c.kind == "dir"}
        assert flagged == {str(changed), str(older)}  # unchanged folders are not listed again
    finally:
        w.stop()


@linux_only
def test_live_polled_folder_changed_before_its_baseline_is_listed_again(root: Path) -> None:
    """Beyond the watch budget the poller's first stat is the check: a change between the scan's
    listing and that baseline is reported, not taken as the starting point."""
    a, b = root / "a", root / "b"
    for d in (a, b):
        d.mkdir()
    listed = {d: int(os.stat(d).st_mtime) for d in (a, b)}
    time.sleep(0.2)
    since = time.time_ns()
    (b / "late.bin").write_bytes(b"l")
    w = InotifyWatcher(str(root), poll_interval=60, max_watches=2)  # the root and a: b is polled
    got = Collector()
    w.start(got)
    try:
        w.add_dirs([str(a), str(b)], [listed[a], listed[b]], since_ns=since)
        got.wait_for(has("dir", b, is_dir=True))
        st = w.status()
        assert (st.watched_dirs, st.polled_dirs, st.state) == (2, 1, "degraded")
        assert not has("dir", a)(got.changes())
    finally:
        w.stop()


@linux_only
def test_live_budget_polls_the_rest(root: Path) -> None:
    a, b, c = root / "a", root / "a" / "b", root / "a" / "b" / "c"
    c.mkdir(parents=True)
    w = InotifyWatcher(str(root), poll_interval=0.2, max_watches=2)
    got = Collector()
    w.start(got)
    try:
        w.add_dirs([str(a), str(b), str(c)])
        wait_until(lambda: w.status().polled_dirs == 2)
        st = w.status()
        assert st.watched_dirs == 2
        assert st.state == "degraded" and st.detail == "inotify 监视额度不足，2 个文件夹改为轮询"
        time.sleep(0.1)  # baselines are taken right away; make sure the next change is a new mtime
        (c / "new.txt").write_bytes(b"n")
        got.wait_for(has("dir", c, is_dir=True))
        (a / "watched.txt").write_bytes(b"w")  # the watched part still reports at once
        got.wait_for(has("created", a / "watched.txt"))
        shutil.rmtree(b)
        got.wait_for(has("deleted", b, is_dir=True))
    finally:
        w.stop()


@linux_only
def test_overflow_revalidation_drops_stale_watches(root: Path) -> None:
    """After IN_Q_OVERFLOW the deletion of a watched folder may be among the lost events; a folder
    re-created under the same name must get a watch of its own (driven by hand: no thread)."""
    from spaceknife.monitor import linux

    d = root / "d"
    (d / "sub").mkdir(parents=True)
    w = InotifyWatcher(str(root))
    got = Collector()
    w._sink = got
    w._ino = ino = linux._Inotify()
    try:
        w._watch_root()
        w._place(str(d), None, 0, [])
        w._place(str(d / "sub"), None, 0, [])
        old = w._tree.find(str(d))
        assert old is not None and len(w._tree) == 3
        shutil.rmtree(d)
        (d / "new").mkdir(parents=True)
        ino.read(1 << 20)  # these events are "lost"
        res = interpret([RawEvent(-1, IN_Q_OVERFLOW)], w._tree)
        assert res.queue_overflow
        w._recheck.extend(w._tree.wds())
        assert not w._recheck_some(ino)
        assert w._tree.find(str(d)) is None and len(w._tree) == 1  # stale entry and its subtree gone
        assert w._held == 1
        w._walk_some()
        w._walk_some()
        new = w._tree.find(str(d))
        assert new is not None and new != old
        assert w._tree.find(str(d / "new")) is not None and len(w._tree) == 3
        assert has("created", d / "new", is_dir=True)(got.changes())
        assert w._held == 3
    finally:
        w._close()
    assert linux._BUDGET.used >= 0 and w._held == 0


@linux_only
def test_live_root_deleted(root: Path, watching: tuple[InotifyWatcher, Collector]) -> None:
    w, got = watching
    root.rmdir()
    got.wait_for(has("overflow", root, is_dir=True))
    wait_until(lambda: w.status().state == "failed")
    assert "扫描根目录" in w.status().detail


@linux_only
def test_live_stop_is_quick_and_silent(root: Path) -> None:
    w = InotifyWatcher(str(root))
    got = Collector()
    w.start(got)
    (root / "one").write_bytes(b"")
    got.wait_for(has("created", root / "one"))
    t0 = time.monotonic()
    w.stop()
    assert time.monotonic() - t0 < 1.0
    assert w.status().state == "stopped" and w.status().watched_dirs == 0
    seen = len(got.changes())
    (root / "two").write_bytes(b"")
    time.sleep(0.2)
    assert len(got.changes()) == seen
    w.stop()


@linux_only
def test_create_watcher_picks_inotify(root: Path) -> None:
    from spaceknife.monitor.base import create_watcher

    assert isinstance(create_watcher(str(root)), InotifyWatcher)
