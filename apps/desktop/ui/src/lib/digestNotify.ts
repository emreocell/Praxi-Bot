/**
 * The optional daily digest notification (issue #589): a short summary toast at a time the owner
 * picks. Off by default. Built from the same data as the "Today" card (`dailyDigest.ts`), so no
 * model is called. It follows the quiet-hours rule of every other toast: inside the quiet window
 * it is not shown, it only lands in the notification history.
 */
import type { BotDigest } from "./dailyDigest.ts";
import { type QuietHours, isQuiet, parseMinutes } from "./quietHours.ts";

/** A digest older than this after its time is skipped instead of arriving late (app was closed). */
export const DIGEST_LATE_MS = 6 * 60 * 60 * 1000;

export type DigestPlan = "wait" | "show" | "hold" | "skip";

/** Local calendar day as `YYYY-MM-DD`. */
export function dayKey(at: Date): string {
  const p = (n: number) => String(n).padStart(2, "0");
  return `${at.getFullYear()}-${p(at.getMonth() + 1)}-${p(at.getDate())}`;
}

/** Tolerant parse of the stored time: "" (off) unless it is a real `HH:MM`. */
export function parseDigestTime(raw: unknown): string {
  return typeof raw === "string" && parseMinutes(raw) !== null ? raw : "";
}

/**
 * What to do right now. `wait`: not time yet (or off, or already handled today). `show`: send the
 * toast. `hold`: it is due but the quiet window is on, so keep it in the history only. `skip`:
 * the moment passed long ago; mark the day as handled without a toast.
 * Every answer except `wait` means "mark `dayKey(now)` as handled".
 */
export function planDigest(now: Date, digestTime: string, lastDay: string, quiet: QuietHours): DigestPlan {
  const target = parseMinutes(digestTime);
  if (target === null || lastDay === dayKey(now)) {
    return "wait";
  }
  const minutes = now.getHours() * 60 + now.getMinutes();
  if (minutes < target) {
    return "wait";
  }
  if ((minutes - target) * 60_000 > DIGEST_LATE_MS) {
    return "skip";
  }
  return isQuiet(now, quiet) ? "hold" : "show";
}

export interface DigestTotals {
  done: number;
  failed: number;
  stopped: number;
  waiting: number;
  running: number;
  costUsd: number | null;
}

export function digestTotals(rows: readonly BotDigest[]): DigestTotals {
  const total: DigestTotals = { done: 0, failed: 0, stopped: 0, waiting: 0, running: 0, costUsd: null };
  for (const r of rows) {
    total.done += r.done;
    total.failed += r.failed;
    total.stopped += r.stopped;
    total.waiting += r.waiting;
    total.running += r.running;
    if (r.costUsd !== null) {
      total.costUsd = (total.costUsd ?? 0) + r.costUsd;
    }
  }
  return total;
}

type T = (key: string, values?: Record<string, unknown>) => string;

/**
 * The toast text: counts and cost only, never a bot name or any run content. Private mode shows
 * not even the counts. Returns `null` when there is nothing to report.
 */
export function digestText(
  rows: readonly BotDigest[],
  opts: { private: boolean; cost: (usd: number) => string },
  t: T
): { title: string; body: string } | null {
  if (rows.length === 0) {
    return null;
  }
  const title = t("digest.notifyTitle");
  if (opts.private) {
    return { title, body: t("digest.notifyPrivate") };
  }
  const n = digestTotals(rows);
  const parts: string[] = [];
  if (n.waiting > 0) parts.push(t("digest.waiting", { count: n.waiting }));
  if (n.failed > 0) parts.push(t("digest.failed", { count: n.failed }));
  if (n.done > 0) parts.push(t("digest.done", { count: n.done }));
  if (n.stopped > 0) parts.push(t("digest.stopped", { count: n.stopped }));
  if (n.running > 0) parts.push(t("digest.running", { count: n.running }));
  if (n.costUsd !== null) parts.push(t("digest.totalCost", { cost: opts.cost(n.costUsd) }));
  return { title, body: parts.join(". ") };
}
