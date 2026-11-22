import type { NotifyKind } from "./notifyRules.ts";
import { type QuietHours, shouldHold } from "./quietHours.ts";

/**
 * Per-bot mute (#230).  A muted bot stays silent about finished and failed runs only.  Its
 * approvals and questions keep following the quiet-hours rule and always stay in "Needs you",
 * so a mute can never hide a request that waits for a person.
 */

/** Upper bound on stored ids, so a corrupt store cannot grow without limit. */
export const MUTED_BOTS_MAX = 500;

/** Tolerant parse of stored data: unique, non-empty strings in their stored order. */
export function parseMutedBots(raw: unknown): string[] {
  if (!Array.isArray(raw)) {
    return [];
  }
  const ids = raw.filter((x): x is string => typeof x === "string" && x.trim() !== "");
  return [...new Set(ids)].slice(0, MUTED_BOTS_MAX);
}

export function isBotMuted(muted: readonly string[], botId: string): boolean {
  return botId !== "" && muted.includes(botId);
}

/** Mute or unmute `botId`; returns the same array when nothing changes. */
export function setBotMuted(muted: string[], botId: string, on: boolean): string[] {
  if (botId === "" || isBotMuted(muted, botId) === on) {
    return muted;
  }
  return on ? [...muted, botId] : muted.filter((id) => id !== botId);
}

/** The kinds a mute silences: never "needsYou". */
export function isMuteable(kind: NotifyKind): boolean {
  return kind === "longRun" || kind === "failed" || kind === "scheduleDone";
}

export type HoldReason = "quiet" | "muted";

/**
 * Why a toast is held back, or null to show it.  The mute is checked first: it is the more
 * specific reason and holds the toast at any hour.
 */
export function holdReason(
  at: Date,
  quiet: QuietHours,
  kind: NotifyKind,
  botId: string,
  muted: readonly string[]
): HoldReason | null {
  if (isMuteable(kind) && isBotMuted(muted, botId)) {
    return "muted";
  }
  return shouldHold(at, quiet, kind === "needsYou") ? "quiet" : null;
}
