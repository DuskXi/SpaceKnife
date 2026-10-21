"""The backend's ``--apply-update`` mode: a portable copy's new version put in place (update_swap)."""

from __future__ import annotations

import json
import os
import subprocess
import sys
import threading
import time
from pathlib import Path

import pytest

from spaceknife import update_swap
from spaceknife.update_swap import Log, Plan, PlanError

WIN = sys.platform == "win32"
EXE = "SpaceKnife.exe" if WIN else "spaceknife"


def tree(root: Path) -> Plan:
    """A portable copy with its data folder, and the new version unpacked in its updates folder."""
    app = root / "SpaceKnife-0.3.3-portable"
    updates = app / "data" / "data" / "updates"
    new = updates / "staging-0.3.4" / "SpaceKnife-0.3.4-portable"
    for folder, version in ((app, "0.3.3"), (new, "0.3.4")):
        (folder / "resources" / "backend" / "_internal").mkdir(parents=True)
        (folder / EXE).write_text(version, encoding="utf-8")
        (folder / "resources" / "app.asar").write_text(version, encoding="utf-8")
        (folder / "resources" / "backend" / "_internal" / "python312.dll").write_text(
            version, encoding="utf-8"
        )
        (folder / "data").mkdir(exist_ok=True)
    (app / ".hidden").write_text("old", encoding="utf-8")
    (app / "data" / "keep.txt").write_text("mine", encoding="utf-8")
    (new / "data" / "README.txt").write_text("the archive's note", encoding="utf-8")
    (new / "E2E_NEW_BUILD.txt").write_text("0.3.4", encoding="utf-8")
    return Plan(
        pid=4242,
        app=app,
        new=new,
        old=updates / "previous-0.3.3",
        exe=app / EXE,
        log=updates / "update-0.3.4.log",
    )


def write_plan(plan: Plan, file: Path, **change: object) -> Path:
    raw: dict[str, object] = {
        "v": 1,
        "pid": plan.pid,
        "app": str(plan.app),
        "new": str(plan.new),
        "old": str(plan.old),
        "exe": str(plan.exe),
        "log": str(plan.log),
        "keep": list(plan.keep),
        **change,
    }
    file.write_text(json.dumps(raw), encoding="utf-8")
    return file


def log_lines(plan: Plan) -> list[str]:
    return [line.split(" ", 2)[2] for line in plan.log.read_text(encoding="utf-8").splitlines()]


def test_the_plan_is_read_and_checked_before_anything_moves(tmp_path: Path) -> None:
    plan = tree(tmp_path)
    file = tmp_path / "update-0.3.4.json"
    assert Plan.read(write_plan(plan, file)) == plan
    for change, says in (
        ({"v": 2}, "version 1"),
        ({"pid": "x"}, "invalid literal"),
        ({"new": "relative/new"}, "absolute"),
        ({"exe": str(tmp_path / EXE)}, "is not in"),
        ({"new": str(plan.app)}, "must not be"),
    ):
        with pytest.raises(PlanError, match=says):
            Plan.read(write_plan(plan, file, **change))
    (plan.new / EXE).unlink()  # an incomplete new version
    with pytest.raises(PlanError, match="incomplete"):
        Plan.read(write_plan(plan, file))
    with pytest.raises(PlanError, match="can't read"):
        Plan.read(tmp_path / "missing.json")


def test_the_new_version_takes_the_place_of_the_old_one(tmp_path: Path) -> None:
    plan = tree(tmp_path)
    started: list[list[str]] = []
    waited: list[int] = []

    def wait(pid: int, _timeout: float) -> bool:
        waited.append(pid)
        return True

    code = update_swap.run(
        plan,
        Log(plan.log),
        wait=wait,
        start=lambda exe, args: started.append([str(exe), *args]),
    )
    assert code == 0
    assert waited == [4242]
    assert started == [[str(plan.exe), "--updated"]]
    assert (plan.app / EXE).read_text(encoding="utf-8") == "0.3.4"
    assert (plan.app / "resources" / "backend" / "_internal" / "python312.dll").read_text(
        encoding="utf-8"
    ) == "0.3.4"
    assert (plan.app / "E2E_NEW_BUILD.txt").is_file()
    assert not (plan.app / ".hidden").exists()
    # the data folder stays as it was: the archive's own data/README.txt doesn't come along
    assert sorted(p.name for p in (plan.app / "data").iterdir()) == ["data", "keep.txt"]
    assert sorted(p.name for p in plan.old.iterdir()) == sorted([".hidden", EXE, "resources"])
    assert (plan.old / EXE).read_text(encoding="utf-8") == "0.3.3"
    assert log_lines(plan) == ["waiting for SpaceKnife (4242) to quit", "updated"]


def test_an_app_that_does_not_quit_is_left_alone(tmp_path: Path) -> None:
    plan = tree(tmp_path)
    started: list[list[str]] = []
    code = update_swap.run(
        plan, Log(plan.log), wait=lambda _p, _t: False, start=lambda e, a: started.append(a)
    )
    assert code == 1
    assert started == []  # it still runs: no second copy
    assert (plan.app / EXE).read_text(encoding="utf-8") == "0.3.3"
    assert log_lines(plan)[-1] == "SpaceKnife did not quit: kept the old version"


def test_the_old_version_comes_back_when_the_new_files_cannot_move_in(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    plan = tree(tmp_path)
    real = update_swap.place

    def place(src: Path, dst: Path) -> None:
        if src.name == "resources":
            raise OSError("disk full")
        real(src, dst)

    monkeypatch.setattr(update_swap, "place", place)
    started: list[list[str]] = []
    code = update_swap.run(
        plan, Log(plan.log), wait=lambda _p, _t: True, start=lambda e, a: started.append(a)
    )
    assert code == 1
    assert started == [[]]  # the old version, without --updated
    assert (plan.app / EXE).read_text(encoding="utf-8") == "0.3.3"
    assert (plan.app / "resources" / "app.asar").read_text(encoding="utf-8") == "0.3.3"
    assert (plan.app / ".hidden").is_file()
    assert not (plan.app / "E2E_NEW_BUILD.txt").exists()
    assert (plan.app / "data" / "keep.txt").is_file()
    assert log_lines(plan)[-2:] == ["could not move the new files in: disk full", "kept the old version"]


def test_moving_out_tries_again_and_puts_back_what_moved(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    plan = tree(tmp_path)
    real = os.replace
    held: list[str] = []

    def replace(src: os.PathLike[str] | str, dst: os.PathLike[str] | str) -> None:
        if Path(src).name == "resources" and Path(src).parent == plan.app:
            held.append("x")
            raise PermissionError("in use")
        real(src, dst)

    monkeypatch.setattr(os, "replace", replace)
    with pytest.raises(PermissionError):
        update_swap.move_out(plan.app, plan.old, plan.keep, Log(plan.log), timeout=0.6)
    monkeypatch.setattr(os, "replace", real)
    assert len(held) >= 2  # tried again
    assert sorted(p.name for p in plan.app.iterdir()) == sorted([".hidden", EXE, "data", "resources"])
    assert list(plan.old.iterdir()) == []
    assert log_lines(plan) == ["busy: resources: in use"]


def test_what_cannot_be_renamed_moves_piece_by_piece_or_is_copied(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Like Windows with this process running from the new files: its DLL is held open, and the
    folders holding it can't be renamed."""
    plan = tree(tmp_path)
    held = plan.new / "resources" / "backend" / "_internal" / "python312.dll"
    real = os.replace

    def replace(src: os.PathLike[str] | str, dst: os.PathLike[str] | str) -> None:
        if Path(src) == held or held.is_relative_to(Path(src)):
            raise PermissionError("in use")
        real(src, dst)

    monkeypatch.setattr(os, "replace", replace)
    target = tmp_path / "placed"
    update_swap.place(plan.new / "resources", target)
    assert (target / "app.asar").read_text(encoding="utf-8") == "0.3.4"
    assert (target / "backend" / "_internal" / "python312.dll").read_text(encoding="utf-8") == "0.3.4"
    assert held.is_file()  # copied: the original stays until the new backend clears the staging folder
    assert not (plan.new / "resources" / "app.asar").exists()  # moved


def test_waiting_for_a_process(tmp_path: Path) -> None:
    done = subprocess.Popen([sys.executable, "-c", "pass"])
    while done.poll() is None:
        time.sleep(0.05)
    assert update_swap.wait_gone(done.pid, 1.0)
    # Linux tells an exited, unreaped process by /proc; macOS has no portable copy to swap
    if sys.platform != "darwin":
        unreaped = subprocess.Popen([sys.executable, "-c", "pass"])
        time.sleep(0.5)  # exited, its parent (this test) hasn't reaped it yet: a zombie on POSIX
        assert update_swap.wait_gone(unreaped.pid, 2.0)
        unreaped.wait()
    running = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(30)"])
    try:
        assert not update_swap.wait_gone(running.pid, 0.3)
    finally:
        running.kill()
        running.wait()


def test_the_app_starts_without_the_bootloader_s_environment(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(sys, "_MEIPASS", "/opt/sk/resources/backend/_internal", raising=False)
    env = {
        "PATH": "/usr/bin",
        "_PYI_ARCHIVE_FILE": "x",
        "_PYI_PARENT_PROCESS_LEVEL": "1",
        "LD_LIBRARY_PATH": "/opt/sk/resources/backend/_internal:/usr/lib/x",
        "LD_LIBRARY_PATH_ORIG": "/usr/lib/x",
        "DYLD_LIBRARY_PATH": "/opt/sk/resources/backend/_internal",
    }
    assert update_swap.launch_env(env) == {"PATH": "/usr/bin", "LD_LIBRARY_PATH": "/usr/lib/x"}
    assert update_swap.launch_env({"LD_LIBRARY_PATH": "/usr/lib/mine"}) == {
        "LD_LIBRARY_PATH": "/usr/lib/mine"
    }


def test_a_bad_plan_says_why_and_touches_nothing(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    plan = tree(tmp_path)
    (plan.new / EXE).unlink()
    file = write_plan(plan, tmp_path / "update-0.3.4.json")
    assert update_swap.main([str(file)]) == 2
    said = json.loads(capsys.readouterr().out)
    assert said["ok"] is False and "incomplete" in said["error"]
    assert "not started: the new version is incomplete" in (tmp_path / "update-0.3.4.log").read_text(
        encoding="utf-8"
    )
    assert (plan.app / EXE).read_text(encoding="utf-8") == "0.3.3"
    assert update_swap.main([]) == 2


def test_the_whole_swap_as_its_own_process(tmp_path: Path) -> None:
    """``python -m spaceknife.update_swap <plan>``: says ok, outlives the app it waits for, swaps the
    files and starts the new version on its own."""
    plan = tree(tmp_path)
    ran = tmp_path / "ran.txt"
    if WIN:
        script = f'@echo %* > "{ran}"\r\n'
        name = "SpaceKnife.cmd"
    else:
        script = f'#!/bin/sh\necho "$@" > "{ran}"\n'
        name = "spaceknife.sh"
    for folder in (plan.app, plan.new):
        (folder / EXE).unlink()
        (folder / name).write_text(script, encoding="utf-8")
        (folder / name).chmod(0o755)
    app = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(1.5)"])  # the quitting app
    threading.Thread(target=app.wait, daemon=True).start()  # reaped when it exits, as by its real parent
    plan = Plan(pid=app.pid, app=plan.app, new=plan.new, old=plan.old, exe=plan.app / name, log=plan.log)
    file = write_plan(plan, tmp_path / "update-0.3.4.json")
    swap = subprocess.Popen(
        [sys.executable, "-m", "spaceknife.update_swap", str(file)],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
    )
    try:
        assert swap.stdout is not None
        assert json.loads(swap.stdout.readline()) == {"ok": True}
        swap.stdout.close()  # as the app does: it quits right after this line
        assert swap.wait(30) == 0
        deadline = time.monotonic() + 15
        while not (ran.is_file() and ran.read_text(encoding="utf-8").strip()) and time.monotonic() < deadline:
            time.sleep(0.1)
        assert ran.read_text(encoding="utf-8").strip() == "--updated"
    finally:
        app.wait()
        if swap.poll() is None:
            swap.kill()
    assert (plan.app / "E2E_NEW_BUILD.txt").is_file()
    assert log_lines(plan) == [f"waiting for SpaceKnife ({app.pid}) to quit", "updated"]
