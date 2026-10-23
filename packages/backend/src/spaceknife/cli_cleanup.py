"""``spaceknife dupes-cleanup``: the duplicate-file cleanup plan from the command line, for tests and
verification only (docs/dupes.md §16.18.6).

It scans and searches like ``spaceknife dupes``, builds the cleanup plan with a preset (and
optionally one action for every copy not kept), runs the risk check and prints the plan: per action,
per volume, the risk kinds. ``--execute`` runs it: it skips the interface's review, so it needs
``--approve`` (every high risk acknowledged as shown) and ``--only-under`` (the sandbox: nothing
outside those folders can be touched, I13). ``--undo``, ``--purge`` and ``--recover`` work on a run
of the journal, inside the sandbox too. This is not the everyday way: that is the cleanup mode.
"""

from __future__ import annotations

import asyncio
import json
import os
import sys
import time
from typing import Annotated, Any

import typer
from rich.console import Console
from rich.table import Table

from . import fmt
from .i18n import tr

console = Console(stderr=True)


def dupes_cleanup(
    paths: Annotated[list[str], typer.Argument(help="Folders to scan and search.")] = [],  # noqa: B006
    only_under: Annotated[
        list[str] | None,
        typer.Option(
            "--only-under", help="The sandbox: nothing outside is touched (required to change files)."
        ),
    ] = None,
    cross: Annotated[bool, typer.Option("--cross", help="Compare across all given paths.")] = False,
    min_size: Annotated[str, typer.Option("--min-size", help="Smallest file compared.")] = "64K",
    preset: Annotated[
        str, typer.Option(help="watch, conservative, recommended, links, backup.")
    ] = "recommended",
    action: Annotated[
        str | None,
        typer.Option(help="One action for every copy not kept: leave, trash, hardlink, symlink, clone."),
    ] = None,
    verify: Annotated[str, typer.Option(help="auto, content, metadata.")] = "auto",
    approve: Annotated[
        bool, typer.Option("--approve", help="Acknowledge every high risk and approve.")
    ] = False,
    execute: Annotated[
        bool, typer.Option("--execute", help="Run the approved plan (needs --approve, --only-under).")
    ] = False,
    undo: Annotated[str | None, typer.Option("--undo", help="Undo this run.")] = None,
    purge: Annotated[str | None, typer.Option("--purge", help="Empty this run's recovery area.")] = None,
    recover: Annotated[bool, typer.Option("--recover", help="Finish interrupted runs.")] = False,
    as_json: Annotated[bool, typer.Option("--json", help="Print one JSON object.")] = False,
) -> None:
    """The duplicate-file cleanup plan, for tests and verification (changes files only with --execute)."""
    roots = [os.path.abspath(r) for r in (only_under or [])]
    if (execute or undo or purge or recover) and not roots:
        raise typer.BadParameter("--only-under is required to change files")
    if execute and not approve:
        raise typer.BadParameter("--execute needs --approve")
    if roots:
        os.environ["SPACEKNIFE_CLEANUP_SANDBOX"] = os.pathsep.join(roots)
    out: dict[str, Any] = {}
    t0 = time.monotonic()
    if undo or purge or recover:
        out.update(_history(undo=undo, purge=purge, recover=recover))
    elif paths:
        out.update(
            asyncio.run(_plan_and_run(paths, cross, min_size, preset, action, verify, approve, execute))
        )
    out["elapsed_s"] = round(time.monotonic() - t0, 3)
    if as_json:
        sys.stdout.write(json.dumps(out, ensure_ascii=False, indent=1, default=str) + "\n")
        return
    t = Table(title=tr("重复文件清理"), show_header=False)
    for k, v in out.items():
        t.add_row(str(k), json.dumps(v, ensure_ascii=False, default=str)[:400])
    console.print(t)


def _ctx(run_id: str, plan: dict[str, Any]) -> Any:
    from .dupes.cleanup import execute as X
    from .dupes.cleanup.fsops import faults_from_env, make_fs, sandbox_from_env
    from .paths import data_dir

    return X.RunCtx(
        run_id=run_id,
        steps=list(plan.get("steps", [])),
        fs=make_fs(sandbox_from_env(), faults_from_env()),
        options=dict(plan.get("options", {})),
        data_dir=data_dir(),
        ops_kind=str(plan.get("ops", "native")),
        at_root=False,
        check_locks=True,
        lockers=_lockers,
    )


def _lockers(paths: list[str]) -> dict[str, list[str]]:
    from .ops.locks import find_lockers

    return find_lockers(paths)


def _history(*, undo: str | None, purge: str | None, recover: bool) -> dict[str, Any]:
    from .dupes.cleanup import execute as X
    from .dupes.cleanup.journal import read_plan, run_dirs, runs_root

    out: dict[str, Any] = {}
    if recover:
        done = {}
        for d in run_dirs():
            if X.interrupted(d):
                done[d.name] = X.recover(_ctx(d.name, read_plan(d)), d)
        out["recovered"] = done
    for run_id, kind in ((undo, "undo"), (purge, "purge")):
        if not run_id:
            continue
        d = runs_root() / run_id
        ctx = _ctx(run_id, read_plan(d))
        res = X.undo(ctx, d) if kind == "undo" else X.purge(ctx, d, verify="auto", ack_streams=True)
        counts: dict[str, int] = {}
        for code, _ in res.values():
            counts[code] = counts.get(code, 0) + 1
        out[kind] = {"run": run_id, "counts": counts, "items": {str(k): v for k, v in res.items()}}
    return out


async def _plan_and_run(
    paths: list[str],
    cross: bool,
    min_size: str,
    preset: str,
    action: str | None,
    verify: str,
    approve: bool,
    execute: bool,
) -> dict[str, Any]:
    from spaceknife_protocol import dupes_cleanup as pc

    from .cli_dupes import _parse_size, _scan
    from .dupes import Job, JobOptions, KeepOptions, Results, Scope
    from .dupes.cleanup import edits as E
    from .dupes.cleanup import execute as X
    from .dupes.cleanup import views as V
    from .dupes.cleanup.caps import volume_caps
    from .dupes.cleanup.facts import TagIndex, probe_many, system_facts
    from .dupes.cleanup.journal import Journal, new_run_id
    from .dupes.cleanup.plan import CleanupPlan, policy_from_keep
    from .dupes.cleanup.policy import preset as make_preset
    from .io.registry import IoRegistry
    from .ops.trash import trash_status

    scans = await _scan(paths, 0)
    registry = IoRegistry(max_workers=8)
    await registry.start()
    try:
        scope = Scope(scans if cross else scans[:1], min_size=_parse_size(min_size))
        res = Results(scope.scans, KeepOptions())
        job = Job(scope, options=JobOptions(load="max"), registry=registry, sink=res)
        await job.run()
    finally:
        await registry.stop()
    sysf = system_facts(native=job.ops.name == "native")
    vols = [volume_caps(v, sys_facts=sysf, trash_of=lambda m: trash_status(m, usage=False)) for v in job.vols]
    policy = make_preset(preset, policy_from_keep(res.keep))
    policy.options.verify = verify  # type: ignore[assignment]
    plan = CleanupPlan(
        "cli",
        res,
        [s.scan_id for s in scope.scans],
        policy,
        vols=vols,
        sys_facts=sysf,
        tags=TagIndex([s.store for s in scope.scans]),
    )
    todo = plan.probe_order()
    for i in range(0, len(todo), 256):
        chunk = todo[i : i + 256]
        for r, p in zip(chunk, probe_many([plan.paths[r] for r in chunk]), strict=True):
            plan.probes[r] = p
    plan.recompute()
    if action:
        E.edit(plan, pc.CleanupEdit(op="action_set", action=action, scope=pc.CleanupScope(filter="")))  # type: ignore[arg-type]
    if approve:
        for rid in list(plan.unacked()):
            plan.ack(rid)
    summary = V.summary(plan, plan.gates()).model_dump()
    out: dict[str, Any] = {
        "groups": len(plan.groups),
        "by_action": summary["by_action"],
        "by_volume": summary["by_volume"],
        "risks": [{k: b[k] for k in ("id", "code", "severity", "copies")} for b in summary["buckets"]],
        "gates": summary["gates"],
        "volumes": [
            {
                "name": v.name,
                "fs": v.fs,
                "hardlink": v.hardlink,
                "symlink": v.symlink,
                "clone": v.clone,
                "exchange": v.exchange,
                "trash": v.trash,
            }
            for v in vols
        ],
        "freed": {"now": fmt.size(summary["freed_now"]), "later": fmt.size(summary["freed_later"])},
    }
    if not approve:
        return out
    appr = plan.approve()
    out["approval"] = {"token": appr.token, "steps": len(appr.steps)}
    if not execute:
        return out
    run_id = new_run_id()
    steps = [s.to_json() for s in appr.steps]
    doc = {"run": run_id, "steps": steps, "options": appr.options, "ops": job.ops.name, "roots": paths}
    ctx = _ctx(run_id, doc)
    ctx.journal = Journal.create(run_id, doc)
    ctx.journal.write({"t": "run", "run": run_id, "os": sys.platform}, sync=True)
    skip = X.preflight(ctx, steps)
    report = X.run(ctx, skip=skip)
    ctx.journal.close()
    out["run"] = {
        "id": run_id,
        "state": report["state"],
        "counts": report["counts"],
        "stop": report.get("stop", ""),
        "items": [
            {
                "seq": o.seq,
                "path": next(s["path"] for s in steps if int(s["seq"]) == o.seq),
                "action": next(s["action"] for s in steps if int(s["seq"]) == o.seq),
                "code": o.code,
                "why": o.why,
                "rec": o.rec,
                "shared": o.shared,
            }
            for o in sorted(ctx.outcomes.values(), key=lambda o: o.seq)
        ],
    }
    return out


__all__ = ["dupes_cleanup"]
