import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { after, before, test } from "node:test";
import { createServer, type ViteDevServer } from "vite";

type Mod = typeof import("./gettingStarted.ts");

let server: ViteDevServer;
let m: Mod;

before(async () => {
  server = await createServer({
    root: fileURLToPath(new URL("../..", import.meta.url)),
    logLevel: "error",
    optimizeDeps: { noDiscovery: true, entries: [] },
    server: { middlewareMode: true, watch: null }
  });
  m = (await server.ssrLoadModule("/src/lib/gettingStarted.ts")) as Mod;
});

after(async () => {
  await server?.close();
});

const read = (p: string) => readFileSync(new URL(p, import.meta.url), "utf8");

const NONE = { hasModel: false, runCount: 0, decidedApprovals: 0, appCount: 0, scheduleCount: 0, serverCount: 0 };

test("every step has real text in en, tr and de, with the same ids", () => {
  const ids = m.STEP_DEFS.map((d) => d.id);
  assert.equal(ids.length, 6);
  const en = m.stepTextFor("en");
  for (const lang of ["tr", "de"]) {
    const text = m.stepTextFor(lang);
    assert.deepEqual(text.map((x) => x.id), ids);
    for (const [i, item] of text.entries()) {
      assert.notEqual(item.title, en[i]!.title, `${lang}/${item.id} title is still English`);
      assert.notEqual(item.body, en[i]!.body, `${lang}/${item.id} body is still English`);
      assert.ok(item.action.length > 0);
    }
  }
  assert.deepEqual(m.stepTextFor("en").map((x) => x.id), ids);
});

test("nothing is done on a fresh install or while the app cannot say", () => {
  const fresh = m.buildSteps("en", NONE);
  assert.equal(fresh.some((s) => s.done), false);
  assert.deepEqual(m.progressOf(fresh), { done: 0, total: 5, complete: false, next: "model" });
  const unknown = m.buildSteps("en", { hasModel: null, runCount: null, decidedApprovals: null, appCount: null, scheduleCount: null, serverCount: null });
  assert.equal(unknown.some((s) => s.done), false);
});

test("each step completes from its own real signal", () => {
  const done = (patch: object, id: string) => m.buildSteps("en", { ...NONE, ...patch }).find((s) => s.id === id)!.done;
  assert.equal(done({ hasModel: true }, "model"), true);
  assert.equal(done({ runCount: 1 }, "message"), true);
  assert.equal(done({ decidedApprovals: 1 }, "approval"), true);
  assert.equal(done({ appCount: 1 }, "app"), true);
  assert.equal(done({ scheduleCount: 1 }, "schedule"), true);
  assert.equal(done({ serverCount: 1 }, "server"), true);
  assert.equal(done({ runCount: 1 }, "approval"), false);
});

test("the optional server step never blocks the finish; the next step skips it", () => {
  const all = m.buildSteps("en", { hasModel: true, runCount: 2, decidedApprovals: 1, appCount: 1, scheduleCount: 1, serverCount: 0 });
  assert.deepEqual(m.progressOf(all), { done: 5, total: 5, complete: true, next: null });
  const part = m.buildSteps("en", { ...NONE, hasModel: true, runCount: 1 });
  assert.equal(m.progressOf(part).next, "approval");
});

test("the approval step does not promise a card to a bot that runs at the Full level", () => {
  const body = (botsAsk: boolean | null | undefined, lang = "en") =>
    m.buildSteps(lang, { ...NONE, botsAsk }).find((s) => s.id === "approval")!.body;
  assert.match(body(true), /stop and ask you first/);
  assert.match(body(null), /stop and ask you first/);
  assert.match(body(false), /Full level/);
  assert.doesNotMatch(body(false), /create a file/);
  for (const lang of ["tr", "de"]) {
    assert.notEqual(body(false, lang), body(true, lang), `${lang} has a Full-level wording`);
  }
  assert.equal(m.botsAsk([]), null);
  assert.equal(m.botsAsk([{ config: { permissions: { preset: "full" } } }]), false);
  assert.equal(m.botsAsk([{ config: { permissions: { preset: "full" } } }, { config: {} }]), true);
  assert.equal(m.botsAsk([{ config: { permissions: { preset: "approve_destructive" } } }]), true);
});

test("a decision made in the app completes the approval step (the daemon lists only waiting cards)", () => {
  const hook = read("../state/useGettingStarted.ts");
  assert.match(hook, /decidedHere \? Math\.max\(1/);
  assert.match(read("../components/PendingDecisions.tsx"), /decideApproval\([^)]*\);\s*markApprovalDecided\(\)/);
});

test("only decided approvals count", () => {
  assert.equal(m.countDecided([{ decision: null }, {}]), 0);
  assert.equal(m.countDecided([{ decision: null }, { decision: "approved" }, { decision: "denied" }]), 2);
});

test("the checklist shows only on the empty chat and in Help, can be put away and back, and is accessible", () => {
  const view = read("../components/GettingStarted.tsx");
  assert.match(view, /variant === "start" && hidden\) \{\s*return null/);
  assert.match(view, /variant === "help" && hidden/);
  assert.match(view, /role="progressbar"/);
  assert.match(view, /aria-valuenow=\{progress\.done\}/);
  assert.match(view, /aria-labelledby=/);
  assert.match(view, /<ol /);
  const app = read("../App.tsx");
  assert.equal((app.match(/<GettingStarted\b/g) ?? []).length, 2);
  assert.match(app, /gettingStartedHidden: true/);
  assert.match(app, /gettingStartedHidden: false/);
  assert.match(read("../views/BotEmptyView.tsx"), /\{extra\}/);
});

test("steps are read from real daemon state, never guessed", () => {
  const hook = read("../state/useGettingStarted.ts");
  for (const call of ["listRuns", "listApprovals", "listSchedules", "listHosts"]) {
    assert.ok(hook.includes(`client.${call}()`));
  }
  assert.match(hook, /allSettled/);
  assert.match(hook, /dto === null \? null : hasRealModel\(dto\)/);
});
