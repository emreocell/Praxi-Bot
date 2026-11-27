import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { presetToAutonomy } from "./botProfile.ts";
import { launchRecipe, recipeBotConfig, type LaunchInput, type LaunchStage, type RecipeApi } from "./recipeLaunch.ts";
import { buildKickoff, buildMissionGoal, MAX_MISSION_GOAL, parseRecipeFile, type Recipe } from "./recipes.ts";

const locale = (lang: string): { recipes: { error: Record<string, string> } } =>
  JSON.parse(readFileSync(new URL(`../i18n/locales/${lang}.json`, import.meta.url), "utf8"));
const book = parseRecipeFile(JSON.parse(readFileSync(new URL("../data/recipes.json", import.meta.url), "utf8")));
const byId = (id: string): Recipe => book.recipes.find((r) => r.id === id)!;

interface Calls {
  bots: unknown[];
  chats: unknown[][];
  messages: unknown[][];
  schedules: unknown[];
  missions: Record<string, unknown>[];
}

function fakeApi(fail: Partial<Record<"createBot" | "createChat" | "sendMessage" | "createSchedule" | "startMission", number>> = {}) {
  const calls: Calls = { bots: [], chats: [], messages: [], schedules: [], missions: [] };
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
    },
    startMission: async (input: Record<string, unknown>) => {
      maybeFail("startMission");
      calls.missions.push(input);
      return { mission: { id: `mission-${calls.missions.length}`, chat_id: `mission-chat-${calls.missions.length}` } };
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
  assert.deepEqual(!first.ok && first.progress, { botId: "bot-1", level: "balanced" });
  const second = await launchRecipe(api, backlinks({ progress: !first.ok ? first.progress : undefined }));
  assert.equal(!second.ok && second.stage, "message");
  assert.deepEqual(!second.ok && second.progress, { botId: "bot-1", level: "balanced", chatId: "chat-1" });
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

// ---- recipes that describe long work start a mission, on the level the owner confirmed ----

const MISSION_RECIPES = ["backlinks", "leads", "price-watch", "weekly-report"];
const SHORT_RECIPES = ["daily-brief", "inbox-triage"];
const answersFor = (id: string): Record<string, string> =>
  ({
    backlinks: { site: "https://example.com", about: "Candles", count: "10" },
    leads: { sites: "https://a.example\nhttps://b.example", fields: "name, email", filename: "leads.csv" },
    "price-watch": { pages: "https://a.example/pricing", focus: "" },
    "weekly-report": { folder: "/home/me/work", audience: "my team" },
    "daily-brief": { topics: "tech", tasks: "" },
    "inbox-triage": { important: "", ignore: "" }
  })[id]!;
const launchOf = (id: string, over: Partial<LaunchInput> = {}): LaunchInput => ({
  ...backlinks(),
  recipe: byId(id),
  answers: answersFor(id),
  ...over
});

test("only the long recipes describe a mission; the short ones stay a chat run or a schedule", () => {
  assert.deepEqual(book.recipes.filter((r) => r.mission).map((r) => r.id).sort(), [...MISSION_RECIPES].sort());
  for (const id of SHORT_RECIPES) assert.equal(byId(id).mission, undefined, id);
});

test("a long recipe on the Full level starts a mission with the goal, a title and the recipe's limits, and no chat message", async () => {
  for (const id of MISSION_RECIPES) {
    const { api, calls } = fakeApi();
    const input = launchOf(id, { level: "auto" });
    const res = await launchRecipe(api, input);
    assert.deepEqual(res, { ok: true, botId: "bot-1", chatId: "mission-chat-1", missionId: "mission-1", schedule: "none" }, id);
    assert.equal(calls.missions.length, 1, id);
    const m = calls.missions[0]!;
    assert.equal(m.bot_id, "bot-1");
    assert.equal(m.title, byId(id).text.en!.name);
    assert.equal(m.goal, buildMissionGoal(book, input.recipe, input.answers, "en"), id);
    assert.match(String(m.goal), /Mission plan: .*one ledger item per/s, id);
    // The mission has its own chat (made by the daemon): no second chat, no first message.
    assert.equal(calls.chats.length + calls.messages.length, 0, id);
    // Every limit the daemon wants is spelled out, the recipe's own three and the two stall/failure limits.
    assert.deepEqual(m.budgets, { ...byId(id).mission!.budgets, max_stalled_sessions: 4, max_failed_sessions: 3 }, id);
  }
});

test("the bot of a mission is created on the Full level the owner confirmed, and the mission goal is the answers filled in", async () => {
  const { api, calls } = fakeApi();
  await launchRecipe(api, launchOf("leads", { level: "auto" }));
  const cfg = (calls.bots[0] as { config: { permissions: { preset: string } } }).config;
  assert.equal(presetToAutonomy(cfg.permissions.preset as never), "auto");
  const goal = String(calls.missions[0]!.goal);
  assert.match(goal, /leads\.csv/);
  assert.match(goal, /https:\/\/b\.example/);
  assert.doesNotMatch(goal, /\{\{/);
  // The chat guidance (progress file, "ask me") does not go into a mission: the ledger replaces it.
  assert.doesNotMatch(goal, /progress file/);
});

test("below the Full level a long recipe is an ordinary chat run: no mission, and the level is never raised for it", async () => {
  for (const level of ["ask", "balanced"] as const) {
    for (const id of MISSION_RECIPES) {
      const { api, calls } = fakeApi();
      const input = launchOf(id, { level });
      const res = await launchRecipe(api, input);
      assert.deepEqual(res, { ok: true, botId: "bot-1", chatId: "chat-1", schedule: "none" }, `${id} ${level}`);
      assert.equal(calls.missions.length, 0, `${id} ${level}`);
      assert.deepEqual(calls.messages[0], ["chat-1", buildKickoff(book, input.recipe, input.answers, "en")], `${id} ${level}`);
      const cfg = (calls.bots[0] as { config: { permissions: { preset: string } } }).config;
      assert.equal(presetToAutonomy(cfg.permissions.preset as never), level, `${id} ${level}`);
    }
  }
});

test("a short recipe stays a chat run even on the Full level", async () => {
  for (const id of SHORT_RECIPES) {
    const { api, calls } = fakeApi();
    const res = await launchRecipe(api, launchOf(id, { level: "auto" }));
    assert.equal(res.ok && res.missionId, undefined, id);
    assert.equal(calls.missions.length, 0, id);
    assert.equal(calls.messages.length, 1, id);
  }
});

test("a mission recipe with a schedule starts the mission and still repeats the kickoff as a normal run", async () => {
  const { api, calls } = fakeApi();
  const input = launchOf("price-watch", { level: "auto", schedule: { freq: "daily", time: "09:00" } });
  const res = await launchRecipe(api, input);
  assert.deepEqual(res, { ok: true, botId: "bot-1", chatId: "mission-chat-1", missionId: "mission-1", schedule: "created" });
  const s = calls.schedules[0] as { payload: unknown };
  assert.deepEqual(s.payload, { kind: "agent_prompt", text: buildKickoff(book, input.recipe, input.answers, "en") });
});

test("a failed mission start keeps the bot; the retry starts one mission, never a second bot", async () => {
  const { api, calls } = fakeApi({ startMission: 1 });
  const first = await launchRecipe(api, launchOf("backlinks", { level: "auto" }));
  assert.equal(!first.ok && first.stage, "mission");
  assert.deepEqual(!first.ok && first.progress, { botId: "bot-1", level: "auto" });
  const again = await launchRecipe(api, launchOf("backlinks", { level: "auto", progress: !first.ok ? first.progress : undefined }));
  assert.equal(again.ok && again.missionId, "mission-1");
  assert.equal(calls.bots.length, 1);
  assert.equal(calls.missions.length, 1);
  // Once the mission exists a further call (a double click) starts nothing new.
  const third = await launchRecipe(api, launchOf("backlinks", { level: "auto", progress: { botId: "bot-1", level: "auto", missionId: "mission-1", chatId: "mission-chat-1" } }));
  assert.equal(third.ok && third.missionId, "mission-1");
  assert.equal(calls.missions.length, 1);
});

test("a retry on another level than the bot was created with is refused before anything else happens", async () => {
  const { api, calls } = fakeApi({ startMission: 1 });
  const first = await launchRecipe(api, launchOf("backlinks", { level: "auto" }));
  const progress = !first.ok ? first.progress : undefined;
  const lowered = await launchRecipe(api, launchOf("backlinks", { level: "balanced", progress }));
  assert.equal(!lowered.ok && lowered.stage, "level");
  assert.equal(calls.bots.length, 1);
  assert.equal(calls.chats.length + calls.messages.length + calls.missions.length, 0);
  // The other way round: a bot made at balanced is not silently run as a mission.
  const chat = fakeApi({ sendMessage: 1 });
  const stuck = await launchRecipe(chat.api, launchOf("backlinks", { level: "balanced" }));
  const raised = await launchRecipe(chat.api, launchOf("backlinks", { level: "auto", progress: !stuck.ok ? stuck.progress : undefined }));
  assert.equal(!raised.ok && raised.stage, "level");
  assert.equal(chat.calls.missions.length, 0);
});

test("a goal too long for a mission creates nothing, while the same answers still work as a chat run", async () => {
  const long = "x".repeat(1900);
  const answers = { sites: long, fields: long, filename: "leads.csv" };
  assert.ok(buildMissionGoal(book, byId("leads"), answers, "en").length > MAX_MISSION_GOAL);
  const { api, calls } = fakeApi();
  const res = await launchRecipe(api, launchOf("leads", { level: "auto", answers }));
  assert.equal(!res.ok && res.stage, "goal");
  assert.equal(calls.bots.length, 0);
  const chat = await launchRecipe(fakeApi().api, launchOf("leads", { level: "balanced", answers }));
  assert.equal(chat.ok, true);
});

test("the shipped mission goals fit the daemon's limit with every answer at its longest", () => {
  for (const id of MISSION_RECIPES) {
    const recipe = byId(id);
    for (const lang of ["en", "tr"]) {
      const answers = Object.fromEntries(recipe.questions.map((q) => [q.id, q.kind === "choice" ? q.options![0]! : q.kind === "url" ? "https://example.com" : q.kind === "folder" ? "/home/me/work" : "short"]));
      assert.ok(buildMissionGoal(book, recipe, answers, lang).length < MAX_MISSION_GOAL / 2, `${id} ${lang}`);
    }
  }
});

test("every launch stage has its message in English and Turkish", () => {
  const stages: Record<LaunchStage, true> = { check: true, goal: true, level: true, bot: true, chat: true, message: true, mission: true };
  for (const stage of Object.keys(stages)) {
    for (const lang of ["en", "tr"]) assert.ok(locale(lang).recipes.error[stage], `${lang} ${stage}`);
  }
});
