import assert from "node:assert/strict";
import { test } from "node:test";
import { stepReason } from "./stepReason.ts";
import { siteIcon } from "./siteIcon.ts";

test("a failed step gets a short real reason, not a generic sentence", () => {
  assert.deepEqual(stepReason({ status: "error", error: "request timed out", durationMs: 30_400 }, false), {
    kind: "timeout",
    seconds: 30
  });
  assert.deepEqual(stepReason({ status: "denied" }, false), { kind: "blocked" });
  assert.deepEqual(stepReason({ status: "expired" }, false), { kind: "noAnswer" });
  assert.deepEqual(stepReason({ status: "running" }, true), { kind: "stopped" });
  assert.deepEqual(stepReason({ status: "error", error: "boom" }, false), { kind: "other" });
  assert.equal(stepReason({ status: "done" }, false), null);
});

test("the site chip is a letter or a globe, never a remote image", () => {
  assert.deepEqual(siteIcon("www.webioo.com.tr"), { kind: "letter", letter: "W" });
  assert.deepEqual(siteIcon("örnek.de"), { kind: "letter", letter: "Ö" });
  assert.equal(siteIcon(null).kind, "globe");
  assert.equal(siteIcon("-").kind, "globe");
});
