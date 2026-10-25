// Plugins' commands in a context menu and in the details panel's actions: whether each can run on
// the node there (its `when`, then its `check` method), asked in one palette.check_many. One that
// can't is greyed with the reason, as the palette's row says it; until the answer comes, they show
// as they are (running one asks palette.check anyway).

import { useEffect, useState } from "react";

import type { PaletteCommand } from "@/protocol/generated";
import { call } from "@/state/actions";
import { paletteContext } from "@/state/commands";
import { get } from "@/state/store";
import { t } from "@/lib/i18n";

export interface CommandsOn {
  viewId: string | null;
  scanId: string | null;
  /** the node they are about (a menu's block, the details panel's node) */
  node: number | null;
}

/** Why a command can't run there (greyed), or "" when it can (or before the answer). */
export function unavailable(states: ReadonlyMap<string, PaletteCommand> | null, id: string): string {
  const c = states?.get(id);
  return c && !c.enabled ? c.status || t("当前不可用") : "";
}

/** The states of commands `ids` on `on`; asked again when `epoch` changes (the scan finished, a
 * setting changed …). Null until answered, and when there is nothing to ask. */
export function useCommandStates(ids: readonly string[], on: CommandsOn | null, epoch = ""): ReadonlyMap<string, PaletteCommand> | null {
  const [states, setStates] = useState<ReadonlyMap<string, PaletteCommand> | null>(null);
  const key = on && ids.length ? `${ids.join(",")}|${on.viewId}|${on.scanId}|${on.node}|${epoch}` : "";
  useEffect(() => {
    setStates(null);
    if (!key || !on) return;
    let live = true;
    void call("palette.check_many", { ids: ids.slice(0, 64), context: paletteContext(get(), on) })
      .then((r) => live && setStates(new Map(r.commands.map((c) => [c.id, c]))))
      .catch(() => undefined); // an older backend: they stay as they are
    return () => {
      live = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps -- the key says what changed
  }, [key]);
  return states;
}
