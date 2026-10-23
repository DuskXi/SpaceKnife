"""Codes and fixed numbers of the duplicate-file engine (docs/dupes.md).

Reason codes 0–13 are shared with the native module (``packages/native/src/dupes/codes.rs``);
the others are set by the Python stages only.
"""

from __future__ import annotations

from typing import Final

import numpy as np

from ..i18n import Texts

# ------------------------------------------------------------------ reasons (u1)
OK: Final = 0
GONE: Final = 1
DENIED: Final = 2
NOT_FILE: Final = 3
CLOUD: Final = 4
SURROGATE: Final = 5
AMBIGUOUS: Final = 6
CHANGED: Final = 7
IN_USE: Final = 8
ATIME: Final = 9
READ_ERROR: Final = 10
UNKNOWN_KEY: Final = 11
UNSUPPORTED: Final = 13
UNNAMEABLE: Final = 20  # the name cannot be passed to the OS as listed
STOPPED: Final = 21  # the job stopped (cancel, byte or time budget) before this one was decided
EXTERNAL: Final = 22  # more hard links than names in the scans: deleting this name frees nothing
NETWORK: Final = 23  # on a network or FUSE file system mounted below the root (not compared by default)

REASON_NAMES: Final[dict[int, str]] = {
    GONE: "gone",
    DENIED: "denied",
    NOT_FILE: "not_file",
    CLOUD: "cloud",
    SURROGATE: "surrogate",
    AMBIGUOUS: "ambiguous_name",
    CHANGED: "changed",
    IN_USE: "in_use",
    ATIME: "atime",
    READ_ERROR: "read_error",
    UNKNOWN_KEY: "read_error",
    UNSUPPORTED: "read_error",
    UNNAMEABLE: "unnameable",
    STOPPED: "stopped",
    EXTERNAL: "external_links",
    NETWORK: "network_mount",
}

# ------------------------------------------------------------------ candidate states (u1)
ST_SELECTED: Final = 0
ST_ALIAS: Final = 1  # another name of a physical file already in the group
ST_CLONE: Final = 2  # shares all its blocks with another member
ST_PROBED: Final = 3
ST_VERIFYING: Final = 4
ST_CONFIRMED: Final = 5
ST_UNIQUE: Final = 6  # found different from every other member (the size was shared, the content not)
ST_SKIPPED: Final = 7

# candidate flags (u1)
F_RACY: Final = 1  # modified shortly before it was read: its digest is not cached for joining
F_CACHE_HIT: Final = 2
F_NETWORK: Final = 4
F_PINNED: Final = 8  # a name Windows trims (trailing dot or space): may only be kept
F_NEEDED: Final = 16  # protected or needed by a program: always kept
F_VIEW: Final = 32  # members: the same single-named file seen by another path (overlapping scans, a
# symlinked folder, a bind mount): it carries its representative's role and is counted once

# ------------------------------------------------------------------ roles of verified members (u1)
ROLE_KEEP: Final = 1
ROLE_EXTRA: Final = 2
ROLE_ALIAS: Final = 3
ROLE_CLONE: Final = 4
ROLE_NEEDED: Final = 5
ROLE_PINNED: Final = 7
ROLE_NAMES: Final[dict[int, str]] = {
    ROLE_KEEP: "keep",
    ROLE_EXTRA: "extra",
    ROLE_ALIAS: "alias",
    ROLE_CLONE: "clone",
    ROLE_NEEDED: "needed",
    ROLE_PINNED: "pinned",
}

# why a keeper was kept (u1)
WHY_RULE: Final = 1
WHY_PREFER: Final = 2
WHY_FOLDER: Final = 3
WHY_VOLUME: Final = 4
WHY_NEEDED: Final = 5
WHY_MANUAL: Final = 6
WHY_PINNED: Final = 7
WHY_NAMES: Final[dict[int, str]] = {
    WHY_RULE: "rule",
    WHY_PREFER: "preferred",
    WHY_FOLDER: "folder",
    WHY_VOLUME: "volume",
    WHY_NEEDED: "needed",
    WHY_MANUAL: "manual",
    WHY_PINNED: "pinned",
}

# ------------------------------------------------------------------ frame column `dup` (u1)
DUP_NONE: Final = 0
DUP_PENDING: Final = 1
DUP_KEEP: Final = 2
DUP_EXTRA: Final = 3
DUP_SHARED: Final = 4
DUP_SKIPPED: Final = 5
DUP_UNIQUE: Final = 6  # compared: no other file has its contents (drawn like DUP_NONE)
# folders and blocks: share of the row's bytes that are extra copies
TIER_EDGES: Final = (0.10, 0.40, 0.80)

# ------------------------------------------------------------------ numbers
BLOCK: Final = 4096
MID_FROM: Final = 64 << 20
CHUNK: Final = 1 << 20
FIRST_CHECKPOINTS: Final = (1 << 20, 4 << 20, 16 << 20, 64 << 20, 256 << 20)
CHECKPOINT_STEP: Final = 256 << 20
LARGE_GROUP: Final = 32  # members read together; bigger groups go in rounds
DEFAULT_MIN_SIZE: Final = 1 << 20
RACY_S: Final = 10.0  # a file modified this shortly before it was read is not trusted from the cache

ALG_BLAKE3: Final = 1
ALG_BLAKE2B: Final = 2  # the Python fallback
PROBE_PARAMS: Final = 1

# ------------------------------------------------------------------ table layouts
# candidates: 40 bytes each (docs/dupes.md §6.1)
CAND_DTYPE: Final = np.dtype(
    {
        "names": [
            "node",
            "scan",
            "vol",
            "state",
            "reason",
            "nlink",
            "flags",
            "group",
            "fid_lo",
            "fid_hi",
            "quick",
        ],
        "formats": ["<i4", "u1", "u1", "u1", "u1", "<u2", "u1", "<i4", "<u8", "<u8", "<u8"],
        "offsets": [0, 4, 5, 6, 7, 8, 10, 12, 16, 24, 32],
        "itemsize": 40,
    }
)

# verified members: 64 bytes each (§6.2). ``role`` holds the role (low nibble) and why a keeper was
# kept (high nibble); ``unit`` is the member row of the physical file's representative (i4: rows of
# every group, not of one)
MEMBER_DTYPE: Final = np.dtype(
    {
        "names": [
            "fid_lo",
            "fid_hi",
            "mtime",
            "change",
            "usn",
            "alloc",
            "node",
            "group",
            "scan",
            "vol",
            "role",
            "flags",
            "unit",
        ],
        "formats": ["<u8", "<u8", "<i8", "<i8", "<i8", "<i8", "<i4", "<i4", "u1", "u1", "u1", "u1", "<i4"],
        "offsets": [0, 8, 16, 24, 32, 40, 48, 52, 56, 57, 58, 59, 60],
        "itemsize": 64,
    }
)


def checkpoints(size: int) -> list[int]:
    """Where S3 compares interim digests: 1, 4, 16, 64, 256 MiB, then every 256 MiB, then the end."""
    out = [c for c in FIRST_CHECKPOINTS if c < size]
    c = FIRST_CHECKPOINTS[-1] + CHECKPOINT_STEP
    while c < size:
        out.append(c)
        c += CHECKPOINT_STEP
    out.append(size)
    return out


def probe_ranges(size: int) -> list[tuple[int, int]]:
    """The byte ranges S2 reads (same as the native reader)."""
    r = [(0, min(size, BLOCK))]
    if size >= MID_FROM:
        for k in (1, 2):
            at = (size // 3 * k) & ~(BLOCK - 1)
            r.append((at, min(at + BLOCK, size)))
    t = max(0, size - 1) & ~(BLOCK - 1)
    r.append((t, size))
    return r


def probe_bytes(size: int) -> int:
    """Bytes S2 reads of a file of ``size`` bytes that is not read whole."""
    return sum(z - a for a, z in probe_ranges(size))


# User-facing text (zh-CN source, docs/i18n)
TEXT = Texts(
    {
        "gone": "已经不在了",
        "denied": "没有权限读取",
        "not_file": "已经不是普通文件",
        "cloud": "云占位文件（读取内容会触发下载，没有比较）",
        "surrogate": "这里现在是一个链接",
        "ambiguous_name": "有只差大小写的同名项，打开的可能是另一个",
        "changed": "核对期间文件变了",
        "in_use": "正被别的程序独占",
        "atime": "系统不让保留访问时间，没有读",
        "read_error": "读取出错",
        "unnameable": "名字不能用于系统调用",
        "stopped": "查找在核对它之前停止了",
        "external_links": "另有硬链接不在扫描里，删了不腾空间",
        "small": "小于下限",
        "system": "受保护或程序的文件夹",
        "network": "网络盘",
        "network_mount": "在网络盘或 FUSE 挂载上（默认不比较：ID 不可靠，读云盘会触发下载）",
        "bins": "回收站",
        "group_title_one": "{name}",
        "group_title_many": "{name} 等 {n} 个名字",
        "keep_rule": "按「{rule}」保留这一份",
        "keep_preferred": "在优先保留的位置 {path} 里",
        "keep_folder": "和同一对文件夹里的其他组留在同一边",
        "keep_volume": "每个卷至少留一份",
        "keep_needed": "程序需要或受保护",
        "keep_manual": "手动选择保留",
        "keep_pinned": "名字以点或空格结尾，删除篮删不了",
        "rule_oldest": "最早创建",
        "rule_newest": "最新修改",
        "rule_shortest": "路径最短",
    }
)
