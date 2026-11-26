import { normalizeBotShape } from "ui/bot-shape";
import type { ApiClient } from "../api/client.ts";
import type { Bot, Schedule } from "../api/types.ts";
import { type AutonomyLevel, autonomyToPreset, configFromDraft, draftFromBot, type ModelRef } from "./botProfile.ts";
import { humanLine } from "./humanError.ts";
import { DEFAULT_SIMPLE, simpleSpec, type SimpleSchedule } from "./scheduleBuilder.ts";
import {
  type Answers,
  answerProblems,
  buildKickoff,
  type Recipe,
  type RecipeBook,
  recipeBotName,
  recipeFolder,
  recipeText
} from "./recipes.ts";

/** The slice of the daemon client the creation flow needs (a mock fits too). */
export type RecipeApi = Pick<ApiClient, "createBot" | "createChat" | "sendMessage" | "createSchedule">;

/** What a retry must not redo: each step that already succeeded. */
export interface LaunchProgress {
  botId?: string;
  chatId?: string;
  messageSent?: boolean;
  /** `false`: the schedule was tried and failed (the task itself is running; it is not retried). */
  scheduleId?: string | false;
}

export interface LaunchInput {
  recipe: Recipe;
  book: Pick<RecipeBook, "empty" | "guidance">;
  answers: Answers;
  lang: string;
  /** What the owner confirmed in the review step.  This, never the recipe's own level, is applied. */
  level: AutonomyLevel;
  executor: ModelRef;
  /** `null`: no schedule.  The owner turned the repeat on and may have changed the time. */
  schedule: (Pick<SimpleSchedule, "freq" | "time"> & { weekday?: number }) | null;
  tz: string;
  progress?: LaunchProgress;
  /** Called once, right after the bot exists (the engine consent of a coding-CLI model is applied here). */
  onBotCreated?: (bot: Bot) => Promise<void>;
}

export type LaunchStage = "check" | "bot" | "chat" | "message";

export type LaunchResult =
  | { ok: true; botId: string; chatId: string; schedule: "created" | "none" | "failed"; scheduleError?: string }
  | { ok: false; stage: LaunchStage; message: string; progress: LaunchProgress };

/** The bot profile a recipe asks for, at the level the owner confirmed. */
export function recipeBotConfig(
  recipe: Recipe,
  answers: Answers,
  lang: string,
  level: AutonomyLevel,
  executor: ModelRef
): Record<string, unknown> {
  const folder = recipeFolder(recipe, answers);
  const draft = {
    ...draftFromBot(undefined),
    name: recipeBotName(recipe, lang),
    shape: normalizeBotShape(recipe.bot.shape) ?? "pebble",
    color: recipe.bot.color,
    executor,
    preset: autonomyToPreset(level),
    // A folder question confines the bot to that folder: it reads and writes nowhere else.
    files: folder ? { mode: "folder" as const, folders: [folder] } : { mode: "computer" as const, folders: [] }
  };
  return configFromDraft(draft);
}

/**
 * Create the bot, open a chat and send the task, then (if the owner asked) schedule it.  Each step
 * is recorded in `progress`, so a retry after a failure continues instead of creating a second bot
 * or sending the task twice.  A failing schedule never undoes a running task.
 */
export async function launchRecipe(api: RecipeApi, input: LaunchInput): Promise<LaunchResult> {
  const { recipe, answers, lang } = input;
  const progress: LaunchProgress = { ...input.progress };
  const fail = (stage: LaunchStage, e: unknown): LaunchResult => ({ ok: false, stage, message: humanLine(e), progress: { ...progress } });

  // The form is checked again here: the flow never creates anything from an incomplete answer set.
  if (Object.keys(answerProblems(recipe, answers)).length > 0) {
    return { ok: false, stage: "check", message: "incomplete", progress };
  }

  if (!progress.botId) {
    try {
      const config = recipeBotConfig(recipe, answers, lang, input.level, input.executor);
      const bot = await api.createBot({ name: recipeBotName(recipe, lang), config });
      progress.botId = bot.id;
      await input.onBotCreated?.(bot);
    } catch (e) {
      return fail("bot", e);
    }
  }
  const botId = progress.botId!;

  if (!progress.chatId) {
    try {
      progress.chatId = (await api.createChat(botId, recipeText(recipe, lang).name)).id;
    } catch (e) {
      return fail("chat", e);
    }
  }
  const chatId = progress.chatId;

  const kickoff = buildKickoff(input.book, recipe, answers, lang);
  if (!progress.messageSent) {
    try {
      await api.sendMessage(chatId, kickoff);
      progress.messageSent = true;
    } catch (e) {
      return fail("message", e);
    }
  }

  if (!input.schedule) return { ok: true, botId, chatId, schedule: "none" };
  if (progress.scheduleId) return { ok: true, botId, chatId, schedule: "created" };
  if (progress.scheduleId === false) return { ok: true, botId, chatId, schedule: "failed" };
  const simple: SimpleSchedule = { ...DEFAULT_SIMPLE, ...input.schedule, weekday: input.schedule.weekday ?? DEFAULT_SIMPLE.weekday };
  const spec = simpleSpec(simple, input.tz);
  if (!spec) {
    progress.scheduleId = false;
    return { ok: true, botId, chatId, schedule: "failed", scheduleError: "time" };
  }
  try {
    const made: Schedule = await api.createSchedule({
      bot_id: botId,
      name: recipeText(recipe, lang).name,
      spec,
      payload: { kind: "agent_prompt", text: kickoff },
      enabled: true,
      misfire_policy: "skip"
    });
    progress.scheduleId = made.id;
    return { ok: true, botId, chatId, schedule: "created" };
  } catch (e) {
    progress.scheduleId = false;
    return { ok: true, botId, chatId, schedule: "failed", scheduleError: humanLine(e) };
  }
}
