import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { createLeaveRegistry, type LeaveGuard } from "./leaveGuard.ts";

const read = (rel: string) => readFileSync(new URL(rel, import.meta.url), "utf8");

function guard(dirty: boolean, busy: boolean, asked: string[] = [], name = "g", agree = true): LeaveGuard {
  return {
    state: () => ({ dirty, busy }),
    confirm: (proceed) => {
      asked.push(name);
      if (agree) proceed();
    }
  };
}

test("with nothing unsaved the action runs at once", () => {
  const reg = createLeaveRegistry();
  reg.register(guard(false, false));
  let ran = 0;
  assert.equal(reg.attempt(() => ran++), "went");
  assert.equal(ran, 1);
});

test("unsaved data asks first and the action waits for the answer", () => {
  const reg = createLeaveRegistry();
  const asked: string[] = [];
  let answer: (() => void) | null = null;
  reg.register({
    state: () => ({ dirty: true, busy: false }),
    confirm: (proceed) => {
      asked.push("edit");
      answer = proceed;
    }
  });
  let ran = 0;
  assert.equal(reg.attempt(() => ran++), "pending");
  assert.equal(ran, 0);
  assert.deepEqual(asked, ["edit"]);
  answer!(); // the user chose "discard"
  assert.equal(ran, 1);
});

test("keeping the edits (no answer) means the route never changes", () => {
  const reg = createLeaveRegistry();
  reg.register({ state: () => ({ dirty: true, busy: false }), confirm: () => {} });
  let ran = 0;
  reg.attempt(() => ran++);
  assert.equal(ran, 0);
});

test("a save in flight blocks every route change, dirty or not", () => {
  const reg = createLeaveRegistry();
  reg.register(guard(false, true));
  let ran = 0;
  assert.equal(reg.attempt(() => ran++), "blocked");
  assert.equal(ran, 0);
});

test("stacked dialogs ask top first, then the next, then go", () => {
  const reg = createLeaveRegistry();
  const asked: string[] = [];
  reg.register(guard(true, false, asked, "edit"));
  reg.register(guard(true, false, asked, "add-skill"));
  let ran = 0;
  reg.attempt(() => ran++);
  assert.deepEqual(asked, ["add-skill", "edit"]);
  assert.equal(ran, 1);
});

test("an unregistered (closed) dialog no longer guards", () => {
  const reg = createLeaveRegistry();
  const off = reg.register(guard(true, true));
  off();
  let ran = 0;
  assert.equal(reg.attempt(() => ran++), "went");
  assert.equal(ran, 1);
});

test("silent navigation blocks dirty and busy work without changing normal guard behavior", () => {
  const reg = createLeaveRegistry();
  const asked: string[] = [];
  const releaseDraft = reg.register(guard(true, false, asked, "draft"), true);
  const releaseEdit = reg.register(guard(false, true, asked, "edit"));
  let ran = 0;
  assert.equal(reg.attemptSilently(() => ran++), "blocked");
  releaseEdit();
  assert.equal(reg.attemptSilently(() => ran++), "blocked");
  assert.deepEqual(asked, []);
  assert.equal(reg.attempt(() => ran++), "went");
  releaseDraft();
  assert.equal(reg.attemptSilently(() => ran++), "went");
  assert.equal(ran, 2);
});

// Wiring (source level - the UI tests run without a DOM): every route change of the shell goes
// through `go`, the bot dialogs register a guard, and the palette / panel use the same gate.
test("App routes every user navigation through the leave gate", () => {
  const app = read("../App.tsx");
  // External deep links use the silent gate. The mock review and post-delete routes are internal.
  const direct = app.split("\n").filter((l) => /\bsetRoute\(/.test(l));
  const allowed = direct.filter(
    (l) =>
      l.includes("leave.attempt(() => setRoute(next))") ||
      l.includes('setRoute(destination.kind === "chat" ? { ...destination, external: true } : destination)') ||
      l.includes('setRoute({ kind: "home" });') ||
      /screen === "(bot|myBots|schedules|needs|notifications|home)/.test(l) ||
      l.includes('kind: "chat", chatId: "33333333') ||
      l.includes('setRoute({ kind: "onboarding" });')
  );
  assert.deepEqual(direct.filter((l) => !allowed.includes(l)), []);
  assert.match(app, /leave\.attemptSilently\(\(\) => \{[\s\S]*?setRoute\(destination/);
  assert.match(app, /leave\.attempt\(\(\) => \{\s*setPanelOpen\(false\)/);
  for (const dialog of ["BotEditModal.tsx", "../components/AddSkillModal.tsx"]) {
    const src = read(dialog.startsWith("..") ? dialog : `../views/${dialog}`);
    assert.match(src, /useLeaveGuard\(/, dialog);
  }
});
