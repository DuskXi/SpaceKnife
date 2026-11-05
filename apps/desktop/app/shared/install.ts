// The two ways the update card offers to put a downloaded version in place, and what each does for
// a kind of install. Main runs the step (main/index.ts installUpdate); the renderer shows the buttons
// (shell/UpdateCard.tsx). Neither ever starts by itself: only a click in the card does.

/** How this copy was installed (spaceknife_protocol.update.InstallKind). */
export type InstallKind = "nsis" | "portable" | "appimage" | "deb" | "rpm" | "dmg" | "dev" | "unknown";

/** 「无缝更新」: SpaceKnife puts the new version in place itself and starts again. The classic way:
 *  the installer, disk image or package opens and SpaceKnife quits (or the file is shown in its
 *  folder, to replace by hand). */
export type InstallHow = "seamless" | "classic";

/**
 * - "silent": the Windows installer runs without its pages and starts the new version
 * - "swap": the new version's backend swaps a portable copy's files once the app is gone
 *   (spaceknife.update_swap) and starts it
 * - "replace": the new AppImage takes the old one's place and starts
 * - "wizard": the Windows installer with its pages; SpaceKnife quits
 * - "open": the disk image or package opens in the system's installer; SpaceKnife quits
 * - "reveal": the downloaded file is shown in its folder, to unpack or replace by hand
 */
export type InstallStep = "silent" | "swap" | "replace" | "wizard" | "open" | "reveal";

const STEPS: Partial<Record<InstallKind, Partial<Record<InstallHow, InstallStep>>>> = {
  nsis: { seamless: "silent", classic: "wizard" },
  portable: { seamless: "swap", classic: "reveal" },
  appimage: { seamless: "replace", classic: "reveal" },
  dmg: { classic: "open" },
  deb: { classic: "open" },
  rpm: { classic: "open" },
};

/** What a click on that button does for this kind of install; null when the card doesn't offer it. */
export function installStep(kind: InstallKind, how: InstallHow): InstallStep | null {
  return STEPS[kind]?.[how] ?? null;
}

/** Whether the step closes SpaceKnife (a running scan is asked about first). */
export function stepQuits(step: InstallStep): boolean {
  return step !== "reveal";
}
