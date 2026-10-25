// Insight cards of the details page (UI-MAIN-41/42, docs/plugins.md 「insight」): what the selected
// item is, whether it can be cleaned up and how. Cards come from the insight rules (the app's, the
// plugins' insight_rules, the user's own) and from plugins' code (insight.node). A card's command is
// shown and can be copied, never run; 「加入删除篮」 only puts the item in the basket.

import { useEffect, useState } from "react";

import type { InsightCard } from "@/protocol/generated";
import { basketAdd, call, cancellable } from "@/state/actions";
import { toast, useApp } from "@/state/store";
import { t } from "@/lib/i18n";
import { Icon, type IconName } from "@/ui/Icon";

import { pluginName } from "./store";

const KIND_ICON: Record<string, IconName> = { app: "layers", cache: "clock", build: "terminal", vm: "drive", chat: "log", game: "play", system: "shield" };

/** What a card's cleanup advice says. */
export function cleanupText(cleanup: string): { text: string; tone: "ok" | "warn" | "muted"; basket: boolean } {
  switch (cleanup) {
    case "safe":
      return { text: t("可以放心清理"), tone: "ok", basket: true };
    case "tool":
      return { text: t("用它自己的程序或命令清理"), tone: "warn", basket: false };
    case "keep":
      return { text: t("建议保留"), tone: "muted", basket: false };
    default:
      return { text: t("清理前先确认里面的内容"), tone: "warn", basket: true };
  }
}

export function InsightCards({ scanId, node, tick }: { scanId: string; node: number; tick: unknown }) {
  const [cards, setCards] = useState<InsightCard[]>([]);
  const on = useApp((s) => s.capabilities.includes("plugins.v1"));
  const version = useApp((s) => s.extensions?.version ?? 0);
  useEffect(() => {
    setCards([]);
    if (!on || node < 0) return;
    let alive = true;
    const job = cancellable("insight.node", { scan_id: scanId, node });
    job.promise.then((r) => alive && r && setCards(r.cards)).catch(() => undefined);
    return () => {
      alive = false;
      job.cancel();
    };
  }, [on, scanId, node, tick, version]);
  if (!cards.length) return null;
  return (
    <section className="ins-section insight-cards" aria-label={t("这是什么")}>
      {cards.map((c, i) => (
        <Card key={`${c.rule}:${c.plugin ?? ""}:${i}`} card={c} scanId={scanId} node={node} />
      ))}
    </section>
  );
}

function Card({ card, scanId, node }: { card: InsightCard; scanId: string; node: number }) {
  const advice = cleanupText(card.cleanup);
  const add = async () => {
    // a card about a folder above the selection names its path: that folder goes in
    let target = node;
    if (card.path) {
      const found = await call("tree.find", { scan_id: scanId, path: card.path }).catch(() => null);
      if (found?.node) target = found.node.id;
    }
    await basketAdd(scanId, target); // the user's own pick: no plugin origin
  };
  return (
    <article className="insight-card">
      <header>
        <span className="insight-icon">
          <Icon name={KIND_ICON[card.kind] ?? "info"} size={14} />
        </span>
        <strong className="flex">{card.title}</strong>
        {card.source === "plugin" && card.plugin && <span className="muted small">{pluginName(card.plugin)}</span>}
      </header>
      {card.inside && card.path && (
        <p className="muted small insight-about" title={card.path}>
          {t("说的是它所在的文件夹 {name}", { name: card.path.split(/[\\/]/).filter(Boolean).pop() ?? card.path })}
        </p>
      )}
      <p>{card.reason}</p>
      {card.app && <p className="muted small">{t("来自：{app}", { app: card.app })}</p>}
      {card.note && <p className="muted small">{card.note}</p>}
      <div className={`insight-advice ${advice.tone}`}>{advice.text}</div>
      {card.regenerate && (
        <p className="small insight-regen">
          <Icon name="refresh" size={12} /> {t("删掉以后：{how}", { how: card.regenerate })}
        </p>
      )}
      {card.command && (
        <div className="insight-command">
          <code className="mono" title={t("只显示，SpaceKnife 不会运行它")}>
            {card.command}
          </code>
          <button
            className="icon-btn tiny"
            aria-label={t("复制命令")}
            title={t("复制命令")}
            onClick={() => {
              window.sk.shell.copyText(card.command);
              toast(t("已复制"));
            }}
          >
            <Icon name="copy" size={12} />
          </button>
        </div>
      )}
      {advice.basket && (
        <button className="ghost tiny" onClick={() => void add()}>
          <Icon name="basket" size={12} /> {t("加入删除篮")}
        </button>
      )}
    </article>
  );
}
