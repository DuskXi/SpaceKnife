// 「关于与更新」 (settings page "about"): this copy of SpaceKnife and how it was installed, the update
// check with what it found (the update card itself when there is a newer release), the versions of
// the parts (copied in one go for a bug report), and the timeline of the recent releases with their
// notes. The timeline comes from the backend's cache (update.releases): it is there offline too and
// fills in after the first check.

import { useEffect, useMemo, useState } from "react";

import type { SystemInfo, UpdateInfo } from "@/protocol/generated";
import { PROTOCOL_VERSION } from "@/protocol/generated";
import { render } from "@/help/markdown";
import { call, checkUpdates, settingsGo } from "@/state/actions";
import { openHelpTab } from "@/state/help";
import { toast, useApp } from "@/state/store";
import { hasUpdate, installKindName, notesFor, releaseDate, seriesName } from "@/state/updates";
import { UpdateCard } from "@/shell/UpdateCard";
import { fmtAgo, fmtDateTime } from "@/lib/format";
import { t } from "@/lib/i18n";
import { Icon } from "@/ui/Icon";

const seconds = (iso: string | null | undefined): number => (iso ? Math.floor(new Date(iso).getTime() / 1000) || 0 : 0);

/** The top of the page: version, install, what the last check found and when the next one comes. */
export function AboutHeader() {
  const st = useApp((s) => s.update);
  const conn = useApp((s) => s.conn);
  const kind = st?.install_kind ?? "unknown";
  const checked = seconds(st?.checked_at);
  const next = seconds(st?.next_check_at);
  const status = !st
    ? t("这个后端不支持更新检查")
    : st.checking
      ? t("正在检查更新…")
      : st.error
        ? t("上次检查失败：{error}", { error: st.error })
        : hasUpdate(st)
          ? st.muted
            ? t("有新版本 {version}（已跳过 {series} 的提示）", { version: st.latest.version, series: seriesName(st.series ?? "") })
            : t("有新版本 {version}", { version: st.latest.version })
          : checked
            ? t("已是最新版本")
            : t("还没有检查过更新");
  return (
    <>
      <section className="about-head" aria-label={t("版本")}>
        <span className="about-logo">
          <Icon name="logo" size={28} />
        </span>
        <div className="about-title">
          <h2>SpaceKnife {window.sk.version}</h2>
          <div className="about-sub">
            <span>{installKindName(kind)}</span>
            {st && <span className="uc-dot" aria-hidden="true" />}
            {st && <span>{st.channel === "stable" ? t("只要正式版") : t("包括预览版")}</span>}
          </div>
          <div className={`about-status${st?.error ? " warn" : hasUpdate(st) ? " new" : checked ? "" : " idle"}`} aria-live="polite">
            <Icon name={st?.error ? "warning" : hasUpdate(st) ? "up" : checked ? "check" : "info"} size={14} />
            {status}
          </div>
          <div className="about-times muted small">
            {checked > 0 && <span title={fmtDateTime(checked)}>{t("上次检查：{when}", { when: fmtAgo(checked) })}</span>}
            {next > 0 && <span>{t("下次自动检查：{when}", { when: fmtDateTime(next) })}</span>}
            {st && !st.enabled && <span>{t("自动检查已关闭")}</span>}
          </div>
        </div>
        <div className="about-actions">
          <button className="ghost" disabled={!st || st.checking || conn !== "ready"} onClick={() => void checkUpdates()}>
            <Icon name="refresh" size={14} />
            {t("检查更新")}
          </button>
          <button className="ghost" onClick={() => window.sk.shell.openExternal("https://github.com/DuskXi/SpaceKnife/releases")}>
            <Icon name="reveal" size={14} />
            {t("全部发布")}
          </button>
        </div>
      </section>
      {hasUpdate(st) && (
        <div className="about-update">
          <UpdateCard status={st} placement="inline" />
        </div>
      )}
    </>
  );
}

/** The versions of the parts, as a settings group; one button copies them all. */
export function ComponentsGroup() {
  const conn = useApp((s) => s.conn);
  const kind = useApp((s) => s.update?.install_kind ?? "unknown");
  const [sys, setSys] = useState<SystemInfo | null>(null);
  useEffect(() => {
    if (conn !== "ready") return;
    let alive = true;
    call("system.info", {})
      .then((r) => alive && setSys(r))
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, [conn]);
  const v = window.sk.versions;
  const rows: [string, string][] = [
    ["SpaceKnife", window.sk.version],
    [t("安装方式"), installKindName(kind)],
    ["Electron", v.electron || "—"],
    ["Chromium", v.chrome || "—"],
    ["Node.js", v.node || "—"],
    ["Python", sys?.server.python ?? "—"],
    [t("协议"), PROTOCOL_VERSION],
    [t("系统"), sys ? `${sys.platform.system} ${sys.platform.release} (${sys.platform.machine})` : "—"],
  ];
  const copy = () => {
    window.sk.shell.copyText(rows.map(([k, val]) => `${k}: ${val}`).join("\n"));
    toast(t("已复制版本信息"));
  };
  return (
    <section className="set-group" aria-label={t("组件版本")}>
      <div className="set-group-head">
        <h2>{t("组件版本")}</h2>
        <button className="link small" onClick={copy}>
          <Icon name="copy" size={12} />
          {t("复制版本信息")}
        </button>
      </div>
      <div className="set-card about-versions">
        {rows.map(([k, val]) => (
          <div key={k} className="about-version-row">
            <span className="muted">{k}</span>
            <span className="mono">{val}</span>
          </div>
        ))}
      </div>
    </section>
  );
}

/** The recent releases, newest first, each with its notes in the interface language (the newest
 * one open). */
export function ReleaseTimeline() {
  const checkedAt = useApp((s) => s.update?.checked_at ?? null);
  const current = useApp((s) => s.update?.current) || window.sk.version;
  const lang = useApp((s) => s.lang);
  const conn = useApp((s) => s.conn);
  const [releases, setReleases] = useState<UpdateInfo[] | null>(null);
  const [open, setOpen] = useState<Set<string>>(new Set());
  useEffect(() => {
    if (conn !== "ready") return;
    let alive = true;
    call("update.releases", {})
      .then((r) => {
        if (!alive) return;
        setReleases(r.releases);
        setOpen((was) => (was.size || !r.releases[0] ? was : new Set([r.releases[0].version])));
      })
      .catch(() => alive && setReleases([]));
    return () => {
      alive = false;
    };
  }, [checkedAt, conn]);
  const here = releases?.findIndex((r) => r.version === current) ?? -1;

  return (
    <section className="about-timeline" aria-label={t("发布时间线")}>
      <div className="set-group-head">
        <h2>{t("发布时间线")}</h2>
        {releases && releases.length > 0 && <span className="muted small">{t("最近 {n} 个版本", { n: releases.length })}</span>}
      </div>
      {releases === null ? (
        <div className="muted small">{t("正在读取…")}</div>
      ) : releases.length === 0 ? (
        <div className="set-card about-empty">
          <span className="muted">{t("还没有发布记录：检查一次更新后，这里会列出最近的版本和它们的更新内容")}</span>
          <button className="ghost" disabled={conn !== "ready"} onClick={() => void checkUpdates()}>
            {t("检查更新")}
          </button>
        </div>
      ) : (
        <ol className="tl">
          {releases.map((r, i) => (
            <Release
              key={r.tag}
              release={r}
              lang={lang}
              current={r.version === current}
              newer={here > 0 && i < here}
              open={open.has(r.version)}
              onToggle={() =>
                setOpen((was) => {
                  const next = new Set(was);
                  if (!next.delete(r.version)) next.add(r.version);
                  return next;
                })
              }
            />
          ))}
        </ol>
      )}
    </section>
  );
}

function Release(props: { release: UpdateInfo; lang: "zh-CN" | "en"; current: boolean; newer: boolean; open: boolean; onToggle: () => void }) {
  const { release: r, lang, current, newer, open, onToggle } = props;
  const html = useMemo(() => (open ? render(notesFor(r.notes, lang), lang === "zh-CN" ? "zh" : "en", "index").html : ""), [open, r.notes, lang]);
  const notesId = `tl-notes-${r.tag}`;
  const onClick = (e: React.MouseEvent) => {
    const a = (e.target as HTMLElement).closest("a");
    if (!a) return;
    e.preventDefault();
    const doc = a.getAttribute("data-doc");
    if (doc) openHelpTab(doc, a.getAttribute("data-anchor"));
    else if (a.hasAttribute("data-external")) window.sk.shell.openExternal(a.getAttribute("href") ?? "");
  };
  return (
    <li className={`tl-item${current ? " current" : ""}${newer ? " newer" : ""}`}>
      <span className="tl-dot" aria-hidden="true" />
      <button className="tl-head" aria-expanded={open} aria-controls={notesId} onClick={onToggle}>
        <span className="tl-version">{r.version}</span>
        {current && <span className="tl-tag current">{t("当前")}</span>}
        {newer && <span className="tl-tag new">{t("新")}</span>}
        {r.prerelease && <span className="tl-tag">{t("预览版")}</span>}
        <span className="flex" />
        <span className="tl-date mono">{releaseDate(r.published_at)}</span>
        <Icon name={open ? "chevronDown" : "chevronRight"} size={14} />
      </button>
      {open && (
        <div id={notesId} className="tl-notes">
          {html ? (
            <div className="help-body" onClick={onClick} onAuxClick={(e) => (e.target as HTMLElement).closest("a") && e.preventDefault()} dangerouslySetInnerHTML={{ __html: html }} />
          ) : (
            <div className="muted small">{t("这个版本没有发布说明")}</div>
          )}
          <button className="link small" onClick={() => window.sk.shell.openExternal(r.url)}>
            {t("在 GitHub 上查看")}
            <Icon name="reveal" size={12} />
          </button>
        </div>
      )}
    </li>
  );
}

/** 「常规」's way to the page: the version and whether a newer one is known, one click away. */
export function AboutEntry() {
  const st = useApp((s) => s.update);
  const desc = hasUpdate(st) ? t("有新版本 {version}", { version: st.latest.version }) : t("版本信息、更新设置与发布时间线");
  return (
    <section className="set-group" aria-label={t("关于")}>
      <div className="set-group-head">
        <h2>{t("关于")}</h2>
      </div>
      <div className="set-card">
        <div className="set-row">
          <div className="set-text">
            <div className="set-name">SpaceKnife {window.sk.version}</div>
            <div className="set-desc">{desc}</div>
          </div>
          <div className="set-control">
            <button className="ghost" onClick={() => settingsGo("about")}>
              {t("关于与更新")}
              <Icon name="chevronRight" size={14} />
            </button>
          </div>
        </div>
      </div>
    </section>
  );
}
