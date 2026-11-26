import assert from "node:assert/strict";
import { test } from "node:test";
import { canGoTo, galleryLevel, nextStep, NEW_BOT_STEPS, prevStep, stepNumber } from "./newBotSteps.ts";

test("the new-bot flow: start, then name and look, then what it can do (create there)", () => {
  assert.deepEqual([...NEW_BOT_STEPS], ["start", "identity", "access"]);
  assert.equal(stepNumber("start"), 1);
  assert.equal(stepNumber("access"), 3);
  assert.equal(nextStep("start"), "identity");
  assert.equal(nextStep("identity"), "access");
  assert.equal(nextStep("access"), null);
  assert.equal(prevStep("start"), null);
  assert.equal(prevStep("access"), "identity");
});

test("the stepper goes back freely; forward only past a step that is complete (a name is needed)", () => {
  assert.equal(canGoTo("access", "start", { nameMissing: true }), true);
  assert.equal(canGoTo("access", "identity", { nameMissing: false }), true);
  assert.equal(canGoTo("start", "access", { nameMissing: false }), false);
  assert.equal(canGoTo("identity", "access", { nameMissing: true }), false);
  assert.equal(canGoTo("identity", "access", { nameMissing: false }), true);
  // From the start the first step is a choice (blank or a ready-made bot), not a form: the stepper does not skip it.
  assert.equal(canGoTo("start", "identity", { nameMissing: false }), false);
  assert.equal(canGoTo("access", "access", { nameMissing: false }), false);
});

test("a ready-made bot's card says how freely it starts, in the same words as the autonomy picker", () => {
  assert.deepEqual(galleryLevel("read-and-draft"), { kind: "autonomy", level: "ask" });
  assert.deepEqual(galleryLevel("approve-destructive"), { kind: "autonomy", level: "balanced" });
  // A gallery template is never imported above "approve-destructive" (initialLevel), so "full" reads as balanced.
  assert.deepEqual(galleryLevel("full"), { kind: "autonomy", level: "balanced" });
  assert.deepEqual(galleryLevel("read-only"), { kind: "preset", preset: "read-only" });
  assert.equal(galleryLevel(undefined), null);
  assert.equal(galleryLevel("weird"), null);
});
