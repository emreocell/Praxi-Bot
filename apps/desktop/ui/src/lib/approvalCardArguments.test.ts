import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { after, before, test } from "node:test";
import { createElement, type ComponentType } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createServer, type ViteDevServer } from "vite";
import type { ApprovalCardProps } from "../components/ChatCards.tsx";

let server: ViteDevServer;
let ApprovalCard: ComponentType<ApprovalCardProps>;

before(async () => {
  server = await createServer({
    root: fileURLToPath(new URL("../..", import.meta.url)),
    logLevel: "error",
    optimizeDeps: { noDiscovery: true, entries: [] },
    server: { middlewareMode: true, watch: null, hmr: false },
    ssr: { noExternal: ["ui", "@tauri-apps/api"] }
  });
  await server.ssrLoadModule("/src/i18n/index.ts");
  ({ ApprovalCard } = await server.ssrLoadModule("/src/components/ChatCards.tsx"));
});
after(async () => {
  await server?.close();
});

const render = (name: string, input: unknown, details?: unknown): string =>
  renderToStaticMarkup(
    createElement(ApprovalCard, {
      tool: {
        kind: "tool",
        key: "c1",
        callId: "c1",
        requestedMs: 1,
        gated: true,
        name,
        input,
        details,
        summary: "",
        status: "awaiting_approval"
      } as ApprovalCardProps["tool"],
      onDecide: async () => {}
    })
  );

test("approval cards for fs_delete, email_send and an MCP send show every argument", () => {
  const del = render("fs_delete", { path: "C:/Users/me/Documents/taxes-2025.xlsx" });
  assert.ok(del.includes("fs_delete"), "tool name");
  assert.ok(del.includes("C:/Users/me/Documents/taxes-2025.xlsx"), "path");

  const mail = render("email_send", { to: "boss@example.test", subject: "Resignation", body: "I quit." });
  assert.ok(mail.includes("boss@example.test") && mail.includes("Resignation") && mail.includes("I quit."));

  const mcp = render("use_tool", {
    name: "mcp:gmail/send_message",
    input: { to: "ceo@example.test", text: "wire the money" }
  });
  assert.ok(mcp.includes("gmail.send_message"), "unwrapped tool name");
  assert.ok(mcp.includes("ceo@example.test") && mcp.includes("wire the money"));
});

test("hidden characters in a command and in the input are written as <U+XXXX>, a cut is announced", () => {
  const cmd = render("shell_run", { command: "echo ok\u202Etxt.exe\u200B" });
  assert.ok(cmd.includes("&lt;U+202E&gt;") && cmd.includes("&lt;U+200B&gt;"), cmd);
  assert.ok(!cmd.includes("\u202E") && !cmd.includes("\u200B"));

  const mail = render("email_send", { to: "a@example.test\u2066", body: "x" });
  assert.ok(mail.includes("&lt;U+2066&gt;"));

  assert.ok(render("shell_run", { command: "x".repeat(2500) }).includes("approval-command-shortened"));
  assert.ok(!render("shell_run", { command: "ls" }).includes("approval-command-shortened"));
  assert.ok(render("fs_write", { path: "a", content: "y".repeat(5000) }).includes("approval-input-shortened"));
});

test("a browser card says what happens once and offers once, this task and deny", () => {
  const html = render(
    "use_tool",
    { name: "mcp:playwright/browser_navigate", input: { url: "https://example.com/start" } },
    {
      kind: "egress",
      reason: "this run read file notes.txt, which includes private data, and now wants to open https://example.com/start in your browser (playwright)",
      reason_code: "egress_browser",
      read: ["file notes.txt"],
      host: "example.com",
      payload: "url: https://example.com/start",
      payload_kind: "body",
      payload_lines: 1,
      payload_chars: 30,
      hidden_chars: 0,
      payload_bytes: 30,
      review_full_payload: false,
      why: "egress",
      once_only: true,
      run_grant: true,
      always_offered: false,
      browser_action: true
    }
  );
  assert.ok(html.includes("Praxi wants to open https://example.com/start in your browser."), html);
  assert.ok(html.includes("Allow for this task"), "task scope offered");
  assert.ok(!html.includes("Always allow for this bot"), "no standing grant next to private data");
  // The tool name is not repeated in front of the sentence that already names it.
  const label = /Exactly what (\S+) will receive/.exec(html);
  assert.ok(label, "input label");
  assert.equal(html.split(`${label[1]} Exactly what`).length, 1, "no duplicated tool name");

  const clean = render(
    "use_tool",
    { name: "mcp:crm/update", input: { id: 1 } },
    { why: "policy", once_only: false, run_grant: true, always_offered: true }
  );
  assert.ok(clean.includes("Allow for this task") && clean.includes("Always allow for this bot (this tool)"));
  assert.ok(!clean.includes(">This chat<"), "this task replaces this chat");
  // A tool name alone never makes a call a browser action.
  const unmarked = render("use_tool", { name: "mcp:other/browser_navigate", input: { url: "https://example.com" } }, {
    why: "policy",
    once_only: false
  });
  assert.ok(!unmarked.includes("in your browser."), "only the daemon's marker words a browser action");
});
