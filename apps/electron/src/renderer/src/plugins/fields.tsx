// Plugin fields of a node (docs/plugins.md 「node_field」): a 「插件」 section of the details page after
// the app's own facts, and a few lines on the hover card. The backend asks every enabled plugin in
// parallel, each within its budget (1 s for the details, 30 ms for the hover card), and caches the
// answers; the hover card never waits: it shows what is cached, a later hover the rest.

import { useEffect, useState } from "react";

import type { PluginField, PluginFieldsResult } from "@/protocol/generated";
import { cancellable } from "@/state/actions";
import { useApp } from "@/state/store";
import { classHexNow } from "@/lib/classes";
import { t } from "@/lib/i18n";

import { toneColor } from "./colors";
import { showNode } from "./nodes";
import { ActionButton } from "./report/ReportView";
import { formatCell, numericUnit } from "./report/format";

/** Whether any enabled plugin has a field for this place (else nothing is asked). */
export const useHasFields = (slot: "inspector" | "hover"): boolean => useApp((s) => !!s.extensions?.node_fields.some((f) => f.slots.includes(slot)));

/** The fields of a node for a place; `tick` asks again (the details column's throttled refresh). */
export function useFields(scanId: string | null | undefined, node: number | null, slot: "inspector" | "hover", tick: unknown = 0): PluginFieldsResult | null {
  const has = useHasFields(slot);
  const version = useApp((s) => s.extensions?.version ?? 0);
  const [res, setRes] = useState<PluginFieldsResult | null>(null);
  const [again, setAgain] = useState(0);
  useEffect(() => {
    setRes(null);
    setAgain(0);
  }, [has, scanId, node, slot, tick, version]);
  useEffect(() => {
    if (!has || !scanId || node == null || node < 0) return;
    let alive = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const job = cancellable("plugin.fields", { scan_id: scanId, node, slot });
    job.promise
      .then((r) => {
        if (!alive) return;
        setRes(r);
        // the details panel: a busy plugin answers later, ask again a few times (the hover card
        // asks again on the next hover)
        if (r?.pending && slot === "inspector" && again < 5) timer = setTimeout(() => alive && setAgain((n) => n + 1), 1500);
      })
      .catch(() => alive && setRes(null));
    return () => {
      alive = false;
      if (timer) clearTimeout(timer);
      job.cancel();
    };
  }, [has, scanId, node, slot, tick, version, again]);
  return res;
}

function FieldValue({ f }: { f: PluginField }) {
  const text = formatCell(f.value, f.unit);
  const color = toneColor(f.tone, classHexNow) ?? undefined;
  return (
    <span className={numericUnit(f.unit) ? "mono" : undefined} style={{ color }} title={f.unit === "path" ? String(f.value ?? "") : undefined}>
      {f.node ? (
        <button className="report-link" onClick={() => void showNode(f.node!)}>
          {text}
        </button>
      ) : (
        text
      )}
    </span>
  );
}

/** The 「插件」 section of the details page. */
export function InspectorFields({ scanId, viewId, node, tick }: { scanId: string; viewId: string; node: number; tick: unknown }) {
  const res = useFields(scanId, node, "inspector", tick);
  if (!res?.fields.length) return null;
  return (
    <section className="ins-section plugin-fields" aria-label={t("插件")}>
      <h3>{t("插件")}</h3>
      <dl className="report-kv">
        {res.fields.map((f) => (
          <div key={f.id} className="report-kv-row">
            <dt className="muted">{f.title}</dt>
            <dd>
              <FieldValue f={f} />
              {f.action && <ActionButton action={f.action} env={{ owner: f.plugin, ctx: { scan_id: scanId, view_id: viewId, node, selection: [node] } }} rc={{}} tiny />}
            </dd>
          </div>
        ))}
      </dl>
    </section>
  );
}

/** A few plugin lines of the hover card (at most four, from the cache). */
export function HoverFields({ scanId, node }: { scanId: string | undefined; node: number | null }) {
  const res = useFields(scanId, node, "hover");
  if (!res?.fields.length) return null;
  return (
    <div className="hc-fields">
      {res.fields.slice(0, 4).map((f) => (
        <div key={f.id} className="hc-field">
          <span className="muted">{f.title}</span>
          <FieldValue f={f} />
        </div>
      ))}
    </div>
  );
}
