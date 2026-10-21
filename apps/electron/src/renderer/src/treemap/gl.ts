// WebGL2 instanced renderer for treemap shapes: one draw call per picture.
//
// Each instance is a rounded rectangle evaluated as a signed distance field in its own pixel
// space: fill, optional (dashed) border, stripes / dot patterns, or a soft shadow. A picture can
// be drawn under an affine transform and an opacity, which is all the zoom animation needs.

import { ATTRIBUTES } from "@/visual/glContext";

import { STRIDE } from "./paint";

const VS = `#version 300 es
precision highp float;
layout(location=0) in vec2 a_corner;
layout(location=1) in vec4 a_rect;
layout(location=2) in vec4 a_fill;
layout(location=3) in vec4 a_border;
layout(location=4) in vec4 a_pcolor;
layout(location=5) in vec4 a_shape;   // radius, topOnly, borderWidth, pattern
layout(location=6) in vec4 a_extra;   // patternSize, dashed, alpha, -
uniform vec2 u_res;
uniform vec4 u_xform;                 // scale x, scale y, translate x, translate y
out vec2 v_local;
out vec2 v_half;
flat out vec4 v_fill;
flat out vec4 v_border;
flat out vec4 v_pcolor;
flat out vec4 v_shape;
flat out vec4 v_extra;
void main() {
  float grow = a_shape.w == 4.0 ? a_extra.x : 1.0;   // shadows reach beyond their rectangle
  vec2 size = a_rect.zw + 2.0 * grow;
  vec2 local = a_corner * size - grow;
  vec2 px = a_rect.xy + local;
  px = px * u_xform.xy + u_xform.zw;
  v_local = local - a_rect.zw * 0.5;
  v_half = a_rect.zw * 0.5;
  v_fill = a_fill; v_border = a_border; v_pcolor = a_pcolor; v_shape = a_shape; v_extra = a_extra;
  vec2 clip = px / u_res * 2.0 - 1.0;
  gl_Position = vec4(clip.x, -clip.y, 0.0, 1.0);
}`;

const FS = `#version 300 es
precision highp float;
in vec2 v_local;
in vec2 v_half;
flat in vec4 v_fill;
flat in vec4 v_border;
flat in vec4 v_pcolor;
flat in vec4 v_shape;
flat in vec4 v_extra;
uniform float u_alpha;
uniform float u_scale;
out vec4 o;
float sdRound(vec2 p, vec2 h, float r) {
  vec2 q = abs(p) - h + r;
  return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}
vec4 over(vec4 top, vec4 under) { return top + under * (1.0 - top.a); }
void main() {
  float radius = v_shape.x;
  if (v_shape.y > 0.5 && v_local.y > 0.0) radius = 0.0;     // rounded top corners only
  radius = min(radius, min(v_half.x, v_half.y));
  float d = sdRound(v_local, v_half, radius);
  float aa = 0.5 / max(u_scale, 1e-3);
  float pattern = v_shape.w;
  float alpha = v_extra.z * u_alpha;
  if (pattern == 4.0) {                                        // soft shadow
    float blur = max(v_extra.x, 1.0);
    float a = 1.0 - smoothstep(-blur * 0.5, blur, d);
    o = vec4(v_fill.rgb, 1.0) * v_fill.a * a * a * alpha;
    return;
  }
  float inside = clamp(0.5 - d / (2.0 * aa), 0.0, 1.0);
  if (inside <= 0.0) discard;
  vec2 p = v_local + v_half;                                   // 0..w, 0..h
  vec4 col = v_fill;
  float ps = v_extra.x;
  if (pattern == 1.0 || pattern == 2.0) {
    float t = pattern == 1.0 ? (p.x + p.y) : (p.x - p.y + 4096.0);
    float period = ps * 2.0 * 1.41421356;
    if (mod(t, period) < period * 0.5) col = v_pcolor;
  } else if (pattern == 3.0) {
    vec2 g = mod(p, ps);
    float dot = max(1.0, ps / 6.0);
    if (g.x < dot && g.y < dot) col = over(v_pcolor, col);
  }
  float bw = v_shape.z;
  if (bw > 0.0 && v_border.a > 0.0) {
    float ring = clamp(0.5 - (d + bw) / (2.0 * aa), 0.0, 1.0); // 1 inside the border band's inner edge
    float band = inside - ring;
    if (v_extra.y > 0.5) {                                     // dashed: 4 on, 3 off (in dpr units)
      float unit = max(1.0, bw);
      if (mod(p.x + p.y, 7.0 * unit) > 4.0 * unit) band = 0.0;
    }
    col = mix(col, v_border, band / max(inside, 1e-3));
  }
  float a = col.a * inside * alpha;
  o = vec4(col.rgb * a, a);
}`;

export interface Picture {
  vao: WebGLVertexArrayObject;
  buffer: WebGLBuffer;
  count: number;
  capacity: number; // bytes the buffer holds: it only grows, and is updated in place
}

export class GlRenderer {
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

  /** Instances into a picture. `previous` keeps its vertex array and its buffer, which only grows
   * and is updated in place: a hover repaint allocates nothing on the GPU side (like the polar
   * flame graph's sectors.ts). */
  upload(data: Float32Array, count: number, previous?: Picture | null): Picture {
    const gl = this.gl;
    const need = count * STRIDE * 4;
    if (previous) {
      gl.bindBuffer(gl.ARRAY_BUFFER, previous.buffer);
      if (need > previous.capacity) {
        previous.capacity = Math.max(need, 2 * previous.capacity);
        gl.bufferData(gl.ARRAY_BUFFER, previous.capacity, gl.DYNAMIC_DRAW);
      }
      if (count) gl.bufferSubData(gl.ARRAY_BUFFER, 0, data, 0, count * STRIDE);
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
    const capacity = Math.max(need, 64 * STRIDE * 4);
    gl.bufferData(gl.ARRAY_BUFFER, capacity, gl.DYNAMIC_DRAW);
    if (count) gl.bufferSubData(gl.ARRAY_BUFFER, 0, data, 0, count * STRIDE);
    const bytes = STRIDE * 4;
    const attrs: [number, number, number][] = [
      [1, 4, 0],
      [2, 4, 4],
      [3, 4, 8],
      [4, 4, 12],
      [5, 4, 16],
      [6, 4, 20],
    ];
    for (const [loc, size, off] of attrs) {
      gl.enableVertexAttribArray(loc);
      gl.vertexAttribPointer(loc, size, gl.FLOAT, false, bytes, off * 4);
      gl.vertexAttribDivisor(loc, 1);
    }
    gl.bindVertexArray(null);
    return { vao, buffer, count, capacity };
  }

  /** Clear to the ground colour (the clear ignores the viewport), set premultiplied blending, then use(). */
  begin(width: number, height: number, ground: [number, number, number]): void {
    const gl = this.gl;
    gl.clearColor(ground[0], ground[1], ground[2], 1);
    gl.clear(gl.COLOR_BUFFER_BIT);
    gl.enable(gl.BLEND);
    gl.blendFunc(gl.ONE, gl.ONE_MINUS_SRC_ALPHA);
    this.use(width, height);
  }

  /** Make this program current for a width × height canvas without clearing or touching the blend
   * state: another program may already have drawn into this context in the same frame. */
  use(width: number, height: number): void {
    const gl = this.gl;
    gl.viewport(0, 0, width, height);
    gl.useProgram(this.program);
    gl.uniform2f(this.uRes, width, height);
  }

  draw(p: Picture, sx = 1, sy = 1, tx = 0, ty = 0, alpha = 1): void {
    if (!p.count || alpha <= 0) return;
    const gl = this.gl;
    gl.uniform4f(this.uXform, sx, sy, tx, ty);
    gl.uniform1f(this.uAlpha, alpha);
    gl.uniform1f(this.uScale, Math.min(sx, sy));
    gl.bindVertexArray(p.vao);
    gl.drawArraysInstanced(gl.TRIANGLE_STRIP, 0, 4, p.count);
    gl.bindVertexArray(null);
  }

  dispose(p: Picture | null): void {
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
