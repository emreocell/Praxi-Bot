/**
 * Folds a run's timeline into "thinking chains" (card 198 D).
 *
 * A chain is a maximal stretch of ordinary tool calls plus the short thoughts
 * the bot wrote right before a tool call. Everything else keeps its own row:
 * the final answer (text that no tool call follows), questions, plans,
 * notices, subagents and any call that is waiting for approval (the approval
 * card must always stay visible on its own).
 */
import {
  isSubagentTool,
  type TimelineItem,
  type ToolItem
} from "../state/timeline.ts";

export type ChainStep =
  | { kind: "thought"; key: string; text: string; ts: number }
  | { kind: "tool"; key: string; tool: ToolItem };

export interface Chain {
  key: string;
  steps: ChainStep[];
  startMs: number;
  /** Where the "thought for N s" span begins (the run start for the first chain). */
  spanStartMs: number;
  /** Last moment anything in the chain finished (or was said). */
  endMs: number;
  /** The run is still going and this is its last stretch. */
  live: boolean;
  /** Number of calls that failed, were denied or expired. */
  problems: number;
}

export type Segment =
  | { kind: "chain"; chain: Chain }
  | { kind: "item"; item: TimelineItem };

/** A thought is short and one line; anything longer is the answer. */
export const MAX_THOUGHT_CHARS = 200;

export function isShortThought(text: string): boolean {
  const t = text.trim();
  return t.length > 0 && t.length <= MAX_THOUGHT_CHARS && !/\n/.test(t);
}

/** The first chain of a run also counts the model's time before its first call. */
function spanStart(
  first: number,
  end: number,
  isFirst: boolean,
  runStartedMs?: number
): number {
  if (
    isFirst &&
    runStartedMs !== undefined &&
    runStartedMs > 0 &&
    runStartedMs < first &&
    end - runStartedMs < 24 * 3600 * 1000
  ) {
    return runStartedMs;
  }
  return first;
}

/** A tool call the chain may absorb. */
export function chainable(item: TimelineItem): item is ToolItem {
  return (
    item.kind === "tool" &&
    item.status !== "awaiting_approval" &&
    !isSubagentTool(item.name)
  );
}

function stepTs(step: ChainStep): number {
  return step.kind === "thought" ? step.ts : step.tool.requestedMs;
}

function stepEnd(step: ChainStep): number {
  return step.kind === "thought"
    ? step.ts
    : (step.tool.completedMs ?? step.tool.requestedMs);
}

/**
 * Split `items` (one run, in order) into chains and standalone items.
 * `runFinished`: the run has ended, so no chain is live.
 */
export function groupRunItems(
  items: readonly TimelineItem[],
  runFinished: boolean,
  runStartedMs?: number
): Segment[] {
  const out: Segment[] = [];
  let steps: ChainStep[] = [];

  const flush = (isLast: boolean) => {
    if (steps.length === 0) {
      return;
    }
    const first = steps[0] as ChainStep;
    let live = false;
    let problems = 0;
    let endMs = stepEnd(first);
    for (const s of steps) {
      endMs = Math.max(endMs, stepEnd(s));
      if (s.kind === "tool") {
        const st = s.tool.status;
        if (st === "error" || st === "denied" || st === "expired") {
          problems++;
        }
        if (st === "requested" || st === "running") {
          live = true;
        }
      }
    }
    out.push({
      kind: "chain",
      chain: {
        key: `chain-${first.key}`,
        steps,
        startMs: stepTs(first),
        spanStartMs: spanStart(stepTs(first), endMs, out.length === 0, runStartedMs),
        endMs,
        live: !runFinished && (live || isLast),
        problems
      }
    });
    steps = [];
  };

  for (let i = 0; i < items.length; i++) {
    const item = items[i] as TimelineItem;
    if (chainable(item)) {
      steps.push({ kind: "tool", key: item.key, tool: item });
      continue;
    }
    if (item.kind === "text") {
      // a thought only when the next non-text item is a chainable tool call
      let j = i + 1;
      while (j < items.length && (items[j] as TimelineItem).kind === "text") {
        j++;
      }
      const next = items[j];
      if (next !== undefined && chainable(next) && isShortThought(item.text)) {
        steps.push({
          kind: "thought",
          key: item.key,
          text: item.text.trim(),
          ts: item.ts
        });
        continue;
      }
    }
    flush(false);
    out.push({ kind: "item", item });
  }
  // the trailing chain is the run's current stretch only when nothing follows
  flush(true);
  return out;
}

/** Whole seconds a chain took, at least 1. */
export function chainSeconds(chain: Pick<Chain, "spanStartMs" | "endMs">): number {
  return Math.max(1, Math.round((chain.endMs - chain.spanStartMs) / 1000));
}

/**
 * Where a chain sorts among the chat messages: when it finished, so a message
 * the user sent while the run was still working is never shown after work that
 * had not happened yet. Never before its own start nor after the next row of
 * the same run.
 */
export function chainOrderTs(
  chain: Pick<Chain, "startMs" | "endMs">,
  nextRowTs?: number
): number {
  const capped =
    nextRowTs === undefined ? chain.endMs : Math.min(chain.endMs, nextRowTs);
  return Math.max(chain.startMs, capped);
}
