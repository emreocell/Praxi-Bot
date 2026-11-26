import type { RunState, TimelineItem } from "../state/timeline.ts";

/**
 * A failed run shows its error ONCE: the card under the run (ChatView `runError`, with the fix button).
 * The daemon reports the same failure twice, as an `error` event (kept by the timeline as a
 * `chat.notice.error` line) and as `run_finished.error`, which drew a plain line above an identical card.
 * Pure, so the rule is tested without a browser.
 */

const normalise = (message: string): string =>
  message
    .trim()
    .replace(/^(?:error:\s*)+/i, "")
    .replace(/\s+/g, " ")
    .toLowerCase();

/** The same failure text, give or take an "Error:" prefix, case and whitespace. */
export function sameFailure(a: string, b: string): boolean {
  const x = normalise(a);
  const y = normalise(b);
  if (x === "" || y === "") return false;
  const [short, long] = x.length <= y.length ? [x, y] : [y, x];
  // A prefix the daemon adds ("execution error: ...") still names the same failure; a few letters do not.
  return x === y || (short.length >= 8 && long.endsWith(short));
}

/**
 * The run's items without the error notices that repeat its final error. Other notices, and every
 * notice while the run is still going or when it ended fine, are kept. `codeOf` (optional) names a
 * known failure; two texts with the same non-null code are the same failure too.
 */
export function itemsWithoutDuplicateError(
  run: Pick<RunState, "items" | "error" | "finishedMs">,
  codeOf?: (message: string) => string | null
): TimelineItem[] {
  const final = run.error;
  if (run.finishedMs === undefined || !final) return run.items;
  const finalCode = codeOf?.(final) ?? null;
  const repeats = (message: string) =>
    sameFailure(message, final) || (finalCode !== null && codeOf?.(message) === finalCode);
  const kept = run.items.filter(
    (i) => !(i.kind === "notice" && i.i18nKey === "chat.notice.error" && repeats(i.values.message ?? ""))
  );
  return kept.length === run.items.length ? run.items : kept;
}
