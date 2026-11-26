import assert from "node:assert/strict";
import { test } from "node:test";
import type { Message } from "../api/types";
import { lastTurn, offersRegenerate, supersededGroups, supersededRunIds } from "./resendTurn.ts";

function msg(id: string, role: string, extra: Partial<Message> = {}): Message {
  return { id, chat_id: "c", role, content: id, created_at_ms: Number(id.replace(/\D/g, "")) || 0, ...extra } as Message;
}

test("the last turn is the last live user message; superseded ones are ignored", () => {
  const turn = lastTurn([
    msg("u1", "user", { source: "owner", superseded: true }),
    msg("a1", "assistant", { superseded: true }),
    msg("u2", "user", { source: "owner" }),
    msg("a2", "assistant", { used_write_tools: true })
  ]);
  assert.equal(turn?.user.id, "u2");
  assert.equal(turn?.resendable, true);
  assert.equal(turn?.usedWriteTools, true);
  assert.equal(turn?.answered, true);
});

test("a message from another source or followed by foreign rows is not resendable", () => {
  assert.equal(lastTurn([msg("u1", "user", { source: "telegram" })])?.resendable, false);
  assert.equal(lastTurn([msg("u1", "user", { source: "owner" }), msg("s1", "system")])?.resendable, false);
  assert.equal(lastTurn([]), null);
});

test("superseded messages group per consecutive stretch and name their runs", () => {
  const all = [
    msg("u1", "user", { superseded: true, run_id: "r1" }),
    msg("a1", "assistant", { superseded: true, run_id: "r1" }),
    msg("u2", "user"),
    msg("u3", "user", { superseded: true, run_id: "r3" })
  ];
  assert.deepEqual(supersededGroups(all).map((g) => g.messages.length), [2, 1]);
  assert.deepEqual([...supersededRunIds(all)].sort(), ["r1", "r3"]);
});

test("regenerate is offered after an answer, and after a failed run with no answer, but not for a foreign message", () => {
  const answered = lastTurn([msg("u1", "user", { source: "owner" }), msg("a1", "assistant")]);
  const failed = lastTurn([msg("u1", "user", { source: "owner" })]);
  const foreign = lastTurn([msg("u1", "user", { source: "telegram" })]);
  assert.equal(offersRegenerate(answered, false), true);
  assert.equal(offersRegenerate(failed, true), true);
  assert.equal(offersRegenerate(failed, false), false);
  assert.equal(offersRegenerate(foreign, true), false);
  assert.equal(offersRegenerate(null, true), false);
});
