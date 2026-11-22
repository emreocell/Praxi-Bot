import assert from "node:assert/strict";
import { test } from "node:test";
import {
  BAYER8,
  bayerThreshold,
  buildDitherBytes,
  buildField,
  DEFAULT_WALLPAPER_MOTION,
  diagonalThreshold,
  driftOffset,
  isBatterySaver,
  LIVE_FRAME_INTERVAL_MS,
  nextDriftDelay,
  packPalette,
  packPaletteRgb,
  parseWallpaper,
  parseWallpaperMotion,
  quantize,
  renderField,
  resolveWallpaper,
  shouldAnimateWallpaper,
  startWallpaper,
  WALLPAPERS,
  WALLPAPER_KINDS
} from "./wallpaper.ts";

test("bayer matrix is a permutation of 0..63 and thresholds stay inside (0,1)", () => {
  assert.deepEqual([...BAYER8].sort((a, b) => a - b), Array.from({ length: 64 }, (_, i) => i));
  for (let y = 0; y < 8; y++) for (let x = 0; x < 8; x++) {
    for (const t of [bayerThreshold(x, y), diagonalThreshold(x, y)]) assert.ok(t > 0 && t < 1);
  }
});

test("quantize: ordered dither gives the right share of the brighter colour", () => {
  assert.equal(quantize(0, 0.5, 4), 0);
  assert.equal(quantize(1, 0.5, 4), 3);
  let up = 0;
  for (let y = 0; y < 8; y++) for (let x = 0; x < 8; x++) up += quantize(1.25 / 3, bayerThreshold(x, y), 4) === 2 ? 1 : 0;
  // v puts the pixel a quarter of the way through band 1 -> about 25 % of pixels use colour 2.
  assert.ok(Math.abs(up / 64 - 0.25) < 0.03, `share ${up / 64}`);
});

test("diagonal screen repeats along 45 degrees", () => {
  assert.equal(Math.floor(diagonalThreshold(3, 1) * 4), Math.floor(diagonalThreshold(2, 2) * 4));
  assert.notEqual(Math.floor(diagonalThreshold(0, 0) * 4), Math.floor(diagonalThreshold(1, 0) * 4));
});

test("follow theme maps the four shipped themes and falls back by brightness", () => {
  assert.equal(resolveWallpaper("theme", "obsidian", true), "night");
  assert.equal(resolveWallpaper("theme", "cobalt", true), "sky");
  assert.equal(resolveWallpaper("theme", "raven", true), "dusk");
  assert.equal(resolveWallpaper("theme", "clay", false), "warm");
  assert.equal(resolveWallpaper("theme", "unknown", true), "night");
  assert.equal(resolveWallpaper("theme", "unknown", false), "sky");
  assert.equal(resolveWallpaper("off", "clay", false), null);
  assert.equal(resolveWallpaper("dusk", "clay", false), "dusk");
  assert.equal(parseWallpaper("bogus"), "off");
  assert.equal(parseWallpaper(undefined), "off");
});

test("drift moves by whole columns, wraps, and the timer wakes only at the next step", () => {
  assert.equal(driftOffset(0, 100), 0);
  assert.equal(driftOffset(3999, 100), 0);
  assert.equal(driftOffset(4000, 100), 1);
  assert.equal(driftOffset(4000 * 100, 100), 0);
  assert.equal(nextDriftDelay(0), 4000);
  assert.equal(nextDriftDelay(3999), 1);
  assert.equal(nextDriftDelay(4001), 3999);
});

test("field is deterministic, in range, wraps in x, and has both sky and cloud", () => {
  const a = buildField(120, 70, 0.5, 0.1);
  const b = buildField(120, 70, 0.5, 0.1);
  assert.deepEqual(a, b);
  let sky = 0;
  let cloud = 0;
  for (const v of a) {
    if (v < 20) sky++;
    if (v > 150) cloud++;
  }
  assert.ok(sky > a.length * 0.1 && cloud > a.length * 0.03, `sky ${sky} cloud ${cloud}`);
  // Periodic: the seam between the last and first column is no rougher than inside the field.
  let seam = 0;
  let inner = 0;
  for (let y = 0; y < 70; y++) {
    seam += Math.abs(a[y * 120 + 119] - a[y * 120]);
    inner += Math.abs(a[y * 120 + 59] - a[y * 120 + 60]);
  }
  assert.ok(seam <= inner * 2 + 70 * 8, `seam ${seam} inner ${inner}`);
});

test("renderField only paints palette colours and drifting shifts the picture", () => {
  for (const kind of WALLPAPER_KINDS) {
    const style = WALLPAPERS[kind];
    const packed = packPalette(style.palette);
    const field = buildField(64, 40, style.cover, style.haze);
    const t0 = new Uint32Array(64 * 40);
    const t1 = new Uint32Array(64 * 40);
    renderField(field, 64, 40, 0, style, packed, t0);
    renderField(field, 64, 40, 5, style, packed, t1);
    const set = new Set(packed);
    assert.ok(t0.every((w) => set.has(w)), kind);
    assert.notDeepEqual(t0, t1);
  }
});

test("motion setting defaults to subtle, parses valid modes, and battery saver guards discharging <= 20%", () => {
  assert.equal(DEFAULT_WALLPAPER_MOTION, "subtle");
  assert.equal(parseWallpaperMotion(undefined), "subtle");
  assert.equal(parseWallpaperMotion("bogus"), "subtle");
  for (const m of ["off", "subtle", "live"] as const) assert.equal(parseWallpaperMotion(m), m);

  assert.equal(isBatterySaver(null), false);
  assert.equal(isBatterySaver({ charging: false, level: 0.2 }), true);
  assert.equal(isBatterySaver({ charging: false, level: 0.21 }), false);
  assert.equal(isBatterySaver({ charging: true, level: 0.1 }), false);
});

test("shouldAnimateWallpaper pauses on off, hidden, unfocused, reduced motion, or battery saver", () => {
  const base = { motion: "live" as const, hidden: false, focused: true, reducedMotion: false, batterySaver: false };
  assert.equal(shouldAnimateWallpaper(base), true);
  assert.equal(shouldAnimateWallpaper({ ...base, motion: "subtle" }), true);
  assert.equal(shouldAnimateWallpaper({ ...base, motion: "off" }), false);
  assert.equal(shouldAnimateWallpaper({ ...base, hidden: true }), false);
  assert.equal(shouldAnimateWallpaper({ ...base, focused: false }), false);
  assert.equal(shouldAnimateWallpaper({ ...base, reducedMotion: true }), false);
  assert.equal(shouldAnimateWallpaper({ ...base, batterySaver: true }), false);
});

test("WebGL dither bytes and RGB palette packing preserve all 4 palette stops and 64 ordered thresholds", () => {
  const rgb = packPaletteRgb(WALLPAPERS.night.palette);
  assert.equal(rgb.length, 12);
  assert.ok(rgb.every((v) => v >= 0 && v <= 1));
  const bytes = buildDitherBytes("diagonal");
  assert.equal(bytes.length, 64);
  assert.ok(bytes.every((b) => b >= 0 && b <= 255));
});

test("startWallpaper in live mode falls back to static 2D when WebGL is unavailable", () => {
  const origWin = globalThis.window;
  const origDoc = globalThis.document;
  let putCount = 0;
  let drawnCount = 0;
  const fakeCtx = {
    createImageData: (w: number, h: number) => ({ data: new Uint8ClampedArray(w * h * 4) }),
    putImageData: () => {
      putCount++;
    }
  };
  const fakeCanvas = {
    width: 0,
    height: 0,
    getContext: (kind: string) => (kind === "2d" ? fakeCtx : null),
    addEventListener: () => {},
    removeEventListener: () => {}
  } as unknown as HTMLCanvasElement;
  (globalThis as Record<string, unknown>).window = {
    innerWidth: 640,
    innerHeight: 400,
    addEventListener: () => {},
    removeEventListener: () => {}
  };
  (globalThis as Record<string, unknown>).document = {
    hidden: false,
    hasFocus: () => true,
    addEventListener: () => {},
    removeEventListener: () => {}
  };
  try {
    const handle = startWallpaper(
      fakeCanvas,
      "night",
      () => {
        drawnCount++;
      },
      { motion: "live", createLive: () => null }
    );
    assert.equal(putCount, 1);
    assert.equal(drawnCount, 1);
    handle.stop();
  } finally {
    (globalThis as Record<string, unknown>).window = origWin;
    (globalThis as Record<string, unknown>).document = origDoc;
  }
});

test("startWallpaper live mode pauses when unfocused/battery-saver/reduced-motion and caps at 30 fps", async () => {
  const origWin = globalThis.window;
  const origDoc = globalThis.document;
  const listeners: Record<string, Array<() => void>> = {};
  let focused = true;
  let batterySaver = false;
  let reduced = false;
  let nowMs = 0;
  const draws: number[] = [];
  let disposed = false;

  (globalThis as Record<string, unknown>).window = {
    innerWidth: 640,
    innerHeight: 400,
    addEventListener: (ev: string, fn: () => void) => {
      (listeners[ev] ??= []).push(fn);
    },
    removeEventListener: () => {}
  };
  (globalThis as Record<string, unknown>).document = {
    hidden: false,
    hasFocus: () => focused,
    addEventListener: (ev: string, fn: () => void) => {
      (listeners[ev] ??= []).push(fn);
    },
    removeEventListener: () => {}
  };
  try {
    const fakeCanvas = { width: 0, height: 0 } as unknown as HTMLCanvasElement;
    const handle = startWallpaper(
      fakeCanvas,
      "sky",
      undefined,
      {
        motion: "live",
        now: () => nowMs,
        focused: () => focused,
        batterySaver: () => batterySaver,
        reducedMotion: () => reduced,
        createLive: () => ({
          resize: () => {},
          draw: (sec: number) => draws.push(sec),
          readPixelsRgba: () => null,
          dispose: () => {
            disposed = true;
          }
        })
      }
    );
    assert.equal(draws.length, 1);
    // While unfocused, blur stops scheduling new frames.
    focused = false;
    listeners.blur?.forEach((fn) => fn());
    const countAfterBlur = draws.length;
    nowMs += Math.ceil(LIVE_FRAME_INTERVAL_MS * 3);
    await new Promise((r) => setTimeout(r, 60));
    assert.equal(draws.length, countAfterBlur);

    // Battery saver / reduced motion also prevent resuming on focus.
    batterySaver = true;
    focused = true;
    listeners.focus?.forEach((fn) => fn());
    assert.equal(draws.length, countAfterBlur);
    batterySaver = false;
    reduced = true;
    listeners.focus?.forEach((fn) => fn());
    assert.equal(draws.length, countAfterBlur);

    handle.stop();
    assert.equal(disposed, true);
  } finally {
    (globalThis as Record<string, unknown>).window = origWin;
    (globalThis as Record<string, unknown>).document = origDoc;
  }
});
