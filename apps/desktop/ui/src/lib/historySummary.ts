import type { Bot } from "../api/types.ts";
import type { PatchOp } from "./botPatch.ts";

/**
 * Whether the daemon keeps a rolling summary of the older part of this bot's long chats
 * (`history.summary` in the profile). On unless the owner switched it off.
 */
export function historySummaryOn(config: unknown): boolean {
  const history = (config as { history?: { summary?: unknown } } | null | undefined)?.history;
  return history?.summary !== false;
}

/** The one field-level edit that flips the switch: "on" is the default, so it just drops the key. */
export function historySummaryOps(on: boolean): PatchOp[] {
  return [on ? { op: "unset", path: "/history/summary" } : { op: "set", path: "/history/summary", value: false }];
}

export function historySummaryPatch(bot: Bot, on: boolean): { expected_version: number; ops: PatchOp[] } {
  return { expected_version: bot.version, ops: historySummaryOps(on) };
}
