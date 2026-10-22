// The theme a web panel gets (its /_sk/base.css reads these variables): the app's design tokens
// under short names. base.css carries the same values as defaults, so a panel looks right before
// the first theme message and in the bridge's mock mode (resources/plugin-sdk/base.css; the test
// keeps the two in step).

import { tokens } from "@design/tokens";

const c = tokens.color;

/** CSS variable → value, as the panel's bridge.js sets them on its document. */
export function themeVars(): Record<string, string> {
  return {
    "--sk-bg": c.bg.panel,
    "--sk-bg-raised": c.bg.raised,
    "--sk-bg-hover": c.bg.hover,
    "--sk-text": c.text.primary,
    "--sk-muted": c.text.secondary,
    "--sk-subtle": c.text.tertiary,
    "--sk-disabled": c.text.disabled,
    "--sk-accent": c.accent.default,
    "--sk-on-accent": c.text.onAccent,
    "--sk-border": c.line.subtle,
    "--sk-border-strong": c.line.strong,
    "--sk-ok": c.status.success,
    "--sk-warn": c.status.warning,
    "--sk-danger": c.status.danger,
    "--sk-font": tokens.font.family.ui.map((f) => (f.includes(" ") ? `"${f}"` : f)).join(", "),
    "--sk-mono": tokens.font.family.mono.map((f) => (f.includes(" ") ? `"${f}"` : f)).join(", "),
  };
}
