import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { after, before, test } from "node:test";
import { createElement, type ComponentType } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createServer, type ViteDevServer } from "vite";
import type { ApprovalCardProps } from "../components/ChatCards.tsx";
import type { DockPage } from "../components/ApprovalDock.tsx";
import { approvalKeyAction } from "./approvalKeys.ts";

let server: ViteDevServer;
let ApprovalCard: ComponentType<ApprovalCardProps>;
let ApprovalDock: ComponentType<{ pages: readonly DockPage[] }>;
let enterMayAllow: (tool: ApprovalCardProps["tool"]) => boolean;

before(async () => {
  server = await createServer({
    root: fileURLToPath(new URL("../..", import.meta.url)),
    logLevel: "error",
    optimizeDeps: { noDiscovery: true, entries: [] },
    server: { middlewareMode: true, watch: null, hmr: false },
    ssr: { noExternal: ["ui", "@tauri-apps/api"] }
  });
  await server.ssrLoadModule("/src/i18n/index.ts");
  ({ ApprovalCard, enterMayAllow } = await server.ssrLoadModule("/src/components/ChatCards.tsx"));
  ({ ApprovalDock } = await server.ssrLoadModule("/src/components/ApprovalDock.tsx"));
});
after(async () => {
  await server?.close();
});

const tool = (callId: string, name = "browser_navigate", input: unknown = { url: "https://webioo.com.tr" }) =>
  ({
    kind: "tool",
    key: callId,
    callId,
    requestedMs: 1,
    gated: true,
    name,
    input,
    summary: "",
    status: "awaiting_approval"
  }) as ApprovalCardProps["tool"];

const page = (callId: string, name?: string): DockPage => ({
  key: callId,
  render: (active) =>
    createElement(ApprovalCard, { tool: tool(callId, name), variant: "dock", keyboard: active, onDecide: async () => {} })
});

test("one approval is one docked card with the three answers, no counter", () => {
  const html = renderToStaticMarkup(createElement(ApprovalDock, { pages: [page("c1")] }));
  assert.ok(html.includes('data-testid="approval-dock"'), "the dock is there");
  assert.ok(html.includes("data-approval-dock"), "the keyboard rules find the dock");
  assert.equal((html.match(/data-testid="approval-card"/g) ?? []).length, 1, "exactly one card");
  assert.ok(!html.includes("approval-dock-counter"), "no counter for a single approval");
  assert.ok(html.includes("webioo.com.tr"), "says what it wants to do");
  assert.ok(html.includes("approval-details"), "details are there, collapsed");
  assert.ok(!/<details[^>]*\sopen/.test(html), "details start collapsed");
});

test("several approvals are a counter on one dock, one card shown at a time", () => {
  const html = renderToStaticMarkup(createElement(ApprovalDock, { pages: [page("c1"), page("c2"), page("c3")] }));
  assert.equal((html.match(/data-testid="approval-card"/g) ?? []).length, 1, "one card at a time");
  assert.ok(html.includes("approval-dock-counter"));
  assert.ok(html.includes("Approval 1 of 3"), html);
});

test("an empty dock renders nothing", () => {
  assert.equal(renderToStaticMarkup(createElement(ApprovalDock, { pages: [] })), "");
});

test("a card that must be read does not advertise Enter", () => {
  const read: DockPage = { ...page("c9", "email_send"), keys: "deny" };
  const html = renderToStaticMarkup(createElement(ApprovalDock, { pages: [read] }));
  assert.ok(!html.includes("Allow once</span>"), "no Enter hint");
  assert.ok(html.includes("Deny</span>"), "Esc hint stays");
});

test("the chat asks approvals only in the dock: no inline card, no second strip, a new message is not queued behind it", () => {
  const view = readFileSync(new URL("../views/ChatView.tsx", import.meta.url), "utf8");
  // A pending approval renders nothing in the message stream.
  assert.match(view, /if \(item\.status === "awaiting_approval"\) \{\s*return null;\s*\}/);
  assert.doesNotMatch(view, /status === "awaiting_approval" \? \(\s*<ApprovalCard/);
  // The strip that repeated it is not drawn while the dock asks.
  assert.match(view, /approvalDocked && barModel\?\.kind === "waiting"/);
  // The dock sits in the composer wrapper, directly above the box.
  // (It is always mounted so it can fade out after the last answer; it renders nothing without pages.)
  assert.match(view, /<div className=\{styles\.boxWrap\}[^>]*>\s*<ApprovalDock pages=\{dockPages\}/);
  // A message typed while an approval waits goes to the daemon (which skips the approval).
  // (`queuesLocally` is false while `approvalWaits`, which also holds when the host says the run waits but no card arrived.)
  assert.match(view, /queuesLocally\(\{ runActive: active \|\| offline, approvalWaits: approvalWaits && !offline, slashCommand: body\.startsWith\("\/"\), hasFiles \}\)/);
  assert.match(view, /approvalWaits = pending\.own\.some\(\(e\) => e\.kind === "approval"\) \|\| waitLost/);
});

test("the card shows how much the call can change: a calm chip to look, a red one and its own words to delete", () => {
  const one = (name: string, input: unknown) =>
    renderToStaticMarkup(
      createElement(ApprovalDock, {
        pages: [
          {
            key: "r1",
            render: (active) =>
              createElement(ApprovalCard, { tool: tool("r1", name, input), variant: "dock", keyboard: active, onDecide: async () => {} })
          }
        ]
      })
    );
  const site = one("browser_navigate", { url: "https://webioo.com.tr" });
  assert.match(site, /data-risk="read"/);
  assert.ok(site.includes(">Allow once</button>") || site.includes(">Allow once<"), "a read keeps the once-only Allow label");
  const del = one("fs_delete", { path: "./drafts/old" });
  assert.match(del, /data-risk="delete"/);
  assert.ok(del.includes("Allow delete"), "the delete card names what Allow does");
  assert.ok(del.includes("Deletes"), "and carries the tier chip");
});

const page_body = { tagName: "BODY", closest: () => null };
const enter = { key: "Enter", ctrlKey: false, metaKey: false, altKey: false, shiftKey: false, target: page_body };
const escape = { ...enter, key: "Escape" };
/** What the Enter key does for a card once it is armed (the card on screen at keypress time). */
const enterOn = (card: ApprovalCardProps["tool"]) => approvalKeyAction(enter, { armed: true, canAllow: enterMayAllow(card) });

test("Enter never allows a delete, a shell command or a program start: only a click on Allow does", () => {
  assert.equal(enterOn(tool("d1", "fs_delete", { path: "./drafts/old" })), null);
  assert.equal(enterOn(tool("d2", "pc_fs_delete", { path: "./drafts/old" })), null);
  assert.equal(enterOn(tool("s1", "shell_run", { command: "rm -rf ./drafts" })), null);
  assert.equal(enterOn(tool("s2", "pc_shell_run", { command: "ls" })), null);
  assert.equal(enterOn(tool("p1", "desktop_launch", { executable: "notepad.exe" })), null);
  assert.equal(enterOn(tool("p2", "pc_desktop_launch", { executable: "cmd.exe", args: ["/c", "del x"] })), null);
  assert.equal(enterOn(tool("p3", "use_tool", { name: "shell_run", input: { command: "ls" } })), null);
});

test("Enter does not allow a change, a send, a payment, a secret or a tool the tiers do not know", () => {
  assert.equal(enterOn(tool("w1", "fs_write", { path: "a.txt", content: "x" })), null);
  assert.equal(enterOn(tool("e1", "email_send", { to: "a@b.c", subject: "x" })), null);
  assert.equal(enterOn(tool("y1", "payment_create", {})), null);
  assert.equal(enterOn(tool("k1", "secret_read", { name: "x" })), null);
  assert.equal(enterOn(tool("u1", "some_new_tool", {})), null);
});

test("Enter still allows a look-only card once it is armed, never before, and Esc denies every card", () => {
  const look = tool("l1", "browser_navigate", { url: "https://webioo.com.tr" });
  assert.equal(enterOn(look), "allow");
  assert.equal(enterOn(tool("l2", "fs_read", { path: "notes.txt" })), "allow");
  assert.equal(approvalKeyAction(enter, { armed: false, canAllow: enterMayAllow(look) }), null, "not before the arming delay");
  for (const card of [
    look,
    tool("d1", "fs_delete", { path: "a" }),
    tool("s1", "shell_run", { command: "ls" }),
    tool("p1", "desktop_launch", { executable: "notepad.exe" }),
    tool("e1", "email_send", { to: "a@b.c", subject: "x" })
  ]) {
    assert.equal(approvalKeyAction(escape, { armed: false, canAllow: enterMayAllow(card) }), "deny", card.name);
  }
});

test("the dock hint advertises Enter only for pages Enter may answer, and the keys are bound to the card shown", () => {
  const view = readFileSync(new URL("../views/ChatView.tsx", import.meta.url), "utf8");
  assert.match(view, /keys: enterMayAllow\(tool\) \? "both" : "deny"/, "the hint follows the same rule as the key");
  assert.match(view, /key: entry\.callId,/, "one page per call id");
  assert.match(view, /onDecide=\{decisions\.decide\(entry\.callId\)\}/, "the answer goes to that card's call id");
  const del: DockPage = { ...page("c8", "fs_delete"), keys: enterMayAllow(tool("c8", "fs_delete", { path: "a" })) ? "both" : "deny" };
  const html = renderToStaticMarkup(createElement(ApprovalDock, { pages: [del] }));
  assert.ok(!html.includes("Allow once</span>"), "a delete card shows no Enter hint");
  assert.ok(html.includes("Deny</span>"), "Esc hint stays");
});
