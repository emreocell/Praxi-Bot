import assert from "node:assert/strict";
import { test } from "node:test";
import { toastDuration } from "./toastTiming.ts";

test("error toasts stay longer than confirmations", () => {
  assert.ok(toastDuration("danger") > toastDuration("success"));
  assert.equal(toastDuration("info"), toastDuration("success"));
});
