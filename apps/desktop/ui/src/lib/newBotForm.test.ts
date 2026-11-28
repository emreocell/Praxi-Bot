import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { AvatarFileError } from "./avatar.ts";
import {
  applyLook,
  createFromTemplate,
  isAvatarRefusal,
  LookNotAppliedError,
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
  assert.equal(importLevel("full", "full"), "full");
  assert.equal(importLevel("read-only", "approve-destructive"), "read-only");
  assert.equal(importLevel("custom", "read-and-draft"), "read-and-draft");
  assert.equal(importLevel("custom", "full"), "approve-destructive");
  assert.equal(importLevel("custom", null), "approve-destructive");
});

test("a template import never asks for more than the owner's requested level", () => {
  // A stale "full" on the page with a template that asks for read-only: read-only is sent.
  assert.equal(importLevel("full", "read-only"), "read-only");
  assert.equal(importLevel("full", "read-and-draft"), "read-and-draft");
  assert.equal(importLevel("approve-destructive", "read-and-draft"), "read-and-draft");
  // Asking for nothing is the daemon's default, approve-destructive.
  assert.equal(importLevel("full", undefined), "approve-destructive");
  assert.equal(importLevel("full", "weird"), "approve-destructive");
  assert.equal(importLevel("read-and-draft", undefined), "read-and-draft");
});

test("levelsFor is sorted most freedom first, whatever order the source list has", () => {
  const rank = { full: 3, "approve-destructive": 2, "read-and-draft": 1, "read-only": 0 } as const;
  for (const requested of ["read-only", "read-and-draft", "approve-destructive", "full", undefined]) {
    for (const choice of [GENERAL_CHOICE, "researcher"]) {
      const levels = levelsFor(choice, requested);
      const sorted = [...levels].sort((a, b) => rank[b] - rank[a]);
      assert.deepEqual(levels, sorted, `${choice}/${requested}`);
    }
  }
  // The first level is the top the choice can get: what every `levels[0]` fallback relies on.
  assert.equal(levelsFor(GENERAL_CHOICE, "read-only")[0], "full");
  assert.equal(levelsFor("researcher", "read-and-draft")[0], "read-and-draft");
  assert.equal(levelAfterPick("full", true, "researcher", "read-and-draft", "approve-destructive"), "read-and-draft");
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
  // The engine prefix is not case-sensitive, nor are surrounding spaces.
  assert.equal(templateNeedsOtherModel("researcher", "CLI:claude/default"), true);
  assert.equal(templateNeedsOtherModel("researcher", "  Cli:claude/default "), true);
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

const PNG = "data:image/png;base64,AAAA";

class FakeApiError extends Error {
  status: number;
  code: string;
  constructor(status: number, code: string, message: string) {
    super(message);
    this.status = status;
    this.code = code;
  }
}

const avatarRefused = () => new FakeApiError(400, "bad_request", "bad_patch:`avatar` is 300000 bytes; the limit is 200000");
const versionConflict = () => new FakeApiError(409, "version_conflict", "stale version");

type PatchFailure = "avatar" | "version" | "down" | null;

function fakeClient(patchFails: PatchFailure = null) {
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
        if (patchFails === "version") throw versionConflict();
        if (patchFails === "down") throw new FakeApiError(0, "network", "connection refused");
        if (patchFails === "avatar" && ops.some((o) => o.path === "/bot/avatar")) throw avatarRefused();
        return { ...bot, version: 4 } as never;
      }
    }
  };
}

test("a template bot is imported under the page's level, model and name, then given the chosen look", async () => {
  const { client, calls } = fakeClient();
  const look = { shape: "cloud" as const, color: "#4c8dff", avatar: null };
  const bot = await createFromTemplate(client, { template: TEMPLATE, preset: "read-and-draft", model: "anthropic/claude", name: "  Mail ", look });
  assert.deepEqual(calls.imports, [
    {
      contents: JSON.stringify(TEMPLATE),
      permission_level: "read-and-draft",
      computer_files: false,
      model: "anthropic/claude",
      name: "Mail"
    }
  ]);
  assert.deepEqual(calls.patches, [["b1", { expected_version: 3, ops: lookOps(look) }]]);
  assert.equal(bot.version, 4);
});

test("a stale higher level on the page is sent as the template's own level, not as Full", async () => {
  const { client, calls } = fakeClient();
  const look = { shape: "cloud" as const, color: "#4c8dff", avatar: null };
  await createFromTemplate(client, { template: TEMPLATE, preset: "full", model: "m", name: "Mail", look });
  assert.equal((calls.imports[0] as { permission_level: string }).permission_level, "read-and-draft");
});

test("a look change the daemon refuses for another reason is shown, never reported as success", async () => {
  for (const failure of ["version", "down"] as const) {
    const { client, calls } = fakeClient(failure);
    const look = { shape: "gem" as const, color: "#fff", avatar: null };
    const err = await createFromTemplate(client, { template: TEMPLATE, preset: "read-only", model: "m", name: "Mail", look }).catch((e) => e);
    assert.ok(err instanceof LookNotAppliedError, failure);
    // The created bot is handed back so it is not made twice, and the root cause is kept.
    assert.equal(err.bot.id, "b1");
    assert.equal(err.reason instanceof FakeApiError, true);
    assert.equal((err.reason as FakeApiError).code, failure === "version" ? "version_conflict" : "network");
    assert.equal(calls.patches.length, 1, "no retry without the picture");
  }
});

test("a failed look with a picture is not retried without it unless the picture was refused", async () => {
  const { client, calls } = fakeClient("version");
  const look = { shape: "gem" as const, color: "#fff", avatar: PNG };
  await assert.rejects(applyLook(client, { id: "b1", version: 3 } as never, look), FakeApiError);
  assert.equal(calls.patches.length, 1);
});

test("a refused picture does not cost the template bot its shape and colour", async () => {
  const { client, calls } = fakeClient("avatar");
  const look = { shape: "gem" as const, color: "#fff", avatar: PNG };
  const bot = await createFromTemplate(client, { template: TEMPLATE, preset: "read-only", model: "m", name: "Mail", look });
  assert.equal(calls.patches.length, 2);
  assert.deepEqual(calls.patches[1], ["b1", { expected_version: 3, ops: lookOps({ ...look, avatar: null }) }]);
  assert.equal(bot.version, 4);
});

test("only a daemon refusal that names the avatar counts as a refused picture", () => {
  assert.equal(isAvatarRefusal(avatarRefused()), true);
  assert.equal(isAvatarRefusal(new FakeApiError(413, "too_large", "`avatar` is too big")), true);
  assert.equal(isAvatarRefusal(versionConflict()), false);
  assert.equal(isAvatarRefusal(new FakeApiError(400, "bad_request", "bad_patch:`color` must look like #rrggbb")), false);
  assert.equal(isAvatarRefusal(new FakeApiError(500, "internal", "avatar")), false);
  assert.equal(isAvatarRefusal(new Error("avatar")), false);
  assert.equal(isAvatarRefusal(null), false);
});

test("a picture that is not a small png/jpeg/gif/webp data URL is refused before anything is created", async () => {
  const bad: Array<[string, string]> = [
    ["data:image/svg+xml;base64,AAAA", "unreadable"],
    ["data:text/html;base64,AAAA", "unreadable"],
    ["https://example.com/a.png", "unreadable"],
    ["data:image/png;base64,", "unreadable"],
    ["data:image/png;base64,AA AA!", "unreadable"],
    [`data:image/png;base64,${"A".repeat(200_001)}`, "tooLarge"]
  ];
  for (const [avatar, reason] of bad) {
    const { client, calls } = fakeClient();
    const err = await createFromTemplate(client, {
      template: TEMPLATE,
      preset: "read-only",
      model: "m",
      name: "Mail",
      look: { shape: "gem", color: "#fff", avatar }
    }).catch((e) => e);
    assert.ok(err instanceof AvatarFileError, avatar.slice(0, 30));
    assert.equal(err.reason, reason);
    assert.equal(calls.imports.length, 0);
    assert.equal(calls.patches.length, 0);
  }
  // Valid types pass.
  for (const type of ["png", "jpeg", "gif", "webp"]) {
    const { client, calls } = fakeClient();
    await createFromTemplate(client, {
      template: TEMPLATE,
      preset: "read-only",
      model: "m",
      name: "Mail",
      look: { shape: "gem", color: "#fff", avatar: `data:image/${type};base64,AAAA` }
    });
    assert.equal(calls.imports.length, 1, type);
  }
});

test("the dialog names the field with a visible label and moves the focus by refs, not by a selector", () => {
  const src = read("../views/NewBotModal.tsx");
  assert.match(src, /<label className=\{styles\.label\} htmlFor="new-bot-name">/);
  assert.match(src, /id="new-bot-name"/);
  assert.match(src, /choiceRefs\.current\.get\(next\.id\)\?\.focus\(\)/);
  assert.doesNotMatch(src, /CSS\.escape|querySelector/);
});
