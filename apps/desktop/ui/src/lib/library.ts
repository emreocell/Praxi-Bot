/**
 * The Library tab in numbers: what a bot reads (documents), what it was told (notes) and what it
 * remembers (memory). Pure, so the shelves and their empty states are decided in one place.
 */
import type { KnowledgeSource, MemoryItem } from "../api/types";
import type { SettingsTarget } from "./settingsLink.ts";

export interface LibraryShelves {
  /** Files, folders, saved pages, chats and mail the bot looks things up in (not notes). */
  documents: KnowledgeSource[];
  /** Short texts the owner left for the bot. */
  notes: KnowledgeSource[];
  /** Documents inside `documents` (a folder counts its files). */
  documentCount: number;
  /** Memories in use for this bot: its own and the shared ones. */
  remembered: number;
  /** Memories that wait for the owner's OK before the bot may use them. */
  waiting: number;
}

/** Newest first, so the shelf shows what changed last. */
const newest = (a: KnowledgeSource, b: KnowledgeSource) => b.updated_ms - a.updated_ms;

/**
 * `memories` is the bot's memory list, `pending` the separate "needs your OK" list (all bots).
 * A pending memory counts once, as waiting, whichever list it came from.
 */
export function libraryShelves(
  botId: string,
  sources: readonly KnowledgeSource[],
  memories: readonly MemoryItem[],
  pending: readonly MemoryItem[] = []
): LibraryShelves {
  const notes = sources.filter((s) => s.kind === "note").sort(newest);
  const documents = sources.filter((s) => s.kind !== "note").sort(newest);
  const forBot = (m: MemoryItem) => !m.deleted && (m.bot_id === botId || m.bot_id === null || m.bot_id === undefined);
  const waiting = new Set([...memories, ...pending].filter((m) => forBot(m) && m.pending).map((m) => m.id));
  return {
    documents,
    notes,
    documentCount: documents.reduce((sum, s) => sum + Math.max(0, s.documents), 0),
    remembered: memories.filter((m) => forBot(m) && !m.pending).length,
    waiting: waiting.size
  };
}

/** Where "Add a document" and "Open memory" go: the Settings card, opened on this bot. */
export function libraryTarget(card: "knowledge" | "memory", botId: string): SettingsTarget {
  return { page: "models", card, field: "bot", bot: botId };
}
