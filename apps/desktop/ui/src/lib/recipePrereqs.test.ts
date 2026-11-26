import assert from "node:assert/strict";
import { test } from "node:test";
import type { EmailAccount } from "../api/types.ts";
import { blockingPrereqs, canStart, checkPrereqs, type PrereqFacts } from "./recipePrereqs.ts";
import type { RecipeNeed } from "./recipes.ts";

const mailbox = (over: Partial<EmailAccount> = {}): EmailAccount =>
  ({ name: "bot-mail", address: "bot@example.com", bot: null, shared_with: [], default_for: [], ...over }) as unknown as EmailAccount;

const facts = (over: Partial<PrereqFacts> = {}): PrereqFacts => ({
  hasModel: true,
  hasBrowser: true,
  mailboxes: [],
  servers: [],
  composioApps: [],
  ...over
});

const model: RecipeNeed = { kind: "model" };
const browser: RecipeNeed = { kind: "browser" };
const email: RecipeNeed = { kind: "email", purpose: "own" };
const calendar: RecipeNeed = { kind: "app", apps: ["googlecalendar", "outlook"], label: { en: "A calendar app" }, optional: true };

test("each need reads its own fact and keeps the recipe's order", () => {
  const res = checkPrereqs([model, browser, email], facts({ hasBrowser: false }));
  assert.deepEqual(res.map((r) => [r.need.kind, r.state]), [["model", "ok"], ["browser", "missing"], ["email", "missing"]]);
});

test("a met need has no fix; a missing one points at the screen that fixes it", () => {
  const res = checkPrereqs([model, browser, email, calendar], facts({ hasModel: false, hasBrowser: false }));
  assert.deepEqual(res.map((r) => r.fix), [
    { kind: "settings", section: "ai" },
    { kind: "settings", section: "computer" },
    { kind: "settings", section: "mail" },
    { kind: "apps" }
  ]);
  assert.equal(checkPrereqs([model], facts())[0]!.fix, undefined);
});

test("a mailbox counts when any bot may use it, not when it belongs to another bot", () => {
  assert.equal(checkPrereqs([email], facts({ mailboxes: [mailbox()] }))[0]!.state, "ok");
  assert.equal(checkPrereqs([email], facts({ mailboxes: [mailbox({ bot: "11111111-1111-1111-1111-111111111111" })] }))[0]!.state, "missing");
});

test("a connected app is found by Composio slug or name, or by a connected server; disabled or pending ones do not count", () => {
  const app = (slug: string, state = "active", enabled = true) => ({ slug, name: slug, state, enabled }) as never;
  assert.equal(checkPrereqs([calendar], facts({ composioApps: [app("googlecalendar")] }))[0]!.state, "ok");
  assert.equal(checkPrereqs([calendar], facts({ composioApps: [app("google_calendar")] }))[0]!.state, "ok");
  assert.equal(checkPrereqs([calendar], facts({ composioApps: [app("googlecalendar", "pending")] }))[0]!.state, "missing");
  assert.equal(checkPrereqs([calendar], facts({ composioApps: [app("googlecalendar", "active", false)] }))[0]!.state, "missing");
  assert.equal(checkPrereqs([calendar], facts({ composioApps: [app("github")] }))[0]!.state, "missing");
  const server = (name: string, status = "connected", enabled = true) => ({ name, status, enabled }) as never;
  assert.equal(checkPrereqs([calendar], facts({ servers: [server("Outlook Calendar")] }))[0]!.state, "ok");
  assert.equal(checkPrereqs([calendar], facts({ servers: [server("outlook", "error")] }))[0]!.state, "missing");
});

test("a fact that could not be read is unknown, never met, and does not block", () => {
  const res = checkPrereqs([model, browser, email, { kind: "app", apps: ["x"], label: { en: "x" } }], facts({ hasModel: null, hasBrowser: null, mailboxes: null, servers: null, composioApps: null }));
  assert.ok(res.every((r) => r.state === "unknown"));
  assert.equal(canStart(res), true);
  // One list failing is open too, unless the other already said yes.
  assert.equal(checkPrereqs([calendar], facts({ servers: null }))[0]!.state, "unknown");
  assert.equal(checkPrereqs([calendar], facts({ servers: null, composioApps: [{ slug: "googlecalendar", name: "Google Calendar", state: "active", enabled: true }] as never }))[0]!.state, "ok");
});

test("only a required need that is known to be missing blocks the start", () => {
  const res = checkPrereqs([model, calendar, email], facts({ hasModel: false }));
  assert.deepEqual(blockingPrereqs(res).map((r) => r.need.kind), ["model", "email"]);
  assert.equal(canStart(res), false);
  const optionalOnly = checkPrereqs([model, calendar], facts());
  assert.equal(optionalOnly[1]!.state, "missing");
  assert.equal(canStart(optionalOnly), true);
});
