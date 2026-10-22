import { useEffect } from "react";

import { boot } from "@/state/actions";
import { BrowserLayer } from "@/shell/BrowserLayer";
import { ContextMenu } from "@/shell/ContextMenu";
import { ExportDialog } from "@/shell/ExportDialog";
import { FullDiskAccessDialog } from "@/shell/FullDiskAccessDialog";
import { OpToast } from "@/shell/OpToast";
import { activeTab, useApp } from "@/state/store";
import { inBrowser } from "@/lib/host";
import { t } from "@/lib/i18n";
import { LogPanel } from "@/shell/LogPanel";
import { Palette } from "@/shell/Palette";
import { StatusBar } from "@/shell/StatusBar";
import { TitleBar } from "@/shell/TitleBar";
import { Toolbar } from "@/shell/Toolbar";
import { UpdatePrompt } from "@/shell/UpdateCard";
import { HelpPage } from "@/pages/HelpPage";
import { MachinePage } from "@/pages/MachinePage";
import { ScanPage } from "@/pages/ScanPage";
import { SettingsPage } from "@/pages/SettingsPage";
import { StartPage } from "@/pages/StartPage";
import { visibleSurface } from "@/state/updates";
import { onKey } from "@/plugins/keymap";
import { CleanupDialog } from "@/plugins/cleanup";
import { ConfirmDialog } from "@/plugins/confirm";
import { PluginDialogHost, PluginTabPage } from "@/plugins/slots";

export function App() {
  const tab = useApp((s) => activeTab(s));
  const conn = useApp((s) => s.conn);
  const message = useApp((s) => s.connMessage);
  const toastText = useApp((s) => s.toast);
  const paletteOpen = useApp((s) => s.paletteOpen);
  const updateDialog = useApp((s) => visibleSurface(s.update, s.updateOpen) === "dialog");
  useApp((s) => s.lang); // 「界面语言」: a change re-renders the whole tree, every t() follows (M7.5)

  useEffect(() => {
    void boot();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  return (
    <>
      <div className="app" inert={paletteOpen || updateDialog}>
        <TitleBar />
        <Toolbar />
        {tab.kind === "scan" ? (
          <ScanPage tab={tab} />
        ) : tab.kind === "settings" ? (
          <SettingsPage />
        ) : tab.kind === "help" ? (
          <HelpPage />
        ) : tab.kind === "machine" ? (
          <MachinePage />
        ) : tab.kind === "plugin" ? (
          <PluginTabPage tab={tab} />
        ) : (
          <StartPage tab={tab} />
        )}
        <LogPanel />
        <StatusBar />
        {conn === "failed" && (
          <div className="fatal">
            <h2>{inBrowser() ? t("连不上 SpaceKnife") : t("后端没有启动")}</h2>
            <p className="mono">{message}</p>
          </div>
        )}
        {toastText && (
          <div className="toast" role="status">
            {toastText}
          </div>
        )}
      </div>
      <Palette />
      <ContextMenu />
      <OpToast />
      <ExportDialog />
      <PluginDialogHost />
      <CleanupDialog />
      <ConfirmDialog />
      <FullDiskAccessDialog />
      <UpdatePrompt />
      <BrowserLayer />
    </>
  );
}
