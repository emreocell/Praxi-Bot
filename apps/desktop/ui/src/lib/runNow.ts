/**
 * What the bot is doing right now, for the right panel's "now" row and the chat's slim status
 * line: the chat's live run, its newest real tool step, how many sub-agents it has, and whether it
 * is waiting for the owner. Pure; the sentence itself is worded by the caller (`describeTool`).
 */
import { type EventsState, type RunState, topLevelRuns, type ToolItem } from "../state/timeline.ts";
import { currentStep } from "./activityPanel.ts";

export interface RunNow {
  run: RunState;
  startedMs: number;
  /** Newest tool step a person would call a step (no plumbing). */
  step: ToolItem | null;
  /** The step that waits for the owner's decision, if any. */
  approval: ToolItem | null;
  subagents: number;
}

/** The chat's live top-level run, or `null` when it is idle. */
export function runNow(events: EventsState, chatId: string | undefined): RunNow | null {
  if (!chatId) {
    return null;
  }
  const live = [...topLevelRuns(events, chatId)].reverse().find((r) => r.finishedMs === undefined);
  if (!live) {
    return null;
  }
  const approval =
    live.items.find((i): i is ToolItem => i.kind === "tool" && i.status === "awaiting_approval") ?? null;
  const subagents = Object.values(events.runs).filter((r) => r.parentRunId === live.id).length;
  return { run: live, startedMs: live.startedMs, step: currentStep(live), approval, subagents };
}

/**
 * What the BOT is doing now, for the panel's header: the open chat's live run first, else the
 * newest live run in any other chat of this bot (a bot working elsewhere is not "idle").
 */
export function botRunNow(
  events: EventsState,
  activeChatId: string | undefined,
  chatIds: readonly string[]
): RunNow | null {
  const here = runNow(events, activeChatId);
  if (here) return here;
  let best: RunNow | null = null;
  for (const id of chatIds) {
    if (id === activeChatId) continue;
    const other = runNow(events, id);
    if (other && (!best || other.startedMs > best.startedMs)) best = other;
  }
  return best;
}
