"""The cleanup plan's parts on their own (docs/dupes.md §16): the keep engine, the journal, the
executor's steps with injected failures and crashes, undo, purge, recovery after a crash.

Executor tests make their files in a temporary folder and run with the sandbox set to it; every
failure case asserts the copy is either exactly as before (its name holds the same file, no
temporary name, nothing in the recovery area) or fully done, never lost, never overwritten.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import textwrap
from pathlib import Path
from typing import Any

import pytest
from hypothesis import given, settings
from hypothesis import strategies as st

from spaceknife.dupes import codes as C
from spaceknife.dupes.cleanup import codes as K
from spaceknife.dupes.cleanup import execute as X
from spaceknife.dupes.cleanup.fsops import Faults, PosixFs, Sandbox
from spaceknife.dupes.cleanup.journal import Journal, fold, read_records
from spaceknife.dupes.cleanup.keep import GroupInputs, Unit, decide
from spaceknife.dupes.cleanup.plan import policy_from_keep
from spaceknife.dupes.cleanup.policy import ManualCopy, Policy, Rule, Selector, copy_stem, plain_name_keys
from spaceknife.dupes.keep import KeepOptions, UnitView, choose

POSIX = os.name != "nt"
posix_only = pytest.mark.skipif(
    not POSIX, reason="the POSIX executor (Windows: the Windows checkout runs winfs)"
)


# ------------------------------------------------------------------ the keep engine
def _gi(
    units: list[Unit], *, side: dict[str, int] | None = None, manual: list[ManualCopy | None] | None = None
) -> GroupInputs:
    return GroupInputs(
        digest="d",
        manual=manual or [None] * len(units),
        keep_set=None,
        keep_masks={},
        group_rules={},
        pairs=[],
        side=side,
    )


unit_st = st.builds(
    lambda i, vol, bt, mt, depth, forced: (i, vol, bt, mt, depth, forced),
    st.integers(0, 9),
    st.integers(0, 2),
    st.integers(0, 5),
    st.integers(0, 5),
    st.integers(1, 3),
    st.booleans(),
)


@settings(max_examples=300, deadline=None)
@given(
    st.lists(unit_st, min_size=2, max_size=6, unique_by=lambda x: x[0]),
    st.sampled_from(["oldest", "newest", "shortest"]),
    st.booleans(),
    st.lists(st.integers(0, 5), max_size=2),
)
def test_the_default_policy_keeps_what_the_drawer_keeps(
    raw: list[Any], rule: str, per_volume: bool, manual_keep: list[int]
) -> None:
    views: list[UnitView] = []
    units: list[Unit] = []
    for n, (i, vol, bt, mt, depth, forced) in enumerate(raw):
        path = "/" + "/".join(f"d{i}" for _ in range(depth)) + f"/f{i}.bin"
        views.append(UnitView(rows=[n], path=path, vol=vol, btime=bt, mtime=mt, forced=forced and n % 3 == 0))
        units.append(
            Unit(
                rep=n,
                rows=[n],
                path=path,
                vol=vol,
                btime=bt,
                mtime=mt,
                forced=K.SRC_FORCED_NEEDED if views[-1].forced else 0,
            )
        )
    overrides = {(b"d", views[k].path): "keep" for k in manual_keep if k < len(views)}
    opts = KeepOptions(rule=rule, per_volume=per_volume, folder_consistent=False, overrides=overrides)  # type: ignore[arg-type]
    old = choose(views, opts, digest=b"d")
    pol = policy_from_keep(opts, exact=True)
    manual = [ManualCopy(keep="keep") if (b"d", u.path) in overrides else None for u in units]
    new = decide(units, pol, _gi(units, manual=manual), exact=True)
    kept_old = {i for i, (role, _why) in enumerate(old) if role == C.ROLE_KEEP}
    kept_new = {i for i, s in enumerate(new.state) if s != K.K_NO}
    assert kept_new == kept_old
    assert kept_new  # never an empty keep set
    if per_volume:
        assert {units[i].vol for i in kept_new} == {u.vol for u in units}


def test_constraints_only_add_keepers_and_rules_only_pick() -> None:
    units = [
        Unit(rep=0, rows=[0], path="/D/Photos/a.jpg", vol=0, btime=5, mtime=5),
        Unit(rep=1, rows=[1], path="/D/Backup/a.jpg", vol=0, btime=1, mtime=1),
        Unit(rep=2, rows=[2], path="/E/Old/a.jpg", vol=1, btime=2, mtime=2),
    ]
    pol = Policy(criteria=["oldest_created"], per_volume=True)
    pol.keep_rules.append(Rule(1, "keep", "prefer", Selector(), folders=["/D/Photos"]))
    d = decide(units, pol, _gi(units))
    assert [s != K.K_NO for s in d.state] == [True, False, True]  # prefer picks on D, E keeps one
    assert d.src[0] == K.SRC_PREFER and d.src[2] in (K.SRC_PER_VOLUME, K.SRC_CRITERION, K.SRC_TIE)
    # avoid: only the avoided are left on D, one stays anyway and the group says so
    pol2 = Policy(criteria=["oldest_created"], per_volume=False)
    pol2.keep_rules.append(Rule(2, "keep", "avoid", Selector(), folders=["/D"]))
    d2 = decide(units, pol2, _gi(units))
    assert [s != K.K_NO for s in d2.state] == [False, False, True]
    # keep_n asks for more, never fewer
    pol3 = Policy(criteria=["oldest_created"], per_volume=False)
    pol3.keep_rules.append(Rule(3, "keep", "keep_n", Selector(), n=2))
    gi = _gi(units)
    gi.group_rules = {3: True}
    d3 = decide(units, pol3, gi)
    assert sum(1 for s in d3.state if s != K.K_NO) == 2


def test_manual_not_keep_on_every_copy_uses_the_safety_net_visibly() -> None:
    units = [Unit(rep=i, rows=[i], path=f"/x/{i}.bin", vol=0, btime=i, mtime=i) for i in range(2)]
    d = decide(
        units, Policy(per_volume=False), _gi(units, manual=[ManualCopy(keep="not"), ManualCopy(keep="not")])
    )
    assert d.safety and sum(1 for s in d.state if s != K.K_NO) == 1


def test_copy_like_names() -> None:
    assert copy_stem("x (1).jpg") == "x.jpg"
    assert copy_stem("x - Copy.jpg") == "x.jpg"
    assert copy_stem("x - 副本.jpg") == "x.jpg"
    assert copy_stem("Copy of x.jpg") == "x.jpg"
    assert copy_stem("x.jpg") is None
    # only when the plain name is a member too
    assert plain_name_keys(["x.jpg", "x (1).jpg"]) == [0, 1]
    assert plain_name_keys(["x (1).jpg", "y.jpg"]) == [0, 0]


def test_policies_round_trip_through_json() -> None:
    from spaceknife.dupes.cleanup.policy import preset

    p = preset("recommended")
    p.manual[("ab", "/x")] = ManualCopy(keep="keep", action="", target="")
    q = Policy.from_json(json.loads(json.dumps(p.to_json())))
    assert q.to_json() == p.to_json()
    assert any(r.when.risky and r.effect == "leave" for r in q.action_rules)  # 「任何高风险 → 不动」


# ------------------------------------------------------------------ the journal
def test_a_line_half_written_is_ignored(tmp_path: Path) -> None:
    j = Journal.create("r-test-0001", {"steps": []}, root=tmp_path)
    j.write({"t": "run", "run": "r-test-0001"})
    j.write({"t": "intent", "seq": 1, "act": "hardlink", "path": "/x"}, sync=True)
    j.close()
    with open(tmp_path / "r-test-0001" / "journal.jsonl", "a", encoding="utf-8") as f:
        f.write('{"t":"done","seq":1,"code":"done","c":"0000')  # a crash mid-line
    with open(tmp_path / "r-test-0001" / "journal.jsonl", "a", encoding="utf-8") as f:
        f.write('\n{"t":"done","seq":1,"code":"done","c":"deadbeefdeadbeef"}\n')  # a line that does not check
    recs = read_records(tmp_path / "r-test-0001")
    assert [r["t"] for r in recs] == ["run", "intent"]
    steps, run, end = fold(recs)
    assert run is not None and end is None and steps[1].intent is not None and not steps[1].result


# ------------------------------------------------------------------ the executor
def _stamp(path: Path) -> dict[str, Any]:
    s = os.stat(path)
    return {
        "path": str(path),
        "dev": s.st_dev,
        "fid_lo": s.st_ino,
        "fid_hi": 0,
        "size": s.st_size,
        "mtime": s.st_mtime_ns,
        "change": s.st_ctime_ns,
        "usn": 0,
        "vol": 0,
    }


def _step(seq: int, action: str, copy: Path, keeper: Path, text: str = "") -> dict[str, Any]:
    c = _stamp(copy)
    k = _stamp(keeper)
    return {
        "seq": seq,
        "group": 0,
        "digest": "00",
        "action": action,
        "path": str(copy),
        "vol": 0,
        **{f: c[f] for f in ("dev", "fid_lo", "fid_hi", "size", "mtime", "change", "usn")},
        "parent": str(copy.parent),
        "parent_id": None,
        "keepers": [k],
        "target": k if action != "trash" else None,
        "text": text or (os.path.relpath(keeper, copy.parent) if action == "symlink" else ""),
        "risks": [],
        "alloc": c["size"],
        "scan": 0,
        "node": 0,
        "verify_content": False,
        "row": -1,
    }


def _ctx(
    tmp_path: Path, steps: list[dict[str, Any]], faults: str = "", *, run_id: str = "r-20261023-000000-abcd"
) -> X.RunCtx:
    fs = PosixFs(Sandbox([str(tmp_path)]), Faults.parse(faults) if faults else None)
    ctx = X.RunCtx(
        run_id=run_id,
        steps=steps,
        fs=fs,
        options={"on_failure": "continue", "verify": "metadata"},
        data_dir=tmp_path / "home" / "data",
        ops_kind="python",
        check_locks=False,
    )
    ctx.journal = Journal.create(run_id, {"steps": steps, "options": ctx.options}, root=tmp_path / "runs")
    ctx.journal.write({"t": "run", "run": run_id}, sync=True)
    return ctx


def _pair(tmp_path: Path, name: str = "a.bin", data: bytes = b"x" * 70000) -> tuple[Path, Path]:
    k = tmp_path / "k" / name
    c = tmp_path / "copy" / name
    k.parent.mkdir(parents=True, exist_ok=True)
    c.parent.mkdir(parents=True, exist_ok=True)
    k.write_bytes(data)
    c.write_bytes(data)
    return k, c


def _ino(p: Path) -> int:
    return os.lstat(p).st_ino


def _leftovers(folder: Path) -> list[str]:
    return sorted(n for n in os.listdir(folder) if n.startswith(".sk-"))


@posix_only
@pytest.mark.parametrize("action", ["hardlink", "symlink"])
def test_a_replacement_and_its_undo(tmp_path: Path, action: str) -> None:
    k, c = _pair(tmp_path)
    c_ino, k_ino = _ino(c), _ino(k)
    ctx = _ctx(tmp_path, [_step(1, action, c, k)])
    report = X.run(ctx)
    assert report["state"] == "done", ctx.outcomes
    o = ctx.outcomes[1]
    assert o.code == "done"
    if action == "hardlink":
        assert _ino(c) == k_ino
    else:
        assert (
            c.is_symlink()
            and os.readlink(c) == os.path.relpath(k, c.parent)
            and c.read_bytes() == k.read_bytes()
        )
    rec = Path(o.rec)
    assert rec.exists() and _ino(rec) == c_ino and str(rec).startswith(str(tmp_path / "home"))
    assert _leftovers(c.parent) == []
    ctx.journal.close()  # type: ignore[union-attr]
    run_dir = tmp_path / "runs" / ctx.run_id
    plan = X.undo_plan(run_dir)
    assert [(s, can) for s, _p, can, _w in plan] == [(1, True)]
    out = X.undo(ctx, run_dir)
    assert out[1][0] == "undone"
    assert _ino(c) == c_ino and not c.is_symlink() and not rec.exists()
    assert os.lstat(k).st_nlink == 1 and _leftovers(c.parent) == []


@posix_only
def test_undo_refuses_a_link_changed_after_the_run_and_can_put_the_original_beside(tmp_path: Path) -> None:
    k, c = _pair(tmp_path)
    ctx = _ctx(tmp_path, [_step(1, "symlink", c, k)])
    X.run(ctx)
    ctx.journal.close()  # type: ignore[union-attr]
    os.unlink(c)
    c.write_bytes(b"theirs")  # the user replaced the link with a file of their own
    run_dir = tmp_path / "runs" / ctx.run_id
    assert X.undo(ctx, run_dir)[1][0] == "undo_changed"
    assert c.read_bytes() == b"theirs"
    assert X.undo(ctx, run_dir, beside=True)[1][0] == "undo_beside"
    assert c.read_bytes() == b"theirs" and (c.parent / "a (原件).bin").read_bytes() == k.read_bytes()


@posix_only
@pytest.mark.parametrize(
    "fault",
    [
        "open_dir@1:eacces",
        "open_copy@1:eio",
        "open_keeper_link@1:eacces",
        "make@1:exists",
        "make@1:enospc",
        "make@1:eio",
        "exchange@1:eio",
        "rename@1:enospc",
        "rename@1:exists",
        "rename@1:eacces",
        "remove@1:eio",
    ],
)
def test_every_failure_leaves_the_copy_as_it_was_or_done(tmp_path: Path, fault: str) -> None:
    k, c = _pair(tmp_path)
    c_ino = _ino(c)
    data = c.read_bytes()
    ctx = _ctx(tmp_path, [_step(1, "hardlink", c, k)], fault)
    X.run(ctx)
    o = ctx.outcomes[1]
    # the original is somewhere: at its name, or in the recovery area when the step is done
    if o.code == "done":
        assert Path(o.rec).exists() and _ino(Path(o.rec)) == c_ino
    elif o.code == "failed_attention":
        names = [p for p in (c, *c.parent.iterdir()) if p.exists() and _ino(p) == c_ino]
        assert names, "the original must keep a name"
        assert ctx.stop_reason
    else:
        assert _ino(c) == c_ino and c.read_bytes() == data, o
        assert _leftovers(c.parent) == [] or o.code == "failed_attention"
    assert k.read_bytes() == data  # the keeper is never a step's object
    if "enospc" in fault or ("eio" in fault and fault.startswith(("make", "exchange"))):
        assert ctx.stop_reason  # a full disk or an I/O error stops the whole run


@posix_only
def test_a_name_taken_is_left_alone(tmp_path: Path) -> None:
    k, c = _pair(tmp_path)
    taken = c.parent / X.tmp_name("r-20261023-000000-abcd", 1)
    taken.write_bytes(b"someone else's")
    ctx = _ctx(tmp_path, [_step(1, "hardlink", c, k)])
    X.run(ctx)
    assert ctx.outcomes[1].code == "skipped_name_taken"
    assert taken.read_bytes() == b"someone else's" and _ino(c) != _ino(k)


class _Racing(PosixFs):
    """Another program saves the copy's path (write a new file, rename it over) right before the
    swap: the executor must notice and leave theirs in place."""

    def exchange(self, dh: int, a: str, b: str) -> None:
        if not getattr(self, "_raced", False):
            self._raced = True
            folder = self._path(dh)
            tmp = os.path.join(folder, "their-save.tmp")
            with open(tmp, "wb") as f:
                f.write(b"saved by someone")
            os.replace(tmp, os.path.join(folder, b))
        super().exchange(dh, a, b)


@posix_only
def test_a_save_racing_the_swap_is_put_back(tmp_path: Path) -> None:
    k, c = _pair(tmp_path)
    ctx = _ctx(tmp_path, [_step(1, "hardlink", c, k)])
    ctx.fs = _Racing(Sandbox([str(tmp_path)]))
    X.run(ctx)
    assert ctx.outcomes[1].code == "skipped_raced", ctx.outcomes[1]
    assert c.read_bytes() == b"saved by someone"  # theirs is back at the name, never overwritten
    assert _leftovers(c.parent) == []


@posix_only
def test_nothing_outside_the_sandbox_is_touched(tmp_path: Path) -> None:
    k, c = _pair(tmp_path)
    ctx = _ctx(tmp_path, [_step(1, "hardlink", c, k)])
    ctx.fs = PosixFs(Sandbox([str(tmp_path / "elsewhere")]))
    X.run(ctx)
    assert not ctx.outcomes[1].code.startswith("done")
    assert _ino(c) != _ino(k) and _leftovers(c.parent) == []


@posix_only
def test_a_changed_copy_is_never_replaced(tmp_path: Path) -> None:
    import time

    k, c = _pair(tmp_path)
    step = _step(1, "hardlink", c, k)
    time.sleep(0.05)  # past the file system's clock tick: the stamps move
    c.write_bytes(b"y" * 70000)  # same size, new contents
    ctx = _ctx(tmp_path, [step])
    X.run(ctx)
    assert ctx.outcomes[1].code == "skipped_changed"
    assert c.read_bytes() == b"y" * 70000


@posix_only
def test_reading_again_catches_a_change_the_stamps_cannot_show(tmp_path: Path) -> None:
    """Changed within the same clock tick as the stamps were taken (or with the time set back): only
    reading the contents again shows it; verify=auto does that for recent files (risk R11 when off)."""
    from spaceknife.dupes.guard import Stamped, _digests

    k, c = _pair(tmp_path)
    step = _step(1, "hardlink", c, k)
    ks = _stamp(k)
    digest = _digests(
        "python", [Stamped(str(k), ks["dev"], ks["fid_lo"], 0, ks["size"], ks["mtime"], ks["change"])]
    )[0]
    assert digest is not None
    step["digest"] = digest.hex()
    step["verify_content"] = True
    st0 = os.stat(c)
    c.write_bytes(b"y" * 70000)
    os.utime(c, ns=(st0.st_atime_ns, st0.st_mtime_ns))  # the time set back
    step["change"] = os.stat(c).st_ctime_ns  # and the change time as if unseen (a tick shared)
    ctx = _ctx(tmp_path, [step])
    ctx.options["verify"] = "auto"
    skip = X.preflight(ctx, [step])
    assert skip[1][0] == "skipped_changed"
    X.run(ctx, skip=skip)
    assert c.read_bytes() == b"y" * 70000 and _ino(c) != _ino(k)


@posix_only
def test_ten_failures_in_a_row_stop_the_run(tmp_path: Path) -> None:
    steps = []
    for i in range(12):
        k, c = _pair(tmp_path, f"f{i}.bin", bytes([i]) * 5000)
        steps.append(_step(i + 1, "hardlink", c, k))
    ctx = _ctx(tmp_path, steps, "exchange@*:eacces")
    X.run(ctx)
    assert ctx.stop_reason == "stop_systemic"
    assert sum(1 for o in ctx.outcomes.values() if o.code == "cancelled") >= 1


@posix_only
def test_purge_deletes_only_what_is_proven_elsewhere(tmp_path: Path) -> None:
    k1, c1 = _pair(tmp_path, "a.bin", b"a" * 9000)
    k2, c2 = _pair(tmp_path, "b.bin", b"b" * 9000)
    ctx = _ctx(tmp_path, [_step(1, "hardlink", c1, k1), _step(2, "hardlink", c2, k2)])
    X.run(ctx)
    ctx.journal.close()  # type: ignore[union-attr]
    run_dir = tmp_path / "runs" / ctx.run_id
    recs = {seq: Path(o.rec) for seq, o in ctx.outcomes.items()}
    with open(recs[2], "ab") as f:
        f.write(b"!")  # the original changed after the run (someone wrote to it in the recovery area)
    plan = {seq: can for seq, _p, can, _w, _b, _s in X.purge_plan(run_dir, ops_kind="python")}
    assert plan == {1: True, 2: False}
    out = X.purge(ctx, run_dir, verify="auto")
    assert out[1][0] == "purged" and out[2][0] == "refused"
    assert not recs[1].exists() and recs[2].exists()
    assert c1.read_bytes() == b"a" * 9000  # the content is still at its path (the keeper's name)


@posix_only
def test_in_place_sharing_where_the_file_system_cannot(tmp_path: Path) -> None:
    k, c = _pair(tmp_path, data=b"z" * (1 << 20))
    ctx = _ctx(tmp_path, [_step(1, "clone", c, k)])
    X.run(ctx)
    o = ctx.outcomes[1]
    # tmpfs and ext4 have no FIDEDUPERANGE: unavailable, nothing changed (btrfs and xfs: shared)
    assert o.code in ("skipped_unavailable", "done", "done_partial"), o
    assert c.read_bytes() == k.read_bytes() == b"z" * (1 << 20)
    assert _ino(c) != _ino(k)


CRASH_SCRIPT = textwrap.dedent(
    """
    import json, os, sys
    from pathlib import Path
    from spaceknife.dupes.cleanup import execute as X
    from spaceknife.dupes.cleanup.fsops import Faults, PosixFs, Sandbox
    from spaceknife.dupes.cleanup.journal import Journal
    tmp = Path(sys.argv[1]); steps = json.loads(Path(sys.argv[2]).read_text()); run_id = sys.argv[3]
    fs = PosixFs(Sandbox([str(tmp)]), Faults.parse(sys.argv[4]))
    ctx = X.RunCtx(run_id=run_id, steps=steps, fs=fs, options={"verify": "metadata"}, data_dir=tmp / "home" / "data", ops_kind="python", check_locks=False)
    ctx.journal = Journal.create(run_id, {"steps": steps, "options": ctx.options}, root=tmp / "runs")
    ctx.journal.write({"t": "run", "run": run_id}, sync=True)
    X.run(ctx)
    """
)


@posix_only
@pytest.mark.parametrize(
    "where", ["after_make", "after_swap", "after_move", "before_done", "exchange", "rename"]
)
@pytest.mark.parametrize("action", ["hardlink", "symlink"])
def test_a_crash_at_any_point_is_finished_from_the_journal(tmp_path: Path, where: str, action: str) -> None:
    k, c = _pair(tmp_path)
    c_ino, k_ino = _ino(c), _ino(k)
    steps = [_step(1, action, c, k)]
    sf = tmp_path / "steps.json"
    sf.write_text(json.dumps(steps))
    run_id = "r-20261023-000000-dead"
    proc = subprocess.run(
        [sys.executable, "-c", CRASH_SCRIPT, str(tmp_path), str(sf), run_id, f"{where}@1:exit"],
        capture_output=True,
        timeout=60,
        check=False,
    )
    assert proc.returncode == 137, proc.stderr.decode(errors="replace")[-2000:]
    run_dir = tmp_path / "runs" / run_id
    assert X.interrupted(run_dir)
    fs = PosixFs(Sandbox([str(tmp_path)]))
    ctx = X.RunCtx(
        run_id=run_id, steps=steps, fs=fs, options={}, data_dir=tmp_path / "home" / "data", ops_kind="python"
    )
    counts = X.recover(ctx, run_dir)
    assert not X.interrupted(run_dir)
    st_, _, end = fold(read_records(run_dir))
    assert end is not None and end["state"] == "recovered"
    result = st_[1].result
    if result.startswith("done"):
        assert (_ino(c) == k_ino) if action == "hardlink" else c.is_symlink()
        rec = Path(st_[1].done["rec"])  # type: ignore[index]
        assert _ino(rec) == c_ino
    else:
        assert result in ("failed_rolled_back", "cancelled"), (result, counts)
        assert _ino(c) == c_ino and not c.is_symlink()
    assert _leftovers(c.parent) == []
    assert k.read_bytes() == c.read_bytes()
    # recovering twice changes nothing
    assert X.recover(ctx, run_dir) == {"restored": 0, "done": 0, "attention": 0, "cancelled": 0, "at": 0}
