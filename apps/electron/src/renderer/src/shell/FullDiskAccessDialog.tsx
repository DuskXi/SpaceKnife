// macOS without Full Disk Access (spaceknife.privacy): before a scan, grant it once in System
// Settings, leave the consent-guarded places out, or let macOS ask for each of them.

import { useState } from "react";

import { t } from "@/lib/i18n";
import { answerProtectedPlaces, type ProtectedChoice } from "@/state/actions";
import { useApp } from "@/state/store";

export function FullDiskAccessDialog() {
  const open = useApp((s) => s.fdaPrompt);
  const [remember, setRemember] = useState(false);
  if (!open) return null;
  const answer = (choice: ProtectedChoice) => {
    answerProtectedPlaces(choice, remember);
    setRemember(false);
  };
  return (
    <div className="modal-back" role="presentation" onMouseDown={() => answer("cancel")}>
      <div
        className="modal fda-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="fda-title"
        onMouseDown={(e) => e.stopPropagation()}
        onKeyDown={(e) => e.key === "Escape" && answer("cancel")}
      >
        <h3 id="fda-title">{t("让 SpaceKnife 一次读到全部文件")}</h3>
        <p className="muted">{t("macOS 会为桌面、文稿、下载、iCloud 云盘和其他云盘、照片图库以及其他应用的数据分别询问权限，扫描时会一个接一个弹窗。")}</p>
        <p className="muted">{t("开启「完全磁盘访问权限」可以一次授权全部：在系统设置的列表里打开 SpaceKnife 的开关（没有就点 + 添加），再按提示重新打开 SpaceKnife。")}</p>
        <label className="export-check">
          <input type="checkbox" checked={remember} onChange={(e) => setRemember(e.target.checked)} />
          {t("记住「跳过」或「逐个询问」的选择（可在设置 › 扫描范围里修改）")}
        </label>
        <div className="modal-buttons">
          <button className="ghost" onClick={() => answer("allow")}>
            {t("照常扫描，逐个询问")}
          </button>
          <button className="ghost" onClick={() => answer("skip")}>
            {t("跳过受保护的位置")}
          </button>
          <button className="primary" autoFocus onClick={() => answer("settings")}>
            {t("打开系统设置")}
          </button>
        </div>
      </div>
    </div>
  );
}
