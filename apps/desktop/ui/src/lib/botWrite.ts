import type { ApiClient } from "../api/client.ts";
import type { Bot, BotInput } from "../api/types.ts";
import { conflictBot } from "./botPatch.ts";
import { humanLine } from "./humanError.ts";

/** A stale (`409 version_conflict`) or missing (`428`) profile version: reload and try again. */
export function isVersionError(e: unknown): boolean {
  const err = e as { status?: number; code?: string } | null;
  return !!err && ((err.status === 409 && err.code === "version_conflict") || err.status === 428);
}

/**
 * Replace a bot's profile with the version it was loaded at. A version error reloads the bot
 * once and builds the input again from the fresh one; a second one is a real conflict and is
 * thrown (show it with `botWriteMessage`).
 */
export async function writeBotProfile(
  client: Pick<ApiClient, "getBot" | "updateBot">,
  bot: Bot,
  build: (current: Bot) => BotInput
): Promise<Bot> {
  let current = bot;
  for (let attempt = 0; ; attempt += 1) {
    try {
      return await client.updateBot(current.id, {
        ...build(current),
        expected_version: current.version
      });
    } catch (e) {
      if (attempt >= 1 || !isVersionError(e)) throw e;
      current = conflictBot(e) ?? (await client.getBot(current.id));
    }
  }
}

/** The engine step failed and so did putting the old model back: the bot stays on the new one. */
export class RollbackFailedError extends Error {
  /** Why the engine step failed (the first error). */
  reason: unknown;
  /** Why the old model could not be restored. */
  undoError: unknown;
  constructor(reason: unknown, undoError: unknown) {
    super("the engine step failed and the previous model could not be restored");
    this.name = "RollbackFailedError";
    this.reason = reason;
    this.undoError = undoError;
  }
}

/**
 * Save a profile, then run the step that has to follow it (the engine's consent). When that step
 * fails, `undo` puts the profile back and the error still reaches the caller: a bot is never left
 * on a model that cannot answer because its second step failed. When the undo fails too, a
 * `RollbackFailedError` carries both reasons, so the caller can say the bot is still on the new model.
 */
export async function writeThenApply(
  write: () => Promise<Bot>,
  apply: (saved: Bot) => Promise<void>,
  undo: (saved: Bot) => Promise<unknown>
): Promise<Bot> {
  const saved = await write();
  try {
    await apply(saved);
  } catch (e) {
    try {
      await undo(saved);
    } catch (undoError) {
      throw new RollbackFailedError(e, undoError);
    }
    throw e;
  }
  return saved;
}

/** Human sentence for a failed bot write: never the daemon's raw version message. */
export function botWriteMessage(e: unknown, t: (key: string) => string): string {
  if (isVersionError(e)) return t("bots.writeConflict");
  return humanLine(e);
}
