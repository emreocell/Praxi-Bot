import { useSyncExternalStore } from "react";
import {
  addEntry,
  type HistoryEntry,
  markAllRead as markAll,
  markRead as markOne,
  parseHistory
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
export function clearNotificationHistory(): void {
  commit([]);
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
