// WebGL2 instanced renderer for shapes.v1: one draw call per picture. Each instance is a rounded
// rectangle (a circle when the radius is half its sides) or a convex polygon of up to 12 points,
// evaluated as a signed distance field around its box's centre, with the treemap program's looks
// (treemap/gl.ts): fill, optional (dashed) border, stripes / dot patterns, or a soft shadow. An
// instance's `offset` grows the shape (selection rings, shadows) without touching its geometry. A
// picture can be drawn under an affine transform and an opacity: all the zoom animation needs.

import { ATTRIBUTES } from "@/visual/glContext";

/** Floats per instance (see SLOT). */
export const SHAPE_STRIDE = 48;
/** Points of a polygon at most (spaceknife_protocol.view.MAX_POLY_POINTS). */
export const MAX_POINTS = 12;
/** Float offsets within an instance. */
export const SLOT = { rect: 0, fill: 4, border: 8, pattern: 12, shape: 16, extra: 20, points: 24 } as const;

const VS = `#version 300 es
precision highp float;
layout(location=0) in vec2 a_corner;
layout(location=1) in vec4 a_rect;    // x, y, w, h (px)
layout(location=2) in vec4 a_fill;
layout(location=3) in vec4 a_border;
layout(location=4) in vec4 a_pcolor;
layout(location=5) in vec4 a_shape;   // radius, borderWidth, pattern, patternSize
layout(location=6) in vec4 a_extra;   // dashed, alpha, offset (grows the shape), points (0: rounded rectangle)
layout(location=7) in vec4 a_p0;      // polygon points around the box's centre, two per attribute
layout(location=8) in vec4 a_p1;
layout(location=9) in vec4 a_p2;
layout(location=10) in vec4 a_p3;
layout(location=11) in vec4 a_p4;
layout(location=12) in vec4 a_p5;
uniform vec2 u_res;
uniform vec4 u_xform;                 // scale x, scale y, translate x, translate y
out vec2 v_local;
flat out vec2 v_half;
flat out vec4 v_fill;
flat out vec4 v_border;
flat out vec4 v_pcolor;
flat out vec4 v_shape;
flat out vec4 v_extra;
flat out vec4 v_p0;
flat out vec4 v_p1;
flat out vec4 v_p2;
flat out vec4 v_p3;
flat out vec4 v_p4;
flat out vec4 v_p5;
void main() {
  // cover what the fragment shader can light: the grown shape, a shadow's blur, 1 px of anti-aliasing
  float grow = max(0.0, a_extra.z) + (a_shape.z == 4.0 ? a_shape.w : 0.0) + 1.0;
  vec2 size = a_rect.zw + 2.0 * grow;
  vec2 local = a_corner * size - grow;
  vec2 px = (a_rect.xy + local) * u_xform.xy + u_xform.zw;
  v_local = local - a_rect.zw * 0.5;
  v_half = a_rect.zw * 0.5;
  v_fill = a_fill; v_border = a_border; v_pcolor = a_pcolor; v_shape = a_shape; v_extra = a_extra;
  v_p0 = a_p0; v_p1 = a_p1; v_p2 = a_p2; v_p3 = a_p3; v_p4 = a_p4; v_p5 = a_p5;
  vec2 clip = px / u_res * 2.0 - 1.0;
  gl_Position = vec4(clip.x, -clip.y, 0.0, 1.0);
}`;

const FS = `#version 300 es
precision highp float;
in vec2 v_local;
flat in vec2 v_half;
flat in vec4 v_fill;
flat in vec4 v_border;
flat in vec4 v_pcolor;
flat in vec4 v_shape;
flat in vec4 v_extra;
flat in vec4 v_p0;
flat in vec4 v_p1;
flat in vec4 v_p2;
flat in vec4 v_p3;
flat in vec4 v_p4;
flat in vec4 v_p5;
uniform float u_alpha;
uniform float u_scale;
out vec4 o;
float sdRound(vec2 p, vec2 h, float r) {
  vec2 q = abs(p) - h + r;
  return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}
// a convex polygon: the largest distance past one of its edges (exact inside and along the edges)
float sdPoly(vec2 p, int n) {
  vec2 v[12] = vec2[12](v_p0.xy, v_p0.zw, v_p1.xy, v_p1.zw, v_p2.xy, v_p2.zw,
                        v_p3.xy, v_p3.zw, v_p4.xy, v_p4.zw, v_p5.xy, v_p5.zw);
  float area = 0.0;
  for (int i = 0; i < 12; i++) {
    if (i >= n) break;
    vec2 a = v[i];
    vec2 b = v[(i + 1) % n];
    area += a.x * b.y - b.x * a.y;
  }
  float s = area >= 0.0 ? 1.0 : -1.0;
  float d = -1e9;
  for (int i = 0; i < 12; i++) {
    if (i >= n) break;
    vec2 a = v[i];
    vec2 e = v[(i + 1) % n] - a;
    float len = length(e);
    if (len < 1e-4) continue;
    vec2 nrm = s * vec2(e.y, -e.x) / len;   // outward
    d = max(d, dot(p - a, nrm));
  }
  return d;
}
vec4 over(vec4 top, vec4 under) { return top + under * (1.0 - top.a); }
void main() {
  int n = int(v_extra.w + 0.5);
  float d = n >= 3 ? sdPoly(v_local, n) : sdRound(v_local, v_half, min(v_shape.x, min(v_half.x, v_half.y)));
  d -= v_extra.z;                                            // grown (selection rings, shadows)
  float aa = 0.5 / max(u_scale, 1e-3);
  float pattern = v_shape.z;
  float alpha = v_extra.y * u_alpha;
  if (pattern == 4.0) {                                      // soft shadow
    float blur = max(v_shape.w, 1.0);
    float a = 1.0 - smoothstep(-blur * 0.5, blur, d);
    a *= a;
    o = vec4(v_fill.rgb, 1.0) * v_fill.a * a * alpha;
    return;
  }
  float inside = clamp(0.5 - d / (2.0 * aa), 0.0, 1.0);
  if (inside <= 0.0) discard;
  vec2 p = v_local + v_half;                                 // 0..w, 0..h
  vec4 col = v_fill;
  float ps = v_shape.w;
  if (pattern == 1.0 || pattern == 2.0) {
    float t = pattern == 1.0 ? (p.x + p.y) : (p.x - p.y + 4096.0);
    float period = ps * 2.0 * 1.41421356;
    if (mod(t, period) < period * 0.5) col = v_pcolor;
  } else if (pattern == 3.0) {
    vec2 g = mod(p, ps);
    float dot = max(1.0, ps / 6.0);
    if (g.x < dot && g.y < dot) col = over(v_pcolor, col);
  }
  float bw = v_shape.y;
  if (bw > 0.0 && v_border.a > 0.0) {
    float ring = clamp(0.5 - (d + bw) / (2.0 * aa), 0.0, 1.0);
    float band = inside - ring;
    if (v_extra.x > 0.5) {                                   // dashed: 4 on, 3 off (in dpr units)
      float unit = max(1.0, bw);
      if (mod(p.x + p.y, 7.0 * unit) > 4.0 * unit) band = 0.0;
    }
    col = mix(col, v_border, band / max(inside, 1e-3));
  }
  float a = col.a * inside * alpha;
  o = vec4(col.rgb * a, a);
}`;

export interface ShapePicture {
  vao: WebGLVertexArrayObject;
  buffer: WebGLBuffer;
  count: number;
  capacity: number; // bytes the buffer holds: it only grows, and is updated in place
}

export class ShapeRenderer {
  private gl: WebGL2RenderingContext;
  private program: WebGLProgram;
  private quad: WebGLBuffer;
  private uRes: WebGLUniformLocation;
  private uXform: WebGLUniformLocation;
  private uAlpha: WebGLUniformLocation;
  private uScale: WebGLUniformLocation;

  constructor(canvas: HTMLCanvasElement) {
    const gl = canvas.getContext("webgl2", ATTRIBUTES); // the host's context (visual/glContext.ts)
    if (!gl) throw new Error("WebGL2 is not available");
    this.gl = gl;
    this.program = link(gl, VS, FS);
    this.quad = gl.createBuffer()!;
    gl.bindBuffer(gl.ARRAY_BUFFER, this.quad);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([0, 0, 1, 0, 0, 1, 1, 1]), gl.STATIC_DRAW);
    this.uRes = gl.getUniformLocation(this.program, "u_res")!;
    this.uXform = gl.getUniformLocation(this.program, "u_xform")!;
    this.uAlpha = gl.getUniformLocation(this.program, "u_alpha")!;
    this.uScale = gl.getUniformLocation(this.program, "u_scale")!;
  }

  /** Instances into a picture; `previous` keeps its vertex array and its buffer (which only grows). */
  upload(data: Float32Array, count: number, previous?: ShapePicture | null): ShapePicture {
    const gl = this.gl;
    const need = count * SHAPE_STRIDE * 4;
    if (previous) {
      gl.bindBuffer(gl.ARRAY_BUFFER, previous.buffer);
      if (need > previous.capacity) {
        previous.capacity = Math.max(need, 2 * previous.capacity);
        gl.bufferData(gl.ARRAY_BUFFER, previous.capacity, gl.DYNAMIC_DRAW);
      }
      if (count) gl.bufferSubData(gl.ARRAY_BUFFER, 0, data, 0, count * SHAPE_STRIDE);
      previous.count = count;
      return previous;
    }
    const vao = gl.createVertexArray()!;
    const buffer = gl.createBuffer()!;
    gl.bindVertexArray(vao);
    gl.bindBuffer(gl.ARRAY_BUFFER, this.quad);
    gl.enableVertexAttribArray(0);
    gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 0, 0);
    gl.bindBuffer(gl.ARRAY_BUFFER, buffer);
    const capacity = Math.max(need, 64 * SHAPE_STRIDE * 4);
    gl.bufferData(gl.ARRAY_BUFFER, capacity, gl.DYNAMIC_DRAW);
    if (count) gl.bufferSubData(gl.ARRAY_BUFFER, 0, data, 0, count * SHAPE_STRIDE);
    const bytes = SHAPE_STRIDE * 4;
    for (let k = 0; k < 12; k++) {
      const loc = k + 1;
      gl.enableVertexAttribArray(loc);
      gl.vertexAttribPointer(loc, 4, gl.FLOAT, false, bytes, 4 * k * 4);
      gl.vertexAttribDivisor(loc, 1);
    }
    gl.bindVertexArray(null);
    return { vao, buffer, count, capacity };
  }

  /** Make this program current for a width × height canvas (the host cleared it and set the blend). */
  use(width: number, height: number): void {
    const gl = this.gl;
    gl.viewport(0, 0, width, height);
    gl.useProgram(this.program);
    gl.uniform2f(this.uRes, width, height);
  }

  draw(p: ShapePicture, sx = 1, sy = 1, tx = 0, ty = 0, alpha = 1): void {
    if (!p.count || alpha <= 0) return;
    const gl = this.gl;
    gl.uniform4f(this.uXform, sx, sy, tx, ty);
    gl.uniform1f(this.uAlpha, alpha);
    gl.uniform1f(this.uScale, Math.min(sx, sy));
    gl.bindVertexArray(p.vao);
    gl.drawArraysInstanced(gl.TRIANGLE_STRIP, 0, 4, p.count);
    gl.bindVertexArray(null);
  }

  dispose(p: ShapePicture | null): void {
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

/** The shaders, for a test that compiles them where WebGL2 runs. */
export const SHADERS = { VS, FS } as const;
