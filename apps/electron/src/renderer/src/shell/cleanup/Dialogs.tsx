// The dialogs of cleanup mode: a bulk edit's preview, the last confirmation before running (its
// focus on 「取消」), the preflight's dropped steps, undo, emptying the recovery area (the backend
// waits 5 s first: 「取消」 in that time calls it off), and taking up saved edits.

import { useEffect, useRef, useState } from "react";

import {
  actionLabel,
  commitPreview,
  confirmExecute,
  confirmPurge,
  confirmUndo,
  resume,
  setCleanup,
  stopTask,
  useCleanup,
} from "@/state/cleanup";
import { fitPath, fmtInt, fmtSize, plural } from "@/lib/format";
import { t } from "@/lib/i18n";

import { Modal } from "./parts";

const close = () => setCleanup({ dialog: null });

export function CleanupDialogs() {
  const d = useCleanup((s) => s.dialog);
  const task = useCleanup((s) => s.task);
  if (task && task.kind === "purge" && task.state === "waiting") return <PurgeCountdown />;
  if (!d) return null;
  if (d.kind === "preview") return <PreviewDialog />;
  if (d.kind === "execute") return <ExecuteDialog />;
  if (d.kind === "drops") return <DropsDialog />;
  if (d.kind === "undo") return <UndoDialog />;
  if (d.kind === "purge") return <PurgeDialog />;
  if (d.kind === "resume") return <ResumeDialog />;
  return null;
}

function CancelFirst({ label, onOk, okLabel, disabled }: { label?: string; onOk: () => void; okLabel: string; disabled?: boolean }) {
  const ref = useRef<HTMLButtonElement>(null);
  useEffect(() => ref.current?.focus(), []);
  return (
    <div className="modal-buttons">
      <button ref={ref} className="ghost" onClick={close} autoFocus>
        {label ?? t("取消")}
      </button>
      <button className="primary" onClick={onOk} disabled={disabled}>
        {okLabel}
      </button>
    </div>
  );
}

function PreviewDialog() {
  const d = useCleanup((s) => s.dialog);
  const [also, setAlso] = useState(false);
  if (d?.kind !== "preview") return null;
  const diff = d.diff;
  const keys = ["keep", "clone", "hardlink", "symlink", "trash", "leave"];
  const kept = Object.entries(diff.unavailable).filter(([why]) => why !== "risk_rule");
  const held = diff.unavailable.risk_rule ?? 0;
  return (
    <Modal title={t("先看一下这次改动")} wide>
      <table className="cl-sum small">
        <thead>
          <tr>
            <th />
            <th>{t("改前")}</th>
            <th>{t("改后")}</th>
          </tr>
        </thead>
        <tbody>
          {keys.map((k) => (
            <tr key={k} className={(diff.before[k] ?? 0) !== (diff.after[k] ?? 0) ? "differs" : ""}>
              <th>{actionLabel(k)}</th>
              <td className="mono">{fmtInt(diff.before[k] ?? 0)}</td>
              <td className="mono">{fmtInt(diff.after[k] ?? 0)}</td>
            </tr>
          ))}
          <tr>
            <th>{t("腾出")}</th>
            <td className="mono">{fmtSize(diff.freed_before)}</td>
            <td className="mono">{fmtSize(diff.freed_after)}</td>
          </tr>
        </tbody>
      </table>
      <div className="small">{plural(diff.changed, t("涉及 {n} 组", { n: fmtInt(diff.changed) }), "1 group changes")}</div>
      {kept.length > 0 && (
        <div className="small warn">
          {t("这些份做不到，保持原来的决定：")}
          {kept.map(([why, n]) => (
            <span key={why} className="cl-reason">
              {reasonLabel(why)} · {fmtInt(n)}
            </span>
          ))}
        </div>
      )}
      {held > 0 && <div className="small warn">{t("{n} 份有高风险，规则「任何高风险 → 不动」把它们改成了不动（要这样做，先关掉那条规则，或者审阅时确认）", { n: fmtInt(held) })}</div>}
      <RiskList title={t("新增的风险：")} risks={diff.risks_added} titles={diff.risk_titles ?? {}} />
      <RiskList title={t("不再有的风险：")} risks={diff.risks_removed} titles={diff.risk_titles ?? {}} muted />
      {diff.protected > 0 && (
        <label className="small cl-check">
          <input type="checkbox" checked={also} onChange={(e) => setAlso(e.target.checked)} />
          {t("也改你手动选过的 {n} 份", { n: fmtInt(diff.protected) })}
        </label>
      )}
      {diff.examples.length > 0 && (
        <div className="small cl-examples">
          {diff.examples.map((x) => (
            <div key={x.path} className="cl-example">
              <span className="mono" title={x.path}>
                {fitPath(x.path, 64)}
              </span>
              <span className="muted">
                {actionLabel(x.before)} → {actionLabel(x.after)}
              </span>
            </div>
          ))}
        </div>
      )}
      <CancelFirst okLabel={t("照这样改")} onOk={() => void commitPreview(also)} />
    </Modal>
  );
}

/** Risks of a preview by title, the most copies first. */
function RiskList({ title, risks, titles, muted }: { title: string; risks: Record<string, number>; titles: Record<string, string>; muted?: boolean }) {
  const rows = Object.entries(risks).sort((a, b) => b[1] - a[1]);
  if (!rows.length) return null;
  return (
    <div className={`small cl-risk-list${muted ? " muted" : ""}`}>
      {title}
      {rows.map(([id, n]) => (
        <span key={id} className="cl-reason" title={id}>
          {titles[id] ?? id} · {fmtInt(n)}
        </span>
      ))}
    </div>
  );
}

export const reasonLabel = (why: string): string =>
  ({
    other_volume: t("跨卷"),
    other_subvolume: t("跨子卷"),
    fs_no_hardlink: t("文件系统不支持硬链接"),
    fs_no_symlink: t("文件系统不支持符号链接"),
    fs_no_clone: t("文件系统不能共用数据块"),
    no_symlink_right: t("没有建符号链接的权限"),
    link_limit: t("名字到了上限"),
    protected_hardlinks: t("保留那份属于别的用户"),
    immutable: t("文件不可改动"),
    in_sync_root: t("在同步文件夹里"),
    vcs_internal: t("版本库的内部文件"),
    in_package: t("在 macOS 的包里"),
    no_trash: t("没有回收站"),
    no_recovery_area: t("建不了恢复区"),
    no_exchange: t("不支持原子交换"),
    folder_not_writable: t("文件夹不能写"),
    name_unusable: t("名字不能用"),
    no_keeper_here: t("这个卷上没有保留点"),
    already_shared: t("已经共用数据块"),
    network: t("网络盘"),
    no_native: t("缺原生模块"),
    macos_off: t("macOS 上默认关"),
    forced_keep: t("必须保留"),
    stale: t("文件变了"),
    readonly_inplace: t("只读"),
    encrypted: t("加密"),
    sparse_mismatch: t("稀疏文件不一致"),
    in_basket: t("在删除篮里"),
    keeper: t("是保留点"),
    not_owner: t("不是文件的主人"),
  })[why] ?? why;

function ExecuteDialog() {
  const st = useCleanup((s) => s.state);
  if (!st) return null;
  const sm = st.summary;
  const n = sm.copies;
  const has = (a: string) => sm.by_action.some((x) => x.action === a && x.copies > 0);
  const replacing = has("hardlink") || has("symlink");
  return (
    <Modal title={t("执行清理方案")}>
      <div className="small">
        {sm.by_action
          .filter((a) => a.action !== "leave")
          .map((a) => t("{act} {n} 份", { act: actionLabel(a.action), n: fmtInt(a.copies) }))
          .join(" · ")}
      </div>
      <div className="small">
        {sm.by_volume.map((v) => t("{vol} 马上 {now}，之后 {later}", { vol: v.volume, now: fmtSize(v.freed_now), later: fmtSize(v.freed_later) })).join(" · ")}
      </div>
      <ul className="small cl-exec-notes">
        <li>{t("每一份执行前都会再核对，变过的跳过，从不换成别的处理")}</li>
        {replacing && <li>{t("换成链接的，原件先放在恢复区：清除恢复区才真正腾出空间")}</li>}
        {has("trash") && <li>{t("移到回收站的在回收站里：清空回收站才腾出空间")}</li>}
        {has("clone") && <li>{t("块克隆马上腾出空间，内容不变")}</li>}
        <li>{t("做完以后在「清理记录」里可以撤销")}</li>
      </ul>
      <CancelFirst okLabel={plural(n, t("执行 {n} 项", { n: fmtInt(n) }), "Run 1 item")} onOk={() => void confirmExecute(false)} />
    </Modal>
  );
}

function DropsDialog() {
  const d = useCleanup((s) => s.dialog);
  if (d?.kind !== "drops") return null;
  const dropped = d.task.skipped.reduce((n, s) => n + s.count, 0);
  return (
    <Modal title={t("预检时有 {n} 项不能做", { n: fmtInt(dropped) })}>
      {d.task.skipped.map((s) => (
        <details key={s.code} className="small">
          <summary>
            {s.text} · {fmtInt(s.count)}
          </summary>
          {s.examples.map((e) => (
            <div key={e} className="mono muted">
              {e}
            </div>
          ))}
        </details>
      ))}
      <p className="small">{t("这些项会跳过，从不改成别的处理。其余的照常核对后执行")}</p>
      <CancelFirst okLabel={t("执行其余的")} onOk={() => void confirmExecute(true)} />
    </Modal>
  );
}

function UndoDialog() {
  const d = useCleanup((s) => s.dialog);
  const [beside, setBeside] = useState(false);
  if (d?.kind !== "undo") return null;
  const cannot = d.plan.items.filter((x) => !x.can);
  return (
    <Modal title={t("撤销这次清理")}>
      <p className="small">{t("把 {n} 项换回原来的文件（回收站里的从回收站还原）", { n: fmtInt(d.plan.can) })}</p>
      {cannot.length > 0 && <div className="small muted">{t("{n} 项不需要或不能撤销：{why}", { n: fmtInt(cannot.length), why: cannot[0]!.why })}</div>}
      <label className="small cl-check">
        <input type="checkbox" checked={beside} onChange={(e) => setBeside(e.target.checked)} />
        {t("清理之后被改过的，把原件还原到旁边（名字后加「(原件)」）")}
      </label>
      <CancelFirst okLabel={t("撤销 {n} 项", { n: fmtInt(d.plan.can) })} disabled={!d.plan.can} onOk={() => void confirmUndo(beside)} />
    </Modal>
  );
}

function PurgeDialog() {
  const d = useCleanup((s) => s.dialog);
  if (d?.kind !== "purge") return null;
  const p = d.plan;
  return (
    <Modal title={t("清除恢复区")}>
      <p className="small">
        {t("永久删除恢复区里的 {n} 份原件（{size}）？删除之前会核对每一份原件没有变过、保留的那份还在而且没有变；对不上的不删。这一步不能撤销", { n: fmtInt(p.can), size: fmtSize(p.bytes) })}
      </p>
      {p.cannot > 0 && <div className="small warn">{t("{n} 份不能清除：{why}", { n: fmtInt(p.cannot), why: p.items.find((x) => !x.can)?.why ?? "" })}</div>}
      {p.in_trash > 0 && <div className="small muted">{t("回收站里的 {n} 份要清空回收站才腾出（SpaceKnife 不替你清空回收站）", { n: fmtInt(p.in_trash) })}</div>}
      {p.streams > 0 && (
        <label className="small cl-check">
          <input type="checkbox" checked={d.ackStreams} onChange={(e) => setCleanup({ dialog: { ...d, ackStreams: e.target.checked } })} />
          {t("{n} 份原件有没比较过的附加数据（备用流、扩展属性）：也清除它们", { n: fmtInt(p.streams) })}
        </label>
      )}
      <CancelFirst okLabel={t("5 秒后清除 {n} 份", { n: fmtInt(p.can) })} disabled={!p.can} onOk={() => void confirmPurge()} />
    </Modal>
  );
}

function PurgeCountdown() {
  const task = useCleanup((s) => s.task);
  const [left, setLeft] = useState(5);
  useEffect(() => {
    const id = setInterval(() => setLeft((n) => Math.max(0, n - 1)), 1000);
    return () => clearInterval(id);
  }, []);
  if (!task) return null;
  return (
    <Modal title={t("清除恢复区")}>
      <p className="small">{t("{s} 秒后开始清除…", { s: left })}</p>
      <div className="modal-buttons">
        <button className="primary" autoFocus onClick={() => void stopTask()}>
          {t("取消")}
        </button>
      </div>
    </Modal>
  );
}

function ResumeDialog() {
  const st = useCleanup((s) => s.state);
  const offer = st?.resume_offer;
  if (!offer) return null;
  return (
    <Modal title={t("接着上次的清理方案？")}>
      <p className="small">
        {t("上次保存于 {at}：{m} 处手动选择，{r} 条规则。对得上的照用，对不上的列出来", {
          at: offer.saved_at.replace("T", " ").slice(0, 16),
          m: fmtInt(offer.manual),
          r: fmtInt(offer.rules),
        })}
      </p>
      <div className="modal-buttons">
        <button className="ghost" onClick={() => void resume(false)}>
          {t("重新开始")}
        </button>
        <button className="primary" onClick={() => void resume(true)}>
          {t("接着改")}
        </button>
      </div>
    </Modal>
  );
}
