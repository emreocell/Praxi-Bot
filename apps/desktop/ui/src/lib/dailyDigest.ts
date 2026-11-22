/**
 * The daily digest: what each bot did today and what it cost, built only from data the app
 * already has (the run list, the usage summary and the per-bot limits). No model is called.
 */

/** The run fields the digest reads. */
export interface DigestRun {
  bot_id: string;
  state: string;
  created_at_ms: number;
}

/** One bot's usage since the start of the day (a row of `GET /usage` `by_bot`). */
export interface DigestUsage {
  bot_id: string;
  model_calls: number;
  prompt_tokens: number;
  completion_tokens: number;
  cost_usd?: number | null;
}

/** A bot's daily hard limits; `null` means no limit of that kind. */
export interface DigestLimit {
  hardUsd: number | null;
  hardTokens: number | null;
}

export interface BotDigest {
  botId: string;
  done: number;
  failed: number;
  stopped: number;
  /** Runs that wait for the owner (paused, or asking for approval). */
  waiting: number;
  running: number;
  calls: number;
  tokens: number;
  costUsd: number | null;
  /** 0..1+ share of the daily limit already used, or `null` without a limit. */
  limitUsed: number | null;
}

/** Local midnight of `now`, in epoch milliseconds. */
export function startOfLocalDay(now: Date): number {
  return new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
}

/** How a run's state counts in the digest. */
export function digestBucket(state: string): "done" | "failed" | "stopped" | "waiting" | "running" {
  switch (state) {
    case "succeeded":
      return "done";
    case "failed":
      return "failed";
    case "cancelled":
      return "stopped";
    case "paused":
    case "waiting_approval":
      return "waiting";
    default:
      return "running";
  }
}

/**
 * The largest share of a daily limit that is used up (money and tokens each count when they
 * have a limit), or `null` when the bot has no daily limit at all.
 */
export function limitUsed(
  limit: DigestLimit | null | undefined,
  costUsd: number | null,
  tokens: number
): number | null {
  const shares: number[] = [];
  if (limit?.hardUsd && limit.hardUsd > 0 && costUsd !== null) {
    shares.push(costUsd / limit.hardUsd);
  }
  if (limit?.hardTokens && limit.hardTokens > 0) {
    shares.push(tokens / limit.hardTokens);
  }
  return shares.length > 0 ? Math.max(...shares) : null;
}

/**
 * One entry per bot that did something since `dayStartMs`, the ones that need attention first
 * (waiting, then failed), then the busiest. Bots with no run and no model call are left out.
 */
export function buildDigest(input: {
  runs: readonly DigestRun[];
  usage: readonly DigestUsage[];
  limits: ReadonlyMap<string, DigestLimit>;
  dayStartMs: number;
}): BotDigest[] {
  const byBot = new Map<string, BotDigest>();
  const entry = (botId: string): BotDigest => {
    let e = byBot.get(botId);
    if (!e) {
      e = {
        botId,
        done: 0,
        failed: 0,
        stopped: 0,
        waiting: 0,
        running: 0,
        calls: 0,
        tokens: 0,
        costUsd: null,
        limitUsed: null
      };
      byBot.set(botId, e);
    }
    return e;
  };
  for (const run of input.runs) {
    if (run.created_at_ms >= input.dayStartMs) {
      entry(run.bot_id)[digestBucket(run.state)] += 1;
    }
  }
  for (const u of input.usage) {
    if (u.model_calls <= 0 && !byBot.has(u.bot_id)) {
      continue;
    }
    const e = entry(u.bot_id);
    e.calls = u.model_calls;
    e.tokens = u.prompt_tokens + u.completion_tokens;
    e.costUsd = u.cost_usd ?? null;
  }
  const out = [...byBot.values()];
  for (const e of out) {
    e.limitUsed = limitUsed(input.limits.get(e.botId), e.costUsd, e.tokens);
  }
  const runs = (e: BotDigest) => e.done + e.failed + e.stopped + e.waiting + e.running;
  return out.sort(
    (a, b) =>
      b.waiting - a.waiting || b.failed - a.failed || runs(b) - runs(a) || b.calls - a.calls
  );
}
