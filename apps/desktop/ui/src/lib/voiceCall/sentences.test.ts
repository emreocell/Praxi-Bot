import assert from "node:assert/strict";
import { test } from "node:test";
import { takeSpeakable } from "./sentences.ts";

test("only complete sentences are spoken while the reply streams, the rest on final", () => {
  const a = takeSpeakable("Hello there. How are", 0, false);
  assert.deepEqual(a.chunks, ["Hello there."]);
  const b = takeSpeakable("Hello there. How are you? Fine", a.end, false);
  assert.deepEqual(b.chunks, ["How are you?"]);
  const c = takeSpeakable("Hello there. How are you? Fine", b.end, true);
  assert.deepEqual(c.chunks, ["Fine"]);
  assert.equal(c.end, "Hello there. How are you? Fine".length);
});

test("a decimal point is not a sentence end and a trailing dot waits for more text", () => {
  assert.deepEqual(takeSpeakable("Pi is 3.14 roughly", 0, false).chunks, []);
  assert.deepEqual(takeSpeakable("Done.", 0, false).chunks, []);
  assert.deepEqual(takeSpeakable("Done.", 0, true).chunks, ["Done."]);
});

test("a long run without punctuation is cut so speech does not stall", () => {
  const text = "word ".repeat(100);
  const out = takeSpeakable(text, 0, false);
  assert.ok(out.chunks.length >= 1);
  assert.ok(out.chunks.every((c) => c.length <= 220));
});
