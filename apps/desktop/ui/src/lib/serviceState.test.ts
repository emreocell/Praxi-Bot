import assert from "node:assert/strict";
import test from "node:test";
import {
  initialServiceMemory,
  observeService,
  SERVICE_FAILED_MS,
  SERVICE_QUIET_MS,
  serviceView
} from "./serviceState.ts";

const T0 = 1_000_000;
const ready = () => observeService(initialServiceMemory(T0), true, T0);

test("a restart shorter than the quiet period never shows anything", () => {
  let m = observeService(ready(), false, T0 + 100);
  assert.deepEqual(serviceView(m, T0 + 100 + SERVICE_QUIET_MS - 1), {
    status: "ready", bannerVisible: false, blocked: false, canRestart: false
  });
  m = observeService(m, true, T0 + 2500);
  assert.equal(serviceView(m, T0 + 2500).status, "ready");
});

test("a longer gap shows one reconnecting banner and blocks only dependent buttons", () => {
  const m = observeService(ready(), false, T0);
  const v = serviceView(m, T0 + SERVICE_QUIET_MS);
  assert.equal(v.status, "reconnecting");
  assert.equal(v.bannerVisible, true);
  assert.equal(v.blocked, true);
  assert.equal(v.canRestart, false);
});

test("flapping keeps resetting the gap: it never reaches the banner", () => {
  let m = ready();
  let now = T0;
  for (let i = 0; i < 20; i += 1) {
    m = observeService(m, false, now);
    now += 2000;
    assert.equal(serviceView(m, now).bannerVisible, false);
    m = observeService(m, true, now);
    now += 1000;
  }
  assert.equal(serviceView(m, now).status, "ready");
});

test("failed after 15 s offers the restart; the first answer returns to ready", () => {
  let m = observeService(ready(), false, T0);
  m = observeService(m, false, T0 + 5000); // later failed probes keep the original start
  const v = serviceView(m, T0 + SERVICE_FAILED_MS);
  assert.equal(v.status, "failed");
  assert.equal(v.canRestart, true);
  m = observeService(m, true, T0 + SERVICE_FAILED_MS + 10);
  assert.deepEqual(serviceView(m, T0 + SERVICE_FAILED_MS + 10), {
    status: "ready", bannerVisible: false, blocked: false, canRestart: false
  });
});

test("before the first answer the state is starting (blocked, banner only after the quiet period)", () => {
  const m = initialServiceMemory(T0);
  const early = serviceView(m, T0 + 500);
  assert.equal(early.status, "starting");
  assert.equal(early.bannerVisible, false);
  assert.equal(early.blocked, true);
  const late = serviceView(m, T0 + SERVICE_QUIET_MS + 1);
  assert.equal(late.status, "starting");
  assert.equal(late.bannerVisible, true);
});
