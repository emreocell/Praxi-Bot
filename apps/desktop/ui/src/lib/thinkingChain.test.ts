import assert from "node:assert/strict";
import { test } from "node:test";
import type { TextItem, TimelineItem, ToolItem, ToolStatus } from "../state/timeline.ts";
import { chainSeconds, groupRunItems } from "./thinkingChain.ts";

const text = (key: string, ts: number, t = key): TextItem => ({ kind: "text", key, ts, text: t });
const tool = (key: string, ts: number, status: ToolStatus = "done", name = "web_search"): ToolItem => ({
  kind: "tool",
  key,
  callId: key,
  name,
  input: {},
  status,
  requestedMs: ts,
  completedMs: status === "done" ? ts + 2000 : undefined,
  gated: false
});
const kinds = (items: TimelineItem[], finished: boolean) =>
  groupRunItems(items, finished).map((s) => s.kind);

test("tools and the thoughts before them form one chain; the final text stays", () => {
  const segs = groupRunItems(
    [text("a", 0, "I need data"), tool("t1", 1000), tool("t2", 4000), text("z", 9000, "Answer")],
    true
  );
  assert.deepEqual(segs.map((s) => s.kind), ["chain", "item"]);
  const chain = (segs[0] as { kind: "chain"; chain: import("./thinkingChain.ts").Chain }).chain;
  assert.deepEqual(chain.steps.map((s) => s.kind), ["thought", "tool", "tool"]);
  assert.equal(chain.live, false);
  assert.equal(chainSeconds(chain), 6);
});

test("text with no tool call after it is the answer, not a thought", () => {
  assert.deepEqual(kinds([text("a", 0), text("b", 1)], true), ["item", "item"]);
});

test("an approval request interrupts the chain and is never absorbed", () => {
  assert.deepEqual(
    kinds([tool("t1", 0), tool("t2", 1, "awaiting_approval", "shell_run"), tool("t3", 2, "running")], false),
    ["chain", "item", "chain"]
  );
});

test("subagent calls, questions and todos keep their own rows", () => {
  const q: TimelineItem = { kind: "question", key: "q", callId: "q", question: "?", options: [], requestedMs: 5 };
  assert.deepEqual(
    kinds([tool("t1", 0), tool("s", 1, "done", "spawn_subagent"), q], true),
    ["chain", "item", "item"]
  );
});

test("live only while the run is unfinished; failures are counted", () => {
  const run = groupRunItems([tool("t1", 0), tool("t2", 1, "error")], false);
  const c = (run[0] as { kind: "chain"; chain: import("./thinkingChain.ts").Chain }).chain;
  assert.equal(c.live, true);
  assert.equal(c.problems, 1);
  const done = groupRunItems([tool("t1", 0), tool("t2", 1, "error")], true);
  assert.equal((done[0] as { kind: "chain"; chain: import("./thinkingChain.ts").Chain }).chain.live, false);
});

test("a chain followed by the streaming answer is not live", () => {
  const segs = groupRunItems([tool("t1", 0), text("a", 5000, "partial")], false);
  assert.equal((segs[0] as { kind: "chain"; chain: import("./thinkingChain.ts").Chain }).chain.live, false);
});
