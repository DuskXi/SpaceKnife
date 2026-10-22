"""What the enable dialog says about each permission (in the interface language)."""

from __future__ import annotations

from collections.abc import Sequence

from spaceknife_protocol.plugins import PermissionInfo

from ..i18n import Texts, _

PERMISSION_TEXT = Texts(
    {
        "tree.read": "读取扫描结果：文件名、路径、大小和时间",
        "files.read": "读取扫描范围内文件的内容",
        "snapshots.read": "读取保存的快照",
        "basket.propose": "把文件加入删除篮（真正删除仍需你确认）",
        "net": "连接网络：{hosts}",
        "exec": "运行外部程序：{programs}",
        "host.open": "打开或定位文件、打开终端、打开网页",
        "clipboard": "写入剪贴板",
    }
)


def permission_infos(
    perms: Sequence[str], hosts: Sequence[str], programs: Sequence[str]
) -> list[PermissionInfo]:
    out = []
    for p in perms:
        warn = False
        if p == "net":
            if "*" in hosts:
                text, warn = _("可以连接任意网络地址"), True
            else:
                text = PERMISSION_TEXT["net"].format(hosts=_("、").join(hosts))
        elif p == "exec":
            text, warn = PERMISSION_TEXT["exec"].format(programs=_("、").join(programs)), True
        else:
            text = PERMISSION_TEXT.get(p, p)
        out.append(PermissionInfo(id=p, text=text, warn=warn))  # type: ignore[arg-type]
    return out


__all__ = ["PERMISSION_TEXT", "permission_infos"]
