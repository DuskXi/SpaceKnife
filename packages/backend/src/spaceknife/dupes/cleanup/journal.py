"""The journal of a cleanup run (docs/dupes.md §16.10.1–16.10.2).

``data_dir()/dupes/cleanup/runs/<run>/``:

- ``plan.json``: the frozen steps, acknowledgements and options, written (and fsynced) before the
  first step;
- ``journal.jsonl``: append-only, one JSON object per line, each with ``"c"``: the first 8 bytes of
  the BLAKE2b of the line without it (hex). A line that does not check (a crash while writing it) is
  ignored. A batch's ``intent`` records are fsynced before its first step: every name the run may
  make (temporary names, names in the recovery area) is on disk before it exists. ``step``, ``done``,
  ``fail`` are written as they happen and fsynced once per batch. Recovery never trusts them alone:
  it looks at what is on disk;
- ``report.json``: counts and every step's result, at the end (rebuilt from the journal if missing).

Paths stay on this machine; logs name only the run id and counts.
"""

from __future__ import annotations

import contextlib
import hashlib
import json
import os
import secrets
import time
from collections.abc import Iterator
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path
from typing import Any


def runs_root() -> Path:
    from ...paths import data_dir

    return data_dir() / "dupes" / "cleanup" / "runs"


def plans_root() -> Path:
    from ...paths import data_dir

    return data_dir() / "dupes" / "cleanup" / "plans"


def new_run_id() -> str:
    return f"r-{datetime.now().strftime('%Y%m%d-%H%M%S')}-{secrets.token_hex(2)}"


def _check(obj: dict[str, Any]) -> str:
    body = json.dumps(obj, sort_keys=True, ensure_ascii=False, separators=(",", ":"))
    return hashlib.blake2b(body.encode("utf-8", "surrogatepass"), digest_size=8).hexdigest()


def _fsync_dir(path: Path) -> None:
    if os.name == "nt":
        return
    with contextlib.suppress(OSError):
        fd = os.open(str(path), os.O_RDONLY)
        try:
            os.fsync(fd)
        finally:
            os.close(fd)


class JournalError(OSError):
    pass


@dataclass(slots=True)
class Journal:
    dir: Path
    _f: Any = None
    _dirty: bool = False
    fault: Any = None  # tests: a callable(phase) that may raise

    @staticmethod
    def create(run_id: str, plan: dict[str, Any], root: Path | None = None) -> Journal:
        d = (root or runs_root()) / run_id
        try:
            d.mkdir(parents=True, exist_ok=False)
            _fsync_dir(d.parent)
            tmp = d / "plan.json.tmp"
            with open(tmp, "w", encoding="utf-8", errors="surrogatepass") as f:
                json.dump(plan, f, ensure_ascii=False)
                f.flush()
                os.fsync(f.fileno())
            os.replace(tmp, d / "plan.json")
            _fsync_dir(d)
        except OSError as exc:
            raise JournalError(exc.errno or 0, str(exc)) from exc
        j = Journal(d)
        j._open()
        return j

    @staticmethod
    def open(run_dir: Path) -> Journal:
        j = Journal(run_dir)
        j._open()
        return j

    def _open(self) -> None:
        try:
            self._f = open(self.dir / "journal.jsonl", "a", encoding="utf-8", errors="surrogatepass")  # noqa: SIM115
        except OSError as exc:
            raise JournalError(exc.errno or 0, str(exc)) from exc

    def write(self, rec: dict[str, Any], *, sync: bool = False) -> None:
        if self.fault is not None:
            self.fault("journal")
        rec = dict(rec)
        rec.setdefault("at", round(time.time(), 3))
        rec["c"] = _check(rec)
        try:
            self._f.write(json.dumps(rec, sort_keys=True, ensure_ascii=False, separators=(",", ":")) + "\n")
            self._dirty = True
            if sync:
                self.sync()
        except OSError as exc:
            raise JournalError(exc.errno or 0, str(exc)) from exc

    def sync(self) -> None:
        if self._f is None or not self._dirty:
            return
        try:
            self._f.flush()
            os.fsync(self._f.fileno())
            self._dirty = False
        except OSError as exc:
            raise JournalError(exc.errno or 0, str(exc)) from exc

    def close(self) -> None:
        if self._f is not None:
            with contextlib.suppress(OSError):
                self.sync()
            with contextlib.suppress(OSError):
                self._f.close()
            self._f = None

    def write_report(self, report: dict[str, Any]) -> None:
        tmp = self.dir / "report.json.tmp"
        with contextlib.suppress(OSError):
            with open(tmp, "w", encoding="utf-8", errors="surrogatepass") as f:
                json.dump(report, f, ensure_ascii=False)
                f.flush()
                os.fsync(f.fileno())
            os.replace(tmp, self.dir / "report.json")


def read_records(run_dir: Path) -> list[dict[str, Any]]:
    """The journal's records that check out (a line half written is dropped)."""
    out: list[dict[str, Any]] = []
    try:
        with open(run_dir / "journal.jsonl", encoding="utf-8", errors="surrogatepass") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    rec = json.loads(line)
                except ValueError:
                    continue
                c = rec.pop("c", None)
                if not isinstance(rec, dict) or c != _check(rec):
                    continue
                out.append(rec)
    except OSError:
        pass
    return out


def read_plan(run_dir: Path) -> dict[str, Any]:
    try:
        with open(run_dir / "plan.json", encoding="utf-8", errors="surrogatepass") as f:
            data: dict[str, Any] = json.load(f)
            return data
    except (OSError, ValueError):
        return {}


def run_dirs(root: Path | None = None) -> Iterator[Path]:
    base = root or runs_root()
    with contextlib.suppress(OSError):
        for d in sorted(base.iterdir(), reverse=True):
            if d.is_dir() and d.name.startswith("r-"):
                yield d


@dataclass(slots=True)
class StepState:
    """What the journal says of one step (the last word wins)."""

    seq: int
    intent: dict[str, Any] | None = None
    phases: list[str] = field(default_factory=list)
    made: dict[str, Any] | None = None
    result: str = ""  # done / done_partial / skipped_* / failed_* / cancelled
    message: str = ""
    done: dict[str, Any] | None = None
    undone: bool = False
    undo_code: str = ""  # the last undo that did not happen: why (undo_taken, undo_changed ...)
    purged: bool = False
    resolved: str = ""


def fold(
    records: list[dict[str, Any]],
) -> tuple[dict[int, StepState], dict[str, Any] | None, dict[str, Any] | None]:
    """(per step, the run record, the end record)."""
    steps: dict[int, StepState] = {}
    run: dict[str, Any] | None = None
    end: dict[str, Any] | None = None
    for rec in records:
        t = rec.get("t")
        if t == "run":
            run = rec
            continue
        if t == "end":
            end = rec
            continue
        seq = int(rec.get("seq", 0))
        if not seq:
            continue
        st = steps.setdefault(seq, StepState(seq))
        if t == "intent":
            st.intent = rec
        elif t == "step":
            st.phases.append(str(rec.get("p", "")))
            if rec.get("p") == "made":
                st.made = rec
        elif t == "done":
            st.result = str(rec.get("code", "done"))
            st.done = rec
        elif t in ("fail", "skip"):
            st.result = str(rec.get("code", "failed_rolled_back"))
            st.message = str(rec.get("why", ""))
        elif t == "undo":
            st.undone = rec.get("code") == "undone"
            st.undo_code = "" if st.undone else str(rec.get("code", ""))
            if not st.undone:
                st.message = str(rec.get("why", ""))
        elif t == "purged":
            st.purged = True
        elif t == "resolved":
            st.resolved = str(rec.get("choice", ""))
            if rec.get("choice") == "put_back":
                st.result = "failed_rolled_back"
    return steps, run, end
