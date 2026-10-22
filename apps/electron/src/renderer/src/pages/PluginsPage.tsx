// Settings › 插件 (docs/plugins.md 「设置页与插件页」): the installed plugins with their state and a
// switch each; opened, what a plugin is, may do and adds, its folder and fingerprint, what went
// wrong, and restart / uninstall. Enabling a plugin for the first time (or after its files changed)
// shows its permissions first and needs a yes; the backend then checks that the files are still the
// ones shown. Installing from a folder or a .zip never enables. The plugins' own settings follow as
// groups of this page (settings meta, category "plugins").

import { useState } from "react";

import type { PluginDescribeResult, PluginInfo } from "@/protocol/generated";
import { RpcError } from "@/protocol/rpc";
import { call } from "@/state/actions";
import { set, toast, useApp } from "@/state/store";
import { fmtInt, fmtSize, plural } from "@/lib/format";
import { inBrowser } from "@/lib/host";
import { byLang, t } from "@/lib/i18n";
import { Icon } from "@/ui/Icon";
import { ask } from "@/plugins/confirm";
import { PluginIcon } from "@/plugins/icons";
import { putPlugin, refreshPlugins } from "@/plugins/store";

type State = PluginInfo["state"];

/** The tag of a plugin's state. */
export function stateLabel(state: State, dev = false): { text: string; tone: "ok" | "warn" | "danger" | "muted" } {
  const tag = ((): { text: string; tone: "ok" | "warn" | "danger" | "muted" } => {
    switch (state) {
      case "enabled":
        return { text: t("已启用"), tone: "ok" };
      case "disabled":
        return { text: t("未启用"), tone: "muted" };
      case "needs_confirm":
        return { text: t("需要确认"), tone: "warn" };
      case "changed":
        return { text: t("文件已改动"), tone: "warn" };
      case "incompatible":
        return { text: t("不兼容"), tone: "danger" };
      case "crashed":
        return { text: t("已崩溃"), tone: "danger" };
      default:
        return { text: t("出错"), tone: "danger" };
    }
  })();
  return dev && state === "enabled" ? { text: t("开发中"), tone: "warn" } : tag;
}

const errText = (err: unknown) => (err instanceof RpcError ? err.message : String(err));

export function PluginsPage() {
  const list = useApp((s) => s.plugins);
  const open = useApp((s) => s.pluginOpen);
  const conn = useApp((s) => s.conn);
  const on = useApp((s) => s.capabilities.includes("plugins.v1"));
  const [confirm, setConfirm] = useState<PluginDescribeResult | null>(null);
  const [busy, setBusy] = useState("");
  if (!on) return <div className="settings-banner warn">{t("这个后端不支持插件")}</div>;
  const plugins = list?.plugins ?? [];

  const install = async (path: string | null) => {
    if (!path) return;
    setBusy("install");
    try {
      let r = await call("plugin.install", { path });
      if (r.conflict) {
        const yes = await ask({
          title: t("替换已安装的插件？"),
          body: [t("已经装有「{id}」{old}，要换成 {new} 吗？旧版本会移到插件文件夹的 .old 里。", { id: r.id, old: r.existing_version, new: r.version })],
          ok: t("替换"),
        });
        if (!yes) return;
        r = await call("plugin.install", { path, replace: true });
      }
      await refreshPlugins();
      set({ pluginOpen: r.id });
      toast(t("已安装「{id}」{version}：启用前会先列出它要的权限", { id: r.id, version: r.version }), 4000);
    } catch (err) {
      toast(t("安装失败：{error}", { error: errText(err) }), 5000);
    } finally {
      setBusy("");
    }
  };

  const link = async () => {
    const [path] = await window.sk.dialog.openFolders();
    if (!path) return;
    try {
      const info = await call("plugin.link", { path });
      await refreshPlugins();
      set({ pluginOpen: info.id });
    } catch (err) {
      toast(t("链接失败：{error}", { error: errText(err) }), 5000);
    }
  };

  /** The switch: on asks first (plugin.describe → the dialog), off at once. */
  const toggle = async (p: PluginInfo) => {
    if (p.builtin) return;
    setBusy(p.id);
    try {
      if (p.state === "enabled") putPlugin(await call("plugin.disable", { id: p.id }));
      else setConfirm(await call("plugin.describe", { id: p.id }));
    } catch (err) {
      toast(errText(err), 5000);
    } finally {
      setBusy("");
    }
  };

  const enable = async (d: PluginDescribeResult) => {
    setConfirm(null);
    try {
      putPlugin(await call("plugin.enable", { id: d.plugin.id, hash: d.plugin.hash, grant: d.plugin.permissions.map((x) => x.id) }));
      toast(t("已启用「{name}」", { name: d.plugin.name }));
    } catch (err) {
      toast(t("没有启用：{error}", { error: errText(err) }), 5000);
      void refreshPlugins();
    }
  };

  return (
    <section className="set-group plugins-group" aria-label={t("已安装的插件")}>
      <div className="set-group-head">
        <h2>{t("已安装的插件")}</h2>
        <span className="muted small">{plural(plugins.length, t("{n} 个", { n: fmtInt(plugins.length) }), "1 plugin")}</span>
      </div>
      <div className="plugins-tools">
        <button
          className="ghost small"
          disabled={conn !== "ready" || !!busy}
          onClick={async () => void install((await window.sk.dialog.openFolders())[0] ?? null)}
        >
          <Icon name="folder" size={14} />
          {t("从文件夹安装…")}
        </button>
        <button
          className="ghost small"
          disabled={conn !== "ready" || !!busy}
          onClick={async () => void install(await window.sk.dialog.openFile({ title: t("选择插件压缩包"), filters: [{ name: t("插件压缩包"), extensions: ["zip"] }] }))}
        >
          <Icon name="upload" size={14} />
          {t("从 zip 安装…")}
        </button>
        {list?.dev_mode && (
          <button className="ghost small" disabled={conn !== "ready"} onClick={() => void link()}>
            <Icon name="terminal" size={14} />
            {t("链接开发中的插件文件夹…")}
          </button>
        )}
        <span className="flex" />
        {list && !inBrowser() && (
          <button className="ghost small" onClick={() => void window.sk.shell.openPath(list.folder)}>
            <Icon name="reveal" size={14} />
            {t("打开插件文件夹")}
          </button>
        )}
      </div>
      <div className="set-card plugins-list">
        {!list && <div className="set-row muted">{t("正在读取…")}</div>}
        {list && !plugins.length && (
          <div className="set-row muted plugins-empty">{t("还没有安装插件。把插件文件夹放进 {folder}，或用上面的按钮安装。", { folder: list.folder })}</div>
        )}
        {plugins.map((p) => (
          <PluginRow key={p.id} p={p} open={open === p.id} busy={busy === p.id} onToggle={() => void toggle(p)} />
        ))}
      </div>
      {confirm && <EnableDialog d={confirm} onYes={() => void enable(confirm)} onNo={() => setConfirm(null)} />}
    </section>
  );
}

function PluginRow({ p, open, busy, onToggle }: { p: PluginInfo; open: boolean; busy: boolean; onToggle: () => void }) {
  const tag = stateLabel(p.state, p.dev);
  const on = p.state === "enabled";
  return (
    <div className={`plugin-row${open ? " open" : ""}`}>
      <div className="plugin-row-main">
        <button className="plugin-row-head" aria-expanded={open} onClick={() => set({ pluginOpen: open ? null : p.id })}>
          <Icon name={open ? "chevronDown" : "chevronRight"} size={12} />
          <span className="plugin-row-icon">
            <PluginIcon plugin={p.id} name={p.icon} size={18} />
          </span>
          <span className="plugin-row-text">
            <span className="plugin-row-name">
              {p.name}
              <span className="mono muted small">{p.version}</span>
              <span className={`plugin-tag ${tag.tone}`}>{tag.text}</span>
              {p.builtin && <span className="plugin-tag muted">{t("内置")}</span>}
            </span>
            <span className="muted small plugin-row-sub">
              {p.author ? `${p.author} · ` : ""}
              <span className="mono">{p.id}</span>
            </span>
          </span>
        </button>
        <button
          role="switch"
          aria-checked={on}
          aria-label={t("启用「{name}」", { name: p.name })}
          className={`switch${on ? " on" : ""}`}
          disabled={busy || p.builtin || p.state === "incompatible"}
          title={p.builtin ? t("内置插件总是启用") : p.state === "incompatible" ? t("这个插件与当前版本不兼容") : undefined}
          onClick={onToggle}
        >
          <span />
        </button>
      </div>
      {open && <PluginDetails p={p} />}
    </div>
  );
}

function PluginDetails({ p }: { p: PluginInfo }) {
  const errors = p.issues.filter((i) => i.severity === "error");
  const warnings = p.issues.filter((i) => i.severity === "warning");
  const counts = Object.entries(p.contributions).filter(([, n]) => n > 0);
  const restart = async () => {
    try {
      putPlugin(await call("plugin.restart", { id: p.id }));
      toast(t("已重启「{name}」", { name: p.name }));
    } catch (err) {
      toast(errText(err), 5000);
    }
  };
  const uninstall = async () => {
    const yes = await ask({
      title: t("卸载「{name}」？", { name: p.name }),
      body: [t("插件文件夹会移到回收站；它的设置保留，再装回来就还在。"), p.folder],
      ok: t("卸载"),
      danger: true,
    });
    if (!yes) return;
    try {
      await call("plugin.uninstall", { id: p.id, remove_data: false });
      set({ pluginOpen: null });
      await refreshPlugins();
      toast(t("已卸载「{name}」", { name: p.name }));
    } catch (err) {
      toast(t("卸载失败：{error}", { error: errText(err) }), 5000);
    }
  };
  return (
    <div className="plugin-details">
      {p.description && <p>{p.description}</p>}
      {(p.state === "changed" || p.state === "needs_confirm") && (
        <div className="report-callout warn">
          <Icon name="warning" size={14} />
          <span>{p.state === "changed" ? t("插件文件在确认之后改动过：重新打开开关并确认后才会启用") : t("还没有确认过这个插件的权限：打开开关时会列出")}</span>
        </div>
      )}
      {errors.length > 0 && (
        <ul className="plugin-issues danger">
          {errors.map((i, k) => (
            <li key={k}>
              <span className="mono small">{i.file ? `${i.file}${i.line ? `:${i.line}` : ""} ` : ""}</span>
              {i.message}
            </li>
          ))}
        </ul>
      )}
      {warnings.length > 0 && (
        <ul className="plugin-issues warn">
          {warnings.map((i, k) => (
            <li key={k}>{i.message}</li>
          ))}
        </ul>
      )}
      <dl className="report-kv">
        <div className="report-kv-row">
          <dt className="muted">{t("权限")}</dt>
          <dd>{p.permissions.length ? <Permissions p={p} /> : <span className="muted">{t("不需要额外权限")}</span>}</dd>
        </div>
        {counts.length > 0 && (
          <div className="report-kv-row">
            <dt className="muted">{t("提供")}</dt>
            <dd>{counts.map(([kind, n]) => `${kindLabel(kind)} ${n}`).join(" · ")}</dd>
          </div>
        )}
        {p.keys.length > 0 && (
          <div className="report-kv-row">
            <dt className="muted">{t("快捷键")}</dt>
            <dd className="mono">{p.keys.join(" · ")}</dd>
          </div>
        )}
        <div className="report-kv-row">
          <dt className="muted">{t("文件夹")}</dt>
          <dd className="plugin-folder">
            <span className="mono small" title={p.folder}>
              {p.folder}
            </span>
            {!inBrowser() && (
              <button className="icon-btn tiny" aria-label={t("打开文件夹")} title={t("打开文件夹")} onClick={() => void window.sk.shell.openPath(p.folder)}>
                <Icon name="reveal" size={12} />
              </button>
            )}
          </dd>
        </div>
        {p.hash && (
          <div className="report-kv-row">
            <dt className="muted">{t("指纹")}</dt>
            <dd className="mono small" title={p.hash}>
              {p.hash.replace(/^sha256:/, "").slice(0, 16)}
            </dd>
          </div>
        )}
        <div className="report-kv-row">
          <dt className="muted">{t("运行方式")}</dt>
          <dd>{runtimeLabel(p)}</dd>
        </div>
        {p.homepage && (
          <div className="report-kv-row">
            <dt className="muted">{t("主页")}</dt>
            <dd className="mono small">{p.homepage}</dd>
          </div>
        )}
      </dl>
      {p.process && p.process.stderr.length > 0 && (
        <details className="plugin-stderr">
          <summary className="small">{t("最近的错误输出")}</summary>
          <pre className="mono small">{p.process.stderr.join("\n")}</pre>
        </details>
      )}
      {!p.builtin && (
        <div className="plugin-actions">
          {p.runtime === "process" && p.state === "enabled" && (
            <button className="ghost small" onClick={() => void restart()}>
              <Icon name="refresh" size={14} />
              {t("重启")}
            </button>
          )}
          <button className="ghost small danger-text" onClick={() => void uninstall()}>
            <Icon name="basket" size={14} />
            {t("卸载")}
          </button>
        </div>
      )}
    </div>
  );
}

function runtimeLabel(p: PluginInfo): string {
  if (p.runtime === "declarative") return t("只有声明，不运行代码");
  if (p.runtime === "inprocess") return t("在 SpaceKnife 进程里运行（开发者模式）");
  const proc = p.process;
  if (!proc || proc.state === "stopped") return t("单独的进程 · 用到时启动");
  const mem = proc.memory ? ` · ${fmtSize(proc.memory)}` : "";
  const restarts = proc.restarts ? ` · ${t("重启 {n} 次", { n: proc.restarts })}` : "";
  const state = { starting: t("正在启动"), running: t("运行中"), idle: t("空闲"), crashed: t("已崩溃"), stopped: t("已停止") }[proc.state];
  return `${t("单独的进程")} · ${state}${mem}${restarts}`;
}

/** Contribution kinds as the plugin page names them. */
function kindLabel(kind: string): string {
  const names: Record<string, string> = {
    command: t("命令"),
    keybinding: t("键位"),
    setting: t("设置"),
    report: t("报告"),
    panel: t("面板"),
    node_field: t("字段"),
    visualization: t("可视化"),
    color_mode: t("配色"),
    export_format: t("导出格式"),
    export_template: t("导出模板"),
    insight_rules: t("洞察规则"),
    insight: t("洞察"),
    file_class: t("文件类别"),
    folder_rule: t("文件夹规则"),
    node_set: t("节点集合"),
    filter_preset: t("过滤预设"),
    cleanup: t("清理"),
    scan_source: t("扫描来源"),
    watcher: t("监控器"),
    change_annotator: t("对比原因"),
    hook: t("钩子"),
    method: t("方法"),
    event: t("事件"),
    state: t("共享状态"),
    stream: t("订阅流"),
    icon: t("图标"),
  };
  return names[kind] ?? kind;
}

function Permissions({ p, added = [] }: { p: Pick<PluginInfo, "permissions" | "net_hosts" | "exec">; added?: readonly string[] }) {
  return (
    <ul className="plugin-perms">
      {p.permissions.map((x) => (
        <li key={x.id} className={`${x.warn ? "warn" : ""}${added.includes(x.id) ? " added" : ""}`}>
          <Icon name={x.warn ? "warning" : "shield"} size={12} />
          <span>
            {x.text}
            {added.includes(x.id) && <span className="plugin-tag warn">{t("新增")}</span>}
            {x.id === "net" && p.net_hosts.length > 0 && <span className="mono muted small"> {p.net_hosts.join(", ")}</span>}
            {x.id === "exec" && p.exec.length > 0 && <span className="mono muted small"> {p.exec.join(", ")}</span>}
          </span>
        </li>
      ))}
    </ul>
  );
}

/** The question before a plugin runs: who it is, what it may do, what it adds. */
function EnableDialog({ d, onYes, onNo }: { d: PluginDescribeResult; onYes: () => void; onNo: () => void }) {
  const p = d.plugin;
  const panels = (p.contributions.panel ?? 0) > 0;
  return (
    <div className="modal-back" role="presentation" onMouseDown={onNo}>
      <div className="modal plugin-enable" role="alertdialog" aria-modal="true" aria-labelledby="sk-enable-title" onMouseDown={(e) => e.stopPropagation()}>
        <div className="plugin-enable-head">
          <PluginIcon plugin={p.id} name={p.icon} size={24} />
          <div>
            <h3 id="sk-enable-title">{t("启用「{name}」？", { name: p.name })}</h3>
            <div className="muted small">
              {p.version}
              {p.author ? ` · ${p.author}` : ""} · <span className="mono">{p.id}</span>
            </div>
          </div>
        </div>
        {p.description && <p className="small">{p.description}</p>}
        <h4>{t("它可以")}</h4>
        {p.permissions.length ? <Permissions p={p} added={d.added} /> : <p className="muted small">{t("不需要额外权限：只读它自己的设置和数据")}</p>}
        {d.summary.length > 0 && (
          <>
            <h4>{t("它会添加")}</h4>
            <ul className="plugin-summary small">
              {d.summary.map((line, i) => (
                <li key={i}>{line}</li>
              ))}
            </ul>
          </>
        )}
        {d.conflicts.length > 0 && (
          <div className="report-callout warn">
            <Icon name="warning" size={14} />
            <span>{d.conflicts.join(byLang("；", "; "))}</span>
          </div>
        )}
        <div className="plugin-enable-notes muted small">
          {d.unsigned && <p>{t("这个插件没有签名：只启用你信任的来源。")}</p>}
          {p.dev && <p>{t("这是链接的开发文件夹：文件改动后不会要求重新确认。")}</p>}
          {p.runtime !== "declarative" && <p>{t("插件的代码在单独的进程里运行，宿主会检查它调用的接口和权限，但这不是操作系统级的沙箱。删除文件始终要经过删除篮和你的确认。")}</p>}
          {panels && inBrowser() && <p>{t("浏览器模式下，插件的网页面板可能绕过网络限制向外发送数据。")}</p>}
          <p className="mono">{t("文件指纹 {hash}", { hash: p.hash.replace(/^sha256:/, "").slice(0, 16) })}</p>
        </div>
        <div className="modal-buttons">
          <button className="ghost" onClick={onNo}>
            {t("取消")}
          </button>
          <button className="primary" autoFocus onClick={onYes}>
            {t("启用")}
          </button>
        </div>
      </div>
    </div>
  );
}
