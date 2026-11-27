import assert from "node:assert/strict";
import { test } from "node:test";
import { approvalRisk } from "./approvalRisk.ts";

test("opening a site only looks: the calm tier", () => {
  assert.equal(approvalRisk("browser_navigate", { url: "https://webioo.com.tr" }), "read");
  assert.equal(approvalRisk("fs_read", { path: "notes.txt" }), "read");
});

test("a shell command, a delete and a mail send are told apart", () => {
  assert.equal(approvalRisk("shell_run", { command: "rm -rf ./drafts/old" }), "run");
  assert.equal(approvalRisk("fs_delete", { path: "./drafts/old" }), "delete");
  assert.equal(approvalRisk("email_send", { to: "ekip@praxi.dev", subject: "Report" }), "send");
});

test("a wrapped call is judged by the tool it runs, not by use_tool", () => {
  assert.equal(approvalRisk("use_tool", { name: "fs_delete", input: { path: "a" } }), "delete");
});

test("writing a file or changing a bot is a change; a tool the tiers do not know stays unmarked", () => {
  assert.equal(approvalRisk("fs_write", { path: "a.txt", content: "x" }), "change");
  assert.equal(approvalRisk("bot_update", {}, { kind: "bot", action: "update", profile: {} }), "change");
  assert.equal(approvalRisk("bot_update", {}, { kind: "bot", action: "delete", profile: {} }), "delete");
  assert.equal(approvalRisk("some_new_tool", {}), null);
});

test("document and git tools: reading looks, editing and committing change", () => {
  for (const tool of ["docx_read", "xlsx_read", "pptx_read", "csv_read", "git_diff", "git_status"]) {
    assert.equal(approvalRisk(tool, { path: "a" }), "read", tool);
  }
  for (const tool of ["docx_write", "docx_append", "xlsx_write", "xlsx_append_rows", "xlsx_update_cells", "git_commit"]) {
    assert.equal(approvalRisk(tool, { path: "a" }), "change", tool);
  }
});

test("data that leaves the computer is a send even when the tool name is plain", () => {
  assert.equal(
    approvalRisk("web_extract", { url: "https://example.com" }, { kind: "egress", host: "example.com", read: ["file a"], payload: "x" }),
    "send"
  );
});

test("a schedule card that carries an egress review is still a change, never a send", () => {
  const details = {
    kind: "schedule",
    action: "create",
    name: "Daily quote",
    prompt: "Tell me a quote",
    schedule: "Every day at 09:00",
    timezone: "Europe/Istanbul",
    egress: { kind: "egress", reason: "egress", host: "this bot's scheduler", payload: "x" }
  };
  assert.equal(approvalRisk("schedule_create", {}, details), "change");
});
