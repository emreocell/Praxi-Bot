/**
 * Logic behind the "My documents" settings card: estimates in plain words, the folder list of what is
 * already added, and indexing a freshly chosen folder page by page.
 */
import type { ApiClient } from "../api/client";
import type { KnowledgeFileFailure, KnowledgeMutation, KnowledgeSkipped, KnowledgeSource } from "../api/types";

/** An estimate said in the unit a person thinks in. */
export type EstimateParts =
  | { unit: "lessThanMinute" }
  | { unit: "minutes"; value: number }
  | { unit: "hours"; value: number };

/** "less than a minute", "about 12 minutes", "about 2 hours" for a rough number of seconds. */
export function estimateParts(seconds: number): EstimateParts {
  if (!Number.isFinite(seconds) || seconds < 45) return { unit: "lessThanMinute" };
  const minutes = Math.round(seconds / 60);
  if (minutes < 90) return { unit: "minutes", value: Math.max(1, minutes) };
  return { unit: "hours", value: Math.round(minutes / 60) };
}

/** Folder-like sources, the ones this card manages (single files and notes live in the advanced card). */
export function folderSources(sources: KnowledgeSource[]): KnowledgeSource[] {
  return sources.filter((source) => source.kind === "folder");
}

const trimPath = (path: string) => path.replace(/[\\/]+$/, "").toLowerCase();

/** The folder (or a folder around it, or inside it) is already one of the sources. */
export function overlappingFolder(sources: KnowledgeSource[], path: string): KnowledgeSource | undefined {
  const wanted = trimPath(path);
  return folderSources(sources).find((source) => {
    const existing = trimPath(source.locator);
    return existing === wanted || wanted.startsWith(`${existing}/`) || wanted.startsWith(`${existing}\\`) || existing.startsWith(`${wanted}/`) || existing.startsWith(`${wanted}\\`);
  });
}

/** Last part of a path, for a default label. */
export function folderName(path: string): string {
  const parts = path.split(/[\\/]+/).filter(Boolean);
  return parts[parts.length - 1] ?? path;
}

/** Order the kinds of file for display: the ones people know first. */
const TYPE_ORDER = ["pdf", "docx", "xlsx", "text", "web", "code"];
export function typeCounts(byType: Record<string, number>): { type: string; count: number }[] {
  return Object.entries(byType)
    .filter(([, count]) => count > 0)
    .sort(([a], [b]) => (TYPE_ORDER.indexOf(a) + 1 || 99) - (TYPE_ORDER.indexOf(b) + 1 || 99))
    .map(([type, count]) => ({ type, count }));
}

export interface IndexProgress {
  /** Files read and stored so far. */
  indexed: number;
  failures: KnowledgeFileFailure[];
  skipped: KnowledgeSkipped;
  /** The last source, with its newest revision. */
  source: KnowledgeSource;
}

const MAX_PAGES = 200;

/**
 * Carry on from the first page the daemon indexed while adding a folder until every file is read.
 * `stop` is checked between pages (the card was closed, or the owner pressed pause); the daemon's own
 * folder watcher then finishes the rest in the background, so stopping here loses nothing.
 */
export async function finishIndexing(
  client: Pick<ApiClient, "reindexKnowledgeSource">,
  bot: string,
  first: KnowledgeMutation,
  onProgress: (progress: IndexProgress) => void,
  stop: () => boolean = () => false
): Promise<IndexProgress> {
  let current = first;
  let indexed = 0;
  const failures: KnowledgeFileFailure[] = [];
  for (let page = 0; page < MAX_PAGES; page += 1) {
    indexed += current.indexing.processed;
    failures.push(...(current.indexing.failures ?? []));
    const progress = { indexed, failures: [...failures], skipped: current.indexing.skipped, source: current.source };
    onProgress(progress);
    const cursor = current.indexing.next_cursor;
    if (!cursor || stop()) return progress;
    current = await client.reindexKnowledgeSource(bot, current.source.id, current.source.revision, cursor);
  }
  return { indexed, failures, skipped: current.indexing.skipped, source: current.source };
}
