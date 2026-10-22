// Icons of what plugins add: a name of the app's icon set, or a plugin's own SVG (icons/<name>.svg),
// which the backend rebuilt from shapes only with currentColor (extensions.get icons, keyed
// "<plugin>:<name>"). Anything else falls back to a neutral icon.

import { icons } from "@design/icons";

import { useApp } from "@/state/store";
import { Icon, type IconName } from "@/ui/Icon";

/** The app's icon for a name ("vizFlame", "viz-flame"), or null. */
export function builtinIcon(name: string | null | undefined): IconName | null {
  if (!name) return null;
  const camel = name.replace(/-([a-z0-9])/g, (_, c: string) => c.toUpperCase());
  return camel in icons ? (camel as IconName) : null;
}

/** The key of a plugin's own icon: "icons/star.svg" or "star" → "<plugin>:star". */
export function pluginIconKey(plugin: string, name: string): string {
  const stem = name.replace(/^.*\//, "").replace(/\.svg$/i, "");
  // as the backend names it (plugins/adapters.py _slug)
  const slug = (stem.toLowerCase().replace(/[^a-z0-9]+/g, "_").replace(/^_+|_+$/g, "") || "x").slice(0, 24).replace(/_+$/, "") || "x";
  return `${plugin}:${slug}`;
}

export function PluginIcon({ plugin, name, size = 16, fallback = "layers" }: { plugin?: string | null; name?: string | null; size?: number; fallback?: IconName }) {
  const svg = useApp((s) => (plugin && name ? s.extensions?.icons[pluginIconKey(plugin, name)] : undefined));
  const own = builtinIcon(name);
  if (own) return <Icon name={own} size={size} />;
  if (svg) {
    // sanitized by the backend (plugins/icons.py): svg, shapes, geometry, currentColor only
    return <span className="plugin-icon" style={{ width: size, height: size }} aria-hidden="true" dangerouslySetInnerHTML={{ __html: svg }} />;
  }
  return <Icon name={fallback} size={size} />;
}
