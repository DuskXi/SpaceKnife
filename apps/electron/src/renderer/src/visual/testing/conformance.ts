// What every component's geometry must do for the host and the shell (docs/visuals.md): labels land
// on their own row, bounds hold anchors and outlines, nothing hits outside the canvas, and empty
// frames or HIT_NONE never throw. Run per registered component on its fixture scenes; [] = conforms.

import { GEOMETRY_COLUMNS, isNested, isWithin } from "../frame";
import { HIT_NONE, HIT_ROOT, type Box, type Hit, type Pt, type VisualComponent, type VisualFrame, type VisualGeometry } from "../types";
import { PathRecorder } from "./fixtures";

/** Rows whose short side is below this carry no label and are not checked for one. */
const MIN_MINOR = 4;

const inside = (b: Box, p: Pt, tol: number) => p.x >= b.x - tol && p.x <= b.x + b.w + tol && p.y >= b.y - tol && p.y <= b.y + b.h + tol;
const fmt = (p: Pt) => `(${p.x.toFixed(1)}, ${p.y.toFixed(1)})`;

export function checkGeometry<F extends VisualFrame>(component: Pick<VisualComponent<F>, "layout" | "geometry">, f: F): string[] {
  const g = component.geometry;
  const errors: string[] = [];
  const guard = (what: string, fn: () => void) => {
    try {
      fn();
    } catch (e) {
      errors.push(`${what}: threw ${String(e)}`);
    }
  };
  const W = f.info.width;
  const H = f.info.height;

  if (component.layout !== f.layout) errors.push(`frame layout ${f.layout}, component layout ${component.layout}`);
  const cols = GEOMETRY_COLUMNS[f.layout] ?? [];
  for (const name of cols) if (f.geometry[name]?.length !== f.count) errors.push(`geometry column ${name} missing or not ${f.count} rows long`);

  for (let i = 0; i < f.count; i++) guard(`row ${i}`, () => checkTarget(g, f, i, errors));
  if (f.info.root) guard("root", () => checkTarget(g, f, HIT_ROOT, errors));

  const outside: Pt[] = [
    { x: -1, y: -1 },
    { x: W, y: H },
    { x: W + 10, y: H / 2 },
    { x: -10, y: H / 2 },
    { x: W / 2, y: -10 },
    { x: W / 2, y: H + 10 },
  ];
  for (const p of outside)
    guard(`hitTest ${fmt(p)}`, () => {
      const h = g.hitTest(f, p.x, p.y);
      if (h !== HIT_NONE) errors.push(`hitTest ${fmt(p)} outside the canvas gives ${h}`);
    });

  guard("HIT_NONE", () => {
    if (g.bounds(f, HIT_NONE) !== null) errors.push("HIT_NONE has bounds");
    for (const slot of ["label", "tag", "badge"] as const) if (g.anchor(f, HIT_NONE, slot) !== null) errors.push(`HIT_NONE has a ${slot} anchor`);
    if (g.path(f, HIT_NONE, 0, new PathRecorder())) errors.push("HIT_NONE has an outline");
  });

  const empty = emptied(f);
  guard("empty frame", () => {
    const h = g.hitTest(empty, W / 2, H / 2);
    if (h !== HIT_NONE && h !== HIT_ROOT) errors.push(`hitTest on an empty frame gives row ${h}`);
    for (const hit of [HIT_NONE, HIT_ROOT]) {
      g.bounds(empty, hit);
      g.anchor(empty, hit, "label");
      g.box(empty, hit);
      g.path(empty, hit, 0, new PathRecorder());
    }
  });
  return errors;
}

/** A row's (or the root's) box, label anchor, bounds and outline agree with each other and with hitTest. */
function checkTarget<F extends VisualFrame>(g: VisualGeometry<F>, f: F, hit: Hit, errors: string[]): void {
  const who = hit === HIT_ROOT ? "root" : `row ${hit}`;
  const b = g.box(f, hit);
  if (!Number.isFinite(b.major) || !Number.isFinite(b.minor) || b.major < 0 || b.minor < 0) {
    errors.push(`${who}: box ${b.major} × ${b.minor}`);
    return;
  }
  if (b.minor < MIN_MINOR) return;
  const bounds = g.bounds(f, hit);
  if (!bounds) errors.push(`${who}: no bounds`);
  const a = g.anchor(f, hit, "label");
  if (!a) errors.push(`${who}: no label anchor (box ${b.major} × ${b.minor})`);
  else {
    const h = g.hitTest(f, a.x, a.y);
    // a nested layout draws every row inside the root: one of them may cover the root's anchor
    const own = hit === HIT_ROOT ? h === HIT_ROOT || (isNested(f) && h >= 0) : h >= 0 && isWithin(f, h, hit);
    if (!own) errors.push(`${who}: hitTest at its label anchor ${fmt(a)} gives ${h}`);
    if (bounds && !inside(bounds, a, 0)) errors.push(`${who}: label anchor ${fmt(a)} outside its bounds`);
  }
  for (const slot of ["tag", "badge"] as const) {
    const p = g.anchor(f, hit, slot);
    if (p && bounds && !inside(bounds, p, 0)) errors.push(`${who}: ${slot} anchor ${fmt(p)} outside its bounds`);
  }
  const rec = new PathRecorder();
  if (!g.path(f, hit, 0, rec)) errors.push(`${who}: no outline`);
  else if (!rec.ops.length) errors.push(`${who}: path() said true but drew nothing`);
  else if (bounds) {
    const off = rec.points().find((p) => !inside(bounds, p, 1));
    if (off) errors.push(`${who}: outline point ${fmt(off)} outside its bounds`);
  }
}

/** The same frame with no rows (columns of length 0), info and root kept. */
function emptied<F extends VisualFrame>(f: F): F {
  const out: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(f)) out[k] = ArrayBuffer.isView(v) ? new (v.constructor as new (n: number) => ArrayBufferView)(0) : v;
  out.geometry = Object.fromEntries(Object.keys(f.geometry).map((k) => [k, out[k] ?? new Int32Array(0)]));
  out.count = 0;
  out.names = [];
  out.rowOf = () => -1;
  out.info = { ...f.info, rows: 0, selected_row: null, flash: [] };
  return out as F;
}
