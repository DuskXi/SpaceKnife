// A plugin's web panel (docs/plugins.md 「Web 面板」): its files in a sandboxed iframe (scripts only:
// no same origin, popups, forms, modals or top navigation), served with a strict content security
// policy by Electron's skplugin:// scheme or, in browser mode, by headless mode's /plugins/<id>/web/.
// The panel says hello; this frame answers with a MessagePort bound to that plugin and its
// permissions (bridge.ts), then keeps it told of the context, the theme and the language.

import { useEffect, useMemo, useRef, useState } from "react";

import type { PanelInfo, ReportContext } from "@/protocol/generated";
import { basketAddMany, call, editFilter, viewCall } from "@/state/actions";
import { runShortcut } from "@/state/commands";
import { get, toast, useApp } from "@/state/store";
import { t } from "@/lib/i18n";

import { callPlugin, onPluginEvent, subscribePlugin } from "../client";
import { ask } from "../confirm";
import { resolveNode, showNode, viewFor } from "../nodes";
import { pluginName } from "../store";
import { isHello, PanelBridge } from "./bridge";
import { themeVars } from "./theme";

/** The address of a panel's entry file, and the prefix every document of that plugin's panels has. */
export function panelAddress(plugin: string, entry: string, where: { electron: boolean; origin: string }): { src: string; base: string } {
  const rel = entry.replace(/^\/+/, "").replace(/^web\//, "");
  const base = where.electron ? `skplugin://${plugin}/` : `${where.origin}/plugins/${plugin}/web/`;
  return { src: base + rel.split("/").map(encodeURIComponent).join("/"), base };
}

/** Electron hosts panels on skplugin://; a browser tab on its server's /plugins/ (headless mode). */
const inElectron = () => !!window.sk.versions?.electron;

/** A node a panel names: { node, scan_id? } or a NodeRef; the context's scan by default. */
function nodeArg(args: Record<string, unknown>, ctx: ReportContext): { scan_id: string; node: number; path: string } | null {
  const ref = (args.node && typeof args.node === "object" ? args.node : args) as { scan_id?: unknown; node?: unknown; path?: unknown };
  const scan = typeof ref.scan_id === "string" ? ref.scan_id : ctx.scan_id;
  const node = typeof ref.node === "number" ? ref.node : typeof args.node === "number" ? args.node : null;
  if (!scan || node == null) return null;
  return { scan_id: scan, node, path: typeof ref.path === "string" ? ref.path : "" };
}

export type PanelPlace = "tab" | "inspector" | "dialog";

/** What the page gets as its context: where the user is, where the page is, its params. */
export function panelContext(ctx: ReportContext, info: Pick<PanelInfo, "params" | "slot">, place: PanelPlace | undefined): Record<string, unknown> {
  return { ...ctx, slot: place ?? info.slot, params: { ...(info.params ?? {}) } };
}

export function PanelFrame({ info, ctx, height, place }: { info: PanelInfo; ctx: ReportContext; height?: number; place?: PanelPlace }) {
  const frame = useRef<HTMLIFrameElement>(null);
  const bridge = useRef<PanelBridge | null>(null);
  const ctxRef = useRef(ctx);
  ctxRef.current = ctx;
  const placeRef = useRef(place);
  placeRef.current = place;
  // the tree of the context's scan changed (a rescan, a change on disk, the scan went on): told as "tree"
  const treeMark = useApp((s) => {
    const st = ctx.scan_id ? s.scans[ctx.scan_id] : undefined;
    return st ? `${st.state}:${st.items}:${st.bytes_alloc}` : "";
  });
  const lang = useApp((s) => s.lang);
  const [h, setH] = useState(height ?? info.height);
  const [refused, setRefused] = useState("");
  const { src, base } = useMemo(() => panelAddress(info.plugin, info.entry, { electron: inElectron(), origin: location.origin }), [info.plugin, info.entry]);

  useEffect(() => {
    const el = frame.current;
    if (!el) return;
    const owner = info.plugin;
    const hostOp = async (op: string, args: Record<string, unknown>): Promise<unknown> => {
      const c = ctxRef.current;
      switch (op) {
        case "toast": {
          const text = String(args.text ?? "").slice(0, 300);
          if (text) toast(t("{plugin}：{text}", { plugin: pluginName(owner), text }), 3000);
          return null;
        }
        case "select": {
          const n = nodeArg(args, c);
          if (!n) return null;
          if (args.stay === true) {
            // select it in its view, the user stays where the panel is
            const where = viewFor(n.scan_id);
            const id = where ? await resolveNode(n) : null;
            if (where && id != null) await viewCall(where.viewId, "view.reveal", { key: id });
            return null;
          }
          await showNode(n);
          return null;
        }
        case "zoom": {
          const n = nodeArg(args, c);
          const where = n ? viewFor(n.scan_id) : null;
          const id = n ? await resolveNode(n) : null;
          if (where && id != null) await viewCall(where.viewId, "view.zoom", { key: id });
          return null;
        }
        case "filter": {
          const where = c.scan_id ? viewFor(c.scan_id) : null;
          if (where) await editFilter(where.viewId, String(args.expr ?? ""), null, true);
          return null;
        }
        case "command":
          await runShortcut(String(args.command));
          return null;
        case "copy":
          window.sk.shell.copyText(String(args.text ?? "").slice(0, 65536));
          toast(t("已复制"));
          return null;
        case "url": {
          const url = String(args.url ?? "");
          if (!/^https:\/\//i.test(url)) throw new Error("only https:// addresses");
          if (await ask({ title: t("打开网页"), body: [t("插件「{plugin}」要在浏览器里打开：", { plugin: pluginName(owner) }), url], ok: t("打开") })) window.sk.shell.openUrl(url);
          return null;
        }
        case "basket": {
          const list = Array.isArray(args.nodes) ? args.nodes : [];
          const scan = typeof args.scan_id === "string" ? args.scan_id : c.scan_id;
          const nodes = list.map((x) => (typeof x === "number" ? x : (x as { node?: unknown })?.node)).filter((x): x is number => typeof x === "number");
          if (!scan || !nodes.length) return { added: 0 };
          const reason = String(args.reason ?? "").slice(0, 120);
          await basketAddMany(scan, nodes, info.id, nodes.map(() => reason));
          return { added: nodes.length };
        }
        default: {
          // reveal, open, terminal: the node's path on this computer
          const n = nodeArg(args, c);
          if (!n) return null;
          const id = await resolveNode(n);
          if (id == null) return null;
          const r = await call("tree.node", { scan_id: n.scan_id, node: id });
          if (!r.on_disk) throw new Error(t("这个位置不在磁盘上"));
          if (op === "reveal") window.sk.shell.showItemInFolder(r.path);
          else if (op === "open") void window.sk.shell.openPath(r.path);
          else window.sk.shell.openTerminal(r.node.kind === "dir" ? r.path : r.path.replace(/[\\/][^\\/]*$/, ""));
          return null;
        }
      }
    };
    const onMessage = (e: MessageEvent) => {
      if (!isHello(e, el.contentWindow)) return;
      // a new document in the frame (loaded, reloaded, navigated inside the plugin): a new port
      bridge.current?.close();
      const channel = new MessageChannel();
      const b = new PanelBridge(
        {
          plugin: owner,
          panel: info.id,
          permissions: info.permissions,
          call: (method, params, signal) => callPlugin(method, params, { signal }),
          onEvent: (wire, cb) => onPluginEvent(wire, cb),
          subscribe: (topic, params, cb) => subscribePlugin(topic, params, cb),
          host: hostOp,
          now: () => performance.now(),
        },
        channel.port1,
        {
          base,
          onResize: (px) => setH(px),
          onRefused: (href) => setRefused(href || "?"),
          onReady: () => {
            setRefused("");
            b.push("lang", { lang: get().lang });
            b.push("theme", { mode: "dark", vars: themeVars() });
            b.push("context", panelContext(ctxRef.current, info, placeRef.current));
          },
        },
      );
      bridge.current = b;
      el.contentWindow!.postMessage({ sk: 1, type: "connect", plugin: owner }, "*", [channel.port2]);
    };
    window.addEventListener("message", onMessage);
    return () => {
      window.removeEventListener("message", onMessage);
      bridge.current?.close();
      bridge.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps -- a new bridge for another plugin or page only
  }, [info.id, info.plugin, info.permissions, base]);

  useEffect(() => bridge.current?.push("context", panelContext(ctx, info, place)), [ctx, info, place]);
  useEffect(() => bridge.current?.push("lang", { lang }), [lang]);
  useEffect(() => {
    if (treeMark) bridge.current?.push("tree", { scan_id: ctx.scan_id });
  }, [treeMark, ctx.scan_id]);

  if (refused) {
    return (
      <div className="report-callout danger" role="alert">
        {t("面板打开了不属于这个插件的页面，已断开：{href}", { href: refused })}
      </div>
    );
  }
  return (
    <iframe
      ref={frame}
      className="plugin-panel"
      title={info.title}
      src={src}
      sandbox="allow-scripts"
      referrerPolicy="no-referrer"
      allow=""
      style={{ height: h }}
    />
  );
}
