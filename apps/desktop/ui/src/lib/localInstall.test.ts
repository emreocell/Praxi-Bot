import assert from "node:assert/strict";
import test from "node:test";
import { canInstall, isBusy, percent, phaseKey, sizeLabel } from "./localInstall.ts";

const state = (phase: string, extra: object = {}) =>
  ({ phase, model_id: null, bytes_done: 0, bytes_total: 0, error: null, ...extra }) as never;

test("a failed install names its reason in plain words, never a raw code", () => {
  assert.equal(phaseKey(state("failed", { error: "disk" })), "errorDisk");
  assert.equal(phaseKey(state("failed", { error: "port_busy" })), "errorPortBusy");
  assert.equal(phaseKey(state("failed", { error: null })), "errorOther");
  assert.equal(phaseKey(state("idle")), null);
});

test("progress is clamped and polling stops when nothing runs", () => {
  assert.equal(percent(state("downloading_model", { bytes_done: 50, bytes_total: 200 })), 25);
  assert.equal(percent(state("downloading_model", { bytes_done: 900, bytes_total: 200 })), 100);
  assert.equal(percent(state("idle")), 0);
  assert.equal(isBusy(state("starting")), true);
  assert.equal(isBusy(state("ready")), false);
});

test("a model that is too big or has no disk space cannot be installed", () => {
  const m = (fit: string, disk_ok: boolean | null) => ({ fit, disk_ok }) as never;
  assert.equal(canInstall(m("comfortable", true)), true);
  assert.equal(canInstall(m("slow", null)), true);
  assert.equal(canInstall(m("too_large", true)), false);
  assert.equal(canInstall(m("unknown", true)), false);
  assert.equal(canInstall(m("comfortable", false)), false);
  assert.equal(sizeLabel(2.5 * 1024 ** 3), "2.5 GB");
  assert.equal(sizeLabel(800 * 1024 ** 2), "800 MB");
});
