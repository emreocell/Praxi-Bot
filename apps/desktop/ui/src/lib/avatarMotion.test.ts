import assert from "node:assert/strict";
import { existsSync, statSync } from "node:fs";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import {
  createAvatarLoop,
  omegaForSettle,
  springSettled,
  springStep,
  type LoopScheduler,
  type SpringState
} from "../../../../../packages/ui/src/avatarSpring.ts";
import {
  RENDER_FOR_SHAPE,
  RENDER_NAMES,
  renderLayout,
  renderTint
} from "../../../../../packages/ui/src/avatarRender.ts";
import { BOT_SHAPE_IDS, CHARACTER_COLORS } from "../../../../../packages/ui/src/botShape.ts";

const run = (s: SpringState, target: number, omega: number, dt: number, steps: number) => {
  const xs: number[] = [];
  for (let i = 0; i < steps; i += 1) xs.push(springStep(s, target, omega, dt).x);
  return xs;
};

test("gaze spring converges monotonically, never overshoots and settles in ~180-260 ms", () => {
  const omega = omegaForSettle(220);
  const s = { x: -30, v: 0 };
  const xs = run(s, 25, omega, 1 / 60, 60);
  for (let i = 1; i < xs.length; i += 1) {
    assert.ok(xs[i] >= xs[i - 1], `monotonic at ${i}`);
    assert.ok(xs[i] <= 25, `no overshoot at ${i}`);
  }
  // Within 2 % of the 55 degree sweep by ~240 ms, but not yet there after one frame (no snap).
  assert.ok(Math.abs(xs[14] - 25) < 55 * 0.03, `near target at 250 ms: ${xs[14]}`);
  assert.ok(xs[0] < -25, `first frame only starts the move: ${xs[0]}`);
});

test("spring stepping is frame-rate independent", () => {
  const omega = omegaForSettle(240);
  const a = { x: 0, v: 0 };
  const b = { x: 0, v: 0 };
  run(a, 10, omega, 1 / 30, 9); // 30 fps for 300 ms
  run(b, 10, omega, 1 / 120, 36); // 120 fps for 300 ms
  assert.ok(Math.abs(a.x - b.x) < 1e-9, `${a.x} vs ${b.x}`);
  assert.ok(Math.abs(a.v - b.v) < 1e-9);
});

test("a retarget while moving fast cannot overshoot the new target", () => {
  const omega = omegaForSettle(200);
  const s = { x: 0, v: 0 };
  run(s, 40, omega, 1 / 60, 4); // racing toward 40
  assert.ok(s.v > 50, "moving fast");
  const xs = run(s, s.x + 1, omega, 1 / 60, 30); // pointer stops just ahead
  const target = xs.length ? xs[xs.length - 1] : s.x;
  for (const x of xs) assert.ok(x <= target + 1e-9, `overshoot ${x} > ${target}`);
  assert.ok(springSettled(s, target));
});

test("the shared loop runs only while something moves, and sleeps when settled or hidden", () => {
  let now = 0;
  let pending: ((t: number) => void) | null = null;
  let requests = 0;
  let visible = true;
  const scheduler: LoopScheduler = {
    request: (cb) => {
      requests += 1;
      pending = cb;
      return requests;
    },
    cancel: () => {
      pending = null;
    },
    now: () => now,
    visible: () => visible
  };
  const loop = createAvatarLoop(scheduler);
  const frame = () => {
    const cb = pending;
    pending = null;
    now += 16;
    cb?.(now);
  };
  const subjects = Array.from({ length: 20 }, (_, i) => {
    const s = { x: 0, v: 0 };
    const target = 10 + i;
    const omega = omegaForSettle(200);
    return {
      steps: 0,
      step(dt: number) {
        this.steps += 1;
        springStep(s, target, omega, dt);
        return springSettled(s, target);
      }
    };
  });
  assert.equal(loop.running, false, "idle page requests no frames");
  for (const s of subjects) loop.wake(s);
  assert.equal(requests, 1, "twenty avatars share one frame request");
  for (let i = 0; i < 60 && loop.running; i += 1) frame();
  assert.equal(loop.activeCount, 0, "everything settled");
  assert.equal(loop.running, false, "loop sleeps once settled");
  const before = requests;
  frame();
  assert.equal(requests, before, "no further frames while settled");

  // Hidden window: a wake does not schedule frames until resumed.
  visible = false;
  loop.wake(subjects[0]);
  assert.equal(loop.running, false);
  visible = true;
  loop.resume();
  assert.equal(loop.running, true);
});

test("every persisted shape id has a render, and every render file is bundled", () => {
  assert.deepEqual(Object.keys(RENDER_FOR_SHAPE).sort(), [...BOT_SHAPE_IDS].sort());
  for (const id of BOT_SHAPE_IDS) assert.ok(RENDER_NAMES.includes(RENDER_FOR_SHAPE[id]), id);
  assert.equal(new Set(Object.values(RENDER_FOR_SHAPE)).size, 16, "each shape has its own body");
  for (const name of RENDER_NAMES) {
    const file = fileURLToPath(new URL(`../../../../../packages/ui/src/assets/renders/${name}.webp`, import.meta.url));
    assert.ok(existsSync(file), `${name}.webp exists`);
    assert.ok(statSync(file).size < 40_000, `${name}.webp stays small`);
  }
});

test("render layouts rest on the ground and keep both eyes inside the body", () => {
  for (const id of BOT_SHAPE_IDS) {
    const l = renderLayout(id);
    assert.ok(Math.abs(l.maxY - 55.5) < 0.05, `${id} rests on the ground`);
    assert.ok(l.maxX - l.minX <= 50.01 && l.maxY - l.minY <= 48.01, `${id} fits the box`);
    for (const x of [l.leftEyeX, l.rightEyeX]) assert.ok(x > l.minX + 2 && x < l.maxX - 2, `${id} eye x`);
    assert.ok(l.eyeY > l.minY + 4 && l.eyeY < l.maxY - 4, `${id} eye y`);
  }
});

test("render tint: white keeps white clay, dark picks are lifted, every swatch stays a colour", () => {
  assert.equal(renderTint("#f4f4f5").multiply, "#ffffff", "white swatch leaves the clay as rendered");
  const dark = renderTint("#0a0f1e");
  const lum = (hex: string) => [1, 3, 5].reduce((a, i) => a + parseInt(hex.slice(i, i + 2), 16), 0) / 765;
  assert.ok(lum(dark.body) > 0.3, `dark tint lifted to clay: ${dark.body}`);
  assert.ok(dark.liftOpacity > 0);
  for (const c of CHARACTER_COLORS) {
    const t = renderTint(c);
    assert.match(t.multiply, /^#[0-9a-f]{6}$/);
    assert.ok(t.glow >= 0 && t.glow <= 0.24);
  }
});
