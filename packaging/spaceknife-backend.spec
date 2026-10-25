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
# the standard library plugins may import (spaceknife_sdk/imports.py; plugin check holds plugins to
# it): nothing of the app's own needs sqlite3, tarfile, zoneinfo …, so PyInstaller would leave them out
from spaceknife_sdk.imports import STDLIB

for _mod in sorted(STDLIB - {"__future__"}):
    hidden += collect_submodules(_mod) if _mod in ("xml", "email", "urllib", "concurrent", "asyncio") else [_mod]
# locales/*.json; the shipped plugins (plugins/builtin/<id>/: plugin.toml, rules, templates, and
# plugin.py, which is no Python package: the plugin loader imports it from the file)
datas = collect_data_files("spaceknife", excludes=["**/__pycache__/**"])
datas += collect_data_files(
    "spaceknife", includes=["plugins/builtin/**/*.py"], excludes=["**/__pycache__/**"], include_py_files=True
)
# the panel bridge copy (plugin build / plugin dev) and the author docs (spaceknife plugin docs)
datas += collect_data_files("spaceknife_sdk", excludes=["**/__pycache__/**"])
datas += [(ROOT + f"/docs/{doc}", "spaceknife_sdk/docs") for doc in ("plugin-quick.md", "plugin-dev.md", "plugin-api.md")]

a = Analysis(
    [SPECPATH + "/backend_entry.py"],
    # tools/build_package.py: the release build of spaceknife_native (build paths remapped) first
    pathex=[os.environ["SPACEKNIFE_NATIVE_RELEASE"]] if os.environ.get("SPACEKNIFE_NATIVE_RELEASE") else [],
    hiddenimports=hidden,
    datas=datas,
    # dev tools; pydantic.mypy (the mypy plugin) would pull in mypy itself. setuptools is a build tool
    # nothing imports at run time; collected anyway, it brought PyInstaller's runtime hook
    # pyi_rth_setuptools, which imported setuptools and its vendored packages at every start of the
    # backend: 130–230 ms of a command-line scan's start-up (docs/packaging.md, 命令行扫描的启动)
    excludes=[
        "tkinter",
        "_tkinter",
        "pytest",
        "hypothesis",
        "IPython",
        "PIL",
        "resvg_py",
        "pydantic.mypy",
        "mypy",
        "setuptools",
        "_distutils_hack",
        "pkg_resources",
        "distutils",
    ],
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
