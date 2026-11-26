import assert from "node:assert/strict";
import { test } from "node:test";
import { cardPanelShown, deriveView, formatLive, IDLE_PAUSE_MS, measureFps, SCREEN_START_TIMEOUT_MS, type ViewInputs } from "./computerView.ts";
import { gradientBase, gradientVars, hexToOklch, NEUTRAL_CHROMA } from "./botGradient.ts";

const base: ViewInputs = {
  wanted: false,
  pausedIdle: false,
  issue: null,
  targetsStatus: "unknown",
  hasTarget: false,
  targetUnavailable: null,
  startFailure: null,
  noScreen: null,
  targetsError: null,
  hasFrame: false
};

test("the computer card never starts a view by itself: idle until asked, then starting, live, paused", () => {
  assert.equal(deriveView(base).kind, "idle");
  // a panel that merely opens (targets known, a frame exists elsewhere) is still idle
  assert.equal(deriveView({ ...base, targetsStatus: "ok", hasTarget: true, hasFrame: true }).kind, "idle");
  assert.equal(deriveView({ ...base, wanted: true }).kind, "starting");
  assert.equal(deriveView({ ...base, wanted: true, targetsStatus: "ok", hasTarget: true, hasFrame: true }).kind, "live");
  // the quiet-time timer ended the view
  const paused = deriveView({ ...base, pausedIdle: true });
  assert.equal(paused.kind, "paused");
  assert.equal(paused.paused, "idle");
  assert.equal(IDLE_PAUSE_MS, 180_000);
});

test("a card start resolves to a frame, an unavailable reason, or a retryable timeout", () => {
  assert.equal(SCREEN_START_TIMEOUT_MS, 6_000);
  const starting: ViewInputs = { ...base, wanted: true, hasTarget: true, targetsStatus: "ok" };
  assert.equal(deriveView(starting).kind, "starting");
  assert.equal(deriveView({ ...starting, hasFrame: true }).kind, "live");
  assert.deepEqual(deriveView({ ...starting, startFailure: "permission" }), {
    kind: "unavailable", unavailable: "permission", paused: null
  });
  assert.deepEqual(deriveView({ ...starting, startFailure: "timeout" }), {
    kind: "timeout", unavailable: "timeout", paused: null
  });
  assert.equal(deriveView({ ...starting, startFailure: null }).kind, "starting");
});

test("an unreachable server or a refused screen is 'unavailable' with a reason, not a spinner", () => {
  assert.deepEqual(deriveView({ ...base, issue: "offline" }), { kind: "unavailable", unavailable: "offline", paused: null });
  assert.equal(deriveView({ ...base, issue: "connecting" }).kind, "idle");
  assert.equal(deriveView({ ...base, issue: "connecting", wanted: true }).unavailable, "connecting");
  assert.equal(deriveView({ ...base, wanted: true, targetsError: "forbidden", targetsStatus: "failed" }).unavailable, "permission");
  assert.deepEqual(deriveView({ ...base, wanted: true, targetsStatus: "ok", noScreen: "no_display" }).unavailable, { noScreen: "no_display" });
  assert.deepEqual(
    deriveView({ ...base, wanted: true, targetsError: "network", targetsStatus: "failed" }).unavailable,
    { streamError: "network" }
  );
  assert.equal(formatLive(42_000), "0:42");
  assert.equal(measureFps([0, 500, 1000, 1500, 2000, 2500], 2500), 2);
  assert.equal(measureFps([2400], 2500), null);
});

test("the gradient keeps a white bot tinted and mid-light, so it never washes out", () => {
  const white = hexToOklch("#f4f4f5");
  assert.ok(white && white.c < NEUTRAL_CHROMA && white.l > 0.9);
  const base = gradientBase("#f4f4f5");
  assert.ok(base.c >= 0.04, "a neutral colour is given a tint");
  assert.ok(base.l <= 0.82 && base.l >= 0.5, "lightness stays in the middle band");
  const dark = gradientBase("#000000");
  assert.ok(dark.l >= 0.5);
  // a chromatic colour keeps its hue
  const blue = gradientBase("#3b82f6");
  assert.ok(Math.abs(blue.h - hexToOklch("#3b82f6")!.h) < 0.01);

  // light-palette variant keeps lightness within 0.42..0.62 and chroma >= 0.1
  const lightWhite = gradientBase("#ffffff", true);
  assert.ok(lightWhite.l >= 0.42 && lightWhite.l <= 0.62, "white on light palette stays in light band");
  assert.ok(lightWhite.c >= 0.1, "white on light palette has sufficient chroma");

  const lightDark = gradientBase("#000000", true);
  assert.ok(lightDark.l >= 0.42 && lightDark.l <= 0.62, "black on light palette stays in light band");
  assert.ok(lightDark.c >= 0.1, "black on light palette has sufficient chroma");

  const lightBlue = gradientBase("#3b82f6", true);
  assert.ok(Math.abs(lightBlue.h - hexToOklch("#3b82f6")!.h) < 0.01, "chromatic colour keeps its hue on light palette");

  const vars = gradientVars("#8b9cff");
  assert.match(vars["--bot"], /^oklch\(/);
  assert.match(vars["--bot2"], /calc\(h \+ 48\)/);
  assert.match(vars["--bot3"], /calc\(l - \.22\)/);
});

test("the viewer hiding the bot panel does not count as the owner closing it (the card keeps its state)", () => {
  assert.equal(cardPanelShown(true, false, false), true, "panel open");
  assert.equal(cardPanelShown(false, false, false), false, "panel closed by the owner: the view ends");
  assert.equal(cardPanelShown(false, true, false), true, "viewer open, panel stepped aside");
  assert.equal(cardPanelShown(false, false, true), true, "viewer just closed, panel not back yet");
  assert.equal(cardPanelShown(true, true, true), true);
});
