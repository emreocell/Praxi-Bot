import assert from "node:assert/strict";
import { test } from "node:test";
import { describeTool, hostOf, humanizeName } from "./toolLabels.ts";

test("known builtin tools get a stable key, icon and chips", () => {
  const s = describeTool("web_search", { query: "cpu llm benchmark" });
  assert.equal(s.key, "chat.step.web_search");
  assert.equal(s.icon, "search");
  assert.deepEqual(s.chips, [{ text: "cpu llm benchmark" }]);
  const r = describeTool("web_extract", { url: "https://www.github.com/a/b" });
  assert.equal(r.key, "chat.step.web_read");
  assert.deepEqual(r.chips, [{ text: "github.com", icon: "globe" }]);
  const f = describeTool("fs_read", { path: "C:/work/notes\\todo.md" });
  assert.deepEqual(f.chips, [{ text: "todo.md" }]);
});

test("prefix families cover tools the table does not list", () => {
  assert.equal(describeTool("browser_hover", {}).key, "chat.step.browser");
  assert.equal(describeTool("desktop_click", {}).key, "chat.step.desktop");
  assert.equal(describeTool("memory_store", {}).key, "chat.step.memory");
});

test("MCP tools name the app and the action", () => {
  const s = describeTool("github.search_pull_requests", { query: "is:closed" }, (n) =>
    n === "github" ? "GitHub" : null
  );
  assert.equal(s.key, "chat.step.mcp");
  assert.deepEqual(s.values, { app: "GitHub", action: "search pull requests" });
  assert.equal(s.brand, "github");
  assert.deepEqual(s.chips, [{ text: "is:closed" }]);
  const u = describeTool("linear.createIssue", {});
  assert.deepEqual(u.values, { app: "Linear", action: "create Issue" });
});

test("unknown tools fall back to a generic wording, never crash on odd input", () => {
  const g = describeTool("frobnicate_widgets", null);
  assert.equal(g.key, "chat.step.generic");
  assert.deepEqual(g.values, { name: "frobnicate widgets" });
  assert.deepEqual(describeTool("x", [1, 2]).chips, []);
  assert.deepEqual(describeTool("web_search", { query: 42 }).chips, []);
});

test("helpers", () => {
  assert.equal(hostOf("not a url"), null);
  assert.equal(hostOf("https://a.io/x"), "a.io");
  assert.equal(humanizeName("get_open-PRs"), "get open PRs");
});
