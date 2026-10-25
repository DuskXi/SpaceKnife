// The visualizations this renderer ships: imported once (state/actions.ts) before the first
// connection, so that hello announces their layouts (visual/registry.ts). shapes.v1 draws the
// plugins' own layouts.

import { flameComponent } from "./flame/component";
import { flamePolarComponent } from "./flamePolar/component";
import { registerVisual } from "./registry";
import { shapesComponent } from "./shapes/component";
import { treemapComponent } from "./treemap/component";

registerVisual(treemapComponent);
registerVisual(flameComponent);
registerVisual(flamePolarComponent);
registerVisual(shapesComponent);
