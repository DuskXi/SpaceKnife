import "@fontsource-variable/geist";
import "@fontsource-variable/geist-mono";
import "@fontsource-variable/noto-sans-sc";
import "@design/tokens.css";
import "./styles/app.css";
import "./styles/help.css";
import "./styles/browser.css";
import "./styles/plugins.css";

import { installShim } from "./shim";

import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

// In a browser (headless mode, the demo) window.sk comes from the shim, which may first ask the
// server what computer it is on: the interface loads after it, as modules read window.sk.platform.
void installShim().then(async () => {
  const [{ App }, { ErrorBoundary }] = await Promise.all([import("./App"), import("./ui/ErrorBoundary")]);
  createRoot(document.getElementById("root")!).render(
    <StrictMode>
      <ErrorBoundary>
        <App />
      </ErrorBoundary>
    </StrictMode>,
  );
});
