import assert from "node:assert/strict";
import { test } from "node:test";
import { mapClientPointToNormalized } from "./liveViewInput.ts";
import {
  ACTUAL_SCALE,
  atZoomLimit,
  FIT_SCALE,
  fitFactor,
  isActualSize,
  remoteSize,
  resolutionLabel,
  stageGeometry,
  stepZoom,
  type ViewScale,
  zoomPercent
} from "./liveViewScale.ts";

const near = (actual: number, expected: number, eps = 1e-9) =>
  assert.ok(Math.abs(actual - expected) <= eps, `${actual} !~ ${expected}`);

const frame1280 = { width: 1280, height: 800, sourceRect: { width: 1280, height: 800 } };
// The same 1280x800 display streamed at the saver profile.
const frame960 = { width: 960, height: 600, sourceRect: { width: 1280, height: 800 } };

test("the label and 1:1 base are the remote display's size, whatever the stream size is (#895)", () => {
  assert.equal(resolutionLabel(frame1280), "1280×800");
  assert.equal(resolutionLabel(frame960), "1280×800");
  assert.equal(resolutionLabel({ width: 960, height: 600 }), "960×600");
  assert.deepEqual(remoteSize(frame1280), { width: 1280, height: 800 });
  assert.deepEqual(remoteSize(frame960), { width: 1280, height: 800 });
  assert.deepEqual(remoteSize({ width: 960, height: 600, sourceRect: { width: 0, height: 0 } }), {
    width: 960,
    height: 600
  });
  assert.equal(remoteSize({ width: 0, height: 600 }), null);
});

test("fit shows the whole screen at any window size, aspect kept, never cropped", () => {
  const content = { width: 1280, height: 800 };
  for (const area of [
    { width: 1920, height: 1080 },
    { width: 600, height: 900 },
    { width: 1000, height: 300 },
    { width: 3000, height: 3000 },
    { width: 320, height: 200 }
  ]) {
    const g = stageGeometry(FIT_SCALE, area, content);
    assert.ok(g);
    assert.ok(g.width <= area.width + 1e-9 && g.height <= area.height + 1e-9, `inside ${area.width}x${area.height}`);
    near(g.width / g.height, 1.6);
    assert.equal(g.scrolls, false);
    // One side touches the area: the picture is as large as it can be.
    assert.ok(Math.abs(g.width - area.width) < 1e-6 || Math.abs(g.height - area.height) < 1e-6);
  }
});

test("fit letterboxes a wide window and a tall window on the matching axis", () => {
  const content = { width: 1280, height: 800 };
  const wide = stageGeometry(FIT_SCALE, { width: 2000, height: 800 }, content);
  assert.deepEqual([wide?.width, wide?.height], [1280, 800]);
  const tall = stageGeometry(FIT_SCALE, { width: 640, height: 900 }, content);
  assert.deepEqual([tall?.width, tall?.height], [640, 400]);
  near(fitFactor({ width: 640, height: 900 }, content), 0.5);
});

test("the stream's own size does not change the fitted picture", () => {
  const area = { width: 1100, height: 700 };
  const a = stageGeometry(FIT_SCALE, area, remoteSize(frame1280));
  const b = stageGeometry(FIT_SCALE, area, remoteSize(frame960));
  assert.deepEqual(a, b);
});

test("1:1 is one remote pixel per device pixel and scrolls when larger than the area", () => {
  const content = { width: 1280, height: 800 };
  const big = stageGeometry(ACTUAL_SCALE, { width: 900, height: 600 }, content, 1);
  assert.deepEqual([big?.width, big?.height, big?.scrolls], [1280, 800, true]);
  const roomy = stageGeometry(ACTUAL_SCALE, { width: 2000, height: 1200 }, content, 1);
  assert.deepEqual([roomy?.width, roomy?.height, roomy?.scrolls], [1280, 800, false]);
  // On a 2x display one remote pixel is one device pixel: half a CSS pixel.
  const hidpi = stageGeometry(ACTUAL_SCALE, { width: 900, height: 600 }, content, 2);
  assert.deepEqual([hidpi?.width, hidpi?.height, hidpi?.scrolls], [640, 400, false]);
});

test("geometry waits for a measured area and a known frame", () => {
  assert.equal(stageGeometry(FIT_SCALE, { width: 0, height: 0 }, { width: 10, height: 10 }), null);
  assert.equal(stageGeometry(FIT_SCALE, { width: 100, height: 100 }, null), null);
  assert.equal(stageGeometry(FIT_SCALE, { width: Number.NaN, height: 100 }, { width: 10, height: 10 }), null);
});

test("zoom steps move up and down through the levels and clamp at the ends", () => {
  const area = { width: 1000, height: 500 };
  const content = { width: 1280, height: 800 };
  // Fit shows it at 0.625 css px per pixel on a 1x display: the next level up is 0.75, down is 0.5.
  assert.deepEqual(stepZoom(FIT_SCALE, 1, area, content, 1), { kind: "zoom", factor: 0.75 });
  assert.deepEqual(stepZoom(FIT_SCALE, -1, area, content, 1), { kind: "zoom", factor: 0.5 });
  let s: ViewScale = ACTUAL_SCALE;
  s = stepZoom(s, 1, area, content);
  assert.deepEqual(s, { kind: "zoom", factor: 1.5 });
  s = stepZoom(stepZoom(stepZoom(s, 1, area, content), 1, area, content), 1, area, content);
  assert.deepEqual(s, { kind: "zoom", factor: 4 });
  assert.deepEqual(stepZoom(s, 1, area, content), { kind: "zoom", factor: 4 });
  assert.deepEqual(stepZoom({ kind: "zoom", factor: 0.5 }, -1, area, content), { kind: "zoom", factor: 0.5 });
  assert.ok(isActualSize(ACTUAL_SCALE) && !isActualSize(FIT_SCALE) && !isActualSize({ kind: "zoom", factor: 2 }));
});

test("stepping from fit on a 2x display counts device pixels, so the first press always changes the size", () => {
  const content = { width: 1280, height: 800 };
  const area = { width: 1500, height: 700 };
  // Fit shows 0.875 css px per remote pixel = 1.75 device pixels per remote pixel on a 2x display.
  assert.deepEqual(stepZoom(FIT_SCALE, 1, area, content, 2), { kind: "zoom", factor: 2 });
  assert.deepEqual(stepZoom(FIT_SCALE, -1, area, content, 2), { kind: "zoom", factor: 1.5 });
  assert.equal(zoomPercent(FIT_SCALE, area, content, 2), 175);
  assert.equal(zoomPercent(FIT_SCALE, area, content, 1), 88);
});

test("an unmeasured area (the first frame, before the viewer is measured) steps from 1:1 on any display", () => {
  const content = { width: 1280, height: 800 };
  const unmeasured = { width: 0, height: 0 };
  for (const dpr of [1, 2, 1.5]) {
    assert.deepEqual(stepZoom(FIT_SCALE, 1, unmeasured, content, dpr), { kind: "zoom", factor: 1.5 });
    assert.deepEqual(stepZoom(FIT_SCALE, -1, unmeasured, content, dpr), { kind: "zoom", factor: 0.75 });
  }
  assert.deepEqual(stepZoom(FIT_SCALE, 1, unmeasured, null, 2), { kind: "zoom", factor: 1.5 });
  // Nonsense device ratios fall back to 1 instead of poisoning the step.
  for (const dpr of [0, -1, Number.NaN, Number.POSITIVE_INFINITY]) {
    assert.deepEqual(stepZoom(FIT_SCALE, 1, { width: 1280, height: 800 }, content, dpr), { kind: "zoom", factor: 1.5 });
  }
  assert.equal(zoomPercent(FIT_SCALE, unmeasured, content, 2), null);
  assert.equal(zoomPercent(FIT_SCALE, { width: 1500, height: 700 }, null, 2), null);
});

test("the zoom buttons know when the first or last level is reached", () => {
  const area = { width: 1500, height: 700 };
  const content = { width: 1280, height: 800 };
  assert.equal(atZoomLimit(FIT_SCALE, 1, area, content), false);
  assert.equal(atZoomLimit(FIT_SCALE, -1, area, content), false);
  assert.equal(atZoomLimit({ kind: "zoom", factor: 4 }, 1, area, content), true);
  assert.equal(atZoomLimit({ kind: "zoom", factor: 4 }, -1, area, content), false);
  assert.equal(atZoomLimit({ kind: "zoom", factor: 0.5 }, -1, area, content), true);
  assert.equal(atZoomLimit({ kind: "zoom", factor: 0.5 }, 1, area, content), false);
  assert.equal(zoomPercent({ kind: "zoom", factor: 1.5 }, area, content, 2), 150);
});

/** A picture element laid out at `geometry` inside a viewport at (vx, vy), scrolled by (sx, sy). */
function pictureBox(
  geometry: { width: number; height: number },
  viewport: { x: number; y: number; width: number; height: number },
  scroll = { x: 0, y: 0 }
) {
  // `margin: auto` centres a picture smaller than the viewport; a larger one starts at the corner.
  const offsetX = Math.max(0, (viewport.width - geometry.width) / 2);
  const offsetY = Math.max(0, (viewport.height - geometry.height) / 2);
  return {
    left: viewport.x + offsetX - scroll.x,
    top: viewport.y + offsetY - scroll.y,
    width: geometry.width,
    height: geometry.height
  };
}

test("pointer mapping reaches every corner and the centre of the remote screen in fit mode", () => {
  const content = { width: 1280, height: 800 };
  for (const viewport of [
    { x: 40, y: 80, width: 1500, height: 700 },
    { x: 0, y: 0, width: 500, height: 900 }
  ]) {
    const g = stageGeometry(FIT_SCALE, viewport, content);
    assert.ok(g);
    const box = pictureBox(g, viewport);
    const at = (fx: number, fy: number) =>
      mapClientPointToNormalized(box.left + fx * box.width, box.top + fy * box.height, box, 960, 600);
    for (const [fx, fy] of [[0, 0], [1, 0], [0, 1], [1, 1]] as const) {
      const corner = at(fx, fy);
      assert.ok(corner, `corner ${fx},${fy} is on the screen`);
      near(corner.x, fx, 1e-9);
      near(corner.y, fy, 1e-9);
    }
    const mid = at(0.5, 0.5);
    assert.ok(mid);
    near(mid.x, 0.5);
    near(mid.y, 0.5);
    // The letterbox bars are not part of the remote screen.
    assert.equal(mapClientPointToNormalized(box.left - 1, box.top + 10, box, 960, 600), null);
    assert.equal(mapClientPointToNormalized(box.left + 10, box.top + box.height + 1, box, 960, 600), null);
  }
});

test("a click on a remote pixel lands on that pixel at 1:1, including when scrolled", () => {
  const content = { width: 1280, height: 800 };
  const viewport = { x: 10, y: 50, width: 900, height: 600 };
  const g = stageGeometry(ACTUAL_SCALE, viewport, content, 1);
  assert.ok(g);
  const scroll = { x: 300, y: 120 };
  const box = pictureBox(g, viewport, scroll);
  // The viewport's top-left pixel shows remote pixel (300, 120).
  const p = mapClientPointToNormalized(viewport.x, viewport.y, box, 960, 600);
  assert.ok(p);
  near(p.x * 1280, 300);
  near(p.y * 800, 120);
  // Remote pixel (640, 400) sits at the viewport offset (340, 280) from there.
  const q = mapClientPointToNormalized(viewport.x + 340, viewport.y + 280, box, 960, 600);
  assert.ok(q);
  near(q.x * 1280, 640);
  near(q.y * 800, 400);
});

test("mapping is the same in fit and zoom for the same remote point, and a drag outside clamps", () => {
  const content = { width: 1280, height: 800 };
  const viewport = { x: 0, y: 0, width: 800, height: 500 };
  const target = { x: 0.25, y: 0.75 };
  for (const scale of [FIT_SCALE, ACTUAL_SCALE, { kind: "zoom", factor: 2 } as ViewScale]) {
    const g = stageGeometry(scale, viewport, content, 1);
    assert.ok(g);
    const box = pictureBox(g, viewport);
    const p = mapClientPointToNormalized(
      box.left + target.x * box.width,
      box.top + target.y * box.height,
      box,
      960,
      600
    );
    assert.ok(p);
    near(p.x, target.x);
    near(p.y, target.y);
    assert.deepEqual(
      mapClientPointToNormalized(box.left - 500, box.top + box.height + 500, box, 960, 600, { clamp: true }),
      { x: 0, y: 1 }
    );
  }
});
