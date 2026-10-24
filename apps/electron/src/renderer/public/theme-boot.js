// The theme chosen in the settings (appearance.theme), set before the page's styles apply. In the app,
// main restores it (nativeTheme, <userData>/theme.json) before the window opens; in a browser
// (headless mode) nothing tells the page until the backend's settings arrive, so lib/theme.ts also
// keeps the choice in localStorage under this key, and this reads it back: a reload opens in that
// theme from its first frame. A classic script in <head> of its own: it runs before the first paint,
// and the Content-Security-Policy allows no inline script.
try {
  var theme = localStorage.getItem("sk.theme");
  if (theme === "light" || theme === "dark") document.documentElement.dataset.theme = theme;
} catch (e) {
  // no storage: the browser's theme until the settings arrive
}
