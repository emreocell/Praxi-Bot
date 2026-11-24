import type { Bot, BotSpendLimits, Run } from "../api/types.ts";
import type { ControlSnapshot } from "../overlay/events.ts";
import { itemTs, type EventsState, type NeedsYouEntry } from "../state/timeline.ts";
import { isProcessingRunState } from "./runState.ts";

export type BotStatus = "idle" | "running" | "approval" | "paused";
export type BotFilter = "all" | BotStatus;
export type BotSort = "name" | "activity" | "cost";
export interface OverviewBot {
  bot: Bot;
  status: BotStatus;
  lastActivity: number | null;
  pausedRunId?: string;
  needsChatId?: string;
}

/** Events take precedence over a run-list snapshot still catching up after a finish. */
export function overviewBots(
  bots: readonly Bot[], runs: readonly Run[], events: EventsState,
  needs: readonly NeedsYouEntry[], native: ControlSnapshot | null
): OverviewBot[] {
  return bots.map((bot) => {
    const facts = runs.filter((r) => r.bot_id === bot.id);
    const live = Object.values(events.runs).filter((r) => r.botId === bot.id);
    const pending = needs.find((n) => n.botId === bot.id);
    const unfinished = facts.filter((r) => events.runs[r.id]?.finishedMs === undefined);
    const running = unfinished.some((r) => isProcessingRunState(r.state)) || live.some(
      (r) => r.finishedMs === undefined && !facts.some((f) => f.id === r.id && !isProcessingRunState(f.state))
    );
    const paused = unfinished.find((r) => r.state === "paused");
    const ownControl = native?.bot?.botId === bot.id ? native.mode : null;
    const times = [
      ...facts.map((r) => r.updated_at_ms ?? r.created_at_ms),
      ...live.flatMap((r) => [r.startedMs, r.finishedMs ?? 0, ...r.items.map(itemTs)])
    ].filter((ms) => Number.isFinite(ms) && ms > 0);
    const status: BotStatus = pending || unfinished.some((r) => r.state === "waiting_approval") ? "approval" : ownControl === "paused" ? "paused" : running || ownControl === "bot_driving" ? "running" : paused ? "paused" : "idle";
    return {
      bot,
      status,
      lastActivity: times.length ? Math.max(...times) : null,
      pausedRunId: status === "paused" ? paused?.id : undefined,
      needsChatId: pending?.chatId || undefined
    };
  });
}

export function selectOverviewBots(
  rows: readonly OverviewBot[], query: string, filter: BotFilter, sort: BotSort,
  spend: Readonly<Record<string, BotSpendLimits | null>>, locale: string
): OverviewBot[] {
  const q = query.trim().toLocaleLowerCase(locale);
  return rows.filter((r) => (filter === "all" || r.status === filter) && r.bot.name.toLocaleLowerCase(locale).includes(q))
    .sort((a, b) => {
      if (sort === "activity") {
        const d = (b.lastActivity ?? -1) - (a.lastActivity ?? -1);
        if (d) return d;
      }
      if (sort === "cost") {
        // Missing data sorts last; it must never be presented as free usage.
        const d = (spend[b.bot.id]?.usage.daily.usd ?? -1) - (spend[a.bot.id]?.usage.daily.usd ?? -1);
        if (d) return d;
      }
      return a.bot.name.localeCompare(b.bot.name, locale) || a.bot.id.localeCompare(b.bot.id);
    });
}

export function dailyBudget(data: BotSpendLimits) {
  return {
    used: data.usage.daily.usd,
    limit: data.limits.daily?.hard_usd ?? (data.is_default ? data.default_daily_hard_usd : null)
  };
}

/** Native pause is computer-wide: never offer it for another bot or for a remote daemon. */
export function nativePauseAction(botId: string, local: boolean, native: ControlSnapshot): "pause" | "resume" | null {
  if (!local || native.bot?.botId !== botId) return null;
  return native.mode === "bot_driving" ? "pause" : native.mode === "paused" ? "resume" : null;
}
