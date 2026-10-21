// Browser mode (headless): what a browser tab adds to the interface. The path dialog (a browser can't
// show the server's folders, shim.ts asks here), a question before the tab closes while a scan or a
// deletion runs or scan tabs are open (Ctrl+W closes the browser's tab, not SpaceKnife's), the tab's
// title naming the computer, and the title bar's chip saying which computer the files are on.

import { useEffect, useRef, useState, useSyncExternalStore } from "react";

import { inBrowser, serverName } from "@/lib/host";
import { t } from "@/lib/i18n";
import { answerPath, onPathChange, shownPath } from "@/state/browser";
import { useApp } from "@/state/store";
import { Icon } from "@/ui/Icon";

export function BrowserLayer() {
  return inBrowser() ? <Browser /> : null;
}

function Browser() {
  const busy = useApp(
    (s) => Object.values(s.scans).some((x) => x.state === "scanning" || x.state === "queued") || !!(s.op && (s.op.phase === "running" || s.op.phase === "waiting")),
  );
  const scanTabs = useApp((s) => s.tabs.some((tab) => tab.kind === "scan"));
  const name = serverName();

  useEffect(() => {
    document.documentElement.classList.add("sk-browser");
    document.title = name ? `SpaceKnife · ${name}` : "SpaceKnife";
  }, [name]);

  useEffect(() => {
    if (!busy && !scanTabs) return;
    const guard = (e: BeforeUnloadEvent) => {
      e.preventDefault();
      e.returnValue = ""; // older browsers ask only with this set
    };
    window.addEventListener("beforeunload", guard);
    return () => window.removeEventListener("beforeunload", guard);
  }, [busy, scanTabs]);

  return <PathDialog />;
}

/** The title bar's chip: SpaceKnife on which computer, and whether the connection is encrypted. */
export function RemoteChip() {
  const remote = window.sk.remote;
  if (!remote) return null;
  // plain HTTP matters once it leaves this computer, not through localhost
  const plain = !remote.secure && !isLoopback(location.hostname);
  const tip = [
    t("这是 {host} 上的 SpaceKnife：扫描、移到回收站和删除，都作用于那台电脑上的文件", { host: remote.hostname }),
    remote.secure ? t("连接经 HTTPS 加密") : plain ? t("明文 HTTP：同一网络上的人能看到传输的内容") : "",
  ]
    .filter(Boolean)
    .join("\n");
  return (
    <span className={`remote-chip${plain ? " plain" : ""}`} title={tip} aria-label={tip}>
      <Icon name="globe" size={13} />
      <span className="remote-name">{remote.hostname}</span>
      {plain && <span className="remote-plain">HTTP</span>}
    </span>
  );
}

/** localhost, 127.x.x.x, [::1]: the page and the server are on one computer. */
export const isLoopback = (hostname: string): boolean => /^(localhost|127\.\d+\.\d+\.\d+|\[::1\])$/i.test(hostname);

function PathDialog() {
  const request = useSyncExternalStore(onPathChange, shownPath);
  const [value, setValue] = useState("");
  const input = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (!request) return;
    setValue(request.value);
    requestAnimationFrame(() => {
      const el = input.current;
      if (!el) return;
      el.focus();
      el.setSelectionRange(el.value.length, el.value.length);
    });
  }, [request]);

  if (!request) return null;
  return (
    <div className="modal-back" role="presentation" onMouseDown={() => answerPath(null)}>
      <form
        className="modal path-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="path-dialog-title"
        onMouseDown={(e) => e.stopPropagation()}
        onSubmit={(e) => {
          e.preventDefault();
          answerPath(value);
        }}
      >
        <h3 id="path-dialog-title">{request.title}</h3>
        <p className="muted small">{request.hint}</p>
        <input
          ref={input}
          className="path-dialog-input mono"
          value={value}
          spellCheck={false}
          autoComplete="off"
          aria-label={request.hint}
          onChange={(e) => setValue(e.target.value)}
          onKeyDown={(e) => {
            e.stopPropagation(); // typing is not a shortcut
            if (e.key === "Escape") answerPath(null);
          }}
        />
        <div className="modal-buttons">
          <button type="button" className="ghost" onClick={() => answerPath(null)}>
            {t("取消")}
          </button>
          <button type="submit" className="primary" disabled={!value.trim()}>
            {t("确定")}
          </button>
        </div>
      </form>
    </div>
  );
}
