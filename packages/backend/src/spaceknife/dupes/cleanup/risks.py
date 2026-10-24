"""The risk catalogue (docs/dupes.md §16.6.3) as one table, evaluated over arrays.

Each entry names the actions it concerns and a function of the plan's arrays giving, per copy, its
severity (0 when the evidence is not there). There is no risk without evidence: the wording quotes
it (「这份在 OneDrive 的同步文件夹 C:\\Users\\x\\OneDrive 里」), filled in only for the copies shown.
General notes without per-copy evidence are in the action's description, once (§16.6.5).

Levels: info · low ○ · medium ■ · high ▲ (acknowledged per kind before running) · blocked ⊘ (the
action is not offered; that lives in ``caps``).
"""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass, field

import numpy as np

from ...i18n import Texts
from . import codes as K

TRASH, HARD, SYM, CLONE = K.A_TRASH, K.A_HARDLINK, K.A_SYMLINK, K.A_CLONE
REPL = frozenset({HARD, SYM})
ANY = frozenset({TRASH, HARD, SYM, CLONE})


@dataclass(slots=True)
class Ctx:
    """The arrays a risk is evaluated on (one row per member row of the results)."""

    act: np.ndarray  # u1
    tags: np.ndarray  # u8: the copy's facts
    ktags: np.ndarray  # u8: its reference keeper's (the target, or the keeper kept for it)
    diff: np.ndarray  # u4: how the copy differs from that keeper (codes.D_*)
    cross: np.ndarray  # bool: the group spans volumes
    last_vol: np.ndarray  # bool: after the plan its volume keeps no copy of the group
    other_vol: np.ndarray  # bool: the reference keeper is on another volume
    knlink: np.ndarray  # u4: the keeper's names
    links_to: np.ndarray  # i4: symbolic links of the plan pointing at the same keeper
    probed: np.ndarray  # bool: the copy and its keeper were looked at (the risk check)
    streams: np.ndarray  # i8: bytes of the copy's extra streams / attributes (not the web mark)
    shared: np.ndarray  # bool: some blocks of the group are shared already (snapshots, clones)
    btrfs: np.ndarray  # bool
    other_scan: np.ndarray  # bool: the keeper is in another scan's tab
    in_place_recovery: np.ndarray  # bool: the original would stay in its own folder
    no_trash: np.ndarray  # bool: the volume has no recycle bin (the recovery area takes it)
    size: np.ndarray  # i8
    hdd: np.ndarray  # bool
    win: bool = False
    verify: str = "auto"
    clone_rate: float = 400e6  # bytes / s for reading both copies (C03)
    extra: dict[str, np.ndarray] = field(default_factory=dict)


@dataclass(frozen=True, slots=True)
class RiskDef:
    id: str  # R01 ...
    code: str  # last_on_volume ...
    acts: frozenset[int]
    fn: Callable[[Ctx], np.ndarray]


def _t(ctx: Ctx, bits: int) -> np.ndarray:
    out: np.ndarray = (ctx.tags & np.uint64(bits)) != 0
    return out


def _k(ctx: Ctx, bits: int) -> np.ndarray:
    out: np.ndarray = (ctx.ktags & np.uint64(bits)) != 0
    return out


def _d(ctx: Ctx, bits: int) -> np.ndarray:
    out: np.ndarray = (ctx.diff & np.uint32(bits)) != 0
    return out


def _lvl(mask: np.ndarray, sev: int) -> np.ndarray:
    return np.where(mask, np.uint8(sev), np.uint8(0)).astype(np.uint8)


def _pick(*pairs: tuple[np.ndarray, int]) -> np.ndarray:
    """The highest level whose mask holds (pairs from the highest down)."""
    out = np.zeros(len(pairs[0][0]), np.uint8)
    for mask, sev in reversed(pairs):
        out = np.where(mask, np.uint8(sev), out)
    return out.astype(np.uint8)


def _by_act(ctx: Ctx, levels: dict[int, int], mask: np.ndarray) -> np.ndarray:
    out = np.zeros(len(ctx.act), np.uint8)
    for a, sev in levels.items():
        out = np.where(mask & (ctx.act == a), np.uint8(sev), out)
    return out.astype(np.uint8)


H, M, L, N = K.SEV_HIGH, K.SEV_MEDIUM, K.SEV_LOW, K.SEV_INFO
VOLATILE = K.T_DOWNLOADS | K.T_TEMP | K.T_CACHE | K.T_SAFE_CLEAN
PROGRAMISH = K.T_PROGRAM | K.T_GAME | K.T_APP | K.T_LOCKED_APP


def _ones(ctx: Ctx) -> np.ndarray:
    return np.ones(len(ctx.act), bool)


CATALOG: tuple[RiskDef, ...] = (
    # R: redundancy, keepers, checking
    RiskDef("R01", "last_on_volume", frozenset({TRASH, SYM}), lambda c: _lvl(c.cross & c.last_vol, H)),
    RiskDef(
        "R02",
        "backup_like",
        frozenset({TRASH, HARD, SYM, CLONE}),
        lambda c: _by_act(c, {TRASH: M, HARD: H, SYM: H, CLONE: L}, _t(c, K.T_BACKUP)),
    ),
    RiskDef("R03", "redundancy_drop", REPL, lambda c: _lvl(_ones(c), N)),
    RiskDef(
        "R04",
        "keeper_volatile",
        frozenset({TRASH, SYM}),
        lambda c: _lvl(_k(c, VOLATILE) & ~_t(c, VOLATILE), H),
    ),
    RiskDef(
        "R05",
        "keeper_removable",
        frozenset({TRASH, SYM}),
        lambda c: _lvl(_k(c, K.T_REMOVABLE) & ~_t(c, K.T_REMOVABLE), H),
    ),
    RiskDef(
        "R06",
        "keeper_program",
        frozenset({TRASH, SYM}),
        lambda c: _lvl(_k(c, PROGRAMISH) & ~_t(c, PROGRAMISH), H),
    ),
    RiskDef("R07", "keeper_other_owner", frozenset({TRASH, SYM}), lambda c: _lvl(_d(c, K.D_OWNER), M)),
    RiskDef("R08", "keeper_recent", ANY, lambda c: _lvl(_k(c, K.T_RECENT1), M)),
    RiskDef("R10", "cache_only", ANY, lambda c: _lvl(_t(c, K.T_CACHE_ONLY), L)),
    RiskDef(
        "R11",
        "stamps_only",
        frozenset({TRASH, HARD, SYM}),
        lambda c: _lvl(
            np.full(len(c.act), c.verify == "metadata")
            & (_t(c, K.T_NO_USN | K.T_RECENT1 | K.T_CACHE_ONLY) | _k(c, K.T_NO_USN | K.T_RECENT1)),
            H,
        ),
    ),
    RiskDef("R12", "link_target", frozenset({TRASH}), lambda c: _lvl(_t(c, K.T_LINK_TARGET), H)),
    RiskDef("R13", "cross_scan_keeper", ANY, lambda c: _lvl(c.other_scan, N)),
    # T: the recycle bin
    RiskDef(
        "T01", "sync_delete", frozenset({TRASH}), lambda c: _lvl(_t(c, K.T_SYNC) & ~_t(c, K.T_SYNC_DUP), H)
    ),
    RiskDef(
        "T02", "sync_delete_dup", frozenset({TRASH}), lambda c: _lvl(_t(c, K.T_SYNC) & _t(c, K.T_SYNC_DUP), L)
    ),
    RiskDef("T03", "vcs_tracked", frozenset({TRASH}), lambda c: _lvl(_t(c, K.T_VCS), M)),
    RiskDef("T04", "app_data", frozenset({TRASH}), lambda c: _lvl(_t(c, K.T_APP | K.T_LOCKED_APP), H)),
    RiskDef("T05", "program_or_game", frozenset({TRASH}), lambda c: _lvl(_t(c, K.T_PROGRAM | K.T_GAME), H)),
    RiskDef("T06", "media_library", frozenset({TRASH}), lambda c: _lvl(_t(c, K.T_MEDIA_LIB), H)),
    RiskDef("T07", "project_reference", frozenset({TRASH}), lambda c: _lvl(_t(c, K.T_PROJECT), M)),
    RiskDef("T08", "name_differs", frozenset({TRASH}), lambda c: _lvl(_d(c, K.D_NAME), L)),
    RiskDef("T09", "system_attr", frozenset({TRASH}), lambda c: _lvl(_t(c, K.T_SYSTEM), H)),
    RiskDef("T10", "hidden_temp_root", frozenset({TRASH}), lambda c: _lvl(_t(c, K.T_HIDDEN_TEMP_ROOT), M)),
    RiskDef(
        "T11",
        "copy_recent",
        frozenset({TRASH}),
        lambda c: _pick((_t(c, K.T_RECENT1), M), (_t(c, K.T_RECENT7), L)),
    ),
    RiskDef("T12", "extra_streams", frozenset({TRASH}), lambda c: _lvl(c.streams > 0, M)),
    RiskDef("T13", "no_trash_recovery", frozenset({TRASH}), lambda c: _lvl(c.no_trash, N)),
    # H: hard links
    RiskDef(
        "H01",
        "edits_shared",
        frozenset({HARD}),
        lambda c: _pick(
            (_t(c, K.T_INPLACE | K.T_DOCLIKE), H),
            (_t(c, K.T_MEDIA), M),
            (_t(c, K.T_ARCHIVE), L),
            (_ones(c), M),
        ),
    ),
    RiskDef(
        "H02", "save_breaks_link", frozenset({HARD}), lambda c: _pick((_t(c, K.T_DOCLIKE), M), (_ones(c), N))
    ),
    RiskDef(
        "H03",
        "metadata_changes",
        frozenset({HARD}),
        lambda c: _lvl(_d(c, K.D_MTIME | K.D_BTIME | K.D_ATTRS), L),
    ),
    RiskDef(
        "H04",
        "owner_acl",
        frozenset({HARD}),
        lambda c: _pick(
            (_d(c, K.D_OWNER | K.D_ACL_EXPLICIT | K.D_UNKNOWN), H),
            (_d(c, K.D_ACL_INHERITED | K.D_MODE | K.D_SELINUX), M),
        ),
    ),
    RiskDef(
        "H05",
        "motw_lost",
        frozenset({HARD}),
        lambda c: _pick((_d(c, K.D_MOTW_LOST) & _t(c, K.T_EXEC), H), (_d(c, K.D_MOTW_LOST), L)),
    ),
    RiskDef("H06", "efs_differs", frozenset({HARD}), lambda c: _lvl(_d(c, K.D_EFS), H)),
    RiskDef(
        "H07",
        "streams_lost",
        frozenset({HARD}),
        lambda c: _pick((_d(c, K.D_STREAMS_BIG), H), (_d(c, K.D_STREAMS_SMALL), L)),
    ),
    RiskDef("H08", "keeper_in_sync", frozenset({HARD}), lambda c: _lvl(_k(c, K.T_SYNC), H)),
    RiskDef(
        "H09", "program_or_app", frozenset({HARD}), lambda c: _lvl(_t(c, PROGRAMISH) | _k(c, PROGRAMISH), H)
    ),
    RiskDef("H10", "vcs_worktree", frozenset({HARD}), lambda c: _lvl(_t(c, K.T_VCS), M)),
    RiskDef("H11", "many_names", frozenset({HARD}), lambda c: _lvl(c.knlink >= 100, N)),
    RiskDef("H12", "smb_share", frozenset({HARD}), lambda c: _lvl(_t(c, K.T_SMB), L)),
    RiskDef("H13", "dedup_volume", frozenset({HARD}), lambda c: _lvl(_t(c, K.T_DEDUP), M)),
    # S: symbolic links
    RiskDef(
        "S01",
        "target_fragile",
        frozenset({SYM}),
        lambda c: _pick((_k(c, VOLATILE | K.T_IN_BASKET), H), (_ones(c), M)),
    ),
    RiskDef("S02", "target_removable", frozenset({SYM}), lambda c: _lvl(_k(c, K.T_REMOVABLE), H)),
    RiskDef("S03", "target_other_volume", frozenset({SYM}), lambda c: _lvl(c.other_vol, M)),
    RiskDef("S04", "link_style", frozenset({SYM}), lambda c: _lvl(_ones(c), N)),
    RiskDef(
        "S05",
        "apps_dont_follow",
        frozenset({SYM}),
        lambda c: _pick(
            (_t(c, K.T_EXEC | K.T_FONT_DRIVER | K.T_GAME), H),
            (_t(c, K.T_DOCLIKE | K.T_PROJECT), M),
            (_ones(c), L),
        ),
    ),
    RiskDef("S06", "target_in_sync", frozenset({SYM}), lambda c: _lvl(_k(c, K.T_SYNC), H)),
    RiskDef("S07", "vcs_typechange", frozenset({SYM}), lambda c: _lvl(_t(c, K.T_VCS), H)),
    RiskDef("S08", "target_permission", frozenset({SYM}), lambda c: _lvl(_d(c, K.D_STRICTER | K.D_OWNER), M)),
    RiskDef("S09", "smb_share", frozenset({SYM}), lambda c: _lvl(_t(c, K.T_SMB), M)),
    RiskDef("S10", "many_links_to_keeper", frozenset({SYM}), lambda c: _lvl(c.links_to >= 10, M)),
    # C: block clones
    RiskDef("C01", "space_display", frozenset({CLONE}), lambda c: _lvl(_ones(c), N)),
    RiskDef("C02", "snapshot_held", frozenset({CLONE}), lambda c: _pick((c.shared, M), (c.btrfs, L))),
    RiskDef(
        "C03",
        "brief_exclusive",
        frozenset({CLONE}),
        lambda c: _pick(
            (np.full(len(c.act), c.win) & (2 * c.size / max(c.clone_rate, 1.0) > 5), M), (_ones(c), L)
        ),
    ),
    RiskDef("C04", "compare_reads", frozenset({CLONE}), lambda c: _lvl(np.full(len(c.act), c.win), N)),
    RiskDef("C05", "fragmentation", frozenset({CLONE}), lambda c: _lvl(c.hdd, L)),
    RiskDef(
        "C09",
        "sync_reupload",
        frozenset({CLONE}),
        lambda c: _lvl(_t(c, K.T_SYNC) & np.full(len(c.act), c.win), M),
    ),
    # X: replacements
    RiskDef("X01", "brief_absence", REPL, lambda c: _lvl(np.full(len(c.act), c.win), L)),
    RiskDef(
        "X02",
        "recovery_in_place",
        REPL,
        lambda c: _pick((c.in_place_recovery & _t(c, K.T_VCS), M), (c.in_place_recovery, L)),
    ),
    RiskDef(
        "X03", "readonly_attr", REPL, lambda c: _lvl(_t(c, K.T_READONLY) & np.full(len(c.act), c.win), N)
    ),
    RiskDef("X04", "unknown_facts", frozenset({HARD, SYM, CLONE}), lambda c: _lvl(~c.probed, M)),
)
BY_ID: dict[str, RiskDef] = {r.id: r for r in CATALOG}
BY_CODE: dict[str, RiskDef] = {r.code: r for r in CATALOG}
IDX: dict[str, int] = {r.id: i for i, r in enumerate(CATALOG)}


def evaluate(ctx: Ctx) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
    """(rows, risk index, severity) of every risk present, sorted by row."""
    rows: list[np.ndarray] = []
    ids: list[np.ndarray] = []
    sevs: list[np.ndarray] = []
    for i, r in enumerate(CATALOG):
        on = np.isin(ctx.act, np.array(sorted(r.acts), np.uint8))
        if not on.any():
            continue
        sev = r.fn(ctx)
        hit = np.flatnonzero(on & (sev > 0))
        if len(hit):
            rows.append(hit.astype(np.int64))
            ids.append(np.full(len(hit), i, np.int16))
            sevs.append(sev[hit])
    if not rows:
        return np.zeros(0, np.int64), np.zeros(0, np.int16), np.zeros(0, np.uint8)
    r_ = np.concatenate(rows)
    order = np.argsort(r_, kind="stable")
    return r_[order], np.concatenate(ids)[order], np.concatenate(sevs)[order]


TEXT = Texts(
    {
        # titles of the kinds (review buckets)
        "t_last_on_volume": "处理后这个卷上就没有这份内容了",
        "t_backup_like": "像一份备份",
        "t_redundancy_drop": "独立的份数变少",
        "t_keeper_volatile": "保留那份在会被清理的地方",
        "t_keeper_removable": "保留那份在可移动盘上",
        "t_keeper_program": "保留那份在程序或应用的文件夹里",
        "t_keeper_other_owner": "保留那份属于别的用户",
        "t_keeper_recent": "保留那份最近还改过",
        "t_cache_only": "内容相同来自缓存",
        "t_stamps_only": "只凭时间戳核对",
        "t_link_target": "SpaceKnife 建的符号链接指向它",
        "t_cross_scan_keeper": "保留那份在另一个扫描里",
        "t_sync_delete": "删除会同步到云端",
        "t_sync_delete_dup": "删除会同步到云端（云端还有保留的那份）",
        "t_vcs_tracked": "版本库里的文件",
        "t_app_data": "应用的数据",
        "t_program_or_game": "程序或游戏的文件",
        "t_media_library": "媒体库按路径引用它",
        "t_project_reference": "工程里的文件被删了可能找不到",
        "t_name_differs": "名字和保留那份不同",
        "t_system_attr": "带「系统」属性",
        "t_hidden_temp_root": "卷根目录下的隐藏临时文件",
        "t_copy_recent": "最近还改过",
        "t_extra_streams": "有没比较过的附加数据",
        "t_no_trash_recovery": "这个卷没有回收站",
        "t_edits_shared": "改一份另一份也变",
        "t_save_breaks_link": "保存后链接会悄悄断开",
        "t_metadata_changes": "时间和属性跟着保留那份",
        "t_owner_acl": "所有者或权限不同",
        "t_motw_lost": "会丢掉「来自网络」的标记",
        "t_efs_differs": "加密状态不同",
        "t_streams_lost": "附加数据会被换成保留那份的",
        "t_keeper_in_sync": "保留那份在同步盘里",
        "t_program_or_app": "程序、游戏或应用的文件",
        "t_vcs_worktree": "版本库里的文件",
        "t_many_names": "保留那份已经有很多名字",
        "t_smb_share": "在网络共享的文件夹里",
        "t_dedup_volume": "卷开着重复数据删除",
        "t_target_fragile": "移动或删掉保留那份，链接就打不开",
        "t_target_removable": "链接指向可移动盘",
        "t_target_other_volume": "链接指向另一个卷",
        "t_link_style": "相对或绝对路径",
        "t_apps_dont_follow": "有的程序不认链接",
        "t_target_in_sync": "链接指向同步盘里的文件",
        "t_vcs_typechange": "版本库会看成「文件变成链接」",
        "t_target_permission": "链接按保留那份的权限打开",
        "t_many_links_to_keeper": "很多链接指向同一份",
        "t_space_display": "资源管理器里的大小不变",
        "t_snapshot_held": "快照可能还占着数据块",
        "t_brief_exclusive": "执行时短暂不能写",
        "t_compare_reads": "共用之前逐段比较",
        "t_fragmentation": "机械盘上读起来可能变慢",
        "t_sync_reupload": "同步工具可能重新上传",
        "t_brief_absence": "替换时路径有一瞬间不存在",
        "t_recovery_in_place": "原件暂时留在同一个文件夹",
        "t_readonly_attr": "只读属性按新文件来",
        "t_unknown_facts": "还在核对权限和附加数据",
        # what the user reads per copy (evidence filled in)
        "m_last_on_volume": "处理后 {vol} 上就没有这份内容了：跨盘的副本常常是有意留的备份",
        "m_backup_like": "「{folder}」像一份备份。{why}",
        "m_backup_why_trash": "移到回收站后，这份备份就少了一份",
        "m_backup_why_hardlink": "硬链接不是备份：两处是同一个文件，改坏了、被勒索软件加密了，两处一起坏",
        "m_backup_why_symlink": "符号链接不是备份：保留那份坏了或不在了，这里也就没了",
        "m_backup_why_clone": "两份共用数据块：这几个扇区坏了，两份一起坏",
        "m_redundancy_drop": "这份内容从 {before} 份独立的变成 {after} 份（另 {links} 处是链接）",
        "m_keeper_volatile": "保留那份在 {where} 里：那里的文件会被清理，到时候这份内容一份也不剩",
        "m_keeper_removable": "保留那份在可移动盘 {kvol} 上：盘不在、坏了或丢了，这份内容就没了",
        "m_keeper_program": "保留那份在 {where} 里：卸载或更新时它会被删掉或改掉",
        "m_keeper_other_owner": "保留那份属于用户 {kowner}（这份是 {owner}）：他删了它，你这份就没了",
        "m_keeper_recent": "保留那份最近改过（{ago}），可能还在用",
        "m_cache_only": "这一组的「内容相同」来自缓存：执行前会把两份都重读一遍",
        "m_stamps_only": "只凭时间戳核对：有程序改了内容又把时间改回去时看不出来（原件仍在恢复区或回收站）",
        "m_link_target": "有 SpaceKnife 建的符号链接指向它：移走以后这些链接都打不开",
        "m_cross_scan_keeper": "保留那份在 {kvol} 的扫描里",
        "m_sync_delete": "删除会同步到 {provider} 的云端和你的其他设备（云端回收站会留一段时间）；保留那份不在这个同步文件夹 {root} 里",
        "m_sync_delete_dup": "删除会同步到 {provider} 的云端；云端还有保留的那份",
        "m_vcs_tracked": "这是版本库 {repo} 里的文件：会显示成删除，提交后别人那边也会删",
        "m_app_data": "这是「{app}」的数据文件夹里的文件：程序可能还要用它，删了可能报错或重新下载",
        "m_program_or_game": "它在 {where} 里：删了程序可能出错，校验时会重新下载",
        "m_media_library": "{lib} 按路径引用它：删了库里会显示缺失",
        "m_project_reference": "这个文件夹像一个工程（有 {marker}）：别的文件可能按这个路径引用它，删了会找不到。换成链接或块克隆能保住这个路径",
        "m_name_differs": "这份叫 {name}，保留那份叫 {kname}：按这个名字找它的程序会找不到",
        "m_system_attr": "它带 Windows 的「系统」属性",
        "m_hidden_temp_root": "这是 {vol} 根目录下的隐藏临时文件 {name}：不知道是哪个程序的，可能还在用",
        "m_copy_recent": "这份最近改过（{ago}）",
        "m_extra_streams": "它还有没比较过的附加数据（{names}，{size}）：放进回收站时一起保留，清空回收站时一起丢掉",
        "m_no_trash_recovery": "这个卷没有回收站：先放进 SpaceKnife 的恢复区，清除恢复区后才腾出空间",
        "m_edits_shared": "以后在任何一处改它，另一处跟着变：它们是同一个文件（{cls}）",
        "m_save_breaks_link": "用 Word 这类程序保存后，链接会悄悄断开：这里又变回独立的一份，空间重新占用，两处内容也不再一样",
        "m_metadata_changes": "这里会显示保留那份的修改时间 {kmtime}（原来 {mtime}），只读和隐藏属性也跟着保留那份",
        "m_owner_acl": "这一份的所有者是 {owner}，保留那份是 {kowner}{acl}：换成硬链接后这里按保留那份的权限，有人可能打不开，也可能多了访问权",
        "m_owner_acl_acl": "，权限也不同",
        "m_owner_acl_unknown": "读不到这份或保留那份的权限：按不同算",
        "m_motw_lost": "这份带着「从网上下载」的标记，保留那份没有：换成后打开它时，系统不再提醒它来自网络",
        "m_efs_differs": "两份的加密（EFS）状态不同：换成后这里按保留那份的",
        "m_streams_lost": "这份的附加数据（{names}，{size}）会被换成保留那份的；原件在恢复区里，清除前都还在",
        "m_keeper_in_sync": "保留那份在 {kprovider} 里：它可能被转成只在云端的占位文件，硬链接的每个名字会一起变；从这里改它时，同步工具可能察觉不到",
        "m_program_or_app": "这是程序、游戏或应用的文件（{where}）：更新时可能就地改写它（另一处跟着变），校验工具可能把它当成损坏",
        "m_vcs_worktree": "这是版本库 {repo} 里的文件：就地改它会改到另一处；切换分支时会把它重写成独立的文件，省下的空间会回来",
        "m_many_names": "保留那份已经有 {n} 个名字",
        "m_smb_share": "这个文件夹共享在网络上：别的电脑上看到的也会变",
        "m_dedup_volume": "这个卷开着 Windows 的重复数据删除：它们可能已经共享数据块，实际腾出可能接近 0",
        "m_target_fragile": "以后移动、改名或删掉 {keeper}，这里就打不开了",
        "m_target_removable": "保留那份在可移动盘 {kvol} 上：拔掉盘、盘符变了，这里就打不开",
        "m_target_other_volume": "要 {vol} 和 {kvol} 都在才打得开；整盘备份 {vol} 时，备份里只有一个链接，没有内容",
        "m_link_style_relative": "相对路径：只搬其中一边会断",
        "m_link_style_absolute": "绝对路径：整个文件夹换位置、换盘符会断",
        "m_apps_dont_follow": "有的程序不认链接：备份软件可能只备份链接本身，压缩软件可能只存链接，游戏或校验程序可能报文件损坏",
        "m_target_in_sync": "保留那份在 {kprovider} 里：它可能被设成「仅在线」，那时打开这里要先下载，离线时打不开",
        "m_vcs_typechange": "版本库会把它看成「从文件变成链接」，提交后别人那边也变成链接（在 Windows 上常常打不开）",
        "m_target_permission": "链接按保留那份的权限打开（所有者 {kowner}）：原来能读这里的用户可能读不了",
        "m_many_links_to_keeper": "{n} 个符号链接都会指向这一份：移动或删除它，这些链接全部失效",
        "m_space_display": "资源管理器和多数工具仍把两份各算一次大小，只有盘的可用空间会变多；以后改写其中一份，改写的部分会重新占用空间",
        "m_snapshot_held": "这块盘上可能有快照还引用这份的数据：快照删掉之前，腾出的空间可能比算出来的少",
        "m_brief_exclusive": "执行时大约 {s} 秒内别的程序不能写这两份（读不受影响）",
        "m_compare_reads": "共用数据块之前会逐段比较两份，要读 {size}",
        "m_fragmentation": "共用数据块后文件更零碎，在机械硬盘上读起来可能变慢",
        "m_sync_reupload": "同步工具可能把它当成改动，重新上传一次（内容没变）",
        "m_brief_absence": "替换时这个路径会有几微秒不存在：恰好在那一刻打开它的程序会报找不到",
        "m_recovery_in_place": "原件暂时以隐藏名字留在同一个文件夹里，直到清除",
        "m_readonly_attr": "这份是只读的：替换后这里的只读属性按新文件来",
        "m_unknown_facts": "还在核对这份和保留那份的权限和附加数据",
        "where_downloads": "下载或桌面文件夹",
        "where_temp": "临时文件夹",
        "where_cache": "缓存文件夹",
        "where_program": "程序的文件夹",
        "where_app": "应用数据的文件夹",
        "where_game": "游戏的文件夹",
        "a_moment": "刚才",
        "hours_ago": "{n} 小时前",
        "days_ago": "{n} 天前",
        "names_more": "{names} 等 {n} 项",
        "unknown": "（不知道）",
    }
)
