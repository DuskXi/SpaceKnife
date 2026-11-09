// The update card's install buttons (components/shell/UpdateCard.vue): what each one is called and
// what it does, for a kind of install and the step it runs (shared/install.ts).

import { t } from "@/lib/i18n";
import type { InstallStep } from "@/shared/install";
import type { InstallKind } from "@/state/updates";

/** The big button's title: 「无缝更新」, or what the classic way opens. */
export function choiceTitle(kind: InstallKind, step: InstallStep): string {
  switch (step) {
    case "silent":
    case "swap":
    case "replace":
      return t("无缝更新");
    case "wizard":
      return t("运行安装程序");
    case "open":
      return kind === "dmg" ? t("打开磁盘映像") : t("打开安装包");
    case "reveal":
      return t("自己替换");
  }
}

/** What the button does, in one line under its title. */
export function choiceHint(kind: InstallKind, step: InstallStep): string {
  switch (step) {
    case "silent":
      return t("SpaceKnife 关闭，在后台装好新版本后自动重新打开");
    case "swap":
      return t("SpaceKnife 关闭，由它自己换上新版本的程序文件，再重新打开；data 文件夹原样保留");
    case "replace":
      return t("SpaceKnife 关闭，换上新的 AppImage 后重新打开");
    case "wizard":
      return t("SpaceKnife 退出并打开安装向导，按步骤装好新版本");
    case "open":
      return kind === "dmg"
        ? t("SpaceKnife 退出并打开磁盘映像：把 SpaceKnife 拖进「应用程序」替换旧版本")
        : t("SpaceKnife 退出，用系统的软件安装程序打开安装包");
    case "reveal":
      return kind === "appimage"
        ? t("在文件夹中显示新的 AppImage：退出 SpaceKnife 后用它替换旧文件")
        : t("在文件夹中显示压缩包：退出 SpaceKnife 后自己解压覆盖，保留 data 文件夹");
  }
}
