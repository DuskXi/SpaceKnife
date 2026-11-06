//! User-facing texts of the file operations (the Chinese source text is the message id, see
//! `i18n`), by key, as in the Python modules' `TEXT` tables.

use crate::i18n::{Arg, format, t};

/// `ops.paths` reasons (keys of [`super::paths::match_rules`]).
pub(crate) fn path_text(key: &str) -> &'static str {
    t(match key {
        "root" => "卷的根目录不能删除",
        "mount" => "这里挂载着另一个卷，不能删除",
        "system" => "系统目录，删除会损坏操作系统",
        "boot" => "启动与恢复所需的文件，删除后系统可能无法启动",
        "system_file" => "系统的分页 / 休眠 / 交换文件，由系统管理",
        "program_root" => "程序安装位置的根目录，只能删除其中的单个程序",
        "data_root" => "系统级应用数据的根目录，只能删除其中的单项",
        "shared_root" => "系统共享目录本身，只能删除其中的内容",
        "users_root" => "存放所有用户主目录的文件夹",
        "home" => "用户主目录本身不能删除",
        "profile" => "用户的应用配置与数据目录本身，只能删除其中的单项",
        "trash" => "回收站目录，请用系统的「清空回收站」",
        "package_db" => "软件包数据库，删除会损坏包管理器",
        "app" => "SpaceKnife 自己的数据目录",
        "program" => "SpaceKnife 正在运行的程序所在目录",
        _ => "",
    })
}

/// Every key [`path_text`] knows.
pub const PATH_KEYS: &[&str] = &[
    "root",
    "mount",
    "system",
    "boot",
    "system_file",
    "program_root",
    "data_root",
    "shared_root",
    "users_root",
    "home",
    "profile",
    "trash",
    "package_db",
    "app",
    "program",
];

/// `ops.trash` texts.
pub(crate) fn trash_text(key: &str) -> &'static str {
    t(match key {
        "trashed" => "已移到回收站",
        "restored" => "已还原",
        "deleted" => "已永久删除",
        "missing" => "已不存在",
        "denied" => "没有权限",
        "locked" => "正在被 {names} 使用，需先退出",
        "locked_unknown" => "正被其他程序使用，需先关闭",
        "cancelled" => "已取消",
        "cancelled_partly" => "已取消，其中一部分已删除",
        "error" => "操作失败：{detail}",
        "partly" => "部分内容未能删除：{detail}",
        "mount_inside" => "其中有另一个卷的挂载点，未进入",
        "no_bin_remote" => "网络位置没有回收站，只能永久删除",
        "no_bin_removable" => "可移动磁盘没有回收站，只能永久删除",
        "no_bin_volume" => "这个卷没有回收站，只能永久删除",
        "no_bin_nuke" => "这个卷的回收站设置为「不移到回收站，直接删除」，只能永久删除",
        "too_big" => "超出回收站容量（{size}，回收站最多 {limit}），只能永久删除",
        "too_long" => "路径过长，回收站放不下，只能永久删除",
        "refused" => "系统准备直接删除而不是移到回收站，已阻止，未做任何改动",
        "destroyed" => "未在回收站中找到，可能已被系统直接删除",
        "no_trash_dir" => "这个卷上没有可用的回收站目录，只能永久删除",
        "cross_mount" => "回收站与它不在同一个挂载点，无法移入",
        "no_name" => "回收站中找不到可用的名字",
        "not_moved" => "未能移到回收站（系统没有说明原因），未做改动",
        "unsupported" => "这个系统不支持移到回收站",
        "exists" => "原位置已有同名项目，未覆盖",
        "ambiguous_name" => "另一项与它只差大小写，按名字会打开另一项；没有改动任何东西",
        "trailing_name" => {
            "它或上级文件夹的名字以点或空格结尾，Windows 按路径找到的不是它；没有改动任何东西"
        }
        "bad_ref" => "不是可还原的回收站条目",
        "gone_from_trash" => "回收站中已没有这一项（可能已被清空）",
        "list_sep" => "、",
        _ => "",
    })
}

/// `ops.basket` texts.
pub(crate) fn basket_text(key: &str) -> &'static str {
    t(match key {
        "group_name" => "{folder} 中 {count} 个小项",
        "bad_node" => "扫描结果中没有这一项",
        "virtual" => "MFT 伪文件夹中的项不在磁盘上",
        "bad_name" => "它或上级文件夹的名字里有路径不能有的字符，没有路径能找到它",
        "dup_name" => "它或上级文件夹与同一文件夹里的另一项名字完全相同，按路径找到的可能是另一项",
        "trailing_name" => "它或上级文件夹的名字以点或空格结尾，Windows 按路径找到的不是它",
        "not_folder" => "小项聚合块的所在位置不是文件夹",
        "not_member" => "不是所在文件夹的直接子项",
        "missing" => "已不存在",
        "members_missing" => "其中 {count} 项已不存在",
        "locked" => "正在被 {names} 使用，需先退出",
        "links_file" => "另有 {count} 个硬链接不在删除篮中，删除后不释放空间",
        "links_dir" => "其中 {count} 个文件在别处还有硬链接，删除后不释放这部分空间",
        "failed" => "{message}",
        "members_failed" => "{count} 项未完成：{message}",
        "list_sep" => "、",
        "note_sep" => "；",
        _ => "",
    })
}

/// A template filled with `{name}` fields.
pub(crate) fn fill(template: &str, fields: &[(&str, Arg)]) -> String {
    format(template, fields)
}
