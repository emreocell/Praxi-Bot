import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { presetToAutonomy } from "./botProfile.ts";
import { launchRecipe, recipeBotConfig, type LaunchInput, type RecipeApi } from "./recipeLaunch.ts";
import { buildKickoff, parseRecipeFile, type Recipe } from "./recipes.ts";

const book = parseRecipeFile(JSON.parse(readFileSync(new URL("../data/recipes.json", import.meta.url), "utf8")));
const byId = (id: string): Recipe => book.recipes.find((r) => r.id === id)!;

interface Calls {
  bots: unknown[];
  chats: unknown[][];
  messages: unknown[][];
  schedules: unknown[];
}

function fakeApi(fail: Partial<Record<"createBot" | "createChat" | "sendMessage" | "createSchedule", number>> = {}) {
  const calls: Calls = { bots: [], chats: [], messages: [], schedules: [] };
  const left = { ...fail };
  const maybeFail = (name: keyof typeof fail) => {
    if ((left[name] ?? 0) > 0) {
      left[name]! -= 1;
      throw new Error(`${name} failed`);
    }
  };
  const api = {
    createBot: async (input: unknown) => {
      maybeFail("createBot");
      calls.bots.push(input);
      return { id: `bot-${calls.bots.length}`, version: 1, config: (input as { config: unknown }).config };
    },
    createChat: async (...args: unknown[]) => {
      maybeFail("createChat");
      calls.chats.push(args);
      return { id: `chat-${calls.chats.length}` };
    },
    sendMessage: async (...args: unknown[]) => {
      maybeFail("sendMessage");
      calls.messages.push(args);
      return {};
    },
    createSchedule: async (input: unknown) => {
      maybeFail("createSchedule");
      calls.schedules.push(input);
      return { id: `sched-${calls.schedules.length}` };
    }
  } as unknown as RecipeApi;
  return { api, calls };
}

const executor = { provider: "anthropic", model: "claude-sonnet-5-5" };
const backlinks = (over: Partial<LaunchInput> = {}): LaunchInput => ({
  recipe: byId("backlinks"),
  book,
  answers: { site: "https://example.com", about: "Candles", count: "10" },
  lang: "en",
  level: "balanced",
  executor,
  schedule: null,
  tz: "Europe/Istanbul",
  ...over
});

test("the flow creates the bot, opens a chat and sends the kickoff, in that order", async () => {
  const { api, calls } = fakeApi();
  const input = backlinks();
  const res = await launchRecipe(api, input);
  assert.deepEqual(res, { ok: true, botId: "bot-1", chatId: "chat-1", schedule: "none" });
  assert.equal(calls.bots.length, 1);
  assert.deepEqual(calls.chats[0], ["bot-1", "Get backlinks for my site"]);
  assert.deepEqual(calls.messages[0], ["chat-1", buildKickoff(book, input.recipe, input.answers, "en")]);
  assert.equal(calls.schedules.length, 0);
  assert.match(String(calls.messages[0]![1]), /Aim for 10 sites/);
});

test("the bot gets exactly the level the owner confirmed, never the recipe's own", () => {
  const level = (l: "ask" | "balanced" | "auto") => {
    const cfg = recipeBotConfig(byId("price-watch"), { pages: "https://x.example", focus: "" }, "en", l, executor) as { permissions: { preset: string } };
    return presetToAutonomy(cfg.permissions.preset as never);
  };
  assert.equal(level("ask"), "ask");
  assert.equal(level("balanced"), "balanced");
  assert.equal(level("auto"), "auto");
});

test("the bot is named in the UI language and takes its look and model from the recipe and the owner", () => {
  const cfg = recipeBotConfig(byId("backlinks"), {}, "tr", "balanced", executor) as { bot: Record<string, string>; models: Record<string, string> };
  assert.equal(cfg.bot.name, "Bağlantı kurucu");
  assert.equal(cfg.bot.shape, "diamond");
  assert.equal(cfg.bot.color, "#2fc4b2");
  assert.equal(cfg.models.executor, "anthropic/claude-sonnet-5-5");
});

test("a folder question confines the bot to that folder; without one it is not confined", () => {
  const weekly = recipeBotConfig(byId("weekly-report"), { folder: "/home/me/work" }, "en", "balanced", executor) as { files?: unknown };
  assert.deepEqual(weekly.files, { mode: "folder", folders: ["/home/me/work"] });
  const open = recipeBotConfig(byId("backlinks"), {}, "en", "balanced", executor) as { files?: unknown };
  assert.equal(open.files, undefined);
});

test("an incomplete form creates nothing", async () => {
  const { api, calls } = fakeApi();
  const res = await launchRecipe(api, backlinks({ answers: { site: "", about: "", count: "10" } }));
  assert.equal(res.ok, false);
  assert.equal(!res.ok && res.stage, "check");
  assert.equal(calls.bots.length, 0);
});

test("a schedule is created only when asked for, with the kickoff as its prompt and the owner's time", async () => {
  const { api, calls } = fakeApi();
  const recipe = byId("inbox-triage");
  const input = backlinks({ recipe, answers: { important: "", ignore: "" }, schedule: { freq: "weekdays", time: "07:45" } });
  const res = await launchRecipe(api, input);
  assert.deepEqual(res, { ok: true, botId: "bot-1", chatId: "chat-1", schedule: "created" });
  const s = calls.schedules[0] as Record<string, unknown>;
  assert.equal(s.bot_id, "bot-1");
  assert.equal(s.name, "Triage my inbox every morning");
  assert.deepEqual(s.spec, { cron: "45 7 * * 1-5 Europe/Istanbul" });
  assert.deepEqual(s.payload, { kind: "agent_prompt", text: buildKickoff(book, recipe, input.answers, "en") });
  assert.equal(s.enabled, true);
  assert.equal(s.misfire_policy, "skip");
});

test("a weekly schedule uses the chosen weekday", async () => {
  const { api, calls } = fakeApi();
  const recipe = byId("weekly-report");
  await launchRecipe(api, backlinks({ recipe, answers: { folder: "/w", audience: "" }, schedule: { freq: "weekly", time: "17:00", weekday: 5 } }));
  assert.deepEqual((calls.schedules[0] as { spec: unknown }).spec, { cron: "0 17 * * 5 Europe/Istanbul" });
});

test("a failing schedule never undoes the running task and is reported", async () => {
  const { api, calls } = fakeApi({ createSchedule: 1 });
  const res = await launchRecipe(api, backlinks({ schedule: { freq: "daily", time: "09:00" } }));
  assert.equal(res.ok, true);
  assert.equal(res.ok && res.schedule, "failed");
  assert.equal(typeof (res.ok && res.scheduleError), "string");
  assert.equal(calls.messages.length, 1);
});

test("a failure in the bot step creates nothing else; a retry then starts clean", async () => {
  const { api, calls } = fakeApi({ createBot: 1 });
  const first = await launchRecipe(api, backlinks());
  assert.equal(!first.ok && first.stage, "bot");
  assert.deepEqual(!first.ok && first.progress, {});
  assert.equal(calls.chats.length + calls.messages.length, 0);
  const again = await launchRecipe(api, backlinks({ progress: !first.ok ? first.progress : undefined }));
  assert.equal(again.ok, true);
  assert.equal(calls.bots.length, 1);
});

test("a retry after a chat or message failure never creates a second bot or sends the task twice", async () => {
  const { api, calls } = fakeApi({ createChat: 1, sendMessage: 1 });
  const first = await launchRecipe(api, backlinks());
  assert.equal(!first.ok && first.stage, "chat");
  assert.deepEqual(!first.ok && first.progress, { botId: "bot-1" });
  const second = await launchRecipe(api, backlinks({ progress: !first.ok ? first.progress : undefined }));
  assert.equal(!second.ok && second.stage, "message");
  assert.deepEqual(!second.ok && second.progress, { botId: "bot-1", chatId: "chat-1" });
  const third = await launchRecipe(api, backlinks({ progress: !second.ok ? second.progress : undefined }));
  assert.deepEqual(third, { ok: true, botId: "bot-1", chatId: "chat-1", schedule: "none" });
  assert.equal(calls.bots.length, 1);
  assert.equal(calls.chats.length, 1);
  assert.equal(calls.messages.length, 1);
});

test("the engine hook runs once, right after the bot exists, and a failure in it stops before a chat is opened", async () => {
  const { api, calls } = fakeApi();
  const seen: string[] = [];
  await launchRecipe(api, backlinks({ onBotCreated: async (b) => void seen.push(b.id) }));
  assert.deepEqual(seen, ["bot-1"]);
  const second = fakeApi();
  const res = await launchRecipe(second.api, backlinks({ onBotCreated: async () => { throw new Error("no consent"); } }));
  assert.equal(!res.ok && res.stage, "bot");
  assert.equal(second.calls.chats.length, 0);
  assert.equal(calls.bots.length, 1);
});
