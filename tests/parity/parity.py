# /// script
# requires-python = ">=3.12"
# dependencies = ["websockets>=13"]
# ///
"""SpaceKnife protocol parity gate (tests/parity/README.md).

    uv run tests/parity/parity.py run    --ref <v0.3.4 worktree> --cand <spaceknife-cli> [scenario ...]
    uv run tests/parity/parity.py record --ref <v0.3.4 worktree> [scenario ...]
    uv run tests/parity/parity.py check  --cand <spaceknife-cli> [scenario ...]
    uv run tests/parity/parity.py coverage
    uv run tests/parity/parity.py gen-invalid
    uv run tests/parity/parity.py listing     # golden/<platform>/listing.json (record writes it too)

`run` drives both backends and compares their normalized transcripts; `record` stores the
reference's transcript as golden/<platform>/<scenario>.json; `check` compares the candidate with
those goldens (what crates/sk-server/tests/parity_replay.rs does without Python).
"""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import shutil
import sys
from pathlib import Path
from typing import Any

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

from skparity import engine, norm  # noqa: E402

SCENARIOS = HERE / "scenarios"
GOLDEN = HERE / "golden" / sys.platform
SCHEMA = HERE.parent.parent / "crates" / "sk-protocol" / "generated" / "protocol.schema.json"


def load(name: str) -> dict[str, Any]:
    return json.loads((SCENARIOS / f"{name}.json").read_text(encoding="utf-8"))


def names(selected: list[str]) -> list[str]:
    allnames = sorted(p.stem for p in SCENARIOS.glob("*.json"))
    return selected or allnames


def runs_here(sc: dict[str, Any]) -> bool:
    return sys.platform in sc.get("platforms", ["win32", "linux", "darwin"])


def known_list(sc: dict[str, Any]) -> list[dict[str, Any]]:
    glob = json.loads((HERE / "known_diffs.json").read_text(encoding="utf-8"))["known"]
    return [*glob, *sc.get("known_diffs", [])]


def doc(transcript: list[dict[str, Any]], sc: dict[str, Any]) -> dict[str, Any]:
    return {"format": 1, "scenario": sc["name"], "platform": sys.platform, "steps": transcript}


# -------------------------------------------------------------------- directory order

LISTING = GOLDEN / "listing.json"


def write_listing() -> Path:
    """golden/<platform>/listing.json: how the file system of this machine's temp folder (where
    `record` puts the runs' folders) lists every fixture tree's folders. Its `filesystem` (what CI
    rebuilds: an ext4's hash seed) is kept; after recording on another file system, set it anew
    (README.md, "Directory order")."""
    old = json.loads(LISTING.read_text(encoding="utf-8")) if LISTING.exists() else {}
    trees = [k for k, v in engine.fixtures.specs().items() if isinstance(v, dict)]
    out = {
        "comment": old.get("comment", "How the file system the goldens were recorded on listed each fixture tree's folders (parity.py listing): 0.3.4 keeps that order for entries of equal size (README.md, 'Directory order')."),
        "filesystem": old.get("filesystem", {"type": "unknown"}),
        "trees": engine.fixtures.listings(trees, None),
    }
    LISTING.write_text(json.dumps(out, indent=1, ensure_ascii=False) + "\n", encoding="utf-8", newline="\n")
    return LISTING


def other_order(sc: dict[str, Any], parent: str | None) -> str | None:
    """The first fixture folder the file system of `parent` (None: the system's temp folder) lists
    otherwise than the recording, or None."""
    if not LISTING.exists():
        return None
    recorded = json.loads(LISTING.read_text(encoding="utf-8"))["trees"]
    trees = sorted(set(sc.get("fixtures", {}).values()))
    for tree, actual in engine.fixtures.listings(trees, parent).items():
        if (folder := engine.fixtures.listing_difference(recorded.get(tree), actual)) is not None:
            return f"{tree}: {folder}"
    return None


# -------------------------------------------------------------------- the upgrade scenario


def root_key_034(roots: list[str]) -> str:
    """0.3.4's `snapshot.root_key` (tags, palette jumps and snapshot folders are filed by it)."""
    import hashlib

    normed = "\n".join(sorted(os.path.normcase(os.path.normpath(r)) for r in roots))
    digest = hashlib.sha1(normed.encode("utf-8", "surrogatepass")).hexdigest()[:12]
    base = os.path.basename(roots[0].rstrip("\\/")) or roots[0]
    label = "".join(ch if ch.isalnum() else "_" for ch in base)
    return f"{label.strip('_')[:24] or 'root'}-{digest}"


TEMPLATE_SKIP = {"logs", "snapshots", "Cache", "cache", "updates"}


def template_values(places: engine.Places, feed: str) -> dict[str, str]:
    """What a data folder may name that differs between runs: the folders and the feed's port."""
    return {**places.mapping(), "$FEED": feed}


def save_home_template(home: Path, places: engine.Places, sc: dict[str, Any], out: Path, feed: str) -> None:
    """The data folder 0.3.4 wrote, without machine paths: $ROOT/$HOME/$TMP, the feed URL and the
    root keys of `template_roots` become placeholders (README.md, 'The upgrade scenario')."""
    if out.exists():
        shutil.rmtree(out)
    keys = []
    for roots in sc.get("template_roots", []):
        real = [places.path(r) for r in roots]
        keys.append(root_key_034(real))
    subst = []
    for name, real in sorted(template_values(places, feed).items(), key=lambda kv: -len(kv[1])):
        subst.append((json.dumps(real)[1:-1], "{json:" + name + "}"))
        subst.append((real, "{raw:" + name + "}"))
    manifest: dict[str, Any] = {"keys": sc.get("template_roots", []), "files": []}
    for p in sorted(home.rglob("*")):
        rel = p.relative_to(home)
        if p.is_dir() or rel.parts[0] in TEMPLATE_SKIP or p.suffix.lower() in (".sks", ".log", ".lock", ".tmp"):
            continue
        text = p.read_text(encoding="utf-8")
        for real, ph in subst:
            text = text.replace(real, ph)
        relname = rel.as_posix()
        for i, k in enumerate(keys):
            text = text.replace(k, "{key:%d}" % i)
            relname = relname.replace(k, "{key:%d}" % i)
        target = out / "files" / f"{len(manifest['files']):03d}"
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text, encoding="utf-8", newline="")
        manifest["files"].append({"path": relname, "data": target.relative_to(out).as_posix()})
    (out / "manifest.json").write_text(json.dumps(manifest, indent=1, ensure_ascii=False) + "\n", encoding="utf-8", newline="\n")


def apply_home_template(src: Path, home: Path, places: engine.Places, feed: str) -> None:
    """The reverse of save_home_template, for `check` (the Rust replayer does the same)."""
    manifest = json.loads((src / "manifest.json").read_text(encoding="utf-8"))
    keys = [root_key_034([places.path(r) for r in roots]) for roots in manifest["keys"]]
    for f in manifest["files"]:
        text = (src / f["data"]).read_text(encoding="utf-8")
        rel = f["path"]
        for i, k in enumerate(keys):
            text = text.replace("{key:%d}" % i, k)
            rel = rel.replace("{key:%d}" % i, k)
        for name, real in template_values(places, feed).items():
            text = text.replace("{json:" + name + "}", json.dumps(real)[1:-1]).replace("{raw:" + name + "}", real)
        target = home / Path(*rel.split("/"))
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text, encoding="utf-8", newline="")


async def upgrade_setup(spec: engine.BackendSpec, sc: dict[str, Any], rules: dict[str, Any], feed: str) -> engine.Places:
    """Run the scenario's `setup` steps on the writer (0.3.4) and keep its folders. A setup step
    that fails is an error of the scenario, not a difference."""
    setup = {**sc, "steps": sc["setup"]["steps"]}
    places = engine.Places.make(spec.label)
    try:
        transcript, _ = await engine.run_scenario(spec, setup, rules, feed, places=places, keep=True)
    except BaseException:
        places.remove()
        raise
    failed = [r for r in transcript if "error" in r or "step_error" in r]
    if failed:
        places.remove()
        raise SystemExit(f"{sc['name']}: setup failed on {spec.label}: {json.dumps(failed)[:2000]}")
    return places


def reader_places(writer: engine.Places, label: str) -> engine.Places:
    p = engine.Places.make(label)
    shutil.rmtree(p.root)
    p.root = writer.root  # tags and jumps are filed by the scan root's path: keep it
    return p


# -------------------------------------------------------------------- commands


def report(name: str, bad: list[dict[str, Any]], known: list[dict[str, Any]], sc: dict[str, Any], limit: int) -> str:
    xfail = sc.get("xfail")
    if not bad:
        status = "XPASS" if xfail else "PASS"
    else:
        status = "XFAIL" if xfail else "FAIL"
    bugs = sorted({k["bug"] for k in known if k.get("bug")})
    line = f"{status:6} {name}  ({len(bad)} differences, {len(known)} known)"
    if bugs:
        line += f"  [known bugs: {', '.join(bugs)}]"
    if xfail:
        line += f"  [xfail: {xfail}]"
    out = [line]
    for d in bad[:limit]:
        out.append(f"    {d['path']}: expected {json.dumps(d['expected'], ensure_ascii=False)[:200]}  "
                   f"actual {json.dumps(d['actual'], ensure_ascii=False)[:200]}")
    if len(bad) > limit:
        out.append(f"    … {len(bad) - limit} more")
    return "\n".join(out)


async def cmd_run(args: argparse.Namespace) -> int:
    ref = engine.BackendSpec.reference(args.ref)
    cand = engine.BackendSpec.candidate(args.cand)
    server, feed = await engine.start_feed()
    failed = 0
    try:
        for name in names(args.scenarios):
            sc = load(name)
            if not runs_here(sc):
                print(f"SKIP   {name}  (not on {sys.platform})")
                continue
            rules = norm.load_rules(sc.get("normalize"))
            if "setup" in sc:
                writer = await upgrade_setup(ref, sc, rules, feed)
                readers: list[engine.Places] = []
                try:
                    pr = reader_places(writer, "ref")
                    readers.append(pr)
                    pc = reader_places(writer, "cand")
                    readers.append(pc)
                    shutil.rmtree(pr.home)
                    shutil.rmtree(pc.home)
                    shutil.copytree(writer.home, pr.home)
                    shutil.copytree(writer.home, pc.home)
                    exp, _ = await engine.run_scenario(ref, sc, rules, feed, places=pr, keep=True)
                    act, _ = await engine.run_scenario(cand, sc, rules, feed, places=pc, keep=True)
                finally:
                    # the readers' root is the writer's: their own folder goes, then the writer's
                    for p in readers:
                        p.remove()
                    writer.remove()
            else:
                exp, _ = await engine.run_scenario(ref, sc, rules, feed)
                act, _ = await engine.run_scenario(cand, sc, rules, feed)
            diffs = norm.compare(norm.keyed(exp), norm.keyed(act), rules, "steps")
            bad, known = norm.split_known(diffs, known_list(sc))
            print(report(name, bad, known, sc, args.limit))
            if args.out:
                o = Path(args.out)
                o.mkdir(parents=True, exist_ok=True)
                (o / f"{name}.reference.json").write_text(json.dumps(doc(exp, sc), indent=1, ensure_ascii=False), encoding="utf-8")
                (o / f"{name}.candidate.json").write_text(json.dumps(doc(act, sc), indent=1, ensure_ascii=False), encoding="utf-8")
            if bad and not sc.get("xfail"):
                failed += 1
    finally:
        server.close()
    return 1 if failed else 0


async def cmd_record(args: argparse.Namespace) -> int:
    ref = engine.BackendSpec.reference(args.ref)
    server, feed = await engine.start_feed()
    GOLDEN.mkdir(parents=True, exist_ok=True)
    try:
        for name in names(args.scenarios):
            sc = load(name)
            if not runs_here(sc):
                continue
            rules = norm.load_rules(sc.get("normalize"))
            if "setup" in sc:
                writer = await upgrade_setup(ref, sc, rules, feed)
                readers: list[engine.Places] = []
                try:
                    save_home_template(writer.home, writer, sc, GOLDEN / f"{name}.home", feed)
                    pr = reader_places(writer, "ref")
                    readers.append(pr)
                    shutil.rmtree(pr.home)
                    shutil.copytree(writer.home, pr.home)
                    exp, _ = await engine.run_scenario(ref, sc, rules, feed, places=pr, keep=True)
                finally:
                    for p in readers:
                        p.remove()
                    writer.remove()
            else:
                exp, _ = await engine.run_scenario(ref, sc, rules, feed)
            out = GOLDEN / f"{name}.json"
            out.write_text(json.dumps(doc(exp, sc), indent=1, ensure_ascii=False) + "\n", encoding="utf-8", newline="\n")
            errors = [r for r in exp if "step_error" in r]
            print(f"recorded {out.relative_to(HERE)}  ({len(exp)} records, {len(errors)} step errors)")
    finally:
        server.close()
    print(f"recorded {write_listing().relative_to(HERE)}  (check its `filesystem`: README.md, 'Directory order')")
    return 0


def cmd_listing(_: argparse.Namespace) -> int:
    print(f"wrote {write_listing().relative_to(HERE)}")
    return 0


async def cmd_check(args: argparse.Namespace) -> int:
    cand = engine.BackendSpec.candidate(args.cand)
    server, feed = await engine.start_feed()
    failed = 0
    try:
        for name in names(args.scenarios):
            sc = load(name)
            golden = GOLDEN / f"{name}.json"
            if not runs_here(sc) or not golden.exists():
                print(f"SKIP   {name}  (no golden for {sys.platform})")
                continue
            rules = norm.load_rules(sc.get("normalize"))
            exp = json.loads(golden.read_text(encoding="utf-8"))["steps"]
            if "setup" in sc:
                live = {i for i, s in enumerate(sc["steps"]) if s.get("live_only")}
                exp = [r for r in exp if r["i"] not in live]

            async def replay_in(parent: str | None) -> tuple[list[dict[str, Any]], list[dict[str, Any]], str | None]:
                """(differences, known ones, the first fixture folder listed otherwise) of a replay
                with its folders in `parent` (None: the system's temp folder)."""
                moved = other_order(sc, parent)
                places = engine.Places.make("cand", parent)
                try:
                    if "setup" in sc:
                        for fname, tree in sc.get("fixtures", {}).items():
                            engine.fixtures.build(places.root / fname, tree)
                        apply_home_template(GOLDEN / f"{name}.home", places.home, places, feed)
                        act, _ = await engine.run_scenario(cand, sc, rules, feed, places=places, replay=True)
                    else:
                        act, _ = await engine.run_scenario(cand, sc, rules, feed, places=places)
                finally:
                    places.remove()  # run_scenario removed it already, unless something failed first
                diffs = norm.compare(norm.keyed(exp), norm.keyed(act), rules, "steps")
                return (*norm.split_known(diffs, known_list(sc)), moved)

            # the replayer's rule (README.md, "Directory order"): in the system's temp folder; where
            # it lists the fixture folders otherwise and that makes differences, again in
            # SPACEKNIFE_PARITY_TMP, which must list them as recorded; without it, not counted
            bad, known, moved = await replay_in(None)
            if bad and moved:
                parent = engine.fixtures.temp_parent()
                if not parent:
                    print(f"SKIP   {name}  ({len(bad)} differences not counted: this file system lists the fixture folders otherwise than the recording's ({moved}), and 0.3.4 keeps that order for entries of equal size)")
                    continue
                # in a temp folder of the system, as the recording's were (fixtures.TEMP_RULE)
                why = engine.fixtures.not_a_temp_folder(parent)
                if why:
                    print(f"FAIL   {name}  ({engine.fixtures.PARITY_TMP} is not in a temp folder of the system: {why}; a fixture's insight there, a change's reason in compare, comes from the folder's path, not from TMPDIR)")
                    failed += 1
                    continue
                bad, known, moved = await replay_in(parent)
                if moved:
                    print(f"FAIL   {name}  ({engine.fixtures.PARITY_TMP} lists the fixture folders otherwise than {LISTING.relative_to(HERE)}: {moved})")
                    failed += 1
                    continue
            print(report(name, bad, known, sc, args.limit))
            if bad and not sc.get("xfail"):
                failed += 1
    finally:
        server.close()
    return 1 if failed else 0


def methods_of(sc: dict[str, Any]) -> set[str]:
    out = set()
    for s in sc.get("steps", []) + sc.get("setup", {}).get("steps", []):
        if "call" in s:
            out.add(s["call"])
        if "subscribe" in s:
            out.add("$/subscribe")
        if "wait_scan" in s:
            out |= {"$/subscribe", "$/unsubscribe"}
    return out


def cmd_coverage(_: argparse.Namespace) -> int:
    schema = json.loads(SCHEMA.read_text(encoding="utf-8"))
    every = set(schema["methods"])
    called: dict[str, set[str]] = {}
    for name in names([]):
        sc = load(name)
        for m in methods_of(sc):
            called.setdefault(m, set()).add(name)
    ok = {m for m in every if any(n != "invalid_params" for n in called.get(m, ()))}
    print(f"{len(ok)}/{len(every)} methods called with valid params by a scenario")
    print("only checked for invalid params:", ", ".join(sorted(every - ok)) or "-")
    unknown = sorted(set(called) - every - {"test.sleep"})
    if unknown:
        print("unknown methods in scenarios:", ", ".join(unknown))
    return 0


def cmd_gen_invalid(_: argparse.Namespace) -> int:
    """scenarios/invalid_params.json from the protocol schema: every method with an unknown field,
    every method with a required field without it, and `$/subscribe` to an unknown topic."""
    schema = json.loads(SCHEMA.read_text(encoding="utf-8"))
    defs = schema["$defs"]
    steps: list[dict[str, Any]] = [{"note": "generated by `parity.py gen-invalid`; do not edit by hand"}]
    last = []
    for m in sorted(schema["methods"]):
        # session.hello has the handshake scenario; machine.read_health ignores unknown fields
        # (both backends) and would then ask for administrator rights
        if m in ("session.hello", "machine.read_health"):
            continue
        ref = schema["methods"][m]["params"].rsplit("/", 1)[-1]
        p = defs.get(ref, {})
        target = last if m == "app.shutdown" else steps
        unknown: dict[str, Any] = {"call": m, "params": {"__parity_unknown__": 1}}
        if m.startswith("machine."):
            # lenient (both backends): the answer is this machine's disks, compared live only
            unknown["shape"] = True
            unknown["live_only"] = True
        target.append(unknown)
        req = p.get("required", [])
        if req:
            target.append({"call": m, "params": {}})
    # params by position: neither backend answers such a request (both drop the frame)
    for m in ("system.ping", "scan.start"):
        steps.append({"call": m, "params": [1], "timeout": 3})
    steps.extend(last)
    steps.append({"call": "$/subscribe", "params": {"topic": "no-such-topic"}})
    steps.append({"call": "no.such_method", "params": {}})
    sc = {
        "name": "invalid_params",
        "description": "Every method refuses an unknown field, a missing required field and non-object params; unknown topics and methods.",
        "steps": steps,
    }
    (SCENARIOS / "invalid_params.json").write_text(json.dumps(sc, indent=1) + "\n", encoding="utf-8", newline="\n")
    print(f"wrote scenarios/invalid_params.json ({len(steps)} steps)")
    return 0


ORACLE = HERE.parent.parent / "crates" / "sk-core" / "tests" / "data" / "spacesniffer_filter_oracle.json"


def cmd_gen_filter(_: argparse.Namespace) -> int:
    """scenarios/filter_oracle.json: every probe of the SpaceSniffer oracle through filter.parse,
    view.filter and view.results on the oracle's fixture trees (crates/sk-core/tests/filter_compat.rs
    checks the same probes on an in-memory store)."""
    oracle = json.loads(ORACLE.read_text(encoding="utf-8"))
    steps: list[dict[str, Any]] = [{"note": "generated by `parity.py gen-filter`; do not edit by hand"}]

    def probes(scan: str, view: str, texts: list[str]) -> None:
        for text in texts:
            steps.append({"call": "filter.parse", "params": {"text": text}})
            steps.append({"call": "view.filter", "params": {"view_id": f"${view}.state.view_id", "text": text},
                          "pick": ["filter"]})
            steps.append({"call": "view.results", "params": {"view_id": f"${view}.state.view_id", "sort": "name", "limit": 100},
                          "pick": ["active", "files", "alloc", "total", "rows"]})

    for scan, view, tree, texts in (
        ("s", "v", "filter", sorted(set(oracle["results"]) | set(oracle["dialogs"]))),
        ("u", "w", "units", sorted(oracle["unit_results"])),
    ):
        steps.append({"call": "scan.start", "params": {"roots": [f"$ROOT/{tree}"], "workers": 0, "auto_snapshot": False},
                      "as": scan, "record": False})
        steps.append({"wait_scan": f"${scan}.scan_id", "pick": ["state", "items", "files"]})
        steps.append({"call": "view.open", "params": {"scan_id": f"${scan}.scan_id", "viewport": {"width": 1000, "height": 700, "dpr": 1},
                                                      "settings": {"detail": 15, "min_element": 1}}, "as": view, "record": False})
        probes(scan, view, texts)
    sc = {
        "name": "filter_oracle",
        "description": "The SpaceSniffer filter oracle's probes over the protocol (filter.parse, view.filter, view.results) on scanned fixture trees.",
        "fixtures": {"filter": "filter", "units": "units"},
        "normalize": {"time_keys": ["mtime"]},
        "steps": steps,
    }
    (SCENARIOS / "filter_oracle.json").write_text(json.dumps(sc, indent=1, ensure_ascii=False) + "\n", encoding="utf-8", newline="\n")
    print(f"wrote scenarios/filter_oracle.json ({len(steps)} steps)")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    for name in ("run", "record", "check"):
        s = sub.add_parser(name)
        if name in ("run", "record"):
            s.add_argument("--ref", default=os.environ.get("SPACEKNIFE_PARITY_REF"), required=not os.environ.get("SPACEKNIFE_PARITY_REF"),
                           help="a worktree of tag v0.3.4 with `uv sync` done")
        if name in ("run", "check"):
            s.add_argument("--cand", default=os.environ.get("SPACEKNIFE_PARITY_RUST"), required=not os.environ.get("SPACEKNIFE_PARITY_RUST"),
                           help="the spaceknife-cli executable")
            s.add_argument("--limit", type=int, default=12, help="differences shown per scenario")
        if name == "run":
            s.add_argument("--out", help="also write both transcripts into this folder")
        s.add_argument("scenarios", nargs="*")
    sub.add_parser("coverage")
    sub.add_parser("gen-invalid")
    sub.add_parser("gen-filter")
    sub.add_parser("listing")
    args = ap.parse_args()
    if args.cmd == "listing":
        return cmd_listing(args)
    if args.cmd == "coverage":
        return cmd_coverage(args)
    if args.cmd == "gen-invalid":
        return cmd_gen_invalid(args)
    if args.cmd == "gen-filter":
        return cmd_gen_filter(args)
    fn = {"run": cmd_run, "record": cmd_record, "check": cmd_check}[args.cmd]
    if sys.platform == "win32":
        asyncio.set_event_loop_policy(asyncio.WindowsProactorEventLoopPolicy())
    return asyncio.run(fn(args))


if __name__ == "__main__":
    sys.exit(main())
