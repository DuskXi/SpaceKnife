# PyInstaller spec of the frozen backend (M8): onedir dist/backend/spaceknife-backend/.
# Built by tools/build_package.py (needs `uv sync --group native --group package`); the Electron
# installers ship the folder as resources/backend/ (apps/electron/electron-builder.yml). PyInstaller
# freezes the interpreter it runs on, so each OS and CPU architecture is built on its own machine.
# ruff: noqa
import json
import os
import sys

from PyInstaller.utils.hooks import collect_data_files, collect_submodules

ROOT = SPECPATH + "/.."
APP_VERSION = json.loads(open(ROOT + "/apps/electron/package.json", encoding="utf-8").read())["version"]

# every module: request handlers, platform back ends and the MFT reader are imported on demand
hidden = [
    *collect_submodules("spaceknife"),
    *collect_submodules("spaceknife_protocol"),
    *collect_submodules("spaceknife_sdk"),
    *collect_submodules("spaceknife_native"),
]
# locales/*.json, insights/builtin.toml, export/builtin/*.sktpl
datas = collect_data_files("spaceknife", excludes=["**/__pycache__/**"])

a = Analysis(
    [SPECPATH + "/backend_entry.py"],
    # tools/build_package.py: the release build of spaceknife_native (build paths remapped) first
    pathex=[os.environ["SPACEKNIFE_NATIVE_RELEASE"]] if os.environ.get("SPACEKNIFE_NATIVE_RELEASE") else [],
    hiddenimports=hidden,
    datas=datas,
    # dev tools; pydantic.mypy (the mypy plugin) would pull in mypy itself
    excludes=["tkinter", "_tkinter", "pytest", "hypothesis", "IPython", "PIL", "resvg_py", "pydantic.mypy", "mypy"],
    noarchive=False,
)
LINUX = sys.platform.startswith("linux")
MAC = sys.platform == "darwin"
if LINUX:
    # Every target system has its own GCC runtime (glibc's pthread_cancel needs libgcc_s; apt and
    # dnf need libstdc++), as manylinux wheels assume; the build host's copies may need a newer
    # glibc than RHEL 9's 2.34 (Ubuntu 22.04's libgcc_s, GCC 12: GLIBC_2.35). tools/build_package.py
    # checks the glibc floor of everything that stays.
    a.binaries = [b for b in a.binaries if not b[0].startswith(("libgcc_s.so", "libstdc++.so"))]
if LINUX or MAC:
    # Python's build configuration (_sysconfigdata_*) names where this interpreter is installed,
    # under the build machine's home folder: compile a copy without it (the paths mean nothing in
    # the frozen app; tools/build_package.py rejects any build-machine path in the bundle).
    home = os.path.expanduser("~")
    neutral = "/Users/builder" if MAC else "/home/builder"
    pure = []
    for name, src, typecode in a.pure:
        if name.startswith("_sysconfigdata") and src and src.endswith(".py"):
            text = open(src, encoding="utf-8").read().replace(home, neutral)
            src = os.path.join(workpath, name + ".py")
            with open(src, "w", encoding="utf-8") as f:
                f.write(text)
        pure.append((name, src, typecode))
    a.pure = pure
pyz = PYZ(a.pure)

version = None  # Windows: the name Task Manager shows, the product version
if sys.platform == "win32":
    from PyInstaller.utils.win32.versioninfo import (
        FixedFileInfo,
        StringFileInfo,
        StringStruct,
        StringTable,
        VarFileInfo,
        VarStruct,
        VSVersionInfo,
    )

    nums = (*(int(n) for n in APP_VERSION.split(".")), 0)
    strings = {
        "CompanyName": "DuskXi",
        "FileDescription": "SpaceKnife backend",
        "FileVersion": APP_VERSION,
        "InternalName": "spaceknife-backend",
        "LegalCopyright": "Copyright © 2026 DuskXi",
        "OriginalFilename": "spaceknife-backend.exe",
        "ProductName": "SpaceKnife",
        "ProductVersion": APP_VERSION,
    }
    version = VSVersionInfo(
        ffi=FixedFileInfo(filevers=nums, prodvers=nums),
        kids=[
            StringFileInfo([StringTable("040904B0", [StringStruct(k, v) for k, v in strings.items()])]),
            VarFileInfo([VarStruct("Translation", [0x0409, 1200])]),
        ],
    )

exe = EXE(
    pyz,
    a.scripts,
    [("X utf8", None, "OPTION")],  # = python -X utf8 (the ready line and logs are UTF-8 on every platform)
    exclude_binaries=True,
    name="spaceknife-backend",
    console=True,  # stdio pipes to Electron; started hidden (windowsHide / SW_HIDE)
    strip=False,  # strip breaks the libraries auditwheel patched (numpy's openblas: not page-aligned)
    upx=False,
    # Windows: the backend's icon in Task Manager. macOS would convert the .ico for nothing (the app
    # bundle's icon comes from electron-builder); Linux ignores it.
    icon=ROOT + "/apps/electron/build/icon.ico" if sys.platform == "win32" else None,
    version=version,
)
coll = COLLECT(exe, a.binaries, a.datas, name="spaceknife-backend", strip=False, upx=False)
