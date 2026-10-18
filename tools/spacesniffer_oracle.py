"""Record SpaceSniffer 2.0.5.18's filter behaviour as a compatibility oracle (Windows only).

SpaceSniffer can be driven from its command line: ``scan <dir> filter <expr> export "Simple file
list" <out> autoclose`` writes the files that survive the filter. This script builds a fixture
tree with precisely controlled sizes, timestamps, attributes, classes and folder names, runs
every probe expression through SpaceSniffer and stores the results as JSON. The SpaceKnife test
suite replays the same fixture against our filter engine and compares the sets.

SpaceSniffer runs on a separate, never-shown desktop (``CreateDesktopW``): neither its window nor
its error dialogs appear on the user's screen or take the keyboard focus. When a filter is
invalid SpaceSniffer shows "Filter contains errors" and waits; the probe reads the dialog's text,
records it and ends the process after 20 s.

Usage (from the repo root, with SpaceSniffer.exe available)::

    uv run python tools/spacesniffer_oracle.py --exe SpaceSniffer.exe \\
        --out packages/backend/tests/data/spacesniffer_filter_oracle.json

``--recheck`` re-runs only ``RECHECK`` and ``UNIT_PROBES`` and merges them into ``--out`` (saved
after every probe). The fixture definition lives in ``packages/backend/tests/filter_fixture.py``
so tests and this recorder can never disagree about it.
"""

from __future__ import annotations

import argparse
import ctypes
import json
import shutil
import subprocess
import sys
import tempfile
import time
from ctypes import wintypes
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "packages" / "backend" / "tests"))

import filter_fixture as fx  # noqa: E402

# SpaceSniffer neither exports nor shows a dialog when a folder mask (or a filter on a tree without
# sub-folders) leaves nothing to show: it sits at "Ready". Such probes are recorded as empty.
TIMEOUT_NO_DIALOG = "<timeout without dialog>"
TIMEOUT_S = 20

Probe = tuple[list[str] | None, list[str]]


class _StartupInfo(ctypes.Structure):
    _fields_ = [
        ("cb", wintypes.DWORD),
        ("lpReserved", wintypes.LPWSTR),
        ("lpDesktop", wintypes.LPWSTR),
        ("lpTitle", wintypes.LPWSTR),
        ("dwX", wintypes.DWORD),
        ("dwY", wintypes.DWORD),
        ("dwXSize", wintypes.DWORD),
        ("dwYSize", wintypes.DWORD),
        ("dwXCountChars", wintypes.DWORD),
        ("dwYCountChars", wintypes.DWORD),
        ("dwFillAttribute", wintypes.DWORD),
        ("dwFlags", wintypes.DWORD),
        ("wShowWindow", wintypes.WORD),
        ("cbReserved2", wintypes.WORD),
        ("lpReserved2", ctypes.c_void_p),
        ("hStdInput", wintypes.HANDLE),
        ("hStdOutput", wintypes.HANDLE),
        ("hStdError", wintypes.HANDLE),
    ]


class _ProcessInfo(ctypes.Structure):
    _fields_ = [
        ("hProcess", wintypes.HANDLE),
        ("hThread", wintypes.HANDLE),
        ("dwProcessId", wintypes.DWORD),
        ("dwThreadId", wintypes.DWORD),
    ]


class HiddenDesktop:
    """A desktop in the interactive window station that is never switched to."""

    NAME = "spaceknife-oracle"

    def __init__(self) -> None:
        self.user32: Any = ctypes.WinDLL("user32", use_last_error=True)  # type: ignore[attr-defined,unused-ignore]
        self.kernel32: Any = ctypes.WinDLL("kernel32", use_last_error=True)  # type: ignore[attr-defined,unused-ignore]
        self.user32.CreateDesktopW.restype = wintypes.HANDLE
        self.handle = self.user32.CreateDesktopW(self.NAME, None, None, 0, 0x10000000, None)  # GENERIC_ALL
        if not self.handle:
            raise ctypes.WinError(ctypes.get_last_error())  # type: ignore[attr-defined,unused-ignore]

    def close(self) -> None:
        self.user32.CloseDesktop(wintypes.HANDLE(self.handle))

    def run(self, args: list[str], cwd: Path, timeout_s: float) -> tuple[bool, list[str]]:
        """Run ``args`` on this desktop. Returns (exited, dialog texts if it had to be killed)."""
        si = _StartupInfo()
        si.cb = ctypes.sizeof(si)
        si.lpDesktop = f"winsta0\\{self.NAME}"
        si.dwFlags = 0x1  # STARTF_USESHOWWINDOW
        si.wShowWindow = 7  # SW_SHOWMINNOACTIVE, in case the desktop were ever shown
        pi = _ProcessInfo()
        cmdline = ctypes.create_unicode_buffer(subprocess.list2cmdline(args))
        ok = self.kernel32.CreateProcessW(
            None, cmdline, None, None, False, 0, None, str(cwd), ctypes.byref(si), ctypes.byref(pi)
        )
        if not ok:
            raise ctypes.WinError(ctypes.get_last_error())  # type: ignore[attr-defined,unused-ignore]
        try:
            waited = self.kernel32.WaitForSingleObject(pi.hProcess, int(timeout_s * 1000))
            if waited == 0:  # WAIT_OBJECT_0
                return True, []
            texts = self.dialog_texts(pi.dwProcessId)
            self.kernel32.TerminateProcess(pi.hProcess, 1)
            self.kernel32.WaitForSingleObject(pi.hProcess, 5000)
            return False, texts
        finally:
            self.kernel32.CloseHandle(pi.hProcess)
            self.kernel32.CloseHandle(pi.hThread)

    def dialog_texts(self, pid: int) -> list[str]:
        """Title and control texts of the dialogs ``pid`` shows on this desktop."""
        user32 = self.user32
        proc = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)  # type: ignore[attr-defined,unused-ignore]
        texts: list[str] = []

        def text_of(hwnd: int) -> str:
            n = user32.GetWindowTextLengthW(hwnd)
            buf = ctypes.create_unicode_buffer(n + 1)
            user32.GetWindowTextW(hwnd, buf, n + 1)
            return buf.value

        def class_of(hwnd: int) -> str:
            buf = ctypes.create_unicode_buffer(64)
            user32.GetClassNameW(hwnd, buf, 64)
            return buf.value

        def on_child(hwnd: int, _: int) -> bool:
            if (t := text_of(hwnd).strip()) and class_of(hwnd) not in ("Button", "TButton"):
                texts.append(t)
            return True

        def on_top(hwnd: int, _: int) -> bool:
            owner = wintypes.DWORD()
            user32.GetWindowThreadProcessId(hwnd, ctypes.byref(owner))
            if owner.value == pid and class_of(hwnd) in ("#32770", "TMessageForm"):
                texts.append(f"[{text_of(hwnd)}]")
                user32.EnumChildWindows(hwnd, proc(on_child), 0)
            return True

        user32.EnumDesktopWindows(wintypes.HANDLE(self.handle), proc(on_top), 0)
        return texts


def run_probe(desk: HiddenDesktop, exe: Path, workdir: Path, tree: Path, expr: str) -> Probe:
    """Files surviving ``expr``; ``(None, dialog texts)`` when SpaceSniffer rejected it with a dialog."""
    out = workdir / "out.txt"
    out.unlink(missing_ok=True)
    args = [str(exe), "scan", str(tree)]
    if expr:
        args += ["filter", expr]
    args += ["export", "Simple file list", str(out), "autoclose"]
    exited, texts = desk.run(args, workdir, TIMEOUT_S)
    if not exited:
        if not texts:
            return [], [TIMEOUT_NO_DIALOG]
        return None, texts
    if not out.exists():
        return None, []
    lines = out.read_text(encoding="utf-8-sig", errors="replace").splitlines()
    prefix = str(tree).rstrip("\\") + "\\"
    return sorted(line[len(prefix) :].replace("\\", "/") for line in lines if line.startswith(prefix)), []


def _save(out: Path, payload: dict[str, Any]) -> None:
    out.parent.mkdir(parents=True, exist_ok=True)
    tmp = out.with_suffix(".tmp")
    tmp.write_text(json.dumps(payload, ensure_ascii=False, indent=1) + "\n", encoding="utf-8")
    tmp.replace(out)


def _record(payload: dict[str, Any], section: str, expr: str, probe: Probe) -> None:
    res, texts = probe
    payload.setdefault(section, {})[expr] = res
    dialogs = payload.setdefault("dialogs", {})
    key = expr if section == "results" else f"units:{expr}"
    dialogs.pop(key, None)
    if texts:
        dialogs[key] = texts


def _shown(res: list[str] | None, texts: list[str]) -> str:
    if res is None:
        return "ERROR " + " ".join(t.replace("\n", " ") for t in texts)
    return f"{len(res)} files" + (" (no export, empty)" if texts else "")


def main() -> int:
    if sys.platform != "win32":
        print("SpaceSniffer only runs on Windows", file=sys.stderr)
        return 2
    parser = argparse.ArgumentParser()
    parser.add_argument("--exe", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument(
        "--recheck", action="store_true", help="only re-run fx.RECHECK and fx.UNIT_PROBES, merging into --out"
    )
    args = parser.parse_args()
    if args.recheck:
        payload: dict[str, Any] = json.loads(args.out.read_text(encoding="utf-8"))
        probes = [("results", e) for e in fx.RECHECK] + [("unit_results", e) for e in fx.UNIT_PROBES]
    else:
        payload = {"spacesniffer": "2.0.5.18", "fixture_version": fx.VERSION}
        probes = [("results", e) for e in fx.PROBES] + [("unit_results", e) for e in fx.UNIT_PROBES]
    payload["recorded_at"] = time.strftime("%Y-%m-%d %H:%M:%S")
    desk = HiddenDesktop()
    try:
        with tempfile.TemporaryDirectory(prefix="ss-oracle-") as tmp:
            work = Path(tmp)
            exe = work / "SpaceSniffer.exe"
            shutil.copy2(args.exe, exe)  # run a private copy: its config file lands in the temp dir
            now = time.time()
            tree, units = work / "fixture", work / "units"
            fx.build(tree, now)
            fx.build(units, now, fx.UNIT_FILES)
            for i, (section, expr) in enumerate(probes, 1):
                res, texts = run_probe(desk, exe, work, tree if section == "results" else units, expr)
                _record(payload, section, expr, (res, texts))
                _save(args.out, payload)
                print(f"[{i:>3}/{len(probes)}] {section[:5]} {expr!r:40} -> {_shown(res, texts)}", flush=True)
    finally:
        desk.close()
    print(f"wrote {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
