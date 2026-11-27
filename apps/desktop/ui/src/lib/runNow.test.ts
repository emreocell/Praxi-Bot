import assert from "node:assert/strict";
import { test } from "node:test";
import type { EventsState, RunState, ToolItem } from "../state/timeline.ts";
import { botRunNow, runNow } from "./runNow.ts";

function tool(name: string, status: ToolItem["status"], at: number): ToolItem {
  return {
    kind: "tool",
    key: `${name}-${at}`,
    callId: `${name}-${at}`,
    name,
    input: {},
    status,
    requestedMs: at
  } as ToolItem;
}

function run(id: string, extra: Partial<RunState> = {}): RunState {
  return { id, chatId: "chat-1", botId: "bot-1", startedMs: 1000, items: [], ...extra };
}

test("runNow returns null when chatId is undefined", () => {
  const events: EventsState = { runs: {}, lastGseq: 0 };
  assert.equal(runNow(events, undefined), null);
});

test("runNow returns null when a chat has only finished runs", () => {
  const finishedRun = run("r1", { finishedMs: 2000 });
  const events: EventsState = {
    runs: { r1: finishedRun },
    lastGseq: 0
  };
  assert.equal(runNow(events, "chat-1"), null);
});

test("runNow returns the live run with its startedMs", () => {
  const liveRun = run("r-live", { startedMs: 1500 });
  const events: EventsState = {
    runs: { "r-live": liveRun },
    lastGseq: 0
  };
  const result = runNow(events, "chat-1");
  assert.ok(result);
  assert.equal(result.run.id, "r-live");
  assert.equal(result.startedMs, 1500);
  assert.equal(result.step, null);
  assert.equal(result.approval, null);
  assert.equal(result.subagents, 0);
});

test("when several top-level runs exist, the newest live run is chosen", () => {
  // topLevelRuns returns runs in collection order; runNow walks in reverse ([...].reverse().find)
  // so the latest live run added is chosen even if an older run was left unfinished.
  const oldUnfinished = run("r-old", { startedMs: 1000 });
  const newLive = run("r-new", { startedMs: 2000 });
  const events: EventsState = {
    runs: {
      "r-old": oldUnfinished,
      "r-new": newLive
    },
    lastGseq: 0
  };
  const result = runNow(events, "chat-1");
  assert.ok(result);
  assert.equal(result.run.id, "r-new", "the newest live run in reverse order is selected");
});

test("a finished newest run does not hide an older run that is still live", () => {
  const oldLive = run("r-old-live", { startedMs: 1000 });
  const newFinished = run("r-new-done", { startedMs: 2000, finishedMs: 2500 });
  const events: EventsState = {
    runs: { "r-old-live": oldLive, "r-new-done": newFinished },
    lastGseq: 0
  };
  assert.equal(runNow(events, "chat-1")?.run.id, "r-old-live");
});

test("runs of another chat are ignored", () => {
  const other = run("r-other-chat", { chatId: "chat-2" });
  const events: EventsState = { runs: { "r-other-chat": other }, lastGseq: 0 };
  assert.equal(runNow(events, "chat-1"), null);
});

test("tool with awaiting_approval becomes approval, and discovery tools are ignored for step", () => {
  const awaitingTool = tool("email_send", "awaiting_approval", 1100);
  const realTool = tool("web_search", "done", 1200);
  const discoveryTool = tool("find_tool", "requested", 1300);

  const liveRun = run("r-tools", {
    items: [awaitingTool, realTool, discoveryTool]
  });

  const events: EventsState = {
    runs: { "r-tools": liveRun },
    lastGseq: 0
  };

  const result = runNow(events, "chat-1");
  assert.ok(result);
  // awaiting_approval tool is found
  assert.equal(result.approval?.name, "email_send");
  // currentStep skips find_tool and picks the newest real tool (web_search)
  assert.equal(result.step?.name, "web_search");
});

test("subagents counts all child runs with parentRunId matching live run, including finished ones", () => {
  const liveRun = run("r-parent", { startedMs: 1000 });
  const sub1 = run("sub-1", { parentRunId: "r-parent", startedMs: 1100 });
  const sub2Finished = run("sub-2", { parentRunId: "r-parent", startedMs: 1200, finishedMs: 1500 });
  const unrelated = run("other", { parentRunId: "different-parent" });

  const events: EventsState = {
    runs: {
      "r-parent": liveRun,
      "sub-1": sub1,
      "sub-2": sub2Finished,
      other: unrelated
    },
    lastGseq: 0
  };

  const result = runNow(events, "chat-1");
  assert.ok(result);
  assert.equal(result.subagents, 2);
});

test("botRunNow prefers the open chat's live run", () => {
  const here = run("r-here", { startedMs: 1000 });
  const there = run("r-there", { chatId: "chat-2", startedMs: 5000 });
  const events: EventsState = { runs: { "r-here": here, "r-there": there }, lastGseq: 0 };
  assert.equal(botRunNow(events, "chat-1", ["chat-1", "chat-2"])?.run.id, "r-here");
});

test("botRunNow finds the bot working in another chat (the header must not say idle)", () => {
  const older = run("r-old", { chatId: "chat-2", startedMs: 2000 });
  const newer = run("r-new", { chatId: "chat-3", startedMs: 3000 });
  const done = run("r-done", { chatId: "chat-4", startedMs: 9000, finishedMs: 9500 });
  const foreign = run("r-foreign", { chatId: "chat-9", startedMs: 9999 });
  const events: EventsState = { runs: { "r-old": older, "r-new": newer, "r-done": done, "r-foreign": foreign }, lastGseq: 0 };
  assert.equal(botRunNow(events, "chat-1", ["chat-1", "chat-2", "chat-3", "chat-4"])?.run.id, "r-new");
  assert.equal(botRunNow(events, undefined, ["chat-4"]), null);
});
