import assert from "node:assert/strict";
import { test } from "node:test";
import { suggestScheduleNotify } from "./scheduleNotify.ts";

test("a reminder is announced every time, background work only on problems", () => {
  assert.equal(suggestScheduleNotify("Tell me: your tea is ready", false), "always");
  assert.equal(suggestScheduleNotify("remind me to water the plants", false), "always");
  assert.equal(suggestScheduleNotify("Summarize my inbox", true), "always");
  assert.equal(suggestScheduleNotify("Summarize my inbox", false), "problems");
});
