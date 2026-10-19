"""Synthetic snapshots for screenshots and demos: a made-up 2 TB data drive, no real disk data.

    uv run python tools/demo/mock_snapshot.py --home DIR

Writes two snapshots of the same fictional volume ``S:\\`` (label "Data") into DIR's snapshot
folder, as if it had been scanned a month apart: ``2026-08-20`` (base) and ``2026-09-24`` (now).
Between them a video project and a photo month were added, a game was updated and another one
uninstalled, a model checkpoint folder grew, an ISO and an old phone backup were deleted, so a
snapshot comparison has something to show. Everything (names, sizes, dates) comes from a seeded
random generator: the same files every run. Prints the two paths, newest last.
"""

from __future__ import annotations

import argparse
import os
import random
import sys
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

import numpy as np

from spaceknife.store import snapshot
from spaceknife.store.nodes import ENTRY_COLUMNS, Kind, NodeStore

GB = 1 << 30
MB = 1 << 20
KB = 1 << 10
ROOT = "S:\\"  # a letter that is rarely mounted: the start page matches records to live drives
CLUSTER = 4096
TOTAL = 2_000_396_746_752  # a "2 TB" drive
BASE_DAY = datetime(2026, 8, 20, 21, 14, tzinfo=UTC)
NOW_DAY = datetime(2026, 9, 24, 22, 3, tzinfo=UTC)

# A file is (size, mtime); a folder is a dict of name -> file or folder.
Tree = dict[str, Any]


def ts(year: int, month: int, day: int = 15) -> int:
    return int(datetime(year, month, min(day, 28), 12, tzinfo=UTC).timestamp())


class Gen:
    def __init__(self, seed: int) -> None:
        self.r = random.Random(seed)

    def size(self, lo: float, hi: float) -> int:
        """Log-uniform between lo and hi bytes."""
        return int(np.exp(self.r.uniform(np.log(lo), np.log(hi))))

    def when(self, y0: int = 2019, y1: int = 2026) -> int:
        y = self.r.randint(y0, y1)
        return ts(y, self.r.randint(1, 12 if y < 2026 else 8), self.r.randint(1, 28))

    # ------------------------------------------------------------------ areas

    def game(self, title: str, total: float, year: int) -> Tree:
        t = ts(year, self.r.randint(1, 8))
        paks = {
            f"pakchunk{i}-WindowsNoEditor.pak": (int(total * w), t) for i, w in enumerate(self.split(0.78, 6))
        }
        movies = {
            f"{n}.bk2": (self.size(80 * MB, 900 * MB), t)
            for n in ("Intro", "Credits", "Act1", "Act2", "Finale")
        }
        binaries = {f"{title.replace(' ', '')}-Win64-Shipping.exe": (self.size(90 * MB, 260 * MB), t)}
        binaries |= {
            f"{lib}.dll": (self.size(200 * KB, 60 * MB), t)
            for lib in (
                "steam_api64",
                "bink2w64",
                "dxil",
                "amd_fidelityfx_dx12",
                "nvngx_dlss",
                "sl.interposer",
                "PhysX3_x64",
                "tbb12",
            )
        }
        engine = {
            "Shaders": {
                f"ShaderArchive-{i:03d}.ushaderbytecode": (self.size(2 * MB, 140 * MB), t) for i in range(40)
            },
            "Localization": {
                lang: {"Game.locres": (self.size(200 * KB, 4 * MB), t)}
                for lang in ("en", "de", "fr", "ja", "ko", "zh-Hans", "es", "pt-BR")
            },
            "Config": {
                f"{n}.ini": (self.size(1 * KB, 40 * KB), t)
                for n in ("Default", "Engine", "Game", "Input", "Scalability")
            },
        }
        return {
            "Content": {"Paks": paks, "Movies": movies},
            "Binaries": {"Win64": binaries},
            "Engine": engine,
            "redist": {"vc_redist.x64.exe": (25 * MB, t), "DXSETUP.exe": (520 * KB, t)},
        }

    def split(self, share: float, n: int) -> list[float]:
        w = [self.r.random() ** 2 + 0.05 for _ in range(n)]
        s = sum(w)
        return [share * x / s for x in w]

    def footage(self, n: int, year: int, month: int, prefix: str = "C") -> Tree:
        t = ts(year, month, self.r.randint(1, 28))
        return {f"{prefix}{i:04d}.MP4": (self.size(700 * MB, 6 * GB), t) for i in range(1, n + 1)}

    def video_project(self, name: str, year: int, month: int, clips: int) -> Tree:
        t = ts(year, month, 27)
        return {
            "Footage": self.footage(clips, year, month),
            "Proxies": {
                f"C{i:04d}_Proxy.mov": (self.size(60 * MB, 400 * MB), t) for i in range(1, clips + 1)
            },
            "Audio": {f"ambience-{i:02d}.wav": (self.size(30 * MB, 600 * MB), t) for i in range(12)},
            "Exports": {
                f"{name.lower().replace(' ', '-')}-final-4k.mp4": (self.size(6 * GB, 14 * GB), t),
                f"{name.lower().replace(' ', '-')}-teaser.mp4": (self.size(300 * MB, 900 * MB), t),
            },
            f"{name}.prproj": (self.size(20 * MB, 120 * MB), t),
            "Cache": {f"media-cache-{i:03d}.cfa": (self.size(4 * MB, 260 * MB), t) for i in range(60)},
        }

    def photo_month(self, year: int, month: int) -> Tree:
        out: Tree = {}
        for _ in range(self.r.randint(30, 140)):
            t = ts(year, month, self.r.randint(1, 28))
            n = self.r.randint(1000, 9999)
            out[f"DSC{n:05d}.ARW"] = (self.size(22 * MB, 48 * MB), t)
            if self.r.random() < 0.7:
                out[f"DSC{n:05d}.JPG"] = (self.size(4 * MB, 14 * MB), t)
        if self.r.random() < 0.35:
            out["Edited"] = {
                f"edit-{i:03d}.jpg": (self.size(6 * MB, 30 * MB), ts(year, month, 28))
                for i in range(self.r.randint(5, 40))
            }
        return out

    def music(self) -> Tree:
        first = [
            "Velvet",
            "Northern",
            "Paper",
            "Glass",
            "Silent",
            "Electric",
            "Crimson",
            "Lunar",
            "Golden",
            "Hollow",
            "Neon",
            "Wild",
        ]
        second = [
            "Harbor",
            "Lights",
            "Engines",
            "Tides",
            "Orchard",
            "Canyon",
            "Atlas",
            "Signal",
            "Meadow",
            "Static",
            "Parade",
            "Avenue",
        ]
        words = [
            "Night",
            "Ocean",
            "Drive",
            "Echoes",
            "Summer",
            "Paper",
            "Gravity",
            "Fireflies",
            "Horizon",
            "Static",
            "Rain",
            "Home",
        ]
        out: Tree = {}
        for _ in range(60):
            artist = f"{self.r.choice(first)} {self.r.choice(second)}"
            albums: Tree = out.setdefault(artist, {})
            for _ in range(self.r.randint(1, 4)):
                album = f"{self.r.choice(words)} {self.r.choice(words)} ({self.r.randint(2004, 2026)})"
                t = self.when(2016, 2026)
                tracks = {
                    f"{k:02d} - {self.r.choice(words)} {self.r.choice(words)}.flac": (
                        self.size(18 * MB, 70 * MB),
                        t,
                    )
                    for k in range(1, self.r.randint(8, 15))
                }
                tracks["cover.jpg"] = (self.size(300 * KB, 3 * MB), t)
                albums[album] = tracks
        return out

    def node_modules(self, depth: int = 0) -> Tree:
        names = [
            "react",
            "react-dom",
            "scheduler",
            "typescript",
            "vite",
            "esbuild",
            "rollup",
            "postcss",
            "eslint",
            "lodash",
            "zod",
            "date-fns",
            "@babel",
            "@types",
            "webpack",
            "terser",
            "sass",
            "chalk",
            "commander",
            "semver",
            "acorn",
            "debug",
            "ms",
            "tslib",
            "core-js",
            "caniuse-lite",
            "browserslist",
            "picocolors",
            "nanoid",
            "glob",
        ]
        out: Tree = {}
        for name in self.r.sample(names, k=min(len(names), 26 if depth == 0 else 4)):
            t = self.when(2025, 2026)
            pkg: Tree = {
                "package.json": (self.size(600, 8 * KB), t),
                "README.md": (self.size(1 * KB, 40 * KB), t),
                "LICENSE": (1100, t),
            }
            pkg["dist"] = {
                f"{name.strip('@')}{s}": (self.size(2 * KB, 3 * MB), t)
                for s in (".js", ".cjs", ".mjs", ".d.ts", ".min.js", ".js.map")
            }
            pkg["lib"] = {
                f"module{i}.js": (self.size(400, 90 * KB), t) for i in range(self.r.randint(10, 80))
            }
            if name in ("typescript", "esbuild", "@babel", "core-js") and depth == 0:
                pkg["lib"] |= {f"chunk{i}.js": (self.size(40 * KB, 9 * MB), t) for i in range(120)}
            if depth < 1 and self.r.random() < 0.3:
                pkg["node_modules"] = self.node_modules(depth + 1)
            out[name] = pkg
        return out

    def rust_target(self) -> Tree:
        t = self.when(2026, 2026)
        deps = {
            f"lib{c}-{self.r.getrandbits(64):016x}.rlib": (self.size(200 * KB, 90 * MB), t)
            for c in ("serde", "tokio", "rayon", "regex", "syn", "hyper", "clap", "image", "wgpu", "naga")
            for _ in range(6)
        }
        inc = {
            f"{c}-{i:03x}": {
                f"s-{j}.o": (self.size(20 * KB, 6 * MB), t) for j in range(self.r.randint(5, 30))
            }
            for c in ("core", "app", "render")
            for i in range(12)
        }
        return {
            "debug": {
                "deps": deps,
                "incremental": inc,
                "build": {
                    f"{c}-{i}": {"out": {"output.o": (self.size(1 * MB, 40 * MB), t)}}
                    for c in ("ring", "zstd-sys", "openssl-sys")
                    for i in range(3)
                },
            },
            "release": {"deps": {k: (v[0] // 2, v[1]) for k, v in list(deps.items())[:30]}},
        }

    def repo(self, name: str, kind: str) -> Tree:
        t = self.when(2025, 2026)
        out: Tree = {
            ".git": {
                "objects": {
                    "pack": {
                        f"pack-{self.r.getrandbits(64):016x}.pack": (self.size(20 * MB, 1.6 * GB), t)
                        for _ in range(self.r.randint(1, 4))
                    }
                },
                "index": (self.size(100 * KB, 6 * MB), t),
            },
            "src": {
                f"{w}.{kind}": (self.size(600, 60 * KB), t)
                for w in (
                    "main",
                    "app",
                    "config",
                    "router",
                    "store",
                    "utils",
                    "api",
                    "models",
                    "views",
                    "tests",
                )
            },
            "README.md": (self.size(1 * KB, 20 * KB), t),
        }
        if kind in ("ts", "js"):
            out["node_modules"] = self.node_modules()
            out["dist"] = {f"assets-{i}.js": (self.size(20 * KB, 2 * MB), t) for i in range(20)}
        if kind == "rs":
            out["target"] = self.rust_target()
        return out

    # ------------------------------------------------------------------ the drive

    def drive(self) -> Tree:
        """The drive on the base day."""
        r = self.r
        games = {
            "Starfall Odyssey": self.game("Starfall Odyssey", 96 * GB, 2025),
            "Iron Frontier 2": self.game("Iron Frontier 2", 74 * GB, 2024),
            "Neon Drift": self.game("Neon Drift", 38 * GB, 2025),
            "Hollow Keep": self.game("Hollow Keep", 12 * GB, 2023),
            "Skyward Tactics": self.game("Skyward Tactics", 51 * GB, 2026),
            "Abyssal Tide": self.game("Abyssal Tide", 63 * GB, 2025),
        }
        steam = {
            "steamapps": {
                "common": games,
                "shadercache": {
                    str(r.randint(100000, 3000000)): {
                        "fozpipelines": {
                            "steam_pipeline_cache.foz": (self.size(80 * MB, 1.2 * GB), self.when(2025, 2026))
                        }
                    }
                    for _ in range(14)
                },
                "workshop": {
                    "content": {
                        str(r.randint(10**6, 9 * 10**6)): {
                            f"item{i}.vpk": (self.size(5 * MB, 600 * MB), self.when(2024, 2026))
                            for i in range(r.randint(1, 6))
                        }
                        for _ in range(30)
                    }
                },
            }
        }

        videos: Tree = {
            "Projects": {
                "2025 Coastline Trip": self.video_project("Coastline Trip", 2025, 8, 38),
                "2026 Spring City Walk": self.video_project("Spring City Walk", 2026, 4, 22),
            },
            "Drone": {f"2026-0{m}": self.footage(r.randint(6, 14), 2026, m, "DJI_") for m in (3, 5, 6)},
            "Screen Recordings": {
                f"recording-2026-0{r.randint(1, 8)}-{i:02d}.mp4": (
                    self.size(40 * MB, 2 * GB),
                    self.when(2026, 2026),
                )
                for i in range(36)
            },
            "Movies": {
                f"{w}.mkv": (self.size(3 * GB, 14 * GB), self.when(2020, 2025))
                for w in (
                    "The Long Harbor",
                    "Glass Orchard",
                    "Northern Signal",
                    "Paper Atlas",
                    "Electric Meadow",
                    "Silent Canyon",
                    "Lunar Parade",
                )
            },
        }

        photos: Tree = {}
        for year in range(2019, 2027):
            for month in range(1, 13):
                if year == 2026 and month > 8:
                    break
                photos.setdefault(str(year), {})[f"{year}-{month:02d}"] = self.photo_month(year, month)
        photos["Lightroom Catalog"] = {
            "Catalog.lrcat": (self.size(1.5 * GB, 3 * GB), ts(2026, 8, 20)),
            "Catalog Previews.lrdata": {
                f"{i:X}": {
                    f"preview-{j}.lrprev": (self.size(200 * KB, 4 * MB), self.when(2024, 2026))
                    for j in range(r.randint(20, 80))
                }
                for i in range(16)
            },
        }

        ml = self.repo("ml-lab", "py")
        ml["data"] = {
            f"train-{i:05d}-of-00048.parquet": (self.size(600 * MB, 2.4 * GB), ts(2026, 6)) for i in range(48)
        }
        ml["checkpoints"] = {f"step-{s}k.safetensors": (14 * GB, ts(2026, 7)) for s in (10, 20)}
        dev = {
            "web-shop": self.repo("web-shop", "ts"),
            "dashboard": self.repo("dashboard", "ts"),
            "landing-page": self.repo("landing-page", "js"),
            "rust-tools": self.repo("rust-tools", "rs"),
            "game-mods": self.repo("game-mods", "rs"),
            "ml-lab": ml,
            "notes": {
                f"{w}.md": (self.size(1 * KB, 60 * KB), self.when(2024, 2026))
                for w in ("ideas", "todo", "reading", "setup", "commands", "meetings")
            },
        }

        vms = {
            "Windows 11 Dev": {
                "Windows 11 Dev.vhdx": (96 * GB, ts(2026, 8, 12)),
                "Windows 11 Dev.vmcx": (60 * KB, ts(2026, 8)),
            },
            "Ubuntu Server 24.04": {
                "Ubuntu Server 24.04.vdi": (42 * GB, ts(2026, 7)),
                "Logs": {f"VBox.log.{i}": (self.size(100 * KB, 2 * MB), ts(2026, 7)) for i in range(4)},
            },
            "Fedora Workstation": {"fedora.qcow2": (28 * GB, ts(2026, 5))},
        }

        backups: Tree = {
            "Phone Backup 2026-03.zip": (64 * GB, ts(2026, 3)),
            "System Image 2025-12": {
                "WindowsImageBackup": {
                    "Backup 2025-12-28": {
                        f"{r.getrandbits(128):032x}.vhdx": (self.size(40 * GB, 120 * GB), ts(2025, 12, 28))
                        for _ in range(2)
                    }
                }
            },
            "Documents Archive 2024.7z": (12 * GB, ts(2025, 1)),
        }

        downloads: Tree = {
            "ubuntu-24.04.3-desktop-amd64.iso": (int(6.1 * GB), ts(2026, 8, 9)),
            "Win11_24H2_x64.iso": (int(5.8 * GB), ts(2026, 2)),
            "fedora-workstation-42-x86_64.iso": (int(2.3 * GB), ts(2026, 5)),
        }
        downloads |= {
            f"setup-{w}-{r.randint(1, 9)}.{r.randint(0, 20)}.exe": (
                self.size(20 * MB, 900 * MB),
                self.when(2025, 2026),
            )
            for w in ("editor", "studio", "player", "drivers", "toolkit", "client", "launcher", "sdk")
        }
        downloads |= {
            f"{w}.zip": (self.size(5 * MB, 4 * GB), self.when(2024, 2026))
            for w in ("assets-pack", "fonts", "sample-project", "textures-4k", "soundbank", "dataset-mini")
        }
        downloads |= {
            f"document-{i:03d}.pdf": (self.size(100 * KB, 40 * MB), self.when(2023, 2026)) for i in range(80)
        }

        documents = {
            "Work": {
                f"{w}-{i:02d}.{e}": (self.size(20 * KB, 30 * MB), self.when(2021, 2026))
                for w in ("report", "plan", "budget", "slides", "minutes")
                for i in range(40)
                for e in (r.choice(("docx", "xlsx", "pptx", "pdf")),)
            },
            "Scans": {
                f"scan-{i:04d}.pdf": (self.size(200 * KB, 12 * MB), self.when(2019, 2026)) for i in range(300)
            },
            "Books": {
                f"book-{i:03d}.epub": (self.size(400 * KB, 30 * MB), self.when(2019, 2026))
                for i in range(120)
            },
        }

        return {
            "Games": {"SteamLibrary": steam},
            "Videos": videos,
            "Photos": photos,
            "Music": self.music(),
            "Dev": dev,
            "VMs": vms,
            "Backups": backups,
            "Downloads": downloads,
            "Documents": documents,
        }

    def month_later(self, d: Tree) -> Tree:
        """The same drive five weeks on (a separate generator, so the base stays identical)."""
        games = d["Games"]["SteamLibrary"]["steamapps"]["common"]
        del games["Hollow Keep"]  # uninstalled
        games["Starfall Odyssey"]["Content"]["Paks"]["pakchunk6-WindowsNoEditor.pak"] = (
            12 * GB,
            ts(2026, 9, 18),
        )
        d["Videos"]["Projects"]["2026 Mountain Weekend"] = self.video_project("Mountain Weekend", 2026, 9, 34)
        d["Photos"]["2026"]["2026-09"] = self.photo_month(2026, 9)
        d["Photos"]["Lightroom Catalog"]["Catalog.lrcat"] = (int(2.9 * GB), ts(2026, 9, 22))
        d["Dev"]["ml-lab"]["checkpoints"] |= {
            f"step-{s}k.safetensors": (14 * GB, ts(2026, 9, s // 3)) for s in (30, 40, 50)
        }
        d["Dev"]["web-shop"]["node_modules"] = self.node_modules()  # reinstalled
        d["VMs"]["Windows 11 Dev"]["Windows 11 Dev.vhdx"] = (104 * GB, ts(2026, 9, 20))
        backups = d["Backups"]
        del backups["Phone Backup 2026-03.zip"]
        backups["Phone Backup 2026-09.zip"] = (71 * GB, ts(2026, 9, 21))
        downloads = d["Downloads"]
        del downloads["Win11_24H2_x64.iso"]
        downloads["driver-pack-2026.09.exe"] = (int(1.2 * GB), ts(2026, 9, 3))
        return d


def build_store(tree: Tree) -> NodeStore:
    store = NodeStore()
    root = store.add_root(ROOT)
    queue: list[tuple[int, Tree]] = [(root, tree)]
    while queue:
        node, children = queue.pop()
        names = sorted(children)
        n = len(names)
        entries = {k: np.zeros(n, dtype=np.int64) for k in ENTRY_COLUMNS}
        exts = np.zeros(n, dtype=np.uint16)
        for i, name in enumerate(names):
            value = children[name]
            entries["nlink"][i] = 1
            if isinstance(value, dict):
                entries["kind"][i] = Kind.DIR
                newest = max((_mtime(v) for v in value.values()), default=ts(2024, 1))
                entries["mtime"][i] = entries["ctime"][i] = entries["atime"][i] = newest
            else:
                size, mtime = value
                entries["kind"][i] = Kind.FILE
                entries["size"][i] = size
                entries["alloc"][i] = -(-size // CLUSTER) * CLUSTER
                entries["mtime"][i] = entries["ctime"][i] = entries["atime"][i] = mtime
                dot = name.rfind(".")
                exts[i] = store.ext_id(name[dot + 1 :].lower() if dot > 0 and len(name) - dot <= 17 else "")
        first = store.ingest_names(node, [nm.encode() for nm in names], entries, exts)
        for i, name in enumerate(names):
            if isinstance(children[name], dict):
                queue.append((first + i, children[name]))
    return store


def _mtime(v: Any) -> int:
    return max((_mtime(x) for x in v.values()), default=0) if isinstance(v, dict) else int(v[1])


class _At(datetime):
    """``datetime.now()`` fixed at the snapshot's day (snapshot.save stamps created_at with it)."""

    at: datetime = NOW_DAY

    @classmethod
    def now(cls, tz: Any = None) -> datetime:  # type: ignore[override]
        return cls.at


def write(home: Path, when: datetime, now: bool) -> Path:
    tree = Gen(seed=7).drive()
    if now:
        tree = Gen(seed=8).month_later(tree)
    store = build_store(tree)
    alloc = int(store.cols["tot_alloc"][0])
    unknown = 9 * GB  # system files and metadata the scan does not see
    volume = {
        "mount_point": ROOT,
        "device": "\\\\?\\Volume{00000000-0000-0000-0000-000000000000}\\",
        "fs_type": "NTFS",
        "label": "Data",
        "total": TOTAL,
        "free": TOTAL - alloc - unknown,
        "cluster_size": CLUSTER,
        "is_remote": False,
        "is_removable": False,
        "used": alloc + unknown,
    }
    stats = {
        "roots": [{"path": ROOT, "whole_volume": True}],
        "method": "mft",
        "state": "done",
        "files": int(store.cols["tot_files"][0]),
        "bytes_alloc": alloc,
        "elapsed_s": 6.8 if now else 7.1,
        "started_at": when.replace(second=0).isoformat(),
        "finished_at": when.replace(second=7).isoformat(),
        "errors": 0,
    }
    path = snapshot.auto_path([ROOT], when=when, base=home / "data" / "snapshots")
    _At.at = when
    real = snapshot.datetime  # type: ignore[attr-defined]
    snapshot.datetime = _At  # type: ignore[attr-defined]
    try:
        snapshot.save(store, path, roots=[ROOT], volumes=[volume], stats=stats)
    finally:
        snapshot.datetime = real  # type: ignore[attr-defined]
    os.utime(path, (when.timestamp(), when.timestamp()))
    print(f"{path}  {len(store) - 1:,} items, {alloc / GB:,.0f} GB")
    return path


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument(
        "--home", required=True, type=Path, help="SPACEKNIFE_HOME of the demo / screenshot session"
    )
    args = ap.parse_args()
    write(args.home, BASE_DAY, now=False)
    write(args.home, NOW_DAY, now=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
