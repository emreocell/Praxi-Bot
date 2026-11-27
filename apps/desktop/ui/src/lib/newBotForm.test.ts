import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import {
  createFromTemplate,
  GENERAL_CHOICE,
  importLevel,
  levelAfterPick,
  levelsFor,
  templateNeedsOtherModel,
  lookOps,
  nameAfterPick,
  purposeKey,
  PURPOSE_TEMPLATES,
  shuffleLook,
  startLevel
} from "./newBotForm.ts";

const read = (rel: string) => readFileSync(new URL(rel, import.meta.url), "utf8");

test("every choice has a one-line purpose in English and Turkish; an unknown template has none", () => {
  const en = JSON.parse(read("../i18n/locales/en.json"));
  const tr = JSON.parse(read("../i18n/locales/tr.json"));
  for (const id of [GENERAL_CHOICE, ...PURPOSE_TEMPLATES]) {
    const key = purposeKey(id);
    assert.ok(key, id);
    const leaf = key.split(".").reduce((o: Record<string, unknown>, k) => o?.[k] as Record<string, unknown>, en);
    const leafTr = key.split(".").reduce((o: Record<string, unknown>, k) => o?.[k] as Record<string, unknown>, tr);
    assert.equal(typeof leaf, "string", `${key} in en`);
    assert.equal(typeof leafTr, "string", `${key} in tr`);
    // One line: short enough for a small card.
    assert.ok((leaf as unknown as string).length <= 30, `${key} is short in en`);
    assert.ok((leafTr as unknown as string).length <= 30, `${key} is short in tr`);
  }
  assert.equal(purposeKey("something-new"), null);
});

test("the general helper starts at the recommended level; a template at its own, never above Balanced", () => {
  assert.equal(startLevel(GENERAL_CHOICE, "full", "approve-destructive"), "approve-destructive");
  assert.equal(startLevel("inbox-triage", "read-and-draft", "approve-destructive"), "read-and-draft");
  assert.equal(startLevel("researcher", "approve-destructive", "approve-destructive"), "approve-destructive");
  assert.equal(startLevel("x", "full", "approve-destructive"), "approve-destructive");
  assert.equal(startLevel("x", "read-only", "approve-destructive"), "read-only");
  assert.equal(startLevel("x", "weird", "approve-destructive"), "approve-destructive");
  assert.equal(startLevel("x", undefined, "approve-destructive"), "approve-destructive");
});

test("a template is imported under the level shown on the page; a stale custom never becomes Full", () => {
  assert.equal(importLevel("full", "read-and-draft"), "full");
  assert.equal(importLevel("read-only", "approve-destructive"), "read-only");
  assert.equal(importLevel("custom", "read-and-draft"), "read-and-draft");
  assert.equal(importLevel("custom", "full"), "approve-destructive");
  assert.equal(importLevel("custom", null), "approve-destructive");
});

test("a template offers only the levels it can really get (the import grants the lower of the two)", () => {
  assert.deepEqual(levelsFor(GENERAL_CHOICE, "read-only"), ["full", "approve-destructive", "read-and-draft", "read-only"]);
  assert.deepEqual(levelsFor("researcher", "approve-destructive"), ["approve-destructive", "read-and-draft", "read-only"]);
  assert.deepEqual(levelsFor("inbox-triage", "read-and-draft"), ["read-and-draft", "read-only"]);
  assert.deepEqual(levelsFor("family-helper", "read-only"), ["read-only"]);
  // Asking for nothing is the daemon's default, approve-destructive; an unknown value the same.
  assert.deepEqual(levelsFor("x", undefined), ["approve-destructive", "read-and-draft", "read-only"]);
  assert.deepEqual(levelsFor("x", "weird"), ["approve-destructive", "read-and-draft", "read-only"]);
  // A template that asks for Full can be given Full (the owner's explicit pick; it starts at Balanced).
  assert.deepEqual(levelsFor("x", "full"), ["full", "approve-destructive", "read-and-draft", "read-only"]);
});

test("another pick keeps the owner's own level, lowered if needed and never raised", () => {
  const rec = "approve-destructive" as const;
  // Untouched: each pick starts at its own level.
  assert.equal(levelAfterPick("full", false, "inbox-triage", "read-and-draft", rec), "read-and-draft");
  assert.equal(levelAfterPick("read-and-draft", false, GENERAL_CHOICE, undefined, rec), rec);
  // Chosen by the owner: kept when possible ...
  assert.equal(levelAfterPick("read-only", true, "researcher", "approve-destructive", rec), "read-only");
  assert.equal(levelAfterPick("read-and-draft", true, GENERAL_CHOICE, undefined, rec), "read-and-draft");
  // ... otherwise lowered to the most that pick can get.
  assert.equal(levelAfterPick("full", true, "researcher", "approve-destructive", rec), "approve-destructive");
  assert.equal(levelAfterPick("approve-destructive", true, "family-helper", "read-only", rec), "read-only");
  // "custom" is not a page level: the pick's start.
  assert.equal(levelAfterPick("custom", true, "researcher", "approve-destructive", rec), "approve-destructive");
});

test("a ready-made bot cannot be created on a command-line engine; the general helper can", () => {
  assert.equal(templateNeedsOtherModel("researcher", "cli:claude/default"), true);
  assert.equal(templateNeedsOtherModel("researcher", "anthropic/claude"), false);
  assert.equal(templateNeedsOtherModel(GENERAL_CHOICE, "cli:claude/default"), false);
});

test("a typed name survives another pick; an empty or picked name follows the pick", () => {
  assert.equal(nameAfterPick("", "", "Researcher"), "Researcher");
  assert.equal(nameAfterPick("Researcher", "Researcher", "Coder"), "Coder");
  assert.equal(nameAfterPick("Nova", "Researcher", "Coder"), "Nova");
  assert.equal(nameAfterPick("  ", "Coder", ""), "");
  // Back to the general helper: the picked name is cleared, a typed one is kept.
  assert.equal(nameAfterPick("Coder", "Coder", ""), "");
  assert.equal(nameAfterPick("Byte", "Coder", ""), "Byte");
});

test("the look is applied as three field changes; no picture means no picture op", () => {
  assert.deepEqual(lookOps({ shape: "cloud", color: "#4c8dff", avatar: null }), [
    { op: "set", path: "/bot/shape", value: "cloud" },
    { op: "set", path: "/bot/color", value: "#4c8dff" }
  ]);
  const ops = lookOps({ shape: "gem", color: "#fff", avatar: "data:image/png;base64,AA" });
  assert.equal(ops.length, 3);
  assert.deepEqual(ops[2], { op: "set", path: "/bot/avatar", value: "data:image/png;base64,AA" });
});

test("Surprise me always changes both the shape and the colour", () => {
  const shapes = ["pebble", "cube", "cloud"] as const;
  const colors = ["#111111", "#222222"] as const;
  for (const r of [0, 0.3, 0.5, 0.99]) {
    const next = shuffleLook({ shape: "pebble", color: "#111111" }, shapes, colors, () => r);
    assert.notEqual(next.shape, "pebble");
    assert.equal(next.color, "#222222");
  }
  // Case does not matter for the current colour.
  assert.equal(shuffleLook({ shape: "cube", color: "#222222".toUpperCase() }, shapes, colors, () => 0).color, "#111111");
  // A single option is still returned.
  assert.equal(shuffleLook({ shape: "pebble", color: "#111111" }, ["pebble"], ["#111111"], () => 0.5).shape, "pebble");
});

test("the dialog is one page: no stepper, a primary Create, the access line with a help popover", () => {
  const src = read("../views/NewBotModal.tsx");
  assert.doesNotMatch(src, /newBotSteps|new-bot-next|stepper/);
  assert.match(src, /data-testid="new-bot-submit"/);
  assert.match(src, /data-testid="new-bot-access-help"/);
  assert.match(src, /data-testid="new-bot-more"/);
  // Templates are radio cards, operable with the arrow keys.
  assert.match(src, /role="radiogroup"/);
  assert.match(src, /role="radio"/);
  assert.match(src, /ArrowRight/);
  // A template is created through the import (clamped by the daemon), then given the chosen look.
  assert.match(src, /createFromTemplate\(client, \{ template: tpl, preset: draft\.preset, model, name: draft\.name, look: draft \}\)/);
});

const TEMPLATE = {
  format: "praxi-bot-profile",
  format_version: 1,
  meta: { id: "inbox-triage" },
  bot: { name: "Inbox triage", persona: "Sort mail." },
  permissions: { preset: "read-and-draft" }
} as never;

function fakeClient(patchFails: boolean | "avatar" = false) {
  const calls: { imports: unknown[]; patches: Array<[string, unknown]> } = { imports: [], patches: [] };
  const bot = { id: "b1", name: "Mail", version: 3, config: {}, created_at_ms: 1 };
  return {
    calls,
    client: {
      async importProfile(input: unknown) {
        calls.imports.push(input);
        return { bot, preview: {} } as never;
      },
      async patchBot(id: string, input: unknown) {
        calls.patches.push([id, input]);
        const ops = (input as { ops: Array<{ path: string }> }).ops;
        if (patchFails === true || (patchFails === "avatar" && ops.some((o) => o.path === "/bot/avatar"))) throw new Error("400");
        return { ...bot, version: 4 } as never;
      }
    }
  };
}

test("a template bot is imported under the page's level, model and name, then given the chosen look", async () => {
  const { client, calls } = fakeClient();
  const look = { shape: "cloud" as const, color: "#4c8dff", avatar: null };
  const bot = await createFromTemplate(client, { template: TEMPLATE, preset: "full", model: "anthropic/claude", name: "  Mail ", look });
  assert.deepEqual(calls.imports, [
    {
      contents: JSON.stringify(TEMPLATE),
      permission_level: "full",
      computer_files: false,
      model: "anthropic/claude",
      name: "Mail"
    }
  ]);
  assert.deepEqual(calls.patches, [["b1", { expected_version: 3, ops: lookOps(look) }]]);
  assert.equal(bot.version, 4);
});

test("a refused look change keeps the created template bot", async () => {
  const { client } = fakeClient(true);
  const bot = await createFromTemplate(client, {
    template: TEMPLATE,
    preset: "custom",
    model: "m",
    name: "Mail",
    look: { shape: "gem", color: "#fff", avatar: null }
  });
  assert.equal(bot.id, "b1");
  assert.equal(bot.version, 3);
});

test("a refused picture does not cost the template bot its shape and colour", async () => {
  const { client, calls } = fakeClient("avatar");
  const look = { shape: "gem" as const, color: "#fff", avatar: "data:image/png;base64,AA" };
  const bot = await createFromTemplate(client, { template: TEMPLATE, preset: "read-only", model: "m", name: "Mail", look });
  assert.equal(calls.patches.length, 2);
  assert.deepEqual(calls.patches[1], ["b1", { expected_version: 3, ops: lookOps({ ...look, avatar: null }) }]);
  assert.equal(bot.version, 4);
});
