import type { HoldReason } from "./botMute.ts";
import type { NotifyKind } from "./notifyRules.ts";

/** Newest entries kept; older ones fall off. */
export const HISTORY_MAX = 100;
/** Entries older than this are dropped on load / add. */
export const HISTORY_MAX_AGE_MS = 30 * 24 * 60 * 60 * 1000;
/** Recent window for the Updates group (24 hours). */
export const UPDATES_WINDOW_MS = 24 * 60 * 60 * 1000;

export type ResolvedOutcome = "approved" | "denied" | "answered" | "expired" | "retried";

export interface HistoryEntry {
  id: string;
  kind: NotifyKind;
  /** Concise human headline (generic text in private mode); never more content than the toast had. */
  title: string;
  body: string;
  botId: string;
  botName: string;
  chatId: string;
  runId?: string;
  callId?: string;
  retryAtMs?: number;
  retryable?: boolean;
  resolved?: ResolvedOutcome;
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
const OUTCOMES: readonly ResolvedOutcome[] = ["approved", "denied", "answered", "expired", "retried"];

const str = (v: unknown): string => (typeof v === "string" ? v : "");
const optStr = (v: unknown): string | undefined => (typeof v === "string" && v ? v : undefined);

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
    const runId = optStr(o.runId);
    const callId = optStr(o.callId);
    const resolved = OUTCOMES.includes(o.resolved as ResolvedOutcome)
      ? (o.resolved as ResolvedOutcome)
      : undefined;
    out.push({
      id: str(o.id),
      kind: o.kind as NotifyKind,
      title: str(o.title),
      body: str(o.body),
      botId: str(o.botId),
      botName: str(o.botName),
      chatId: str(o.chatId),
      ...(runId ? { runId } : {}),
      ...(callId ? { callId } : {}),
      ...(typeof o.retryAtMs === "number" && Number.isFinite(o.retryAtMs) ? { retryAtMs: o.retryAtMs } : {}),
      ...(o.retryable === true ? { retryable: true } : {}),
      ...(resolved ? { resolved } : {}),
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

export function markIdsRead(list: HistoryEntry[], ids: readonly string[]): HistoryEntry[] {
  if (ids.length === 0) return list;
  const set = new Set(ids);
  let changed = false;
  const next = list.map((e) => {
    if (set.has(e.id) && !e.read) {
      changed = true;
      return { ...e, read: true };
    }
    return e;
  });
  return changed ? next : list;
}

export function resolveByIds(
  list: HistoryEntry[],
  ids: readonly string[],
  outcome: ResolvedOutcome
): HistoryEntry[] {
  if (ids.length === 0) return list;
  const set = new Set(ids);
  return list.map((e) =>
    set.has(e.id) ? { ...e, read: true, resolved: outcome } : e
  );
}

export function resolveByCallId(
  list: HistoryEntry[],
  callId: string,
  outcome: ResolvedOutcome
): HistoryEntry[] {
  if (!callId) return list;
  let changed = false;
  const next = list.map((e) => {
    if (e.callId === callId && (e.resolved !== outcome || !e.read)) {
      changed = true;
      return { ...e, read: true, resolved: outcome };
    }
    return e;
  });
  return changed ? next : list;
}

export const isSnoozed = (e: HistoryEntry, now: number): boolean =>
  e.snoozedUntil !== undefined && e.snoozedUntil > now;

/** Whether an entry still waits for the user's decision. */
export function isActionable(
  e: HistoryEntry,
  liveCallIds?: ReadonlySet<string>
): boolean {
  if (e.kind !== "needsYou" || e.resolved !== undefined) {
    return false;
  }
  if (liveCallIds !== undefined && e.callId) {
    return liveCallIds.has(e.callId);
  }
  return !e.read;
}

/** Unread entries; pass `now` to leave out the ones snoozed right now. */
export const unreadCount = (list: HistoryEntry[], now?: number): number =>
  list.filter((e) => !e.read && (now === undefined || !isSnoozed(e, now))).length;

/**
 * Sidebar badge count: unread, non-snoozed items that need action. Duplicates
 * collapse the way the inbox shows them ("asked 4 times" is one item).
 */
export const actionableCount = (
  list: HistoryEntry[],
  now?: number,
  liveCallIds?: ReadonlySet<string>
): number =>
  collapseDuplicates(
    list.filter(
      (e) =>
        !e.read &&
        (now === undefined || !isSnoozed(e, now)) &&
        isActionable(e, liveCallIds)
    ),
    liveCallIds
  ).length;

export function snooze(list: HistoryEntry[], id: string, until: number): HistoryEntry[] {
  return list.map((e) => (e.id === id ? { ...e, snoozedUntil: until } : e));
}

export function snoozeIds(
  list: HistoryEntry[],
  ids: readonly string[],
  until: number
): HistoryEntry[] {
  if (ids.length === 0) return list;
  const set = new Set(ids);
  return list.map((e) => (set.has(e.id) ? { ...e, snoozedUntil: until } : e));
}

export function unsnooze(list: HistoryEntry[], id: string): HistoryEntry[] {
  return list.map((e) => {
    if (e.id !== id || e.snoozedUntil === undefined) return e;
    const copy = { ...e };
    delete copy.snoozedUntil;
    return copy;
  });
}

export function unsnoozeIds(list: HistoryEntry[], ids: readonly string[]): HistoryEntry[] {
  if (ids.length === 0) return list;
  const set = new Set(ids);
  return list.map((e) => {
    if (!set.has(e.id) || e.snoozedUntil === undefined) return e;
    const copy = { ...e };
    delete copy.snoozedUntil;
    return copy;
  });
}

export type FilterCategory = "all" | "needsYou" | "runs" | "errors";

export interface HistoryFilter {
  unreadOnly: boolean;
  category: FilterCategory;
  /** Show only the snoozed entries instead of hiding them. */
  snoozed: boolean;
}

export const NO_FILTER: HistoryFilter = { unreadOnly: false, category: "all", snoozed: false };

export function matchesCategory(kind: NotifyKind, category: FilterCategory): boolean {
  switch (category) {
    case "all":
      return true;
    case "needsYou":
      return kind === "needsYou";
    case "runs":
      return kind === "longRun" || kind === "scheduleDone" || kind === "digest";
    case "errors":
      return kind === "failed";
  }
}

export function filterEntries(list: HistoryEntry[], f: HistoryFilter, now: number): HistoryEntry[] {
  return list.filter(
    (e) =>
      isSnoozed(e, now) === f.snoozed &&
      (!f.unreadOnly || !e.read) &&
      matchesCategory(e.kind, f.category)
  );
}

/** Unread entries a quiet-hours window held back (their toast never showed). */
export const heldByQuietCount = (list: HistoryEntry[], now: number): number =>
  list.filter((e) => !e.shown && e.heldBy === "quiet" && !e.read && !isSnoozed(e, now)).length;

export type InboxSection = "needsYou" | "updates" | "earlier";

export interface CollapsedNotification {
  key: string;
  primary: HistoryEntry;
  entries: HistoryEntry[];
  ids: string[];
  count: number;
  unread: boolean;
  latestTs: number;
  /** Live pending callId among the collapsed entries, if any. */
  activeCallId?: string;
}

export interface GroupedInbox {
  needsYou: CollapsedNotification[];
  updates: CollapsedNotification[];
  earlier: CollapsedNotification[];
}

function collapseKey(e: HistoryEntry): string {
  return `${e.botId}\u0000${e.kind}\u0000${e.title}\u0000${e.body}`;
}

/** Collapse duplicate notifications per bot/run while preserving newest-first order. */
export function collapseDuplicates(
  entries: readonly HistoryEntry[],
  liveCallIds?: ReadonlySet<string>
): CollapsedNotification[] {
  const groups = new Map<string, HistoryEntry[]>();
  for (const e of entries) {
    const key = collapseKey(e);
    const bucket = groups.get(key);
    if (bucket) {
      bucket.push(e);
    } else {
      groups.set(key, [e]);
    }
  }
  const result: CollapsedNotification[] = [];
  for (const [key, items] of groups) {
    const sorted = [...items].sort((a, b) => b.ts - a.ts);
    const active = liveCallIds
      ? sorted.find((e) => e.callId && liveCallIds.has(e.callId) && e.resolved === undefined)
      : undefined;
    const primary = active ?? sorted[0];
    const ids = sorted.map((e) => e.id);
    result.push({
      key: `${key}\u0000${primary.id}`,
      primary,
      entries: sorted,
      ids,
      count: sorted.length,
      unread: sorted.some((e) => !e.read),
      latestTs: sorted[0].ts,
      ...(active?.callId ? { activeCallId: active.callId } : {})
    });
  }
  return result.sort((a, b) => b.latestTs - a.latestTs);
}

/** Group visible notifications into Needs you / Updates / Earlier with duplicates collapsed. */
export function groupInbox(
  entries: readonly HistoryEntry[],
  now: number,
  liveCallIds?: ReadonlySet<string>
): GroupedInbox {
  const collapsed = collapseDuplicates(entries, liveCallIds);
  const out: GroupedInbox = { needsYou: [], updates: [], earlier: [] };
  for (const item of collapsed) {
    const actionable =
      item.activeCallId !== undefined ||
      item.entries.some((e) => isActionable(e, liveCallIds));
    if (actionable) {
      out.needsYou.push(item);
    } else if (item.unread || now - item.latestTs < UPDATES_WINDOW_MS) {
      out.updates.push(item);
    } else {
      out.earlier.push(item);
    }
  }
  return out;
}
