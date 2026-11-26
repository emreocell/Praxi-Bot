import assert from "node:assert/strict";
import { test } from "node:test";
import { pendingMemoryView } from "./pendingMemory.ts";

test("a fact picked up from a page reads as outside, shared, and hides a quote it already contains", () => {
  const view = pendingMemoryView({
    content: "User's email address is furkan@example.com",
    source_quote: "“furkan@example.com”",
    trust: "untrusted",
    bot_id: null
  });
  assert.deepEqual(view, { origin: "outside", scope: { kind: "shared" }, quote: null });
});

test("a quote that adds context is kept, and a bot-only memory names its bot", () => {
  const view = pendingMemoryView({
    content: "Prefers short replies",
    source_quote: "please keep it brief, I read on my phone",
    trust: "user",
    bot_id: "b1"
  });
  assert.equal(view.origin, "chat");
  assert.deepEqual(view.scope, { kind: "bot", botId: "b1" });
  assert.equal(view.quote, "please keep it brief, I read on my phone");
});

test("an empty or missing quote is never shown", () => {
  assert.equal(pendingMemoryView({ content: "x", source_quote: "  ", trust: "user" }).quote, null);
  assert.equal(pendingMemoryView({ content: "x", trust: "user" }).quote, null);
});
