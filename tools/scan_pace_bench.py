"""Old against new hard scans, interleaved (docs/native.md 2.2 「列目录线程」, measurements).

Runs two `spaceknife-cli` builds on the same folders, alternating which goes first every round,
and prints the wall time, the scan's own time and the profile lines of each run, then a summary
(median [min-max] per arm, paired ratios new/old).

Cache states (`--state`):

- warm: nothing is done between runs (the first round of each arm is a warm-up and is reported
  apart).
- cold: before every run the file cache is emptied gently (Windows, elevated): the system file
  cache's working set is trimmed (SetSystemFileCacheSize(-1, -1, 0)) and the standby list purged
  (NtSetSystemInformation(SystemMemoryListInformation, MemoryPurgeStandbyList)). Other programs
  lose cached file pages and read them again; no process's working set is touched, no volume is
  dismounted or locked.
- half: purge, then the folders given with `--warm` are read (by the old build, the same for both
  arms), then the whole path is scanned: a scan that stopped half way.
- uneven: purge, then a random `--share` of the path's second-level folders (seeded, the same for
  both arms of a round) is read, then the whole path is scanned.

Before every purge the script looks for other disk benchmarks or scanners (gdu, dua, diskspd, fio,
WizTree, WinDirStat, other spaceknife processes, anything with "bench" in its command line) and
waits (up to `--wait` seconds) or skips the run. Before every scan and purge it checks the free
physical memory: below `--min-free` GB it waits (every 30 s, up to 10 minutes), then skips the run
(a purge while memory is short would make other programs page from the disk). One scan at a time.
Each run records the scan process's peak working set. Drive E: is refused.

    python tools/scan_pace_bench.py --old OLD.exe --new NEW.exe --state warm --rounds 5 "C:\\Program Files"
    python tools/scan_pace_bench.py --old OLD.exe --new NEW.exe --state cold --limit balanced --rounds 3 C:\\

Only the standard library; Python 3.12.
"""

from __future__ import annotations

import argparse
import ctypes
import json
import os
import random
import re
import statistics
import subprocess
import sys
import time
from pathlib import Path

GUARD = re.compile(r"gdu|dua|diskspd|fio\.exe|wiztree|windirstat|spaceknife", re.I)


def refuse_e(path: str) -> None:
    if re.match(r"^\s*[\\/]*(\\\\\?\\)?e:", path, re.I):
        sys.exit(f"refused: {path} is on drive E:")


# ------------------------------------------------------------------------------- guard


def others_running(own: set[int]) -> list[str]:
    """Other disk benchmarks or scanners running now (Windows: from CIM; else ps)."""
    found = []
    if os.name == "nt":
        cmd = [
            "powershell",
            "-NoProfile",
            "-Command",
            "Get-CimInstance Win32_Process | ForEach-Object { \"$($_.ProcessId)`t$($_.Name)`t$($_.CommandLine)\" }",
        ]
    else:
        cmd = ["ps", "-eo", "pid=,comm=,args="]
    out = subprocess.run(cmd, capture_output=True, text=True, errors="replace").stdout
    me = os.getpid()
    for line in out.splitlines():
        parts = line.split("\t") if os.name == "nt" else line.split(None, 2)
        if len(parts) < 2 or not parts[0].strip().isdigit():
            continue
        pid = int(parts[0])
        name = parts[1]
        args = parts[2] if len(parts) > 2 else ""
        if pid == me or pid in own or "scan_pace_bench" in args:
            continue
        if GUARD.search(name) or re.search(r"bench", args, re.I):
            found.append(f"{pid} {name} {args[:120]}")
    return found


def wait_until_alone(own: set[int], wait_s: float) -> bool:
    end = time.monotonic() + wait_s
    while True:
        others = others_running(own)
        if not others:
            return True
        if time.monotonic() >= end:
            print("  skipped: others are measuring the disks:", *others, sep="\n    ")
            return False
        print("  waiting for:", *others, sep="\n    ")
        time.sleep(10)


# ------------------------------------------------------------------------------- memory


def free_gb() -> float | None:
    """Free physical memory, GB (None: not known here)."""
    if os.name == "nt":
        from ctypes import wintypes

        class MEMORYSTATUSEX(ctypes.Structure):
            _fields_ = [
                ("dwLength", wintypes.DWORD),
                ("dwMemoryLoad", wintypes.DWORD),
                ("ullTotalPhys", ctypes.c_ulonglong),
                ("ullAvailPhys", ctypes.c_ulonglong),
                ("ullTotalPageFile", ctypes.c_ulonglong),
                ("ullAvailPageFile", ctypes.c_ulonglong),
                ("ullTotalVirtual", ctypes.c_ulonglong),
                ("ullAvailVirtual", ctypes.c_ulonglong),
                ("ullAvailExtendedVirtual", ctypes.c_ulonglong),
            ]

        m = MEMORYSTATUSEX()
        m.dwLength = ctypes.sizeof(m)
        if ctypes.windll.kernel32.GlobalMemoryStatusEx(ctypes.byref(m)):
            return m.ullAvailPhys / 1e9
        return None
    try:
        for line in open("/proc/meminfo"):
            if line.startswith("MemAvailable:"):
                return int(line.split()[1]) / 1e6
    except OSError:
        pass
    return None


def enough_memory(min_gb: float) -> bool:
    """Free memory at least `min_gb`, waiting up to 10 minutes for it; False: skip."""
    end = time.monotonic() + 600
    while True:
        free = free_gb()
        if free is None or free >= min_gb:
            return True
        if time.monotonic() >= end:
            print(f"  skipped: {free:.1f} GB free for 10 minutes (below {min_gb} GB)", flush=True)
            return False
        print(f"  waiting: {free:.1f} GB free (below {min_gb} GB)", flush=True)
        time.sleep(30)


def peak_working_set(handle: int) -> int | None:
    """Windows: the peak working set of an exited process whose handle is still open."""
    if os.name != "nt":
        return None
    from ctypes import wintypes

    class PROCESS_MEMORY_COUNTERS(ctypes.Structure):
        _fields_ = [
            ("cb", wintypes.DWORD),
            ("PageFaultCount", wintypes.DWORD),
            ("PeakWorkingSetSize", ctypes.c_size_t),
            ("WorkingSetSize", ctypes.c_size_t),
            ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
            ("QuotaPagedPoolUsage", ctypes.c_size_t),
            ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
            ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
            ("PagefileUsage", ctypes.c_size_t),
            ("PeakPagefileUsage", ctypes.c_size_t),
        ]

    psapi = ctypes.WinDLL("psapi")
    psapi.GetProcessMemoryInfo.argtypes = [wintypes.HANDLE, ctypes.c_void_p, wintypes.DWORD]
    psapi.GetProcessMemoryInfo.restype = wintypes.BOOL
    c = PROCESS_MEMORY_COUNTERS()
    c.cb = ctypes.sizeof(c)
    if psapi.GetProcessMemoryInfo(wintypes.HANDLE(handle), ctypes.byref(c), c.cb):
        return c.PeakWorkingSetSize
    return None


# ------------------------------------------------------------------------------- purge


def purge() -> None:
    """The gentle purge of the module docs (Windows, elevated)."""
    if os.name != "nt":
        raise SystemExit("cold states need Windows here (drop_caches / purge elsewhere: by hand)")
    from ctypes import wintypes

    k32 = ctypes.WinDLL("kernel32", use_last_error=True)
    adv = ctypes.WinDLL("advapi32", use_last_error=True)
    ntdll = ctypes.WinDLL("ntdll")

    class LUID(ctypes.Structure):
        _fields_ = [("LowPart", wintypes.DWORD), ("HighPart", wintypes.LONG)]

    class LUID_AND_ATTRIBUTES(ctypes.Structure):
        _fields_ = [("Luid", LUID), ("Attributes", wintypes.DWORD)]

    class TOKEN_PRIVILEGES(ctypes.Structure):
        _fields_ = [("PrivilegeCount", wintypes.DWORD), ("Privileges", LUID_AND_ATTRIBUTES * 1)]

    k32.GetCurrentProcess.restype = wintypes.HANDLE
    k32.CloseHandle.argtypes = [wintypes.HANDLE]
    adv.OpenProcessToken.argtypes = [wintypes.HANDLE, wintypes.DWORD, ctypes.POINTER(wintypes.HANDLE)]
    adv.OpenProcessToken.restype = wintypes.BOOL
    adv.LookupPrivilegeValueW.argtypes = [wintypes.LPCWSTR, wintypes.LPCWSTR, ctypes.POINTER(LUID)]
    adv.LookupPrivilegeValueW.restype = wintypes.BOOL
    adv.AdjustTokenPrivileges.argtypes = [
        wintypes.HANDLE,
        wintypes.BOOL,
        ctypes.POINTER(TOKEN_PRIVILEGES),
        wintypes.DWORD,
        ctypes.c_void_p,
        ctypes.c_void_p,
    ]
    adv.AdjustTokenPrivileges.restype = wintypes.BOOL
    k32.SetSystemFileCacheSize.argtypes = [ctypes.c_size_t, ctypes.c_size_t, wintypes.DWORD]
    k32.SetSystemFileCacheSize.restype = wintypes.BOOL
    ntdll.NtSetSystemInformation.argtypes = [ctypes.c_int, ctypes.c_void_p, wintypes.ULONG]
    ntdll.NtSetSystemInformation.restype = ctypes.c_long

    token = wintypes.HANDLE()
    TOKEN_ADJUST_PRIVILEGES, TOKEN_QUERY, SE_PRIVILEGE_ENABLED = 0x20, 0x8, 0x2
    if not adv.OpenProcessToken(
        k32.GetCurrentProcess(), TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY, ctypes.byref(token)
    ):
        raise OSError(ctypes.get_last_error(), "OpenProcessToken")
    try:
        for name in ("SeIncreaseQuotaPrivilege", "SeProfileSingleProcessPrivilege"):
            tp = TOKEN_PRIVILEGES(1)
            if not adv.LookupPrivilegeValueW(None, name, ctypes.byref(tp.Privileges[0].Luid)):
                raise OSError(ctypes.get_last_error(), f"LookupPrivilegeValueW {name}")
            tp.Privileges[0].Attributes = SE_PRIVILEGE_ENABLED
            ctypes.set_last_error(0)
            ok = adv.AdjustTokenPrivileges(token, False, ctypes.byref(tp), 0, None, None)
            if not ok or ctypes.get_last_error() != 0:  # ERROR_NOT_ALL_ASSIGNED: not elevated
                raise SystemExit(f"cannot enable {name}: run elevated")
    finally:
        k32.CloseHandle(token)
    minus_one = ctypes.c_size_t(-1).value
    if not k32.SetSystemFileCacheSize(minus_one, minus_one, 0):
        raise OSError(ctypes.get_last_error(), "SetSystemFileCacheSize")
    SystemMemoryListInformation, MemoryPurgeStandbyList = 80, 4
    command = ctypes.c_int(MemoryPurgeStandbyList)
    status = ntdll.NtSetSystemInformation(
        SystemMemoryListInformation, ctypes.byref(command), ctypes.sizeof(command)
    )
    if status != 0:
        raise OSError(status, f"NtSetSystemInformation: 0x{status & 0xFFFFFFFF:08x}")


# ------------------------------------------------------------------------------- runs

TIME = re.compile(r"Time\s+(?:(\d+)\s*min\s*)?([\d.,]+)\s*(ms|s)\b")


def parse_time(out: str) -> float | None:
    m = TIME.search(out)
    if not m:
        return None
    minutes = int(m.group(1) or 0)
    value = float(m.group(2).replace(",", ""))
    return minutes * 60 + (value / 1000 if m.group(3) == "ms" else value)


def profile_lines(out: str) -> str:
    """The profile panel's text, joined (wrapped lines put back together)."""
    lines, on = [], False
    for line in out.splitlines():
        if "Where the time went" in line:
            on = True
            continue
        if on:
            if line.startswith("└"):
                break
            lines.append(line.strip("│ ").strip())
    return " ".join(lines)


def scan(exe: str, path: str, limit: str | None, own: set[int]) -> dict:
    env = dict(os.environ, SPACEKNIFE_LANGUAGE="en")
    args = [exe, "scan", path, "--no-snapshot", "--profile", "--top", "1", "--depth", "0"]
    if limit:
        args += ["--disk-limit", limit]
    t0 = time.perf_counter()
    p = subprocess.Popen(args, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, env=env)
    own.add(p.pid)
    out, _ = p.communicate()
    wall = time.perf_counter() - t0
    peak = peak_working_set(int(p._handle)) if os.name == "nt" else None
    own.discard(p.pid)
    text = out.decode("utf-8", errors="replace")
    items = re.search(r"Items\s+([\d,]+)", text)
    return {
        "wall": wall,
        "time": parse_time(text),
        "items": int(items.group(1).replace(",", "")) if items else None,
        "profile": profile_lines(text),
        "code": p.returncode,
        "peak_mb": round(peak / 2**20) if peak else None,
    }


def second_level(path: str) -> list[str]:
    out = []
    for top in sorted(Path(path).iterdir(), key=lambda p: p.name.lower()):
        try:
            if top.is_dir() and not top.is_symlink():
                out += [str(c) for c in sorted(top.iterdir()) if c.is_dir() and not c.is_symlink()]
        except OSError:
            continue
    return out


def prepare(args, own: set[int], rnd: int) -> bool:
    """The cache state before a run; False: skipped."""
    if not enough_memory(args.min_free):
        return False
    if args.state == "warm":
        return True
    if not wait_until_alone(own, args.wait):
        return False
    purge()
    time.sleep(args.settle)
    if args.state == "half":
        for w in args.warm:
            scan(args.old, w, None, own)
    elif args.state == "uneven":
        rng = random.Random(args.seed * 1000 + rnd)
        folders = second_level(args.paths[0])
        pick = rng.sample(folders, max(1, int(len(folders) * args.share)))
        for w in pick:
            scan(args.old, w, None, own)
    return True


def summary(results: list[dict]) -> None:
    def fmt(xs: list[float]) -> str:
        return f"{statistics.median(xs):.2f} s [{min(xs):.2f}–{max(xs):.2f}]" if xs else "-"

    for path in sorted({r["path"] for r in results}):
        rs = [r for r in results if r["path"] == path and not r.get("warmup")]
        by = {arm: [r for r in rs if r["arm"] == arm] for arm in ("old", "new")}
        print(f"\n{path} ({len(by['old'])} + {len(by['new'])} runs)")
        for arm in ("old", "new"):
            print(f"  {arm}: scan {fmt([r['time'] for r in by[arm] if r['time']])}, "
                  f"wall {fmt([r['wall'] for r in by[arm]])}")
        pairs = {}
        for r in rs:
            pairs.setdefault(r["round"], {})[r["arm"]] = r
        both = [p for p in pairs.values() if "new" in p and "old" in p]
        # the scan's own time (no start-up or tear-down; printed to three digits) and the wall
        for key in ("time", "wall"):
            ratios = [p["new"][key] / p["old"][key] for p in both if p["new"][key] and p["old"][key]]
            if ratios:
                print(f"  new/old by round ({key}): {statistics.median(ratios):.2f} "
                      f"[{min(ratios):.2f}–{max(ratios):.2f}] ({', '.join(f'{x:.2f}' for x in ratios)})")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("paths", nargs="+")
    ap.add_argument("--old", required=True)
    ap.add_argument("--new", required=True)
    ap.add_argument("--state", choices=["warm", "cold", "half", "uneven"], default="warm")
    ap.add_argument("--limit", help="--disk-limit for the scans (balanced, quiet, max, a percent)")
    ap.add_argument("--rounds", type=int, default=3)
    ap.add_argument("--warm", nargs="*", default=[], help="half: the folders read first")
    ap.add_argument("--share", type=float, default=0.3, help="uneven: second-level folders read first")
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--settle", type=float, default=3.0, help="seconds after a purge")
    ap.add_argument("--wait", type=float, default=600.0, help="seconds to wait for other measurements")
    ap.add_argument("--min-free", type=float, default=8.0, help="GB of free memory a run needs")
    ap.add_argument("--out", help="append every run as a JSON line to this file")
    args = ap.parse_args()
    for p in args.paths + args.warm:
        refuse_e(p)
    if args.state == "half" and not args.warm:
        sys.exit("--state half needs --warm FOLDER...")
    own: set[int] = set()
    results = []
    warmup = args.state == "warm"
    for path in args.paths:
        for rnd in range(args.rounds + (1 if warmup else 0)):
            arms = ["old", "new"] if rnd % 2 == 0 else ["new", "old"]
            for arm in arms:
                if not prepare(args, own, rnd):
                    continue
                r = scan(getattr(args, arm), path, args.limit, own)
                r.update(path=path, arm=arm, round=rnd, state=args.state, limit=args.limit,
                         warmup=warmup and rnd == 0, at=time.strftime("%Y-%m-%d %H:%M:%S"))
                results.append(r)
                print(f"{r['at']} {path} round {rnd} {arm}: scan {r['time']} s, wall {r['wall']:.2f} s, "
                      f"{r['items']} items, peak {r['peak_mb']} MB{' (warm-up)' if r['warmup'] else ''}"
                      f"\n    {r['profile']}", flush=True)
                if args.out:
                    with open(args.out, "a", encoding="utf-8") as f:
                        f.write(json.dumps(r, ensure_ascii=False) + "\n")
    summary(results)


if __name__ == "__main__":
    main()
