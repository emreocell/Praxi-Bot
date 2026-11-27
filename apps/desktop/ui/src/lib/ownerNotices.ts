/**
 * Owner notices in the app: the same digest Telegram and Discord receive (a mission or a background
 * run that finished, failed or needs you) lands in the notifications panel. The daemon builds the
 * notice from the ledger and the run record, so this only turns it into a history entry: no
 * toast, no model, and in private mode nothing but a generic line.
 */
import type { OwnerNotice } from "../api/types.ts";
import type { HistoryEntry } from "./notificationHistory.ts";
import type { NotifyKind } from "./notifyRules.ts";

type T = (key: string, values?: Record<string, unknown>) => string;

/** History entry id of a notice (stable, so a notice seen twice is one entry). */
export const noticeEntryId = (n: Pick<OwnerNotice, "id">): string => `notice-${n.id}`;

const KIND: Record<OwnerNotice["kind"], NotifyKind> = {
  finished: "scheduleDone",
  progress: "scheduleDone",
  failed: "failed",
  blocked: "needsYou"
};

/** The notices worth adding: not yet in the history, and a run the app already reports is skipped. */
export function freshNotices(list: readonly OwnerNotice[], history: readonly HistoryEntry[]): OwnerNotice[] {
  const ids = new Set(history.map((e) => e.id));
  const runs = new Set(history.map((e) => e.runId).filter((r): r is string => !!r));
  return list.filter((n) => {
    if (ids.has(noticeEntryId(n))) return false;
    // A background run's end is already announced live from the event stream; the digest of a
    // mission is the only report there is.
    return !(n.source === "run" && n.run_id && runs.has(n.run_id));
  });
}

/** The panel line for `n`. Private mode: a generic headline, no title, no content. */
export function noticeEntry(n: OwnerNotice, t: T, opts: { private: boolean; botName?: string }): HistoryEntry {
  const priv = opts.private;
  const title = priv
    ? t(`ownerNotice.private.${n.kind}`)
    : t(`ownerNotice.title.${n.kind}`, {
        title: n.title,
        done: n.progress?.done ?? 0,
        total: n.progress?.total ?? 0
      });
  const parts: string[] = [];
  if (!priv) {
    if (n.progress && n.kind !== "progress") {
      parts.push(
        t("ownerNotice.counts", {
          done: n.progress.done,
          total: n.progress.total,
          blocked: n.progress.blocked,
          failed: n.progress.failed
        })
      );
    }
    if (n.reason) parts.push(n.reason);
    const first = n.done?.[0]?.text ?? n.done?.[0]?.title;
    if (n.kind === "finished" && first) parts.push(first);
    const questions = n.questions?.length ?? 0;
    const inApp = n.app_only?.length ?? 0;
    if (questions > 0) parts.push(t("ownerNotice.questions", { count: questions }));
    if (inApp > 0) parts.push(t("ownerNotice.inApp", { count: inApp }));
  }
  return {
    id: noticeEntryId(n),
    kind: KIND[n.kind],
    title,
    body: parts.join(" "),
    botId: n.bot_id ?? "",
    botName: priv ? "" : (opts.botName ?? n.bot_name ?? ""),
    chatId: n.chat_id ?? "",
    ...(n.run_id ? { runId: n.run_id } : {}),
    ts: n.at_ms,
    shown: true,
    read: false
  };
}

/** The newest `seq` of a list (the cursor for the next poll). */
export const latestSeq = (list: readonly OwnerNotice[], from = 0): number =>
  list.reduce((max, n) => Math.max(max, n.seq ?? 0), from);
