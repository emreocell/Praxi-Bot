import assert from "node:assert/strict";
import { test } from "node:test";
import { stepStates } from "./setupChecklist.ts";

test("the first open step is the only next step", () => {
  assert.deepEqual(stepStates([false, false, false]), ["next", "later", "later"]);
  assert.deepEqual(stepStates([true, false, false]), ["done", "next", "later"]);
});

test("a step done out of order stays done and the earlier open one is next", () => {
  assert.deepEqual(stepStates([false, true, false]), ["next", "done", "later"]);
});

test("all done means no next step", () => {
  assert.deepEqual(stepStates([true, true]), ["done", "done"]);
  assert.deepEqual(stepStates([]), []);
});

test("checklists offer a button only on the next step", async () => {
  const { readFileSync } = await import("node:fs");
  const read = (p: string) => readFileSync(new URL(p, import.meta.url), "utf8");
  assert.match(read("../components/SetupChecklist.tsx"), /state === "next" && step\.action \?/);
  const started = read("../components/GettingStarted.tsx");
  assert.match(started, /\{isNext\(step\) \? \(\s*<div className=\{styles\.actions\}>/);
  assert.doesNotMatch(started, /progress\.next \? "primary" : "secondary"/);
  const host = read("../components/HostEmptyState.tsx");
  assert.match(host, /<SetupChecklist/);
  assert.doesNotMatch(host, /variant=\{noModel \? "secondary" : "primary"\}/);
});
