/**
 * Backdrop scenes drawn once onto a canvas (card 198-A).  No animation loop:
 * callers redraw on resize (debounced) and when a theme-aware scene's colours
 * change.  Ported from the approved prototype (`SCENES` + `drawBackdrop`).
 */
import { mix, normalizeHex } from "ui";
import { backdropInfo, type BackdropSceneInfo } from "./backdropCatalog";

/** Theme colours that theme-aware scenes (ink, contour, grid) draw with. */
export interface SceneColors {
  bg: string;
  raised: string;
  text: string;
  accent: string;
}

type Ctx = CanvasRenderingContext2D;
type Blob = readonly [color: string, x: number, y: number, radius: number];
type SceneFn = (x: Ctx, w: number, h: number, c: SceneColors) => void;

/** 8x8 Bayer matrix. */
const B8 = [
  0, 32, 8, 40, 2, 34, 10, 42, 48, 16, 56, 24, 50, 18, 58, 26, 12, 44, 4, 36, 14, 46, 6, 38, 60, 28, 52, 20, 62, 30, 54, 22,
  3, 35, 11, 43, 1, 33, 9, 41, 51, 19, 59, 27, 49, 17, 57, 25, 15, 47, 7, 39, 13, 45, 5, 37, 63, 31, 55, 23, 61, 29, 53, 21
];

function blobs(x: Ctx, w: number, h: number, base: string, list: readonly Blob[]): void {
  x.fillStyle = base;
  x.fillRect(0, 0, w, h);
  for (const [c, px, py, pr] of list) {
    const g = x.createRadialGradient(px * w, py * h, 0, px * w, py * h, pr * Math.max(w, h));
    g.addColorStop(0, c);
    g.addColorStop(1, `${c}00`);
    x.fillStyle = g;
    x.fillRect(0, 0, w, h);
  }
}

/** Seeded generator so scenes look the same on every redraw. */
function seeded(seed: number): () => number {
  let s = seed;
  return () => (s = (s * 16807) % 2147483647) / 2147483647;
}

const hex = (c: string): string => normalizeHex(c) ?? "#000000";
const rgb = (c: string): [number, number, number] => {
  const h = hex(c);
  return [1, 3, 5].map((i) => parseInt(h.slice(i, i + 2), 16)) as [number, number, number];
};

const SCENES: Record<string, SceneFn> = {
  ink: (x, w, h, c) => {
    const a = hex(c.accent);
    blobs(x, w, h, hex(c.bg), [[a, 0.82, 0.12, 0.5], [a, 0.1, 0.95, 0.42], [hex(c.raised), 0.5, 0.5, 0.6]]);
  },
  aurora: (x, w, h) =>
    blobs(x, w, h, "#3f8f96", [["#7fd0b8", 0.2, 0.2, 0.7], ["#b9e3a6", 0.75, 0.55, 0.6], ["#f3c98b", 0.5, 0.75, 0.35], ["#f6e2b0", 0.52, 0.7, 0.16], ["#2b6f86", 0.95, 0.05, 0.5]]),
  meadow: (x, w, h) => {
    x.fillStyle = "#17260f";
    x.fillRect(0, 0, w, h);
    x.filter = `blur(${Math.max(1, w / 160)}px)`;
    const cs = ["#2f5a1c", "#4f8a22", "#86a83a", "#c9cfa0", "#1d3a14", "#6f9a2c", "#e9e6cf"];
    const rnd = seeded(7);
    for (let i = 0; i < 150; i++) {
      x.save();
      x.translate(rnd() * w, rnd() * h);
      x.rotate(-0.9);
      x.globalAlpha = 0.25 + rnd() * 0.5;
      x.fillStyle = cs[i % cs.length];
      x.fillRect(-w * 0.3, 0, w * (0.25 + rnd() * 0.5), 1 + rnd() * h * 0.035);
      x.restore();
    }
    x.filter = "none";
    x.globalAlpha = 1;
  },
  sunset: (x, w, h) =>
    blobs(x, w, h, "#cfc7ea", [["#ff9a52", 0.15, 0.35, 0.6], ["#ffd166", 0.3, 0.1, 0.4], ["#ff7eb6", 0.55, 0.6, 0.5], ["#8fd3ff", 0.95, 0.2, 0.55], ["#b7f0c8", 0.75, 0.9, 0.4]]),
  night: (x, w, h) =>
    blobs(x, w, h, "#05060f", [["#3b2a8a", 0.3, 0.3, 0.6], ["#0f5a7a", 0.8, 0.8, 0.5], ["#c94f7c", 0.9, 0.1, 0.3]]),
  peach: (x, w, h) =>
    blobs(x, w, h, "#f6d5c0", [["#ff8fa3", 0.2, 0.3, 0.6], ["#ffc46b", 0.8, 0.2, 0.5], ["#ff7b54", 0.6, 0.9, 0.5]]),
  lagoon: (x, w, h) =>
    blobs(x, w, h, "#0c3b4a", [["#19c2b0", 0.25, 0.75, 0.6], ["#f2d38b", 0.8, 0.2, 0.4], ["#0a6aa6", 0.7, 0.9, 0.5]]),
  silk: (x, w, h) => {
    x.fillStyle = "#0b0b0c";
    x.fillRect(0, 0, w, h);
    x.save();
    x.translate(w * 0.45, h * 0.5);
    x.rotate(-0.95);
    for (const [o, t, a] of [[-0.02, 0.09, 0.5], [0.17, 0.03, 0.28], [-0.32, 0.2, 0.13]]) {
      const g = x.createLinearGradient(0, (o - t) * w, 0, (o + t) * w);
      g.addColorStop(0, "rgba(255,255,255,0)");
      g.addColorStop(0.5, `rgba(235,235,242,${a})`);
      g.addColorStop(1, "rgba(255,255,255,0)");
      x.fillStyle = g;
      x.fillRect(-w * 2, -w * 2, w * 4, w * 4);
    }
    x.restore();
  },
  dawn: (x, w, h) =>
    blobs(x, w, h, "#1a1030", [["#ff6a3d", 0.15, 0.95, 0.6], ["#ff3d81", 0.6, 1, 0.5], ["#6a3dff", 0.9, 0.15, 0.6], ["#ffb347", 0.3, 0.65, 0.28]]),
  ocean: (x, w, h) =>
    blobs(x, w, h, "#041018", [["#0b6cff", 0.2, 0.2, 0.6], ["#00d1c1", 0.8, 0.75, 0.55], ["#123a8a", 0.6, 0.1, 0.5], ["#7af0ff", 0.45, 0.9, 0.22]]),
  lava: (x, w, h) =>
    blobs(x, w, h, "#120606", [["#ff4d1a", 0.2, 0.85, 0.6], ["#ffae00", 0.75, 0.95, 0.38], ["#7a0c2e", 0.8, 0.2, 0.6]]),
  pearl: (x, w, h) =>
    blobs(x, w, h, "#e9e6f0", [["#ffd9e8", 0.2, 0.2, 0.6], ["#cfe4ff", 0.8, 0.3, 0.6], ["#fff2c9", 0.5, 0.9, 0.5], ["#d9ffe9", 0.1, 0.9, 0.4]]),
  smoke: (x, w, h) =>
    blobs(x, w, h, "#101012", [["#5a5a66", 0.2, 0.3, 0.5], ["#2c2c36", 0.8, 0.7, 0.6], ["#8a8a99", 0.7, 0.15, 0.25]]),
  curtain: (x, w, h) => {
    x.fillStyle = "#07070a";
    x.fillRect(0, 0, w, h);
    const rnd = seeded(11);
    for (let i = 0; i < 26; i++) {
      const cx = rnd() * w;
      const bw = w * (0.02 + rnd() * 0.07);
      const g = x.createLinearGradient(0, h, 0, h * (0.15 + rnd() * 0.5));
      g.addColorStop(0, `rgba(235,235,245,${0.1 + rnd() * 0.25})`);
      g.addColorStop(1, "rgba(235,235,245,0)");
      x.fillStyle = g;
      x.fillRect(cx, 0, bw, h);
    }
  },
  reed: (x, w, h) =>
    blobs(x, w, h, "#0e1320", [["#3d6bff", 0.2, 0.3, 0.6], ["#ff7a59", 0.8, 0.75, 0.5], ["#ffd27a", 0.6, 0.2, 0.3]]),
  reed2: (x, w, h) =>
    blobs(x, w, h, "#1a0f16", [["#ff5d8f", 0.25, 0.7, 0.6], ["#7a5cff", 0.8, 0.25, 0.55], ["#ffc2a8", 0.5, 0.5, 0.25]]),
  reed3: (x, w, h) =>
    blobs(x, w, h, "#dfe7f2", [["#8fb8ff", 0.2, 0.3, 0.6], ["#ffffff", 0.7, 0.2, 0.4], ["#b9a2f7", 0.8, 0.8, 0.5]]),
  contour: (x, w, h) =>
    blobs(x, w, h, "#000000", [["#ffffff", 0.2, 0.25, 0.55], ["#ffffff", 0.8, 0.7, 0.6], ["#ffffff", 0.55, 0.1, 0.3], ["#ffffff", 0.1, 0.95, 0.35]]),
  grid: (x, w, h, c) => {
    const bg = hex(c.bg);
    const tx = hex(c.text);
    const ac = hex(c.accent);
    const s = Math.max(8, Math.round(w / 52));
    x.fillStyle = bg;
    x.fillRect(0, 0, w, h);
    x.strokeStyle = mix(bg, tx, 0.1);
    x.lineWidth = 1;
    x.beginPath();
    for (let i = 0.5; i < w; i += s) {
      x.moveTo(i, 0);
      x.lineTo(i, h);
    }
    for (let j = 0.5; j < h; j += s) {
      x.moveTo(0, j);
      x.lineTo(w, j);
    }
    x.stroke();
    let g = x.createRadialGradient(w / 2, 0, 0, w / 2, 0, Math.max(w, h) * 0.75);
    g.addColorStop(0, `${bg}00`);
    g.addColorStop(1, bg);
    x.fillStyle = g;
    x.fillRect(0, 0, w, h);
    g = x.createRadialGradient(w / 2, 0, 0, w / 2, 0, w * 0.5);
    g.addColorStop(0, `${ac}55`);
    g.addColorStop(1, `${ac}00`);
    x.fillStyle = g;
    x.fillRect(0, 0, w, h);
  }
};

/** Does the scene read the theme colours (so it must be redrawn when they change)? */
export function sceneIsThemed(id: string): boolean {
  return backdropInfo(id)?.themed === true;
}

/** Internal resolution divisor: dithered scenes are drawn chunky on purpose. */
export function sceneScale(info: BackdropSceneInfo | undefined): number {
  return info?.fx ? 1.5 : 3;
}

function flute(x: Ctx, cv: HTMLCanvasElement, w: number, h: number): void {
  // Reeded glass: each strip bends the picture behind it.
  const t = document.createElement("canvas");
  t.width = w;
  t.height = h;
  t.getContext("2d")?.drawImage(cv, 0, 0);
  const s = Math.max(5, Math.round(w / 44));
  for (let i = 0; i < w; i += s) {
    x.drawImage(t, Math.max(0, i - s * 1.2), 0, s * 3.4, h, i, 0, s, h);
    const gr = x.createLinearGradient(i, 0, i + s, 0);
    gr.addColorStop(0, "rgba(255,255,255,.10)");
    gr.addColorStop(0.45, "rgba(255,255,255,0)");
    gr.addColorStop(1, "rgba(0,0,0,.22)");
    x.fillStyle = gr;
    x.fillRect(i, 0, s, h);
  }
}

/** Draw `scene` into `cv` at `w` x `h` pixels (nothing is drawn for an unknown id). */
export function drawBackdrop(cv: HTMLCanvasElement, scene: string, w: number, h: number, colors: SceneColors): void {
  const info = backdropInfo(scene);
  const draw = SCENES[scene];
  if (!info || !draw || w < 1 || h < 1) return;
  const fx = info.fx;
  cv.width = w;
  cv.height = h;
  cv.style.imageRendering = fx ? "auto" : "pixelated";
  const x = cv.getContext("2d", { willReadFrequently: true });
  if (!x) return;
  draw(x, w, h, colors);
  if (fx === "flute") flute(x, cv, w, h);
  if (fx === "raw") return;
  const im = x.getImageData(0, 0, w, h);
  const d = im.data;
  if (fx === "contour") {
    // Iso-height lines.
    const bg = rgb(colors.bg);
    const ln = rgb(colors.accent);
    const N = 13;
    const band = new Uint8Array(w * h);
    for (let p = 0; p < w * h; p++) band[p] = ((d[p * 4] * 0.3 + d[p * 4 + 1] * 0.59 + d[p * 4 + 2] * 0.11) / 255 * N) | 0;
    for (let y = 0; y < h; y++) {
      for (let i = 0; i < w; i++) {
        const p = y * w + i;
        const edge = (i + 1 < w && band[p] !== band[p + 1]) || (y + 1 < h && band[p] !== band[p + w]);
        const a = edge ? 0.5 : (band[p] / N) * 0.1;
        for (let k = 0; k < 3; k++) d[p * 4 + k] = bg[k] * (1 - a) + ln[k] * a;
      }
    }
  } else if (fx === "grain" || fx === "flute") {
    const amount = fx === "flute" ? 22 : 38;
    for (let o = 0; o < d.length; o += 4) {
      const n = (Math.random() - 0.5) * amount;
      d[o] += n;
      d[o + 1] += n;
      d[o + 2] += n;
    }
  } else {
    // Bayer dither to 4 levels per channel.
    const L = 4;
    for (let y = 0; y < h; y++) {
      for (let i = 0; i < w; i++) {
        const t = (B8[(y & 7) * 8 + (i & 7)] / 64 - 0.5) / L;
        const o = (y * w + i) * 4;
        for (let k = 0; k < 3; k++) {
          const v = Math.round((d[o + k] / 255 + t) * L) / L;
          d[o + k] = v < 0 ? 0 : v > 1 ? 255 : v * 255;
        }
      }
    }
  }
  x.putImageData(im, 0, 0);
}
