// The visualizations this renderer ships: imported once (state/actions.ts) before the first
// connection, so that hello announces their layouts (visual/registry.ts).

import { flameComponent } from "./flame/component";
import { flamePolarComponent } from "./flamePolar/component";
import { registerVisual } from "./registry";
import { treemapComponent } from "./treemap/component";

registerVisual(treemapComponent);
registerVisual(flameComponent);
registerVisual(flamePolarComponent);
