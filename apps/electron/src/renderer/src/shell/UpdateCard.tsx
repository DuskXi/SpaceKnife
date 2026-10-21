// The update card (updates.v1, state/updates.ts): the newest release, the start of its notes, and
// getting it: download in one click, then start the installer or show the file in its folder. The
// same card opens under the title bar's badge, hangs at the top left by itself for a minor release
// and fills the dialog of a major one. It never takes the focus, except as that dialog.

import { useEffect, useMemo, useRef, useState } from "react";

import type { UpdateDownload, UpdateInfo, UpdateStatus } from "@/protocol/generated";
import { cancelUpdate, closeUpdate, downloadUpdate, installUpdate } from "@/state/actions";
import { useApp } from "@/state/store";
import { downloading, downloadOf, downloadPct, hasUpdate, installAction, type InstallKind, releaseDate, summarizeNotes, visibleSurface } from "@/state/updates";
import { revealLabel } from "@/shell/Inspector";
import { fmtSize } from "@/lib/format";
import { t } from "@/lib/i18n";
import { Icon } from "@/ui/Icon";

export type UpdatePlacement = "anchored" | "corner" | "dialog";

/** The card that opens by itself: at the top left for a minor release, as a dialog for a major one. */
export function UpdatePrompt() {
  const status = useApp((s) => s.update);
  const surface = useApp((s) => visibleSurface(s.update, s.updateOpen));
  if (!hasUpdate(status) || (surface !== "corner" && surface !== "dialog")) return null;
  if (surface === "corner") return <UpdateCard status={status} placement="corner" />;
  // no closing by a click beside it: it may open while the user is clicking around
  return (
    <div className="modal-back" role="presentation">
      <UpdateCard status={status} placement="dialog" />
    </div>
  );
}

export function UpdateCard({ status, placement }: { status: UpdateStatus & { latest: UpdateInfo }; placement: UpdatePlacement }) {
  const latest = status.latest;
  const notes = useMemo(() => summarizeNotes(latest.notes), [latest.notes]);
  const date = releaseDate(latest.published_at);
  const dialog = placement === "dialog";
  const self = useRef<HTMLElement>(null);
  const titleId = `update-title-${placement}`;
  useEffect(() => {
    if (dialog) self.current?.focus(); // the dialog itself, not a button: Enter meant for the map does nothing here
  }, [dialog]);

  return (
    <section
      ref={self}
      className={`update-card ${placement}`}
      role="dialog"
      aria-modal={dialog || undefined}
      aria-labelledby={titleId}
      tabIndex={dialog ? -1 : undefined}
      onKeyDown={(e) => {
        if (e.key !== "Escape" || placement === "anchored") return; // under the badge: the badge closes it and takes the focus back
        e.stopPropagation(); // not also the map's Escape (clear the selection)
        closeUpdate();
      }}
    >
      <header className="uc-head">
        <span className="uc-icon">
          <Icon name="up" size={dialog ? 20 : 18} />
        </span>
        <div className="uc-title">
          {dialog && <span className="uc-kicker">{t("大版本更新")}</span>}
          <h3 id={titleId}>{t("SpaceKnife {version} 可用", { version: latest.version })}</h3>
          <div className="uc-sub">
            {date && <span>{t("{date} 发布", { date })}</span>}
            {date && <span className="uc-dot" aria-hidden="true" />}
            <span>{t("你在用 {current}", { current: status.current })}</span>
            {latest.prerelease && <span className="uc-tag">{t("预览版")}</span>}
          </div>
        </div>
        <button className="icon-btn small" aria-label={t("关闭")} title={placement === "anchored" ? t("关闭") : t("关闭（这个版本不再自动提示）")} onClick={closeUpdate}>
          <Icon name="close" size={14} />
        </button>
      </header>
      {notes.lines.length > 0 && (
        <div className="uc-notes">
          {notes.lines.map((line, i) => (
            <p key={i} className={`uc-note ${line.kind}${line.depth ? " deep" : ""}`}>
              {line.text}
            </p>
          ))}
          <button className="link small uc-more" onClick={() => window.sk.shell.openExternal(latest.url)}>
            {t("查看完整说明")}
            <Icon name="reveal" size={12} />
          </button>
        </div>
      )}
      <Download status={status} d={downloadOf(status)} />
    </section>
  );
}

/** What the backend's download is at, and the buttons for the next step. */
function Download({ status, d }: { status: UpdateStatus & { latest: UpdateInfo }; d: UpdateDownload }) {
  const latest = status.latest;
  // installing quits SpaceKnife: a scan still running asks once more first
  const scanning = useApp((s) => Object.values(s.scans).some((x) => x.state === "scanning" || x.state === "queued"));
  const [confirming, setConfirming] = useState(false);
  const asset = latest.asset;
  const openPage = () => window.sk.shell.openExternal(latest.url);
  const later = (
    <button className="text-btn uc-later" onClick={closeUpdate}>
      {t("以后再说")}
    </button>
  );
  const pageBtn = (
    <button className="ghost" onClick={openPage}>
      <Icon name="reveal" size={14} />
      {t("前往下载页")}
    </button>
  );

  if (downloading(d)) {
    const pct = downloadPct(d);
    const sizes = { received: fmtSize(d.received), total: fmtSize(d.total) };
    return (
      <>
        <div className="uc-body" aria-live="polite">
          <div className="uc-progress">
            <span>{d.state === "verifying" ? t("正在校验文件…") : d.total > 0 ? t("正在下载 {received} / {total}", sizes) : t("正在下载 {received}", sizes)}</span>
            {pct !== null && <span className="mono">{pct}%</span>}
          </div>
          <div
            className={`uc-bar${pct === null ? " indeterminate" : ""}`}
            role="progressbar"
            aria-label={t("下载进度")}
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={pct ?? undefined}
          >
            <span style={pct === null ? undefined : { width: `${pct}%` }} />
          </div>
        </div>
        <div className="uc-actions">
          <button className="ghost" onClick={() => void cancelUpdate()}>
            {t("取消")}
          </button>
        </div>
      </>
    );
  }

  if (d.state === "done" && d.path) {
    const path = d.path;
    const install = installAction(status.install_kind) === "install";
    const hint = installHint(status.install_kind);
    const reveal = () => window.sk.shell.showItemInFolder(path);
    return (
      <>
        <div className="uc-body">
          <div className="uc-line">
            <Icon name="check" size={14} />
            <span className="uc-file mono" title={path}>
              {path.split(/[\\/]/).pop() ?? path}
            </span>
          </div>
          {d.verified ? (
            <div className="uc-line ok">
              <Icon name="shield" size={14} />
              {t("SHA-256 校验通过")}
            </div>
          ) : (
            <div className="uc-line warn">
              <Icon name="warning" size={14} />
              {t("这个版本没有发布校验和，文件未经校验")}
            </div>
          )}
          {hint && (
            <div className="uc-line">
              <Icon name="info" size={14} />
              {hint}
            </div>
          )}
          {confirming && (
            <div className="uc-line warn">
              <Icon name="warning" size={14} />
              {t("还有扫描在进行，安装会中断它（已扫描的部分不会保存）")}
            </div>
          )}
        </div>
        <div className="uc-actions">
          {later}
          {install ? (
            <>
              <button className="ghost" onClick={reveal}>
                {revealLabel()}
              </button>
              <button className="primary" onClick={() => (scanning && !confirming ? setConfirming(true) : void installUpdate(path))}>
                <Icon name="play" size={14} />
                {confirming ? t("仍然安装") : t("安装")}
              </button>
            </>
          ) : (
            <button className="primary" onClick={reveal}>
              <Icon name="folder" size={14} />
              {revealLabel()}
            </button>
          )}
        </div>
      </>
    );
  }

  const failed = d.state === "failed";
  return (
    <>
      <div className="uc-body">
        {failed ? (
          <div className="uc-line error" role="alert">
            <Icon name="warning" size={14} />
            {d.error || t("下载失败")}
          </div>
        ) : asset ? (
          <div className="uc-line">
            <Icon name="file" size={14} />
            <span className="uc-file mono">{asset.name}</span>
            <span className="uc-size">{fmtSize(asset.size)}</span>
          </div>
        ) : (
          <div className="uc-line">
            <Icon name="info" size={14} />
            {t("这种安装方式请到发布页下载")}
          </div>
        )}
        {d.state === "cancelled" && <div className="uc-line">{t("已取消下载")}</div>}
      </div>
      <div className="uc-actions">
        {later}
        {pageBtn}
        <button
          className="primary uc-download"
          disabled={!asset}
          title={asset ? `${asset.name} · ${fmtSize(asset.size)}` : t("这种安装方式请到发布页下载")}
          onClick={() => void downloadUpdate(latest.version)}
        >
          <Icon name="up" size={14} />
          {failed ? t("重试") : t("一键下载")}
        </button>
      </div>
    </>
  );
}

/** How the downloaded file replaces the running copy, for this kind of install. */
function installHint(kind: InstallKind): string {
  switch (kind) {
    case "nsis":
      return t("安装程序会关闭 SpaceKnife 并替换旧版本，扫描数据和设置都会保留");
    case "dmg":
      return t("SpaceKnife 会退出并打开磁盘映像：把 SpaceKnife 拖进「应用程序」文件夹替换旧版本");
    case "deb":
    case "rpm":
      return t("SpaceKnife 会退出，并用系统的软件安装程序打开安装包；扫描数据和设置都会保留");
    case "portable":
      return t("退出 SpaceKnife 后解压，用新文件替换旧文件夹里的内容，但留下其中的 data 文件夹（扫描数据和设置都在里面）");
    case "appimage":
      return t("退出 SpaceKnife 后用新的 AppImage 替换旧文件；扫描数据和设置都会保留");
    default:
      return "";
  }
}
