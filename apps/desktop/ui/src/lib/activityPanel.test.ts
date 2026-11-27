import assert from "node:assert/strict";
import { test } from "node:test";
import type { Schedule } from "../api/types.ts";
import type { EventsState, RunState, ToolItem } from "../state/timeline.ts";
import {
  activitySections,
  subagentRows,
  upcomingRows
} from "./activityPanel.ts";

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
  return { id, chatId: "chat", botId: "bot", startedMs: 1000, items: [], ...extra };
}

test("sub-agent rows map run state to the panel states and hide discovery plumbing", () => {
  const runs: Record<string, RunState> = {
    main: run("main", { startedMs: 1000 }),
    a: run("a", {
      parentRunId: "main",
      task: "Search the repos",
      subagentState: "running",
      items: [tool("web_search", "done", 1100), tool("find_tool", "requested", 1200)]
    }),
    b: run("b", {
      parentRunId: "main",
      task: "Write the mail",
      startedMs: 1100,
      items: [tool("email_send", "awaiting_approval", 1300)]
    }),
    c: run("c", { parentRunId: "main", task: "Check CI", startedMs: 1200, finishedMs: 5000, subagentState: "succeeded" }),
    d: run("d", { parentRunId: "main", task: "Lint", startedMs: 1300, finishedMs: 5000, subagentState: "failed" }),
    e: run("e", { parentRunId: "main", task: "Deploy", startedMs: 1400, finishedMs: 5000, subagentState: "cancelled" })
  };
  const events: EventsState = { runs, lastGseq: 0 };
  const rows = subagentRows(events, "chat");
  assert.deepEqual(
    rows.map((r) => [r.id, r.state, r.canStop]),
    [
      ["a", "running", true],
      ["b", "waiting", true],
      ["c", "done", false],
      ["d", "failed", false],
      ["e", "stopped", false]
    ]
  );
  // find_tool is plumbing: the current step is the real call before it
  assert.equal(rows[0]?.step?.name, "web_search");
  assert.equal(subagentRows(events, undefined).length, 0);
});

test("sections without data are not rendered", () => {
  assert.deepEqual(activitySections({ subagents: 0, upcoming: 0, quota: 0 }), ["computer"]);
  assert.deepEqual(activitySections({ subagents: 3, upcoming: 1, quota: 2 }), [
    "subagents",
    "computer",
    "upcoming",
    "quota"
  ]);

  const sched = (over: Partial<Schedule>): Schedule =>
    ({
      id: "s",
      bot_id: "bot",
      name: "Weekly",
      enabled: true,
      has_webhook: false,
      misfire_policy: "skip",
      origin: "user",
      created_at_ms: 0,
      spec: {},
      payload: {},
      ...over
    }) as Schedule;
  // another bot's schedule and a finished one yield no row, so no "Upcoming" header
  assert.deepEqual(
    upcomingRows([sched({ bot_id: "other", next_run_at_ms: 5 }), sched({ next_run_at_ms: null })], "bot"),
    []
  );
  assert.equal(upcomingRows([sched({ next_run_at_ms: 5 }), sched({ id: "t", has_webhook: true })], "bot").length, 2);
});
