import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

const read = (path: string) => readFileSync(new URL(path, import.meta.url), "utf8");
const locale = (lng: string): Record<string, unknown> => JSON.parse(read(`../i18n/locales/${lng}.json`));

test("\"Start from a goal\" is offered on the home screen, the empty chat and the New-bot dialog", () => {
  const app = read("../App.tsx");
  assert.match(app, /data-testid="home-start-from-goal"/);
  assert.match(app, /onStartFromGoal=\{\(\) => setRecipeOpen\(true\)\}/);
  assert.match(app, /<RecipeFlow\b/);
  assert.match(read("../views/BotEmptyView.tsx"), /data-testid="bot-empty-start-from-goal"/);
  const modal = read("../views/NewBotModal.tsx");
  assert.match(modal, /data-testid="new-bot-start-goal"/);
  assert.match(modal, /t\("recipes\.entry"\)/);
});

test("the fix of a missing connection opens an existing screen over the flow, which then checks again", () => {
  const app = read("../App.tsx");
  assert.match(app, /if \(fix\.kind === "apps"\) setAppsOpen\(true\);\s*else setSettingsOpen\(fix\.section\);/);
  assert.match(app, /if \(settingsOpen === null && !appsOpen\) setRecipeRecheck/);
});

test("the flow never creates anything before the review step and the start button waits for the checks", () => {
  const flow = read("../views/RecipeFlow.tsx");
  assert.match(flow, /disabled=\{busy \|\| !loaded \|\| blocked \|\| executor\.model\.trim\(\) === ""\}/);
  assert.match(flow, /if \(!client \|\| !recipe \|\| busy \|\| blocked \|\| !loaded/);
  // The level that is applied is the one in the review step's control, not the recipe's.
  assert.match(flow, /\blevel,\n\s+executor,/);
});

test("the recipe strings exist in English and Turkish, and Turkish is really translated", () => {
  const en = locale("en").recipes as Record<string, Record<string, unknown> | string>;
  const tr = locale("tr").recipes as Record<string, Record<string, unknown> | string>;
  const flat = (o: unknown, p = ""): Array<[string, string]> =>
    Object.entries(o as Record<string, unknown>).flatMap(([k, v]) => (v && typeof v === "object" ? flat(v, `${p}${k}.`) : [[`${p}${k}`, String(v)] as [string, string]]));
  const enFlat = new Map(flat(en));
  const trFlat = new Map(flat(tr));
  assert.deepEqual([...trFlat.keys()].sort(), [...enFlat.keys()].sort());
  const same = [...enFlat].filter(([k, v]) => trFlat.get(k) === v).map(([k]) => k);
  assert.deepEqual(same, []);
});
