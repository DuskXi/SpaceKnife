"""Codes and texts of the duplicate-file cleanup plan (docs/dupes.md §16).

Numbers that live in the plan's arrays (one byte each): actions, severities, the reasons an action
cannot be done (``NA_*``), the sources of a decision, the fact tags (bits of a ``u8``) and the
differences between a copy and the keeper it is compared with. Texts are the user's words (zh-CN
source, ``locales/en/cleanup.json``), never jargon: 「换成硬链接」, not "hardlink".
"""

from __future__ import annotations

from typing import Final, Literal

from ...i18n import Texts

Action = Literal["leave", "trash", "hardlink", "symlink", "clone"]

# ------------------------------------------------------------------ actions (u1)
A_NONE: Final = 0  # a keeper, or a row outside the plan
A_LEAVE: Final = 1
A_TRASH: Final = 2
A_HARDLINK: Final = 3
A_SYMLINK: Final = 4
A_CLONE: Final = 5
ACTIONS: Final[tuple[Action, ...]] = ("leave", "trash", "hardlink", "symlink", "clone")
ACTION_CODE: Final[dict[str, int]] = {a: i + 1 for i, a in enumerate(ACTIONS)}
ACTION_NAME: Final[dict[int, Action]] = {i + 1: a for i, a in enumerate(ACTIONS)}
REPLACING: Final = (A_HARDLINK, A_SYMLINK)  # the original goes to the recovery area

# ------------------------------------------------------------------ severities (u1)
SEV_NONE: Final = 0
SEV_INFO: Final = 1
SEV_LOW: Final = 2
SEV_MEDIUM: Final = 3
SEV_HIGH: Final = 4
SEV_BLOCKED: Final = 5
SEVERITIES: Final = ("none", "info", "low", "medium", "high", "blocked")
SEV_CODE: Final[dict[str, int]] = {s: i for i, s in enumerate(SEVERITIES)}

# ------------------------------------------------------------------ keep state (u1)
K_NO: Final = 0
K_KEEP: Final = 1
K_LOCKED: Final = 2  # may only be kept (layer 1)

# ------------------------------------------------------------------ where a decision came from (u1)
SRC_NONE: Final = 0
SRC_FORCED_NEEDED: Final = 1
SRC_FORCED_ALIAS: Final = 2
SRC_FORCED_PINNED: Final = 3
SRC_FORCED_VCS: Final = 4
SRC_FORCED_NETWORK: Final = 5
SRC_MANUAL_KEEP: Final = 6
SRC_MANUAL_NOT: Final = 7
SRC_GROUP_SET: Final = 8
SRC_PAIR_SIDE: Final = 9
SRC_PAIR_BOTH: Final = 10
SRC_KEEP_ALL: Final = 11
SRC_PER_VOLUME: Final = 12
SRC_PER_FOLDER: Final = 13
SRC_KEEP_N: Final = 14
SRC_PREFER: Final = 15
SRC_AVOID: Final = 16
SRC_FOLDER: Final = 17
SRC_CRITERION: Final = 18
SRC_TIE: Final = 19
SRC_SAFETY_NET: Final = 20
SRC_ACTION_RULE: Final = 21
SRC_ACTION_FALLBACK: Final = 22
SRC_ACTION_PRESET: Final = 23
SRC_ACTION_MANUAL: Final = 24
SRC_ACTION_PAIR: Final = 25
SRC_ACTION_RISK: Final = 26  # a risk-condition rule turned it into 「不动」
SRC_NOT_KEPT: Final = 27  # not kept: another copy was chosen
SRC_NAMES: Final[dict[int, str]] = {
    SRC_FORCED_NEEDED: "forced_needed",
    SRC_FORCED_ALIAS: "forced_alias",
    SRC_FORCED_PINNED: "forced_pinned",
    SRC_FORCED_VCS: "forced_vcs",
    SRC_FORCED_NETWORK: "forced_network",
    SRC_MANUAL_KEEP: "manual_keep",
    SRC_MANUAL_NOT: "manual_not_keep",
    SRC_GROUP_SET: "group_keep_set",
    SRC_PAIR_SIDE: "pair_side",
    SRC_PAIR_BOTH: "pair_both",
    SRC_KEEP_ALL: "keep_all",
    SRC_PER_VOLUME: "per_volume",
    SRC_PER_FOLDER: "per_folder",
    SRC_KEEP_N: "keep_n",
    SRC_PREFER: "prefer",
    SRC_AVOID: "avoid",
    SRC_FOLDER: "folder",
    SRC_CRITERION: "criterion",
    SRC_TIE: "tie",
    SRC_SAFETY_NET: "safety_net",
    SRC_ACTION_RULE: "action_rule",
    SRC_ACTION_FALLBACK: "action_fallback",
    SRC_ACTION_PRESET: "action_preset",
    SRC_ACTION_MANUAL: "action_manual",
    SRC_ACTION_PAIR: "action_pair",
    SRC_ACTION_RISK: "action_risk",
    SRC_NOT_KEPT: "not_kept",
}
MANUAL_SOURCES: Final = (SRC_MANUAL_KEEP, SRC_MANUAL_NOT, SRC_GROUP_SET, SRC_ACTION_MANUAL)
FORCED_SOURCES: Final = (
    SRC_FORCED_NEEDED,
    SRC_FORCED_ALIAS,
    SRC_FORCED_PINNED,
    SRC_FORCED_VCS,
    SRC_FORCED_NETWORK,
)

# ------------------------------------------------------------------ why an action cannot be done (u1)
NA_OK: Final = 0
NA_OTHER_VOLUME: Final = 1
NA_OTHER_SUBVOLUME: Final = 2
NA_FS_NO_HARDLINK: Final = 3
NA_FS_NO_SYMLINK: Final = 4
NA_FS_NO_CLONE: Final = 5
NA_NO_SYMLINK_RIGHT: Final = 6
NA_LINK_LIMIT: Final = 7
NA_PROTECTED_HARDLINKS: Final = 8
NA_IMMUTABLE: Final = 9
NA_IN_SYNC_ROOT: Final = 10
NA_VCS_INTERNAL: Final = 11
NA_IN_PACKAGE: Final = 12
NA_NO_TRASH: Final = 13
NA_NO_RECOVERY: Final = 14
NA_NO_EXCHANGE: Final = 15
NA_FOLDER_NOT_WRITABLE: Final = 16
NA_NAME_UNUSABLE: Final = 17
NA_NO_KEEPER_HERE: Final = 18
NA_ALREADY_SHARED: Final = 19
NA_NETWORK: Final = 20
NA_NO_NATIVE: Final = 21
NA_MACOS_OFF: Final = 22
NA_FORCED_KEEP: Final = 23
NA_STALE: Final = 24
NA_READONLY_INPLACE: Final = 25
NA_ENCRYPTED: Final = 26
NA_SPARSE_MISMATCH: Final = 27
NA_IN_BASKET: Final = 28
NA_KEEPER: Final = 29  # it is a keeper: only 「不动」
NA_NOT_OWNER: Final = 30  # Linux in-place sharing needs the owner or write access
NA_RISK_RULE: Final = 31
NA_NAMES: Final[dict[int, str]] = {
    NA_OTHER_VOLUME: "other_volume",
    NA_OTHER_SUBVOLUME: "other_subvolume",
    NA_FS_NO_HARDLINK: "fs_no_hardlink",
    NA_FS_NO_SYMLINK: "fs_no_symlink",
    NA_FS_NO_CLONE: "fs_no_clone",
    NA_NO_SYMLINK_RIGHT: "no_symlink_right",
    NA_LINK_LIMIT: "link_limit",
    NA_PROTECTED_HARDLINKS: "protected_hardlinks",
    NA_IMMUTABLE: "immutable",
    NA_IN_SYNC_ROOT: "in_sync_root",
    NA_VCS_INTERNAL: "vcs_internal",
    NA_IN_PACKAGE: "in_package",
    NA_NO_TRASH: "no_trash",
    NA_NO_RECOVERY: "no_recovery_area",
    NA_NO_EXCHANGE: "no_exchange",
    NA_FOLDER_NOT_WRITABLE: "folder_not_writable",
    NA_NAME_UNUSABLE: "name_unusable",
    NA_NO_KEEPER_HERE: "no_keeper_here",
    NA_ALREADY_SHARED: "already_shared",
    NA_NETWORK: "network",
    NA_NO_NATIVE: "no_native",
    NA_MACOS_OFF: "macos_off",
    NA_FORCED_KEEP: "forced_keep",
    NA_STALE: "stale",
    NA_READONLY_INPLACE: "readonly_inplace",
    NA_ENCRYPTED: "encrypted",
    NA_SPARSE_MISMATCH: "sparse_mismatch",
    NA_IN_BASKET: "in_basket",
    NA_KEEPER: "keeper",
    NA_NOT_OWNER: "not_owner",
    NA_RISK_RULE: "risk_rule",
}
NA_CODE: Final[dict[str, int]] = {v: k for k, v in NA_NAMES.items()}

# ------------------------------------------------------------------ fact tags (bits of a u8)
T_VCS: Final = 1 << 0  # in a version-control work tree (not its internal folder)
T_VCS_INTERNAL: Final = 1 << 1  # inside .git / .hg / .svn
T_SYNC: Final = 1 << 2  # in a cloud-sync folder (OneDrive, Dropbox, Syncthing, ...)
T_APP: Final = 1 << 3  # application data (AppData, Library, .config, chat apps, VMs)
T_PROGRAM: Final = 1 << 4  # program files, system folders
T_GAME: Final = 1 << 5
T_CACHE: Final = 1 << 6  # caches cleaned on their own (npm, pnpm store, browser caches)
T_TEMP: Final = 1 << 7
T_DOWNLOADS: Final = 1 << 8  # Downloads, Desktop
T_BACKUP: Final = 1 << 9  # a folder that looks like a backup
T_PROJECT: Final = 1 << 10  # a project folder (package.json, Cargo.toml, *.sln ...)
T_REMOVABLE: Final = 1 << 11
T_HIDDEN: Final = 1 << 12
T_SYSTEM: Final = 1 << 13
T_READONLY: Final = 1 << 14
T_RECENT1: Final = 1 << 15  # modified within 24 hours
T_RECENT7: Final = 1 << 16  # ... within 7 days
T_MEDIA_LIB: Final = 1 << 17  # a media library refers to it by path (Lightroom, iTunes, Photos)
T_HIDDEN_TEMP_ROOT: Final = 1 << 18  # a hidden or temporary file at the root of a volume
T_LINK_TARGET: Final = 1 << 19  # a symbolic link SpaceKnife made points at it
T_IN_BASKET: Final = 1 << 20  # the delete basket covers it
T_NETWORK: Final = 1 << 21
T_ENCRYPTED: Final = 1 << 22
T_SPARSE: Final = 1 << 23
T_COMPRESSED: Final = 1 << 24
T_CACHE_ONLY: Final = 1 << 25  # its group was found identical from the cache, and its stamps prove less
T_SMB: Final = 1 << 26  # in a Windows SMB share
T_DEDUP: Final = 1 << 27  # Windows data deduplication
T_PACKAGE: Final = 1 << 28  # inside a macOS package (.app, .photoslibrary)
T_EXEC: Final = 1 << 29  # executables, scripts, macro documents, archives, installers
T_DOCLIKE: Final = 1 << 30  # documents, spreadsheets, code (edited in place, saved by replace)
T_INPLACE: Final = 1 << 31  # formats written in place (disk images, databases, mail stores, PSD)
T_MEDIA: Final = 1 << 32  # images, audio, video
T_ARCHIVE: Final = 1 << 33  # archives, installers
T_FONT_DRIVER: Final = 1 << 34
T_THROUGH_LINK: Final = 1 << 35  # its folder's path goes through a link, junction or mount point
T_NO_USN: Final = 1 << 36  # Windows without a USN stamp: the stamps prove less
T_VIEW: Final = 1 << 37  # another path of the same single-named file
T_SAFE_CLEAN: Final = 1 << 38  # insights rule cleanup = "safe": cleaned up by its program
T_LOCKED_APP: Final = 1 << 39  # insights rule cleanup = "keep": a program needs it
T_SYNC_DUP: Final = 1 << 40  # in the same sync folder as a keeper (helper, set per decision)
TAG_NAMES: Final[dict[int, str]] = {
    T_VCS: "vcs",
    T_VCS_INTERNAL: "vcs_internal",
    T_SYNC: "sync",
    T_APP: "app",
    T_PROGRAM: "program",
    T_GAME: "game",
    T_CACHE: "cache",
    T_TEMP: "temp",
    T_DOWNLOADS: "downloads",
    T_BACKUP: "backup",
    T_PROJECT: "project",
    T_REMOVABLE: "removable",
    T_HIDDEN: "hidden",
    T_SYSTEM: "system",
    T_READONLY: "readonly",
    T_RECENT1: "recent",
    T_RECENT7: "recent_week",
    T_MEDIA_LIB: "media_library",
    T_HIDDEN_TEMP_ROOT: "hidden_temp_root",
    T_LINK_TARGET: "link_target",
    T_IN_BASKET: "in_basket",
    T_NETWORK: "network",
    T_ENCRYPTED: "encrypted",
    T_SPARSE: "sparse",
    T_COMPRESSED: "compressed",
    T_CACHE_ONLY: "cache_only",
    T_SMB: "smb_share",
    T_DEDUP: "dedup",
    T_PACKAGE: "package",
    T_EXEC: "exec",
    T_DOCLIKE: "document",
    T_INPLACE: "in_place",
    T_MEDIA: "media",
    T_ARCHIVE: "archive",
    T_FONT_DRIVER: "font_driver",
    T_THROUGH_LINK: "through_link",
    T_NO_USN: "no_usn",
}
TAG_CODE: Final[dict[str, int]] = {v: k for k, v in TAG_NAMES.items()}

# ------------------------------------------------------------------ copy vs. keeper (bits of a u4)
D_OWNER: Final = 1 << 0
D_ACL_EXPLICIT: Final = 1 << 1
D_ACL_INHERITED: Final = 1 << 2
D_MODE: Final = 1 << 3
D_MOTW_LOST: Final = 1 << 4  # the copy is marked as downloaded, the keeper not
D_EFS: Final = 1 << 5
D_STREAMS_BIG: Final = 1 << 6  # the copy has streams or attributes the keeper lacks, over 4 KiB
D_STREAMS_SMALL: Final = 1 << 7
D_MTIME: Final = 1 << 8
D_BTIME: Final = 1 << 9
D_ATTRS: Final = 1 << 10  # read-only / hidden differ
D_NAME: Final = 1 << 11
D_UNKNOWN: Final = 1 << 12  # the risk check could not read one of them (I7)
D_STRICTER: Final = 1 << 13  # the keeper's owner or permissions are stricter
D_SELINUX: Final = 1 << 14

# ------------------------------------------------------------------ results of a step
RESULT_STOPS: Final = frozenset({"failed_attention"})
DONE_CODES: Final = frozenset({"done", "done_partial"})

TEXT = Texts(
    {
        # actions
        "act_leave": "不动",
        "act_trash": "移到回收站",
        "act_trash_mac": "移到废纸篓",
        "act_hardlink": "换成硬链接",
        "act_symlink": "换成符号链接",
        "act_clone": "共用数据块（块克隆）",
        "act_keep": "保留",
        "sev_info": "提示",
        "sev_low": "低",
        "sev_medium": "中",
        "sev_high": "高",
        "sev_blocked": "不可用",
        # why an action cannot be done
        "na_other_volume": "保留那份在别的卷上：硬链接和块克隆不能跨卷",
        "na_other_subvolume": "btrfs 的不同子卷之间不能建硬链接（块克隆可以）",
        "na_fs_no_hardlink": "{fs} 不支持硬链接",
        "na_fs_no_symlink": "{fs} 不支持符号链接",
        "na_fs_no_clone": "{fs} 不支持共用数据块（ReFS、btrfs、xfs（开了 reflink）、bcachefs 可以）",
        "na_no_symlink_right": "建符号链接要开 Windows 的「开发人员模式」（设置 › 系统 › 开发者选项），或者以管理员身份运行 SpaceKnife",
        "na_link_limit": "保留那份的名字已经到上限（{limit} 个）",
        "na_protected_hardlinks": "系统只允许给自己的文件建硬链接：保留那份属于别的用户",
        "na_immutable": "文件被设成不可改动",
        "na_in_sync_root": "这份在同步文件夹（{provider}）里：SpaceKnife 不在这里建链接或换文件",
        "na_vcs_internal": "这是版本库的内部文件：SpaceKnife 不改这里",
        "na_in_package": "在 macOS 的包里：替换会损坏它",
        "na_no_trash": "这个卷没有回收站，也建不了 SpaceKnife 的恢复区",
        "na_no_recovery_area": "这个卷上建不了恢复区：原件没有地方放，不替换",
        "na_no_exchange": "这个文件系统不支持原子交换，SpaceKnife 不替换这里的文件",
        "na_folder_not_writable": "没有这个文件夹的写权限",
        "na_name_unusable": "名字以点或空格结尾，或者有只差大小写的同名项：按路径找到的可能不是它",
        "na_no_keeper_here": "这个卷上没有可以指向的保留点",
        "na_already_shared": "已经和保留那份共用全部数据块，替换不腾空间",
        "na_network": "网络盘、FUSE 上的文件不处理",
        "na_no_native": "需要 SpaceKnife 的原生模块（这个版本没有带）",
        "na_macos_off": "在 macOS 上没有验证过，默认关（设置里能打开）",
        "na_forced_keep": "必须保留：{why}",
        "na_stale": "核对之后文件变了，这一组要再查一次",
        "na_readonly_inplace": "这份是只读的：SpaceKnife 不改它的属性，就地克隆要写权限",
        "na_encrypted": "一份是加密的（EFS），不能共用数据块",
        "na_sparse_mismatch": "一份是稀疏文件、另一份不是",
        "na_in_basket": "它或保留那份在删除篮里：先从删除篮里拿掉",
        "na_keeper": "这是保留点",
        "na_not_owner": "就地共用数据块要是文件的主人或者有写权限",
        "na_risk_rule": "风险条件规则改成了不动",
        # where a decision came from
        "src_forced_needed": "程序需要或受保护：永远保留",
        "src_forced_alias": "和 {path} 是同一个文件（{n} 个名字）：只能保留",
        "src_forced_pinned": "名字以点或空格结尾，或另有硬链接不在扫描里：只能保留",
        "src_forced_vcs": "在版本库的内部文件夹里：不动",
        "src_forced_network": "网络盘上的文件：只保留",
        "src_manual_keep": "你手动选的",
        "src_manual_not_keep": "你手动设为不保留",
        "src_group_keep_set": "你给这组选的保留点",
        "src_pair_side": "文件夹对：保留 {side} 那边（规则「{a} ⇄ {b}」）",
        "src_pair_both": "这对文件夹两边都留",
        "src_keep_all": "在「全部保留」的 {folder} 里",
        "src_per_volume": "{vol} 上至少留一份",
        "src_per_folder": "每个文件夹各留一份",
        "src_keep_n": "每组至少留 {n} 份",
        "src_prefer": "在优先保留的 {folder} 里（第 {rank} 位）",
        "src_avoid": "只能留在不想留的 {folder} 里",
        "src_folder": "和同一对文件夹里的其他组留在同一边",
        "src_criterion": "按「{criterion}」保留",
        "src_tie": "一样时取路径短的",
        "src_safety_net": "这组本来一份都不留，按「每组至少留一份」留下这一份",
        "src_action_rule": "规则 {n}：{label}",
        "src_action_fallback": "规则要{want}，做不到（{why}）→ 后备：{act}",
        "src_action_preset": "预设「{preset}」：{act}",
        "src_action_manual": "你手动选的：{act}",
        "src_action_pair": "文件夹对规则：另一边{act}",
        "src_action_risk": "规则 {n}：有{risk}的 → 不动",
        "src_not_kept": "保留了组里的另一份",
        "src_rule": "规则",
        # criteria
        "crit_oldest_created": "最早创建",
        "crit_oldest_modified": "最早修改",
        "crit_newest_modified": "最新修改",
        "crit_shortest_path": "路径最短",
        "crit_longest_path": "路径最长",
        "crit_shallowest": "层数最少",
        "crit_deepest": "层数最多",
        "crit_plain_name": "名字不像副本",
        # presets
        "preset_watch": "只看，不动",
        "preset_conservative": "保守",
        "preset_recommended": "推荐",
        "preset_links": "链接优先",
        "preset_backup": "备份盘整理",
        "preset_custom": "自定义",
        "rule_sync_vcs_app": "同步盘、版本库、应用、程序、游戏、媒体库、卷根的隐藏临时文件、链接目标 → 不动",
        "rule_clone": "能就地共用数据块 → 块克隆",
        "rule_rest_trash": "其余 → 回收站",
        "rule_high_leave": "任何高风险 → 不动",
        "rule_same_volume_link": "同卷的 → 硬链接（文档、表格、代码、数据库类除外）",
        "rule_docs_trash": "文档、表格、代码、数据库类 → 回收站",
        "rule_other_link": "别的卷上的 → 符号链接到备份盘",
        "rule_keep_backup": "备份盘 {vol} 上的全部保留",
        "rule_all_leave": "全部不动",
        "rule_all_trash": "不保留的 → 回收站",
        # states of a group that is not in the plan
        "out_stale": "核对之后文件变了（要再查一次）",
        "out_shared": "同一份数据（删了不腾空间）",
        "out_no_extra": "没有多余的",
        "out_excluded": "这一组不动",
        "out_cleaned": "已清理",
        # gates
        "gate_facts": "还在核对权限和附加数据（{pct}%）",
        "gate_acks": "还有 {n} 类高风险没有确认",
        "gate_version": "方案在批准之后改过：再批准一次",
        "gate_job": "查找还在进行",
        "gate_scan": "涉及的卷上有扫描在进行",
        "gate_busy": "还有文件操作在进行，完成后再执行",
        "gate_attention": "上次的清理还有 {n} 项要你看一下（清理记录）",
        "gate_journal": "写不了清理日志 {path}，没有动任何文件",
        "gate_nothing": "方案里没有要做的事",
        "gate_unapproved": "还没有批准",
        # edits refused
        "refuse_last_keeper": "每组至少留一份：先选另一份做保留点",
        "refuse_volume": "{vol} 上至少要留一份（每卷一份开着）",
        "refuse_locked": "这一份必须保留",
        "refuse_stale": "核对之后文件变了，这一组要再查一次",
        "refuse_version": "方案已经变了（第 {now} 版）：刷新后再改",
        "refuse_target": "只能指向这组里合格的保留点",
        "refuse_risk_rule": "带风险条件的规则只能改成「不动」",
        "refuse_rule": "没有这条规则",
        "refuse_running": "清理正在执行：完成后再改方案",
        # misc
        "keep_line": "{how}：{path}",
        "keep_line_many": "保留 {n} 份：{how}",
        "relative": "相对路径",
        "absolute": "绝对路径",
        "unknown_owner": "（读不到）",
        "recovery_where": "原件放在恢复区 {path}",
        "recovery_in_place": "原件以隐藏的名字留在自己的文件夹里",
        "no_trash_recovery": "这个卷没有回收站：先放进 SpaceKnife 的恢复区",
        "list_sep": "、",
        "semi": "；",
        "comma": "，",
        "quoted": "「{name}」",
    }
)


def act_label(code: int, *, mac: bool = False) -> str:
    if code == A_TRASH and mac:
        return TEXT["act_trash_mac"]
    name = ACTION_NAME.get(code)
    return TEXT[f"act_{name}"] if name else TEXT["act_keep"]
