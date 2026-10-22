// What this renderer announces in session.hello for plugins (docs/plugins.md, 「能力推导」):
// plugins.v1 (management, reports, actions, fields, cleanups, plugin.<id>.* calls and events),
// colors.v1 (open colour-mode ids and the frame's pal column), panels.v1 (sandboxed web panels:
// Electron and the browser mode both host them).

export const PLUGIN_CAPABILITIES = ["plugins.v1", "colors.v1", "panels.v1"] as const;

/** A binary frame or a notification of a plugin (plugin.<id>.<name>), not a core one. */
export const isPluginName = (name: string): boolean => name.startsWith("plugin.") && name.split(".").length >= 4;
