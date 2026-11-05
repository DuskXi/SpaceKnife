// Labels of the 「在资源管理器中显示」 action (shell/Inspector), per platform.

import { t } from "@/lib/i18n";

export const revealLabel = () =>
  window.sk.platform === "darwin" ? t("在访达中显示") : window.sk.platform === "win32" ? t("在资源管理器中显示") : t("在文件管理器中显示");

/** Items under the MFT pseudo folders have no folder of their own to open or delete from. */
export const notOnDisk = () => t("NTFS 元数据与孤立文件只在 MFT 快速扫描里可见，没有可以打开或删除的对应位置");
