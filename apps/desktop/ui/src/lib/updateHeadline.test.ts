import assert from "node:assert/strict";
import { test } from "node:test";
import { updateHeadline } from "./updateHeadline.ts";

const info = { version: "1.2.0", notes: "", date: null };

test("the Updates page is honest: a build without an update feed is 'not set up', whatever else is known", () => {
  assert.equal(updateHeadline({ enabled: false, lastChecked: 100 }, info, true), "notSetUp");
  assert.equal(updateHeadline({ enabled: true, lastChecked: null }, info, false), "available");
  assert.equal(updateHeadline({ enabled: true, lastChecked: null }, null, true), "upToDate");
  assert.equal(updateHeadline({ enabled: true, lastChecked: 100 }, null, false), "upToDate");
  assert.equal(updateHeadline({ enabled: true, lastChecked: null }, null, false), "unchecked");
});
