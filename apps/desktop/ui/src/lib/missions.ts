/**
 * Pure logic of the bot panel's "Missions" block: which missions to show, whether to keep polling,
 * the progress numbers, and the one request that answers every blocked item at once. The daemon
 * has no event for missions, so the block polls while something can still change.
 */
import type { Mission, MissionItem } from "../api/types.ts";

/** How often the Activity tab asks for the missions while one is running, paused or blocked. */
export const MISSION_POLL_MS = 5_000;

/** Running, paused or blocked: the mission can still change. */
export function isActiveMission(m: Pick<Mission, "state">): boolean {
  return m.state === "running" || m.state === "paused" || m.state === "blocked";
}

/** Share of the ledger that is done, 0 to 100 (0 while the bot has not listed any item yet). */
export function progressPercent(p: { done: number; total: number }): number {
  if (p.total <= 0) {
    return 0;
  }
  return Math.min(100, Math.max(0, Math.round((p.done / p.total) * 100)));
}

/**
 * The three numbers the owner reads: done, blocked on them, and left for the bot. The daemon sends
 * them without overlap (`left` is what the bot still has to work on), so they add up to the ledger
 * minus the items given up or skipped.
 */
export function missionCounts(p: { done: number; blocked: number; left: number }): {
  done: number;
  blocked: number;
  left: number;
} {
  return { done: p.done, blocked: p.blocked, left: p.left };
}

/** Newest first by the last change. */
function newestFirst(a: Mission, b: Mission): number {
  return b.updated_at_ms - a.updated_at_ms;
}

/** Every active mission and the newest finished one (its digest stays readable), active first. */
export function missionsToShow(all: readonly Mission[]): Mission[] {
  const active = all.filter(isActiveMission).sort(newestFirst);
  const lastEnded = all
    .filter((m) => !isActiveMission(m))
    .sort((a, b) => (b.finished_at_ms ?? b.updated_at_ms) - (a.finished_at_ms ?? a.updated_at_ms))[0];
  return lastEnded ? [...active, lastEnded] : active;
}

/** Poll only while the tab is on screen and at least one mission can still change. */
export function shouldPoll(all: readonly Mission[], visible: boolean): boolean {
  return visible && all.some(isActiveMission);
}

/** The chip's word for a mission state; "blocked" reads as "needs you". */
export type MissionChip = "running" | "paused" | "needsYou" | "done" | "failed" | "stopped";

export function missionChip(state: string): MissionChip {
  switch (state) {
    case "running":
    case "paused":
    case "done":
    case "failed":
      return state;
    case "blocked":
      return "needsYou";
    default:
      return "stopped";
  }
}

/** One line under the title: an i18n key (under `missions.headline`) and its counts. */
export function missionHeadline(m: Mission): { key: string; values: Record<string, number> } {
  const { done, blocked, left } = missionCounts(m.progress);
  const total = m.progress.total;
  const values = { done, blocked, left, total };
  switch (m.state) {
    case "running":
      return { key: total === 0 ? "missions.headline.planning" : "missions.headline.running", values };
    case "paused":
      return { key: "missions.headline.paused", values };
    case "blocked":
      return { key: blocked > 0 ? "missions.headline.needsYou" : "missions.headline.stuck", values };
    case "done":
      return { key: "missions.headline.done", values };
    case "failed":
      return { key: "missions.headline.failed", values };
    default:
      return { key: "missions.headline.stopped", values };
  }
}

/** What the owner has typed for one blocked item; an entry exists once they touched the item. */
export interface AnswerDraft {
  text: string;
  skip: boolean;
}

/** One answer as the daemon takes it: an answer, or `skip` (never both). */
export interface AnswerPayload {
  key: string;
  answer?: string;
  skip?: boolean;
}

/** A touched item with no words and no skip has nothing to send yet. */
export function isUnanswered(d: AnswerDraft): boolean {
  return !d.skip && d.text.trim() === "";
}

/**
 * The request body for the touched items that are still blocked. Untouched items are not sent (they
 * stay blocked); `unanswered` lists touched ones that are empty, and they hold the Send button back.
 */
export function buildAnswers(
  blocked: readonly Pick<MissionItem, "key">[],
  drafts: Readonly<Record<string, AnswerDraft>>
): { answers: AnswerPayload[]; unanswered: string[]; canSend: boolean } {
  const answers: AnswerPayload[] = [];
  const unanswered: string[] = [];
  for (const item of blocked) {
    const d = drafts[item.key];
    if (!d) {
      continue;
    }
    if (isUnanswered(d)) {
      unanswered.push(item.key);
    } else {
      answers.push(d.skip ? { key: item.key, skip: true } : { key: item.key, answer: d.text.trim() });
    }
  }
  return { answers, unanswered, canSend: answers.length > 0 && unanswered.length === 0 };
}
