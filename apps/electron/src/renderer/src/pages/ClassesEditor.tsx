// 「设置 › 文件类别」 (UI-W7-33): add, rename, recolour, reorder and delete file classes. Every change
// is sent at once (classes.update); the backend validates, saves, recolours the treemaps and re-runs
// :class: filters, and the parser vocabulary follows (classes.changed).

import { useEffect, useMemo, useRef, useState } from "react";

import type { ClassesState, FilterClassInfo } from "@/protocol/generated";
import { RpcError } from "@/protocol/rpc";
import { call } from "@/state/actions";
import { set, toast, useApp } from "@/state/store";
import { classHex } from "@/lib/classes";
import { Icon } from "@/ui/Icon";

interface Row {
  key: string;
  id: string | null;
  name: string;
  exts: string;
  color: string;
}

const NEW_COLORS = ["#5EC8B8", "#D4FF3A", "#F2A65A", "#C77DFF", "#FF6B8A", "#4CC9F0"];
let nextKey = 1;

const toRow = (c: FilterClassInfo): Row => ({ key: c.id, id: c.id, name: c.name, exts: c.extensions.join("; "), color: c.color });
const splitExts = (text: string) =>
  text
    .split(/[;,\s]+/)
    .map((e) => e.trim().replace(/^\.+/, "").toLowerCase())
    .filter(Boolean);

export function ClassesEditor() {
  const vocab = useApp((s) => s.vocab);
  const [state, setState] = useState<ClassesState | null>(null);
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
        if (owner && owner !== r.name) mine.push(`${e}（${owner}）`);
        else seen.set(e, r.name);
      }
      if (mine.length) out.set(r.key, mine);
    }
    return out;
  }, [rows]);

  const commit = async (next: Row[]) => {
    setRows(next);
    try {
      const st = await call("classes.update", {
        classes: next.map((r) => ({ id: r.id, name: r.name.trim(), extensions: r.id === "other" ? [] : [r.exts], color: r.color })),
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
    if (!r || (before && before.name === r.name.trim() && before.extensions.join("; ") === splitExts(r.exts).join("; ") && before.color === r.color)) return;
    void commit(rows.map((x) => (x.key === key ? { ...x, exts: splitExts(x.exts).join("; ") } : x)));
  };
  const add = () => {
    const n = rows.length;
    const row: Row = { key: `new-${nextKey++}`, id: null, name: `新类别 ${n}`, exts: "", color: NEW_COLORS[n % NEW_COLORS.length]! };
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

  return (
    <section className="set-group" aria-label="文件类别">
      <div className="set-group-head">
        <h2>类别</h2>
        <span className="muted small">拖动排序：同一扩展名在多个类别里时，排在前面的决定颜色</span>
      </div>
      <div className="set-card classes-card" role="list">
        {rows.map((r) => {
          const other = r.id === "other";
          const hex = r.color || classHex(vocab, r.id ?? "other");
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
              <label className="class-color" title="改颜色" style={{ background: hex }}>
                <input
                  type="color"
                  value={hex.slice(0, 7)}
                  aria-label={`${r.name} 的颜色`}
                  onChange={(e) => patch(r.key, { color: e.target.value.toUpperCase() })}
                  onBlur={() => changed(r.key)}
                />
              </label>
              <div className="class-fields">
                <input
                  className="class-name"
                  value={r.name}
                  disabled={other}
                  aria-label="类别名"
                  onChange={(e) => patch(r.key, { name: e.target.value })}
                  onBlur={() => changed(r.key)}
                  onKeyDown={(e) => e.key === "Enter" && (e.target as HTMLInputElement).blur()}
                />
                <input
                  className="class-exts mono"
                  value={other ? "" : r.exts}
                  placeholder={other ? "所有未归类的扩展名" : "扩展名，用 ; 分隔，例如 safetensors; gguf"}
                  disabled={other}
                  aria-label={`${r.name} 的扩展名`}
                  onChange={(e) => patch(r.key, { exts: e.target.value })}
                  onBlur={() => changed(r.key)}
                  onKeyDown={(e) => e.key === "Enter" && (e.target as HTMLInputElement).blur()}
                />
                {dups.get(r.key) && <div className="warn-text small">已在前面的类别里：{dups.get(r.key)!.join("、")}</div>}
              </div>
              <span className="muted small mono class-filter">:class:{r.name.trim()}</span>
              {!other &&
                (confirm === r.key ? (
                  <span className="class-confirm">
                    <button className="danger-btn small" onClick={() => (setConfirm(null), void commit(rows.filter((x) => x.key !== r.key)))}>
                      删除
                    </button>
                    <button className="ghost small" onClick={() => setConfirm(null)}>
                      取消
                    </button>
                  </span>
                ) : (
                  <button className="icon-btn small" aria-label={`删除类别 ${r.name}`} title="删除（其扩展名归入「其他」）" onClick={() => setConfirm(r.key)}>
                    <Icon name="close" size={14} />
                  </button>
                ))}
            </div>
          );
        })}
      </div>
      <div className="class-actions">
        <button className="ghost small" onClick={add}>
          <Icon name="plus" size={14} /> 添加类别
        </button>
        {state?.custom &&
          (confirm === "reset" ? (
            <span className="class-confirm">
              <span className="small">恢复为内置的 7 个类别？</span>
              <button
                className="danger-btn small"
                onClick={async () => {
                  setConfirm(null);
                  const st = await call("classes.reset", {});
                  setState(st);
                  setRows(st.classes.map(toRow));
                }}
              >
                恢复
              </button>
              <button className="ghost small" onClick={() => setConfirm(null)}>
                取消
              </button>
            </span>
          ) : (
            <button className="ghost small" onClick={() => setConfirm("reset")}>
              恢复默认类别
            </button>
          ))}
      </div>
    </section>
  );
}
