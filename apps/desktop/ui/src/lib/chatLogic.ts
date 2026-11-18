/**
 * Pure decisions behind the calmer chat (244-D): which sections render at all,
 * and when the timeline follows new content. No React here, so they are tested
 * without a browser.
 */

/** Tools whose effects the checkpoint store records. */
const FILE_CHECKPOINT_TOOLS = new Set([
  "fs_write",
  "fs_edit",
  "fs_move",
  "fs_delete"
]);

interface ChangeProbeRun {
  finishedMs?: number;
  items: readonly { kind: string; name?: string }[];
}

/**
 * Worth asking the daemon whether this run changed files? Only a finished run,
 * and only the latest one or one that used a file tool. Whether anything
 * changed is decided by the answer, see `changesSectionVisible`.
 */
export function shouldProbeChanges(
  run: ChangeProbeRun,
  isLastFinished: boolean
): boolean {
  if (run.finishedMs === undefined) {
    return false;
  }
  return (
    isLastFinished ||
    run.items.some(
      (item) =>
        item.kind === "tool" &&
        item.name !== undefined &&
        FILE_CHECKPOINT_TOOLS.has(item.name)
    )
  );
}

/**
 * Owner rule: no empty sections. The Changes section renders only when the
 * daemon listed at least one changed file. `null` = unknown yet / failed:
 * nothing is shown (a section that might be empty is not shown).
 */
export function changesSectionVisible(changeCount: number | null): boolean {
  return changeCount !== null && changeCount > 0;
}

/**
 * The routing chip is engineering detail. Show it when it says something the
 * user cares about (a free, model-less route) or when the debug setting is on.
 */
export function showRouteChip(cheap: boolean, debug: boolean): boolean {
  return cheap || debug;
}

// ---------------------------------------------------------------- scrolling

/** Closer than this to the bottom counts as "reading the latest". */
export const REATTACH_PX = 24;

export interface ScrollMetrics {
  scrollHeight: number;
  scrollTop: number;
  clientHeight: number;
}

export function distanceFromBottom(m: ScrollMetrics): number {
  return m.scrollHeight - m.scrollTop - m.clientHeight;
}

/**
 * Whether the timeline keeps following new content after a scroll event.
 * Scrolling UP detaches at once (even by a few pixels), so streaming text never
 * pulls the page away from a user who is reading. Reaching the bottom again
 * re-attaches. Content growing below leaves the state alone.
 */
export function nextFollow(
  following: boolean,
  m: ScrollMetrics,
  previousTop: number
): boolean {
  if (distanceFromBottom(m) <= REATTACH_PX) {
    return true;
  }
  if (m.scrollTop < previousTop - 1) {
    return false;
  }
  return following;
}

/** The "Jump to latest" button: only while detached and clearly away from the end. */
export function showJumpToLatest(
  following: boolean,
  m: ScrollMetrics
): boolean {
  return !following && distanceFromBottom(m) > REATTACH_PX * 4;
}
