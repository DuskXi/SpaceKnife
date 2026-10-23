// The rule bar of cleanup mode (docs/dupes.md §16.13.4), by layer: locked, the keep rules (with
// the base criteria, one copy per volume, folder-consistent), the action rules in order (each says
// how many copies it decides), folder pairs, manual choices. A new rule takes a folder, tags or a
// file name pattern and an effect; changing rules is previewed like every bulk edit.

import { useState } from "react";

import type { CleanupRuleView, CleanupState } from "@/protocol/generated";
import { ACTIONS, actionLabel, criterionLabel, edit, preview, setOptions, useCleanup } from "@/state/cleanup";
import { fmtInt, fmtSize } from "@/lib/format";
import { t } from "@/lib/i18n";
import { Icon } from "@/ui/Icon";

const CRITERIA = ["oldest_created", "oldest_modified", "newest_modified", "shortest_path", "longest_path", "shallowest", "deepest", "plain_name"];
const TAGS = ["sync", "vcs", "app", "program", "game", "cache", "temp", "downloads", "backup", "project", "media_library", "hidden_temp_root", "link_target", "removable", "recent"];

export const tagLabel = (tag: string): string =>
  ({
    sync: t("同步盘"),
    vcs: t("版本库"),
    app: t("应用数据"),
    program: t("程序"),
    game: t("游戏"),
    cache: t("缓存"),
    temp: t("临时文件夹"),
    downloads: t("下载和桌面"),
    backup: t("像备份"),
    project: t("工程"),
    media_library: t("媒体库"),
    hidden_temp_root: t("卷根的隐藏临时文件"),
    link_target: t("链接目标"),
    removable: t("可移动盘"),
    recent: t("最近改过"),
  })[tag] ?? tag;

export function RuleBar() {
  const st = useCleanup((s) => s.state);
  if (!st) return null;
  return (
    <aside className="cl-rules" aria-label={t("规则")}>
      <section>
        <h4>{t("1 必须保留")}</h4>
        <div className="small muted">{t("程序需要或受保护的、有几个名字的、名字以点或空格结尾的、网络盘上的、版本库内部的：只能保留")}</div>
      </section>
      <section>
        <h4>{t("2 保留规则")}</h4>
        <Criteria st={st} />
        <label className="cl-check small">
          <input type="checkbox" checked={st.per_volume} onChange={(e) => void setOptions({ per_volume: e.target.checked })} />
          {t("跨盘时每个卷至少留一份")}
        </label>
        <label className="cl-check small">
          <input type="checkbox" checked={st.folder_consistent} onChange={(e) => void setOptions({ folder_consistent: e.target.checked })} />
          {t("文件夹一致（自动 {n} 对）", { n: fmtInt(st.auto_pairs) })}
        </label>
        {st.keep_rules.map((r) => (
          <RuleCard key={r.id} r={r} />
        ))}
        <NewRule kind="keep" />
      </section>
      <section>
        <h4>{t("3 处理规则")}</h4>
        <div className="small muted">{t("从上往下第一条命中的生效；做不到时按后备，最后是「不动」")}</div>
        {st.action_rules.map((r, i) => (
          <RuleCard key={r.id} r={r} index={i + 1} />
        ))}
        <NewRule kind="action" />
      </section>
      <section>
        <h4>{t("4 文件夹对 · 5 手动")}</h4>
        <div className="small">
          {t("文件夹对规则 {n} 条", { n: fmtInt(st.pairs.length) })} · {t("手动选择 {n} 处", { n: fmtInt(st.manual) })}
          {st.manual > 0 && (
            <button className="ghost tiny" onClick={() => void preview({ op: "override_clear", scope: {} }, t("已清除手动选择"))}>
              {t("清除")}
            </button>
          )}
        </div>
      </section>
    </aside>
  );
}

function Criteria({ st }: { st: CleanupState }) {
  const move = (i: number, d: number) => {
    const c = [...st.criteria];
    const j = i + d;
    if (j < 0 || j >= c.length) return;
    [c[i], c[j]] = [c[j]!, c[i]!];
    void setOptions({ criteria: c as never });
  };
  const add = (c: string) => void setOptions({ criteria: [...st.criteria, c] as never });
  const remove = (i: number) => void setOptions({ criteria: st.criteria.filter((_, k) => k !== i) as never });
  return (
    <div className="cl-criteria">
      <div className="small muted">{t("基本判据（一样时看下一项）")}</div>
      <ol className="small">
        {st.criteria.map((c, i) => (
          <li key={c}>
            {criterionLabel(c)}
            <button className="icon-btn tiny" aria-label={t("上移")} disabled={i === 0} onClick={() => move(i, -1)}>
              ↑
            </button>
            <button className="icon-btn tiny" aria-label={t("去掉")} disabled={st.criteria.length < 2} onClick={() => remove(i)}>
              <Icon name="close" size={10} />
            </button>
          </li>
        ))}
      </ol>
      <select className="small" aria-label={t("加一条判据")} value="" onChange={(e) => e.target.value && add(e.target.value)}>
        <option value="">{t("加一条判据…")}</option>
        {CRITERIA.filter((c) => !st.criteria.includes(c)).map((c) => (
          <option key={c} value={c}>
            {criterionLabel(c)}
          </option>
        ))}
      </select>
    </div>
  );
}

function RuleCard({ r, index }: { r: CleanupRuleView; index?: number }) {
  return (
    <div className={`cl-rule${r.enabled ? "" : " off"}${r.risky ? " risky" : ""}`}>
      <div className="cl-rule-head">
        <input type="checkbox" aria-label={t("启用")} checked={r.enabled} onChange={(e) => void toggleRule(r, e.target.checked)} />
        <span className="small">
          {index !== undefined && <span className="muted">{index}. </span>}
          {r.label}
        </span>
        <span className="flex" />
        {index !== undefined && index > 1 && (
          <button className="icon-btn tiny" aria-label={t("上移")} onClick={() => void edit({ op: "rule_move", rule_id: r.id, index: index - 2 })}>
            ↑
          </button>
        )}
        <button className="icon-btn tiny" aria-label={t("删掉这条规则")} onClick={() => void preview({ op: "rule_remove", rule_id: r.id }, t("已删掉规则"))}>
          <Icon name="close" size={10} />
        </button>
      </div>
      <div className="small muted">
        {t("决定 {n} 份 · {size}", { n: fmtInt(r.hits), size: fmtSize(r.bytes) })}
        {r.fallback.length > 0 && ` · ${t("后备：{list}", { list: r.fallback.map(actionLabel).join(" → ") })}`}
      </div>
    </div>
  );
}

function toggleRule(r: CleanupRuleView, enabled: boolean): Promise<boolean> {
  return edit({
    op: "rule_update",
    rule_id: r.id,
    rule: {
      id: r.id,
      kind: r.kind,
      effect: r.effect,
      when: r.when as never,
      enabled,
      label: "",
      label_key: "",
      folders: r.folders,
      n: r.n,
      criteria: r.criteria as never,
      fallback: r.fallback as never,
      target_under: r.target_under,
      symlink_style: "",
    },
  });
}

/** A new rule: where (folders, tags, a name pattern), then its effect. Previewed before it is kept. */
function NewRule({ kind }: { kind: "keep" | "action" }) {
  const [open, setOpen] = useState(false);
  const [under, setUnder] = useState("");
  const [tag, setTag] = useState("");
  const [glob, setGlob] = useState("");
  const [effect, setEffect] = useState(kind === "keep" ? "prefer" : "trash");
  const [fallback, setFallback] = useState("leave");
  if (!open)
    return (
      <button className="ghost tiny cl-add" onClick={() => setOpen(true)}>
        <Icon name="plus" size={11} /> {kind === "keep" ? t("加一条保留规则") : t("加一条处理规则")}
      </button>
    );
  const folders = under
    .split(/[;\n]/)
    .map((x) => x.trim())
    .filter(Boolean);
  const submit = () => {
    const when = {
      under: kind === "action" || effect === "keep_all" || effect === "per_folder" ? folders : [],
      tags: tag ? [tag] : [],
      name_glob: glob,
      risk_at_least: effect === "leave_high" ? ("high" as const) : null,
    };
    void preview(
      {
        op: "rule_add",
        rule: {
          id: 0,
          kind,
          effect: effect === "leave_high" ? "leave" : effect,
          when: when as never,
          enabled: true,
          label: "",
          label_key: "",
          folders: kind === "keep" && (effect === "prefer" || effect === "avoid") ? folders : [],
          n: 2,
          criteria: [],
          fallback: kind === "action" && fallback !== "leave" ? [fallback as never] : [],
          target_under: "",
          symlink_style: "",
        },
      },
      t("已加规则"),
    ).then(() => setOpen(false));
  };
  return (
    <div className="cl-newrule small">
      <label>
        {t("文件夹（每行一个）")}
        <textarea rows={2} value={under} onChange={(e) => setUnder(e.target.value)} placeholder={t("例如 D:\\Photos")} />
      </label>
      <label>
        {t("标签")}
        <select value={tag} onChange={(e) => setTag(e.target.value)}>
          <option value="">—</option>
          {TAGS.map((x) => (
            <option key={x} value={x}>
              {tagLabel(x)}
            </option>
          ))}
        </select>
      </label>
      <label>
        {t("名字")}
        <input value={glob} onChange={(e) => setGlob(e.target.value)} placeholder="*.psd" />
      </label>
      <label>
        {t("效果")}
        <select value={effect} onChange={(e) => setEffect(e.target.value)}>
          {kind === "keep" ? (
            <>
              <option value="prefer">{t("优先保留在这些文件夹")}</option>
              <option value="avoid">{t("尽量不留在这些文件夹")}</option>
              <option value="keep_all">{t("这里的全部保留")}</option>
              <option value="per_folder">{t("每个文件夹各留一份")}</option>
              <option value="keep_n">{t("至少留 2 份")}</option>
            </>
          ) : (
            <>
              {ACTIONS.map((a) => (
                <option key={a} value={a}>
                  {actionLabel(a)}
                </option>
              ))}
              <option value="leave_high">{t("有高风险的 → 不动")}</option>
            </>
          )}
        </select>
      </label>
      {kind === "action" && (
        <label>
          {t("做不到时")}
          <select value={fallback} onChange={(e) => setFallback(e.target.value)}>
            {ACTIONS.map((a) => (
              <option key={a} value={a}>
                {actionLabel(a)}
              </option>
            ))}
          </select>
        </label>
      )}
      <div className="modal-buttons">
        <button className="ghost small" onClick={() => setOpen(false)}>
          {t("取消")}
        </button>
        <button className="primary small" onClick={submit}>
          {t("预览")}
        </button>
      </div>
    </div>
  );
}
