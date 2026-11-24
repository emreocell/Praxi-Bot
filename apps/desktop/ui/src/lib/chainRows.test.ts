import assert from "node:assert/strict";
import { test } from "node:test";
import type { ToolItem } from "../state/timeline.ts";
import { buildRows, capRows, chainSummary, collapsedRows, groupHost } from "./chainRows.ts";
import type { ChainStep } from "./thinkingChain.ts";

let n = 0;
function step(name: string, status: ToolItem["status"] = "done"): ChainStep {
  n++;
  return {
    kind: "tool",
    key: `k${n}`,
    tool: { kind: "tool", key: `k${n}`, callId: `c${n}`, name, input: {}, status, requestedMs: n * 1000, gated: false }
  };
}

test("sixteen browser steps fold into one group; a failure stays its own row", () => {
  const steps: ChainStep[] = [
    step("playwright.browser_navigate"),
    ...Array.from({ length: 12 }, () => step("playwright.browser_evaluate")),
    step("playwright.browser_evaluate", "error"),
    step("playwright.browser_click"),
    step("playwright.browser_click")
  ];
  const rows = buildRows(steps);
  assert.deepEqual(rows.map((r) => r.kind), ["group", "tool", "group"]);
  const first = rows[0];
  assert.ok(first && first.kind === "group");
  assert.equal(first.tools.length, 13);
  assert.equal(first.sameAction, false);
  const last = rows[2];
  assert.ok(last && last.kind === "group" && last.sameAction);
});

test("a collapsed finished chain shows only failures; a live one also its current step", () => {
  const rows = buildRows([step("playwright.a"), step("playwright.a"), step("playwright.b", "error"), step("gmail.send", "running")]);
  assert.equal(collapsedRows(rows, false).shown.length, 2); // error + still-running orphan is not hidden
  const live = collapsedRows(buildRows([step("x.a"), step("x.a"), step("x.b", "running")]), true);
  assert.equal(live.shown.length, 1);
  assert.equal(live.hidden, 1);
  const done = collapsedRows(buildRows([step("x.a"), step("x.a")]), false);
  assert.equal(done.shown.length, 0);
});

test("summary counts steps and names the apps", () => {
  const s = chainSummary({ steps: [step("playwright.a"), step("mcp:playwright/b"), step("web_fetch")] });
  assert.deepEqual(s, { steps: 3, names: ["playwright"] });
});

test("a group that visited one site is named after it; mixed sites are not", () => {
  const open = (url: string): ToolItem => ({
    kind: "tool", key: url, callId: url, name: "browser_open", input: { url }, status: "done", requestedMs: 1, gated: false
  });
  assert.equal(groupHost([open("https://www.webioo.com.tr/a"), open("https://webioo.com.tr/b")]), "webioo.com.tr");
  assert.equal(groupHost([open("https://a.com"), open("https://b.com")]), null);
  assert.equal(groupHost([{ ...open("https://a.com"), input: null }]), null);
});

test("a 250-step run renders a capped list until the owner asks for all of it", () => {
  const rows = Array.from({ length: 250 }, (_, i) => i);
  const c = capRows(rows, false);
  assert.equal(c.shown.length, 40);
  assert.equal(c.hidden, 210);
  assert.equal(capRows(rows, true).shown.length, 250);
  assert.equal(capRows(rows.slice(0, 10), false).hidden, 0);
});
