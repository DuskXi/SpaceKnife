// The start page's scan sources of plugins (docs/plugins.md 「扫描来源入口」): besides folders and
// volumes, what enabled plugins can list as a tree (an archive, a remote listing…). A file or folder
// source opens the dialog for it, a text source takes what is typed; then scan.start {source, uri}.
// What a source can't do (open, locate, delete) is said under it, and refused later anyway.

import { useState } from "react";

import type { SourceInfo } from "@/protocol/generated";
import { type Tab, useApp } from "@/state/store";
import { t } from "@/lib/i18n";
import { Icon } from "@/ui/Icon";

import { PluginIcon } from "./icons";
import { pickUri, sourceNote, startSourceScan } from "./sources";
import { pluginName } from "./store";

export function SourcesPanel({ tab }: { tab: Tab }) {
  const sources = useApp((s) => s.extensions?.sources);
  const conn = useApp((s) => s.conn);
  if (!sources?.length) return null;
  return (
    <section className="start-options start-sources" aria-label={t("其他扫描来源")}>
      <h2>{t("其他扫描来源（插件）")}</h2>
      <div className="set-card">
        {sources.map((src) => (
          <SourceRow key={src.id} src={src} tab={tab} disabled={conn !== "ready"} />
        ))}
      </div>
    </section>
  );
}

function SourceRow({ src, tab, disabled }: { src: SourceInfo; tab: Tab; disabled: boolean }) {
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const run = async (uri: string | null) => {
    if (!uri) return;
    setBusy(true);
    try {
      await startSourceScan(src, uri, tab.id);
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="set-row source-row">
      <div className="set-text">
        <div className="set-name">
          <PluginIcon plugin={src.plugin} name={src.icon} />
          {src.title}
          <span className="muted small">{pluginName(src.plugin)}</span>
        </div>
        <div className="set-desc">{sourceNote(src)}</div>
      </div>
      <div className="set-control source-control">
        {src.pick === "text" ? (
          <>
            <input
              className="set-text-input"
              aria-label={src.title}
              placeholder={src.placeholder || t("输入要列出的位置")}
              value={text}
              disabled={disabled || busy}
              spellCheck={false}
              onChange={(e) => setText(e.target.value)}
              onKeyDown={(e) => {
                e.stopPropagation();
                if (e.key === "Enter" && !e.nativeEvent.isComposing) void run(text);
              }}
            />
            <button className="ghost small" disabled={disabled || busy || !text.trim()} onClick={() => void run(text)}>
              <Icon name="play" size={14} />
              {t("扫描")}
            </button>
          </>
        ) : (
          <button className="ghost small" disabled={disabled || busy} title={src.placeholder || undefined} onClick={async () => void run(await pickUri(src))}>
            <Icon name={src.pick === "folder" ? "folder" : "file"} size={14} />
            {busy ? t("正在开始…") : src.pick === "folder" ? t("选择文件夹…") : t("选择文件…")}
          </button>
        )}
      </div>
    </div>
  );
}
