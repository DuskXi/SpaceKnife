// The update card (updates.v1, state/updates.ts): the newest release, the start of its notes, and
// getting it: the download (by itself with 「自动下载更新」, else one click), then two big buttons
// (shared/install.ts): 「无缝更新」 (SpaceKnife puts the new version in place itself and starts again)
// and the classic way (the installer with its pages, the disk image or package opened, or the file
// shown in its folder). Nothing is ever installed without one of those clicks. 「跳过 0.4.x」 keeps a
// whole series quiet (update.mute). The same card opens under the title bar's badge, hangs at the
// top left by itself for a minor release and fills the dialog of a major one. It never takes the
// focus, except as that dialog.

import { useEffect, useMemo, useRef, useState } from "react";

import type { UpdateDownload, UpdateInfo, UpdateStatus } from "@/protocol/generated";
import { cancelUpdate, closeUpdate, downloadUpdate, installUpdate, muteUpdates } from "@/state/actions";
import { useApp } from "@/state/store";
import { downloading, downloadOf, downloadPct, hasUpdate, type InstallKind, installChoices, notesFor, releaseDate, seriesName, summarizeNotes, visibleSurface } from "@/state/updates";
import { type InstallHow, type InstallStep, stepQuits } from "../../../shared/install";
import { revealLabel } from "@/shell/Inspector";
import { fmtSize } from "@/lib/format";
import { t } from "@/lib/i18n";
import { inBrowser, serverName } from "@/lib/host";
import { Icon } from "@/ui/Icon";

/** "inline": in the 「关于与更新」 page, where it stays (no closing, no 「以后再说」) */
export type UpdatePlacement = "anchored" | "corner" | "dialog" | "inline";

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
  const lang = useApp((s) => s.lang);
  // the notes are English, then 简体中文: the start of the part in the interface language
  const notes = useMemo(() => summarizeNotes(notesFor(latest.notes, lang)), [latest.notes, lang]);
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
        {placement !== "inline" && (
          <button className="icon-btn small" aria-label={t("关闭")} title={placement === "anchored" ? t("关闭") : t("关闭（这个版本不再自动提示）")} onClick={closeUpdate}>
            <Icon name="close" size={14} />
          </button>
        )}
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
      {status.muted && status.series && (
        <div className="uc-muted">
          <Icon name="info" size={14} />
          <span>{t("已跳过 {series} 的版本提示：不弹出、不显示徽章、不自动下载", { series: seriesName(status.series) })}</span>
          <button className="link small" onClick={() => muteUpdates(status.series!, false)}>
            {t("恢复提示")}
          </button>
        </div>
      )}
      <Download status={status} d={downloadOf(status)} inline={placement === "inline"} />
    </section>
  );
}

/** What the backend's download is at, and the buttons for the next step. */
function Download({ status, d, inline }: { status: UpdateStatus & { latest: UpdateInfo }; d: UpdateDownload; inline: boolean }) {
  const latest = status.latest;
  // installing quits SpaceKnife: a scan still running asks once more first
  const scanning = useApp((s) => Object.values(s.scans).some((x) => x.state === "scanning" || x.state === "queued"));
  const [confirming, setConfirming] = useState<InstallHow | null>(null);
  const asset = latest.asset;
  const openPage = () => window.sk.shell.openExternal(latest.url);
  const series = status.series;
  const later = inline ? null : (
    <span className="uc-later">
      <button className="text-btn" onClick={closeUpdate}>
        {t("以后再说")}
      </button>
      {series && !status.muted && (
        <button
          className="text-btn"
          title={t("{series} 的版本不再弹出提示、不显示徽章，也不自动下载；可以在「关于与更新」里恢复", { series: seriesName(series) })}
          onClick={() => muteUpdates(series, true)}
        >
          {t("跳过 {series}", { series: seriesName(series) })}
        </button>
      )}
    </span>
  );
  const pageBtn = (
    <button className="ghost" onClick={openPage}>
      <Icon name="reveal" size={14} />
      {t("前往下载页")}
    </button>
  );

  if (inBrowser()) {
    // the update is for the computer SpaceKnife runs on: nothing to download or install from a tab
    return (
      <>
        <div className="uc-body">
          <div className="uc-line">
            <Icon name="info" size={14} />
            {t("请在 {host} 上更新 SpaceKnife：浏览器里不能下载或安装更新", { host: serverName() })}
          </div>
        </div>
        <div className="uc-actions">
          {later}
          {pageBtn}
        </div>
      </>
    );
  }

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
    const kind = status.install_kind;
    const choices = installChoices(kind);
    const reveal = () => window.sk.shell.showItemInFolder(path);
    // closing SpaceKnife stops a running scan: the first click says so, the second goes on
    const go = (how: InstallHow, step: InstallStep) =>
      scanning && stepQuits(step) && confirming !== how ? setConfirming(how) : void installUpdate(path, how);
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
          {choices.length > 0 && (
            <div className="uc-line">
              <Icon name="info" size={14} />
              {t("选一种方式安装：只有你点了才会开始，扫描数据和设置都会保留")}
            </div>
          )}
          {confirming && (
            <div className="uc-line warn">
              <Icon name="warning" size={14} />
              {t("还有扫描在进行，更新会中断它（已扫描的部分不会保存）")}
            </div>
          )}
        </div>
        {choices.length > 0 && (
          <div className="uc-choices">
            {choices.map(({ how, step }) => (
              <button key={how} className={`uc-choice ${how}${confirming === how ? " confirm" : ""}`} onClick={() => go(how, step)}>
                <Icon name={how === "seamless" ? "bolt" : step === "reveal" ? "folder" : "play"} size={18} />
                <span className="uc-choice-text">
                  <strong>{choiceTitle(kind, step)}</strong>
                  <span>{confirming === how ? t("再点一次继续") : choiceHint(kind, step)}</span>
                </span>
              </button>
            ))}
          </div>
        )}
        <div className="uc-actions">
          {later}
          {!choices.some((c) => c.step === "reveal") && (
            <button className={choices.length > 0 ? "ghost" : "primary"} onClick={reveal}>
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

/** The big button's title: 「无缝更新」, or what the classic way opens. */
function choiceTitle(kind: InstallKind, step: InstallStep): string {
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
function choiceHint(kind: InstallKind, step: InstallStep): string {
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
