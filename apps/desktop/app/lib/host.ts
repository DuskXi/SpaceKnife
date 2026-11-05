// Where the interface runs (SkApi.host): SpaceKnife's own window, or a browser tab served by headless
// mode. In a browser the files are on another computer (or this one, through the server): no window
// buttons, no file manager, terminal or update install here (shim.ts says why when asked).

export const inBrowser = (): boolean => window.sk.host === "browser";

/** The computer the files are on, in a browser tab; "" in the app. */
export const serverName = (): string => window.sk.remote?.hostname ?? "";
