import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { dirtySections, editorProblems, parseFallbackInput } from "./botEditor.ts";
import { draftFromBot } from "./botProfile.ts";
import { isTabStop, trapTarget } from "./focusTrap.ts";
import { createLeaveRegistry } from "./leaveGuard.ts";
import {
  checkSkillInstalled,
  emptySkillDraft,
  flowAfterCheck,
  flowAfterEdit,
  flowDirty,
  skillBodyHash,
  type SkillDraft
} from "./skillDraft.ts";

const read = (rel: string) => readFileSync(new URL(rel, import.meta.url), "utf8");

function draft(over: Partial<SkillDraft> = {}): SkillDraft {
  return { ...emptySkillDraft(), name: "test", description: "Does a thing.", body: "# T\n\nDo it.", ...over };
}
const entry = (over = {}) => ({
  name: "test",
  description: "Does a thing.",
  version: "0.1.0",
  scope: "shared",
  requires: null,
  ...over
});

// --- 4: Manage Skills is guarded ------------------------------------------------------------------

test("Manage Skills registers with the leave guard: dirty picks ask, a save in flight blocks", () => {
  assert.match(read("../components/SkillsModal.tsx"), /useLeaveGuard\(\{/);
  assert.match(read("../components/SkillsModal.tsx"), /inert=\{saving\}/);
  // The registry decides exactly as it does for the edit dialog.
  const reg = createLeaveRegistry();
  let asked = 0;
  let saving = false;
  reg.register({
    state: () => ({ dirty: true, busy: saving }),
    confirm: () => {
      asked++;
    }
  });
  let ran = 0;
  assert.equal(reg.attempt(() => ran++), "pending");
  saving = true;
  assert.equal(reg.attempt(() => ran++), "blocked");
  assert.deepEqual([asked, ran], [1, 0]);
});

// --- 8 / 13 / 18: the installed result belongs to the exact file ------------------------------------

test("a body hash tells this file from an older one with the same metadata", async () => {
  assert.equal(
    await skillBodyHash("  abc\n"),
    "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
  );
  const mine = draft();
  const hash = await skillBodyHash(mine.body);
  const same = checkSkillInstalled(mine, { skills: [{ ...entry(), body_sha256: hash }], issues: [] }, hash);
  assert.deepEqual([same.state, same.verified], ["installed", true]);
  // Same name, version, description, triggers - but other instructions on disk.
  const older = checkSkillInstalled(
    mine,
    { skills: [{ ...entry(), body_sha256: await skillBodyHash("# Old\n\nOther.") }], issues: [] },
    hash
  );
  assert.equal(older.state, "different");
  // A daemon without the fingerprint: it cannot be told apart, so the result is NOT verified.
  const noHash = checkSkillInstalled(mine, { skills: [entry()], issues: [] }, hash);
  assert.deepEqual([noHash.state, noHash.verified], ["installed", false]);
});

test("Check, Back, edit, close: the stale 'installed' result no longer protects nothing", () => {
  let flow = flowAfterEdit();
  assert.equal(flowDirty(flow), true);
  flow = flowAfterCheck(flow, { state: "installed", verified: true });
  assert.equal(flowDirty(flow), false); // the daemon lists this very file
  flow = flowAfterEdit(); // Back, change the instructions (or import another file)
  assert.equal(flow.check, null);
  assert.equal(flowDirty(flow), true); // closing asks again
  // An unverified match (no fingerprint) keeps the guard on.
  assert.equal(flowDirty(flowAfterCheck(flowAfterEdit(), { state: "installed", verified: false })), true);
  // A refused or different file never counts as saved.
  for (const state of ["rejected", "different", "notFound", "failed"] as const) {
    assert.equal(flowDirty(flowAfterCheck(flowAfterEdit(), { state })), true, state);
  }
  const modal = read("../components/AddSkillModal.tsx");
  assert.match(modal, /setFlow\(flowAfterEdit\)/);
  assert.match(modal, /skillBodyHash\(draft\.body\)/);
});

// --- 12 / 20: Delete -----------------------------------------------------------------------------------

test("Delete is always available: it opens the two-step dialog, nothing blocks the button", () => {
  const fields = read("../components/BotEditorFields.tsx");
  assert.doesNotMatch(fields, /deleteBlocked/);
  assert.match(fields, /data-testid="bot-edit-delete"/);
  const modal = read("../views/BotEditModal.tsx");
  assert.match(modal, /<DeleteBotDialog/);
  assert.doesNotMatch(modal, /client\s*\.listRuns\(\)/);
});

// --- 17: typed-but-not-added fallback text -----------------------------------------------------------

test("text typed in the fallback field is part of the draft: it blocks Save and counts as unsaved", () => {
  const d = { ...draftFromBot(undefined), name: "A", executor: { provider: "g", model: "m" } };
  const snap = { draft: d, initialDraft: d, apps: {}, initialApps: {}, skillsDirty: false };
  assert.deepEqual(dirtySections(snap), []);
  assert.deepEqual(dirtySections({ ...snap, pendingFallback: "custom/x" }), ["models"]);
  assert.deepEqual(editorProblems(d, {}, "custom/x"), [{ problem: "fallbackPending", section: "models" }]);
  // An invalid pending text is shown as invalid and still blocks.
  assert.deepEqual(parseFallbackInput("gemini/"), { issue: "noModel" });
  assert.deepEqual(editorProblems(d, {}, "gemini/"), [{ problem: "fallbackPending", section: "models" }]);
  assert.deepEqual(editorProblems(d, {}, "  "), []);
  // The text lives in the dialog (not in the field), so a section change keeps it.
  assert.match(read("../views/BotEditModal.tsx"), /pendingFallback=\{pendingFallback\}/);
  assert.doesNotMatch(read("../components/BotEditorFields.tsx"), /const \[text, setText\]/);
});

// --- 19: inert content is not a Tab stop ------------------------------------------------------------------

test("inert content is excluded from the focus trap; with nothing usable focus stays on the dialog", () => {
  assert.equal(isTabStop({ tabIndex: 0, rects: 1, inert: true }), false);
  assert.equal(isTabStop({ tabIndex: 0, rects: 1, inert: false }), true);
  // While saving the whole body and the tabs are inert and Cancel/Save are disabled: no stops.
  assert.equal(trapTarget([], "section-tab", "dialog", false), "none");
  const modal = read("../components/Modal.tsx");
  assert.match(modal, /closest\("\[inert\]"\)/);
  assert.match(modal, /target === "none"\) \{\s*e\.preventDefault\(\);\s*node\.focus\(\)/);
});

// --- 21: translated scope -----------------------------------------------------------------------------------

test("the 'different skill' message translates the scope", () => {
  assert.match(read("../components/AddSkillModal.tsx"), /t\(`bots\.skills\.scope\.\$\{check\.existing\?\.scope/);
});

// --- engine consent dialog: one column ------------------------------------------------------------

test("the engine consent dialog stacks body above footer inside a column root (the Modal frame is a row)", () => {
  const modalCss = read("../components/Modal.module.css");
  assert.match(modalCss, /\.dialog \{[^}]*display: flex;/, "the frame is a flex row");
  const tsx = read("../state/EngineConsent.tsx");
  const root = tsx.indexOf("dialogStyles.root");
  const body = tsx.indexOf("styles.body");
  const footer = tsx.indexOf("<DialogFooter>");
  assert.ok(root > 0 && root < body && body < footer, "body and footer sit inside the column root, never directly in the Modal");
  const dialogCss = read("../components/Dialog.module.css");
  assert.match(dialogCss, /\.root \{[^}]*flex-direction: column;/);
  assert.match(read("../state/EngineConsent.module.css"), /\.body \{[^}]*flex: 1;[^}]*min-height: var\(--space-0\);/);
});
