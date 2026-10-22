// 「设置 › 文件类别」 (UI-W7-33): add, rename, recolour, reorder and delete file classes. Every change
// is sent at once (classes.update); the backend validates, saves, recolours the treemaps and re-runs
// :class: filters, and the parser vocabulary follows (classes.changed). A colour is the theme on
// screen's (W7 待定 22): each theme keeps its own, 「同时应用到另一主题」 sets both. Below, the folders
// the user classed by hand (UI-W7-38, added from the map's menu 「归类」), each one removable.

import { useEffect, useMemo, useRef, useState } from "react";

import type { ClassesState, FilterClassInfo } from "@/protocol/generated";
import { RpcError } from "@/protocol/rpc";
import { call } from "@/state/actions";
import { set, toast, useApp } from "@/state/store";
import { colorOf, themeColor, userColor } from "@/lib/classes";
import { byLang, t } from "@/lib/i18n";
import { themeName } from "@/lib/theme";
import { Icon } from "@/ui/Icon";

interface Row {
  key: string;
  id: string | null;
  name: string;
  exts: string;
  /** the user's colours for the dark and the light theme, "" for the theme's */
  color: string;
  color_light: string;
}

// a new class's colour, the same in both themes (dark text on each, ≥ 5.9:1 in either; they stand
// apart from the built-in classes in both)
const NEW_COLORS = ["#5EC8B8", "#D4FF3A", "#F2A65A", "#C77DFF", "#FF6B8A", "#4CC9F0"];
let nextKey = 1;

// what folders add to a built-in class (spaceknife.pathclass): shown under its extensions
const folderNote = (id: string | null): string | null =>
  id === "game"
    ? t("另外，游戏安装目录里的文件都算这一类：Steam、Epic、GOG、Xbox、HoYoPlay 等启动器的游戏，Unity、Unreal、Godot 游戏，Minecraft，游戏存档")
    : id === "code"
      ? t("另外，项目文件夹里的文件都算这一类：含 .git、package.json、Cargo.toml、pyproject.toml、*.sln 等的文件夹")
      : id === "data"
        ? t("另外，模型缓存里的文件都算这一类：.ollama、.cache/huggingface、.lmstudio")
        : id === "exec"
          ? t("另外，系统卷信息和 NTFS 元数据算这一类；已安装的软件和应用数据（Program Files、AppData 等）里没有类别的文件也算")
          : null;

const toRow = (c: FilterClassInfo): Row => ({ key: c.id, id: c.id, name: c.name, exts: c.extensions.join("; "), color: c.color, color_light: c.color_light });
/** The row's colour field for the theme on screen. */
const field = (): "color" | "color_light" => (themeName() === "light" ? "color_light" : "color");
const splitExts = (text: string) =>
  text
    .split(/[;,\s]+/)
    .map((e) => e.trim().replace(/^\.+/, "").toLowerCase())
    .filter(Boolean);

export function ClassesEditor() {
  const vocab = useApp((s) => s.vocab);
  const lang = useApp((s) => s.lang); // the duplicate notes are in the interface language
  const theme = useApp((s) => s.theme); // the colours shown and changed are this theme's
  const [state, setState] = useState<ClassesState | null>(null);
  // 「同时应用到另一主题」: a colour picked (or reset) goes to both themes
  const [both, setBoth] = useState(false);
  const [rows, setRows] = useState<Row[]>([]);
  const [confirm, setConfirm] = useState<string | null>(null); // row key, or "reset"
  const drag = useRef<string | null>(null);

  useEffect(() => {
    void call("classes.get", {})
      .then((st) => {
        setState(st);
        setRows(st.classes.map(toRow));
      })
      .catch(() => undefined);
  }, []);

  // extensions already taken by an earlier class: that class colours them (UI-W7-33 重复提示)
  const dups = useMemo(() => {
    const seen = new Map<string, string>();
    const out = new Map<string, string[]>();
    for (const r of rows) {
      const mine: string[] = [];
      for (const e of splitExts(r.exts)) {
        const owner = seen.get(e);
        if (owner && owner !== r.name) mine.push(t("{ext}（{owner}）", { ext: e, owner }));
        else seen.set(e, r.name);
      }
      if (mine.length) out.set(r.key, mine);
    }
    return out;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [rows, lang]);

  const commit = async (next: Row[]) => {
    setRows(next);
    try {
      const st = await call("classes.update", {
        classes: next.map((r) => ({ id: r.id, name: r.name.trim(), extensions: r.id === "other" ? [] : [r.exts], color: r.color, color_light: r.color_light })),
      });
      setState(st);
      // new classes got their ids: keep the rows' keys, take the ids
      setRows((cur) => cur.map((r, i) => ({ ...r, id: st.classes[i]?.id ?? r.id })));
      set({ vocab: vocab ? { ...vocab, classes: st.classes } : vocab });
    } catch (err) {
      toast(err instanceof RpcError ? err.message : String(err));
      if (state) setRows(state.classes.map(toRow));
    }
  };
  const patch = (key: string, p: Partial<Row>) => setRows((cur) => cur.map((r) => (r.key === key ? { ...r, ...p } : r)));
  const changed = (key: string) => {
    const r = rows.find((x) => x.key === key);
    const before = state?.classes.find((c) => c.id === r?.id);
    if (
      !r ||
      (before && before.name === r.name.trim() && before.extensions.join("; ") === splitExts(r.exts).join("; ") && before.color === r.color && before.color_light === r.color_light)
    )
      return;
    void commit(rows.map((x) => (x.key === key ? { ...x, exts: splitExts(x.exts).join("; ") } : x)));
  };
  const add = () => {
    const n = rows.length;
    const seed = NEW_COLORS[n % NEW_COLORS.length]!;
    const row: Row = { key: `new-${nextKey++}`, id: null, name: t("新类别 {n}", { n }), exts: "", color: seed, color_light: seed };
    const other = rows.findIndex((r) => r.id === "other");
    const next = [...rows];
    next.splice(other < 0 ? next.length : other, 0, row);
    void commit(next);
  };
  const move = (from: string, to: string) => {
    if (from === to) return;
    const next = rows.filter((r) => r.key !== from);
    const at = next.findIndex((r) => r.key === to);
    const moved = rows.find((r) => r.key === from)!;
    next.splice(at < 0 ? next.length : at, 0, moved);
    // 「其他」 stays last
    const other = next.findIndex((r) => r.id === "other");
    if (other >= 0 && other !== next.length - 1) next.push(next.splice(other, 1)[0]!);
    void commit(next);
  };
  /** The colour fields a pick or a reset changes: the theme on screen's, or both. */
  const recolor = (hex: string): Partial<Row> => (both ? { color: hex, color_light: hex } : { [field()]: hex });

  return (
    <section className="set-group" aria-label={t("文件类别")}>
      <div className="set-group-head">
        <h2>{t("类别")}</h2>
        <span className="muted small">{t("拖动排序：同一扩展名在多个类别里时，排在前面的决定颜色")}</span>
      </div>
      <div className="set-card classes-card" role="list">
        {rows.map((r) => {
          const other = r.id === "other";
          const id = r.id ?? "other";
          const hex = colorOf({ id, color: r.color, color_light: r.color_light });
          // a built-in class's own colour in this theme can go back to the theme's
          const resettable = !!r.id && !!themeColor(id) && !!userColor({ id, color: r.color, color_light: r.color_light }, theme);
          return (
            <div
              key={r.key}
              role="listitem"
              className="class-row"
              draggable={!other}
              onDragStart={(e) => {
                drag.current = r.key;
                e.dataTransfer.effectAllowed = "move";
              }}
              onDragOver={(e) => {
                if (drag.current) e.preventDefault();
              }}
              onDrop={(e) => {
                e.preventDefault();
                if (drag.current) move(drag.current, r.key);
                drag.current = null;
              }}
            >
              <span className={`class-grip${other ? " off" : ""}`} aria-hidden="true">
                ⋮⋮
              </span>
              <label className="class-color" title={t("改颜色")} style={{ background: hex }}>
                <input
                  type="color"
                  value={hex.slice(0, 7)}
                  aria-label={t("{name} 的颜色", { name: r.name })}
                  onChange={(e) => patch(r.key, recolor(e.target.value.toUpperCase()))}
                  onBlur={() => changed(r.key)}
                />
              </label>
              {/* a slot of its own width in every row, so a row with the button does not shift */}
              <span className="class-reset">
                {resettable && (
                  <button className="icon-btn tiny" title={t("恢复默认颜色")} aria-label={t("恢复 {name} 的默认颜色", { name: r.name })} onClick={() => void commit(rows.map((x) => (x.key === r.key ? { ...x, ...recolor("") } : x)))}>
                    <Icon name="refresh" size={12} />
                  </button>
                )}
              </span>
              <div className="class-fields">
                <input
                  className="class-name"
                  value={r.name}
                  disabled={other}
                  aria-label={t("类别名")}
                  onChange={(e) => patch(r.key, { name: e.target.value })}
                  onBlur={() => changed(r.key)}
                  onKeyDown={(e) => e.key === "Enter" && (e.target as HTMLInputElement).blur()}
                />
                <input
                  className="class-exts mono"
                  value={other ? "" : r.exts}
                  placeholder={other ? t("所有未归类的扩展名") : t("扩展名，用 ; 分隔，例如 pak; ucas")}
                  disabled={other}
                  aria-label={t("{name} 的扩展名", { name: r.name })}
                  onChange={(e) => patch(r.key, { exts: e.target.value })}
                  onBlur={() => changed(r.key)}
                  onKeyDown={(e) => e.key === "Enter" && (e.target as HTMLInputElement).blur()}
                />
                {dups.get(r.key) && <div className="warn-text small">{t("已在前面的类别里：{list}", { list: dups.get(r.key)!.join(byLang("、", ", ")) })}</div>}
                {folderNote(r.id) && <div className="muted small class-note">{folderNote(r.id)}</div>}
              </div>
              <span className="muted small mono class-filter">:class:{r.name.trim()}</span>
              {!other &&
                (confirm === r.key ? (
                  <span className="class-confirm">
                    <button className="danger-btn small" onClick={() => (setConfirm(null), void commit(rows.filter((x) => x.key !== r.key)))}>
                      {t("删除")}
                    </button>
                    <button className="ghost small" onClick={() => setConfirm(null)}>
                      {t("取消")}
                    </button>
                  </span>
                ) : (
                  <button className="icon-btn small" aria-label={t("删除类别 {name}", { name: r.name })} title={t("删除（其扩展名归入「其他」）")} onClick={() => setConfirm(r.key)}>
                    <Icon name="close" size={14} />
                  </button>
                ))}
            </div>
          );
        })}
      </div>
      <div className="class-actions">
        <button className="ghost small" onClick={add}>
          <Icon name="plus" size={14} /> {t("添加类别")}
        </button>
        {(state?.custom || !!state?.folders.length) &&
          (confirm === "reset" ? (
            <span className="class-confirm">
              <span className="small">
                {state.folders.length
                  ? t("恢复为内置的 {n} 个类别，并清除 {m} 个文件夹归类？", { n: state.defaults.length, m: state.folders.length })
                  : t("恢复为内置的 {n} 个类别？", { n: state.defaults.length })}
              </span>
              <button
                className="danger-btn small"
                onClick={async () => {
                  setConfirm(null);
                  const st = await call("classes.reset", {});
                  setState(st);
                  setRows(st.classes.map(toRow));
                }}
              >
                {t("恢复")}
              </button>
              <button className="ghost small" onClick={() => setConfirm(null)}>
                {t("取消")}
              </button>
            </span>
          ) : (
            <button className="ghost small" onClick={() => setConfirm("reset")}>
              {t("恢复默认类别")}
            </button>
          ))}
        <label className="export-check small class-both" title={theme === "light" ? t("改的颜色也用在深色主题里") : t("改的颜色也用在浅色主题里")}>
          <input type="checkbox" checked={both} onChange={(e) => setBoth(e.target.checked)} />
          {t("同时应用到另一主题")}
        </label>
      </div>
      <div className="set-group-head">
        <h2>{t("按文件夹归类")}</h2>
        <span className="muted small">{t("在图上右键文件夹，选「归类」添加；这些文件夹里的文件不论扩展名都算所选的类别")}</span>
      </div>
      <div className="set-card" role="list">
        {state && state.folders.length === 0 && <div className="muted small class-folders-empty">{t("还没有。游戏、项目、模型缓存等文件夹会自动识别")}</div>}
        {state?.folders.map((f) => {
          const cls = f.class_id ? state.classes.find((c) => c.id === f.class_id) : null;
          return (
            <div key={f.path} role="listitem" className="class-folder">
              <span className="mono class-folder-path" title={f.path}>
                {f.path}
              </span>
              <span className="class-folder-class">
                {cls && <span className="swatch" style={{ background: colorOf(cls) }} />}
                {cls ? cls.name : t("只按扩展名")}
              </span>
              <button
                className="icon-btn small"
                aria-label={t("取消 {path} 的归类", { path: f.path })}
                title={t("改回自动识别")}
                onClick={async () => {
                  try {
                    setState(await call("classes.folder", { path: f.path, class_id: null }));
                  } catch (err) {
                    toast(err instanceof RpcError ? err.message : String(err));
                  }
                }}
              >
                <Icon name="close" size={14} />
              </button>
            </div>
          );
        })}
      </div>
    </section>
  );
}
