// The one WebGL2 context of a view host: made once per canvas, lent to every component instance
// drawing into it (programs and renderers cached by key), and given back when the host goes away,
// so split panes and tab switches do not pile up contexts until Chromium drops the oldest.

// no depth or stencil buffer: nothing tests depth or stencil (they cost 4–5 B a pixel, ~40 MB in a
// maximized 4K window)
export const ATTRIBUTES: WebGLContextAttributes = { premultipliedAlpha: true, antialias: false, alpha: false, depth: false, stencil: false };

export class GlContext {
  readonly gl: WebGL2RenderingContext;
  private readonly objects = new Map<string, unknown>();
  private releasing: ReturnType<typeof setTimeout> | null = null;
  released = false;

  constructor(readonly canvas: HTMLCanvasElement) {
    // the components' renderers ask the canvas again: getContext returns this same context
    const gl = canvas.getContext("webgl2", ATTRIBUTES);
    if (!gl) throw new Error("WebGL2 is not available");
    this.gl = gl;
  }

  /** One object per key in this context: made on first use, made again after a lost context. */
  shared<T>(key: string, make: () => T): T {
    if (!this.objects.has(key)) this.objects.set(key, make());
    return this.objects.get(key) as T;
  }

  get lost(): boolean {
    return this.gl.isContextLost();
  }

  /** Start a display frame: clear to the ground colour (the clear ignores the viewport) and blend
   * premultiplied colours; each component then makes its own program current. */
  begin(width: number, height: number, ground: readonly [number, number, number]): void {
    const gl = this.gl;
    gl.viewport(0, 0, width, height);
    gl.clearColor(ground[0], ground[1], ground[2], 1);
    gl.clear(gl.COLOR_BUFFER_BIT);
    gl.enable(gl.BLEND);
    gl.blendFunc(gl.ONE, gl.ONE_MINUS_SRC_ALPHA);
  }

  /** The context came back after a loss: what was made in the old one is gone. */
  restored(): void {
    this.objects.clear();
  }

  /** Give the context back once the current task is over, unless the canvas takes it again first
   * (React StrictMode mounts, unmounts and mounts again at once). */
  release(): void {
    if (this.releasing) return;
    this.releasing = setTimeout(() => {
      this.releasing = null;
      this.released = true;
      live.delete(this.canvas);
      this.objects.clear();
      this.gl.getExtension("WEBGL_lose_context")?.loseContext();
    }, 0);
  }

  /** Keep the context after all (a release is pending). */
  keep(): void {
    if (this.releasing) clearTimeout(this.releasing);
    this.releasing = null;
  }
}

const live = new WeakMap<HTMLCanvasElement, GlContext>();

/** The canvas's context: the one it already has (a release pending is called off), or a new one. */
export function acquireGl(canvas: HTMLCanvasElement): GlContext {
  const had = live.get(canvas);
  if (had && !had.released) {
    had.keep();
    return had;
  }
  const made = new GlContext(canvas);
  live.set(canvas, made);
  return made;
}
