// WebGL2 instanced renderer for ring sectors: one draw call per picture.
//
// Each instance is a sector (t0, t1 in turns, r0, r1 in px) laid over a triangle strip of SEGMENTS
// steps, its outer edge pushed out so the chords cover the arc; the strip's steps come from a small
// shared buffer, so attribute 0 stays per vertex. The fragment shader evaluates the sector's signed
// distance (geometry.ts sectorDistance): fill, optional (dashed) border, stripes / dot patterns, or
// a soft shadow, as the rectangle program in treemap/gl.ts does. A polar transform of angles and
// radii is all the zoom animation needs.

import { tokens } from "@design/tokens";

import { SLOT, STRIDE } from "./paint";
import type { PolarXform } from "./geometry";

/** Steps of the strip over a sector's sweep. */
export const SEGMENTS = tokens.visual.flamePolar.segments;
/** Vertices of one strip: an inner and an outer one per step boundary. */
export const STRIP_VERTICES = 2 * (SEGMENTS + 1);

/** Instance attributes: name, location, float offset within an instance (paint.ts SLOT). */
export const ATTRIBUTES: readonly { name: string; location: number; offset: number }[] = [
  { name: "a_sector", location: 1, offset: SLOT.sector },
  { name: "a_fill", location: 2, offset: SLOT.fill },
  { name: "a_border", location: 3, offset: SLOT.border },
  { name: "a_pcolor", location: 4, offset: SLOT.pattern },
  { name: "a_shape", location: 5, offset: SLOT.shape },
  { name: "a_extra", location: 6, offset: SLOT.extra },
];

export const SECTOR_VS = `#version 300 es
precision highp float;
layout(location=0) in vec2 a_strip;   // step along the sweep (0..1), outer side (0 or 1)
layout(location=1) in vec4 a_sector;  // t0, t1 (turns from 12 o'clock, clockwise), r0, r1 (px)
layout(location=2) in vec4 a_fill;
layout(location=3) in vec4 a_border;
layout(location=4) in vec4 a_pcolor;
layout(location=5) in vec4 a_shape;   // borderWidth, pattern, patternSize, dashed
layout(location=6) in vec4 a_extra;   // alpha, gap, inset (negative: grown), -
uniform vec2 u_res;
uniform vec2 u_center;
uniform vec4 u_polar;                 // angles t' = t * x + y (turns), radii r' = r * z + w (px)
out vec2 v_p;
flat out vec4 v_sector;               // after the transform: t0, t1 (turns), r0, r1
flat out vec4 v_fill;
flat out vec4 v_border;
flat out vec4 v_pcolor;
flat out vec4 v_shape;
flat out vec4 v_extra;
const float TURN = 6.28318530718;
const float SEGMENTS = ${SEGMENTS.toFixed(1)};
void main() {
  // the transform first, clamped to one turn; what it squeezes out is not drawn
  vec2 t = clamp(a_sector.xy * u_polar.x + u_polar.y, 0.0, 1.0);
  vec2 r = a_sector.zw * u_polar.z + u_polar.w;
  r.x = max(r.x, 0.0);
  v_sector = vec4(t, r);
  v_fill = a_fill; v_border = a_border; v_pcolor = a_pcolor; v_shape = a_shape; v_extra = a_extra;
  if (t.y <= t.x || r.y <= r.x) {
    gl_Position = vec4(2.0, 2.0, 2.0, 1.0);
    v_p = vec2(0.0);
    return;
  }
  // cover what the fragment shader can light: the grown outline, a shadow's blur, 1 px of anti-aliasing
  float grow = max(0.0, -a_extra.z) + (a_shape.y == 4.0 ? a_shape.z : 0.0) + 1.0;
  float inner = max(0.0, r.x - grow);
  float a0 = t.x * TURN;
  float a1 = t.y * TURN;
  bool full = t.y - t.x >= 1.0 - 1e-6;
  if (!full) {
    // the radial edges' band, widest at the inner radius
    float spread = grow < inner ? asin(grow / inner) : TURN;
    a0 -= spread;
    a1 += spread;
    full = a1 - a0 >= TURN;
  }
  if (full) {
    a0 = 0.0;
    a1 = TURN;
  }
  float halfStep = (a1 - a0) / (2.0 * SEGMENTS);
  float th = mix(a0, a1, a_strip.x);
  float radius = a_strip.y > 0.5 ? (r.y + grow) / cos(halfStep) : inner;
  vec2 p = radius * vec2(sin(th), -cos(th));
  v_p = p;
  vec2 clip = (u_center + p) / u_res * 2.0 - 1.0;
  gl_Position = vec4(clip.x, -clip.y, 0.0, 1.0);
}`;

export const SECTOR_FS = `#version 300 es
precision highp float;
in vec2 v_p;
flat in vec4 v_sector;
flat in vec4 v_fill;
flat in vec4 v_border;
flat in vec4 v_pcolor;
flat in vec4 v_shape;
flat in vec4 v_extra;
uniform vec2 u_center;
uniform float u_alpha;
out vec4 o;
const float TURN = 6.28318530718;
const float PI = 3.14159265359;
// geometry.ts sectorDistance: radial distance, and the perpendicular distance from the nearer
// radial edge moved in by the seam inset (min(gap/2, max(0, (sweep * r0 - 1) / 2)))
float sectorDistance(vec2 p, vec4 s, float gap) {
  float r = length(p);
  float d = max(s.z - r, r - s.w);
  float span = (s.y - s.x) * TURN;
  if (s.y - s.x >= 1.0 - 1e-6) return d;
  float a0 = s.x * TURN;
  float a1 = s.y * TURN;
  float d0 = dot(p, vec2(cos(a0), sin(a0)));
  float d1 = -dot(p, vec2(cos(a1), sin(a1)));
  float edge = span <= PI ? -min(d0, d1) : -max(d0, d1);
  float seam = min(gap * 0.5, max(0.0, (span * s.z - 1.0) * 0.5));
  return max(d, edge + seam);
}
vec4 over(vec4 top, vec4 under) { return top + under * (1.0 - top.a); }
void main() {
  float d = sectorDistance(v_p, v_sector, v_extra.y) + v_extra.z;
  float pattern = v_shape.y;
  float alpha = v_extra.x * u_alpha;
  if (pattern == 4.0) {                                        // soft shadow
    float blur = max(v_shape.z, 1.0);
    float a = 1.0 - smoothstep(-blur * 0.5, blur, d);
    o = vec4(v_fill.rgb, 1.0) * v_fill.a * a * a * alpha;
    return;
  }
  float inside = clamp(0.5 - d, 0.0, 1.0);                     // 0.5 px anti-aliasing
  if (inside <= 0.0) discard;
  vec2 p = u_center + v_p;                                     // patterns follow the screen
  vec4 col = v_fill;
  float ps = v_shape.z;
  if (pattern == 1.0 || pattern == 2.0) {
    float t = pattern == 1.0 ? (p.x + p.y) : (p.x - p.y + 4096.0);
    float period = ps * 2.0 * 1.41421356;
    if (mod(t, period) < period * 0.5) col = v_pcolor;
  } else if (pattern == 3.0) {
    vec2 g = mod(p, ps);
    float dotSize = max(1.0, ps / 6.0);
    if (g.x < dotSize && g.y < dotSize) col = over(v_pcolor, col);
  }
  float bw = v_shape.x;
  if (bw > 0.0 && v_border.a > 0.0) {
    float ring = clamp(0.5 - (d + bw), 0.0, 1.0);              // 1 inside the border band's inner edge
    float band = inside - ring;
    if (v_shape.w > 0.5) {                                     // dashed: 4 on, 3 off (in dpr units)
      float unit = max(1.0, bw);
      if (mod(p.x + p.y, 7.0 * unit) > 4.0 * unit) band = 0.0;
    }
    col = mix(col, v_border, band / max(inside, 1e-3));
  }
  float a = col.a * inside * alpha;
  o = vec4(col.rgb * a, a);
}`;

export interface SectorPicture {
  vao: WebGLVertexArrayObject;
  buffer: WebGLBuffer;
  count: number;
  /** bytes the buffer holds; uploads within it reuse the storage */
  capacity: number;
}

export class SectorRenderer {
  private gl: WebGL2RenderingContext;
  private program: WebGLProgram;
  private strip: WebGLBuffer;
  private uRes: WebGLUniformLocation;
  private uCenter: WebGLUniformLocation;
  private uPolar: WebGLUniformLocation;
  private uAlpha: WebGLUniformLocation;

  constructor(gl: WebGL2RenderingContext) {
    this.gl = gl;
    this.program = link(gl, SECTOR_VS, SECTOR_FS);
    // the strip's vertices, shared by every instance: (step / SEGMENTS, inner 0 / outer 1)
    const strip = new Float32Array(STRIP_VERTICES * 2);
    for (let v = 0; v < STRIP_VERTICES; v++) {
      strip[2 * v] = (v >> 1) / SEGMENTS;
      strip[2 * v + 1] = v & 1;
    }
    this.strip = gl.createBuffer()!;
    gl.bindBuffer(gl.ARRAY_BUFFER, this.strip);
    gl.bufferData(gl.ARRAY_BUFFER, strip, gl.STATIC_DRAW);
    this.uRes = gl.getUniformLocation(this.program, "u_res")!;
    this.uCenter = gl.getUniformLocation(this.program, "u_center")!;
    this.uPolar = gl.getUniformLocation(this.program, "u_polar")!;
    this.uAlpha = gl.getUniformLocation(this.program, "u_alpha")!;
  }

  /** Instances into a picture; `previous` keeps its buffer, which only grows. */
  upload(data: Float32Array, count: number, previous?: SectorPicture | null): SectorPicture {
    const gl = this.gl;
    const bytes = count * STRIDE * 4;
    let p = previous;
    if (!p) {
      const vao = gl.createVertexArray()!;
      const buffer = gl.createBuffer()!;
      gl.bindVertexArray(vao);
      gl.bindBuffer(gl.ARRAY_BUFFER, this.strip);
      gl.enableVertexAttribArray(0);
      gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 0, 0);
      gl.bindBuffer(gl.ARRAY_BUFFER, buffer);
      for (const a of ATTRIBUTES) {
        gl.enableVertexAttribArray(a.location);
        gl.vertexAttribPointer(a.location, 4, gl.FLOAT, false, STRIDE * 4, a.offset * 4);
        gl.vertexAttribDivisor(a.location, 1);
      }
      gl.bindVertexArray(null);
      p = { vao, buffer, count: 0, capacity: 0 };
    }
    gl.bindBuffer(gl.ARRAY_BUFFER, p.buffer);
    if (bytes > p.capacity) {
      const capacity = Math.max(bytes, 2 * p.capacity, 64 * STRIDE * 4);
      gl.bufferData(gl.ARRAY_BUFFER, capacity, gl.DYNAMIC_DRAW);
      p.capacity = capacity;
    }
    if (count) gl.bufferSubData(gl.ARRAY_BUFFER, 0, data, 0, count * STRIDE);
    p.count = count;
    return p;
  }

  /** Make this program current for a width × height canvas around a centre, without clearing or
   * touching the blend state: another program may already have drawn into this frame. */
  use(width: number, height: number, cx: number, cy: number): void {
    const gl = this.gl;
    gl.viewport(0, 0, width, height);
    gl.useProgram(this.program);
    gl.uniform2f(this.uRes, width, height);
    gl.uniform2f(this.uCenter, cx, cy);
  }

  draw(p: SectorPicture, xf: PolarXform, alpha = 1): void {
    if (!p.count || alpha <= 0) return;
    const gl = this.gl;
    gl.uniform4f(this.uPolar, xf[0], xf[1], xf[2], xf[3]);
    gl.uniform1f(this.uAlpha, alpha);
    gl.bindVertexArray(p.vao);
    gl.drawArraysInstanced(gl.TRIANGLE_STRIP, 0, STRIP_VERTICES, p.count);
    gl.bindVertexArray(null);
  }

  dispose(p: SectorPicture | null): void {
    if (!p) return;
    this.gl.deleteVertexArray(p.vao);
    this.gl.deleteBuffer(p.buffer);
  }
}

function link(gl: WebGL2RenderingContext, vs: string, fs: string): WebGLProgram {
  const compile = (type: number, src: string) => {
    const s = gl.createShader(type)!;
    gl.shaderSource(s, src);
    gl.compileShader(s);
    if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(s) ?? "shader error");
    return s;
  };
  const p = gl.createProgram()!;
  gl.attachShader(p, compile(gl.VERTEX_SHADER, vs));
  gl.attachShader(p, compile(gl.FRAGMENT_SHADER, fs));
  gl.linkProgram(p);
  if (!gl.getProgramParameter(p, gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(p) ?? "link error");
  return p;
}
