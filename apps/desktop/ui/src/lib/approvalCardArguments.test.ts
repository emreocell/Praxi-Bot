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
  assert.ok(!del.includes("fs_delete"), "no raw tool id on a compact delete card");
  assert.ok(del.includes("C:/Users/me/Documents/taxes-2025.xlsx"), "path");

  const mail = render("email_send", { to: "boss@example.test", subject: "Resignation", body: "I quit." });
  assert.ok(mail.includes("boss@example.test") && mail.includes("Resignation") && mail.includes("I quit."));

  const mcp = render("use_tool", {
    name: "mcp:gmail/send_message",
    input: { to: "ceo@example.test", text: "wire the money" }
  });
  assert.ok(!mcp.includes("mcp:gmail"), "no raw MCP tool id on the card");
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
  assert.ok(html.includes("Allow for this task"), "task scope offered inside details");
  assert.ok(!html.includes('data-testid="approval-always"'), "no standing grant next to private data");
  assert.ok(html.includes("Full input"), "input label is plain, without a raw tool id");
  assert.ok(!html.includes("mcp:crm/update Full input"), "tool name is not repeated in the label");

  const clean = render(
    "use_tool",
    { name: "mcp:crm/update", input: { id: 1 } },
    { why: "policy", once_only: false, run_grant: true, always_offered: true }
  );
  assert.ok(clean.includes("Allow for this task"));
  // An MCP tool never shows a standing Always allow, whatever its name: only a site does.
  assert.ok(!clean.includes('data-testid="approval-always"'), "no Always for an MCP tool");
  assert.ok(!clean.includes(">This chat<"), "this task replaces this chat");
  // Destructive actions never offer Always allow.
  const del = render("fs_delete", { path: "/Users/emre/tmp/old.txt" }, { why: "policy", once_only: false, always_offered: true });
  assert.ok(!del.includes('data-testid="approval-always"'), "destructive fs_delete hides Always allow");
  // A site approval offers Allow, Always allow, and Deny on one compact card.
  const site = render(
    "browser_open",
    { url: "https://webioo.com.tr/dashboard" },
    { why: "policy", once_only: false, always_offered: true, memory: { kind: "origin", pattern: "https://webioo.com.tr" } }
  );
  assert.ok(site.includes('data-testid="approval-always"'), "site approval offers Always allow");
  assert.ok(site.includes('data-testid="approval-details"'), "site approval provides Details toggle");
  // A tool name alone never makes a call a browser action.
  const unmarked = render("use_tool", { name: "mcp:other/browser_navigate", input: { url: "https://example.com" } }, {
    why: "policy",
    once_only: false
  });
  assert.ok(!unmarked.includes("in your browser."), "only the daemon's marker words a browser action");
});
