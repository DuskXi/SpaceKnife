"""A synthetic directory tree with exactly known contents, for scan tests."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

FILES: dict[str, int] = {
    "a/one.bin": 1000,
    "a/two.bin": 5000,
    "a/sub/movie.mp4": 123456,
    "c.txt": 10,
    "中文 文件夹/照片.jpg": 2048,
    "deep/l1/l2/l3/l4/x.dat": 777,
    "empty.dat": 0,
}
FILES.update({f"many/f{i:03d}.log": 100 + i for i in range(300)})
EXTRA_DIRS = ["b", "b/inner"]


@dataclass(frozen=True)
class TreeFacts:
    root: Path
    files: int
    dirs: int  # excluding the root itself
    size: int


def build_tree(root: Path) -> TreeFacts:
    dirs: set[str] = set(EXTRA_DIRS)
    for rel, size in FILES.items():
        path = root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        with open(path, "wb") as fh:
            fh.write(b"x" * size)
        parent = Path(rel).parent
        while str(parent) not in (".", ""):
            dirs.add(parent.as_posix())
            parent = parent.parent
    for d in EXTRA_DIRS:
        (root / d).mkdir(parents=True, exist_ok=True)
    return TreeFacts(root=root, files=len(FILES), dirs=len(dirs), size=sum(FILES.values()))
