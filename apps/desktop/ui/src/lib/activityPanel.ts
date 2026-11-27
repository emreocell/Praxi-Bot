/**
 * Pure logic of the right panel's "Activity" tab (card 255): the live list of sub-agents of the
 * chat's latest run, the "Upcoming" rows from the scheduler, and which sections have anything to
 * show (a section without data is not rendered at all - no empty headers).
 */
import type { Schedule } from "../api/types.ts";
import {
  childRunsOf,
  type EventsState,
  type RunState,
  subagentStatus,
  topLevelRuns,
  type ToolItem
} from "../state/timeline.ts";
import { isDiscoveryTool } from "./toolLabels.ts";

export type SubagentRowState =
  | "running"
  | "waiting"
  | "done"
  | "failed"
  | "stopped"
  | "paused";

/** What the panel shows for a run's status chip (the daemon says `succeeded`, people say done). */
export function subagentRowState(run: RunState): SubagentRowState {
  switch (subagentStatus(run)) {
    case "waiting_approval":
      return "waiting";
    case "succeeded":
      return "done";
    case "cancelled":
      return "stopped";
    case "failed":
      return "failed";
    case "paused":
      return "paused";
    default:
      return "running";
  }
}

export interface SubagentRow {
  id: string;
  /** 0 for a child of the main run, 1 for a grandchild, ... */
  depth: number;
  task: string;
  state: SubagentRowState;
  startedMs: number;
  /** Set once the run ended; elapsed time stops there. */
  endedMs?: number;
  /** The newest real tool step (discovery plumbing never counts), for one human sentence. */
  step: ToolItem | null;
  /** Only a running / waiting sub-agent can be stopped. */
  canStop: boolean;
}

/** The last tool call a person would call a step: no `find_tool` / `get_tool_details`, no spawn. */
export function currentStep(run: RunState): ToolItem | null {
  for (let i = run.items.length - 1; i >= 0; i--) {
    const item = run.items[i];
    if (
      item?.kind === "tool" &&
      !isDiscoveryTool(item.name) &&
      !/^(subagent|spawn_agent|spawn_subagent|delegate)/.test(item.name)
    ) {
      return item;
    }
  }
  return null;
}

/**
 * Sub-agents of the chat's latest top-level run (and theirs), oldest first, parents before
 * children.  Empty when that run spawned none.
 */
export function subagentRows(events: EventsState, chatId: string | undefined): SubagentRow[] {
  if (!chatId) {
    return [];
  }
  const roots = topLevelRuns(events, chatId);
  const root = roots[roots.length - 1];
  if (!root) {
    return [];
  }
  const out: SubagentRow[] = [];
  const seen = new Set<string>([root.id]);
  const walk = (parent: string, depth: number) => {
    for (const run of childRunsOf(events, parent)) {
      if (seen.has(run.id)) {
        continue;
      }
      seen.add(run.id);
      const state = subagentRowState(run);
      out.push({
        id: run.id,
        depth,
        task: (run.task ?? "").replace(/\s+/g, " ").trim(),
        state,
        startedMs: run.startedMs,
        endedMs: run.finishedMs,
        step: currentStep(run),
        canStop: state === "running" || state === "waiting"
      });
      walk(run.id, depth + 1);
    }
  };
  walk(root.id, 0);
  return out;
}

/** `0:07`, `2:41`, `1:02:03` - no words, so it reads the same in every language. */
export function formatElapsed(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const ss = String(s).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${ss}` : `${m}:${ss}`;
}

export interface UpcomingRow {
  id: string;
  name: string;
  enabled: boolean;
  /** Next timer firing; undefined for a trigger-only schedule (file, email, webhook). */
  nextMs?: number;
  trigger: boolean;
}

const MAX_UPCOMING = 3;

/** The bot's schedules worth listing: armed timers first (soonest first), then paused ones. */
export function upcomingRows(schedules: readonly Schedule[], botId: string): UpcomingRow[] {
  const rows: UpcomingRow[] = [];
  for (const s of schedules) {
    if (s.bot_id !== botId) {
      continue;
    }
    const trigger = s.has_webhook || (s.trigger !== undefined && s.trigger !== null);
    const nextMs = s.next_run_at_ms ?? undefined;
    // an enabled schedule with no next firing and no trigger is finished
    if (s.enabled && nextMs === undefined && !trigger) {
      continue;
    }
    rows.push({ id: s.id, name: s.name, enabled: s.enabled, nextMs, trigger });
  }
  rows.sort((a, b) => {
    if (a.enabled !== b.enabled) {
      return a.enabled ? -1 : 1;
    }
    return (a.nextMs ?? Infinity) - (b.nextMs ?? Infinity) || a.name.localeCompare(b.name);
  });
  return rows.slice(0, MAX_UPCOMING);
}

export type ActivitySection = "subagents" | "upcoming" | "computer" | "quota";

/**
 * Sections of the Activity tab in order.  A section with no data is left out (owner rule: no
 * empty headers).  The computer card always has something honest to say, even with no target.
 */
export function activitySections(have: {
  subagents: number;
  upcoming: number;
  quota: number;
}): ActivitySection[] {
  const out: ActivitySection[] = [];
  if (have.subagents > 0) out.push("subagents");
  out.push("computer");
  if (have.upcoming > 0) out.push("upcoming");
  if (have.quota > 0) out.push("quota");
  return out;
}
