import assert from "node:assert/strict";
import test from "node:test";
import { healthStripMode } from "./health.ts";

test("healthy real provider shows nothing", () => assert.equal(healthStripMode("ok", false), "hidden"));
test("healthy fake provider always shows the tag", () => assert.equal(healthStripMode("ok", true), "tag"));
test("unhealthy shows the full strip", () => {
  assert.equal(healthStripMode("bad", false), "full");
  assert.equal(healthStripMode("wait", true), "full");
});
