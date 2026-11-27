import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import type { BotTerminal } from "../api/types";
import { activeTerminal, aliveCount, screenLines, terminalsForTarget, terminalStateKey } from "./liveTerminals.ts";

const term = (id: string, bot: string | null, state: BotTerminal["state"] = "running"): BotTerminal => ({
  id,
  bot_id: bot,
  command: `prog-${id}`,
  state,
  exit_code: null,
  cols: 120,
  rows: 30,
  age_secs: 1,
  screen: ""
});

test("a bot's own browser shows that bot's terminals, a desktop shows all of them (#894)", () => {
  const all = [term("a", "bot-1"), term("b", "bot-2"), term("c", null)];
  assert.deepEqual(
    terminalsForTarget(all, { kind: "browser", bot_id: "bot-1" }).map((t) => t.id),
    ["a", "c"]
  );
  assert.deepEqual(
    terminalsForTarget(all, { kind: "desktop", bot_id: "bot-1" }).map((t) => t.id),
    ["a", "b", "c"]
  );
  assert.deepEqual(terminalsForTarget(all, null).map((t) => t.id), ["a", "b", "c"]);
  assert.deepEqual(terminalsForTarget(all, { kind: "browser", bot_id: null }).map((t) => t.id), ["a", "b", "c"]);
  assert.deepEqual(terminalsForTarget([], { kind: "desktop" }), []);
});

test("the chosen terminal stays while it exists, else the newest is shown", () => {
  const list = [term("new", null), term("old", null)];
  assert.equal(activeTerminal(list, "old")?.id, "old");
  assert.equal(activeTerminal(list, "gone")?.id, "new");
  assert.equal(activeTerminal(list, null)?.id, "new");
  assert.equal(activeTerminal([], "old"), null);
});

test("states map to the four words, and only live sessions are counted as running", () => {
  assert.equal(terminalStateKey("running"), "running");
  assert.equal(terminalStateKey("waiting_for_input"), "waiting");
  assert.equal(terminalStateKey("finished"), "finished");
  assert.equal(terminalStateKey("failed"), "failed");
  assert.equal(
    aliveCount([term("a", null, "running"), term("b", null, "waiting_for_input"), term("c", null, "finished"), term("d", null, "failed")]),
    2
  );
});

test("the screen text drops trailing blank rows and is never empty", () => {
  assert.deepEqual(screenLines("a\n\nb\n\n   \n"), ["a", "", "b"]);
  assert.deepEqual(screenLines(""), [""]);
  assert.deepEqual(screenLines("x\r\ny"), ["x", "y"]);
});

test("the terminal panel has no way to type: no field, no key or paste handler, no input call (#894)", () => {
  const source = readFileSync(new URL("../views/LiveTerminals.tsx", import.meta.url), "utf8");
  for (const forbidden of ["<input", "<textarea", "contentEditable", "onPaste", "onBeforeInput", "enqueueInput", "requestTakeover"]) {
    assert.equal(source.includes(forbidden), false, forbidden);
  }
  // The only key handling is the tab list's arrow keys.
  assert.equal((source.match(/onKeyDown/g) ?? []).length, 1);
  const client = readFileSync(new URL("../api/client.ts", import.meta.url), "utf8");
  assert.ok(client.includes('"GET", "/view/bot-terminals"'));
  assert.equal(/"(POST|PUT|PATCH)", "\/view\/bot-terminals/.test(client), false);
});
