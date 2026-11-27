import assert from "node:assert/strict";
import { test } from "node:test";
import { readFileSync } from "node:fs";
import {
  describeTool,
  hostOf,
  humanizeName,
  isDiscoveryTool,
  isSendTool,
  KNOWN_TOOL_NAMES,
  mcpVerb,
  stepPhase,
  trLocative
} from "./toolLabels.ts";

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


test("document and git tools get plain step wording, edits apart from reads", () => {
  assert.equal(describeTool("xlsx_read", { path: "/a/b/book.xlsx" }).key, "chat.step.doc_read");
  assert.deepEqual(describeTool("xlsx_read", { path: "/a/b/book.xlsx" }).chips, [{ text: "book.xlsx" }]);
  assert.equal(describeTool("docx_write", { path: "r.docx" }).key, "chat.step.doc_write");
  for (const n of ["docx_append", "xlsx_append_rows", "xlsx_update_cells"]) {
    assert.equal(describeTool(n, { path: "r" }).key, "chat.step.doc_edit", n);
  }
  assert.equal(describeTool("git_diff", {}).key, "chat.step.git_read");
  assert.equal(describeTool("git_commit", {}).key, "chat.step.git_change");
});

test("MCP tools name the app and the action", () => {
  const s = describeTool("github.search_pull_requests", { query: "is:closed" }, (n) =>
    n === "github" ? "GitHub" : null
  );
  assert.equal(s.key, "chat.step.mcp_search");
  assert.deepEqual(s.values, { app: "GitHub", appAt: "GitHub'da", action: "search pull requests" });
  assert.equal(s.brand, "github");
  assert.deepEqual(s.chips, [{ text: "is:closed" }]);
  const u = describeTool("linear.createIssue", {});
  assert.equal(u.key, "chat.step.mcp");
  assert.deepEqual(u.values, { app: "Linear", appAt: "Linear'da", action: "create Issue" });
});

test("use_tool shows the real tool it ran; discovery plumbing is flagged hidden", () => {
  const s = describeTool("use_tool", {
    name: "mcp:github/search_repositories",
    input: { query: "grok bot" }
  });
  assert.equal(s.key, "chat.step.mcp_search");
  assert.equal(s.brand, "github");
  assert.deepEqual(s.chips, [{ text: "grok bot" }]);
  assert.equal(describeTool("use_tool", { name: "web_search", input: { query: "x" } }).key, "chat.step.web_search");
  assert.equal(isDiscoveryTool("find_tool"), true);
  assert.equal(isDiscoveryTool("get_tool_details"), true);
  assert.equal(isDiscoveryTool("use_tool"), false);
});

test("Turkish locative follows vowel harmony and consonant hardening", () => {
  assert.equal(trLocative("GitHub"), "GitHub'da");
  assert.equal(trLocative("Gmail"), "Gmail'de");
  assert.equal(trLocative("Slack"), "Slack'ta");
  assert.equal(trLocative("Notion"), "Notion'da");
  assert.equal(trLocative("Linear"), "Linear'da");
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

test("a family prefix never turns an unlisted tool into a read or \"used\" line", () => {
  for (const name of ["browser_frob", "desktop_frob", "memory_store", "skill_publish", "email_archive", "fs_chmod", "web_post"]) {
    assert.equal(describeTool(name, {}).key, "chat.step.generic", name);
  }
});

test("writing, scripting and typing tools are not worded as reads or generic use", () => {
  assert.equal(describeTool("email_mark_read", {}).key, "chat.step.email_mark");
  assert.notEqual(describeTool("email_mark_read", {}).key, describeTool("email_read", {}).key);
  const keys = ["browser_eval", "browser_fill", "browser_scroll"].map((n) => describeTool(n, {}).key);
  assert.equal(new Set(keys).size, 3);
  // The pointer tools have their own wording, not "looked at a page".
  assert.equal(describeTool("browser_hover", {}).key, "chat.step.browser_hover");
  assert.equal(describeTool("browser_select_text", {}).key, "chat.step.browser_select_text");
  assert.equal(describeTool("browser_pointer", {}).key, "chat.step.browser_pointer");
  assert.equal(describeTool("browser_attach", {}).key, "chat.step.browser_attach");
  assert.equal(describeTool("browser_dismiss_consent", {}).key, "chat.step.browser_dismiss_consent");
  assert.notEqual(describeTool("skill_write", {}).key, describeTool("skill_read", {}).key);
  assert.notEqual(describeTool("memory_save", {}).key, describeTool("memory_search", {}).key);
  assert.notEqual(describeTool("memory_forget", {}).key, describeTool("memory_search", {}).key);
  assert.notEqual(describeTool("notes_write", {}).key, describeTool("notes_read", {}).key);
  assert.equal(describeTool("session_search", { query: "deploy" }).key, "chat.step.session_search");
  assert.equal(describeTool("knowledge_search", { query: "backup" }).key, "chat.step.knowledge_search");
  assert.notEqual(describeTool("desktop_type", {}).key, describeTool("desktop_get_state", {}).key);
  assert.notEqual(describeTool("desktop_key", {}).key, describeTool("desktop_type", {}).key);
});

test("a failed, denied or expired step never uses the done wording", () => {
  assert.equal(stepPhase("done"), "done");
  assert.equal(stepPhase("running"), "running");
  assert.equal(stepPhase("requested"), "running");
  for (const s of ["error", "denied", "expired"]) {
    assert.equal(stepPhase(s), "failed");
  }
});

test("every known tool has running, done and failed text in both locales", () => {
  for (const lang of ["en", "tr"]) {
    const j = JSON.parse(readFileSync(new URL(`../i18n/locales/${lang}.json`, import.meta.url), "utf8"));
    const steps = [...KNOWN_TOOL_NAMES.map((n) => describeTool(n, {}).key), "chat.step.mcp", "chat.step.generic"];
    for (const key of steps) {
      const entry = j.chat.step[key.replace("chat.step.", "")];
      for (const phase of ["running", "done", "failed"]) {
        assert.ok(typeof entry?.[phase] === "string" && entry[phase], `${lang} ${key}.${phase}`);
      }
      assert.notEqual(entry.failed, entry.done, `${lang} ${key} failed must differ from done`);
    }
  }
});

test("only real send tools are labelled Send", () => {
  for (const n of ["email_send", "message_post", "slack.post_message", "reply_all", "tweet", "publish_page"]) {
    assert.equal(isSendTool(n), true, n);
  }
  for (const n of ["email_draft", "email_read", "email_search", "email_mark_read", "shell.run", "fs_delete", "message_read"]) {
    assert.equal(isSendTool(n), false, n);
  }
});

test("mcpVerb classifies draft, send, search, read, and unknown actions", () => {
  assert.equal(mcpVerb("draft reply"), "draft");
  assert.equal(mcpVerb("save drafts"), "draft");
  assert.equal(mcpVerb("send message"), "send");
  assert.equal(mcpVerb("post update"), "send");
  assert.equal(mcpVerb("reply thread"), "send");
  assert.equal(mcpVerb("search pull requests"), "search");
  assert.equal(mcpVerb("get file"), "read");
  assert.equal(mcpVerb("list issues"), "read");
  assert.equal(mcpVerb("create Issue"), null);
  assert.equal(describeTool("server.get_file", {}).key, "chat.step.mcp_read");
  assert.equal(describeTool("server.send_message", {}).key, "chat.step.mcp_send");
});
