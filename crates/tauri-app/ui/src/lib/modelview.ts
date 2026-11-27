// The 3D Model Properties preview: the model drawn with WebGL 2, lit by a key
// light, a fill and some ambient, each part with its colour, highlights, own
// light, see-through and texture (as the renderer draws them, near enough).
// Drag to turn it round (or move it, with the move tool); the wheel zooms.

export type PreviewPart = { name: string; color: [number, number, number]; textured: boolean };
export type PreviewHead = {
  arrays: Record<string, [number, number]>;
  parts: PreviewPart[];
  triangles: number;
  z_up: boolean;
  missing_textures: string[];
};
export type Preview = {
  head: PreviewHead;
  positions: Float32Array;
  normals: Float32Array;
  colors: Float32Array;
  uvs: Float32Array;
  rects: Float32Array;
  partsOf: Uint16Array;
  indices: Uint32Array;
  atlas: Blob | null;
};

/// The binary message `model_preview` sends (see modelprops.rs).
export function unpackPreview(buf: ArrayBuffer): Preview {
  const dv = new DataView(buf);
  const len = dv.getUint32(0, true);
  const head = JSON.parse(new TextDecoder().decode(new Uint8Array(buf, 4, len))) as PreviewHead;
  const start = (4 + len + 3) & ~3;
  const arr = (name: string) => head.arrays[name] ?? [0, 0];
  const f32 = (name: string) => {
    const [at, n] = arr(name);
    return new Float32Array(buf, start + at, n / 4);
  };
  const [pa, pn] = arr("parts_of");
  const [ia, inn] = arr("indices");
  const atlas = head.arrays.atlas ? new Blob([new Uint8Array(buf, start + head.arrays.atlas[0], head.arrays.atlas[1])], { type: "image/png" }) : null;
  return {
    head,
    positions: f32("positions"),
    normals: f32("normals"),
    colors: f32("colors"),
    uvs: f32("uvs"),
    rects: f32("rects"),
    partsOf: new Uint16Array(buf.slice(start + pa, start + pa + pn)),
    indices: new Uint32Array(buf, start + ia, inn / 4),
    atlas,
  };
}

/// How each part looks now (linear colours 0–1).
export type PartShade = {
  visible: boolean;
  /// Its own colour instead of the file's (and no texture), or null.
  color: [number, number, number] | null;
  specular: [number, number, number];
  shininess: number;
  emissive: [number, number, number];
  opacity: number;
  /// It has a picture of its own (shown instead of its colour).
  map: boolean;
};

const VS = `#version 300 es
in vec3 a_pos; in vec3 a_normal; in vec4 a_color; in vec2 a_uv; in vec4 a_rect; in float a_part;
uniform mat4 u_mvp; uniform mat4 u_mv; uniform mat3 u_nm;
out vec3 v_n; out vec4 v_color; out vec2 v_uv; out vec4 v_rect; flat out int v_part; out vec3 v_world;
void main() {
  gl_Position = u_mvp * vec4(a_pos, 1.0);
  v_n = u_nm * a_normal; v_color = a_color; v_uv = a_uv; v_rect = a_rect; v_part = int(a_part + 0.5); v_world = (u_mv * vec4(a_pos, 1.0)).xyz;
}`;

const FS = `#version 300 es
precision highp float; precision highp sampler2D;
in vec3 v_n; in vec4 v_color; in vec2 v_uv; in vec4 v_rect; flat in int v_part; in vec3 v_world;
uniform sampler2D u_atlas; uniform sampler2D u_parts; uniform bool u_own; uniform vec3 u_flat;
out vec4 o;
vec4 part(int row) { return texelFetch(u_parts, ivec2(v_part % 1024, (v_part / 1024) * 4 + row), 0); }
vec3 toSrgb(vec3 c) { return mix(c * 12.92, 1.055 * pow(c, vec3(1.0 / 2.4)) - 0.055, step(0.0031308, c)); }
void main() {
  vec4 look = part(3);
  if (look.x < 0.5) discard;
  vec3 n = normalize(v_n);
  vec3 v = normalize(-v_world);
  if (dot(n, v) < 0.0) n = -n;
  vec4 c = v_color;
  vec4 over = part(0);
  vec4 spec = part(1);
  vec4 glow = part(2);
  if (!u_own) { c = vec4(u_flat, 1.0); spec = vec4(0.0, 0.0, 0.0, 30.0); glow = vec4(0.0, 0.0, 0.0, 1.0); }
  else if (over.w > 0.5 && look.y < 0.5) c = vec4(over.rgb, c.a);
  if (u_own && v_rect.z > 0.0 && (over.w < 0.5 || look.y > 0.5)) {
    vec2 size = vec2(textureSize(u_atlas, 0));
    vec2 half_ = 0.5 / (v_rect.zw * size);
    vec2 at = v_rect.xy + clamp(fract(v_uv), half_, vec2(1.0) - half_) * v_rect.zw;
    vec4 t = texture(u_atlas, at);
    c = (look.y > 0.5 ? vec4(1.0, 1.0, 1.0, c.a) : c) * vec4(pow(t.rgb, vec3(2.2)), t.a);
  }
  vec3 key = normalize(vec3(-0.5, 0.8, 0.9));
  vec3 fill = normalize(vec3(0.7, 0.1, 0.5));
  vec3 light = vec3(0.18) + vec3(0.85) * max(dot(n, key), 0.0) + vec3(0.3) * max(dot(n, fill), 0.0);
  vec3 rgb = c.rgb * light;
  if (max(spec.r, max(spec.g, spec.b)) > 0.0) {
    rgb += spec.rgb * (pow(max(dot(n, normalize(key + v)), 0.0), spec.w) + 0.3 * pow(max(dot(n, normalize(fill + v)), 0.0), spec.w));
  }
  rgb += glow.rgb;
  float a = c.a * glow.w;
  o = vec4(toSrgb(clamp(rgb, 0.0, 1.0)) * a, a);
}`;

export class ModelView {
  private gl: WebGL2RenderingContext;
  private prog: WebGLProgram;
  private vao: WebGLVertexArrayObject | null = null;
  private count = 0;
  private atlas: WebGLTexture;
  private partsTex: WebGLTexture;
  private centre: [number, number, number] = [0, 0, 0];
  private scale = 1;
  private data: Preview | null = null;
  zUp = false;
  own = true;
  flat: [number, number, number] = [0.58, 0.58, 0.58];
  yaw = -30;
  pitch = 18;
  distance = 2.6;
  pan: [number, number] = [0, 0];
  private frame = 0;
  readonly ok: boolean;

  constructor(private canvas: HTMLCanvasElement) {
    const gl = canvas.getContext("webgl2", { antialias: true, premultipliedAlpha: true, alpha: true });
    this.ok = !!gl;
    this.gl = gl as WebGL2RenderingContext;
    if (!gl) {
      this.prog = null as unknown as WebGLProgram;
      this.atlas = this.partsTex = null as unknown as WebGLTexture;
      return;
    }
    const sh = (type: number, src: string) => {
      const s = gl.createShader(type)!;
      gl.shaderSource(s, src);
      gl.compileShader(s);
      if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) console.warn(gl.getShaderInfoLog(s));
      return s;
    };
    const p = gl.createProgram()!;
    gl.attachShader(p, sh(gl.VERTEX_SHADER, VS));
    gl.attachShader(p, sh(gl.FRAGMENT_SHADER, FS));
    gl.linkProgram(p);
    if (!gl.getProgramParameter(p, gl.LINK_STATUS)) console.warn(gl.getProgramInfoLog(p));
    this.prog = p;
    this.atlas = this.texture(1, 1, new Uint8Array([255, 255, 255, 255]));
    this.partsTex = gl.createTexture()!;
  }

  private texture(w: number, h: number, px: Uint8Array) {
    const gl = this.gl;
    const t = gl.createTexture()!;
    gl.bindTexture(gl.TEXTURE_2D, t);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, w, h, 0, gl.RGBA, gl.UNSIGNED_BYTE, px);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    return t;
  }

  async setData(d: Preview) {
    if (!this.ok) return;
    const gl = this.gl;
    this.data = d;
    if (this.vao) gl.deleteVertexArray(this.vao);
    const vao = gl.createVertexArray()!;
    gl.bindVertexArray(vao);
    const attr = (name: string, data: Float32Array, size: number) => {
      const loc = gl.getAttribLocation(this.prog, name);
      if (loc < 0) return;
      const b = gl.createBuffer();
      gl.bindBuffer(gl.ARRAY_BUFFER, b);
      gl.bufferData(gl.ARRAY_BUFFER, data, gl.STATIC_DRAW);
      gl.enableVertexAttribArray(loc);
      gl.vertexAttribPointer(loc, size, gl.FLOAT, false, 0, 0);
    };
    attr("a_pos", d.positions, 3);
    attr("a_normal", d.normals, 3);
    attr("a_color", d.colors, 4);
    attr("a_uv", d.uvs, 2);
    attr("a_rect", d.rects, 4);
    attr("a_part", Float32Array.from(d.partsOf), 1);
    const ib = gl.createBuffer();
    gl.bindBuffer(gl.ELEMENT_ARRAY_BUFFER, ib);
    gl.bufferData(gl.ELEMENT_ARRAY_BUFFER, d.indices, gl.STATIC_DRAW);
    gl.bindVertexArray(null);
    this.vao = vao;
    this.count = d.indices.length;
    this.fit();
    if (d.atlas) {
      try {
        const bmp = await createImageBitmap(d.atlas);
        gl.deleteTexture(this.atlas);
        this.atlas = gl.createTexture()!;
        gl.bindTexture(gl.TEXTURE_2D, this.atlas);
        gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, bmp);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
      } catch {
        /* drawn without its textures */
      }
    }
    this.draw();
  }

  /// Centred and sized to fit, in the up axis chosen.
  fit() {
    const d = this.data;
    if (!d) return;
    const lo = [Infinity, Infinity, Infinity];
    const hi = [-Infinity, -Infinity, -Infinity];
    const p = d.positions;
    for (let i = 0; i < p.length; i += 3) {
      const q = this.axes(p[i], p[i + 1], p[i + 2]);
      for (let k = 0; k < 3; k++) {
        if (q[k] < lo[k]) lo[k] = q[k];
        if (q[k] > hi[k]) hi[k] = q[k];
      }
    }
    this.centre = [0, 1, 2].map((k) => (lo[k] + hi[k]) / 2) as [number, number, number];
    const big = Math.max(hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]);
    this.scale = big > 0 && isFinite(big) ? 1 / big : 1;
  }

  /// File axes → the preview's (Y up, facing +Z).
  private axes(x: number, y: number, z: number): [number, number, number] {
    return this.zUp ? [x, z, -y] : [x, y, z];
  }

  setParts(shades: PartShade[]) {
    if (!this.ok) return;
    const gl = this.gl;
    const n = Math.max(1, shades.length);
    const w = Math.min(1024, n);
    const h = Math.ceil(n / 1024) * 4;
    const px = new Float32Array(w * h * 4);
    shades.forEach((s, i) => {
      const x = i % 1024;
      const y0 = Math.floor(i / 1024) * 4;
      const set = (row: number, v: number[]) => px.set(v, ((y0 + row) * w + x) * 4);
      set(0, s.color ? [...s.color, 1] : [0, 0, 0, 0]);
      set(1, [...s.specular, Math.max(1, s.shininess)]);
      set(2, [...s.emissive, s.opacity]);
      set(3, [s.visible ? 1 : 0, s.map ? 1 : 0, 0, 0]);
    });
    gl.bindTexture(gl.TEXTURE_2D, this.partsTex);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA32F, w, h, 0, gl.RGBA, gl.FLOAT, px);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
    this.draw();
  }

  reset() {
    this.yaw = -30;
    this.pitch = 18;
    this.distance = 2.6;
    this.pan = [0, 0];
    this.draw();
  }

  draw() {
    if (!this.ok || this.frame) return;
    this.frame = requestAnimationFrame(() => {
      this.frame = 0;
      this.render();
    });
  }

  private render() {
    const gl = this.gl;
    const c = this.canvas;
    const dpr = window.devicePixelRatio || 1;
    const [w, h] = [Math.max(1, Math.round(c.clientWidth * dpr)), Math.max(1, Math.round(c.clientHeight * dpr))];
    if (c.width !== w || c.height !== h) [c.width, c.height] = [w, h];
    gl.viewport(0, 0, w, h);
    gl.clearColor(0, 0, 0, 1);
    gl.clear(gl.COLOR_BUFFER_BIT | gl.DEPTH_BUFFER_BIT);
    if (!this.vao || !this.count) return;
    gl.enable(gl.DEPTH_TEST);
    gl.enable(gl.BLEND);
    gl.blendFunc(gl.ONE, gl.ONE_MINUS_SRC_ALPHA);
    gl.useProgram(this.prog);
    // Model: file axes → centred, unit size; then turned, moved, and seen in perspective.
    const s = this.scale;
    const [cx, cy, cz] = this.centre;
    const ax = this.zUp ? [1, 0, 0, 0, 0, 0, -1, 0, 0, 1, 0, 0] : [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0];
    const model = mul(scaleM(s), mul(translateM(-cx, -cy, -cz), [ax[0], ax[1], ax[2], 0, ax[4], ax[5], ax[6], 0, ax[8], ax[9], ax[10], 0, 0, 0, 0, 1]));
    const turn = mul(rotX(this.pitch), rotY(this.yaw));
    const view = mul(translateM(this.pan[0], this.pan[1], -this.distance), turn);
    const proj = perspective(35, w / h, 0.01, 100);
    const mv = mul(view, model);
    const mvp = mul(proj, mv);
    gl.uniformMatrix4fv(gl.getUniformLocation(this.prog, "u_mvp"), false, mvp);
    // Normals: turned as the model is (it's scaled evenly), into the eye's space, where it's lit.
    const nm = mul(turn, [ax[0], ax[1], ax[2], 0, ax[4], ax[5], ax[6], 0, ax[8], ax[9], ax[10], 0, 0, 0, 0, 1]);
    gl.uniformMatrix3fv(gl.getUniformLocation(this.prog, "u_nm"), false, [nm[0], nm[1], nm[2], nm[4], nm[5], nm[6], nm[8], nm[9], nm[10]]);
    gl.uniformMatrix4fv(gl.getUniformLocation(this.prog, "u_mv"), false, mv);
    gl.uniform1i(gl.getUniformLocation(this.prog, "u_own"), this.own ? 1 : 0);
    gl.uniform3f(gl.getUniformLocation(this.prog, "u_flat"), ...this.flat);
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, this.atlas);
    gl.uniform1i(gl.getUniformLocation(this.prog, "u_atlas"), 0);
    gl.activeTexture(gl.TEXTURE1);
    gl.bindTexture(gl.TEXTURE_2D, this.partsTex);
    gl.uniform1i(gl.getUniformLocation(this.prog, "u_parts"), 1);
    gl.bindVertexArray(this.vao);
    gl.drawElements(gl.TRIANGLES, this.count, gl.UNSIGNED_INT, 0);
    gl.bindVertexArray(null);
  }

  dispose() {
    if (this.frame) cancelAnimationFrame(this.frame);
    this.gl?.getExtension("WEBGL_lose_context")?.loseContext();
  }
}

// ---- 4×4 matrices, column-major ----
type M = number[];
function mul(a: M, b: M): M {
  const o = new Array(16).fill(0);
  for (let c = 0; c < 4; c++) for (let r = 0; r < 4; r++) for (let k = 0; k < 4; k++) o[c * 4 + r] += a[k * 4 + r] * b[c * 4 + k];
  return o;
}
const translateM = (x: number, y: number, z: number): M => [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, x, y, z, 1];
const scaleM = (s: number): M => [s, 0, 0, 0, 0, s, 0, 0, 0, 0, s, 0, 0, 0, 0, 1];
function rotX(deg: number): M {
  const [s, c] = [Math.sin((deg * Math.PI) / 180), Math.cos((deg * Math.PI) / 180)];
  return [1, 0, 0, 0, 0, c, s, 0, 0, -s, c, 0, 0, 0, 0, 1];
}
function rotY(deg: number): M {
  const [s, c] = [Math.sin((deg * Math.PI) / 180), Math.cos((deg * Math.PI) / 180)];
  return [c, 0, -s, 0, 0, 1, 0, 0, s, 0, c, 0, 0, 0, 0, 1];
}
function perspective(fovDeg: number, aspect: number, near: number, far: number): M {
  const f = 1 / Math.tan((fovDeg * Math.PI) / 360);
  return [f / aspect, 0, 0, 0, 0, f, 0, 0, 0, 0, (far + near) / (near - far), -1, 0, 0, (2 * far * near) / (near - far), 0];
}
