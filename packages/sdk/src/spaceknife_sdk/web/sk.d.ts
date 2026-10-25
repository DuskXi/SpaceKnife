// Types of the SpaceKnife web panel bridge (./_sk/bridge.js). A plugin's own methods, events and
// states come from its generated web/sk-plugin.d.ts (spaceknife plugin build), which declares
// `sk` as Sk<Methods, Events, States>; without it every call is `unknown`.

/** A node of a scan as plugins see it. */
export interface NodeRef {
  scan_id: string;
  node: number;
  epoch?: number;
  path?: string;
}

/** Where the user is: the scan, the view, the selected node (or the zoom root), the comparison. */
export interface PanelContext {
  scan_id: string | null;
  view_id: string | null;
  node: number | null;
  selection: number[];
  /** where the page is: "tab", "inspector" (the details column) or "dialog" */
  slot?: "tab" | "inspector" | "dialog";
  /** the panel's params from its declaration (one page placed twice, each with its own) */
  params?: Record<string, string | number | boolean>;
  compare_id: string | null;
}

export interface PanelInfo {
  plugin: string;
  lang: "zh-CN" | "en";
  theme: { mode: "dark" | "light"; vars: Record<string, string> } | null;
  context: PanelContext;
}

/** An error the app or the plugin answered with: `code` is a protocol error code
 * (-32014 PERMISSION_DENIED, -32015 PLUGIN_TIMEOUT, -32019 PLUGIN_ERROR, -32020 PLUGIN_LIMIT, -32800 cancelled). */
export interface SkError extends Error {
  code: number;
}

/** A method result that was numpy columns: typed arrays, handed over without copying. */
export interface Columns {
  columns: Record<string, ArrayLike<number> & { buffer: ArrayBuffer }>;
  blobs: Record<string, Uint8Array>;
  layout: string;
  count: number;
}

type Fallback<T, D> = [keyof T] extends [never] ? D : T;

export interface Sk<
  M extends Record<string, { params: unknown; result: unknown }> = Record<string, { params: unknown; result: unknown }>,
  E extends Record<string, unknown> = Record<string, unknown>,
  S extends Record<string, unknown> = Record<string, unknown>,
> {
  readonly ready: Promise<PanelInfo>;
  readonly plugin: string;
  readonly lang: "zh-CN" | "en";
  readonly context: PanelContext;
  /** One of this plugin's methods, by its local name. */
  call<K extends keyof Fallback<M, Record<string, { params: unknown; result: unknown }>> & string>(name: K, params?: M[K]["params"], opts?: { signal?: AbortSignal }): Promise<M[K]["result"]>;
  /** A built-in method a panel may use: tree.node tree.children tree.summary tree.find (tree.read),
   * scan.list scan.status volume.list view.list visual.list filter.parse filter.vocab classes.get
   * export.targets basket.list, snapshot.list (snapshots.read), compare.info compare.summary
   * compare.node insight.node (tree.read), basket.add (basket.propose). */
  core(method: string, params?: Record<string, unknown>, opts?: { signal?: AbortSignal }): Promise<unknown>;
  on<K extends keyof E & string>(event: K, cb: (payload: E[K]) => void): () => void;
  state<K extends keyof S & string>(name: K, cb: (value: S[K]) => void): () => void;
  subscribe(stream: string, params: Record<string, unknown> | null, cb: (item: unknown | Columns) => void): () => void;
  onContext(cb: (ctx: PanelContext) => void): () => void;
  onLang(cb: (lang: "zh-CN" | "en") => void): () => void;
  /** The app's theme changed; the --sk-* variables already follow (for what a canvas paints itself). */
  onTheme(cb: (mode: "dark" | "light") => void): () => void;
  /** The tree of the context's scan changed (a rescan, files changed, the scan went on). */
  onTree(cb: (e: { scan_id: string | null }) => void): () => void;
  host: {
    /** Select it in its scan's view; { stay: true }: the user stays where the panel is. */
    select(node: number | NodeRef, opts?: { stay?: boolean }): Promise<null>;
    zoom(node: number | NodeRef): Promise<null>;
    filter(expr: string): Promise<null>;
    /** one of this plugin's commands, by its full id */
    command(id: string): Promise<null>;
    toast(text: string): Promise<null>;
    /** needs host.open */
    reveal(node: number | NodeRef): Promise<null>;
    open(node: number | NodeRef): Promise<null>;
    terminal(node: number | NodeRef): Promise<null>;
    /** https only; the user confirms first */
    url(url: string): Promise<null>;
    /** needs clipboard */
    copy(text: string): Promise<null>;
    /** needs basket.propose: into the delete basket (deleting is still the user's step) */
    basket(nodes: (number | NodeRef)[], reason?: string, scanId?: string): Promise<{ added: number }>;
  };
  /** The text in the interface language, {placeholders} filled. */
  t(zh: string, en: string, fields?: Record<string, string | number>): string;
  /** Numbers as the app writes them: size(1536000) "1.5 MB" (the user's unit base), count(1234)
   * "1,234", date(unixSeconds) "2026-10-24 21:30". */
  format: { size(bytes: number): string; count(n: number): string; date(unixSeconds: number): string };
  /** Ask for this height (40–800 px) in the details column or a dialog. */
  resize(height: number): void;
}

declare const sk: Sk;
export default sk;
