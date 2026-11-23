import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import type { ProvidersDto } from "../api/types.ts";
import {
  DEMO_TASK_IDS,
  type DemoAction,
  type DemoData,
  type DemoScript,
  type DemoState,
  demoReducer,
  hasRealModel,
  offerDemo,
  showConnectCard,
  startDemo
} from "./demoEngine.ts";

// Plain node cannot import JSON without attributes; the data files are read like the app reads them.
const demoData = (lng: string): DemoData =>
  JSON.parse(readFileSync(fileURLToPath(new URL(`./demo.${["tr", "de"].includes(lng) ? lng : "en"}.json`, import.meta.url)), "utf8"));
const script = demoData("en").summarise;

function run(actions: DemoAction[], s: DemoScript = script): DemoState {
  return actions.reduce((st, a) => demoReducer(st, a, s), startDemo("summarise"));
}
const ticks = (n: number): DemoAction[] => Array.from({ length: n }, () => ({ type: "tick" }));

test("the demo shows its steps, pauses at the approval card, then streams the answer", () => {
  const afterSteps = run(ticks(script.steps.length + 1));
  assert.equal(afterSteps.steps.length, script.steps.length);
  assert.equal(afterSteps.status, "awaitingApproval");
  // More ticks change nothing until the person decides.
  assert.deepEqual(run(ticks(script.steps.length + 20)), afterSteps);
  const partial = run([...ticks(script.steps.length + 1), { type: "allow" }, { type: "tick" }]);
  assert.equal(partial.status, "running");
  assert.ok(partial.answer.length > 0 && partial.answer.length < script.answer.length);
  const done = run([...ticks(script.steps.length + 1), { type: "allow" }, ...ticks(200)]);
  assert.equal(done.status, "done");
  assert.equal(done.answer, script.answer);
});

test("denying the example approval gives the 'nothing was done' answer", () => {
  const done = run([...ticks(script.steps.length + 1), { type: "deny" }, ...ticks(200)]);
  assert.equal(done.status, "done");
  assert.equal(done.decision, "denied");
  assert.equal(done.answer, script.denied);
});

test("Stop ends the demo at once and nothing moves afterwards", () => {
  const stopped = run([...ticks(2), { type: "stop" }]);
  assert.equal(stopped.status, "stopped");
  assert.deepEqual(run([...ticks(2), { type: "stop" }, ...ticks(50), { type: "allow" }]), stopped);
  assert.equal(showConnectCard(stopped), true);
  assert.equal(showConnectCard(startDemo("plan")), false);
});

test("the demo is offered only while no real model is connected", () => {
  const none = { active: [], catalog: [], default_provider: "", configured: [] } as unknown as ProvidersDto;
  const connected = {
    ...none,
    default_provider: "gemini",
    configured: [{ id: "gemini", active: true, custom: false, default_model: "flash", enabled: true, has_key: true, is_default: true, models: [] }]
  } as unknown as ProvidersDto;
  assert.equal(offerDemo(none), true);
  assert.equal(offerDemo(connected), false);
  assert.equal(hasRealModel(null), true, "unknown yet: never flash the demo at people who have a model");
});

test("en, tr and de carry the same keys, real translations and the demo label", () => {
  const en = demoData("en");
  for (const lng of ["tr", "de"]) {
    const d = demoData(lng);
    assert.deepEqual(Object.keys(d.ui), Object.keys(en.ui));
    assert.notEqual(d.ui.connectBody, en.ui.connectBody);
    for (const id of DEMO_TASK_IDS) {
      assert.deepEqual(Object.keys(d[id]), Object.keys(en[id]));
      assert.equal(d[id].steps.length, en[id].steps.length);
      assert.notEqual(d[id].title, en[id].title);
      assert.notEqual(d[id].answer, en[id].answer);
    }
  }
  assert.match(en.ui.label, /^Demo: connect a model to get real answers$/);
});

test("the demo never reaches the daemon, a provider, files or storage", () => {
  const dir = fileURLToPath(new URL(".", import.meta.url));
  const forbidden = /api\/client|useDaemon|fetch\(|XMLHttpRequest|WebSocket|invoke\(|localStorage|settingsStore|plugin-store|readFile|writeFile|navigator\./;
  const files = readdirSync(dir).filter((f) => /\.(ts|tsx)$/.test(f) && !f.endsWith(".test.ts"));
  assert.ok(files.length >= 3);
  for (const f of files) {
    assert.doesNotMatch(readFileSync(`${dir}${f}`, "utf8"), forbidden, f);
  }
});

test("the wiring: cards only without a model, Help always, never as a real chat", () => {
  const app = readFileSync(fileURLToPath(new URL("../App.tsx", import.meta.url)), "utf8");
  assert.match(app, /onDemo=\{showDemoOffer \? \(task\) => setDemo\(\{ task \}\) : undefined\}/);
  assert.match(app, /onDemo=\{\(\) => \{\s*setHelpOpen\(false\);\s*setDemo\(\{ task: null \}\);/);
  assert.doesNotMatch(app.slice(app.indexOf("<DemoModal")), /startChat/);
});
