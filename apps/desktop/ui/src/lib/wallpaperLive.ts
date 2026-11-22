/**
 * WebGL live cloud renderer: layered, slowly drifting procedural fbm clouds rendered at low
 * internal resolution with an ordered dither / halftone post-effect mapped to a 4-colour palette.
 *
 * Kept separate from `wallpaper.ts` so pure unit tests run in Node without WebGL, and so `startWallpaper`
 * can fall back to the 2D static wallpaper whenever WebGL is unavailable or context is lost.
 */

export interface LiveCloudConfig {
  /** Packed 4x3 RGB floats in [0, 1], darkest sky first. */
  paletteRgb: Float32Array;
  /** 8x8 dither thresholds quantized to 0..255 (computed from bayerThreshold / diagonalThreshold). */
  ditherBytes: Uint8Array;
  /** Cloud coverage 0..1. */
  cover: number;
  /** Bottom haze 0..1. */
  haze: number;
}

export interface LiveCloudRenderer {
  resize(w: number, h: number): void;
  draw(elapsedSec: number): void;
  /** Read current framebuffer top-to-bottom RGBA bytes (for one-shot tone sampling). */
  readPixelsRgba(): Uint8Array | null;
  dispose(): void;
}

const VERTEX_SRC = "attribute vec2 p; void main(){ gl_Position = vec4(p, 0.0, 1.0); }";

const FRAGMENT_SRC = `
precision mediump float;
uniform vec2 u_res;
uniform float u_time;
uniform float u_cover;
uniform float u_haze;
uniform vec3 u_c0;
uniform vec3 u_c1;
uniform vec3 u_c2;
uniform vec3 u_c3;
uniform sampler2D u_dither;

float hash(vec2 p) {
  return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453123);
}

float noise(vec2 p) {
  vec2 i = floor(p);
  vec2 f = fract(p);
  vec2 u = f * f * (3.0 - 2.0 * f);
  return mix(
    mix(hash(i), hash(i + vec2(1.0, 0.0)), u.x),
    mix(hash(i + vec2(0.0, 1.0)), hash(i + vec2(1.0, 1.0)), u.x),
    u.y
  );
}

float fbm(vec2 p) {
  float v = 0.0;
  float a = 0.5;
  mat2 rot = mat2(0.8, 0.6, -0.6, 0.8);
  for (int i = 0; i < 4; i++) {
    v += a * noise(p);
    p = rot * p * 2.02 + vec2(17.1, 31.7);
    a *= 0.5;
  }
  return v;
}

float cloudLayer(vec2 uv, float speed, float scale, float bandCenter, float bandWidth, float seed, float morph) {
  vec2 p = vec2(uv.x * scale + u_time * speed, (uv.y - bandCenter) * scale * 1.45);
  float warp = fbm(p * 0.65 + vec2(seed * 0.37, morph * u_time * 0.6)) * 0.35;
  float n = fbm(p + vec2(seed + warp, morph * u_time));
  float vertical = 1.0 - smoothstep(0.0, bandWidth, abs(uv.y - bandCenter));
  float raw = n * 0.72 + vertical * 0.44 - (1.0 - u_cover) * 0.52;
  return smoothstep(0.27, 0.76, raw);
}

void main() {
  vec2 uv = gl_FragCoord.xy / max(vec2(1.0), u_res);
  uv.y = 1.0 - uv.y;
  float aspect = u_res.x / max(1.0, u_res.y);
  vec2 auv = vec2(uv.x * aspect, uv.y);

  // Three parallax cloud layers (distant wisps, mid cumulus, lower foreground banks).
  float l1 = cloudLayer(auv, 0.008, 2.2, 0.42, 0.36, 3.1, 0.003);
  float l2 = cloudLayer(auv, 0.017, 3.0, 0.68, 0.44, 11.7, 0.006);
  float l3 = cloudLayer(auv, 0.028, 4.1, 0.88, 0.36, 23.4, 0.009);

  // Upper-left directional light + shaded undersides.
  float shade2 = cloudLayer(auv - vec2(0.014, 0.016), 0.017, 3.0, 0.68, 0.44, 11.7, 0.006);
  float lit2 = clamp(0.58 + (l2 - shade2) * 1.6 + (1.0 - uv.y) * 0.16, 0.25, 1.0);
  float shade3 = cloudLayer(auv - vec2(0.011, 0.013), 0.028, 4.1, 0.88, 0.36, 23.4, 0.009);
  float lit3 = clamp(0.58 + (l3 - shade3) * 1.5 + (1.0 - uv.y) * 0.16, 0.25, 1.0);

  float d = l1 * 0.48;
  d = max(d, l2 * lit2 * 0.88);
  d = max(d, l3 * lit3);

  float hz = u_haze * uv.y * uv.y;
  d = clamp(d + hz * (1.0 - min(1.0, d * 2.0)), 0.0, 1.0);

  // Ordered dither / halftone post-effect over the 4-colour theme palette.
  float thr = texture2D(u_dither, (gl_FragCoord.xy + 0.5) / 8.0).r;
  float s = d * 3.0;
  float base = min(2.0, floor(s));
  float idx = base + (s - base > thr ? 1.0 : 0.0);
  vec3 col = idx < 0.5 ? u_c0 : (idx < 1.5 ? u_c1 : (idx < 2.5 ? u_c2 : u_c3));
  gl_FragColor = vec4(col, 1.0);
}
`;

function compileShader(gl: WebGLRenderingContext, type: number, source: string): WebGLShader | null {
  const shader = gl.createShader(type);
  if (!shader) return null;
  gl.shaderSource(shader, source);
  gl.compileShader(shader);
  if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
    gl.deleteShader(shader);
    return null;
  }
  return shader;
}

/** Create a low-power WebGL renderer for layered clouds, or null when WebGL is unavailable. */
export function createLiveCloudRenderer(
  canvas: HTMLCanvasElement,
  config: LiveCloudConfig,
  onContextLost?: () => void
): LiveCloudRenderer | null {
  let gl: WebGLRenderingContext | null = null;
  try {
    gl = canvas.getContext("webgl", {
      alpha: false,
      antialias: false,
      depth: false,
      stencil: false,
      preserveDrawingBuffer: false,
      powerPreference: "low-power"
    });
  } catch {
    return null;
  }
  if (!gl) return null;

  const vs = compileShader(gl, gl.VERTEX_SHADER, VERTEX_SRC);
  const fs = compileShader(gl, gl.FRAGMENT_SHADER, FRAGMENT_SRC);
  const program = vs && fs ? gl.createProgram() : null;
  if (!vs || !fs || !program) {
    if (vs) gl.deleteShader(vs);
    if (fs) gl.deleteShader(fs);
    if (program) gl.deleteProgram(program);
    return null;
  }
  gl.attachShader(program, vs);
  gl.attachShader(program, fs);
  gl.linkProgram(program);
  gl.deleteShader(vs);
  gl.deleteShader(fs);
  if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
    gl.deleteProgram(program);
    return null;
  }

  gl.useProgram(program);
  const buffer = gl.createBuffer();
  gl.bindBuffer(gl.ARRAY_BUFFER, buffer);
  gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 3, -1, -1, 3]), gl.STATIC_DRAW);
  const posLoc = gl.getAttribLocation(program, "p");
  gl.enableVertexAttribArray(posLoc);
  gl.vertexAttribPointer(posLoc, 2, gl.FLOAT, false, 0, 0);

  const ditherTex = gl.createTexture();
  gl.activeTexture(gl.TEXTURE0);
  gl.bindTexture(gl.TEXTURE_2D, ditherTex);
  gl.texImage2D(gl.TEXTURE_2D, 0, gl.LUMINANCE, 8, 8, 0, gl.LUMINANCE, gl.UNSIGNED_BYTE, config.ditherBytes);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.REPEAT);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.REPEAT);

  const uRes = gl.getUniformLocation(program, "u_res");
  const uTime = gl.getUniformLocation(program, "u_time");
  gl.uniform1i(gl.getUniformLocation(program, "u_dither"), 0);
  gl.uniform1f(gl.getUniformLocation(program, "u_cover"), config.cover);
  gl.uniform1f(gl.getUniformLocation(program, "u_haze"), config.haze);
  gl.uniform3fv(gl.getUniformLocation(program, "u_c0"), config.paletteRgb.subarray(0, 3));
  gl.uniform3fv(gl.getUniformLocation(program, "u_c1"), config.paletteRgb.subarray(3, 6));
  gl.uniform3fv(gl.getUniformLocation(program, "u_c2"), config.paletteRgb.subarray(6, 9));
  gl.uniform3fv(gl.getUniformLocation(program, "u_c3"), config.paletteRgb.subarray(9, 12));

  let width = Math.max(2, canvas.width);
  let height = Math.max(2, canvas.height);
  let lost = false;

  const handleLost = (e: Event) => {
    e.preventDefault();
    lost = true;
    onContextLost?.();
  };
  canvas.addEventListener("webglcontextlost", handleLost);

  return {
    resize(w: number, h: number) {
      width = Math.max(2, w);
      height = Math.max(2, h);
      canvas.width = width;
      canvas.height = height;
      if (lost) return;
      gl.viewport(0, 0, width, height);
      gl.uniform2f(uRes, width, height);
    },
    draw(elapsedSec: number) {
      if (lost) return;
      gl.uniform1f(uTime, elapsedSec);
      gl.drawArrays(gl.TRIANGLES, 0, 3);
    },
    readPixelsRgba(): Uint8Array | null {
      if (lost) return null;
      try {
        const raw = new Uint8Array(width * height * 4);
        gl.readPixels(0, 0, width, height, gl.RGBA, gl.UNSIGNED_BYTE, raw);
        // Flip vertically so row 0 is the top of the viewport (matches 2D ImageData).
        const flipped = new Uint8Array(raw.length);
        const rowBytes = width * 4;
        for (let y = 0; y < height; y++) {
          const srcOff = (height - 1 - y) * rowBytes;
          flipped.set(raw.subarray(srcOff, srcOff + rowBytes), y * rowBytes);
        }
        return flipped;
      } catch {
        return null;
      }
    },
    dispose() {
      canvas.removeEventListener("webglcontextlost", handleLost);
      if (ditherTex) gl.deleteTexture(ditherTex);
      if (buffer) gl.deleteBuffer(buffer);
      gl.deleteProgram(program);
    }
  };
}
