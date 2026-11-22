import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { after, before, test } from "node:test";
import { createServer, type ViteDevServer } from "vite";
import { BACKDROP_SCENES, backdropInfo } from "./backdropCatalog.ts";
import type * as BackdropDraw from "./backdropDraw.ts";

// backdropDraw draws onto a canvas, so the tests hand it a fake canvas whose 2D context records what
// a scene asks for and serves the pixels the test chooses. The module is loaded through Vite SSR
// because it imports the workspace package `ui` and its sibling without a file extension.
type Rgb = [number, number, number];

interface FakeCanvas {
  canvas: HTMLCanvasElement;
  contextCalls: unknown[][];
  rects: number;
  fills: string[];
  stops: string[];
  reads: number;
  writes: Uint8ClampedArray[];
}

function fakeCanvas(pixel: (x: number, y: number) => Rgb = () => [128, 128, 128], hasContext = true): FakeCanvas {
  const rec = { contextCalls: [], rects: 0, fills: [], stops: [], reads: 0, writes: [] } as unknown as FakeCanvas;
  const noop = (): void => {};
  const gradient = () => ({ addColorStop: (_at: number, color: string) => void rec.stops.push(color) });
  const ctx = {
    filter: "none",
    set fillStyle(value: unknown) {
      if (typeof value === "string") rec.fills.push(value);
    },
    fillRect: () => void rec.rects++,
    createRadialGradient: gradient,
    createLinearGradient: gradient,
    save: noop,
    restore: noop,
    translate: noop,
    rotate: noop,
    beginPath: noop,
    moveTo: noop,
    lineTo: noop,
    stroke: noop,
    drawImage: noop,
    getImageData: (_sx: number, _sy: number, w: number, h: number) => {
      rec.reads++;
      const data = new Uint8ClampedArray(w * h * 4);
      for (let y = 0; y < h; y++) {
        for (let x = 0; x < w; x++) {
          const o = (y * w + x) * 4;
          [data[o], data[o + 1], data[o + 2]] = pixel(x, y);
          data[o + 3] = 255;
        }
      }
      return { data, width: w, height: h };
    },
    putImageData: (image: { data: Uint8ClampedArray }) => void rec.writes.push(image.data)
  };
  rec.canvas = {
    width: 0,
    height: 0,
    style: { imageRendering: "" },
    getContext: (...args: unknown[]) => {
      rec.contextCalls.push(args);
      return hasContext ? ctx : null;
    }
  } as unknown as HTMLCanvasElement;
  return rec;
}

const COLORS: BackdropDraw.SceneColors = { bg: "#101820", raised: "#1c2833", text: "#e8eef2", accent: "#ff7a59" };

let server: ViteDevServer;
let drawBackdrop: typeof BackdropDraw.drawBackdrop;
let sceneIsThemed: typeof BackdropDraw.sceneIsThemed;
let sceneScale: typeof BackdropDraw.sceneScale;
const previousDocument = Object.getOwnPropertyDescriptor(globalThis, "document");

before(async () => {
  // The reeded-glass scenes copy the picture through a second canvas made with `document`.
  Object.defineProperty(globalThis, "document", {
    configurable: true,
    value: { createElement: () => fakeCanvas().canvas }
  });
  server = await createServer({
    root: fileURLToPath(new URL("../..", import.meta.url)),
    logLevel: "error",
    optimizeDeps: { noDiscovery: true, entries: [] },
    server: { middlewareMode: true, watch: null, hmr: false },
    ssr: { noExternal: ["ui"] }
  });
  ({ drawBackdrop, sceneIsThemed, sceneScale } = await server.ssrLoadModule("/src/lib/backdropDraw.ts"));
});

after(async () => {
  await server?.close();
  if (previousDocument) Object.defineProperty(globalThis, "document", previousDocument);
  else Reflect.deleteProperty(globalThis, "document");
});

test("only ink, contour and grid follow the theme, and effect scenes are drawn at the finer scale", () => {
  for (const id of ["ink", "contour", "grid"]) assert.equal(sceneIsThemed(id), true, id);
  for (const id of ["aurora", "silk", "reed", "nope"]) assert.equal(sceneIsThemed(id), false, id);
  assert.equal(sceneScale(backdropInfo("aurora")), 3);
  assert.equal(sceneScale(backdropInfo("silk")), 1.5);
  assert.equal(sceneScale(backdropInfo("grid")), 1.5);
  assert.equal(sceneScale(undefined), 3);
});

test("an unknown scene, the none and custom values or an empty canvas draw nothing", () => {
  const cases: [string, number, number][] = [
    ["nope", 16, 12],
    ["none", 16, 12],
    ["custom", 16, 12],
    ["aurora", 0, 12],
    ["aurora", 16, 0],
    ["aurora", 0.5, 12]
  ];
  for (const [scene, w, h] of cases) {
    const c = fakeCanvas();
    drawBackdrop(c.canvas, scene, w, h, COLORS);
    assert.deepEqual([c.canvas.width, c.canvas.height, c.contextCalls.length], [0, 0, 0], `${scene} ${w}x${h}`);
  }
});

test("every scene in the catalog is drawn at the requested size, dithered ones pixelated", () => {
  for (const scene of BACKDROP_SCENES) {
    const c = fakeCanvas();
    drawBackdrop(c.canvas, scene.id, 16, 12, COLORS);
    assert.deepEqual([c.canvas.width, c.canvas.height], [16, 12], scene.id);
    assert.ok(c.rects > 0, `${scene.id} painted nothing`);
    assert.equal(c.canvas.style.imageRendering, scene.fx ? "auto" : "pixelated", scene.id);
    assert.deepEqual(c.contextCalls, [["2d", { willReadFrequently: true }]], scene.id);
  }
});

test("a canvas without a 2D context is left alone instead of throwing", () => {
  const c = fakeCanvas(undefined, false);
  drawBackdrop(c.canvas, "aurora", 16, 12, COLORS);
  assert.equal(c.rects, 0);
  assert.equal(c.writes.length, 0);
});

test("themed scenes use the theme colours and a bad colour falls back to black", () => {
  const ink = fakeCanvas();
  drawBackdrop(ink.canvas, "ink", 16, 12, { ...COLORS, bg: "#ABC" });
  assert.equal(ink.fills[0], "#aabbcc");
  assert.ok(ink.stops.includes(COLORS.accent), "accent blob");
  assert.ok(ink.stops.includes(`${COLORS.accent}00`), "accent fades to transparent");
  assert.ok(ink.stops.includes(COLORS.raised), "raised blob");

  const bad = fakeCanvas();
  drawBackdrop(bad.canvas, "ink", 16, 12, { ...COLORS, bg: "not a colour" });
  assert.equal(bad.fills[0], "#000000");

  // A fixed scene ignores the theme.
  const aurora = fakeCanvas();
  drawBackdrop(aurora.canvas, "aurora", 16, 12, COLORS);
  assert.equal(aurora.fills[0], "#3f8f96");
  assert.ok(!aurora.stops.includes(COLORS.accent));
});

test("the grid scene is final as drawn, every other scene gets one read and one write of its pixels", () => {
  const grid = fakeCanvas();
  drawBackdrop(grid.canvas, "grid", 16, 12, COLORS);
  assert.deepEqual([grid.reads, grid.writes.length], [0, 0]);

  for (const id of ["aurora", "silk", "reed", "contour"]) {
    const c = fakeCanvas();
    drawBackdrop(c.canvas, id, 16, 12, COLORS);
    assert.equal(c.reads, 1, id);
    assert.equal(c.writes.length, 1, id);
    assert.equal(c.writes[0].length, 16 * 12 * 4, id);
  }
});

test("dithered scenes are cut to four levels per channel without losing the tone", () => {
  const c = fakeCanvas(() => [100, 100, 100]);
  drawBackdrop(c.canvas, "aurora", 16, 16, COLORS);
  const [pixels] = c.writes;
  const levels = new Set<number>();
  let sum = 0;
  for (let o = 0; o < pixels.length; o += 4) {
    assert.equal(pixels[o + 3], 255, "alpha is untouched");
    for (let k = 0; k < 3; k++) levels.add(pixels[o + k]);
    sum += pixels[o];
  }
  // Mid-grey sits between two of the five levels (0, 64, 128, 191, 255): the ordered pattern uses both.
  assert.deepEqual([...levels].sort((a, b) => a - b), [64, 128]);
  assert.ok(Math.abs(sum / (16 * 16) - 100) < 1, `mean ${sum / (16 * 16)}`);
});

test("grain is seeded by scene and size, and stays within the grain amount", () => {
  const grain = (scene: string, w: number, h: number): Uint8ClampedArray => {
    const c = fakeCanvas();
    drawBackdrop(c.canvas, scene, w, h, COLORS);
    return c.writes[0];
  };
  const first = grain("silk", 16, 12);
  assert.deepEqual(grain("silk", 16, 12), first, "same scene and size draws the same grain");
  assert.notDeepEqual(grain("dawn", 16, 12), first, "another scene gets other grain");
  assert.notDeepEqual(grain("silk", 20, 12).slice(0, 64), first.slice(0, 64), "another size gets other grain");

  // The flat grey the fake canvas serves moves by at most half the amount (38 for grain, 22 for flute).
  const spread = (pixels: Uint8ClampedArray): number => {
    let widest = 0;
    for (let o = 0; o < pixels.length; o += 4) {
      assert.equal(pixels[o + 3], 255, "alpha is untouched");
      for (let k = 0; k < 3; k++) widest = Math.max(widest, Math.abs(pixels[o + k] - 128));
    }
    return widest;
  };
  assert.ok(spread(first) > 0 && spread(first) <= 19, `silk ${spread(first)}`);
  assert.ok(spread(grain("reed", 16, 12)) > 0 && spread(grain("reed", 16, 12)) <= 11, "reed");
});

test("contour draws the accent on the edges between height bands over the theme background", () => {
  const colors = { ...COLORS, bg: "#102030", accent: "#a0b0c0" };
  // Left half is black (band 0), right half a light grey (a higher band): one edge between them.
  const c = fakeCanvas((x) => (x < 8 ? [0, 0, 0] : [200, 200, 200]));
  drawBackdrop(c.canvas, "contour", 16, 2, colors);
  const [pixels] = c.writes;
  const at = (x: number, y: number): number[] => [...pixels.slice((y * 16 + x) * 4, (y * 16 + x) * 4 + 3)];
  for (const y of [0, 1]) {
    assert.deepEqual(at(0, y), [0x10, 0x20, 0x30], "flat band 0 is the plain background");
    assert.deepEqual(at(6, y), [0x10, 0x20, 0x30]);
    assert.deepEqual(at(7, y), [0x58, 0x68, 0x78], "the edge is halfway to the accent");
    const faint = at(12, y);
    [0x10, 0x20, 0x30].forEach((bg, k) => assert.ok(faint[k] > bg && faint[k] < [0x58, 0x68, 0x78][k], `band tint ${k}`));
  }
  assert.equal(pixels[3], 255, "alpha is untouched");
});
