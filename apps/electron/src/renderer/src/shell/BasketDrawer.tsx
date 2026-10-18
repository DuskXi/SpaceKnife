// The delete basket drawer (UI-W5-25…49): what goes, what it frees, then the recycle bin or a
// permanent deletion (two-step), with undo. The basket itself lives in the backend.

import { useMemo, useState } from "react";

import type { BasketItem, BasketVolume } from "@/protocol/generated";
import { basketClear, basketExecute, basketRemove } from "@/state/actions";
import { set, useApp } from "@/state/store";
import { fmtInt, fmtSize } from "@/lib/format";
import { sizeParts } from "@/lib/format";
import { Icon } from "@/ui/Icon";

type Sort = "size" | "name" | "path";
const SORT_ZH: Record<Sort, string> = { size: "按大小", name: "按名称", path: "按位置" };

export function BasketDrawer() {
  const basket = useApp((s) => s.basket);
  const busy = useApp((s) => s.op && (s.op.phase === "running" || s.op.phase === "waiting"));
  const [sort, setSort] = useState<Sort>("size");
  const [confirm, setConfirm] = useState(false);
  const items = useMemo(() => sorted(basket?.items ?? [], sort), [basket, sort]);
  const total = basket?.freeable ?? 0;
  const vol = mainVolume(basket?.volumes ?? []);
  const blocked = items.filter((it) => it.status !== "ready");
  const [num, unit] = sizeParts(total);
  const noTrash = (basket?.volumes ?? []).filter((v) => !v.trash && v.freeable > 0);

  return (
    <aside className="basket-drawer" aria-label="删除篮">
      <div className="bk-summary">
        <div className="bk-title">
          <Icon name="basket" />
          <strong>删除篮</strong>
          <span className="muted">· {fmtInt(items.length)} 项</span>
          <span className="flex" />
          <button className="icon-btn" title="清空删除篮（不删除任何文件）" aria-label="清空删除篮" disabled={!items.length || !!busy} onClick={() => void basketClear()}>
            <Icon name="more" />
          </button>
          <button className="icon-btn" title="关闭删除篮" aria-label="关闭删除篮" onClick={() => set({ basketOpen: false })}>
            <Icon name="close" />
          </button>
        </div>
        <div className="bk-free">
          <div className="muted small">
            <span className="bk-legend" /> 可释放
          </div>
          <div className="bk-big">
            <span className="mono">{num}</span> <span className="bk-unit">{unit}</span>
          </div>
        </div>
        {vol && <CapacityBar vol={vol} />}
      </div>
      <div className="bk-listhead">
        <span>待删除</span>
        <select className="bk-sort" value={sort} onChange={(e) => setSort(e.target.value as Sort)} aria-label="排序">
          {(Object.keys(SORT_ZH) as Sort[]).map((k) => (
            <option key={k} value={k}>
              {SORT_ZH[k]}
            </option>
          ))}
        </select>
      </div>
      <div className="bk-list" role="list">
        {items.length === 0 && <div className="muted bk-empty">在树图上右键「加入删除篮」，或选中后按 Delete</div>}
        {items.map((it) => (
          <Row key={it.id} it={it} share={total ? it.alloc / total : 0} />
        ))}
      </div>
      {items.length > 0 && (
        <div className="bk-total">
          <span>合计 {fmtInt(items.length)} 项</span>
          <span className="mono">{fmtSize(total)}</span>
        </div>
      )}
      {items.length > 0 && (
        <div className="bk-note">
          <Icon name="info" size={14} />
          <div>
            <div>移到回收站后，空间要等清空回收站才会释放</div>
            <div className="muted small">
              {noTrash.length ? `${noTrash.map((v) => `${v.mount_point}：${v.trash_note || "没有回收站"}`).join("；")}。这些项只能永久删除` : vol?.trash_note || "同一个卷上的回收站，随时可以还原"}
            </div>
          </div>
        </div>
      )}
      <span className="flex" />
      <div className="bk-actions">
        {blocked.length > 0 && (
          <div className="bk-estimate warn-text">
            <Icon name="warning" size={14} /> {fmtInt(blocked.length)} 项现在不能删除，执行时会跳过
          </div>
        )}
        <div className="bk-buttons">
          <button className="primary flex" disabled={!items.length || !!busy} onClick={() => void basketExecute("trash")}>
            <Icon name="basket" size={14} /> 移到回收站
          </button>
          <button className="danger-btn" disabled={!items.length || !!busy} onClick={() => setConfirm(true)}>
            永久删除…
          </button>
        </div>
      </div>
      {confirm && (
        <div className="modal-back" role="presentation" onMouseDown={() => setConfirm(false)}>
          <div className="modal" role="alertdialog" aria-modal="true" aria-labelledby="bk-confirm" onMouseDown={(e) => e.stopPropagation()}>
            <h3 id="bk-confirm">永久删除 {fmtInt(items.length)} 项（{fmtSize(total)}）？</h3>
            <p className="muted">不进回收站，无法从回收站还原。执行前有 5 秒可以撤销。</p>
            <div className="modal-buttons">
              <button className="ghost" autoFocus onClick={() => setConfirm(false)}>
                取消
              </button>
              <button
                className="danger-btn"
                onClick={() => {
                  setConfirm(false);
                  void basketExecute("delete");
                }}
              >
                永久删除
              </button>
            </div>
          </div>
        </div>
      )}
    </aside>
  );
}

function Row({ it, share }: { it: BasketItem; share: number }) {
  const where = it.path.replace(/[\\/][^\\/]*[\\/]?$/, "");
  return (
    <div className={`bk-row${it.status !== "ready" ? " blocked" : ""}`} role="listitem">
      <div className="bk-row-main">
        <span className={`bk-icon ${it.kind}`}>
          <Icon name={it.kind === "dir" ? "folder" : it.kind === "group" ? "layers" : "file"} size={15} />
        </span>
        <span className="bk-text">
          <span className="bk-name" title={it.path}>
            {it.kind === "group" ? `${fmtInt(it.members)} 个小项` : it.name}
            {it.tag && <span className={`tag-mini ${it.tag}`} />}
          </span>
          <span className="bk-path mono">{where}</span>
        </span>
        <span className="bk-size">
          <span className="mono">{fmtSize(it.alloc)}</span>
          <span className="muted small">{Math.round(share * 100)}%</span>
        </span>
        <button className="icon-btn small" title="从删除篮移除" aria-label={`从删除篮移除 ${it.name}`} onClick={() => void basketRemove([it.id])}>
          <Icon name="close" size={14} />
        </button>
      </div>
      {it.status !== "ready" && (
        <div className="bk-warn">
          <Icon name="warning" size={13} /> {it.note || STATUS_ZH[it.status]}
        </div>
      )}
    </div>
  );
}

const STATUS_ZH: Record<BasketItem["status"], string> = {
  ready: "",
  locked: "正被其他程序使用",
  missing: "已经不在磁盘上了",
  protected: "受保护的系统位置，不能删除",
};

function CapacityBar({ vol }: { vol: BasketVolume }) {
  const used = Math.max(0, vol.total - vol.free - vol.freeable);
  const pct = (n: number) => `${vol.total ? (n / vol.total) * 100 : 0}%`;
  return (
    <div className="bk-cap">
      <div className="bk-bar" aria-hidden="true">
        <span className="used" style={{ width: pct(used) }} />
        <span className="queued" style={{ width: pct(vol.freeable) }} />
        <span className="free" style={{ width: pct(vol.free) }} />
      </div>
      <div className="row-between small">
        <span>
          {vol.mount_point} 可用 {fmtSize(vol.free)} › <strong>{fmtSize(vol.free + vol.freeable)}</strong>
        </span>
        <span className="muted">共 {fmtSize(vol.total)}</span>
      </div>
    </div>
  );
}

function mainVolume(vols: BasketVolume[]): BasketVolume | null {
  return vols.reduce<BasketVolume | null>((a, v) => (a == null || v.freeable > a.freeable ? v : a), null);
}

function sorted(items: BasketItem[], sort: Sort): BasketItem[] {
  const out = [...items];
  if (sort === "size") out.sort((a, b) => b.alloc - a.alloc);
  else if (sort === "name") out.sort((a, b) => a.name.localeCompare(b.name, "zh-CN"));
  else out.sort((a, b) => a.path.localeCompare(b.path, "zh-CN"));
  return out;
}
