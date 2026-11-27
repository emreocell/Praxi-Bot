// Mock mode: what the first bot already knows, so the Library tab shows filled shelves
// (documents and notes); the other bots start empty and show the empty states.
import type { KnowledgeSource } from "./types";

const DAY = 86_400_000;

export function seedKnowledgeSources(bot: string, now: number): [string, KnowledgeSource[]][] {
  const source = (id: string, kind: KnowledgeSource["kind"], label: string, locator: string, documents: number, age: number): KnowledgeSource => ({
    id, kind, label, locator, documents, chunks: documents * 6, enabled: true, requires_path_selection: false,
    revision: 1, remote_profile_fingerprint: null, updated_ms: now - age
  });
  return [[bot, [
    source("ks-1", "folder", "Proje belgeleri", "~/Documents/Praxi", 42, 2 * DAY),
    source("ks-2", "saved_web", "Sürüm kontrol listesi", "https://example.com/release-checklist", 1, 5 * DAY),
    source("ks-3", "note", "Yazım tercihleri", "saved:note", 1, DAY),
    source("ks-4", "note", "Şirket adresi ve vergi no", "saved:note", 1, 9 * DAY)
  ]]];
}
