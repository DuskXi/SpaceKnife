"""Running an approved cleanup plan (docs/dupes.md §16.8–16.10).

Invariants enforced here (tests assert each):

- I1 a group's content stays in a checked keeper the run never touches; a keeper is never a step;
- I2 no name is ever replaced: swaps (Linux, macOS), no-replace renames (Windows, the recovery
  area), exclusive creates of temporary names; the original always has a name (its own, the
  temporary T, or R in the recovery area);
- I3 every name is checked (identity, stamps, link count) through a handle or a folder descriptor
  before it is touched; the executor only deletes names it made, after checking them again;
- I4 only purging cannot be undone (separately confirmed, after a 5 s delay, every original and
  keeper checked first);
- I6 the preflight and the run may skip a step, never add or change one;
- I10 a batch's intent is in the journal (fsynced) before its first step; an interrupted run is
  finished from the journal and what is on disk before anything else (:func:`recover`);
- I13 with a sandbox, nothing outside it is touched (checked in every primitive).

Per step (hard link, symbolic link): R1 open the folder (identity as approved) · R2 open the copy
(Windows: others may only read) · R3 open the keeper · R5 make the stand-in under a temporary name
and check it · R6 check the copy again · R7 put it in place without replacing anything (POSIX: swap
the two names, check the swapped-out one is the copy; Windows: the copy to the recovery area by
handle, then the stand-in to the copy's name, and back if that fails) · R8 (POSIX) the original to
the recovery area · R9 check the result. A failure rolls back to exactly the state before and
records ``failed_rolled_back``; if that cannot be proven, ``failed_attention`` and the run stops.
"""

from __future__ import annotations

import contextlib
import os
import sys
import threading
import time
from collections.abc import Callable, Sequence
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from ...i18n import Texts
from .fsops import FsError, FsOps, St
from .journal import Journal, JournalError, StepState, fold, read_plan, read_records
from .recovery import Place, place_for, remove_if_empty, write_manifest

BATCH = 64
SAME_FAILURES_STOP = 10
DEDUPE_SEG = 16 << 20
CLONE_SEG = 64 << 20

TEXT = Texts(
    {
        "done": "完成",
        "done_partial": "完成，部分数据块已共用",
        "skipped_changed": "核对之后这个文件变过，没有动",
        "skipped_keeper_changed": "保留的那份变了，这一组没有动",
        "skipped_keeper_lost": "保留的那份不在了或者要被删掉，这一组没有动",
        "skipped_keeper_busy": "保留的那份正被别的程序打开，这一组没有动",
        "skipped_locked": "它正被别的程序打开，没有动",
        "skipped_denied": "没有权限，没有动",
        "skipped_missing": "它已经不在了",
        "skipped_protected": "受保护的位置，没有动",
        "skipped_ambiguous_name": "按名字打开的可能是另一项（只差大小写或名字以点、空格结尾），没有动",
        "skipped_in_basket": "它在删除篮里，留给删除篮处理",
        "skipped_parent_changed": "它所在的文件夹换过了，没有动",
        "skipped_name_taken": "临时名字被占用了，没有动",
        "skipped_raced": "替换的那一刻别的程序动了这个路径：已经换回原样",
        "skipped_unavailable": "到这里才发现这个处理做不了，没有动",
        "skipped_no_right": "没有建符号链接的权限，没有动",
        "skipped_link_limit": "保留那份的名字到了上限，没有动",
        "skipped_fs_limit": "文件系统的引用数到了上限，没有动",
        "skipped_risk_rose": "执行前发现新的风险，没有动",
        "skipped_trash_full": "回收站放不下，没有动（从不改成永久删除）",
        "skipped_error": "没有动：{why}",
        "failed_rolled_back": "中途出错，已经恢复原样并核对过：{why}",
        "failed_attention": "中途出错，没能证明恢复了原样：要你看一下（{why}）",
        "cancelled": "没有开始",
        "would_do": "预检通过",
        "stop_systemic": "看起来是系统性的问题（连续 {n} 项同样的失败），已经停下",
        "stop_disk": "磁盘满了或者读写出错，已经停下",
        "stop_journal": "写不了清理日志，已经停下",
        "stop_attention": "有一项要你看一下，已经停下",
        "stop_user": "按你的要求停下了",
        "stop_resolves_elsewhere": "符号链接指到了别处：对那里路径的理解有误，已经停下",
        "undone": "已撤销",
        "undo_changed": "这一项清理之后被改过，撤销会丢掉这些改动，没有撤销",
        "undo_taken": "原来的位置已经有东西，没有覆盖",
        "undo_nothing": "共用数据块（没有可撤销的：内容、名字和属性都没变）",
        "undo_beside": "原件已还原到旁边：{path}",
        "purged": "已清除",
        "purge_original_changed": "原件在清理之后变过：没有清除",
        "purge_keeper_changed": "保留的那份改过了或不在了：恢复区里的这份可能是唯一的一份，没有清除",
        "purge_streams": "它有没比较过的附加数据：要单独确认",
        "purge_in_trash": "在回收站里：要清空回收站才腾出（SpaceKnife 不替你清空回收站）",
        "original_suffix": " (原件)",
        "interrupted": (
            "上次的清理在第 {seq} 项时中断了（SpaceKnife 退出或系统重启）。"
            "已核对：{restored} 项恢复原样，{done} 项已完成，{attention} 项要你看一下"
        ),
        "attention_state": "原件在 {rec}，{path} 现在是{what}，没有删任何东西",
        "what_missing": "空的",
        "what_other": "别的东西",
        "what_standin": "替身",
    }
)
STOPPING = frozenset({"failed_attention"})


def text_of(code: str, why: str = "") -> str:
    try:
        return TEXT[code].format(why=why or "—", path=why, n=SAME_FAILURES_STOP)
    except KeyError:
        return code


@dataclass(slots=True)
class Outcome:
    seq: int
    code: str
    why: str = ""
    stop: str = ""  # a reason to stop the whole run
    rec: str = ""  # the original's path in the recovery area
    rec_stamps: dict[str, int] | None = None
    keeper_after: dict[str, int] | None = None
    copy_after: dict[str, int] | None = None
    trash_ref: str = ""
    shared: int = 0
    freed_now: int = 0
    freed_later: int = 0


@dataclass(slots=True)
class RunCtx:
    run_id: str
    steps: list[dict[str, Any]]
    fs: FsOps
    options: dict[str, str]
    data_dir: Path
    ops_kind: str = "native"
    keep_atime: str = "skip"
    at_root: bool = True
    no_trash_recovery: bool = True
    check_locks: bool = True
    elsewhere: Any = None  # dupes.guard.Elsewhere of the delete basket
    roots: dict[str, str] = field(default_factory=dict)  # copy path -> its scan root
    lockers: Callable[[list[str]], dict[str, list[str]]] | None = None
    progress: Callable[[str, int, int, str, dict[str, int]], None] | None = None
    on_batch: Callable[[list[tuple[dict[str, Any], Outcome]]], None] | None = None
    cancel: threading.Event = field(default_factory=threading.Event)
    pause: threading.Event = field(default_factory=threading.Event)
    permit: Callable[[str], None] | None = None  # the disk's permit for a batch
    places: dict[int, Place] = field(default_factory=dict)
    journal: Journal | None = None
    counts: dict[str, int] = field(default_factory=dict)
    outcomes: dict[int, Outcome] = field(default_factory=dict)
    stop_reason: str = ""
    no_symlinks: bool = False  # the process lost the right to make them (ERROR_PRIVILEGE_NOT_HELD)
    # keepers this run changed itself (a hard link adds a name: change time, USN and link count
    # move): their stamps right after that step, which later steps compare with
    seen: dict[str, dict[str, int]] = field(default_factory=dict)
    bad_volumes: set[int] = field(default_factory=set)  # volumes where an action proved impossible


def _stamps(st: St) -> dict[str, int]:
    return {
        "dev": st.dev,
        "fid_lo": st.fid_lo,
        "fid_hi": st.fid_hi,
        "size": st.size,
        "mtime": st.mtime,
        "change": st.change,
        "nlink": st.nlink,
        "usn": st.usn,
    }


def _same(st: St, want: dict[str, Any], *, change: bool = True, size: bool = True) -> bool:
    if (st.dev, st.fid_lo, st.fid_hi) != (int(want["dev"]), int(want["fid_lo"]), int(want["fid_hi"])):
        return False
    if size and st.size != int(want["size"]):
        return False
    if st.mtime != int(want["mtime"]):
        return False
    if change and int(want.get("change", 0)) and st.change != int(want["change"]):
        return False
    wu = int(want.get("usn", 0) or 0)
    return not (change and wu and st.usn and st.usn != wu)


def _ident(want: dict[str, Any]) -> tuple[int, int, int]:
    return (int(want["dev"]), int(want["fid_lo"]), int(want["fid_hi"]))


# ------------------------------------------------------------------ preflight (read only)
def preflight(ctx: RunCtx, steps: Sequence[dict[str, Any]]) -> dict[int, tuple[str, str]]:
    """Per seq that cannot go ahead: (code, detail). Reads metadata (and, with ``verify``, the
    contents of the pairs it asks for); writes nothing."""
    from ...ops.paths import protected_reason
    from ...ops.trash import reaches_another, win32_trims
    from ..guard import _digests, _folder_ids, _Reach
    from ..reads import FallbackOps, default_ops

    ops = default_ops() if ctx.ops_kind == "native" else FallbackOps()
    out: dict[int, tuple[str, str]] = {}
    names: list[str] = []
    for s in steps:
        names.append(s["path"])
        names.extend(k["path"] for k in s["keepers"])
        if s.get("target"):
            names.append(s["target"]["path"])
    uniq = list(dict.fromkeys(names))
    now: dict[str, dict[str, int]] = {}
    for i in range(0, len(uniq), 256):
        chunk = uniq[i : i + 256]
        res = ops.identity(chunk, True)
        for j, p in enumerate(chunk):
            now[p] = {k: int(v[j]) for k, v in res.items()}
    el = ctx.elsewhere
    covers = el.covers if el is not None else None
    reach = _Reach(_folder_ids(el.plain())) if el is not None else None
    busy: dict[str, list[str]] = {}
    if ctx.check_locks and ctx.lockers is not None:
        busy = ctx.lockers(uniq)
    links_to: dict[str, int] = {}

    def intact(k: dict[str, Any]) -> str:
        row = now.get(k["path"])
        if row is None or row["reason"] != 0:
            return "skipped_keeper_lost"
        if covers is not None and (covers(k["path"]) or (reach is not None and reach.lost(k["path"]))):
            return "skipped_keeper_lost"
        if (row["dev"], row["fid_lo"], row["fid_hi"]) != _ident(k):
            return "skipped_keeper_changed"
        if row["size"] != int(k["size"]) or row["mtime"] != int(k["mtime"]):
            return "skipped_keeper_changed"
        if int(k.get("change", 0)) and row["change"] != int(k["change"]):
            return "skipped_keeper_changed"
        if int(k.get("usn", 0)) and row.get("usn") and row["usn"] != int(k["usn"]):
            return "skipped_keeper_changed"
        if busy.get(k["path"]):
            return "skipped_keeper_busy"
        return ""

    for s in steps:
        seq = int(s["seq"])
        path = s["path"]
        row = now.get(path)
        if row is None or row["reason"] == 1:
            out[seq] = ("skipped_missing", "")
            continue
        if row["reason"] != 0:
            out[seq] = ("skipped_changed", "")
            continue
        if (
            (row["dev"], row["fid_lo"], row["fid_hi"]) != _ident(s)
            or row["size"] != int(s["size"])
            or row["mtime"] != int(s["mtime"])
        ):
            out[seq] = ("skipped_changed", "")
            continue
        if int(s.get("change", 0)) and row["change"] != int(s["change"]):
            out[seq] = ("skipped_changed", "")
            continue
        if int(s.get("usn", 0)) and row.get("usn") and row["usn"] != int(s["usn"]):
            out[seq] = ("skipped_changed", "")
            continue
        if s["action"] != "trash" and row.get("nlink", 1) > 1:
            out[seq] = ("skipped_changed", "")
            continue
        if covers is not None and covers(path):
            out[seq] = ("skipped_in_basket", "")
            continue
        if protected_reason(path) is not None:
            out[seq] = ("skipped_protected", "")
            continue
        root = ctx.roots.get(path, "")
        if win32_trims(path) or (root and reaches_another(path, root)):
            out[seq] = ("skipped_ambiguous_name", "")
            continue
        if busy.get(path):
            out[seq] = ("skipped_locked", "")
            continue
        if int(s["vol"]) in ctx.bad_volumes:
            out[seq] = ("skipped_unavailable", "")
            continue
        t = s.get("target")
        if t is not None:
            why = intact(t)
            if why:
                out[seq] = (why, "")
                continue
            if t["path"] == path or _ident(t) == _ident(s):
                out[seq] = ("skipped_keeper_changed", "")
                continue
        else:
            whys = [intact(k) for k in s["keepers"]]
            if not any(w == "" for w in whys):
                out[seq] = (next((w for w in whys if w), "skipped_keeper_lost"), "")
                continue
        if s["action"] == "hardlink" and t is not None:
            links_to[t["path"]] = links_to.get(t["path"], 0) + 1
            limit = 1024 if sys.platform == "win32" else 65000
            if now[t["path"]].get("nlink", 1) + links_to[t["path"]] > limit:
                out[seq] = ("skipped_link_limit", "")
                continue
        verify = ctx.options.get("verify", "auto")
        need = verify == "content" or (verify == "auto" and bool(s.get("verify_content")))
        if need and s["action"] != "clone":
            other = t if t is not None else next((k for k in s["keepers"] if not intact(k)), None)
            if other is not None:
                from ..guard import Stamped

                pair: list[dict[str, Any]] = [s, other]
                files = [
                    Stamped(
                        x["path"],
                        int(x["dev"]),
                        int(x["fid_lo"]),
                        int(x["fid_hi"]),
                        int(x["size"]),
                        int(x["mtime"]),
                        int(x["change"]),
                        int(x.get("usn", 0)),
                    )
                    for x in pair
                ]
                got = _digests(ctx.ops_kind, files, ctx.keep_atime)
                if got[0] is None or got[0] != got[1] or got[0].hex() != s["digest"]:
                    out[seq] = ("skipped_changed", "")
                    continue
    return out


# ------------------------------------------------------------------ helpers of a run
def _code_of(e: FsError, *, where: str) -> str:
    c = e.code
    if c == "missing":
        return "skipped_keeper_lost" if where == "keeper" else "skipped_missing"
    if c == "locked":
        return "skipped_keeper_busy" if where == "keeper" else "skipped_locked"
    if c == "denied":
        return "skipped_denied"
    if c == "exists":
        return "skipped_name_taken"
    if c in ("exdev", "unsupported", "readonly_fs"):
        return "skipped_unavailable"
    if c == "link_limit":
        return "skipped_link_limit"
    if c == "no_privilege":
        return "skipped_no_right"
    if c == "fs_limit":
        return "skipped_fs_limit"
    if c == "sandbox":
        return "skipped_denied"
    return "failed_rolled_back"


def _stop_for(e: FsError) -> str:
    return "stop_disk" if e.code in ("enospc", "eio") else ""


class _Handles:
    def __init__(self, fs: FsOps) -> None:
        self.fs = fs
        self.hs: list[int] = []

    def add(self, h: int) -> int:
        self.hs.append(h)
        return h

    def close(self) -> None:
        for h in reversed(self.hs):
            with contextlib.suppress(Exception):
                self.fs.close(h)
        self.hs.clear()


def _place(ctx: RunCtx, folder: str, dev: int) -> Place:
    p = ctx.places.get(dev)
    if p is None:
        p = place_for(
            ctx.fs, ctx.run_id, folder, sandbox=ctx.fs.sandbox, at_root=ctx.at_root, data_dir=ctx.data_dir
        )
        ctx.places[dev] = p
    return p


def tmp_name(run_id: str, seq: int) -> str:
    return f".sk-{run_id[-6:]}-{seq}.tmp"


# ------------------------------------------------------------------ one replacement step
def do_replace(ctx: RunCtx, s: dict[str, Any]) -> Outcome:
    fs = ctx.fs
    seq = int(s["seq"])
    fs.seq = seq
    act = s["action"]
    folder = s["parent"]
    name = os.path.basename(s["path"])
    k = s["target"]
    kfolder, kname = os.path.split(k["path"])
    T = tmp_name(ctx.run_id, seq)
    H = _Handles(fs)
    journal = ctx.journal
    assert journal is not None
    stage = "open"
    made: St | None = None
    try:
        try:
            dh = H.add(fs.open_dir(folder))
        except FsError as e:
            return Outcome(seq, _code_of(e, where="dir"), str(e))
        dst = fs.fstat(dh)
        pid = s.get("parent_id")
        if pid and (dst.dev, dst.fid_lo, dst.fid_hi) != tuple(pid):
            return Outcome(seq, "skipped_parent_changed")
        try:
            ch = H.add(fs.open_entry(dh, name, purpose="copy"))
        except FsError as e:
            return Outcome(seq, _code_of(e, where="copy"), str(e))
        cst = fs.fstat(ch)
        if cst.kind != "file" or cst.nlink != 1 or not _same(cst, s):
            return Outcome(seq, "skipped_changed")
        try:
            kdh = H.add(fs.open_dir(kfolder))
            kh = H.add(fs.open_entry(kdh, kname, purpose="keeper_link" if act == "hardlink" else "keeper"))
        except FsError as e:
            return Outcome(seq, _code_of(e, where="keeper"), str(e))
        kst = fs.fstat(kh)
        if kst.kind != "file" or not _same(kst, k):
            return Outcome(seq, "skipped_keeper_changed")
        # the recovery area (same volume): its folder handle before anything is made
        place = _place(ctx, folder, dst.dev)
        rfolder, rname = place.name_for(seq, name, ctx.run_id)
        rdh = dh if not rfolder else H.add(fs.open_dir(rfolder))
        if rfolder and fs.fstat(rdh).dev != dst.dev:
            return Outcome(seq, "skipped_unavailable", "recovery area on another volume")
        rec_path = os.path.join(rfolder or folder, rname)
        # R5 the stand-in under a temporary name
        stage = "make"
        try:
            if act == "hardlink":
                fs.make_hardlink(kh, kdh, kname, dh, T)
            else:
                if ctx.no_symlinks:
                    return Outcome(seq, "skipped_no_right")
                fs.make_symlink(dh, folder, T, s["text"])
        except FsError as e:
            code = _code_of(e, where="make")
            if code == "skipped_no_right":
                ctx.no_symlinks = True
            if code == "skipped_unavailable":
                ctx.bad_volumes.add(int(s["vol"]))
            return Outcome(
                seq, code if code != "failed_rolled_back" else "failed_rolled_back", str(e), stop=_stop_for(e)
            )
        made = fs.stat_at(dh, T)
        if made is None:
            # the folder was swapped while the link was made through its path (Windows symlinks)
            return Outcome(seq, "failed_attention", "stand-in not found in the folder", stop="stop_attention")
        if act == "hardlink":
            if made.ident != kst.ident:
                return _undo_made(
                    ctx, H, dh, T, made, Outcome(seq, "failed_rolled_back", "link is not the keeper")
                )
        else:
            text = fs.read_link(dh, T)
            reached = fs.stat_at(dh, T, follow=True)
            if made.kind != "link" or text != s["text"]:
                return _undo_made(ctx, H, dh, T, made, Outcome(seq, "failed_rolled_back", "link text"))
            if reached is None or reached.ident != kst.ident:
                return _undo_made(
                    ctx,
                    H,
                    dh,
                    T,
                    made,
                    Outcome(
                        seq,
                        "failed_rolled_back",
                        "symlink_resolves_elsewhere",
                        stop="stop_resolves_elsewhere",
                    ),
                )
        journal.write({"t": "step", "seq": seq, "p": "made", "id": list(made.ident), "kind": made.kind})
        fs.fault("after_make")
        # R6 the copy did not change meanwhile
        stage = "recheck"
        cst2 = fs.fstat(ch)
        if not _same(cst2, s):
            return _undo_made(ctx, H, dh, T, made, Outcome(seq, "skipped_changed"))
        if fs.swaps:
            return _swap_posix(ctx, H, s, dh, ch, cst, kh, kst, made, T, name, rdh, rname, rec_path, act)
        return _swap_windows(ctx, H, s, dh, ch, cst, kh, kst, made, T, name, rdh, rname, rec_path, act)
    except FsError as e:
        if made is not None and stage in ("make", "recheck"):
            return _undo_made(
                ctx, H, dh, T, made, Outcome(seq, "failed_rolled_back", str(e), stop=_stop_for(e))
            )
        return Outcome(seq, _code_of(e, where=stage), str(e), stop=_stop_for(e))
    finally:
        H.close()


def _undo_made(ctx: RunCtx, H: _Handles, dh: int, T: str, made: St, out: Outcome) -> Outcome:
    """Remove our stand-in (checked to be ours); a failure to do so needs the user."""
    try:
        ok = ctx.fs.remove_own(dh, T, made.ident)
    except FsError as e:
        return Outcome(
            out.seq, "failed_attention", f"{out.why}; removing the stand-in: {e}", stop="stop_attention"
        )
    if not ok:
        return Outcome(
            out.seq,
            "failed_attention",
            f"{out.why}; the temporary name holds something else",
            stop="stop_attention",
        )
    return out


def _verify_final(ctx: RunCtx, s: dict[str, Any], dh: int, name: str, kst: St, made: St, act: str) -> bool:
    fs = ctx.fs
    fin = fs.stat_at(dh, name)
    if fin is None:
        return False
    if act == "hardlink":
        return fin.ident == kst.ident
    if fin.kind != "link" or fin.ident != made.ident:
        return False
    if fs.read_link(dh, name) != s["text"]:
        return False
    reached = fs.stat_at(dh, name, follow=True)
    return reached is not None and reached.ident == kst.ident


def _done(ctx: RunCtx, s: dict[str, Any], kh: int, kst: St, rec_path: str, rec: St, act: str) -> Outcome:
    kafter = ctx.fs.fstat(kh)
    if kafter.size != kst.size or kafter.mtime != kst.mtime:
        # the keeper changed under us: everything is in place and the original is kept; record it
        pass
    return Outcome(
        int(s["seq"]),
        "done",
        rec=rec_path,
        rec_stamps=_stamps(rec),
        keeper_after=_stamps(kafter),
        freed_later=int(s.get("alloc", 0)),
    )


def _swap_posix(
    ctx: RunCtx,
    H: _Handles,
    s: dict[str, Any],
    dh: int,
    ch: int,
    cst: St,
    kh: int,
    kst: St,
    made: St,
    T: str,
    name: str,
    rdh: int,
    rname: str,
    rec_path: str,
    act: str,
) -> Outcome:
    fs = ctx.fs
    seq = int(s["seq"])
    journal = ctx.journal
    assert journal is not None
    # R7 swap: the stand-in takes the name, the original goes to T
    try:
        fs.exchange(dh, T, name)
    except FsError as e:
        if e.code == "unsupported":
            ctx.bad_volumes.add(int(s["vol"]))
            return _undo_made(ctx, H, dh, T, made, Outcome(seq, "skipped_unavailable", str(e)))
        return _undo_made(ctx, H, dh, T, made, Outcome(seq, "failed_rolled_back", str(e), stop=_stop_for(e)))
    at_t = fs.stat_at(dh, T)
    if at_t is None or at_t.ident != cst.ident:
        # someone replaced the name between our checks and the swap: swap back, leave theirs there
        try:
            fs.exchange(dh, T, name)
        except FsError as e:
            return Outcome(seq, "failed_attention", f"swap back: {e}", stop="stop_attention")
        back = fs.stat_at(dh, T)
        if back is None or back.ident != made.ident:
            return Outcome(seq, "failed_attention", "swap back did not restore", stop="stop_attention")
        return _undo_made(ctx, H, dh, T, made, Outcome(seq, "skipped_raced"))
    journal.write({"t": "step", "seq": seq, "p": "swapped"})
    fs.fault("after_swap")
    # R8 the original to the recovery area, never replacing a name
    try:
        fs.rename_at(dh, T, rdh, rname)
    except FsError as e:
        # put the original back: swap again, then remove the stand-in
        try:
            fs.exchange(dh, T, name)
        except FsError as e2:
            return Outcome(seq, "failed_attention", f"{e}; swap back: {e2}", stop="stop_attention")
        orig = fs.stat_at(dh, name)
        if orig is None or orig.ident != cst.ident:
            return Outcome(seq, "failed_attention", f"{e}; the original is not back", stop="stop_attention")
        code = "skipped_name_taken" if e.code == "exists" else "failed_rolled_back"
        return _undo_made(ctx, H, dh, T, made, Outcome(seq, code, str(e), stop=_stop_for(e)))
    rec = fs.stat_at(rdh, rname)
    if rec is None or rec.ident != cst.ident:
        return Outcome(
            seq, "failed_attention", "the original is not where it was moved", stop="stop_attention"
        )
    journal.write({"t": "step", "seq": seq, "p": "moved", "rec": rec_path})
    fs.fault("after_move")
    if not _verify_final(ctx, s, dh, name, kst, made, act):
        return Outcome(seq, "failed_attention", "final check", stop="stop_attention", rec=rec_path)
    return _done(ctx, s, kh, kst, rec_path, rec, act)


def _swap_windows(
    ctx: RunCtx,
    H: _Handles,
    s: dict[str, Any],
    dh: int,
    ch: int,
    cst: St,
    kh: int,
    kst: St,
    made: St,
    T: str,
    name: str,
    rdh: int,
    rname: str,
    rec_path: str,
    act: str,
) -> Outcome:  # pragma: no cover - Windows (run on the Windows checkout)
    fs = ctx.fs
    seq = int(s["seq"])
    journal = ctx.journal
    assert journal is not None
    rename_h = fs.rename_h
    # R7a the original to the recovery area, by its handle, no replace
    try:
        rename_h(ch, rdh, rname)
    except FsError as e:
        code = "skipped_name_taken" if e.code == "exists" else _code_of(e, where="copy")
        return _undo_made(ctx, H, dh, T, made, Outcome(seq, code, str(e), stop=_stop_for(e)))
    journal.write({"t": "step", "seq": seq, "p": "moved", "rec": rec_path})
    fs.fault("after_move")
    # R7b the stand-in to the original's name, no replace
    try:
        th = H.add(fs.open_entry(dh, T, purpose="own"))
        if fs.fstat(th).ident != made.ident:
            raise FsError("wrong_ident", "the temporary name changed")
        rename_h(th, dh, name)
    except FsError as e:
        # the name was taken by someone (or the stand-in moved): the original back by its handle
        try:
            rename_h(ch, dh, name)
        except FsError as e2:
            with contextlib.suppress(FsError):
                fs.remove_own(dh, T, made.ident)
            return Outcome(
                seq,
                "failed_attention",
                f"{e}; the original stays in the recovery area ({e2})",
                stop="stop_attention",
                rec=rec_path,
            )
        removed = False
        with contextlib.suppress(FsError):
            removed = fs.remove_own(dh, T, made.ident)
        if not removed:
            return Outcome(
                seq, "failed_attention", "the stand-in could not be removed", stop="stop_attention"
            )
        return Outcome(seq, "skipped_raced" if e.code == "exists" else "failed_rolled_back", str(e))
    journal.write({"t": "step", "seq": seq, "p": "swapped"})
    fs.fault("after_swap")
    rec = fs.fstat(ch)
    if rec.ident != cst.ident:
        return Outcome(
            seq, "failed_attention", "the original changed identity", stop="stop_attention", rec=rec_path
        )
    if not _verify_final(ctx, s, dh, name, kst, made, act):
        return Outcome(seq, "failed_attention", "final check", stop="stop_attention", rec=rec_path)
    return _done(ctx, s, kh, kst, rec_path, rec, act)


# ------------------------------------------------------------------ block clones
def do_clone(ctx: RunCtx, s: dict[str, Any]) -> Outcome:
    fs = ctx.fs
    seq = int(s["seq"])
    fs.seq = seq
    folder, name = os.path.split(s["path"])
    k = s["target"]
    kfolder, kname = os.path.split(k["path"])
    H = _Handles(fs)
    try:
        try:
            dh = H.add(fs.open_dir(folder))
            ch = H.add(fs.open_entry(dh, name, purpose="data" if fs.swaps else "data_rw"))
        except FsError as e:
            return Outcome(seq, _code_of(e, where="copy"), str(e))
        cst = fs.fstat(ch)
        if cst.kind != "file" or not _same(cst, s):
            return Outcome(seq, "skipped_changed")
        try:
            kdh = H.add(fs.open_dir(kfolder))
            kh = H.add(fs.open_entry(kdh, kname, purpose="data"))
        except FsError as e:
            return Outcome(seq, _code_of(e, where="keeper"), str(e))
        kst = fs.fstat(kh)
        if kst.kind != "file" or not _same(kst, k):
            return Outcome(seq, "skipped_keeper_changed")
        size = int(s["size"])
        if fs.swaps:
            return _dedupe_linux(ctx, H, s, dh, name, ch, cst, kh, kst, size)
        return _clone_windows(ctx, s, ch, cst, kh, kst, size, folder)
    except FsError as e:
        return Outcome(seq, _code_of(e, where="copy"), str(e), stop=_stop_for(e))
    finally:
        H.close()


def _extents_equal(paths: list[str]) -> bool | None:
    try:
        from ..reads import default_ops

        got = default_ops().extents(paths)
    except Exception:
        return None
    if got is None:
        return None
    if any(int(r) != 0 for r in got["reason"]):
        return None
    h = got["hash"]
    if not all(int(c) for c in got["complete"]):
        return False
    return bytes(h[0:16]) == bytes(h[16:32])


def _dedupe_linux(
    ctx: RunCtx,
    H: _Handles,
    s: dict[str, Any],
    dh: int,
    name: str,
    ch: int,
    cst: St,
    kh: int,
    kst: St,
    size: int,
) -> Outcome:
    fs = ctx.fs
    seq = int(s["seq"])
    dedupe = fs.dedupe
    off = 0
    reopened = False
    while off < size:
        n = min(DEDUPE_SEG, size - off)
        if ctx.permit is not None:
            ctx.permit(s["path"])
        try:
            done, status = dedupe(kh, ch, off, n)
        except FsError as e:
            if e.code == "denied" and not reopened:
                reopened = True
                try:
                    ch = H.add(fs.open_entry(dh, name, purpose="data_rw"))
                except FsError:
                    return Outcome(seq, "skipped_denied" if off == 0 else "done_partial", str(e), shared=off)
                if not _same(fs.fstat(ch), s):
                    return Outcome(seq, "skipped_changed", shared=off)
                continue
            if e.code == "unsupported":
                ctx.bad_volumes.add(int(s["vol"]))
                return Outcome(seq, "skipped_unavailable" if off == 0 else "done_partial", str(e), shared=off)
            return Outcome(
                seq,
                "failed_rolled_back" if off == 0 else "done_partial",
                str(e),
                stop=_stop_for(e),
                shared=off,
            )
        if status == 1:  # FILE_DEDUPE_RANGE_DIFFERS: the kernel compared them byte for byte
            return Outcome(seq, "skipped_changed", "contents differ", shared=off)
        if done <= 0:
            break  # the tail the kernel would not share
        off += done
    after = fs.fstat(ch)
    eq = _extents_equal([s["target"]["path"], s["path"]])
    code = "done" if (eq is True or (eq is None and off >= size)) else "done_partial"
    return Outcome(
        seq,
        code,
        shared=off,
        copy_after=_stamps(after),
        keeper_after=_stamps(fs.fstat(kh)),
        freed_now=int(s.get("alloc", 0)) if code == "done" else off,
    )


def _clone_windows(
    ctx: RunCtx, s: dict[str, Any], ch: int, cst: St, kh: int, kst: St, size: int, folder: str
) -> Outcome:  # pragma: no cover - Windows
    from . import winfs

    fs = ctx.fs
    seq = int(s["seq"])
    if cst.attrs & winfs.ATTR_READONLY:
        return Outcome(seq, "skipped_unavailable", "read-only")
    if (cst.attrs ^ kst.attrs) & (
        winfs.ATTR_SPARSE | winfs.ATTR_ENCRYPTED
    ) or cst.attrs & winfs.ATTR_ENCRYPTED:
        return Outcome(seq, "skipped_unavailable", "sparse or encrypted differ")
    if winfs.integrity(ch) != winfs.integrity(kh):
        return Outcome(seq, "skipped_unavailable", "integrity streams differ")
    held = winfs.suspend_times(ch)
    cluster = winfs.cluster_size(folder)
    seg = max(cluster, CLONE_SEG - CLONE_SEG % cluster)
    end = size - size % cluster  # the last partial cluster is not cloned (phase 1)
    off = 0
    try:
        while off < end:
            n = min(seg, end - off)
            if ctx.permit is not None:
                ctx.permit(s["path"])
            a = fs.read_at(kh, n, off)
            b = fs.read_at(ch, n, off)
            if a != b or len(a) != n:
                return Outcome(
                    seq, "skipped_changed" if off == 0 else "done_partial", "contents differ", shared=off
                )
            fs.clone_range(ch, kh, off, n)
            off += n
    except FsError as e:
        if e.code == "fs_limit":
            return Outcome(seq, "skipped_fs_limit" if off == 0 else "done_partial", str(e), shared=off)
        if e.code == "unsupported":
            ctx.bad_volumes.add(int(s["vol"]))
            return Outcome(seq, "skipped_unavailable" if off == 0 else "done_partial", str(e), shared=off)
        return Outcome(
            seq, "failed_rolled_back" if off == 0 else "done_partial", str(e), stop=_stop_for(e), shared=off
        )
    after = fs.fstat(ch)
    restored = False
    if after.mtime != cst.mtime:
        restored = winfs.set_mtime(ch, cst.mtime)
        after = fs.fstat(ch)
    eq = _extents_equal([s["target"]["path"], s["path"]])
    code = "done" if eq is True and end == size else "done_partial"
    return Outcome(
        seq,
        code,
        why="mtime restored" if restored else ("" if held else "times not held"),
        shared=off,
        copy_after=_stamps(after),
        keeper_after=_stamps(fs.fstat(kh)),
        freed_now=off,
    )


# ------------------------------------------------------------------ the recycle bin
def do_trash_batch(ctx: RunCtx, batch: list[dict[str, Any]]) -> list[Outcome]:
    from ...ops.trash import move_to_trash, trash_status
    from ..guard import Guard, Stamped

    def stamped(x: dict[str, Any]) -> Stamped:
        return Stamped(
            x["path"],
            int(x["dev"]),
            int(x["fid_lo"]),
            int(x["fid_hi"]),
            int(x["size"]),
            int(x["mtime"]),
            int(x["change"]),
            int(x.get("usn", 0)),
        )

    out: list[Outcome] = []
    todo: list[dict[str, Any]] = []
    for s in batch:
        ctx.fs.guard(s["path"])  # the sandbox: refuses before anything
        st = trash_status(s["path"], usage=False)
        if not st.available:
            if ctx.no_trash_recovery:
                out.append(_to_recovery(ctx, s))
            else:
                out.append(Outcome(int(s["seq"]), "skipped_unavailable", st.reason))
            continue
        todo.append(s)
    if not todo:
        return out
    ctx.fs.seq = int(todo[0]["seq"])
    ctx.fs.fault("trash")
    paths = [s["path"] for s in todo]
    guards = {
        s["path"]: Guard(
            member=stamped(s),
            keepers=tuple(stamped(k) for k in s["keepers"]),
            ops=ctx.ops_kind,
            recheck_content=False,
            keep_atime=ctx.keep_atime,
        )
        for s in todo
    }
    roots = {p: ctx.roots[p] for p in paths if p in ctx.roots}
    results = move_to_trash(
        paths, check_locks=ctx.check_locks, roots=roots, guards=guards, also_doomed=ctx.elsewhere
    )
    mapping = {
        "ok": "done",
        "missing": "skipped_missing",
        "denied": "skipped_denied",
        "locked": "skipped_locked",
        "protected": "skipped_protected",
        "no_trash": "skipped_trash_full",
        "ambiguous_name": "skipped_ambiguous_name",
        "changed": "skipped_changed",
        "keeper_changed": "skipped_keeper_changed",
        "last_copy": "skipped_keeper_lost",
        "keeper_busy": "skipped_keeper_busy",
        "cancelled": "cancelled",
        "exists": "skipped_name_taken",
        "error": "skipped_error",
    }
    for s, r in zip(todo, results, strict=True):
        code = mapping.get(r.code, "skipped_error")
        out.append(
            Outcome(
                int(s["seq"]),
                code,
                r.message if code != "done" else "",
                trash_ref=r.trash_ref,
                freed_later=int(s.get("alloc", 0)) if code == "done" else 0,
            )
        )
    return out


def _to_recovery(ctx: RunCtx, s: dict[str, Any]) -> Outcome:
    """「移到回收站」 on a volume without one: a no-replace rename into the recovery area."""
    fs = ctx.fs
    seq = int(s["seq"])
    fs.seq = seq
    folder, name = os.path.split(s["path"])
    H = _Handles(fs)
    try:
        dh = H.add(fs.open_dir(folder))
        ch = H.add(fs.open_entry(dh, name, purpose="copy"))
        cst = fs.fstat(ch)
        if cst.kind != "file" or not _same(cst, s):
            return Outcome(seq, "skipped_changed")
        place = _place(ctx, folder, fs.fstat(dh).dev)
        rfolder, rname = place.name_for(seq, name, ctx.run_id)
        rdh = dh if not rfolder else H.add(fs.open_dir(rfolder))
        if fs.swaps:
            fs.rename_at(dh, name, rdh, rname)
        else:
            fs.rename_h(ch, rdh, rname)
        rec = fs.stat_at(rdh, rname)
        if rec is None or rec.ident != cst.ident:
            return Outcome(seq, "failed_attention", "moved file not found", stop="stop_attention")
        rec_path = os.path.join(rfolder or folder, rname)
        return Outcome(seq, "done", rec=rec_path, rec_stamps=_stamps(rec), freed_later=int(s.get("alloc", 0)))
    except FsError as e:
        return Outcome(seq, _code_of(e, where="copy"), str(e), stop=_stop_for(e))
    finally:
        H.close()


# ------------------------------------------------------------------ the run
_OWN_CHANGES = ("change", "usn", "nlink")


def expect_own(ctx: RunCtx, s: dict[str, Any]) -> None:
    """A keeper this run already linked to is expected as that step left it: only what linking
    changes (change time, USN, link count) is taken from then; its identity, size and modification
    time are still the approved ones, so a change by anyone else is still caught."""
    for k in [*([s["target"]] if s.get("target") else []), *s.get("keepers", [])]:
        seen = ctx.seen.get(k["path"])
        if seen and (int(seen["dev"]), int(seen["fid_lo"]), int(seen["fid_hi"])) == _ident(k):
            for f in _OWN_CHANGES:
                if f in seen:
                    k[f] = int(seen[f])


def intent_of(ctx: RunCtx, s: dict[str, Any]) -> dict[str, Any]:
    rec = {
        "t": "intent",
        "seq": int(s["seq"]),
        "act": s["action"],
        "path": s["path"],
        "copy": {k: s[k] for k in ("dev", "fid_lo", "fid_hi", "size", "mtime", "change", "usn")},
        "tmp": tmp_name(ctx.run_id, int(s["seq"])) if s["action"] in ("hardlink", "symlink") else "",
        "text": s.get("text", ""),
    }
    if s.get("target"):
        rec["keeper"] = {
            k: s["target"][k] for k in ("path", "dev", "fid_lo", "fid_hi", "size", "mtime", "change", "usn")
        }
    return rec


def run(ctx: RunCtx, *, skip: dict[int, tuple[str, str]] | None = None) -> dict[str, Any]:
    """Every approved step not in ``skip``, in batches; the journal before and after each."""
    journal = ctx.journal
    assert journal is not None
    steps = [s for s in ctx.steps if int(s["seq"]) not in (skip or {})]
    for seq, (code, why) in (skip or {}).items():
        ctx.outcomes[seq] = Outcome(seq, code, why)
        ctx.counts[code] = ctx.counts.get(code, 0) + 1
        journal.write({"t": "skip", "seq": seq, "code": code, "why": why})
    total = len(ctx.steps)
    done = len(skip or {})
    last_code = ""
    same = 0
    i = 0
    try:
        journal.sync()
        while i < len(steps):
            while ctx.pause.is_set() and not ctx.cancel.is_set():
                time.sleep(0.05)
            if ctx.cancel.is_set():
                ctx.stop_reason = ctx.stop_reason or "stop_user"
                break
            # a batch is a run of trash steps on one volume, in their order (the recycle bin goes
            # per batch); a step is never taken out of turn, so none is skipped or done twice
            first = steps[i]
            batch = [first]
            if first["action"] == "trash":
                for x in steps[i + 1 : i + BATCH]:
                    if x["action"] != "trash" or x["vol"] != first["vol"]:
                        break
                    batch.append(x)
            i += len(batch)
            for s in batch:
                expect_own(ctx, s)
            if ctx.permit is not None and first["action"] != "clone":
                ctx.permit(first["path"])
            for s in batch:
                journal.write(intent_of(ctx, s))
            journal.sync()
            if first["action"] == "trash":
                outs = do_trash_batch(ctx, batch)
            elif first["action"] == "clone":
                outs = [do_clone(ctx, first)]
            else:
                outs = [do_replace(ctx, first)]
            pairs: list[tuple[dict[str, Any], Outcome]] = []
            by_seq = {int(s["seq"]): s for s in batch}
            for o in outs:
                ctx.outcomes[o.seq] = o
                ctx.counts[o.code] = ctx.counts.get(o.code, 0) + 1
                rec: dict[str, Any] = {
                    "t": "done" if o.code in ("done", "done_partial") else "fail",
                    "seq": o.seq,
                    "code": o.code,
                    "why": o.why,
                }
                if o.rec:
                    rec["rec"] = o.rec
                if o.rec_stamps:
                    rec["rec_stamps"] = o.rec_stamps
                if o.keeper_after:
                    rec["keeper_after"] = o.keeper_after
                if o.copy_after:
                    rec["copy_after"] = o.copy_after
                if o.trash_ref:
                    rec["trash_ref"] = o.trash_ref
                if o.shared:
                    rec["shared"] = o.shared
                rec["freed_now"] = o.freed_now
                rec["freed_later"] = o.freed_later
                ctx.fs.fault("before_done")
                journal.write(rec)
                tgt = by_seq[o.seq].get("target")
                if o.keeper_after and tgt:
                    ctx.seen[tgt["path"]] = dict(o.keeper_after)
                pairs.append((by_seq[o.seq], o))
                done += 1
                if o.code.startswith("failed") or o.code == "skipped_error":
                    same = same + 1 if o.code == last_code else 1
                    last_code = o.code
                else:
                    same, last_code = 0, ""
                if o.stop:
                    ctx.stop_reason = o.stop
                elif o.code in STOPPING:
                    ctx.stop_reason = "stop_attention"
                elif o.code == "failed_rolled_back" and ctx.options.get("on_failure") == "stop":
                    ctx.stop_reason = "stop_user"
                elif same >= SAME_FAILURES_STOP:
                    ctx.stop_reason = "stop_systemic"
            journal.sync()
            if ctx.on_batch is not None and pairs:
                ctx.on_batch(pairs)
            if ctx.progress is not None:
                ctx.progress("execute", done, total, first["path"], dict(ctx.counts))
            if ctx.stop_reason:
                break
    except JournalError:
        ctx.stop_reason = "stop_journal"
    for s in steps[i:]:
        seq = int(s["seq"])
        if seq not in ctx.outcomes:
            ctx.outcomes[seq] = Outcome(seq, "cancelled")
            ctx.counts["cancelled"] = ctx.counts.get("cancelled", 0) + 1
    state = "done"
    if ctx.stop_reason == "stop_user" and ctx.cancel.is_set():
        state = "stopped"
    elif ctx.stop_reason:
        state = "failed" if ctx.stop_reason in ("stop_attention", "stop_journal", "stop_disk") else "partial"
    elif any(not o.code.startswith("done") for o in ctx.outcomes.values()):
        state = "partial"
    with contextlib.suppress(JournalError):
        journal.write(
            {"t": "end", "state": state, "counts": dict(ctx.counts), "stop": ctx.stop_reason}, sync=True
        )
    manifest_items = [
        {
            "seq": o.seq,
            "path": next((s["path"] for s in ctx.steps if int(s["seq"]) == o.seq), ""),
            "recovery": o.rec,
        }
        for o in ctx.outcomes.values()
        if o.rec
    ]
    for place in ctx.places.values():
        if place.path:
            write_manifest(
                place.path, [x for x in manifest_items if str(x["recovery"]).startswith(place.path)]
            )
            remove_if_empty(place.path)
    report = {"run": ctx.run_id, "state": state, "counts": dict(ctx.counts), "stop": ctx.stop_reason}
    journal.write_report(report)
    return report


# ------------------------------------------------------------------ undo
def undo_plan(run_dir: Path, seqs: Sequence[int] | None = None) -> list[tuple[int, str, bool, str]]:
    """(seq, path, can, why) per done step (``seqs``: only these)."""
    plan = read_plan(run_dir)
    steps, _, _ = fold(read_records(run_dir))
    by_seq = {int(s["seq"]): s for s in plan.get("steps", [])}
    out: list[tuple[int, str, bool, str]] = []
    for seq, st in sorted(steps.items()):
        if seqs is not None and seq not in seqs:
            continue
        s = by_seq.get(seq)
        if s is None or st.result not in ("done", "done_partial") or st.undone:
            continue
        if s["action"] == "clone":
            out.append((seq, s["path"], False, TEXT["undo_nothing"]))
        elif st.purged:
            out.append((seq, s["path"], False, TEXT["purged"]))
        else:
            out.append((seq, s["path"], True, ""))
    return out


def undo(
    ctx: RunCtx, run_dir: Path, seqs: Sequence[int] | None = None, *, beside: bool = False
) -> dict[int, tuple[str, str]]:
    """Put done steps back, last first. Per seq: (code, text)."""
    from ...ops.trash import restore

    fs = ctx.fs
    plan = read_plan(run_dir)
    steps, _, _ = fold(read_records(run_dir))
    by_seq = {int(s["seq"]): s for s in plan.get("steps", [])}
    journal = Journal.open(run_dir)
    out: dict[int, tuple[str, str]] = {}
    folders: set[str] = set()
    try:
        for seq, st in sorted(steps.items(), reverse=True):
            if seqs is not None and seq not in seqs:
                continue
            s = by_seq.get(seq)
            if s is None or st.result not in ("done", "done_partial") or st.undone or st.purged:
                continue
            fs.seq = seq
            done = st.done or {}
            if done.get("rec"):
                folders.add(os.path.dirname(str(done["rec"])))
            try:
                if s["action"] == "clone":
                    out[seq] = ("undo_nothing", TEXT["undo_nothing"])
                    continue
                if s["action"] == "trash" and done.get("trash_ref"):
                    fs.guard(s["path"])
                    r = restore([done["trash_ref"]])[0]
                    code = "undone" if r.ok else ("undo_taken" if r.code == "exists" else "skipped_error")
                elif s["action"] == "trash":
                    code = _put_back(ctx, s, done, beside=beside)
                else:
                    code = _undo_replace(ctx, s, done, beside=beside)
            except FsError as e:
                code = "skipped_error"
                out[seq] = (code, text_of(code, str(e)))
                journal.write({"t": "undo", "seq": seq, "code": code, "why": str(e)}, sync=True)
                continue
            journal.write({"t": "undo", "seq": seq, "code": code}, sync=True)
            out[seq] = (code, text_of(code))
            if ctx.progress is not None:
                ctx.progress("undo", len(out), len(steps), s["path"], {})
    finally:
        journal.close()
    for f in folders:
        remove_if_empty(f)
    return out


def _beside_name(name: str) -> str:
    stem, ext = os.path.splitext(name)
    return f"{stem}{TEXT['original_suffix']}{ext}"


def _put_back(ctx: RunCtx, s: dict[str, Any], done: dict[str, Any], *, beside: bool) -> str:
    """An original in the recovery area back to its own name (or beside it), never replacing."""
    fs = ctx.fs
    rec = done.get("rec", "")
    if not rec:
        return "skipped_error"
    folder, name = os.path.split(s["path"])
    rfolder, rname = os.path.split(rec)
    H = _Handles(fs)
    try:
        dh = H.add(fs.open_dir(folder))
        rdh = H.add(fs.open_dir(rfolder))
        cur = fs.stat_at(rdh, rname)
        want = done.get("rec_stamps") or {}
        if cur is None or (want and cur.ident != (want["dev"], want["fid_lo"], want["fid_hi"])):
            return "undo_changed"
        target = name
        if fs.stat_at(dh, name) is not None:
            if not beside:
                return "undo_taken"
            target = _beside_name(name)
        if fs.swaps:
            fs.rename_at(rdh, rname, dh, target)
        else:
            h = H.add(fs.open_entry(rdh, rname, purpose="own"))
            fs.rename_h(h, dh, target)
        return "undone"
    finally:
        H.close()


def _undo_replace(ctx: RunCtx, s: dict[str, Any], done: dict[str, Any], *, beside: bool) -> str:
    fs = ctx.fs
    seq = int(s["seq"])
    folder, name = os.path.split(s["path"])
    rec = done.get("rec", "")
    if not rec:
        return "skipped_error"
    rfolder, rname = os.path.split(rec)
    want_rec = done.get("rec_stamps") or {}
    kafter = done.get("keeper_after") or {}
    H = _Handles(fs)
    T2 = f".sk-{ctx.run_id[-6:]}-{seq}.undo"
    try:
        dh = H.add(fs.open_dir(folder))
        rdh = H.add(fs.open_dir(rfolder)) if rfolder != folder else dh
        orig = fs.stat_at(rdh, rname)
        if orig is None or orig.ident != (want_rec["dev"], want_rec["fid_lo"], want_rec["fid_hi"]):
            return "undo_changed"
        cur = fs.stat_at(dh, name)
        ours = False
        if cur is not None:
            if s["action"] == "hardlink":
                k = s["target"]
                ours = cur.ident == (int(k["dev"]), int(k["fid_lo"]), int(k["fid_hi"])) and (
                    not kafter or (cur.size == kafter["size"] and cur.mtime == kafter["mtime"])
                )
            else:
                ours = cur.kind == "link" and fs.read_link(dh, name) == s["text"]
        if not ours:
            if cur is None or beside:
                target = name if cur is None else _beside_name(name)
                if fs.swaps:
                    fs.rename_at(rdh, rname, dh, target)
                else:
                    h = H.add(fs.open_entry(rdh, rname, purpose="own"))
                    fs.rename_h(h, dh, target)
                return "undone" if cur is None else "undo_beside"
            return "undo_changed"
        assert cur is not None
        if fs.swaps:
            fs.rename_at(rdh, rname, dh, T2)
            fs.exchange(dh, T2, name)
            back = fs.stat_at(dh, name)
            if back is None or back.ident != orig.ident:
                fs.exchange(dh, T2, name)  # as it was
                return "skipped_error"
            if not fs.remove_own(dh, T2, cur.ident):
                return "skipped_error"
        else:
            sh = H.add(fs.open_entry(dh, name, purpose="own"))
            if fs.fstat(sh).ident != cur.ident:
                return "skipped_error"
            fs.rename_h(sh, dh, T2)
            try:
                oh = H.add(fs.open_entry(rdh, rname, purpose="own"))
                fs.rename_h(oh, dh, name)
            except FsError:
                fs.rename_h(sh, dh, name)  # the stand-in back
                raise
            if not fs.remove_own(dh, T2, cur.ident):
                return "skipped_error"
        return "undone"
    finally:
        H.close()


# ------------------------------------------------------------------ purge
def purge_plan(
    run_dir: Path, seqs: Sequence[int] | None = None, *, ops_kind: str = "native"
) -> list[tuple[int, str, bool, str, int, bool]]:
    """(seq, recovery path, can, why, bytes, has extra streams) per original in the recovery area."""
    from ..reads import FallbackOps, default_ops
    from .facts import probe

    plan = read_plan(run_dir)
    steps, _, _ = fold(read_records(run_dir))
    by_seq = {int(s["seq"]): s for s in plan.get("steps", [])}
    ops = default_ops() if ops_kind == "native" else FallbackOps()
    out: list[tuple[int, str, bool, str, int, bool]] = []
    for seq, st in sorted(steps.items()):
        if seqs is not None and seq not in seqs:
            continue
        s = by_seq.get(seq)
        done = st.done or {}
        rec = done.get("rec", "")
        if s is None or not rec or st.undone or st.purged or st.result not in ("done", "done_partial"):
            continue
        want = done.get("rec_stamps") or {}
        now = ops.identity([rec], True)
        row = {k: int(v[0]) for k, v in now.items()}
        size = int(s.get("alloc", 0))
        if (
            row["reason"] != 0
            or (row["dev"], row["fid_lo"], row["fid_hi"])
            != (want.get("dev"), want.get("fid_lo"), want.get("fid_hi"))
            or row["size"] != want.get("size")
            or row["mtime"] != want.get("mtime")
            or (want.get("change") and row["change"] != want.get("change"))
        ):
            out.append((seq, rec, False, TEXT["purge_original_changed"], size, False))
            continue
        why = _keeper_ok(s, done, ops)
        if why:
            out.append((seq, rec, False, why, size, False))
            continue
        p = probe(rec)
        out.append((seq, rec, True, "", size, bool(p.ok and p.streams_bytes > 0)))
    return out


def _keeper_ok(s: dict[str, Any], done: dict[str, Any], ops: Any) -> str:
    """The content is still elsewhere: the keeper as the run left it (for a hard link, the name in
    place is the keeper)."""
    k = s.get("target") or (s["keepers"][0] if s["keepers"] else None)
    if k is None:
        return TEXT["purge_keeper_changed"]
    after = done.get("keeper_after") or k
    now = ops.identity([k["path"]], True)
    row = {kk: int(v[0]) for kk, v in now.items()}
    if row["reason"] != 0 or (row["dev"], row["fid_lo"], row["fid_hi"]) != (
        int(after["dev"]),
        int(after["fid_lo"]),
        int(after["fid_hi"]),
    ):
        return TEXT["purge_keeper_changed"]
    if row["size"] != int(after["size"]) or row["mtime"] != int(after["mtime"]):
        return TEXT["purge_keeper_changed"]
    return ""


def purge(
    ctx: RunCtx,
    run_dir: Path,
    seqs: Sequence[int] | None = None,
    *,
    verify: str = "auto",
    ack_streams: bool = False,
) -> dict[int, tuple[str, str]]:
    """Delete originals in the recovery area for good, each one checked right before (I14)."""
    from ..guard import Stamped, _digests

    fs = ctx.fs
    plan = read_plan(run_dir)
    by_seq = {int(s["seq"]): s for s in plan.get("steps", [])}
    items = purge_plan(run_dir, seqs, ops_kind=ctx.ops_kind)
    steps, _, _ = fold(read_records(run_dir))
    journal = Journal.open(run_dir)
    out: dict[int, tuple[str, str]] = {}
    folders: set[str] = set()
    try:
        for seq, rec, can, why, _size, streams in items:
            if ctx.cancel.is_set():
                break
            if not can:
                out[seq] = ("refused", why)
                continue
            if streams and not ack_streams:
                out[seq] = ("refused", TEXT["purge_streams"])
                continue
            s = by_seq[seq]
            done = steps[seq].done or {}
            fs.seq = seq
            k = s.get("target") or s["keepers"][0]
            after = done.get("keeper_after") or k
            need_read = verify == "content" or (
                verify == "auto" and sys.platform == "win32" and not int(after.get("usn", 0))
            )
            if need_read:
                want = done.get("rec_stamps") or {}
                files = [
                    Stamped(
                        rec,
                        int(want["dev"]),
                        int(want["fid_lo"]),
                        int(want["fid_hi"]),
                        int(want["size"]),
                        int(want["mtime"]),
                        int(want["change"]),
                    ),
                    Stamped(
                        k["path"],
                        int(after["dev"]),
                        int(after["fid_lo"]),
                        int(after["fid_hi"]),
                        int(after["size"]),
                        int(after["mtime"]),
                        int(after.get("change", 0)),
                    ),
                ]
                got = _digests(ctx.ops_kind, files, ctx.keep_atime)
                if got[0] is None or got[0] != got[1] or got[0].hex() != s["digest"]:
                    out[seq] = ("refused", TEXT["purge_keeper_changed"])
                    continue
            rfolder, rname = os.path.split(rec)
            H = _Handles(fs)
            try:
                rdh = H.add(fs.open_dir(rfolder))
                cur = fs.stat_at(rdh, rname)
                want = done.get("rec_stamps") or {}
                if (
                    cur is None
                    or cur.ident != (want["dev"], want["fid_lo"], want["fid_hi"])
                    or cur.mtime != want["mtime"]
                    or cur.size != want["size"]
                ):
                    out[seq] = ("refused", TEXT["purge_original_changed"])
                    continue
                fs.fault("purge")
                if not fs.remove_own(rdh, rname, cur.ident):
                    out[seq] = ("refused", TEXT["purge_original_changed"])
                    continue
            except FsError as e:
                out[seq] = ("refused", str(e))
                continue
            finally:
                H.close()
            journal.write({"t": "purged", "seq": seq}, sync=True)
            out[seq] = ("purged", TEXT["purged"])
            folders.add(rfolder)
            if ctx.progress is not None:
                ctx.progress("purge", len(out), len(items), rec, {})
    finally:
        journal.close()
    for f in folders:
        remove_if_empty(f)
    return out


# ------------------------------------------------------------------ interrupted runs
def interrupted(run_dir: Path) -> bool:
    _, run, end = fold(read_records(run_dir))
    return run is not None and end is None


def recover(ctx: RunCtx, run_dir: Path) -> dict[str, int]:
    """Finish an interrupted run from its journal and what is on disk (docs/dupes.md §16.10.4).
    Idempotent: a crash while recovering is recovered the same way next time."""
    fs = ctx.fs
    plan = read_plan(run_dir)
    by_seq = {int(s["seq"]): s for s in plan.get("steps", [])}
    steps, _, end = fold(read_records(run_dir))
    counts = {"restored": 0, "done": 0, "attention": 0, "cancelled": 0, "at": 0}
    if end is not None:
        return counts
    journal = Journal.open(run_dir)
    try:
        for seq, st in sorted(steps.items()):
            if st.intent is None or st.result:
                continue
            counts["at"] = max(counts["at"], seq)
            s = by_seq.get(seq)
            if s is None:
                continue
            fs.seq = seq
            try:
                code, why, extra = _settle(ctx, s, st)
            except FsError as e:
                code, why, extra = "failed_attention", str(e), {}
            rec: dict[str, Any] = {
                "t": "done" if code.startswith("done") else "fail",
                "seq": seq,
                "code": code,
                "why": why,
                **extra,
            }
            journal.write(rec, sync=True)
            if code.startswith("done"):
                counts["done"] += 1
            elif code == "cancelled":
                counts["cancelled"] += 1
            elif code == "failed_attention":
                counts["attention"] += 1
            else:
                counts["restored"] += 1
        journal.write({"t": "end", "state": "recovered", "counts": counts}, sync=True)
    finally:
        journal.close()
    return counts


def _settle(ctx: RunCtx, s: dict[str, Any], st: StepState) -> tuple[str, str, dict[str, Any]]:
    fs = ctx.fs
    act = s["action"]
    folder, name = os.path.split(s["path"])
    copy_id = (int(s["dev"]), int(s["fid_lo"]), int(s["fid_hi"]))
    if act == "clone":
        H = _Handles(fs)
        try:
            dh = H.add(fs.open_dir(folder))
            cur = fs.stat_at(dh, name)
            if cur is None or cur.ident != copy_id or cur.size != int(s["size"]):
                return "failed_attention", "the copy is not as it was", {}
            eq = _extents_equal([s["target"]["path"], s["path"]])
            return ("done" if eq else "done_partial"), "interrupted", {}
        finally:
            H.close()
    if act == "trash":
        H = _Handles(fs)
        try:
            dh = H.add(fs.open_dir(folder))
            cur = fs.stat_at(dh, name)
        finally:
            H.close()
        if cur is not None and cur.ident == copy_id:
            return "cancelled", "", {}
        # moved: to the recycle bin (found by its info record) or to the recovery area
        ref = _find_trash_ref(s)
        if ref:
            return "done", "interrupted", {"trash_ref": ref}
        return "failed_attention", "not at its path, not found in the recycle bin", {}
    # replacements
    T = tmp_name(ctx.run_id, int(s["seq"]))
    k = s.get("target") or {}
    k_id = (int(k.get("dev", 0)), int(k.get("fid_lo", 0)), int(k.get("fid_hi", 0)))
    H = _Handles(fs)
    try:
        dh = H.add(fs.open_dir(folder))
        at_p = fs.stat_at(dh, name)
        at_t = fs.stat_at(dh, T)
        place = _place(ctx, folder, fs.fstat(dh).dev)
        rfolder, rname = place.name_for(int(s["seq"]), name, ctx.run_id)
        rdh = dh if not rfolder else H.add(fs.open_dir(rfolder))
        at_r = fs.stat_at(rdh, rname)
        rec_path = os.path.join(rfolder or folder, rname)
        made_id = tuple(st.made["id"]) if st.made else None

        def is_standin(x: St | None, where: str) -> bool:
            if x is None:
                return False
            if act == "hardlink":
                return x.ident == k_id
            return (
                x.kind == "link"
                and fs.read_link(dh, where) == s["text"]
                and (made_id is None or x.ident == made_id)
            )

        if at_p is not None and at_p.ident == copy_id:
            if at_t is None and at_r is None:
                return "cancelled", "", {}
            if is_standin(at_t, T) and at_r is None:
                fs.remove_own(dh, T, at_t.ident if at_t else None)
                return "failed_rolled_back", "interrupted", {}
            if at_t is not None and not is_standin(at_t, T):
                # something we do not know under our temporary name: move it aside, never delete
                qdir = os.path.join(place.path or folder, "quarantine") if place.path else folder
                with contextlib.suppress(FsError, OSError):
                    os.makedirs(qdir, exist_ok=True)
                    qdh = H.add(fs.open_dir(qdir))
                    fs.rename_at(dh, T, qdh, f"{s['seq']}-unknown") if fs.swaps else None
                return "failed_attention", "unknown file under the temporary name", {}
        if is_standin(at_p, name):
            if at_t is not None and at_t.ident == copy_id and fs.swaps:
                # swapped, the original not moved yet
                fs.rename_at(dh, T, rdh, rname)
                moved = fs.stat_at(rdh, rname)
                if moved is not None and moved.ident == copy_id:
                    return "done", "interrupted", {"rec": rec_path, "rec_stamps": _stamps(moved)}
                return "failed_attention", "the original could not be moved", {}
            if at_r is not None and at_r.ident == copy_id:
                return "done", "interrupted", {"rec": rec_path, "rec_stamps": _stamps(at_r)}
        if at_p is None and at_r is not None and at_r.ident == copy_id and not fs.swaps:
            # Windows: between the two renames: the original back, the stand-in away
            h = H.add(fs.open_entry(rdh, rname, purpose="own"))
            fs.rename_h(h, dh, name)
            if at_t is not None and is_standin(at_t, T):
                fs.remove_own(dh, T, at_t.ident)
            return "failed_rolled_back", "interrupted", {}
        where = "missing" if at_p is None else ("standin" if is_standin(at_p, name) else "other")
        return (
            "failed_attention",
            TEXT["attention_state"].format(
                rec=rec_path if at_r is not None else (os.path.join(folder, T) if at_t is not None else "?"),
                path=s["path"],
                what=TEXT[f"what_{where}"],
            ),
            {"rec": rec_path} if at_r is not None else {},
        )
    finally:
        H.close()


def _find_trash_ref(s: dict[str, Any]) -> str:
    """The recycle bin's record of a path moved there (freedesktop: a .trashinfo naming it)."""
    if sys.platform == "win32":  # pragma: no cover - Windows
        return ""
    from ...ops.trash import _fd_find, parse_trashinfo

    try:
        t = _fd_find(os.path.dirname(s["path"]), create=False)
    except OSError:
        return ""
    if isinstance(t, str):
        return ""
    info_dir = os.path.join(t.root, "info")
    want = s["path"]
    rel = os.path.relpath(want, t.topdir) if t.topdir else want
    with contextlib.suppress(OSError, ValueError):
        for n in os.listdir(info_dir):
            if not n.endswith(".trashinfo"):
                continue
            p = os.path.join(info_dir, n)
            with open(p, encoding="utf-8", errors="replace") as f:
                if parse_trashinfo(f.read()) in (want, rel):
                    return p
    return ""


def resolve(ctx: RunCtx, run_dir: Path, seq: int, choice: str) -> str:
    """An item that needs the user: put the original back (when its name is free), or keep things."""
    plan = read_plan(run_dir)
    s = next((x for x in plan.get("steps", []) if int(x["seq"]) == seq), None)
    steps, _, _ = fold(read_records(run_dir))
    st = steps.get(seq)
    if s is None or st is None:
        return "skipped_error"
    journal = Journal.open(run_dir)
    try:
        if choice == "put_back":
            rec_path = (st.done or {}).get("rec") or ""
            if not rec_path:
                for r in read_records(run_dir):
                    if int(r.get("seq", 0)) == seq and r.get("rec"):
                        rec_path = r["rec"]
            if not rec_path:
                return "skipped_error"
            code = _put_back(ctx, s, {"rec": rec_path}, beside=False)
            if code != "undone":
                return code
        journal.write({"t": "resolved", "seq": seq, "choice": choice}, sync=True)
        return "resolved"
    finally:
        journal.close()
