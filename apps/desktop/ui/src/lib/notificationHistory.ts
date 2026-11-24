import type { HoldReason } from "./botMute.ts";
import type { NotifyKind } from "./notifyRules.ts";

/** Newest entries kept; older ones fall off. */
export const HISTORY_MAX = 100;
/** Entries older than this are dropped on load / add. */
export const HISTORY_MAX_AGE_MS = 30 * 24 * 60 * 60 * 1000;

export interface HistoryEntry {
  id: string;
  kind: NotifyKind;
  /** Title line only (generic text in private mode); never more content than the toast had. */
  title: string;
  body: string;
  botId: string;
  botName: string;
  chatId: string;
  ts: number;
  /** true = a toast was shown; false = held back (see `heldBy`). */
  shown: boolean;
  /** Why a held toast was held; absent on shown entries.  Older stored entries mean quiet hours. */
  heldBy?: HoldReason;
  read: boolean;
  /** Hidden from the inbox (and the badge) until this time. */
  snoozedUntil?: number;
}

const KINDS: readonly NotifyKind[] = ["needsYou", "longRun", "failed", "scheduleDone", "digest"];

const str = (v: unknown): string => (typeof v === "string" ? v : "");

export function parseHistory(raw: unknown, now: number): HistoryEntry[] {
  if (!Array.isArray(raw)) {
    return [];
  }
  const out: HistoryEntry[] = [];
  for (const r of raw) {
    if (typeof r !== "object" || r === null) {
      continue;
    }
    const o = r as Record<string, unknown>;
    if (!KINDS.includes(o.kind as NotifyKind) || typeof o.ts !== "number" || !str(o.id)) {
      continue;
    }
    out.push({
      id: str(o.id),
      kind: o.kind as NotifyKind,
      title: str(o.title),
      body: str(o.body),
      botId: str(o.botId),
      botName: str(o.botName),
      chatId: str(o.chatId),
      ts: o.ts,
      shown: o.shown === true,
      ...(o.shown === true ? {} : { heldBy: o.heldBy === "muted" ? ("muted" as const) : ("quiet" as const) }),
      read: o.read === true,
      ...(typeof o.snoozedUntil === "number" && Number.isFinite(o.snoozedUntil) ? { snoozedUntil: o.snoozedUntil } : {})
    });
  }
  return prune(out, now);
}

/** Newest first, within the age and count caps. */
export function prune(list: HistoryEntry[], now: number): HistoryEntry[] {
  return list
    .filter((e) => now - e.ts <= HISTORY_MAX_AGE_MS)
    .sort((a, b) => b.ts - a.ts)
    .slice(0, HISTORY_MAX);
}

export function addEntry(list: HistoryEntry[], entry: HistoryEntry, now: number): HistoryEntry[] {
  return prune([entry, ...list.filter((e) => e.id !== entry.id)], now);
}

export function markAllRead(list: HistoryEntry[]): HistoryEntry[] {
  return list.some((e) => !e.read) ? list.map((e) => (e.read ? e : { ...e, read: true })) : list;
}

export function markRead(list: HistoryEntry[], id: string): HistoryEntry[] {
  return list.map((e) => (e.id === id && !e.read ? { ...e, read: true } : e));
}

export const isSnoozed = (e: HistoryEntry, now: number): boolean => e.snoozedUntil !== undefined && e.snoozedUntil > now;

/** Unread entries; pass `now` to leave out the ones snoozed right now. */
export const unreadCount = (list: HistoryEntry[], now?: number): number =>
  list.filter((e) => !e.read && (now === undefined || !isSnoozed(e, now))).length;

export function snooze(list: HistoryEntry[], id: string, until: number): HistoryEntry[] {
  return list.map((e) => (e.id === id ? { ...e, snoozedUntil: until } : e));
}

export function unsnooze(list: HistoryEntry[], id: string): HistoryEntry[] {
  return list.map((e) => {
    if (e.id !== id || e.snoozedUntil === undefined) return e;
    const { snoozedUntil: _gone, ...rest } = e;
    return rest;
  });
}

export interface HistoryFilter {
  unreadOnly: boolean;
  kind: NotifyKind | "all";
  /** "" = every bot. */
  botId: string;
  /** Show only the snoozed entries instead of hiding them. */
  snoozed: boolean;
}

export const NO_FILTER: HistoryFilter = { unreadOnly: false, kind: "all", botId: "", snoozed: false };

export function filterEntries(list: HistoryEntry[], f: HistoryFilter, now: number): HistoryEntry[] {
  return list.filter(
    (e) =>
      isSnoozed(e, now) === f.snoozed &&
      (!f.unreadOnly || !e.read) &&
      (f.kind === "all" || e.kind === f.kind) &&
      (f.botId === "" || e.botId === f.botId)
  );
}

/** Unread entries a quiet-hours window held back (their toast never showed). */
export const heldByQuietCount = (list: HistoryEntry[], now: number): number =>
  list.filter((e) => !e.shown && e.heldBy === "quiet" && !e.read && !isSnoozed(e, now)).length;
