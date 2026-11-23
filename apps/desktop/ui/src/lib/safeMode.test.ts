import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import type { Health } from "../api/types";
import { safeModeView } from "./safeMode.ts";

const health = (safe: boolean) => ({ safe_mode: safe }) as Health;

test("safe mode is read from the daemon's health, never guessed", () => {
  assert.equal(safeModeView(null, { started_by_app: true }).active, false);
  assert.equal(safeModeView(health(false), { started_by_app: true }).active, false);
  assert.equal(safeModeView(health(true), { started_by_app: true }).active, true);
  // A daemon that predates the field is not in safe mode.
  assert.equal(safeModeView({} as Health, { started_by_app: true }).active, false);
});

test("the owner can leave or enter only a service the app started", () => {
  assert.deepEqual(safeModeView(health(true), { started_by_app: true }), {
    active: true,
    canLeave: true,
    canEnter: true
  });
  const foreign = safeModeView(health(true), { started_by_app: false });
  assert.equal(foreign.canLeave, false);
  assert.equal(foreign.canEnter, false);
});

test("banner shows only in safe mode and its button restarts normally; entry points ask for safe", () => {
  const banner = readFileSync(new URL("../components/SafeModeBanner.tsx", import.meta.url), "utf8");
  assert.match(banner, /if \(!view\.active\) \{\s*return null;/);
  assert.match(banner, /restartDaemon\(false,/);
  assert.match(banner, /data-testid="safe-mode-banner"/);
  const startup = readFileSync(new URL("../components/StartupProblem.tsx", import.meta.url), "utf8");
  assert.match(startup, /restartDaemon\(true,/);
  const card = readFileSync(new URL("../views/settings/SafeModeCard.tsx", import.meta.url), "utf8");
  assert.match(card, /restartDaemon\(!view\.active,/);
  const app = readFileSync(new URL("../App.tsx", import.meta.url), "utf8");
  assert.match(app, /<SafeModeBanner \/>/);
});
