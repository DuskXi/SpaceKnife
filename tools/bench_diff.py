"""M6 benchmark: snapshot compare of two large synthetic trees, built in memory (no disk).

    uv run python tools/bench_diff.py --nodes 8000000 [--reorder 0.05] [--fold] [--scatter]

The base tree mimics a system disk (deep nesting, a heavy tail of huge folders, mixed-case and a
few non-ASCII names); the target is the base a week later: files resized, entries deleted and
added, some names re-cased, some folders listed in another order. ``--reorder 1`` lists every
folder in another order (the slow path for every name), ``--scatter`` puts child ranges in random
order inside each level (as a live scan does). Prints the compare time, the peak of the memory it
allocates (tracemalloc) against one tree, and ``changes()`` latency. M6 target: 8M + 8M nodes in
10 s, about 2x the memory of one tree.

The generators are also used by the tests (``synth`` / ``mutate`` / ``to_store``).
"""

from __future__ import annotations

import argparse
import time
import tracemalloc
from dataclasses import dataclass

import numpy as np
from rich.console import Console
from rich.table import Table

from spaceknife.store.nodes import Flag, Kind, NodeStore
from spaceknife.store.subtree import ranges_to_ids

ALPHABET = np.frombuffer(b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-", dtype=np.uint8)
DIGITS = np.frombuffer(b"0123456789abcdefghijklmnopqrstuv", dtype=np.uint8)  # base 32
EXTS = [b"", b".dll", b".txt", b".jpg", b".png", b".log", b".json", b".js", b".py", b".dat", b".mp4"]
CJK = np.frombuffer("中".encode(), dtype=np.uint8)


@dataclass
class Tree:
    """A tree as plain arrays in logical order (parents before children; row 0 is the root)."""

    parent: np.ndarray  # i8, -1 for the root
    depth: np.ndarray  # i8, 1 for the root
    kind: np.ndarray  # u1
    size: np.ndarray  # i8
    flags: np.ndarray  # u4
    img: np.ndarray  # u8 names, anywhere in it
    pos: np.ndarray  # i8 start of each name in img
    lens: np.ndarray  # i8
    order: np.ndarray  # i8 sort key of a row among its siblings

    def __len__(self) -> int:
        return len(self.parent)


def _names(rng: np.random.Generator, rank: np.ndarray, is_dir: np.ndarray) -> tuple[np.ndarray, np.ndarray]:
    """Random names, unique among siblings: stem + "_" + rank in base 32 + extension."""
    n = len(rank)
    stem = rng.integers(2, 15, n)
    ext = np.where(is_dir, 0, rng.integers(0, len(EXTS), n))
    ext_len = np.array([len(e) for e in EXTS])[ext]
    wide = rng.random(n) < 0.01  # a CJK character in front
    lens = wide * len(CJK) + stem + 6 + ext_len
    width = int(lens.max()) if n else 0
    mat = ALPHABET[rng.integers(0, len(ALPHABET), (n, width))]
    rows = np.arange(n)
    col = np.where(wide, len(CJK), 0)
    for j in range(len(CJK)):
        mat[wide, j] = CJK[j]
    at = col + stem
    mat[rows, at] = ord("_")
    for j in range(5):
        mat[rows, at + 1 + j] = DIGITS[(rank >> (5 * (4 - j))) & 31]
    for e, text in enumerate(EXTS):
        sel = np.flatnonzero(ext == e)
        for j, ch in enumerate(text):
            mat[sel, at[sel] + 6 + j] = ch
    keep = np.arange(width)[None, :] < lens[:, None]
    return mat[keep], lens.astype(np.int64)


def synth(nodes: int, seed: int = 7) -> Tree:
    """A synthetic disk of exactly ``nodes`` entries (root included), generated level by level."""
    rng = np.random.default_rng(seed)
    parents = [np.array([-1])]
    depths = [np.array([1])]
    kinds = [np.array([Kind.DIR], dtype=np.uint8)]
    ranks = [np.array([0])]
    frontier = np.array([0])
    total = 1
    depth = 1
    while frontier.size and total < nodes:
        k = len(frontier)
        dirs = rng.poisson(3.6 if depth < 7 else 1.3, k) if depth < 24 else np.zeros(k, dtype=np.int64)
        files = np.minimum((rng.pareto(1.2, k) * 5).astype(np.int64), 30_000)
        count = dirs + files
        count = np.clip(nodes - total - (np.cumsum(count) - count), 0, count)  # stop at ``nodes``
        dirs = np.minimum(dirs, count)
        if not count.any():
            break
        rank = ranges_to_ids(np.zeros(k, dtype=np.int64), count)
        is_dir = rank < np.repeat(dirs, count)
        first = total
        parents.append(np.repeat(frontier, count))
        depths.append(np.full(int(count.sum()), depth + 1))
        kinds.append(np.where(is_dir, Kind.DIR, Kind.FILE).astype(np.uint8))
        ranks.append(rank)
        total += int(count.sum())
        frontier = first + np.flatnonzero(is_dir)
        depth += 1
    parent, kind, rank = np.concatenate(parents), np.concatenate(kinds), np.concatenate(ranks)
    n = len(parent)
    img, lens = _names(rng, rank, kind == Kind.DIR)
    size = np.where(kind == Kind.FILE, np.exp(rng.normal(9, 2.5, n)).astype(np.int64), 0)
    flags = np.where(rng.random(n) < 0.03, np.uint32(Flag.HIDDEN), np.uint32(0)).astype(np.uint32)
    return Tree(
        parent=parent,
        depth=np.concatenate(depths),
        kind=kind,
        size=size,
        flags=flags,
        img=img,
        pos=np.cumsum(lens) - lens,
        lens=lens,
        order=np.arange(n, dtype=np.int64),
    )


def mutate(
    base: Tree,
    seed: int = 8,
    *,
    resize: float = 0.02,
    delete: float = 0.005,
    add: float = 0.005,
    recase: float = 0.001,
    reorder: float = 0.05,
) -> Tree:
    """The base a while later: shares of files resized, entries deleted (with their subtrees) and
    added, names re-cased (first letter), folders whose children are listed in another order."""
    rng = np.random.default_rng(seed)
    n = len(base)
    dead = rng.random(n) < delete
    dead[0] = False
    for d in range(2, int(base.depth.max()) + 1):  # a deleted folder takes its subtree along
        at = np.flatnonzero(base.depth == d)
        dead[at] |= dead[base.parent[at]]
    live = np.flatnonzero(~dead)
    new_id = np.full(n, -1, dtype=np.int64)
    new_id[live] = np.arange(len(live))
    size = base.size[live].copy()
    files = np.flatnonzero((base.kind[live] == Kind.FILE) & (rng.random(len(live)) < resize))
    size[files] = (size[files] * rng.lognormal(0.3, 1.0, len(files))).astype(np.int64)
    # re-cased names: copies with the first byte's ASCII case flipped
    pos, lens = base.pos[live].copy(), base.lens[live].copy()
    first = base.img[pos]
    letter = ((first | 0x20) >= ord("a")) & ((first | 0x20) <= ord("z"))
    flip = np.flatnonzero(letter & (rng.random(len(live)) < recase))
    extra = [base.img]
    if flip.size:
        copy = base.img[ranges_to_ids(pos[flip], lens[flip])].copy()
        starts = np.cumsum(lens[flip]) - lens[flip]
        copy[starts] ^= 0x20
        pos[flip] = len(base.img) + starts
        extra.append(copy)
    # new entries: files (and a few folders) under random live folders
    folders = np.flatnonzero(base.kind[live] == Kind.DIR)
    m = int(add * n)
    at_parent = folders[rng.integers(0, len(folders), m)] if m and folders.size else np.empty(0, np.int64)
    m = len(at_parent)
    rank = (1 << 24) + np.arange(m)  # base-32 ranks no original sibling has
    is_dir = rng.random(m) < 0.05
    img, new_lens = _names(rng, rank, is_dir)
    new_pos = sum(len(x) for x in extra) + np.cumsum(new_lens) - new_lens
    extra.append(img)
    parent = np.concatenate(
        [np.where(base.parent[live] >= 0, new_id[np.maximum(base.parent[live], 0)], -1), at_parent]
    )
    depth = np.concatenate([base.depth[live], base.depth[live][at_parent] + 1])
    kind = np.concatenate([base.kind[live], np.where(is_dir, Kind.DIR, Kind.FILE).astype(np.uint8)])
    new_size = np.where(is_dir, 0, np.exp(rng.normal(9, 2.5, m)).astype(np.int64))
    # another listing order in some folders; new entries anywhere among their siblings
    order = np.concatenate([np.arange(len(live)), rng.integers(0, len(live) + 1, m)])
    shuffled = rng.random(len(parent)) < reorder
    noise = rng.integers(0, 1 << 40, len(parent))
    order = np.where(shuffled[np.maximum(parent, 0)], noise, order)
    return Tree(
        parent=parent,
        depth=depth,
        kind=kind,
        size=np.concatenate([size, new_size]),
        flags=np.concatenate([base.flags[live], np.zeros(m, dtype=np.uint32)]),
        img=np.concatenate(extra),
        pos=np.concatenate([pos, new_pos]),
        lens=np.concatenate([lens, new_lens]),
        order=order,
    )


def to_store(tree: Tree, root: str = "C:\\", *, scatter: bool = False, seed: int = 9) -> NodeStore:
    """Lay ``tree`` out breadth first (siblings by ``order``) as a NodeStore; with ``scatter`` the
    child ranges of each level come in random order, as in a live scan."""
    rng = np.random.default_rng(seed)
    n = len(tree)
    new_id = np.zeros(n, dtype=np.int64)
    new_id[0] = 1  # row 0 of the store is the virtual top
    order_parts = [np.array([0])]
    nxt = 2
    for d in range(2, int(tree.depth.max()) + 1):
        at = np.flatnonzero(tree.depth == d)
        par = new_id[tree.parent[at]]
        block = rng.permutation(nxt + 1)[par] if scatter else par  # parents' blocks shuffled
        at = at[np.lexsort((tree.order[at], block))]
        new_id[at] = nxt + np.arange(len(at))
        order_parts.append(at)
        nxt += len(at)
    order = np.concatenate(order_parts)
    rows = len(order) + 1
    cols: dict[str, np.ndarray] = {}
    parent = np.full(rows, -1, dtype=np.int64)
    parent[1:] = np.where(tree.parent[order] >= 0, new_id[np.maximum(tree.parent[order], 0)], 0)
    cols["parent"] = parent
    depth = np.zeros(rows, dtype=np.int64)
    depth[1:] = tree.depth[order]
    cols["depth"] = depth
    kind = np.full(rows, Kind.DIR, dtype=np.uint8)
    kind[1:] = tree.kind[order]
    cols["kind"] = kind
    flags = np.zeros(rows, dtype=np.uint32)
    flags[0] = Flag.VIRTUAL
    flags[1:] = tree.flags[order]
    cols["flags"] = flags
    size = np.zeros(rows, dtype=np.int64)
    size[1:] = tree.size[order]
    cols["size"] = size
    cols["alloc"] = (size + 4095) // 4096 * 4096
    count = np.bincount(parent[1:], minlength=rows)
    p = parent[1:]
    head = np.ones(len(p), dtype=bool)  # first row of each parent's (contiguous) block
    head[1:] = p[1:] != p[:-1]
    starts = np.zeros(rows, dtype=np.int64)
    starts[p[head]] = np.flatnonzero(head) + 1
    cols["child_start"] = starts
    cols["child_count"] = count
    # names: the root's path, then every name in store order (back to back)
    root_name = np.frombuffer(root.encode(), dtype=np.uint8)
    lens = np.zeros(rows, dtype=np.int64)
    lens[1] = len(root_name)
    lens[2:] = tree.lens[order[1:]]
    names = np.concatenate([root_name, tree.img[ranges_to_ids(tree.pos[order[1:]], tree.lens[order[1:]])]])
    cols["name_len"] = lens
    cols["name_off"] = np.cumsum(lens) - lens
    store = NodeStore.from_columns(cols, names, [""])
    store.reaggregate()
    return store


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--nodes", type=int, default=8_000_000)
    ap.add_argument("--reorder", type=float, default=0.05, help="share of folders listed in another order")
    ap.add_argument("--fold", action="store_true", help="Windows rule (case-insensitive names)")
    ap.add_argument("--scatter", action="store_true", help="child ranges in random order (live scan)")
    ap.add_argument("--repeat", type=int, default=2)
    args = ap.parse_args()

    from spaceknife.diff import compare

    console = Console()
    t = time.perf_counter()
    base_tree = synth(args.nodes)
    target_tree = mutate(base_tree, reorder=args.reorder)
    base = to_store(base_tree, scatter=args.scatter, seed=1)
    target = to_store(target_tree, scatter=args.scatter, seed=2)
    del base_tree, target_tree
    console.print(f"built {len(base):,} + {len(target):,} rows in {time.perf_counter() - t:.1f} s")

    best = float("inf")
    peak = 0
    tree = None
    for _ in range(args.repeat):
        tree = None
        tracemalloc.start()
        t = time.perf_counter()
        tree = compare(base, target, fold_case=args.fold)
        best = min(best, time.perf_counter() - t)
        peak = max(peak, tracemalloc.get_traced_memory()[1])
        tracemalloc.stop()
    assert tree is not None
    t = time.perf_counter()
    grow = tree.changes("grow", 100)
    shrink = tree.changes("shrink", 100)
    changes_ms = (time.perf_counter() - t) * 1000 / 2

    one = base.memory_bytes()
    status = np.bincount(tree.status, minlength=4)
    table = Table(title=f"compare: {args.nodes:,} nodes, reorder {args.reorder}, fold {args.fold}")
    table.add_column("measure")
    table.add_column("value", justify="right")
    table.add_row("compare (best)", f"{best:.2f} s")
    table.add_row("union rows", f"{len(tree):,}")
    table.add_row("unchanged / added / removed / changed", " / ".join(f"{int(x):,}" for x in status))
    table.add_row("one tree (base store)", f"{one / 2**20:,.0f} MB")
    table.add_row("result (union store + arrays)", f"{tree.memory_bytes() / 2**20:,.0f} MB")
    table.add_row("peak allocated by compare", f"{peak / 2**20:,.0f} MB ({peak / one:.2f}x one tree)")
    table.add_row("changes(100)", f"{changes_ms:.1f} ms")
    console.print(table)
    for c in grow[:5] + shrink[:5]:
        console.print(f"  {c.path}{' (rest)' if c.rest else ''}: {c.amount:,} ({c.status.name})")


if __name__ == "__main__":
    main()
