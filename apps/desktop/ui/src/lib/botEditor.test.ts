import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import {
  addFallback,
  BOT_EDITOR_SECTIONS,
  closeAction,
  customRef,
  customRefIssue,
  dirtySections,
  editorProblems,
  editorSection,
  fallbackList,
  permissionTone,
  removeFallback,
  SEPARATE_SAVE,
  sectionAtScroll
} from "./botEditor.ts";
import { type BotDraft, draftFromBot } from "./botProfile.ts";

const read = (rel: string) => readFileSync(new URL(rel, import.meta.url), "utf8");

function base(): BotDraft {
  return {
    ...draftFromBot(undefined),
    name: "Atlas",
    executor: { provider: "gemini", model: "gemini-3.5-flash-lite" }
  };
}

const snap = (draft: BotDraft, over: Partial<Parameters<typeof dirtySections>[0]> = {}) => ({
  draft,
  initialDraft: base(),
  apps: { github: true },
  initialApps: { github: true },
  skillsDirty: false,
  ...over
});

test("an untouched draft is clean", () => {
  const dirty = (d: BotDraft) => dirtySections(snap(d)).length > 0;
  assert.equal(dirty(base()), false);
  // Cosmetic differences do not count as edits.
  assert.equal(dirty({ ...base(), name: "Atlas  " }), false);
  assert.equal(dirty({ ...base(), color: base().color.toUpperCase() }), false);
  assert.equal(dirty({ ...base(), fallback: " " }), false);
});

test("each kind of edit marks its own section", () => {
  assert.deepEqual(dirtySections(snap({ ...base(), persona: "Careful." })), ["instructions"]);
  assert.deepEqual(
    dirtySections(snap({ ...base(), planner: { provider: "openai", model: "gpt-5" } })),
    ["models"]
  );
  assert.deepEqual(dirtySections(snap({ ...base(), fallback: "openai/gpt-5" })), ["models"]);
  assert.deepEqual(dirtySections(snap({ ...base(), preset: "full" })), ["permissions"]);
  const cats = { ...base().categories, shell: "deny" as const };
  assert.deepEqual(dirtySections(snap({ ...base(), categories: cats })), ["permissions"]);
  assert.deepEqual(dirtySections(snap(base(), { skillsDirty: true })), ["apps"]);
  assert.deepEqual(dirtySections(snap(base(), { apps: { github: false } })), ["apps"]);
  // An app that appeared unticked is not a change.
  assert.deepEqual(dirtySections(snap(base(), { apps: { github: true, slack: false } })), []);
  assert.deepEqual(dirtySections(snap({ ...base(), avatar: "data:image/png;base64,AA" })), ["identity"]);
});

test("save is blocked by a missing name or executor, pointing at the right section", () => {
  assert.deepEqual(editorProblems(base()), []);
  assert.deepEqual(editorProblems({ ...base(), name: "  " }), [
    { problem: "nameMissing", section: "identity" }
  ]);
  assert.deepEqual(
    editorProblems({ ...base(), name: "", executor: { provider: "gemini", model: "" } }),
    [
      { problem: "nameMissing", section: "identity" },
      { problem: "executorMissing", section: "models" }
    ]
  );
});

test("closing asks first when dirty and waits while saving", () => {
  assert.equal(closeAction(false, false), "close");
  assert.equal(closeAction(true, false), "confirm");
  assert.equal(closeAction(true, true), "wait");
  assert.equal(closeAction(false, true), "wait");
});

test("the page highlights the section scrolled to, and the last one at the end", () => {
  const tops = [0, 400, 900, 1500];
  const at = (top: number, scrollHeight = 3000) => sectionAtScroll(tops, { top, height: 600, scrollHeight });
  assert.equal(at(0), 0);
  assert.equal(at(380), 1); // within the margin: the next title is about to reach the top
  assert.equal(at(1000), 2);
  assert.equal(at(1600), 3);
  // Scrolled to the very end: a short last section counts even though its top never got there.
  assert.equal(at(1300, 1900), 3);
  assert.equal(sectionAtScroll([], { top: 0, height: 600, scrollHeight: 600 }), 0);
  assert.equal(new Set(BOT_EDITOR_SECTIONS).size, BOT_EDITOR_SECTIONS.length);
});

test("sections that save on their own are real sections of the page", () => {
  for (const s of Object.keys(SEPARATE_SAVE)) assert.ok((BOT_EDITOR_SECTIONS as readonly string[]).includes(s));
});

test("only known section names are accepted from a URL", () => {
  assert.equal(editorSection("models"), "models");
  assert.equal(editorSection("advanced"), "advanced");
  assert.equal(editorSection("general"), undefined);
  assert.equal(editorSection(null), undefined);
});

test("fallback models: ordered, de-duplicated, add and remove", () => {
  assert.deepEqual(fallbackList(" a/x, b/y ,, a/x "), ["a/x", "b/y"]);
  assert.equal(addFallback("a/x", "b/y"), "a/x, b/y");
  assert.equal(addFallback("a/x", "a/x"), "a/x");
  assert.equal(addFallback("", "a/x"), "a/x");
  assert.equal(removeFallback("a/x, b/y", "a/x"), "b/y");
  assert.equal(removeFallback("a/x", "a/x"), "");
});

test("custom model ids are checked before they reach the draft", () => {
  assert.equal(customRefIssue(""), "empty");
  assert.equal(customRefIssue("gemini/ gemini 3"), "spaces");
  assert.equal(customRefIssue("gemini/"), "noModel");
  assert.equal(customRefIssue("gemini/gemini-3.5-flash-lite"), null);
  assert.equal(customRefIssue("my-model"), null);
  assert.deepEqual(customRef(" openrouter/meta-llama/llama-3.3 "), {
    provider: "openrouter",
    model: "meta-llama/llama-3.3"
  });
  assert.deepEqual(customRef("my-model"), { provider: "", model: "my-model" });
  assert.equal(customRef("gemini/"), null);
});

test("permission chip tone follows what runs without asking", () => {
  assert.equal(permissionTone("read-only"), "locked");
  assert.equal(permissionTone("approve-destructive"), "guarded");
  assert.equal(permissionTone("read-and-draft"), "guarded");
  assert.equal(permissionTone("full"), "open");
  assert.equal(permissionTone("custom"), "custom");
});

// Wiring: the dialog must decide through these functions, and must not fall back to native
// selects or a free-text model box outside the Advanced section.
test("BotEditModal is driven by the tested rules", () => {
  const modal = read("../views/BotEditModal.tsx");
  for (const fn of ["closeAction(", "dirtySections(", "editorProblems(", "sectionAtScroll("]) {
    assert.ok(modal.includes(fn), `BotEditModal uses ${fn}`);
  }
  assert.doesNotMatch(modal, /<select/);
  const fields = read("../components/BotEditorFields.tsx");
  for (const fn of ["addFallback(", "removeFallback(", "customRefIssue(", "fallbackList("]) {
    assert.ok(fields.includes(fn), `BotEditorFields uses ${fn}`);
  }
  assert.doesNotMatch(fields, /<select/);
  assert.match(fields, /<ModelPicker/);
});
