import { normalizeBotShape } from "ui/bot-shape";
import type { AutonomyLevel } from "./botProfile.ts";
import type { Frequency } from "./scheduleBuilder.ts";

/**
 * Goal-based setup recipes (issue #578): "what I want" in one place - the goal text, what it
 * needs connected, the few questions to ask once, how freely the bot may act and an optional
 * schedule.  A recipe is data (`data/recipes.json`, versioned, text per language); this file is
 * its schema, validator and the pure helpers the guided flow uses.  Nothing here talks to the
 * daemon.
 *
 * Security boundary: a recipe can only ASK for a level.  It is capped at "balanced" here (a recipe
 * can never ask for "auto"), the flow shows the level before anything is created, and the bot gets
 * exactly the level the owner confirmed, never the recipe's.
 */

export const RECIPE_SCHEMA_VERSION = 1;

/** The most a recipe may ask for.  "auto" can only be chosen by the owner in the review step. */
export type RecipeLevel = Exclude<AutonomyLevel, "auto">;
export const RECIPE_LEVELS: readonly RecipeLevel[] = ["ask", "balanced"] as const;

const LEVEL_RANK: Record<AutonomyLevel, number> = { ask: 0, balanced: 1, auto: 2 };

/** Is `chosen` freer than what the recipe asked for (the review step then says so plainly)? */
export function levelRaised(recipe: Pick<Recipe, "level">, chosen: AutonomyLevel): boolean {
  return LEVEL_RANK[chosen] > LEVEL_RANK[recipe.level];
}

export type RecipeNeed =
  | { kind: "model"; optional?: boolean }
  | { kind: "browser"; optional?: boolean }
  /** `own`: the bot signs up with its own mailbox; `inbox`: it reads the owner's inbox. */
  | { kind: "email"; purpose: "own" | "inbox"; optional?: boolean }
  /** A connected app (Composio slug or app name) matching any of `apps`. */
  | { kind: "app"; apps: string[]; label: Record<string, string>; optional?: boolean };

export type QuestionKind = "text" | "longtext" | "url" | "choice" | "folder";

export interface LocalizedQuestion {
  label: string;
  hint?: string;
  placeholder?: string;
  options?: Record<string, string>;
}

export interface RecipeQuestion {
  id: string;
  kind: QuestionKind;
  required: boolean;
  default?: string;
  options?: string[];
  text: Record<string, LocalizedQuestion>;
}

export interface RecipeSchedule {
  freq: Exclude<Frequency, "once" | "custom">;
  /** `HH:MM`, 24 hour. */
  time: string;
  /** 0 = Sunday .. 6 = Saturday (weekly). */
  weekday?: number;
}

export interface RecipeText {
  name: string;
  blurb: string;
  goal: string;
}

export interface Recipe {
  id: string;
  /** lucide icon name, resolved by the view. */
  icon: string;
  level: RecipeLevel;
  bot: { shape: string; color: string; name: Record<string, string> };
  needs: RecipeNeed[];
  questions: RecipeQuestion[];
  schedule?: RecipeSchedule;
  text: Record<string, RecipeText>;
}

export type Answers = Record<string, string>;

const ID_RE = /^[a-z0-9][a-z0-9-]{0,39}$/;
const QUESTION_ID_RE = /^[a-z][a-z0-9_]{0,23}$/;
const COLOR_RE = /^#[0-9a-f]{6}$/i;
const TIME_RE = /^([01]?\d|2[0-3]):[0-5]\d$/;
const PLACEHOLDER_RE = /\{\{([a-z][a-z0-9_]*)\}\}/g;
const QUESTION_KINDS: readonly QuestionKind[] = ["text", "longtext", "url", "choice", "folder"];
const FREQUENCIES: readonly string[] = ["daily", "weekdays", "weekly", "monthly"];
const MAX_GOAL = 4000;
const MAX_ANSWER = 2000;
const RECIPE_KEYS = ["id", "icon", "level", "bot", "needs", "questions", "schedule", "text"];
const QUESTION_KEYS = ["id", "kind", "required", "default", "options", "text"];

type Json = Record<string, unknown>;
const isObj = (v: unknown): v is Json => typeof v === "object" && v !== null && !Array.isArray(v);
const isStr = (v: unknown): v is string => typeof v === "string" && v.trim() !== "";

function unknownKeys(o: Json, allowed: readonly string[], where: string, errors: string[]) {
  for (const k of Object.keys(o)) if (!allowed.includes(k)) errors.push(`${where}: unknown key "${k}"`);
}

/** Placeholders (`{{id}}`) a text uses, in order. */
export function placeholdersOf(text: string): string[] {
  return [...text.matchAll(PLACEHOLDER_RE)].map((m) => m[1]!);
}

function checkNeed(raw: unknown, where: string, errors: string[]): RecipeNeed | null {
  if (!isObj(raw)) {
    errors.push(`${where}: a need must be an object`);
    return null;
  }
  if (raw.optional !== undefined && typeof raw.optional !== "boolean") errors.push(`${where}: optional must be true or false`);
  const optional = raw.optional === true ? { optional: true } : {};
  switch (raw.kind) {
    case "model":
    case "browser":
      unknownKeys(raw, ["kind", "optional"], where, errors);
      return { kind: raw.kind, ...optional };
    case "email":
      unknownKeys(raw, ["kind", "purpose", "optional"], where, errors);
      if (raw.purpose !== "own" && raw.purpose !== "inbox") {
        errors.push(`${where}: email purpose must be "own" or "inbox"`);
        return null;
      }
      return { kind: "email", purpose: raw.purpose, ...optional };
    case "app": {
      unknownKeys(raw, ["kind", "apps", "label", "optional"], where, errors);
      const apps = raw.apps;
      if (!Array.isArray(apps) || apps.length === 0 || !apps.every(isStr)) {
        errors.push(`${where}: an app need lists at least one app name`);
        return null;
      }
      if (!isObj(raw.label) || !isStr(raw.label.en) || !Object.values(raw.label).every(isStr)) {
        errors.push(`${where}: an app need has a label with an "en" entry`);
        return null;
      }
      return { kind: "app", apps: apps as string[], label: raw.label as Record<string, string>, ...optional };
    }
    default:
      errors.push(`${where}: unknown need "${String(raw.kind)}"`);
      return null;
  }
}

function checkQuestion(raw: unknown, where: string, errors: string[]): RecipeQuestion | null {
  if (!isObj(raw)) {
    errors.push(`${where}: a question must be an object`);
    return null;
  }
  const before = errors.length;
  unknownKeys(raw, QUESTION_KEYS, where, errors);
  if (typeof raw.id !== "string" || !QUESTION_ID_RE.test(raw.id)) errors.push(`${where}: bad question id`);
  if (!QUESTION_KINDS.includes(raw.kind as QuestionKind)) errors.push(`${where}: unknown question kind "${String(raw.kind)}"`);
  if (typeof raw.required !== "boolean") errors.push(`${where}: required must be true or false`);
  const kind = raw.kind as QuestionKind;
  if (kind === "choice") {
    const o = raw.options;
    if (!Array.isArray(o) || o.length === 0 || !o.every(isStr)) errors.push(`${where}: a choice needs options`);
    else if (raw.default !== undefined && !o.includes(raw.default as string)) errors.push(`${where}: default is not one of the options`);
  } else if (raw.options !== undefined) {
    errors.push(`${where}: only a choice has options`);
  }
  if (raw.default !== undefined && typeof raw.default !== "string") errors.push(`${where}: default must be text`);
  const text = raw.text;
  if (!isObj(text) || !isObj(text.en)) {
    errors.push(`${where}: text needs an "en" entry`);
  } else {
    for (const [lang, entry] of Object.entries(text)) {
      if (!isObj(entry) || !isStr(entry.label)) {
        errors.push(`${where}.text.${lang}: a label is needed`);
        continue;
      }
      for (const k of ["hint", "placeholder"]) {
        if (entry[k] !== undefined && typeof entry[k] !== "string") errors.push(`${where}.text.${lang}.${k} must be text`);
      }
      if (kind === "choice" && Array.isArray(raw.options)) {
        const labels = entry.options;
        if (!isObj(labels) || !(raw.options as string[]).every((v) => isStr(labels[v]))) {
          errors.push(`${where}.text.${lang}: every option needs a label`);
        }
      }
    }
  }
  return errors.length === before ? (raw as unknown as RecipeQuestion) : null;
}

export type RecipeCheck = { ok: true; recipe: Recipe } | { ok: false; errors: string[] };

/** Validate one recipe object (untrusted JSON, for example a community file). */
export function validateRecipe(raw: unknown): RecipeCheck {
  const errors: string[] = [];
  if (!isObj(raw)) return { ok: false, errors: ["a recipe must be an object"] };
  const id = typeof raw.id === "string" ? raw.id : "";
  const where = id || "recipe";
  unknownKeys(raw, RECIPE_KEYS, where, errors);
  if (!ID_RE.test(id)) errors.push(`${where}: bad recipe id`);
  if (!isStr(raw.icon)) errors.push(`${where}: an icon name is needed`);
  // The one security rule of the schema: a recipe can never ask for the freest level.
  if (!RECIPE_LEVELS.includes(raw.level as RecipeLevel)) {
    errors.push(`${where}: level must be one of ${RECIPE_LEVELS.join(", ")} (a recipe cannot ask for more)`);
  }

  const bot = raw.bot;
  if (!isObj(bot)) {
    errors.push(`${where}: bot is missing`);
  } else {
    unknownKeys(bot, ["shape", "color", "name"], `${where}.bot`, errors);
    if (typeof bot.shape !== "string" || normalizeBotShape(bot.shape) === null) errors.push(`${where}.bot: unknown shape`);
    if (typeof bot.color !== "string" || !COLOR_RE.test(bot.color)) errors.push(`${where}.bot: color must be #rrggbb`);
    if (!isObj(bot.name) || !isStr(bot.name.en) || !Object.values(bot.name).every(isStr)) errors.push(`${where}.bot: name needs an "en" entry`);
  }

  const needs: RecipeNeed[] = [];
  if (!Array.isArray(raw.needs)) {
    errors.push(`${where}: needs must be a list`);
  } else {
    raw.needs.forEach((n, i) => {
      const need = checkNeed(n, `${where}.needs[${i}]`, errors);
      if (need) needs.push(need);
    });
  }

  const questions: RecipeQuestion[] = [];
  if (!Array.isArray(raw.questions)) {
    errors.push(`${where}: questions must be a list`);
  } else {
    raw.questions.forEach((q, i) => {
      const question = checkQuestion(q, `${where}.questions[${i}]`, errors);
      if (question) questions.push(question);
    });
    const ids = raw.questions.map((q) => (isObj(q) ? q.id : undefined));
    if (new Set(ids).size !== ids.length) errors.push(`${where}: question ids must be unique`);
    if (questions.filter((q) => q.kind === "folder").length > 1) errors.push(`${where}: at most one folder question`);
  }

  if (raw.schedule !== undefined) {
    const s = raw.schedule;
    if (!isObj(s)) {
      errors.push(`${where}: schedule must be an object`);
    } else {
      unknownKeys(s, ["freq", "time", "weekday"], `${where}.schedule`, errors);
      if (typeof s.freq !== "string" || !FREQUENCIES.includes(s.freq)) errors.push(`${where}.schedule: freq must be one of ${FREQUENCIES.join(", ")}`);
      if (typeof s.time !== "string" || !TIME_RE.test(s.time)) errors.push(`${where}.schedule: time must be HH:MM`);
      if (s.freq === "weekly" && !(Number.isInteger(s.weekday) && (s.weekday as number) >= 0 && (s.weekday as number) <= 6)) {
        errors.push(`${where}.schedule: weekly needs weekday 0-6`);
      }
      if (s.freq !== "weekly" && s.weekday !== undefined) errors.push(`${where}.schedule: weekday is only for weekly`);
    }
  }

  const text = raw.text;
  if (!isObj(text) || !isObj(text.en)) {
    errors.push(`${where}: text needs an "en" entry`);
  } else {
    const known = new Set(questions.map((q) => q.id));
    const used = new Set<string>();
    for (const [lang, entry] of Object.entries(text)) {
      if (!isObj(entry) || !isStr(entry.name) || !isStr(entry.blurb) || !isStr(entry.goal)) {
        errors.push(`${where}.text.${lang}: name, blurb and goal are needed`);
        continue;
      }
      if (entry.goal.length > MAX_GOAL) errors.push(`${where}.text.${lang}: goal is too long`);
      for (const p of placeholdersOf(entry.goal)) {
        if (!known.has(p)) errors.push(`${where}.text.${lang}: {{${p}}} is not a question`);
        used.add(p);
      }
    }
    // A folder answer is not text for the goal: it sets the bot's folder (see `recipeFolder`).
    for (const q of questions) if (q.kind !== "folder" && !used.has(q.id)) errors.push(`${where}: question "${q.id}" is never used in the goal`);
  }

  return errors.length === 0 ? { ok: true, recipe: raw as unknown as Recipe } : { ok: false, errors };
}

type Localized = Record<string, string>;

/** A validated recipe file: the recipes plus the text shared by all of them. */
export interface RecipeBook {
  recipes: Recipe[];
  /** Said in place of an optional answer left empty. */
  empty: Localized;
  /** How to work on a task alone; appended to every kickoff message. */
  guidance: Localized;
  errors: string[];
}

/**
 * Validate a whole recipe file.  A bad recipe is left out and reported; it never hides the good
 * ones.  A file with the wrong version or without its shared text yields no recipes at all.
 */
export function parseRecipeFile(raw: unknown): RecipeBook {
  const none = (error: string): RecipeBook => ({ recipes: [], empty: { en: "" }, guidance: { en: "" }, errors: [error] });
  if (!isObj(raw) || raw.schema_version !== RECIPE_SCHEMA_VERSION) {
    return none(`recipes file: schema_version must be ${RECIPE_SCHEMA_VERSION}`);
  }
  const text = (v: unknown): v is Localized => isObj(v) && isStr(v.en) && Object.values(v).every(isStr);
  if (!text(raw.empty) || !text(raw.guidance)) return none('recipes file: "empty" and "guidance" need an "en" text');
  const errors: string[] = [];
  const recipes: Recipe[] = [];
  const seen = new Set<string>();
  const list = Array.isArray(raw.recipes) ? raw.recipes : [];
  if (!Array.isArray(raw.recipes)) errors.push("recipes file: recipes must be a list");
  for (const r of list) {
    const checked = validateRecipe(r);
    if (!checked.ok) {
      errors.push(...checked.errors);
    } else if (seen.has(checked.recipe.id)) {
      errors.push(`${checked.recipe.id}: duplicate recipe id`);
    } else {
      seen.add(checked.recipe.id);
      recipes.push(checked.recipe);
    }
  }
  return { recipes, empty: raw.empty, guidance: raw.guidance, errors };
}

/** The entry for `lang` (`tr-TR` counts as `tr`), falling back to English: a missing language never hides a recipe. */
export function pickLang<T>(map: Record<string, T>, lang: string): T {
  const short = lang.toLowerCase().split("-")[0] ?? "en";
  return map[short] ?? map.en!;
}

export function recipeText(recipe: Recipe, lang: string): RecipeText {
  return pickLang(recipe.text, lang);
}

export function recipeBotName(recipe: Recipe, lang: string): string {
  return pickLang(recipe.bot.name, lang);
}

export function questionText(q: RecipeQuestion, lang: string): LocalizedQuestion {
  return pickLang(q.text, lang);
}

/** What the form starts with: each question's default, else empty. */
export function initialAnswers(recipe: Recipe): Answers {
  return Object.fromEntries(recipe.questions.map((q) => [q.id, q.default ?? ""]));
}

/** An absolute folder path (POSIX, drive letter or UNC), without `..` parts or control characters. */
export function isAbsoluteFolder(p: string): boolean {
  const v = p.trim();
  // eslint-disable-next-line no-control-regex
  if (v === "" || /[\u0000-\u001f]/.test(v)) return false;
  if (!(/^\//.test(v) || /^[A-Za-z]:[\\/]/.test(v) || /^\\\\[^\\]/.test(v))) return false;
  return !v.split(/[\\/]/).includes("..");
}

function isHttpUrl(v: string): boolean {
  try {
    const u = new URL(v.trim());
    return (u.protocol === "http:" || u.protocol === "https:") && u.hostname.includes(".") && u.username === "" && u.password === "";
  } catch {
    return false;
  }
}

export type AnswerProblem = "required" | "invalid" | "tooLong";

/** What is wrong with each answer, by question id.  Empty = the form is complete. */
export function answerProblems(recipe: Recipe, answers: Answers): Record<string, AnswerProblem> {
  const out: Record<string, AnswerProblem> = {};
  for (const q of recipe.questions) {
    const v = (answers[q.id] ?? "").trim();
    if (v === "") {
      if (q.required) out[q.id] = "required";
    } else if (v.length > MAX_ANSWER) {
      out[q.id] = "tooLong";
    } else if (q.kind === "choice" && !q.options?.includes(v)) {
      out[q.id] = "invalid";
    } else if (q.kind === "url" && !isHttpUrl(v)) {
      out[q.id] = "invalid";
    } else if (q.kind === "folder" && !isAbsoluteFolder(v)) {
      out[q.id] = "invalid";
    }
  }
  return out;
}

/**
 * The goal with the answers filled in.  One pass over the template, so an answer that itself
 * contains `{{something}}` is never expanded again.  An empty optional answer reads as "nothing
 * specified" instead of leaving a hole in the sentence.
 */
export function fillGoal(book: Pick<RecipeBook, "empty">, recipe: Recipe, answers: Answers, lang: string): string {
  const empty = pickLang(book.empty, lang);
  return recipeText(recipe, lang).goal.replace(PLACEHOLDER_RE, (_m, id: string) => {
    const v = (answers[id] ?? "").trim();
    return v === "" ? empty : v;
  });
}

/**
 * The first message of the task: the goal, then how to work on it alone (the same autonomy
 * guidance for every recipe).  The same text is what a schedule repeats.
 */
export function buildKickoff(book: Pick<RecipeBook, "empty" | "guidance">, recipe: Recipe, answers: Answers, lang: string): string {
  return `${fillGoal(book, recipe, answers, lang)}\n\n${pickLang(book.guidance, lang)}`;
}

/** The folder the bot should be confined to, if the recipe asks for one. */
export function recipeFolder(recipe: Recipe, answers: Answers): string | null {
  const q = recipe.questions.find((x) => x.kind === "folder");
  const v = q ? (answers[q.id] ?? "").trim() : "";
  return v === "" ? null : v;
}
