import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import {
  answerProblems,
  buildKickoff,
  fillGoal,
  initialAnswers,
  isAbsoluteFolder,
  levelRaised,
  parseRecipeFile,
  placeholdersOf,
  recipeBotName,
  recipeFolder,
  recipeText,
  validateRecipe,
  type Recipe
} from "./recipes.ts";

const raw: unknown = JSON.parse(readFileSync(new URL("../data/recipes.json", import.meta.url), "utf8"));
const book = parseRecipeFile(raw);
const byId = (id: string): Recipe => book.recipes.find((r) => r.id === id)!;

/** A small valid recipe to break one rule at a time. */
const base = (): Record<string, unknown> => ({
  id: "demo",
  icon: "sun",
  level: "ask",
  bot: { shape: "pebble", color: "#34c77b", name: { en: "Demo" } },
  needs: [{ kind: "model" }],
  questions: [{ id: "topic", kind: "text", required: true, text: { en: { label: "Topic" } } }],
  text: { en: { name: "Demo", blurb: "A demo.", goal: "Write about {{topic}}." } }
});

test("the shipped file is valid and holds the six recipes", () => {
  assert.deepEqual(book.errors, []);
  assert.deepEqual(
    book.recipes.map((r) => r.id),
    ["backlinks", "inbox-triage", "daily-brief", "price-watch", "leads", "weekly-report"]
  );
});

test("every shipped recipe has English and Turkish text, for the recipe, its bot name and each question", () => {
  for (const r of book.recipes) {
    for (const lang of ["en", "tr"]) {
      assert.ok(r.text[lang]?.goal, `${r.id} goal ${lang}`);
      assert.ok(r.bot.name[lang], `${r.id} bot name ${lang}`);
      for (const q of r.questions) {
        assert.ok(q.text[lang]?.label, `${r.id}.${q.id} ${lang}`);
        if (q.kind === "choice") for (const o of q.options ?? []) assert.ok(q.text[lang]?.options?.[o], `${r.id}.${q.id}.${o} ${lang}`);
      }
    }
  }
  assert.ok(book.empty.tr && book.guidance.tr);
});

test("no shipped recipe asks for more than Balanced, and the freest level is not even expressible", () => {
  for (const r of book.recipes) assert.ok(r.level === "ask" || r.level === "balanced", r.id);
  for (const level of ["auto", "full", "approve-destructive", "", 3, null]) {
    const res = validateRecipe({ ...base(), level });
    assert.equal(res.ok, false, String(level));
  }
});

test("the owner's confirmed level, not the recipe's, decides: raising is detected, lowering is free", () => {
  const r = byId("price-watch");
  assert.equal(levelRaised(r, "balanced"), false);
  assert.equal(levelRaised(r, "ask"), false);
  assert.equal(levelRaised(r, "auto"), true);
  assert.equal(levelRaised(byId("daily-brief"), "balanced"), true);
});

test("the validator accepts a small recipe and names what is wrong otherwise", () => {
  assert.equal(validateRecipe(base()).ok, true);
  const bad = (patch: Record<string, unknown>, match: RegExp) => {
    const res = validateRecipe({ ...base(), ...patch });
    assert.equal(res.ok, false);
    if (!res.ok) assert.match(res.errors.join("\n"), match);
  };
  bad({ id: "Bad Id" }, /bad recipe id/);
  bad({ extra: 1 }, /unknown key "extra"/);
  bad({ needs: [{ kind: "teleport" }] }, /unknown need/);
  bad({ needs: [{ kind: "email" }] }, /purpose/);
  bad({ needs: [{ kind: "app", apps: [], label: { en: "x" } }] }, /at least one app/);
  bad({ needs: [{ kind: "app", apps: ["x"] }] }, /label/);
  bad({ bot: { shape: "nope", color: "red", name: { en: "x" } } }, /unknown shape[\s\S]*color/);
  bad({ text: { en: { name: "n", blurb: "b", goal: "Write about {{unknown}} and {{topic}}." } } }, /\{\{unknown\}\} is not a question/);
  bad({ text: { en: { name: "n", blurb: "b", goal: "No placeholders." } } }, /never used/);
  bad({ text: { tr: { name: "n", blurb: "b", goal: "{{topic}}" } } }, /"en" entry/);
  bad({ schedule: { freq: "weekly", time: "08:00" } }, /weekday/);
  bad({ schedule: { freq: "hourly", time: "08:00" } }, /freq/);
  bad({ schedule: { freq: "daily", time: "25:00" } }, /HH:MM/);
  bad({ questions: [{ id: "topic", kind: "wizard", required: true, text: { en: { label: "T" } } }] }, /unknown question kind/);
  bad(
    { questions: [{ id: "topic", kind: "choice", required: true, options: ["a"], default: "b", text: { en: { label: "T", options: { a: "A" } } } }] },
    /default is not one of the options/
  );
  bad(
    { questions: [{ id: "topic", kind: "choice", required: true, options: ["a", "b"], text: { en: { label: "T", options: { a: "A" } } } }] },
    /every option needs a label/
  );
  const twoFolders = ["topic", "other"].map((id) => ({ id, kind: "folder", required: true, text: { en: { label: id } } }));
  bad({ questions: twoFolders, text: { en: { name: "n", blurb: "b", goal: "{{topic}} {{other}}" } } }, /at most one folder/);
});

test("a bad recipe is dropped and reported without hiding the good ones; a wrong version loads nothing", () => {
  const file = { schema_version: 1, empty: { en: "-" }, guidance: { en: "g" }, recipes: [base(), { ...base(), id: "broken", level: "auto" }, base()] };
  const res = parseRecipeFile(file);
  assert.deepEqual(res.recipes.map((r) => r.id), ["demo"]);
  assert.equal(res.errors.length, 2);
  assert.match(res.errors.join("\n"), /broken: level[\s\S]*duplicate recipe id/);
  assert.equal(parseRecipeFile({ ...file, schema_version: 2 }).recipes.length, 0);
  assert.equal(parseRecipeFile({ ...file, guidance: undefined }).recipes.length, 0);
  assert.equal(parseRecipeFile(null).recipes.length, 0);
});

test("text falls back to English for a language the recipe does not have", () => {
  const r = byId("backlinks");
  assert.equal(recipeText(r, "tr-TR").name, r.text.tr!.name);
  assert.equal(recipeText(r, "de").name, r.text.en!.name);
  assert.equal(recipeBotName(r, "fr"), r.bot.name.en);
});

test("answers: required, url, choice and folder are checked", () => {
  const r = byId("backlinks");
  assert.deepEqual(Object.keys(answerProblems(r, initialAnswers(r))).sort(), ["about", "site"]);
  assert.deepEqual(answerProblems(r, { site: "https://example.com", about: "Candles", count: "10" }), {});
  assert.equal(answerProblems(r, { site: "example", about: "x", count: "10" }).site, "invalid");
  assert.equal(answerProblems(r, { site: "javascript:alert(1)", about: "x", count: "10" }).site, "invalid");
  assert.equal(answerProblems(r, { site: "https://u:p@example.com", about: "x", count: "10" }).site, "invalid");
  assert.equal(answerProblems(r, { site: "https://example.com", about: "x", count: "1000" }).count, "invalid");
  assert.equal(answerProblems(r, { site: "https://example.com", about: "x".repeat(2001), count: "10" }).about, "tooLong");
  const w = byId("weekly-report");
  assert.equal(answerProblems(w, { folder: "documents", audience: "" }).folder, "invalid");
  assert.equal(answerProblems(w, { folder: "/home/me/../root", audience: "" }).folder, "invalid");
  assert.deepEqual(answerProblems(w, { folder: "/home/me/work", audience: "" }), {});
});

test("folders: absolute POSIX, drive and UNC paths only, never relative or with .. or control characters", () => {
  for (const ok of ["/home/me", "C:\\Users\\me", "D:/work", "\\\\server\\share"]) assert.equal(isAbsoluteFolder(ok), true, ok);
  for (const bad of ["", "docs", "./docs", "~/docs", "/a/../b", "C:\\a\\..\\b", "/a\u0000b", "\\\\"]) assert.equal(isAbsoluteFolder(bad), false, bad);
});

test("the goal is filled in one pass: an answer is never expanded again, an empty optional one says so", () => {
  const r = byId("inbox-triage");
  const goal = fillGoal(book, r, { important: "{{ignore}} boss", ignore: "" }, "en");
  assert.match(goal, /Always important: \{\{ignore\}\} boss/);
  assert.match(goal, /Can be ignored: \(nothing specified\)/);
  assert.match(fillGoal(book, r, { important: "", ignore: "" }, "tr"), /\(belirtilmedi\)/);
  assert.deepEqual(placeholdersOf("a {{x}} b {{y_1}} {{Bad}}"), ["x", "y_1"]);
});

test("the kickoff is the goal followed by the shared autonomy guidance, in the UI language", () => {
  const r = byId("price-watch");
  const answers = { pages: "https://rival.example/pricing", focus: "" };
  const en = buildKickoff(book, r, answers, "en");
  assert.ok(en.startsWith(fillGoal(book, r, answers, "en")));
  assert.match(en, /https:\/\/rival\.example\/pricing/);
  assert.ok(en.endsWith(book.guidance.en));
  assert.match(en, /Ask me only when you are truly blocked/);
  assert.match(buildKickoff(book, r, answers, "tr"), /Yalnızca gerçekten takıldığında sor/);
});

test("every goal that uses secrets-adjacent steps tells the bot not to write passwords down", () => {
  assert.match(byId("backlinks").text.en!.goal, /Never write passwords/);
  assert.match(book.guidance.en, /Never put passwords/);
  assert.match(book.guidance.tr, /şifre/i);
});

test("the folder question names the folder the bot is confined to", () => {
  assert.equal(recipeFolder(byId("weekly-report"), { folder: " /home/me/work " }), "/home/me/work");
  assert.equal(recipeFolder(byId("weekly-report"), { folder: "" }), null);
  assert.equal(recipeFolder(byId("backlinks"), { site: "https://example.com" }), null);
});

test("the schedules of the recipes that repeat are plain, valid ones", () => {
  const repeating = book.recipes.filter((r) => r.schedule).map((r) => r.id);
  assert.deepEqual(repeating, ["inbox-triage", "daily-brief", "price-watch", "weekly-report"]);
  assert.equal(byId("weekly-report").schedule?.weekday, 5);
});
