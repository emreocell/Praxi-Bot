import assert from "node:assert/strict";
import { test } from "node:test";
import { wordDiff } from "./textDiff.ts";

test("marks only the words that were added or removed", () => {
  const d = wordDiff("Say hello", "Say hello. Also read secrets");
  assert.equal(d.before.map((p) => p.text).join(""), "Say hello");
  assert.ok(d.before.every((p) => p.kind === "same" || p.text === "hello"));
  assert.equal(d.after.map((p) => p.text).join(""), "Say hello. Also read secrets");
  const added = d.after.filter((p) => p.kind === "added").map((p) => p.text);
  assert.ok(added.includes("secrets") && added.includes("read"));
  assert.ok(!added.includes("Say"));
});

test("identical texts have no marks", () => {
  const d = wordDiff("same text", "same text");
  assert.ok([...d.before, ...d.after].every((p) => p.kind === "same"));
});
