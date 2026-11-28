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
  assert.match(html, /data-testid="approval-details-toggle"[^>]*aria-expanded="false"|aria-expanded="false"[^>]*data-testid="approval-details-toggle"/, "the toggle says it is closed");
  assert.match(html, /<div[^>]*hidden=""[^>]*data-testid="approval-details"/, "details start collapsed");
  // One row of answers: no second line under the card repeating "Allow once" / "Deny" next to the keys.
  assert.equal((html.match(/>Deny</g) ?? []).length, 1, "Deny is said once, by its button");
  assert.equal((html.match(/<kbd/g) ?? []).length, 2, "the two key hints, both inside buttons");
});

test("the Details toggle sits with the answers and controls the region it opens", () => {
  const html = renderToStaticMarkup(createElement(ApprovalDock, { pages: [page("c1")] }));
  const controls = /aria-controls="([^"]+)"[^>]*data-testid="approval-details-toggle"/.exec(html)?.[1];
  assert.ok(controls, html);
  assert.ok(html.includes(`id="${controls}"`), "the region has the id the toggle names");
  const actions = html.slice(0, html.indexOf('data-testid="approval-details"'));
  assert.ok(actions.lastIndexOf("approval-details-toggle") > actions.lastIndexOf(">Deny"), "after Deny, in the same row");
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

test("the card on screen shows its keys inside its buttons: Enter on Allow, Esc on Deny", () => {
  const html = renderToStaticMarkup(createElement(ApprovalDock, { pages: [page("c1")] }));
  assert.ok(html.includes('data-testid="approval-key-allow"'), "Enter hint on Allow");
  assert.ok(html.includes('data-testid="approval-key-deny"'), "Esc hint on Deny");
  assert.ok(html.includes('aria-keyshortcuts="Enter"') && html.includes('aria-keyshortcuts="Escape"'), "the keys are in the accessible description");
  // The hint is decoration: the button's name stays "Allow once" / "Deny" for screen readers and tests.
  assert.match(html, /<kbd[^>]*aria-hidden="true"[^>]*data-testid="approval-key-allow"/);
});

test("a card that must be read does not advertise Enter", () => {
  const html = renderToStaticMarkup(createElement(ApprovalDock, { pages: [page("c9", "email_send")] }));
  assert.ok(!html.includes("approval-key-allow"), "no Enter hint");
  assert.ok(!html.includes('aria-keyshortcuts="Enter"'), "and no Enter shortcut");
  assert.ok(html.includes("approval-key-deny"), "Esc hint stays");
});

test("a card that does not answer the keys shows no key hints", () => {
  const html = renderToStaticMarkup(
    createElement(ApprovalCard, { tool: tool("c7"), variant: "dock", keyboard: false, onDecide: async () => {} })
  );
  assert.ok(!html.includes("<kbd"), html);
  assert.ok(!html.includes("aria-keyshortcuts"));
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

// The card itself has the focus (a node that is its own `[data-approval-card]`).
const focusedCard = { tagName: "DIV", closest: (sel: string): unknown => (sel === "[data-approval-card]" ? focusedCard : null) };
const enter = { key: "Enter", ctrlKey: false, metaKey: false, altKey: false, shiftKey: false, target: focusedCard };
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

test("the Enter hint follows the same rule as the key, and the keys are bound to the card shown", () => {
  const view = readFileSync(new URL("../views/ChatView.tsx", import.meta.url), "utf8");
  assert.match(view, /key: entry\.callId,/, "one page per call id");
  assert.match(view, /keyboard=\{active\}/, "only the page on screen answers the keys");
  assert.match(view, /onDecide=\{decisions\.decide\(entry\.callId\)\}/, "the answer goes to that card's call id");
  for (const [name, input] of [
    ["fs_delete", { path: "a" }],
    ["shell_run", { command: "ls" }],
    ["browser_navigate", { url: "https://webioo.com.tr" }],
    ["fs_read", { path: "notes.txt" }]
  ] as const) {
    const card = tool("k1", name, input);
    const html = renderToStaticMarkup(
      createElement(ApprovalCard, { tool: card, variant: "dock", keyboard: true, onDecide: async () => {} })
    );
    assert.equal(html.includes("approval-key-allow"), enterMayAllow(card), name);
    assert.ok(html.includes("approval-key-deny"), `${name}: Esc denies every card`);
  }
});

const keyed = (over: Partial<ApprovalCardProps>, name = "browser_navigate", input: unknown = { url: "https://webioo.com.tr" }, details?: unknown) =>
  renderToStaticMarkup(
    createElement(ApprovalCard, {
      tool: { ...tool("z1", name, input), details } as ApprovalCardProps["tool"],
      variant: "dock",
      keyboard: true,
      onDecide: async () => {},
      ...over
    })
  );

test("the card the keys answer is a focus stop of its own: no other card is, and nothing is auto-focused", () => {
  const on = keyed({});
  assert.match(on, /<div[^>]*data-approval-card=""/, "the key rules find the card");
  assert.match(on, /<div[^>]*tabindex="0"[^>]*data-approval-card=""|<div[^>]*data-approval-card=""[^>]*tabindex="0"/, "the card can be reached with Tab");
  const off = keyed({ keyboard: false });
  assert.doesNotMatch(off, /<div[^>]*data-approval-card=""[^>]*tabindex|<div[^>]*tabindex="[^"]*"[^>]*data-approval-card=""/, "a card that answers no keys takes no focus stop");
  // The keys are handled on the card (so only while the focus is inside it), not on the window,
  // and neither the card nor Allow is focused for the person.
  const source = readFileSync(new URL("../components/ChatCards.tsx", import.meta.url), "utf8");
  const approval = source.slice(source.indexOf("export function ApprovalCard("));
  assert.doesNotMatch(approval, /autoFocus|\.focus\(\)/);
  assert.doesNotMatch(approval, /window\.addEventListener\("keydown"/);
  assert.match(approval, /onKeyDown=\{onKeyDown\}/);
  const dock = readFileSync(new URL("../components/ApprovalDock.tsx", import.meta.url), "utf8");
  assert.doesNotMatch(dock, /autoFocus|\.focus\(\)/);
});

test("a card that cannot be answered shows no key it would not obey", () => {
  const expired = keyed({ expiresAtMs: Date.now() - 5000 });
  assert.ok(!expired.includes("aria-keyshortcuts"), "an expired card: neither Enter nor Esc");
  assert.ok(!expired.includes("<kbd"));
  const review = keyed({}, "http_request", { url: "https://x.example" }, {
    kind: "egress",
    host: "x.example",
    payload: "secret",
    review_full_payload: true
  });
  assert.ok(!review.includes("approval-key-allow") && !review.includes('aria-keyshortcuts="Enter"'), "a payload still to review: no Enter");
  assert.ok(review.includes("approval-key-deny"), "Deny is still enabled, so Esc stays");
  const unrenderable = keyed({}, "app_action", {}, { kind: "app_action" });
  assert.ok(!unrenderable.includes("approval-key-allow") && !unrenderable.includes('aria-keyshortcuts="Enter"'), "a card that cannot be drawn: no Enter");
  assert.match(unrenderable, /<button[^>]*disabled=""[^>]*>Allow once/, "and its Allow is disabled");
});
