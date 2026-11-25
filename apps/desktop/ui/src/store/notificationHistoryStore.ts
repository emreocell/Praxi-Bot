import { useSyncExternalStore } from "react";
import {
  addEntry,
  type HistoryEntry,
  markAllRead as markAll,
  markIdsRead,
  markRead as markOne,
  parseHistory,
  resolveByCallId,
  resolveByIds,
  type ResolvedOutcome,
  snooze as snoozeOne,
  snoozeIds,
  unsnooze as unsnoozeOne,
  unsnoozeIds
} from "../lib/notificationHistory";

/** The notification history, mirrored to localStorage (this profile only; titles, no content). */
const KEY = "praxi.notificationHistory";
let cache: HistoryEntry[] | null = null;
const listeners = new Set<() => void>();

function load(): HistoryEntry[] {
  if (cache) {
    return cache;
  }
  try {
    const raw = window.localStorage.getItem(KEY);
    cache = parseHistory(raw ? JSON.parse(raw) : null, Date.now());
  } catch {
    cache = [];
  }
  return cache;
}

function commit(next: HistoryEntry[]): void {
  cache = next;
  try {
    window.localStorage.setItem(KEY, JSON.stringify(next));
  } catch {
    // blocked storage: the history lives for this session only
  }
  listeners.forEach((l) => l());
}

export function recordNotification(entry: HistoryEntry): void {
  commit(addEntry(load(), entry, Date.now()));
}
export function markAllNotificationsRead(): void {
  const prev = load();
  const next = markAll(prev);
  if (next !== prev) commit(next);
}
export function markNotificationRead(id: string): void {
  commit(markOne(load(), id));
}
export function markNotificationsRead(ids: readonly string[]): void {
  const prev = load();
  const next = markIdsRead(prev, ids);
  if (next !== prev) commit(next);
}
export function resolveNotifications(ids: readonly string[], outcome: ResolvedOutcome): void {
  commit(resolveByIds(load(), ids, outcome));
}
export function resolveNotificationByCallId(callId: string, outcome: ResolvedOutcome): void {
  const prev = load();
  const next = resolveByCallId(prev, callId, outcome);
  if (next !== prev) commit(next);
}
export function snoozeNotification(id: string, until: number): void {
  commit(snoozeOne(load(), id, until));
}
export function snoozeNotifications(ids: readonly string[], until: number): void {
  commit(snoozeIds(load(), ids, until));
}
export function unsnoozeNotification(id: string): void {
  commit(unsnoozeOne(load(), id));
}
export function unsnoozeNotifications(ids: readonly string[]): void {
  commit(unsnoozeIds(load(), ids));
}
export function clearNotificationHistory(): HistoryEntry[] {
  const prev = load();
  commit([]);
  return prev;
}
export function restoreNotificationHistory(entries: readonly HistoryEntry[]): void {
  commit(parseHistory(entries, Date.now()));
}

export function useNotificationHistory(): HistoryEntry[] {
  return useSyncExternalStore(
    (cb) => {
      listeners.add(cb);
      return () => listeners.delete(cb);
    },
    load
  );
}
