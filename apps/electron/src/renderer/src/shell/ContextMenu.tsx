// The unified context menu (UI-W5-01…24): an object header, open / show / terminal, copy path ▸,
// tags ▸, zoom, rescan, the delete basket and the system menu, the same in every front end.

import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";

import type { PathForms } from "@/protocol/generated";
import { basketAdd, basketAddBlock, call, rescanFolder, viewCall } from "@/state/actions";
import { set, toast, useApp } from "@/state/store";
import { fmtInt, fmtSize } from "@/lib/format";
import { t } from "@/lib/i18n";
import { Kind } from "@/visual/frame";
import { hitOfKey } from "@/visual/Host";
import { rowTitle } from "@/visual/rowTitle";
import { HIT_NONE, HIT_ROOT } from "@/visual/types";
import { Icon, type IconName, Kbd } from "@/ui/Icon";
import { revealLabel } from "@/shell/Inspector";
import { tagLabel } from "@/lib/tags";

const TAGS = [
  { id: "red", keys: "Ctrl+1" },
  { id: "yellow", keys: "Ctrl+2" },
  { id: "green", keys: "Ctrl+3" },
  { id: "blue", keys: "Ctrl+4" },
] as const;
const TAG_INDEX = [null, "red", "yellow", "green", "blue"] as const;
type CopyForm = keyof Omit<PathForms, "on_disk">;
const COPY_FORMS: { form: CopyForm; keys?: string }[] = [{ form: "raw", keys: "Ctrl+C" }, { form: "quoted" }, { form: "posix" }, { form: "uri" }];
/** The label of a copy form, in the current language. */
const copyLabel = (form: CopyForm): string => ({ raw: t("原样"), quoted: t("带引号"), posix: t("POSIX 路径"), uri: "file:// URI" })[form];

interface Item {
  id: string;
  icon: IconName;
  label: string;
  keys?: string;
  sub?: "copy" | "tag";
  danger?: boolean;
  disabled?: string; // why it is unavailable (tooltip)
  run?: () => void;
}

export const closeMenu = () => set({ menu: null });

export function ContextMenu() {
  const menu = useApp((s) => s.menu);
  const view = useApp((s) => (s.menu ? s.views[s.menu.viewId] : undefined));
  const vocab = useApp((s) => s.vocab);
  const settings = useApp((s) => s.settings); // tag display names (W7 标签)
  const lang = useApp((s) => s.lang); // the item labels follow a language switch
  const scanning = useApp((s) => {
    const st = s.menu ? s.scans[s.menu.scanId] : undefined;
    return st?.state === "scanning" || st?.state === "queued";
  });
  const box = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<{ left: number; top: number } | null>(null);
  const [forms, setForms] = useState<PathForms | null>(null);
  const [active, setActive] = useState(0);
  const [sub, setSub] = useState<"copy" | "tag" | null>(null);
  const [subActive, setSubActive] = useState(0);

  const frame = view?.frame ?? null;
  // a row, or the zoom root (the flame graphs' root bar and centre disc)
  const row = menu && frame ? hitOfKey(frame, menu.key) : HIT_NONE;
  const isRoot = row === HIT_ROOT;
  const kind = !frame ? -1 : isRoot ? frame.info.root!.kind : row >= 0 ? frame.kind[row]! : -1;
  const group = kind === Kind.SMALL || kind === Kind.MORE;
  const folder = kind === Kind.FOLDER || kind === Kind.COLLAPSED || kind === Kind.PENDING;
  const node = menu?.node ?? null;

  // the path four ways (copy submenu) and whether it exists on disk at all (MFT pseudo folders)
  useEffect(() => {
    setForms(null);
    setActive(0);
    setSub(null);
    if (!menu || node == null || node <= 0 || group) return;
    let live = true;
    void call("path.forms", { scan_id: menu.scanId, node })
      .then((f) => live && setForms(f))
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, [menu, node, group]);

  // place it: right of the block (8 px), downwards; flip when it does not fit (UI-W5-03)
  useLayoutEffect(() => {
    if (!menu || !box.current) return setPos(null);
    const w = box.current.offsetWidth;
    const h = box.current.offsetHeight;
    const a = menu.avoid;
    let left = a ? a.x + a.w + 8 : menu.x;
    let top = menu.y;
    if (left + w > window.innerWidth - 4) left = a ? Math.max(4, a.x - w - 8) : Math.max(4, menu.x - w);
    if (top + h > window.innerHeight - 4) top = Math.max(4, window.innerHeight - h - 4);
    setPos({ left, top });
    box.current.focus({ preventScroll: true }); // arrows and Enter work at once (UI-W5-02/22)
  }, [menu, forms, row]);

  // any click elsewhere, a resize or the window losing focus closes it (UI-W5-23)
  useEffect(() => {
    if (!menu) return;
    const down = (e: MouseEvent) => {
      if (box.current && !box.current.contains(e.target as Node)) closeMenu();
    };
    const close = () => closeMenu();
    window.addEventListener("mousedown", down, true);
    window.addEventListener("resize", close);
    window.addEventListener("blur", close);
    return () => {
      window.removeEventListener("mousedown", down, true);
      window.removeEventListener("resize", close);
      window.removeEventListener("blur", close);
    };
  }, [menu]);

  const items = useMemo<Item[]>(() => {
    if (!menu || row === HIT_NONE || !frame) return [];
    const gone = forms && !forms.on_disk ? t("NTFS 元数据与孤立文件只在 MFT 快速扫描里可见，没有对应的位置") : undefined;
    const noPath = group ? t("小项聚合块不是一个具体位置") : gone;
    const path = forms?.raw ?? "";
    const parentRow = row >= 0 ? frame.parent[row]! : -1;
    const folderNode = folder ? node : parentRow >= 0 ? frame.node[parentRow]! : null;
    const all: Item[] = [
      { id: "open", icon: folder ? "folder" : "file", label: t("打开"), disabled: noPath, run: () => void window.sk.shell.openPath(path) },
      { id: "reveal", icon: "reveal", label: revealLabel(), keys: "Ctrl+E", disabled: noPath, run: () => window.sk.shell.showItemInFolder(path) },
      {
        id: "terminal",
        icon: "terminal",
        label: t("在此打开终端"),
        keys: "Ctrl+`",
        disabled: noPath,
        run: () => window.sk.shell.openTerminal(folder ? path : path.replace(/[\\/][^\\/]*$/, "")),
      },
      { id: "copy", icon: "copy", label: t("复制路径"), sub: "copy", disabled: noPath },
      { id: "tag", icon: "tag", label: t("加标签"), sub: "tag", disabled: group ? t("小项聚合块不能加标签") : undefined },
      { id: "zoom", icon: "layers", label: t("只看这里（放大）"), keys: "Enter", run: () => void viewCall(menu.viewId, "view.zoom", { key: menu.key }) },
      {
        id: "rescan",
        icon: "refresh",
        label: t("重新扫描此文件夹"),
        disabled: folderNode == null || folderNode <= 0 ? t("没有可重新扫描的文件夹") : gone,
        run: () => folderNode != null && void rescanFolder(menu.scanId, folderNode),
      },
      {
        id: "basket",
        icon: "basket",
        label: t("加入删除篮"),
        keys: "Delete",
        danger: true,
        disabled: gone,
        run: () => void (group ? basketAddBlock(menu.scanId, menu.viewId, menu.key) : node != null && basketAdd(menu.scanId, node)),
      },
      { id: "system", icon: "more", label: t("系统菜单…"), disabled: t("系统原生右键菜单还没有接入（见 TODO）") },
    ];
    // the zoom root is what is zoomed into already
    return isRoot ? all.filter((it) => it.id !== "zoom") : all;
    // lang: the labels are made by t(), so a language switch makes them again
  }, [menu, row, isRoot, frame, forms, group, folder, node, lang]);

  if (!menu || row === HIT_NONE || !frame) return null;
  const root = isRoot ? frame.info.root! : null;
  const size = root ? root.size : Number(frame.size[row]!);
  const crumbs = frame.info.breadcrumb;
  // a row too small for a label carries no name in the frame: its path's last part, once known
  const title = rowTitle(frame, row, { scanning, formatInt: (n) => fmtInt(n) }).line1 || (forms?.raw.split(/[\\/]/).filter(Boolean).pop() ?? "");
  const cls = (root ? undefined : vocab?.classes[frame.cls[row]!]?.name) ?? (folder ? t("文件夹") : t("文件"));
  const parentRow = root ? -1 : frame.parent[row]!;
  const where = root ? (crumbs[crumbs.length - 2]?.label.split(/[\\/]/).filter(Boolean).pop() ?? "") : parentRow >= 0 ? (frame.names[parentRow] ?? "") : "";
  const tag = TAG_INDEX[root ? root.tag : frame.tag[row]!] ?? null;

  const activate = (it: Item) => {
    if (it.disabled) return toast(it.disabled);
    if (it.sub) {
      setSub(it.sub);
      setSubActive(0);
      return;
    }
    closeMenu();
    it.run?.();
  };
  const copy = (form: CopyForm) => {
    if (!forms) return;
    window.sk.shell.copyText(forms[form]);
    toast(t("已复制"));
    closeMenu();
  };
  const setTag = (color: string | null) => {
    closeMenu();
    void viewCall(menu.viewId, "view.select", { key: menu.key }).then(() => viewCall(menu.viewId, "view.tag", { color: color as "red" | null }));
  };

  const onKey = (e: React.KeyboardEvent) => {
    const n = sub === "copy" ? COPY_FORMS.length : sub === "tag" ? TAGS.length + 1 : items.length;
    if (e.key === "Escape") return sub ? setSub(null) : closeMenu();
    if (e.key === "ArrowDown") return sub ? setSubActive((i) => (i + 1) % n) : setActive((i) => (i + 1) % n);
    if (e.key === "ArrowUp") return sub ? setSubActive((i) => (i + n - 1) % n) : setActive((i) => (i + n - 1) % n);
    if (e.key === "ArrowLeft" && sub) return setSub(null);
    if ((e.key === "ArrowRight" && !sub && items[active]?.sub) || (e.key === "Enter" && !sub)) return activate(items[active]!);
    if (e.key === "Enter" && sub === "copy") return copy(COPY_FORMS[subActive]!.form);
    if (e.key === "Enter" && sub === "tag") return setTag(subActive < TAGS.length ? TAGS[subActive]!.id : null);
  };

  return (
    <div
      ref={box}
      className="ctxmenu"
      role="menu"
      aria-label={t("{name} 的操作", { name: title })}
      tabIndex={-1}
      style={pos ?? { left: -9999, top: -9999 }}
      onKeyDown={onKey}
      onContextMenu={(e) => e.preventDefault()}
    >
      <div className="cm-head" title={title}>
        <div className="cm-head-text">
          <div className="cm-title">{title}</div>
          <div className="cm-sub">
            <span className="mono">{fmtSize(size)}</span> · {cls}
            {where && ` · ${where}`}
          </div>
        </div>
        {tag && <span className={`tag-shape ${tag}`} aria-label={t("标签：{tag}", { tag })} />}
      </div>
      {items.map((it, i) => (
        <div key={it.id}>
          {(it.id === "basket" || it.id === "system") && <div className="cm-sep" role="separator" />}
          <button
            role="menuitem"
            className={`cm-item${i === active && !sub ? " active" : ""}${it.danger ? " danger" : ""}`}
            aria-disabled={!!it.disabled}
            aria-haspopup={it.sub ? "menu" : undefined}
            title={it.disabled}
            onMouseEnter={() => {
              setActive(i);
              if (it.sub && !it.disabled) {
                setSub(it.sub);
                setSubActive(0);
              } else setSub(null);
            }}
            onClick={() => activate(it)}
          >
            <span className="cm-icon">
              <Icon name={it.icon} size={16} />
            </span>
            <span className="flex">{it.label}</span>
            {it.sub ? <Icon name="chevronRight" size={14} /> : it.keys ? <Kbd>{it.keys}</Kbd> : null}
          </button>
          {sub && it.sub === sub && (
            <div className={`cm-submenu${sub === "copy" ? " wide" : ""}`} role="menu" aria-label={it.label}>
              {sub === "copy"
                ? COPY_FORMS.map((f, j) => (
                    <button key={f.form} role="menuitem" className={`cm-item two${j === subActive ? " active" : ""}`} onMouseEnter={() => setSubActive(j)} onClick={() => copy(f.form)}>
                      <span className="flex">
                        <span className="cm-line">{copyLabel(f.form)}</span>
                        <span className="cm-preview mono">{forms ? forms[f.form] : "…"}</span>
                      </span>
                      {f.keys && <Kbd>{f.keys}</Kbd>}
                    </button>
                  ))
                : [...TAGS, { id: null, keys: "Ctrl+0" } as const].map((tg, j) => (
                    <button key={tg.id ?? "none"} role="menuitem" className={`cm-item${j === subActive ? " active" : ""}`} onMouseEnter={() => setSubActive(j)} onClick={() => setTag(tg.id)}>
                      <span className="cm-icon">{tg.id ? <span className={`tag-shape ${tg.id}`} /> : <Icon name="close" size={14} />}</span>
                      <span className="flex">{tg.id ? tagLabel(settings, tg.id) : t("清除标签")}</span>
                      <Kbd>{tg.keys}</Kbd>
                    </button>
                  ))}
            </div>
          )}
        </div>
      ))}
    </div>
  );
}
