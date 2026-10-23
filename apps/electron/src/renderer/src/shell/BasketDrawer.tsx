// The delete basket drawer (UI-W5-25…49): what goes, what it frees, then the recycle bin or a
// permanent deletion (two-step), with undo. The basket itself lives in the backend.

import { useMemo, useState } from "react";

import type { BasketItem, BasketVolume } from "@/protocol/generated";
import { basketClear, basketExecute, basketRemove } from "@/state/actions";
import { set, useApp } from "@/state/store";
import { fmtInt, fmtSize, plural } from "@/lib/format";
import { sizeParts } from "@/lib/format";
import { byLang, t } from "@/lib/i18n";
import { inBrowser, serverName } from "@/lib/host";
import { Icon } from "@/ui/Icon";

type Sort = "size" | "name" | "path";
const SORTS: Sort[] = ["size", "name", "path"];
const sortLabel = (k: Sort): string => ({ size: t("按大小"), name: t("按名称"), path: t("按位置") })[k];
/** 「3 项」 / "3 items", "1 item". */
const itemCount = (n: number): string => plural(n, t("{n} 项", { n: fmtInt(n) }), "1 item");

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
    <aside className="basket-drawer" aria-label={t("删除篮")}>
      <div className="bk-summary">
        <div className="bk-title">
          <Icon name="basket" />
          <strong>{t("删除篮")}</strong>
          <span className="muted">· {itemCount(items.length)}</span>
          <span className="flex" />
          <button className="icon-btn" title={t("清空删除篮（不删除任何文件）")} aria-label={t("清空删除篮")} disabled={!items.length || !!busy} onClick={() => void basketClear()}>
            <Icon name="more" />
          </button>
          <button className="icon-btn" title={t("关闭删除篮")} aria-label={t("关闭删除篮")} onClick={() => set({ basketOpen: false })}>
            <Icon name="close" />
          </button>
        </div>
        <div className="bk-free">
          <div className="muted small">
            <span className="bk-legend" /> {t("可释放")}
          </div>
          <div className="bk-big">
            <span className="mono">{num}</span> <span className="bk-unit">{unit}</span>
          </div>
        </div>
        {vol && <CapacityBar vol={vol} />}
      </div>
      <div className="bk-listhead">
        <span>{t("待删除")}</span>
        <select className="bk-sort" value={sort} onChange={(e) => setSort(e.target.value as Sort)} aria-label={t("排序")}>
          {SORTS.map((k) => (
            <option key={k} value={k}>
              {sortLabel(k)}
            </option>
          ))}
        </select>
      </div>
      <div className="bk-list" role="list">
        {items.length === 0 && <div className="muted bk-empty">{t("在树图上右键「加入删除篮」，或选中后按 Delete")}</div>}
        {items.map((it) => (
          <Row key={it.id} it={it} share={total ? it.alloc / total : 0} />
        ))}
      </div>
      {items.length > 0 && (
        <div className="bk-total">
          <span>{plural(items.length, t("合计 {n} 项", { n: fmtInt(items.length) }), "Total: 1 item")}</span>
          <span className="mono">{fmtSize(total)}</span>
        </div>
      )}
      {items.length > 0 && (
        <div className="bk-note">
          <Icon name="info" size={14} />
          <div>
            <div>{t("移到回收站后，空间要等清空回收站才会释放")}</div>
            <div className="muted small">
              {noTrash.length
                ? t("{volumes}。这些项只能永久删除", {
                    volumes: noTrash.map((v) => t("{volume}：{note}", { volume: v.mount_point, note: v.trash_note || t("没有回收站") })).join(byLang("；", "; ")),
                  })
                : vol?.trash_note || t("同一个卷上的回收站，随时可以还原")}
            </div>
          </div>
        </div>
      )}
      <span className="flex" />
      {inBrowser() && items.length > 0 && (
        <div className="bk-remote" role="note">
          <Icon name="globe" size={14} />
          <div>{t("这些文件在 {host} 上：移到回收站和永久删除都作用于那台电脑，撤销的 5 秒也照常", { host: serverName() })}</div>
        </div>
      )}
      <div className="bk-actions">
        {blocked.length > 0 && (
          <div className="bk-estimate warn-text">
            <Icon name="warning" size={14} /> {plural(blocked.length, t("{n} 项现在不能删除，执行时会跳过", { n: fmtInt(blocked.length) }), "1 item can't be deleted now and will be skipped.")}
          </div>
        )}
        <div className="bk-buttons">
          <button className="primary flex" disabled={!items.length || !!busy} onClick={() => void basketExecute("trash")}>
            <Icon name="basket" size={14} /> {t("移到回收站")}
          </button>
          <button className="danger-btn" disabled={!items.length || !!busy} onClick={() => setConfirm(true)}>
            {t("永久删除…")}
          </button>
        </div>
      </div>
      {confirm && (
        <div className="modal-back" role="presentation" onMouseDown={() => setConfirm(false)}>
          <div className="modal" role="alertdialog" aria-modal="true" aria-labelledby="bk-confirm" onMouseDown={(e) => e.stopPropagation()}>
            <h3 id="bk-confirm">{plural(items.length, t("永久删除 {n} 项（{size}）？", { n: fmtInt(items.length), size: fmtSize(total) }), "Delete 1 item ({size}) permanently?", { size: fmtSize(total) })}</h3>
            <p className="muted">{t("不进回收站，无法从回收站还原。执行前有 5 秒可以撤销。")}</p>
            {inBrowser() && (
              <p className="remote-line">
                <Icon name="globe" size={13} />
                {t("删除的是 {host} 上的文件", { host: serverName() })}
              </p>
            )}
            <div className="modal-buttons">
              <button className="ghost" autoFocus onClick={() => setConfirm(false)}>
                {t("取消")}
              </button>
              <button
                className="danger-btn"
                onClick={() => {
                  setConfirm(false);
                  void basketExecute("delete");
                }}
              >
                {t("永久删除")}
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
            {it.kind === "group" ? t("{n} 个小项", { n: fmtInt(it.members) }) : it.name}
            {it.tag && <span className={`tag-mini ${it.tag}`} />}
          </span>
          <span className="bk-path mono">{where}</span>
        </span>
        <span className="bk-size">
          <span className="mono">{fmtSize(it.alloc)}</span>
          <span className="muted small">{Math.round(share * 100)}%</span>
        </span>
        <button className="icon-btn small" title={t("从删除篮移除")} aria-label={t("从删除篮移除 {name}", { name: it.name })} onClick={() => void basketRemove([it.id])}>
          <Icon name="close" size={14} />
        </button>
      </div>
      {it.status !== "ready" && (
        <div className="bk-warn">
          <Icon name="warning" size={13} /> {it.note || statusText(it.status)}
        </div>
      )}
    </div>
  );
}

/** Why an item cannot be deleted now, when the backend gave no note. */
const statusText = (status: BasketItem["status"]): string =>
  ({ ready: "", locked: t("正被其他程序使用"), missing: t("已经不在磁盘上了"), protected: t("受保护的系统位置，不能删除"), blocked: t("有几组保留的那份也在删除篮里，执行时这些组跳过") })[status];

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
          {t("{volume} 可用 {size}", { volume: vol.mount_point, size: fmtSize(vol.free) })} › <strong>{fmtSize(vol.free + vol.freeable)}</strong>
        </span>
        <span className="muted">{t("共 {size}", { size: fmtSize(vol.total) })}</span>
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
